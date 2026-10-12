//! The Core replaces JD2's DownloadWatchDog (4.5k lines, one watchdog thread + job queue),
//! LinkCollector (3.6k) and controller glue. One process-wide instance; observable via events.

use crate::accounts::Vault;
use crate::captcha::{Broker, CaptchaRequest};
use crate::crawler::{self, archive_base, clean_package_name, group_links, CrawlSink, Found, Groupable};
use crate::engine::{self, DlError, DlJob, DlKind, DlState};
use crate::events::{Event, Notice, Stats};
use crate::model::*;
use crate::plugins::{self, Avail, Ctx, ErrCode, Hooks, PResult, PluginAccount, PluginError, Registry, Resolution, P};
use crate::settings::Settings;
use crate::util::http::{host_of, Browser, NetOpts};
use crate::util::rate::{SpeedMeter, TokenBucket};
use crate::util::{self, fmt_bytes, sanitize_dir, sanitize_filename};
use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use reqwest::cookie::Jar;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Notify, Semaphore};
use tokio_util::sync::CancellationToken;

const MAX_BACKOFF: i64 = 10 * 60_000;

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    pub links: HashMap<String, Link>,
    pub packages: HashMap<String, Package>,
    pub accounts: Vec<Account>,
    pub kv: Map<String, Value>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reason {
    Pause,
    Stop,
    Remove,
    Restart,
}

struct Running {
    cancel: CancellationToken,
    speed: Arc<SpeedMeter>,
    ext_speed: Arc<AtomicU64>,
    reason: Arc<Mutex<Option<Reason>>>,
}

#[derive(Clone, Serialize, Default)]
pub struct JobView {
    pub id: String,
    pub source: String,
    pub found: usize,
    pub problems: Vec<(String, String)>,
    pub done: bool,
    pub started_at: i64,
}

pub struct AddRequest {
    pub text: String,
    pub package_name: Option<String>,
    pub dir: Option<String>,
    pub passwords: Vec<String>,
    pub auto_start: Option<bool>,
    pub source: String,
    pub origin: Option<String>,
    pub link_password: Option<String>,
}
impl AddRequest {
    pub fn text(text: &str, source: &str) -> Self {
        AddRequest { text: text.into(), package_name: None, dir: None, passwords: vec![], auto_start: None, source: source.into(), origin: None, link_password: None }
    }
}

pub struct Core {
    pub data_dir: PathBuf,
    pub settings: Arc<Settings>,
    pub state: Mutex<State>,
    pub registry: RwLock<Arc<Registry>>,
    pub captcha: Arc<Broker>,
    pub vault: Vault,
    pub events: broadcast::Sender<Event>,
    /// bumps on every visible change (GUI re-snapshots only when it moves)
    pub version: AtomicU64,
    global_limiter: Arc<TokenBucket>,
    global_speed: Arc<SpeedMeter>,
    running: Mutex<HashMap<String, Running>>,
    host_blocked: Mutex<HashMap<String, (i64, String)>>,
    jars: Mutex<HashMap<String, Arc<Jar>>>,
    changed_links: Mutex<HashSet<String>>,
    changed_pkgs: Mutex<HashSet<String>>,
    dirty: AtomicBool,
    paused: AtomicBool,
    schedule_paused: AtomicBool,
    schedule_limit: Mutex<Option<u64>>,
    disk_low: AtomicBool,
    session_bytes: AtomicU64,
    jobs: Mutex<HashMap<String, JobView>>,
    checking: Mutex<HashSet<String>>,
    check_sem: Arc<Semaphore>,
    extracting: Mutex<HashSet<String>>,
    tick: Notify,
    pub notices: Mutex<Vec<Notice>>,
    pub sys: Mutex<SysInfo>,
}

#[derive(Clone, Default, Serialize)]
pub struct SysInfo {
    pub ytdlp: Option<String>,
    pub seven_zip: bool,
    pub api_url: String,
    pub cnl: bool,
}

