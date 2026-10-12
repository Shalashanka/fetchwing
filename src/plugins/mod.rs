//! Plugin API.
//!
//! JD2 plugins extend 4,000-line base classes (PluginForHost / PluginForDecrypt), compile to Java 6
//! bytecode and are coupled to the whole app. ~45% of JD2 hoster plugins are thin subclasses of
//! templates (XFileSharingProBasic 214, KernelVideoSharingComV2 86, YetiShareCore 52).
//!
//! Fetchwing: a plugin implements one small async trait and touches the world only through `Ctx`.
//! Template families are data (assets/template-sites.json, generated from the decompiled JD2
//! plugins), and users can add sites with JSON files in <data>/plugins/ without recompiling.

pub mod cloud;
pub mod debrid;
pub mod generic;
pub mod gofile;
pub mod mediafire;
pub mod mega;
pub mod onefichier;
pub mod pixeldrain;
pub mod templates;
pub mod ytdlp;

use crate::captcha::CaptchaRequest;
use crate::engine::DirectRequest;
use crate::model::HashInfo;
use crate::util::http::{Browser, HttpErr, Page, Req};
use async_trait::async_trait;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Host,
    Crawler,
}

#[derive(Clone, Debug, Default)]
pub struct Limits {
    pub max_downloads: Option<usize>,
    pub max_conns: Option<usize>,
    pub max_conns_premium: Option<usize>,
}

pub struct Meta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: Kind,
    pub domains: Vec<String>,
    pub patterns: Vec<Regex>,
    pub priority: i32,
    pub description: String,
    pub limits: Limits,
    /// needs an account (debrid services)
    pub account_only: bool,
    pub source: &'static str,
}