impl Core {
    pub fn new(data_dir: PathBuf) -> Arc<Core> {
        std::fs::create_dir_all(&data_dir).ok();
        let (tx, _) = broadcast::channel(4096);
        crate::log::init(&data_dir.join("logs"), std::env::var("FETCHWING_LOG").is_ok(), tx.clone());
        let state: State = std::fs::read(data_dir.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| crate::log::error("core", &format!("state.json unreadable ({e}); starting fresh (backup kept)"))).ok())
            .unwrap_or_default();
        if data_dir.join("state.json").exists() && state.links.is_empty() && state.packages.is_empty() {
            let _ = std::fs::copy(data_dir.join("state.json"), data_dir.join("state.json.bak"));
        }
        let settings = Arc::new(Settings::new(state.kv.get("settings").and_then(|v| v.as_object().cloned()).unwrap_or_default()));
        if settings.str("remote.token").is_empty() {
            let _ = settings.set("remote.token", json!(format!("{}{}{}", new_id(), new_id(), new_id())));
        }
        let spend = state.kv.get("captcha.spend").and_then(|v| serde_json::from_value::<(String, u64)>(v.clone()).ok()).unwrap_or_default();
        let registry = Arc::new(Registry::new(Some(&data_dir.join("plugins"))));
        let core = Arc::new(Core {
            vault: Vault::open(&data_dir),
            captcha: Arc::new(Broker::new(settings.clone(), tx.clone(), spend)),
            paused: AtomicBool::new(state.kv.get("paused").and_then(|v| v.as_bool()).unwrap_or(false)),
            data_dir,
            settings,
            state: Mutex::new(state),
            registry: RwLock::new(registry),
            events: tx,
            version: AtomicU64::new(1),
            global_limiter: Arc::new(TokenBucket::new(0)),
            global_speed: Arc::new(SpeedMeter::new(3000)),
            running: Mutex::new(HashMap::new()),
            host_blocked: Mutex::new(HashMap::new()),
            jars: Mutex::new(HashMap::new()),
            changed_links: Mutex::new(HashSet::new()),
            changed_pkgs: Mutex::new(HashSet::new()),
            dirty: AtomicBool::new(true),
            schedule_paused: AtomicBool::new(false),
            schedule_limit: Mutex::new(None),
            disk_low: AtomicBool::new(false),
            session_bytes: AtomicU64::new(0),
            jobs: Mutex::new(HashMap::new()),
            checking: Mutex::new(HashSet::new()),
            check_sem: Arc::new(Semaphore::new(8)),
            extracting: Mutex::new(HashSet::new()),
            tick: Notify::new(),
            notices: Mutex::new(vec![]),
            sys: Mutex::new(SysInfo::default()),
        });
        core
    }

    // =====================================================================================
    // lifecycle
    // =====================================================================================

    pub fn start(self: &Arc<Self>) {
        {
            let resume = self.settings.bool("general.resumeOnStartup");
            let mut st = self.state.lock();
            for l in st.links.values_mut() {
                if matches!(l.status, Status::Downloading | Status::Captcha | Status::Postprocessing) {
                    l.status = if resume { Status::Queued } else { Status::Paused };
                    l.message = Some("Interrupted - will resume".into());
                }
                if l.status == Status::Checking {
                    l.status = Status::New;
                }
                if l.status == Status::Waiting && l.wait_until.is_none() {
                    l.status = Status::Queued;
                }
            }
        }
        self.apply_limits();
        self.apply_schedule();
        let pending: Vec<String> = self.state.lock().links.values().filter(|l| l.status == Status::New).map(|l| l.id.clone()).collect();
        self.queue_checks(pending);
        let n = self.registry.read().all().len();
        let (nl, np) = {
            let st = self.state.lock();
            (st.links.len(), st.packages.len())
        };
        crate::log::info("core", &format!("Fetchwing core started: {nl} links, {np} packages, {n} plugins, data={}", self.data_dir.display()));

        // scheduler
        let c = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! { _ = c.tick.notified() => {}, _ = tokio::time::sleep(Duration::from_millis(1000)) => {} }
                tokio::time::sleep(Duration::from_millis(30)).await;
                c.schedule_now();
            }
        });
        // events + persistence
        let c = self.clone();
        tokio::spawn(async move {
            let mut i = 0u64;
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                i += 1;
                c.flush_events();
                if i % 4 == 0 {
                    let s = c.stats();
                    let _ = c.events.send(Event::Stats(s));
                    let ids: Vec<String> = c.running.lock().keys().cloned().collect();
                    c.changed_links.lock().extend(ids);
                    c.bump();
                }
                if i % 8 == 0 {
                    c.save();
                }
                if i % 120 == 0 {
                    c.apply_schedule();
                }
                if i % 240 == 0 {
                    c.check_disk();
                }
            }
        });
        // tools: keep yt-dlp current, probe 7-Zip
        let c = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            loop {
                c.refresh_tools(false).await;
                tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
            }
        });
        self.check_disk();
        self.kick();
    }

    pub async fn refresh_tools(self: &Arc<Self>, force: bool) {
        if self.settings.bool("links.ytdlp") && self.settings.bool("links.ytdlpManaged") {
            let last = self.state.lock().kv.get("tools.ytdlp.checked").and_then(|v| v.as_i64()).unwrap_or(0);
            match crate::tools::ensure_ytdlp(&self.data_dir, last, force, &self.settings.str("connection.userAgent")).await {
                Ok((_, v, _)) => {
                    self.state.lock().kv.insert("tools.ytdlp.checked".into(), json!(now_ms()));
                    self.dirty.store(true, Ordering::Relaxed);
                    self.sys.lock().ytdlp = Some(v);
                }
                Err(e) => crate::log::warn("tools", &format!("yt-dlp update failed: {e}")),
            }
        }
        if self.sys.lock().ytdlp.is_none() {
            let v = crate::tools::version(Path::new(&self.ytdlp_path())).await;
            self.sys.lock().ytdlp = v;
        }
        let sz = crate::postprocess::seven_zip_ok(&self.settings.str("archives.sevenZip")).await;
        self.sys.lock().seven_zip = sz;
        self.bump();
    }

    pub fn ytdlp_path(&self) -> String {
        let s = self.settings.str("links.ytdlpPath");
        if !s.is_empty() && s != "yt-dlp" {
            return s;
        }
        let m = crate::tools::managed_ytdlp(&self.data_dir);
        if self.settings.bool("links.ytdlpManaged") && m.exists() {
            m.to_string_lossy().into_owned()
        } else {
            "yt-dlp".into()
        }
    }

    pub fn save(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let bytes = {
            let mut st = self.state.lock();
            st.kv.insert("settings".into(), Value::Object(self.settings.overrides()));
            st.kv.insert("paused".into(), json!(self.paused.load(Ordering::Relaxed)));
            st.kv.insert("captcha.spend".into(), json!(*self.captcha.spend.lock()));
            serde_json::to_vec(&*st)
        };
        match bytes {
            Ok(b) => {
                let tmp = self.data_dir.join("state.json.tmp");
                if std::fs::write(&tmp, &b).and_then(|_| std::fs::rename(&tmp, self.data_dir.join("state.json"))).is_err() {
                    self.dirty.store(true, Ordering::Relaxed);
                }
            }
            Err(e) => crate::log::error("core", &format!("save failed: {e}")),
        }
    }

    pub async fn shutdown(&self) {
        for r in self.running.lock().values() {
            *r.reason.lock() = Some(Reason::Stop);
            r.cancel.cancel();
        }
        for _ in 0..100 {
            if self.running.lock().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.save();
    }

    pub fn reload_plugins(&self) -> usize {
        let r = Arc::new(Registry::new(Some(&self.data_dir.join("plugins"))));
        let n = r.all().len();
        for e in &r.user_errors {
            crate::log::warn("plugins", e);
        }
        *self.registry.write() = r;
        self.notify("info", "Plugins reloaded", &format!("{n} plugins active"), None);
        n
    }

    fn bump(&self) {
        self.version.fetch_add(1, Ordering::Relaxed);
    }
    pub fn kick(&self) {
        self.tick.notify_one();
    }
    pub fn notify(&self, kind: &str, title: &str, body: &str, link: Option<String>) {
        let n = Notice::new(kind, title, body, link);
        {
            let mut v = self.notices.lock();
            v.push(n.clone());
            if v.len() > 50 {
                v.remove(0);
            }
        }
        let _ = self.events.send(Event::Notify(n));
        self.bump();
    }
    fn mark_link(&self, id: &str) {
        self.changed_links.lock().insert(id.to_string());
        self.dirty.store(true, Ordering::Relaxed);
        self.bump();
    }
    fn mark_pkg(&self, id: &str) {
        self.changed_pkgs.lock().insert(id.to_string());
        self.dirty.store(true, Ordering::Relaxed);
        self.bump();
    }
    /// mutate a link under the lock and mark it changed; returns closure result
    fn with_link<R>(&self, id: &str, f: impl FnOnce(&mut Link) -> R) -> Option<R> {
        let r = {
            let mut st = self.state.lock();
            st.links.get_mut(id).map(f)
        };
        if r.is_some() {
            self.mark_link(id);
        }
        r
    }
    pub fn link(&self, id: &str) -> Option<Link> {
        self.state.lock().links.get(id).cloned()
    }

    pub fn settings_changed(&self, key: &str) {
        self.dirty.store(true, Ordering::Relaxed);
        if key == "downloads.speedLimit" {
            self.apply_limits();
        }
        if key == "schedule.rules" {
            self.apply_schedule();
        }
        let _ = self.events.send(Event::Settings);
        self.bump();
        self.kick();
    }

    // =====================================================================================
    // adding links
    // =====================================================================================

    pub fn add_links(self: &Arc<Self>, req: AddRequest) -> String {
        let job_id = new_id();
        self.jobs.lock().insert(job_id.clone(), JobView { id: job_id.clone(), source: req.source.clone(), started_at: now_ms(), ..Default::default() });
        let auto = req.auto_start.unwrap_or(if req.source == "cnl" || req.source == "clipboard" { false } else { self.settings.bool("general.autoStart") });
        let sink = Arc::new(AddSink { core: self.clone(), job: job_id.clone(), req_pkg: req.package_name.clone(), req_dir: req.dir.clone(), source: req.source.clone(), origin: req.origin.clone(), link_pw: req.link_password.clone(), passwords: req.passwords.clone(), auto, pkg_by_name: Mutex::new(HashMap::new()) });
        let reg = self.registry.read().clone();
        let c = self.clone();
        let mk: Arc<dyn Fn(&str, Vec<String>) -> Ctx + Send + Sync> = Arc::new(move |url: &str, pw: Vec<String>| c.make_ctx(url, None, Arc::new(Mutex::new(Map::new())), pw, None, CancellationToken::new()));
        let deep = self.settings.bool("links.deepCrawl");
        let yt = self.settings.bool("links.ytdlp");
        let c = self.clone();
        let jid = job_id.clone();
        tokio::spawn(async move {
            let (found, problems) = crawler::run(reg, req.text, req.passwords, deep, yt, mk, sink).await;
            let probs = {
                let mut j = c.jobs.lock();
                let v = j.get_mut(&jid);
                v.map(|v| { v.done = true; v.problems.clone() }).unwrap_or_default()
            };
            let body = if probs.is_empty() { String::new() } else { format!("{} problem(s): {}", problems, probs.iter().take(3).map(|p| p.1.clone()).collect::<Vec<_>>().join("; ")) };
            if found > 0 {
                c.notify("success", &format!("Added {found} link{}", if found == 1 { "" } else { "s" }), &body, None);
            } else {
                c.notify("warning", "No links added", &body, None);
            }
        });
        job_id
    }

    pub fn job(&self, id: &str) -> Option<JobView> {
        self.jobs.lock().get(id).cloned()
    }

    fn create_package(&self, name: &str, dir: Option<&str>, source: &str, passwords: Vec<String>) -> Package {
        let base = PathBuf::from(self.settings.str("general.downloadDir"));
        let target = match dir {
            Some(d) if Path::new(d).is_absolute() => PathBuf::from(d),
            Some(d) => base.join(d),
            None if self.settings.bool("general.packageSubfolders") => base.join(sanitize_dir(name)),
            None => base,
        };
        let mut st = self.state.lock();
        let order = st.packages.values().map(|p| p.order).max().unwrap_or(0) + 1;
        let mut pw = passwords;
        pw.dedup();
        let p = Package { id: new_id(), name: name.into(), dir: target.to_string_lossy().into_owned(), stage: Stage::Inbox, order, priority: 0, expanded: true, comment: None, passwords: pw, auto_extract: true, added_at: now_ms(), source: source.into(), auto_name: false };
        st.packages.insert(p.id.clone(), p.clone());
        drop(st);
        self.mark_pkg(&p.id);
        p
    }

    // =====================================================================================
    // online check (Inbox)
    // =====================================================================================

    pub fn queue_checks(self: &Arc<Self>, ids: Vec<String>) {
        for id in ids {
            if !self.checking.lock().insert(id.clone()) {
                continue;
            }
            let c = self.clone();
            tokio::spawn(async move {
                let _permit = c.check_sem.clone().acquire_owned().await;
                c.check_one(&id).await;
                c.checking.lock().remove(&id);
            });
        }
    }

    async fn check_one(self: &Arc<Self>, id: &str) {
        let Some(l) = self.link(id) else { return };
        let Some(plugin) = self.registry.read().get(&l.plugin_id) else {
            self.with_link(id, |l| l.status = Status::Unknown);
            return;
        };
        self.with_link(id, |l| l.status = Status::Checking);
        let props = Arc::new(Mutex::new(l.props.clone()));
        let cancel = CancellationToken::new();
        let mut ctx = self.make_ctx(&l.url, Some(id.to_string()), props.clone(), l.passwords.clone(), None, cancel.clone());
        ctx.name = l.name.clone();
        let res = tokio::time::timeout(Duration::from_secs(90), plugin.check(&mut ctx)).await;
        cancel.cancel();
        let props = props.lock().clone();
        let mut rename_pkg = None;
        let dup = {
            let mut st = self.state.lock();
            let mut dup = false;
            let canon_taken = |st: &State, canon: &str, me: &str| st.links.values().any(|o| o.id != me && (o.url == canon || o.props.get("canonical").and_then(|v| v.as_str()) == Some(canon)));
            let canon = match &res {
                Ok(Ok(Some(info))) => info.canonical.clone(),
                _ => None,
            };
            if let Some(c) = &canon {
                dup = canon_taken(&st, c, id);
            }
            let Some(l) = st.links.get_mut(id) else { return };
            l.props = props;
            match res {
                Ok(Ok(Some(info))) => {
                    l.status = match info.status { Avail::Online => Status::Online, Avail::Offline => Status::Offline, Avail::Unknown => Status::Unknown };
                    if let Some(n) = info.name.filter(|n| !n.is_empty()) {
                        if l.props.contains_key("nameFromUrl") || l.name == "download" {
                            l.name = sanitize_filename(&n);
                            l.props.remove("nameFromUrl");
                            rename_pkg = Some(l.package_id.clone());
                        }
                    }
                    if let Some(s) = info.size.filter(|s| *s > 0) {
                        l.size = s;
                    }
                    if info.hash.is_some() {
                        l.hash = info.hash;
                    }
                    if let Some(c) = canon {
                        l.props.insert("canonical".into(), json!(c));
                    }
                    l.message = if l.status == Status::Offline { Some("File is offline".into()) } else { None };
                    if dup {
                        l.enabled = false;
                        l.message = Some("Duplicate of a link already in your list".into());
                    }
                }
                Ok(Ok(None)) => l.status = Status::Unknown,
                Ok(Err(e)) if e.code == ErrCode::Offline => {
                    l.status = Status::Offline;
                    l.message = Some(e.msg);
                }
                Ok(Err(e)) => {
                    l.status = Status::Unknown;
                    l.message = Some(format!("Check failed: {}", e.msg).chars().take(200).collect());
                }
                Err(_) => {
                    l.status = Status::Unknown;
                    l.message = Some("Check timed out".into());
                }
            }
            dup
        };
        let _ = dup;
        self.mark_link(id);
        if let Some(pid) = rename_pkg {
            self.rename_auto_package(&pid);
        }
        self.schedule_auto_confirm(vec![id.to_string()]);
    }

    fn rename_auto_package(&self, pid: &str) {
        let dl_dir = self.settings.str("general.downloadDir");
        let sub = self.settings.bool("general.packageSubfolders");
        let mut st = self.state.lock();
        let members: Vec<&Link> = st.links.values().filter(|l| l.package_id == pid).collect();
        if members.len() != 1 {
            if let Some(p) = st.packages.get_mut(pid) {
                p.auto_name = false;
            }
            return;
        }
        let (lname, started) = (members[0].name.clone(), members[0].bytes_done > 0);
        let Some(p) = st.packages.get_mut(pid) else { return };
        if !p.auto_name {
            return;
        }
        let base = archive_base(&lname).unwrap_or_else(|| util::http::re(r"\.[^.]{1,8}$").replace(&lname, "").into_owned());
        p.name = clean_package_name(&base);
        p.auto_name = false;
        if sub && !started && Path::new(&p.dir).parent() == Some(Path::new(&dl_dir)) {
            p.dir = Path::new(&dl_dir).join(sanitize_dir(&p.name)).to_string_lossy().into_owned();
        }
        drop(st);
        self.mark_pkg(pid);
    }

    fn schedule_auto_confirm(self: &Arc<Self>, ids: Vec<String>) {
        let go: Vec<String> = {
            let st = self.state.lock();
            ids.into_iter().filter(|id| st.links.get(id).is_some_and(|l| l.stage == Stage::Inbox && l.props.get("autoStart") == Some(&Value::Bool(true)))).collect()
        };
        if go.is_empty() {
            return;
        }
        let delay = self.settings.u64("general.autoStartDelay");
        let c = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            let ok: Vec<String> = {
                let st = c.state.lock();
                go.into_iter().filter(|id| st.links.get(id).is_some_and(|l| l.stage == Stage::Inbox && l.props.get("autoStart") == Some(&Value::Bool(true)) && matches!(l.status, Status::Online | Status::Unknown))).collect()
            };
            if !ok.is_empty() {
                c.confirm(&ok);
            }
        });
    }

    // =====================================================================================
    // user actions
    // =====================================================================================

    /// Inbox -> Queue (JD2: "Start downloads" from the LinkGrabber).
    pub fn confirm(&self, ids: &[String]) -> usize {
        let mut n = 0;
        let mut pkgs = HashSet::new();
        {
            let mut st = self.state.lock();
            for id in ids {
                let Some(l) = st.links.get_mut(id) else { continue };
                if l.stage != Stage::Inbox || matches!(l.status, Status::Offline | Status::Checking) {
                    continue;
                }
                l.stage = Stage::Queue;
                l.status = Status::Queued;
                l.message = None;
                l.props.remove("autoStart");
                pkgs.insert(l.package_id.clone());
                n += 1;
            }
            for pid in &pkgs {
                if let Some(p) = st.packages.get_mut(pid) {
                    if p.stage == Stage::Inbox {
                        p.stage = Stage::Queue;
                    }
                }
            }
        }
        for id in ids {
            self.mark_link(id);
        }
        for p in pkgs {
            self.mark_pkg(&p);
        }
        self.kick();
        n
    }
    pub fn links_of(&self, pkg_ids: &[String]) -> Vec<String> {
        self.state.lock().links.values().filter(|l| pkg_ids.contains(&l.package_id)).map(|l| l.id.clone()).collect()
    }

    pub fn start_all(&self) {
        self.paused.store(false, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Relaxed);
        self.bump();
        self.kick();
    }
    pub fn pause_all(&self) {
        self.paused.store(true, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Relaxed);
        for r in self.running.lock().values() {
            *r.reason.lock() = Some(Reason::Stop);
            r.cancel.cancel();
        }
        self.bump();
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn pause_links(&self, ids: &[String]) {
        for id in ids {
            if let Some(r) = self.running.lock().get(id) {
                *r.reason.lock() = Some(Reason::Pause);
                r.cancel.cancel();
            }
            self.captcha.cancel_for_link(id);
            self.with_link(id, |l| {
                if l.stage == Stage::Queue && l.status != Status::Finished {
                    l.status = Status::Paused;
                    l.message = Some("Paused".into());
                }
            });
        }
    }
    pub fn resume_links(&self, ids: &[String]) {
        for id in ids {
            self.with_link(id, |l| {
                if l.stage == Stage::Queue && matches!(l.status, Status::Paused | Status::Failed | Status::Waiting | Status::Skipped) {
                    l.status = Status::Queued;
                    l.message = None;
                    l.error = None;
                    l.wait_until = None;
                    l.attempts = 0;
                }
            });
        }
        self.kick();
    }
    pub fn force_start(self: &Arc<Self>, ids: &[String]) {
        self.confirm(ids);
        self.resume_links(ids);
        for id in ids {
            let ok = self.link(id).is_some_and(|l| l.status == Status::Queued) && !self.running.lock().contains_key(id);
            if ok {
                self.clone().spawn_run(id.clone());
            }
        }
    }
    pub fn reset_links(&self, ids: &[String]) {
        for id in ids {
            if let Some(r) = self.running.lock().get(id) {
                *r.reason.lock() = Some(Reason::Restart);
                r.cancel.cancel();
            }
            self.with_link(id, |l| {
                if let Some(p) = l.prop_str("partPath") {
                    let _ = std::fs::remove_file(p);
                }
                l.bytes_done = 0;
                l.segments.clear();
                l.attempts = 0;
                l.error = None;
                l.message = None;
                l.wait_until = None;
                l.finished_at = None;
                for k in ["partPath", "decryptedUpTo", "finalPath"] {
                    l.props.remove(k);
                }
                if l.stage == Stage::Done {
                    l.stage = Stage::Queue;
                }
                l.status = if l.stage == Stage::Queue { Status::Queued } else { Status::New };
            });
        }
        self.kick();
    }
    pub fn remove_links(&self, ids: &[String], delete_files: bool) {
        let mut touched = HashSet::new();
        {
            for id in ids {
                if let Some(r) = self.running.lock().get(id) {
                    *r.reason.lock() = Some(Reason::Remove);
                    r.cancel.cancel();
                }
                self.captcha.cancel_for_link(id);
            }
            let mut st = self.state.lock();
            for id in ids {
                if let Some(l) = st.links.remove(id) {
                    if let Some(p) = l.prop_str("partPath") {
                        let _ = std::fs::remove_file(p);
                    }
                    if delete_files && l.status == Status::Finished {
                        if let Some(p) = l.prop_str("finalPath") {
                            let _ = std::fs::remove_file(p);
                        }
                    }
                    touched.insert(l.package_id);
                }
            }
            let mut removed_pkgs = vec![];
            for pid in &touched {
                if !st.links.values().any(|l| &l.package_id == pid) {
                    st.packages.remove(pid);
                    removed_pkgs.push(pid.clone());
                }
            }
            if !removed_pkgs.is_empty() {
                let _ = self.events.send(Event::PackagesRemoved(removed_pkgs));
            }
        }
        let _ = self.events.send(Event::LinksRemoved(ids.to_vec()));
        for pid in touched {
            self.refresh_pkg_stage(&pid);
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.bump();
    }
    pub fn remove_packages(&self, pids: &[String], delete_files: bool) {
        let ids = self.links_of(pids);
        self.remove_links(&ids, delete_files);
        let mut st = self.state.lock();
        for p in pids {
            st.packages.remove(p);
        }
        drop(st);
        let _ = self.events.send(Event::PackagesRemoved(pids.to_vec()));
        self.bump();
    }
    pub fn cleanup(&self, what: &str) -> usize {
        let ids: Vec<String> = self.state.lock().links.values().filter(|l| match what {
            "finished" => l.status == Status::Finished || l.status == Status::Skipped,
            "offline" => l.status == Status::Offline,
            _ => l.status == Status::Failed,
        }).map(|l| l.id.clone()).collect();
        self.remove_links(&ids, false);
        ids.len()
    }
    pub fn set_enabled(&self, ids: &[String], on: bool) {
        for id in ids {
            self.with_link(id, |l| l.enabled = on);
        }
        self.kick();
    }
    pub fn set_priority(&self, ids: &[String], p: i32) {
        for id in ids {
            self.with_link(id, |l| l.priority = p.clamp(-2, 2));
        }
        self.kick();
    }
    pub fn update_package(&self, id: &str, name: Option<String>, dir: Option<String>, passwords: Option<Vec<String>>, priority: Option<i32>, expanded: Option<bool>) -> Result<(), String> {
        let started = self.state.lock().links.values().any(|l| l.package_id == id && l.bytes_done > 0);
        let mut st = self.state.lock();
        let p = st.packages.get_mut(id).ok_or("package not found")?;
        if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
            p.name = n;
            p.auto_name = false;
        }
        if let Some(d) = dir.filter(|d| d != &p.dir) {
            if started {
                return Err("Cannot change the folder of a package that already started downloading".into());
            }
            p.dir = d;
        }
        if let Some(pw) = passwords {
            p.passwords = pw;
        }
        if let Some(pr) = priority {
            p.priority = pr.clamp(-2, 2);
        }
        if let Some(e) = expanded {
            p.expanded = e;
        }
        drop(st);
        self.mark_pkg(id);
        self.kick();
        Ok(())
    }
    pub fn set_link_password(&self, id: &str, pw: &str) {
        self.with_link(id, |l| { l.props.insert("linkPassword".into(), json!(pw)); });
    }
    pub fn set_speed_limit(&self, bps: u64) {
        let _ = self.settings.set("downloads.speedLimit", json!(bps));
        self.settings_changed("downloads.speedLimit");
    }

    // =====================================================================================
    // scheduler
    // =====================================================================================

    fn schedule_now(self: &Arc<Self>) {
        if self.paused.load(Ordering::Relaxed) || self.schedule_paused.load(Ordering::Relaxed) || self.disk_low.load(Ordering::Relaxed) {
            return;
        }
        let max_active = self.settings.u64("downloads.maxActive").max(1) as usize;
        let max_host = self.settings.u64("downloads.maxPerHost").max(1) as usize;
        let now = now_ms();
        let running: HashSet<String> = self.running.lock().keys().cloned().collect();
        if running.len() >= max_active {
            return;
        }
        let reg = self.registry.read().clone();
        let mut to_start = vec![];
        {
            let mut st = self.state.lock();
            let mut per_host: HashMap<String, usize> = HashMap::new();
            for id in &running {
                if let Some(l) = st.links.get(id) {
                    *per_host.entry(l.host.clone()).or_default() += 1;
                }
            }
            let mut pkgs: Vec<(String, i32, i64)> = st.packages.values().filter(|p| p.stage != Stage::Inbox).map(|p| (p.id.clone(), p.priority, p.order)).collect();
            pkgs.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
            let pkg_rank: HashMap<String, usize> = pkgs.iter().enumerate().map(|(i, p)| (p.0.clone(), i)).collect();
            let mut cands: Vec<(usize, i32, i64, String)> = st.links.values()
                .filter(|l| l.stage == Stage::Queue && l.enabled && !running.contains(&l.id))
                .filter(|l| l.status == Status::Queued || (matches!(l.status, Status::Waiting | Status::Failed) && l.wait_until.is_some() && l.error.as_ref().is_none_or(|e| e.retryable)))
                .map(|l| (pkg_rank.get(&l.package_id).copied().unwrap_or(usize::MAX), l.priority, l.added_at, l.id.clone()))
                .collect();
            cands.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
            let blocked = self.host_blocked.lock().clone();
            let accounts = st.accounts.clone();
            for (_, _, _, id) in cands {
                if running.len() + to_start.len() >= max_active {
                    break;
                }
                let Some(l) = st.links.get_mut(&id) else { continue };
                if l.wait_until.is_some_and(|w| w > now) {
                    continue;
                }
                let plugin = reg.get(&l.plugin_id);
                let has_acc = has_account(&accounts, &reg, l, plugin.as_ref());
                if let Some((until, why)) = blocked.get(&l.host).filter(|b| b.0 > now) {
                    if !has_acc {
                        if l.message.as_deref() != Some(why.as_str()) {
                            l.status = Status::Waiting;
                            l.wait_until = Some(*until);
                            l.message = Some(why.clone());
                            self.changed_links.lock().insert(id.clone());
                        }
                        continue;
                    }
                }
                let host_limit = if has_acc { max_host } else { max_host.min(plugin.as_ref().and_then(|p| p.meta().limits.max_downloads).unwrap_or(usize::MAX)) };
                let n = per_host.entry(l.host.clone()).or_default();
                if *n >= host_limit {
                    continue;
                }
                *n += 1;
                to_start.push(id);
            }
        }
        for id in to_start {
            self.clone().spawn_run(id);
        }
    }

    fn spawn_run(self: Arc<Self>, id: String) {
        let run = Running { cancel: CancellationToken::new(), speed: Arc::new(SpeedMeter::new(3000)), ext_speed: Arc::new(AtomicU64::new(0)), reason: Arc::new(Mutex::new(None)) };
        {
            let mut r = self.running.lock();
            if r.contains_key(&id) {
                return;
            }
            r.insert(id.clone(), Running { cancel: run.cancel.clone(), speed: run.speed.clone(), ext_speed: run.ext_speed.clone(), reason: run.reason.clone() });
        }
        tokio::spawn(async move {
            let res = self.run_link(&id, &run).await;
            if let Err(e) = res {
                self.handle_error(&id, e, &run);
            }
            self.running.lock().remove(&id);
            self.mark_link(&id);
            self.kick();
        });
    }

    fn pick_account(&self, l: &Link, plugin: Option<&P>) -> Option<(Account, PluginAccount)> {
        let st = self.state.lock();
        let mut domains: Vec<String> = vec![l.host.clone()];
        if let Some(p) = plugin {
            domains.extend(p.meta().domains.iter().cloned());
        }
        st.accounts.iter().find(|a| a.enabled && domains.contains(&a.host) && !matches!(a.status, AccountStatus::Invalid | AccountStatus::Expired) && a.traffic_left != Some(0))
            .map(|a| (a.clone(), PluginAccount { id: a.id.clone(), user: a.user.clone(), password: self.vault.decrypt(&a.secret), premium: a.premium.unwrap_or(false) }))
    }
    fn pick_debrid(&self, l: &Link, plugin: Option<&P>) -> Option<(Account, PluginAccount, P)> {
        let reg = self.registry.read().clone();
        let st = self.state.lock();
        let mut cand = vec![l.host.clone()];
        if let Some(p) = plugin {
            cand.extend(p.meta().domains.iter().cloned());
        }
        for mh in reg.multihosts() {
            for a in &st.accounts {
                if !a.enabled || matches!(a.status, AccountStatus::Invalid | AccountStatus::Expired) || !mh.meta().domains.contains(&a.host) {
                    continue;
                }
                if a.supported_hosts.iter().any(|h| cand.iter().any(|c| c.eq_ignore_ascii_case(h))) {
                    return Some((a.clone(), PluginAccount { id: a.id.clone(), user: a.user.clone(), password: self.vault.decrypt(&a.secret), premium: true }, mh.clone()));
                }
            }
        }
        None
    }

    // =====================================================================================
    // running a download
    // =====================================================================================

    async fn run_link(self: &Arc<Self>, id: &str, run: &Running) -> PResult<()> {
        let Some(l) = self.with_link(id, |l| {
            l.status = Status::Downloading;
            l.message = Some("Starting…".into());
            l.error = None;
            l.wait_until = None;
            l.attempts += 1;
            l.clone()
        }) else { return Ok(()) };
        let pkg = self.state.lock().packages.get(&l.package_id).cloned().ok_or_else(|| PluginError::new(ErrCode::Defect, "package missing"))?;
        let reg = self.registry.read().clone();
        let plugin = reg.get(&l.plugin_id).or_else(|| reg.find_host(&l.url)).ok_or_else(|| plugins::unsupported("No plugin for this link"))?;
        let direct = self.pick_account(&l, Some(&plugin));
        let debrid = if direct.is_none() { self.pick_debrid(&l, Some(&plugin)) } else { None };
        let props = Arc::new(Mutex::new(l.props.clone()));
        let mut ctx = self.make_ctx(&l.url, Some(id.to_string()), props.clone(), l.passwords.clone(), direct.as_ref().map(|d| d.1.clone()), run.cancel.clone());
        ctx.name = l.name.clone();
        ctx.size = l.size;
        let res = if let Some((a, _)) = &direct {
            crate::log::info(&format!("dl/{}", l.host), &format!("using account {}@{}", a.user, a.host));
            plugin.resolve(&mut ctx).await
        } else if let Some((_, acc, mh)) = &debrid {
            self.with_link(id, |l| l.message = Some(format!("Unlocking via {}…", mh.meta().name)));
            match mh.unrestrict(&mut ctx, acc).await {
                Ok(Some(r)) => Ok(r),
                Ok(None) => plugin.resolve(&mut ctx).await,
                Err(e) if matches!(e.code, ErrCode::Unsupported | ErrCode::Retry) => {
                    crate::log::warn(&format!("dl/{}", l.host), &format!("{} failed ({}) - trying free download", mh.meta().name, e.msg));
                    plugin.resolve(&mut ctx).await
                }
                Err(e) => Err(e),
            }
        } else {
            plugin.resolve(&mut ctx).await
        };
        let new_props = props.lock().clone();
        self.with_link(id, |l| l.props = new_props);
        let res = res?;

        let sub_dir = l.prop_str("subdir").map(|s| s.split('/').map(sanitize_dir).collect::<PathBuf>());
        let dir = match sub_dir {
            Some(s) => PathBuf::from(&pkg.dir).join(s),
            None => PathBuf::from(&pkg.dir),
        };
        std::fs::create_dir_all(&dir).map_err(|e| PluginError::new(ErrCode::Defect, format!("Cannot create {}: {e}", dir.display())).hint("Check the download folder and its permissions."))?;
        let log_scope = format!("dl/{}", l.host);

        let final_path = match res {
            Resolution::Http { req, name, size, hash, post } => {
                let (name_now, part) = self.with_link(id, |l| {
                    if let Some(n) = name.filter(|_| l.props.contains_key("nameFromUrl") || l.name == "download") {
                        l.name = sanitize_filename(&n);
                        l.props.remove("nameFromUrl");
                    }
                    if let Some(s) = size.filter(|s| *s > 0) {
                        l.size = s;
                    }
                    if hash.is_some() {
                        l.hash = hash.clone();
                    }
                    if l.prop_str("partPath").is_none() {
                        l.props.insert("partPath".into(), json!(dir.join(format!("{}.part", l.name)).to_string_lossy()));
                    }
                    l.message = None;
                    (l.name.clone(), PathBuf::from(l.prop_str("partPath").unwrap_or_default()))
                }).unwrap_or_default();
                let _ = name_now;
                let lnow = self.link(id).ok_or_else(|| PluginError::new(ErrCode::Cancelled, "removed"))?;
                if lnow.size > 0 {
                    let need = (lnow.size as u64).saturating_sub(lnow.bytes_done);
                    let free = util::free_space(&dir);
                    if free < need + 64 * 1024 * 1024 {
                        return Err(PluginError::new(ErrCode::Defect, format!("Not enough disk space: needs {}, only {} free", fmt_bytes(need as i64), fmt_bytes(free as i64)))
                            .hint("Free up space or change the package folder (right-click the package), then Retry."));
                    }
                }
                let premium = direct.is_some() || debrid.is_some();
                let plugin_cap = req.max_connections.or(if premium { plugin.meta().limits.max_conns_premium } else { plugin.meta().limits.max_conns }).unwrap_or(32);
                let conns = (self.settings.u64("downloads.connections").max(1) as usize).min(plugin_cap);
                let state = Arc::new(Mutex::new(DlState::new(lnow.segments.clone(), lnow.size, lnow.etag.clone(), lnow.last_modified.clone(), lnow.resumable)));
                let limiter = Arc::new(TokenBucket::new(0));
                let (c1, c2, c3) = (self.clone(), self.clone(), self.clone());
                let (i1, i2, i3) = (id.to_string(), id.to_string(), id.to_string());
                let speed = run.speed.clone();
                let gs = self.global_speed.clone();
                let job = DlJob {
                    req,
                    browser: self.browser_for(&l.host),
                    part_path: part.clone(),
                    state: state.clone(),
                    connections: conns,
                    min_segment: self.settings.u64("downloads.minSegment").max(65536),
                    preallocate: self.settings.bool("downloads.preallocate"),
                    limiters: vec![limiter, self.global_limiter.clone()],
                    cancel: run.cancel.clone(),
                    on_progress: Arc::new(move |n| {
                        speed.add(n);
                        gs.add(n);
                        c1.session_bytes.fetch_add(n, Ordering::Relaxed);
                        if let Some(l) = c1.state.lock().links.get_mut(&i1) {
                            l.bytes_done += n;
                        }
                        c1.changed_links.lock().insert(i1.clone());
                    }),
                    on_meta: Arc::new(move |m| {
                        c2.with_link(&i2, |l| {
                            if m.size > 0 {
                                l.size = m.size;
                            }
                            if let Some(f) = m.filename.filter(|_| l.props.contains_key("nameFromUrl")) {
                                l.name = sanitize_filename(&f);
                                l.props.remove("nameFromUrl");
                            }
                            l.resumable = m.resumable;
                        });
                    }),
                    persist: Arc::new(move |s: &DlState| {
                        let segs = s.segments.clone();
                        let (etag, lm, res, done) = (s.etag.clone(), s.last_modified.clone(), s.resumable, s.bytes_done());
                        if let Some(l) = c3.state.lock().links.get_mut(&i3) {
                            l.segments = segs;
                            l.etag = etag;
                            l.last_modified = lm;
                            l.resumable = res;
                            l.bytes_done = done;
                        }
                        c3.dirty.store(true, Ordering::Relaxed);
                    }),
                };
                engine::download(&job).await.map_err(dl_to_plugin)?;
                let size = state.lock().size;
                // decide the final name now (the server may have told us a better one)
                let name = self.link(id).map(|l| l.name).unwrap_or_default();
                let Some(target) = self.place_final(&dir.join(&name), id) else { return Ok(()) };
                for step in post {
                    match step {
                        plugins::PostStep::MegaDecrypt { key, iv } => {
                            self.with_link(id, |l| { l.status = Status::Postprocessing; l.message = Some("Decrypting (MEGA)…".into()); });
                            let from = self.link(id).and_then(|l| l.props.get("decryptedUpTo").and_then(|v| v.as_u64())).unwrap_or(0);
                            let (c, i, p, cancel) = (self.clone(), id.to_string(), part.clone(), run.cancel.clone());
                            let total = size.max(1) as u64;
                            tokio::task::spawn_blocking(move || crate::plugins::mega::decrypt_in_place(&p, key, iv, from, &cancel, &|done| {
                                if let Some(l) = c.state.lock().links.get_mut(&i) {
                                    l.props.insert("decryptedUpTo".into(), json!(done));
                                    l.message = Some(format!("Decrypting {}%", done * 100 / total));
                                }
                                c.changed_links.lock().insert(i.clone());
                            }))
                            .await
                            .map_err(|e| PluginError::new(ErrCode::Retry, e.to_string()))?
                            .map_err(|e| PluginError::new(ErrCode::Retry, format!("MEGA decrypt: {e}")))?;
                        }
                    }
                }
                std::fs::rename(&part, &target).map_err(|e| PluginError::new(ErrCode::Defect, format!("Rename failed: {e}")))?;
                self.with_link(id, |l| { l.size = size; l.bytes_done = size.max(0) as u64; });
                target
            }
            Resolution::Ytdlp { url, args } => {
                self.with_link(id, |l| l.message = Some("Starting yt-dlp…".into()));
                let base = util::http::re(r"\.[^.]{1,5}$").replace(&l.name, "").into_owned();
                let tpl = dir.join(format!("{}.%(ext)s", sanitize_filename(&base))).to_string_lossy().into_owned();
                let (c, i, ext, gs) = (self.clone(), id.to_string(), run.ext_speed.clone(), self.global_speed.clone());
                let p = plugins::ytdlp::run_download(&self.ytdlp_path(), &url, &self.settings.str("links.ytdlpFormat"), &tpl, &args, &run.cancel, &move |done, total, speed| {
                    ext.store(speed, Ordering::Relaxed);
                    let mut st = c.state.lock();
                    if let Some(l) = st.links.get_mut(&i) {
                        if total > 0 {
                            l.size = total as i64;
                        }
                        if done > l.bytes_done {
                            gs.add(done - l.bytes_done);
                            c.session_bytes.fetch_add(done - l.bytes_done, Ordering::Relaxed);
                        }
                        l.bytes_done = done;
                        l.message = None;
                    }
                    drop(st);
                    c.changed_links.lock().insert(i.clone());
                })
                .await?;
                let size = std::fs::metadata(&p).map(|m| m.len() as i64).unwrap_or(-1);
                self.with_link(id, |l| { l.name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(); l.size = size; l.bytes_done = size.max(0) as u64; });
                p
            }
        };
        self.with_link(id, |l| { l.props.insert("finalPath".into(), json!(final_path.to_string_lossy())); l.props.remove("partPath"); });

        // hash verification
        let hash = self.link(id).and_then(|l| l.hash);
        if let Some(h) = hash.filter(|_| self.settings.bool("downloads.verifyHash")) {
            self.with_link(id, |l| { l.status = Status::Postprocessing; l.message = Some(format!("Verifying {:?}…", h.algo)); });
            let p = final_path.clone();
            let algo = h.algo;
            let got = tokio::task::spawn_blocking(move || crate::postprocess::hash_file(&p, algo)).await.ok().and_then(|r| r.ok()).unwrap_or_default();
            let ok = got.eq_ignore_ascii_case(&h.value);
            self.with_link(id, |l| { if let Some(hh) = l.hash.as_mut() { hh.verified = Some(ok); } });
            if !ok {
                let _ = std::fs::rename(&final_path, final_path.with_extension("corrupt"));
                self.with_link(id, |l| { l.segments.clear(); l.bytes_done = 0; l.props.remove("finalPath"); });
                return Err(PluginError::new(ErrCode::Retry, format!("Checksum mismatch ({:?})", h.algo)).hint("The file is corrupt; it will be downloaded again."));
            }
        }
        let (name, size, pid, verified) = self.with_link(id, |l| {
            l.status = Status::Finished;
            l.stage = Stage::Done;
            l.message = if l.hash.as_ref().and_then(|h| h.verified) == Some(true) { Some("Checksum OK".into()) } else { None };
            l.finished_at = Some(now_ms());
            l.segments.clear();
            (l.name.clone(), l.size, l.package_id.clone(), l.hash.as_ref().and_then(|h| h.verified))
        }).unwrap_or_default();
        let _ = verified;
        crate::log::info(&log_scope, &format!("finished {name} ({})", fmt_bytes(size)));
        self.skip_mirrors(id);
        self.refresh_pkg_stage(&pid);
        self.on_package_maybe_done(&pid);
        Ok(())
    }

    /// returns None if skipped (file exists + policy skip)
    fn place_final(&self, target: &Path, id: &str) -> Option<PathBuf> {
        let policy = self.settings.str("general.fileExists");
        let taken_paths: HashSet<String> = self.state.lock().links.values().filter(|o| o.id != id).filter_map(|o| o.prop_str("finalPath").map(str::to_string)).collect();
        let taken = |p: &Path| taken_paths.contains(&p.to_string_lossy().into_owned());
        if target.exists() || taken(target) {
            match policy.as_str() {
                "skip" => {
                    self.with_link(id, |l| {
                        if let Some(p) = l.prop_str("partPath") {
                            let _ = std::fs::remove_file(p);
                        }
                        l.status = Status::Skipped;
                        l.stage = Stage::Done;
                        l.message = Some("File already exists - skipped".into());
                    });
                    return None;
                }
                "overwrite" => {
                    let _ = std::fs::remove_file(target);
                    return Some(target.to_path_buf());
                }
                _ => return Some(util::unique_path(target, taken)),
            }
        }
        Some(target.to_path_buf())
    }

    fn handle_error(self: &Arc<Self>, id: &str, e: PluginError, run: &Running) {
        let reason = *run.reason.lock();
        let now = now_ms();
        let retries = self.settings.u64("downloads.retries") as u32;
        let backoff_s = self.settings.u64("downloads.retryBackoff").max(1) as i64;
        let has_acc = {
            let l = self.link(id);
            l.map(|l| { let reg = self.registry.read().clone(); let p = reg.get(&l.plugin_id); has_account(&self.state.lock().accounts, &reg, &l, p.as_ref()) }).unwrap_or(false)
        };
        let mut block_host: Option<(String, i64, String)> = None;
        let mut notify: Option<(String, String)> = None;
        let mut invalid_acc = false;
        let r = self.with_link(id, |l| {
            match reason {
                Some(Reason::Pause) => { l.status = Status::Paused; l.message = Some("Paused".into()); return; }
                Some(Reason::Stop) => { l.status = Status::Queued; l.message = Some("Stopped".into()); return; }
                Some(Reason::Remove) | Some(Reason::Restart) => return,
                None => {}
            }
            let can_retry = l.attempts <= retries;
            let backoff = (backoff_s * 1000 * 2i64.pow(l.attempts.saturating_sub(1).min(16))).min(MAX_BACKOFF);
            let mut set = |status: Status, code: &str, msg: &str, retryable: bool, hint: Option<String>, wait: Option<i64>| {
                l.status = status;
                l.error = Some(LinkError { code: code.into(), message: msg.into(), hint, retryable });
                l.message = Some(msg.chars().take(300).collect());
                l.wait_until = wait.map(|w| now + w);
            };
            match e.code {
                ErrCode::Cancelled => { l.status = Status::Queued; l.message = Some("Interrupted".into()); }
                ErrCode::Offline => set(Status::Failed, "OFFLINE", &e.msg, false, e.hint.clone(), None),
                ErrCode::HosterWait => {
                    let ms = e.wait_ms.unwrap_or(900_000) as i64;
                    set(Status::Waiting, "QUOTA", &e.msg, true, Some("The hoster limits free downloads. An account or a debrid service removes the wait.".into()), Some(ms));
                    if !has_acc {
                        block_host = Some((l.host.clone(), now + ms, e.msg.clone()));
                    }
                    l.attempts = l.attempts.saturating_sub(1);
                }
                ErrCode::Quota => set(Status::Waiting, "QUOTA", &e.msg, true, Some("Traffic quota exhausted.".into()), Some(3_600_000)),
                ErrCode::PremiumOnly => set(Status::Failed, "PREMIUM_ONLY", &e.msg, false, e.hint.clone(), None),
                ErrCode::AccountInvalid => { invalid_acc = true; set(Status::Queued, "ACCOUNT", &e.msg, true, Some("The account was disabled; check it under Accounts.".into()), None); }
                ErrCode::Password => set(Status::Failed, "PASSWORD", &e.msg, false, e.hint.clone(), None),
                ErrCode::Unsupported => set(Status::Failed, "UNSUPPORTED", &e.msg, false, e.hint.clone(), None),
                ErrCode::CaptchaSkipped => set(Status::Waiting, "CAPTCHA", &e.msg, true, Some("It will ask again later. Configure a solver service to automate captchas.".into()), Some(600_000)),
                ErrCode::CaptchaFailed => set(Status::Failed, "CAPTCHA", &e.msg, can_retry, e.hint.clone(), Some(backoff)),
                ErrCode::Defect => set(Status::Failed, "PLUGIN_DEFECT", &e.msg, can_retry, e.hint.clone().or(Some("The site probably changed.".into())), Some(backoff * 3)),
                ErrCode::Retry => {
                    let hint = e.hint.clone().or(Some(if can_retry { format!("Retrying in {}s", backoff / 1000) } else { "Gave up after several retries. Use Retry to try again.".into() }));
                    set(Status::Failed, "NETWORK", &e.msg, can_retry, hint, Some(backoff))
                }
            }
            if l.status == Status::Failed && l.error.as_ref().is_some_and(|x| !x.retryable) {
                notify = Some((format!("Failed: {}", l.name), l.error.as_ref().and_then(|x| x.hint.clone()).unwrap_or(l.error.as_ref().map(|x| x.message.clone()).unwrap_or_default())));
            }
            crate::log::warn(&format!("dl/{}", l.host), &format!("{}: {} {}", l.name, l.error.as_ref().map(|x| x.code.as_str()).unwrap_or(""), l.error.as_ref().map(|x| x.message.as_str()).unwrap_or("")));
        });
        if r.is_none() {
            return;
        }
        if let Some((h, until, why)) = block_host {
            self.host_blocked.lock().insert(h, (until, why));
        }
        if invalid_acc {
            if let Some(l) = self.link(id) {
                let reg = self.registry.read().clone();
                let p = reg.get(&l.plugin_id);
                let acc = self.pick_account(&l, p.as_ref()).map(|a| a.0.id).or_else(|| self.pick_debrid(&l, p.as_ref()).map(|a| a.0.id));
                if let Some(aid) = acc {
                    self.update_account(&aid, |a| { a.status = AccountStatus::Invalid; a.message = Some(e.msg.clone()); });
                }
            }
        }
        if let Some((t, b)) = notify {
            self.notify("error", &t, &b, Some(id.to_string()));
        }
        // schedule a wake-up for the retry
        if let Some(w) = self.link(id).and_then(|l| l.wait_until) {
            let c = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis((w - now_ms()).max(100) as u64 + 50)).await;
                c.kick();
            });
        }
    }

    /// JD2 "mirror detection": same filename (+size) in the same package from another host -> skip.
    fn skip_mirrors(&self, done_id: &str) {
        let Some(done) = self.link(done_id) else { return };
        let ids: Vec<String> = self.state.lock().links.values()
            .filter(|o| o.id != done.id && o.package_id == done.package_id && o.name == done.name && o.status != Status::Finished && (o.size <= 0 || done.size <= 0 || o.size == done.size))
            .map(|o| o.id.clone()).collect();
        for id in ids {
            if let Some(r) = self.running.lock().get(&id) {
                *r.reason.lock() = Some(Reason::Remove);
                r.cancel.cancel();
            }
            self.with_link(&id, |o| {
                if let Some(p) = o.prop_str("partPath") {
                    let _ = std::fs::remove_file(p);
                }
                o.status = Status::Skipped;
                o.stage = Stage::Done;
                o.message = Some(format!("Mirror: already downloaded from {}", done.host));
            });
        }
    }

    fn refresh_pkg_stage(&self, pid: &str) {
        let mut st = self.state.lock();
        let links: Vec<Stage> = st.links.values().filter(|l| l.package_id == pid).map(|l| l.stage).collect();
        if links.is_empty() {
            return;
        }
        let stage = if links.iter().all(|s| *s == Stage::Done) { Stage::Done } else if links.iter().any(|s| *s != Stage::Inbox) { Stage::Queue } else { Stage::Inbox };
        if let Some(p) = st.packages.get_mut(pid) {
            if p.stage != stage {
                p.stage = stage;
                drop(st);
                self.mark_pkg(pid);
            }
        }
    }

    fn on_package_maybe_done(self: &Arc<Self>, pid: &str) {
        let (all_done, n, pkg) = {
            let st = self.state.lock();
            let links: Vec<&Link> = st.links.values().filter(|l| l.package_id == pid && l.enabled).collect();
            (!links.is_empty() && links.iter().all(|l| matches!(l.status, Status::Finished | Status::Skipped | Status::Offline)), links.iter().filter(|l| l.status == Status::Finished).count(), st.packages.get(pid).cloned())
        };
        let Some(p) = pkg else { return };
        if !all_done {
            return;
        }
        self.notify("success", &format!("Package finished: {}", p.name), &format!("{n} file(s) in {}", p.dir), None);
        if p.auto_extract && self.settings.bool("archives.autoExtract") {
            let c = self.clone();
            let pid = pid.to_string();
            tokio::spawn(async move { c.extract_package(&pid).await });
        }
    }

    pub async fn extract_package(self: &Arc<Self>, pid: &str) {
        if !self.extracting.lock().insert(pid.to_string()) {
            return;
        }
        let (pkg, links) = {
            let st = self.state.lock();
            (st.packages.get(pid).cloned(), st.links.values().filter(|l| l.package_id == pid && l.status == Status::Finished && l.prop_str("finalPath").is_some()).cloned().collect::<Vec<_>>())
        };
        if let Some(p) = pkg {
            let files: Vec<PathBuf> = links.iter().filter_map(|l| l.prop_str("finalPath").map(PathBuf::from)).collect();
            for set in crate::postprocess::find_archives(&files) {
                let owners: Vec<String> = links.iter().filter(|l| l.prop_str("finalPath").is_some_and(|f| set.parts.iter().any(|x| x == Path::new(f)))).map(|l| l.id.clone()).collect();
                let say = |m: String| for id in &owners { self.with_link(id, |l| l.message = Some(m.clone())); };
                say("Extracting…".into());
                let mut pw = p.passwords.clone();
                pw.extend(links.iter().flat_map(|l| l.passwords.clone()));
                pw.extend(self.settings.list("archives.passwords"));
                let c = self.clone();
                let ow = owners.clone();
                let dest = set.first.parent().map(Path::to_path_buf).unwrap_or_default();
                let r = crate::postprocess::extract(&self.settings.str("archives.sevenZip"), &set.first, &dest, &pw, self.settings.bool("archives.lowPriority"), &move |pct| {
                    for id in &ow {
                        c.with_link(id, |l| l.message = Some(format!("Extracting {pct}%")));
                    }
                }).await;
                if r.ok {
                    say(format!("Extracted ✔{}", r.password.as_ref().map(|p| format!(" (password: {p})")).unwrap_or_default()));
                    let after = self.settings.str("archives.deleteAfter");
                    for f in &set.parts {
                        match after.as_str() {
                            "delete" => { let _ = std::fs::remove_file(f); }
                            "recycle" => {
                                let trash = f.parent().unwrap_or(Path::new(".")).join(".fetchwing-trash");
                                let _ = std::fs::create_dir_all(&trash);
                                let _ = std::fs::rename(f, trash.join(f.file_name().unwrap_or_default()));
                            }
                            _ => {}
                        }
                    }
                    self.notify("success", &format!("Extracted {}", set.name), &r.message, None);
                } else {
                    say(format!("Extraction failed: {}", r.message));
                    let hint = if r.message.to_lowercase().contains("password") { "Add the archive password to the package (or the global list) and use Extract." } else { "Is 7-Zip installed? See Settings → Archives." };
                    for id in &owners {
                        self.with_link(id, |l| l.error = Some(LinkError { code: "EXTRACT_FAILED".into(), message: r.message.clone(), hint: Some(hint.into()), retryable: false }));
                    }
                    self.notify("error", &format!("Could not extract {}", set.name), &r.message, None);
                }
            }
        }
        self.extracting.lock().remove(pid);
    }

    // =====================================================================================
    // contexts
    // =====================================================================================

    pub fn net_opts(&self) -> NetOpts {
        NetOpts { user_agent: self.settings.str("connection.userAgent"), timeout_s: self.settings.u64("connection.timeout").max(5), proxy: self.settings.str("connection.proxy") }
    }
    pub fn browser_for(&self, host: &str) -> Browser {
        let jar = self.jars.lock().entry(host.to_string()).or_insert_with(|| Arc::new(Jar::default())).clone();
        Browser::new(jar, host, self.net_opts())
    }

    pub fn make_ctx(self: &Arc<Self>, url: &str, link_id: Option<String>, props: Arc<Mutex<Map<String, Value>>>, passwords: Vec<String>, account: Option<PluginAccount>, cancel: CancellationToken) -> Ctx {
        let host = host_of(url);
        Ctx {
            url: url.to_string(),
            host: host.clone(),
            browser: self.browser_for(&host),
            props,
            passwords,
            account,
            name: String::new(),
            size: -1,
            cancel,
            hooks: Arc::new(LinkHooks { core: self.clone(), link_id }),
        }
    }

    // =====================================================================================
    // accounts
    // =====================================================================================

    pub fn accounts(&self) -> Vec<Account> {
        self.state.lock().accounts.iter().map(|a| Account { secret: String::new(), ..a.clone() }).collect()
    }
    pub fn add_account(self: &Arc<Self>, host: &str, user: &str, password: &str) -> String {
        let host = host.trim().to_lowercase().trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.").split('/').next().unwrap_or("").to_string();
        let a = Account { id: new_id(), host, user: user.into(), secret: self.vault.encrypt(password), enabled: true, status: AccountStatus::Unchecked, premium: None, traffic_left: None, valid_until: None, message: None, last_check: None, supported_hosts: vec![] };
        let id = a.id.clone();
        self.state.lock().accounts.push(a);
        self.dirty.store(true, Ordering::Relaxed);
        let c = self.clone();
        let i = id.clone();
        tokio::spawn(async move { c.check_account(&i).await });
        let _ = self.events.send(Event::Accounts);
        self.bump();
        id
    }
    pub fn remove_account(&self, id: &str) {
        self.state.lock().accounts.retain(|a| a.id != id);
        self.dirty.store(true, Ordering::Relaxed);
        let _ = self.events.send(Event::Accounts);
        self.bump();
    }
    pub fn update_account(&self, id: &str, f: impl FnOnce(&mut Account)) {
        if let Some(a) = self.state.lock().accounts.iter_mut().find(|a| a.id == id) {
            f(a);
        }
        self.dirty.store(true, Ordering::Relaxed);
        let _ = self.events.send(Event::Accounts);
        self.bump();
        self.kick();
    }
    pub async fn check_account(self: &Arc<Self>, id: &str) {
        let Some(a) = self.state.lock().accounts.iter().find(|a| a.id == id).cloned() else { return };
        let reg = self.registry.read().clone();
        let plugin = reg.all().iter().find(|p| p.has_login() && p.meta().domains.contains(&a.host)).cloned();
        let Some(plugin) = plugin else {
            self.update_account(id, |a| { a.status = AccountStatus::Unchecked; a.message = Some("No login check for this host (the account will still be used)".into()); a.last_check = Some(now_ms()); });
            return;
        };
        let cancel = CancellationToken::new();
        let mut ctx = self.make_ctx(&format!("https://{}/", a.host), None, Arc::new(Mutex::new(Map::new())), vec![], None, cancel.clone());
        let acc = PluginAccount { id: a.id.clone(), user: a.user.clone(), password: self.vault.decrypt(&a.secret), premium: false };
        let r = tokio::time::timeout(Duration::from_secs(60), plugin.login(&mut ctx, &acc)).await;
        self.update_account(id, |x| {
            x.last_check = Some(now_ms());
            match r {
                Ok(Ok(Some(info))) => {
                    x.status = if !info.ok { AccountStatus::Invalid } else if info.valid_until.is_some_and(|v| v < now_ms()) { AccountStatus::Expired } else { AccountStatus::Ok };
                    x.premium = Some(info.premium);
                    x.valid_until = info.valid_until;
                    x.traffic_left = info.traffic_left;
                    x.message = info.message;
                    if !info.supported_hosts.is_empty() {
                        x.supported_hosts = info.supported_hosts;
                    }
                }
                Ok(Ok(None)) => x.status = AccountStatus::Unchecked,
                Ok(Err(e)) => { x.status = AccountStatus::Error; x.message = Some(e.msg); }
                Err(_) => { x.status = AccountStatus::Error; x.message = Some("Login check timed out".into()); }
            }
        });
    }

    // =====================================================================================
    // events, stats, limits, schedule
    // =====================================================================================

    fn flush_events(&self) {
        let pids: Vec<String> = self.changed_pkgs.lock().drain().collect();
        let lids: Vec<String> = self.changed_links.lock().drain().collect();
        if pids.is_empty() && lids.is_empty() || self.events.receiver_count() == 0 {
            return;
        }
        let (ps, ls) = {
            let st = self.state.lock();
            (pids.iter().filter_map(|id| st.packages.get(id).cloned()).collect::<Vec<_>>(), lids.iter().filter_map(|id| st.links.get(id).cloned()).collect::<Vec<_>>())
        };
        if !ps.is_empty() {
            let _ = self.events.send(Event::Packages(ps));
        }
        if !ls.is_empty() {
            let ls = ls.into_iter().map(|l| self.with_speed(l)).collect();
            let _ = self.events.send(Event::Links(ls));
        }
    }

    pub fn with_speed(&self, mut l: Link) -> Link {
        if let Some(r) = self.running.lock().get(&l.id) {
            let ext = r.ext_speed.load(Ordering::Relaxed);
            l.speed = if ext > 0 { ext } else { r.speed.get() };
        }
        l
    }

    pub fn snapshot(&self) -> (Vec<Package>, Vec<Link>) {
        let (mut ps, ls) = {
            let st = self.state.lock();
            (st.packages.values().cloned().collect::<Vec<_>>(), st.links.values().cloned().collect::<Vec<_>>())
        };
        ps.sort_by_key(|p| p.order);
        let mut ls: Vec<Link> = ls.into_iter().map(|l| self.with_speed(l)).collect();
        ls.sort_by(|a, b| a.added_at.cmp(&b.added_at).then(a.name.cmp(&b.name)));
        (ps, ls)
    }

    pub fn stats(&self) -> Stats {
        let mut s = Stats::default();
        {
            let st = self.state.lock();
            let mut remaining: u64 = 0;
            for l in st.links.values() {
                match (l.stage, l.status) {
                    (Stage::Inbox, _) => s.inbox += 1,
                    (_, Status::Queued) => s.queued += 1,
                    (_, Status::Waiting) | (_, Status::Captcha) => s.waiting += 1,
                    (_, Status::Finished) => s.finished += 1,
                    (_, Status::Failed) => s.failed += 1,
                    _ => {}
                }
                if l.stage == Stage::Queue && l.enabled && l.size > 0 {
                    remaining += (l.size as u64).saturating_sub(l.bytes_done);
                }
            }
            s.speed = self.global_speed.get();
            s.eta = if s.speed > 0 { (remaining / s.speed) as i64 } else { -1 };
        }
        s.active = self.running.lock().len();
        s.bytes_session = self.session_bytes.load(Ordering::Relaxed);
        s.limit = self.global_limiter.rate();
        let (p, sp, dl) = (self.paused.load(Ordering::Relaxed), self.schedule_paused.load(Ordering::Relaxed), self.disk_low.load(Ordering::Relaxed));
        s.paused = p || sp || dl;
        s.reason = if dl { Some("Low disk space".into()) } else if sp { Some("Paused by schedule".into()) } else if p { Some("Paused".into()) } else { None };
        s
    }

    fn apply_limits(&self) {
        let user = self.settings.u64("downloads.speedLimit");
        let sched = *self.schedule_limit.lock();
        self.global_limiter.set_rate(sched.unwrap_or(user));
    }

    fn apply_schedule(&self) {
        let rules: Vec<_> = self.settings.list("schedule.rules").iter().filter_map(|r| util::parse_rule(r).map_err(|e| crate::log::warn("schedule", &e)).ok()).collect();
        let r = util::active_rule(&rules);
        let was = self.schedule_paused.load(Ordering::Relaxed);
        let pause = r.is_some_and(|r| r.limit.is_none());
        self.schedule_paused.store(pause, Ordering::Relaxed);
        *self.schedule_limit.lock() = r.and_then(|r| r.limit).filter(|l| *l > 0).or(if r.is_some_and(|r| r.limit == Some(0)) { Some(0) } else { None });
        if pause && !was {
            for run in self.running.lock().values() {
                *run.reason.lock() = Some(Reason::Stop);
                run.cancel.cancel();
            }
            crate::log::info("schedule", &format!("paused by rule \"{}\"", r.map(|r| r.raw.as_str()).unwrap_or("")));
        }
        if !pause && was {
            self.kick();
        }
        self.apply_limits();
    }

    fn check_disk(&self) {
        let free = util::free_space(Path::new(&self.settings.str("general.downloadDir")));
        let low = free < self.settings.u64("downloads.diskMinFree");
        let was = self.disk_low.swap(low, Ordering::Relaxed);
        if low && !was {
            for r in self.running.lock().values() {
                *r.reason.lock() = Some(Reason::Stop);
                r.cancel.cancel();
            }
            self.notify("error", "Low disk space - downloads paused", &format!("{} free", fmt_bytes(free as i64)), None);
        }
        if !low && was {
            self.kick();
        }
    }
}

fn has_account(accounts: &[Account], reg: &Registry, l: &Link, plugin: Option<&P>) -> bool {
    let mut domains = vec![l.host.clone()];
    if let Some(p) = plugin {
        domains.extend(p.meta().domains.iter().cloned());
    }
    let ok = |a: &&Account| a.enabled && !matches!(a.status, AccountStatus::Invalid | AccountStatus::Expired);
    accounts.iter().filter(ok).any(|a| domains.contains(&a.host))
        || accounts.iter().filter(ok).any(|a| reg.multihosts().iter().any(|m| m.meta().domains.contains(&a.host)) && a.supported_hosts.iter().any(|h| domains.iter().any(|d| d.eq_ignore_ascii_case(h))))
}

fn dl_to_plugin(e: DlError) -> PluginError {
    match e.kind {
        DlKind::Cancelled => PluginError::new(ErrCode::Cancelled, "cancelled"),
        DlKind::Disk => PluginError::new(ErrCode::Defect, e.msg).hint("Check free space and folder permissions."),
        DlKind::Expired | DlKind::Html => PluginError::new(ErrCode::Retry, e.msg).hint("The download link expired; a fresh one will be requested."),
        _ => PluginError::new(ErrCode::Retry, e.msg),
    }
}

/// Plugin -> core callbacks for one link.
struct LinkHooks {
    core: Arc<Core>,
    link_id: Option<String>,
}