impl Meta {
    pub fn host(id: &str, name: &str, domains: &[&str], patterns: &[&str]) -> Self {
        Meta {
            id: id.into(),
            name: name.into(),
            version: "1.0.0".into(),
            kind: Kind::Host,
            domains: domains.iter().map(|s| s.to_string()).collect(),
            patterns: patterns.iter().map(|p| Regex::new(p).expect("plugin pattern")).collect(),
            priority: 0,
            description: String::new(),
            limits: Limits::default(),
            account_only: false,
            source: "builtin",
        }
    }
    pub fn crawler(id: &str, name: &str, domains: &[&str], patterns: &[&str]) -> Self {
        Meta { kind: Kind::Crawler, ..Self::host(id, name, domains, patterns) }
    }
    pub fn prio(mut self, p: i32) -> Self {
        self.priority = p;
        self
    }
    pub fn limits(mut self, l: Limits) -> Self {
        self.limits = l;
        self
    }
    pub fn desc(mut self, d: &str) -> Self {
        self.description = d.into();
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Avail {
    Online,
    Offline,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct LinkInfo {
    pub status: Avail,
    pub name: Option<String>,
    pub size: Option<i64>,
    pub hash: Option<HashInfo>,
    pub canonical: Option<String>,
}
impl LinkInfo {
    pub fn online(name: Option<String>, size: Option<i64>) -> Self {
        LinkInfo { status: Avail::Online, name, size, hash: None, canonical: None }
    }
    pub fn offline() -> Self {
        LinkInfo { status: Avail::Offline, name: None, size: None, hash: None, canonical: None }
    }
    pub fn unknown() -> Self {
        LinkInfo { status: Avail::Unknown, name: None, size: None, hash: None, canonical: None }
    }
}

#[derive(Clone, Debug)]
pub enum PostStep {
    MegaDecrypt { key: [u32; 4], iv: [u32; 4] },
}

pub enum Resolution {
    Http { req: DirectRequest, name: Option<String>, size: Option<i64>, hash: Option<HashInfo>, post: Vec<PostStep> },
    /// download through yt-dlp (media sites)
    Ytdlp { url: String, args: Vec<String> },
}
impl Resolution {
    pub fn http(req: DirectRequest) -> Self {
        Resolution::Http { req, name: None, size: None, hash: None, post: vec![] }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CrawledItem {
    pub url: String,
    pub name: Option<String>,
    pub size: Option<i64>,
    pub subdir: Option<String>,
    pub online: bool,
    pub props: Map<String, Value>,
}
impl CrawledItem {
    pub fn url(u: impl Into<String>) -> Self {
        CrawledItem { url: u.into(), ..Default::default() }
    }
    pub fn file(u: impl Into<String>, name: Option<String>, size: Option<i64>) -> Self {
        CrawledItem { url: u.into(), name, size, online: true, ..Default::default() }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CrawlResult {
    pub items: Vec<CrawledItem>,
    pub package_name: Option<String>,
    pub passwords: Vec<String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct AccountInfo {
    pub ok: bool,
    pub premium: bool,
    pub valid_until: Option<i64>,
    pub traffic_left: Option<i64>,
    pub message: Option<String>,
    pub supported_hosts: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PluginAccount {
    pub id: String,
    pub user: String,
    pub password: String,
    pub premium: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrCode {
    Offline,
    PremiumOnly,
    HosterWait,
    Quota,
    CaptchaFailed,
    CaptchaSkipped,
    AccountInvalid,
    Defect,
    Retry,
    Password,
    Unsupported,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct PluginError {
    pub code: ErrCode,
    pub msg: String,
    pub wait_ms: Option<u64>,
    pub hint: Option<String>,
}
impl PluginError {
    pub fn new(code: ErrCode, msg: impl Into<String>) -> Self {
        PluginError { code, msg: msg.into(), wait_ms: None, hint: None }
    }
    pub fn hint(mut self, h: &str) -> Self {
        self.hint = Some(h.into());
        self
    }
}
impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.msg)
    }
}
impl From<HttpErr> for PluginError {
    fn from(e: HttpErr) -> Self {
        if e.status == 404 || e.status == 410 {
            offline("File not found (HTTP 404)")
        } else {
            PluginError::new(ErrCode::Retry, e.to_string())
        }
    }
}
impl From<String> for PluginError {
    fn from(e: String) -> Self {
        PluginError::new(ErrCode::Defect, e)
    }
}
pub type PResult<T> = Result<T, PluginError>;

pub fn offline(why: &str) -> PluginError {
    PluginError::new(ErrCode::Offline, why)
}
pub fn defect(why: &str) -> PluginError {
    PluginError::new(ErrCode::Defect, why).hint("The site probably changed; the plugin needs an update.")
}
pub fn hoster_wait(ms: u64, why: &str) -> PluginError {
    PluginError { wait_ms: Some(ms), ..PluginError::new(ErrCode::HosterWait, why) }
}
pub fn premium_only(why: &str) -> PluginError {
    PluginError::new(ErrCode::PremiumOnly, why).hint("Add an account for this host, or a debrid service account that supports it.")
}
pub fn unsupported(why: &str) -> PluginError {
    PluginError::new(ErrCode::Unsupported, why)
}

/// What a plugin may ask of the core.
#[async_trait]
pub trait Hooks: Send + Sync {
    fn status(&self, msg: &str);
    async fn wait(&self, ms: u64, reason: &str, cancel: &CancellationToken) -> PResult<()>;
    async fn captcha(&self, req: CaptchaRequest, host: &str, cancel: &CancellationToken) -> PResult<String>;
    fn setting(&self, key: &str) -> Value;
}

pub struct Ctx {
    pub url: String,
    pub host: String,
    pub browser: Browser,
    pub props: Arc<Mutex<Map<String, Value>>>,
    pub passwords: Vec<String>,
    pub account: Option<PluginAccount>,
    pub name: String,
    pub size: i64,
    pub cancel: CancellationToken,
    pub hooks: Arc<dyn Hooks>,
}

impl Ctx {
    pub async fn get(&mut self, url: &str) -> PResult<Page> {
        self.cancelled()?;
        Ok(self.browser.get(url).await?)
    }
    /// GET that returns error pages instead of failing.
    pub async fn page(&mut self, url: &str) -> PResult<Page> {
        self.cancelled()?;
        Ok(self.browser.get_with(url, &Req { allow_errors: true, ..Default::default() }).await?)
    }
    pub async fn get_h(&mut self, url: &str, headers: Vec<(&str, String)>) -> PResult<Page> {
        self.cancelled()?;
        Ok(self.browser.get_with(url, &Req { headers, allow_errors: true, ..Default::default() }).await?)
    }
    pub async fn post(&mut self, url: &str, form: &[(String, String)], no_follow: bool) -> PResult<Page> {
        self.cancelled()?;
        Ok(self.browser.post_form(url, form, &Req { allow_errors: true, no_follow, ..Default::default() }).await?)
    }
    pub async fn post_h(&mut self, url: &str, form: &[(String, String)], headers: Vec<(&str, String)>) -> PResult<Page> {
        self.cancelled()?;
        Ok(self.browser.post_form(url, form, &Req { allow_errors: true, headers, ..Default::default() }).await?)
    }
    pub async fn post_json(&mut self, url: &str, json: &Value, headers: Vec<(&str, String)>) -> PResult<Page> {
        self.cancelled()?;
        Ok(self.browser.post_json(url, json, &Req { allow_errors: true, headers, ..Default::default() }).await?)
    }
    pub fn status(&self, msg: &str) {
        self.hooks.status(msg);
    }
    pub async fn wait(&self, ms: u64, reason: &str) -> PResult<()> {
        self.hooks.wait(ms, reason, &self.cancel).await
    }
    pub async fn captcha(&self, req: CaptchaRequest) -> PResult<String> {
        self.hooks.captcha(req, &self.host, &self.cancel).await
    }
    pub fn setting(&self, k: &str) -> Value {
        self.hooks.setting(k)
    }
    pub fn prop(&self, k: &str) -> Option<Value> {
        self.props.lock().get(k).cloned()
    }
    pub fn set_prop(&self, k: &str, v: Value) {
        self.props.lock().insert(k.to_string(), v);
    }
    pub fn cancelled(&self) -> PResult<()> {
        if self.cancel.is_cancelled() {
            Err(PluginError::new(ErrCode::Cancelled, "cancelled"))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
pub trait Plugin: Send + Sync {
    fn meta(&self) -> &Meta;
    fn matches(&self, url: &str) -> bool {
        self.meta().patterns.iter().any(|r| r.is_match(url))
    }
    /// Cheap availability check (Inbox). None = this plugin can't check.
    async fn check(&self, _ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        Ok(None)
    }
    async fn resolve(&self, _ctx: &mut Ctx) -> PResult<Resolution> {
        Err(unsupported("not a hoster plugin"))
    }
    async fn crawl(&self, _ctx: &mut Ctx) -> PResult<CrawlResult> {
        Err(unsupported("not a crawler plugin"))
    }
    async fn login(&self, _ctx: &mut Ctx, _acc: &PluginAccount) -> PResult<Option<AccountInfo>> {
        Ok(None)
    }
    /// Multihost / debrid: turn any supported hoster URL into a direct link.
    async fn unrestrict(&self, _ctx: &mut Ctx, _acc: &PluginAccount) -> PResult<Option<Resolution>> {
        Ok(None)
    }
    fn is_multihost(&self) -> bool {
        false
    }
    fn has_login(&self) -> bool {
        false
    }
}

pub type P = Arc<dyn Plugin>;

pub struct Registry {
    plugins: Vec<P>,
    pub user_errors: Vec<String>,
}

impl Registry {
    pub fn new(user_dir: Option<&std::path::Path>) -> Self {
        let mut plugins: Vec<P> = vec![];
        plugins.extend(pixeldrain::all());
        plugins.extend(gofile::all());
        plugins.extend(mediafire::all());
        plugins.extend(cloud::all());
        plugins.extend(mega::all());
        plugins.extend(onefichier::all());
        plugins.extend(ytdlp::all());
        plugins.extend(debrid::all());
        plugins.extend(templates::builtin());
        plugins.push(Arc::new(generic::Generic::new()));
        let mut user_errors = vec![];
        if let Some(dir) = user_dir {
            let (user, errs) = templates::load_user_dir(dir);
            user_errors = errs;
            // user plugins override builtins with the same id
            let ids: std::collections::HashSet<String> = user.iter().map(|p| p.meta().id.clone()).collect();
            plugins.retain(|p| !ids.contains(&p.meta().id));
            plugins.extend(user);
        }
        plugins.sort_by_key(|p| -p.meta().priority);
        Registry { plugins, user_errors }
    }
    pub fn all(&self) -> &[P] {
        &self.plugins
    }
    pub fn find_crawler(&self, url: &str) -> Option<P> {
        self.plugins.iter().find(|p| p.meta().kind == Kind::Crawler && p.matches(url)).cloned()
    }
    pub fn find_host(&self, url: &str) -> Option<P> {
        self.plugins.iter().find(|p| p.meta().kind == Kind::Host && !p.meta().account_only && p.matches(url)).cloned()
    }
    pub fn get(&self, id: &str) -> Option<P> {
        self.plugins.iter().find(|p| p.meta().id == id).cloned()
    }
    pub fn multihosts(&self) -> Vec<P> {
        self.plugins.iter().filter(|p| p.is_multihost()).cloned().collect()
    }
    /// is there a non-generic plugin for this URL? (deep crawl decides what to keep)
    pub fn knows(&self, url: &str) -> bool {
        self.find_crawler(url).is_some_and(|c| c.meta().priority >= 0) || self.find_host(url).is_some_and(|h| h.meta().priority >= 0)
    }
}

/// regex helper for plugin code: first capture group of `pat` in `s`
pub fn cap(pat: &str, s: &str) -> Option<String> {
    let c = crate::util::http::re(pat).captures(s)?;
    Some(c.get(1).or(c.get(0))?.as_str().to_string())
}

pub fn j_str(v: &Value, path: &str) -> Option<String> {
    let mut cur = v;
    for p in path.split('/') {
        cur = match cur {
            Value::Object(m) => m.get(p)?,
            Value::Array(a) => a.get(p.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    match cur {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
pub fn j_i64(v: &Value, path: &str) -> Option<i64> {
    j_str(v, path).and_then(|s| s.parse::<f64>().ok()).map(|f| f as i64)
}