#[async_trait]
impl Hooks for LinkHooks {
    fn status(&self, msg: &str) {
        if let Some(id) = &self.link_id {
            self.core.with_link(id, |l| l.message = Some(msg.into()));
        }
    }
    async fn wait(&self, ms: u64, reason: &str, cancel: &CancellationToken) -> PResult<()> {
        let prev = self.link_id.as_ref().and_then(|id| self.core.with_link(id, |l| {
            let p = l.status;
            if l.stage == Stage::Queue {
                l.status = Status::Waiting;
                l.wait_until = Some(now_ms() + ms as i64);
            }
            l.message = Some(reason.into());
            p
        }));
        let r = tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
            _ = cancel.cancelled() => Err(PluginError::new(ErrCode::Cancelled, "cancelled")),
        };
        if let (Some(id), Some(p)) = (&self.link_id, prev) {
            self.core.with_link(id, |l| { l.status = p; l.wait_until = None; l.message = None; });
        }
        r
    }
    async fn captcha(&self, req: CaptchaRequest, host: &str, cancel: &CancellationToken) -> PResult<String> {
        let prev = self.link_id.as_ref().and_then(|id| self.core.with_link(id, |l| { let p = l.status; l.status = Status::Captcha; l.message = Some("Waiting for captcha".into()); p }));
        let r = self.core.captcha.solve(req, self.link_id.clone(), host, cancel).await;
        if let (Some(id), Some(p)) = (&self.link_id, prev) {
            self.core.with_link(id, |l| { l.status = p; l.message = None; });
        }
        r
    }
    fn setting(&self, key: &str) -> Value {
        if key == "links.ytdlpPath" {
            return json!(self.core.ytdlp_path());
        }
        self.core.settings.get(key)
    }
}

/// Collects crawl results into packages and links.
struct AddSink {
    core: Arc<Core>,
    job: String,
    req_pkg: Option<String>,
    req_dir: Option<String>,
    source: String,
    origin: Option<String>,
    link_pw: Option<String>,
    passwords: Vec<String>,
    auto: bool,
    pkg_by_name: Mutex<HashMap<String, String>>,
}

impl CrawlSink for AddSink {
    fn problem(&self, url: &str, msg: &str) {
        if let Some(j) = self.core.jobs.lock().get_mut(&self.job) {
            j.problems.push((url.into(), msg.into()));
        }
    }
    fn links(&self, found: Vec<Found>) {
        let c = &self.core;
        let ignore: Vec<String> = c.settings.list("links.ignoreExtensions").iter().map(|e| e.trim_start_matches('.').to_lowercase()).collect();
        let fresh: Vec<Found> = {
            let st = c.state.lock();
            found.into_iter().filter(|f| {
                if st.links.values().any(|l| l.url == f.url || l.props.get("canonical").and_then(|v| v.as_str()) == Some(&f.url)) {
                    self.problem(&f.url, "Already in your list");
                    return false;
                }
                let ext = f.name.as_deref().and_then(|n| n.rsplit_once('.')).map(|x| x.1.to_lowercase()).unwrap_or_default();
                !(!ext.is_empty() && ignore.contains(&ext))
            }).collect()
        };
        if fresh.is_empty() {
            return;
        }
        if let Some(j) = c.jobs.lock().get_mut(&self.job) {
            j.found += fresh.len();
        }
        let groups: Vec<Groupable> = fresh.iter().map(|f| Groupable { name: f.name.clone().unwrap_or_else(|| guess_name(&f.url)), host: host_for(f), hint: f.hint.clone() }).collect();
        let names = group_links(&groups, self.req_pkg.as_deref());
        let mut created = vec![];
        let mut to_check = vec![];
        for (f, pname) in fresh.into_iter().zip(names) {
            let pid = {
                let existing = self.pkg_by_name.lock().get(&pname).cloned();
                match existing {
                    Some(p) => p,
                    None => {
                        let mut pw = self.passwords.clone();
                        pw.extend(f.passwords.clone());
                        let p = c.create_package(&pname, self.req_dir.as_deref(), &self.source, pw);
                        if f.name.is_none() && f.hint.is_none() && self.req_pkg.is_none() {
                            if let Some(pp) = c.state.lock().packages.get_mut(&p.id) {
                                pp.auto_name = true;
                            }
                        }
                        self.pkg_by_name.lock().insert(pname.clone(), p.id.clone());
                        p.id
                    }
                }
            };
            let mut props = f.props.clone();
            if let Some(s) = &f.subdir {
                props.insert("subdir".into(), json!(s));
            }
            if let Some(pw) = &self.link_pw {
                props.insert("linkPassword".into(), json!(pw));
            }
            if f.name.is_none() {
                props.insert("nameFromUrl".into(), json!(true));
            }
            props.insert("autoStart".into(), json!(self.auto));
            let hash = props.get("md5").and_then(|v| v.as_str()).map(crate::plugins::cloud::md5_hash)
                .or_else(|| props.get("sha256").and_then(|v| v.as_str()).map(|v| HashInfo { algo: HashAlgo::Sha256, value: v.into(), verified: None }));
            let status = match f.status {
                Some(Avail::Online) => Status::Online,
                Some(Avail::Offline) => Status::Offline,
                Some(Avail::Unknown) | None => Status::New,
            };
            let l = Link {
                id: new_id(), package_id: pid, url: f.url.clone(), origin: f.origin.clone().or(self.origin.clone()), host: host_for(&f), plugin_id: f.plugin.meta().id.clone(),
                name: sanitize_filename(&f.name.clone().unwrap_or_else(|| guess_name(&f.url))), size: f.size.unwrap_or(-1), bytes_done: 0, stage: Stage::Inbox, status,
                message: None, error: None, wait_until: None, enabled: true, priority: 0, segments: vec![], resumable: true, etag: None, last_modified: None, hash,
                passwords: f.passwords.clone(), attempts: 0, added_at: now_ms(), finished_at: None, props, speed: 0,
            };
            if status == Status::New {
                to_check.push(l.id.clone());
            } else {
                created.push(l.id.clone());
            }
            let id = l.id.clone();
            c.state.lock().links.insert(id.clone(), l);
            c.mark_link(&id);
        }
        c.queue_checks(to_check);
        c.schedule_auto_confirm(created);
    }
}

fn host_for(f: &Found) -> String {
    let h = host_of(&f.url);
    f.plugin.meta().domains.iter().find(|d| h == **d || h.ends_with(&format!(".{d}"))).cloned().unwrap_or(h)
}

fn guess_name(url: &str) -> String {
    let n = util::http::filename_from_url(url);
    if n == "download" {
        host_of(url)
    } else {
        n
    }
}
