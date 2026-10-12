//! Native desktop UI (egui). Talks to the in-process Core directly; the same Core also serves the
//! web UI / API, so desktop and headless share one engine.
//!
//! UX principles (docs/ux-revamp.md): one list with stages; paste anywhere; nothing modal steals
//! focus; every error has a hint; everything reachable from the keyboard (Ctrl+K).

mod downloads;
mod theme;
mod views;
mod widgets;

use crate::core::{AddRequest, Core};
use crate::events::Notice;
use crate::model::{Link, Package, Stage, Status};
use crate::util::{fmt_bytes, fmt_duration};
use eframe::egui::{self, Align2, Color32, CursorIcon, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use egui_phosphor::regular as I;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use theme::{alpha, f, fm, fs, mix, Palette};
use widgets::Kind;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Downloads,
    Captchas,
    Accounts,
    Plugins,
    Settings,
    Log,
    About,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum QFilter {
    All,
    Active,
    Waiting,
    Failed,
}

struct Toast {
    n: Notice,
    life: f32, // seconds left
    born: f64,
}

#[derive(Default)]
struct AddDialog {
    open: bool,
    text: String,
    pkg: String,
    pw: String,
    review: bool,
    focus: bool,
}

#[derive(Default)]
struct PaletteState {
    open: bool,
    query: String,
    idx: usize,
}

#[derive(Clone)]
enum Cmd {
    Add,
    Start,
    Pause,
    Go(View),
    Limit(u64),
    Theme,
    ClearFinished,
    RetryFailed,
    OpenDownloads,
    OpenData,
    WebUi,
    UpdateYtdlp,
    ReloadPlugins,
    Setting(String),
    Link(String),
}

pub struct App {
    core: Arc<Core>,
    rt: tokio::runtime::Handle,
    api_url: String,
    pal: Palette,
    dark: bool,
    view: View,
    tab: usize,
    qfilter: QFilter,
    filter: String,
    sel: HashSet<String>,
    anchor: Option<String>,
    cursor: Option<String>,
    version_seen: u64,
    packages: Vec<Package>,
    links: Vec<Link>,
    last_snapshot: Instant,
    add: AddDialog,
    palette: PaletteState,
    toasts: Vec<Toast>,
    notices_seen: usize,
    speed_hist: VecDeque<f32>,
    last_hist: Instant,
    settings_group: String,
    settings_filter: String,
    show_advanced: bool,
    edits: HashMap<String, String>,
    acc_host: String,
    acc_user: String,
    acc_pw: String,
    cap_inputs: HashMap<String, String>,
    plugin_filter: String,
    log_filter: String,
    log_level: usize,
    rename: Option<(String, String)>,
    pw_edit: Option<(String, String)>,
    onboarding: bool,
    ob_token_service: usize,
    ob_token: String,
    disk_free: (Instant, u64),
    frame_time: f64,
    shots: Option<ShotRunner>,
}

/// Test aid: FW_SHOTS=dir[:step,step,...] renders scripted UI states and saves PNGs from the
/// renderer itself (works with a locked/hidden desktop), then exits.
const SAMPLE_LINKS: &str = "https://mega.nz/file/abcdefgh#AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
https://www.youtube.com/watch?v=jNQXAC9IVRw
https://filekeeper.net/2h1nq2vz6sny/file.zip
https://github.com/yt-dlp/yt-dlp/releases/latest
https://example.com/files/setup.exe";

struct ShotRunner {
    dir: std::path::PathBuf,
    steps: Vec<String>,
    i: usize,
    since: f64,
    waiting: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, core: Arc<Core>, rt: tokio::runtime::Handle, api_url: String) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        theme::install_fonts(&cc.egui_ctx);
        let dark = core.settings.str("ui.theme") != "light";
        let pal = theme::apply(&cc.egui_ctx, dark);
        let notices_seen = core.notices.lock().len();
        let onboarding = !core.state.lock().kv.contains_key("ui.onboarded") && core.state.lock().links.is_empty();
        App {
            core, rt, api_url, pal, dark, view: View::Downloads, tab: 1, qfilter: QFilter::All, filter: String::new(),
            sel: HashSet::new(), anchor: None, cursor: None, version_seen: 0, packages: vec![], links: vec![],
            last_snapshot: Instant::now() - Duration::from_secs(5), add: AddDialog::default(), palette: PaletteState::default(),
            toasts: vec![], notices_seen, speed_hist: VecDeque::from(vec![0.0; 90]), last_hist: Instant::now(),
            settings_group: "General".into(), settings_filter: String::new(), show_advanced: false, edits: HashMap::new(),
            acc_host: String::new(), acc_user: String::new(), acc_pw: String::new(), cap_inputs: HashMap::new(),
            plugin_filter: String::new(), log_filter: String::new(), log_level: 0, rename: None, pw_edit: None,
            onboarding, ob_token_service: 0, ob_token: String::new(), disk_free: (Instant::now() - Duration::from_secs(60), 0), frame_time: 0.0,
            shots: std::env::var("FW_SHOTS").ok().map(|v| {
                let (dir, steps) = v.split_once('|').unwrap_or((v.as_str(), "onboarding,queue,detail,inbox,multi,add,palette,settings,accounts,plugins,captchas,log,about,light"));
                ShotRunner { dir: dir.into(), steps: steps.split(',').map(str::to_string).collect(), i: 0, since: 0.0, waiting: false }
            }),
        }
    }

    fn refresh(&mut self) {
        let v = self.core.version.load(Ordering::Relaxed);
        if v != self.version_seen && self.last_snapshot.elapsed() > Duration::from_millis(150) {
            let (p, l) = self.core.snapshot();
            self.packages = p;
            self.links = l;
            self.version_seen = v;
            self.last_snapshot = Instant::now();
            let ids: HashSet<&String> = self.links.iter().map(|l| &l.id).collect();
            self.sel.retain(|s| ids.contains(s));
            if self.cursor.as_ref().is_some_and(|c| !ids.contains(c)) {
                self.cursor = None;
            }
        }
        let notices = self.core.notices.lock().clone();
        if notices.len() < self.notices_seen {
            self.notices_seen = 0;
        }
        for n in notices.into_iter().skip(self.notices_seen) {
            self.notices_seen += 1;
            if self.toasts.iter().any(|t| t.n.title == n.title && t.life > 1.0) {
                continue;
            }
            self.toasts.push(Toast { n, life: 6.0, born: self.frame_time });
        }
        if self.last_hist.elapsed() >= Duration::from_millis(500) {
            self.last_hist = Instant::now();
            self.speed_hist.push_back(self.core.stats().speed as f32);
            if self.speed_hist.len() > 90 {
                self.speed_hist.pop_front();
            }
        }
        if self.disk_free.0.elapsed() > Duration::from_secs(10) {
            self.disk_free = (Instant::now(), crate::util::free_space(std::path::Path::new(&self.core.settings.str("general.downloadDir"))));
        }
        let want_dark = self.core.settings.str("ui.theme") != "light";
        if want_dark != self.dark {
            self.dark = want_dark;
        }
    }

    pub(crate) fn add_links_now(&mut self, text: &str, pkg: Option<String>, pw: Option<String>, review: bool) {
        let _g = self.rt.enter();
        let mut req = AddRequest::text(text, "gui");
        req.package_name = pkg.filter(|s| !s.trim().is_empty());
        if let Some(p) = pw.filter(|s| !s.trim().is_empty()) {
            req.passwords = vec![p.trim().to_string()];
        }
        if review {
            req.auto_start = Some(false);
        }
        self.core.add_links(req);
        self.view = View::Downloads;
        self.tab = if review { 0 } else { 1 };
    }

    fn sel_vec(&self) -> Vec<String> {
        self.sel.iter().cloned().collect()
    }

    fn toggle_theme(&mut self) {
        let next = if self.dark { "light" } else { "dark" };
        let _ = self.core.settings.set("ui.theme", serde_json::json!(next));
        self.core.settings_changed("ui.theme");
    }

    fn run_cmd(&mut self, c: Cmd) {
        let _g = self.rt.enter();
        match c {
            Cmd::Add => self.add.open = true,
            Cmd::Start => self.core.start_all(),
            Cmd::Pause => self.core.pause_all(),
            Cmd::Go(v) => self.view = v,
            Cmd::Limit(b) => self.core.set_speed_limit(b),
            Cmd::Theme => self.toggle_theme(),
            Cmd::ClearFinished => {
                self.core.cleanup("finished");
            }
            Cmd::RetryFailed => {
                let f: Vec<String> = self.links.iter().filter(|l| l.status == Status::Failed).map(|l| l.id.clone()).collect();
                self.core.resume_links(&f);
            }
            Cmd::OpenDownloads => {
                let d = self.core.settings.str("general.downloadDir");
                let _ = std::fs::create_dir_all(&d);
                let _ = open::that_detached(d);
            }
            Cmd::OpenData => {
                let _ = open::that_detached(&self.core.data_dir);
            }
            Cmd::WebUi => {
                let _ = open::that_detached(&self.api_url);
            }
            Cmd::UpdateYtdlp => {
                let c = self.core.clone();
                self.rt.spawn(async move { c.refresh_tools(true).await });
            }
            Cmd::ReloadPlugins => {
                self.core.reload_plugins();
            }
            Cmd::Setting(k) => {
                self.view = View::Settings;
                self.settings_filter = k;
            }
            Cmd::Link(id) => {
                if let Some(l) = self.links.iter().find(|l| l.id == id) {
                    self.tab = match l.stage { Stage::Inbox => 0, Stage::Queue => 1, Stage::Done => 2 };
                    self.view = View::Downloads;
                    self.filter.clear();
                    self.qfilter = QFilter::All;
                    self.sel = [id.clone()].into();
                    self.cursor = Some(id);
                }
            }
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from(self.pal.bg).to_array()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_time = ctx.input(|i| i.time);
        self.run_shots(ctx);
        if self.pal.dark != self.dark {
            self.pal = theme::apply(ctx, self.dark);
        }
        self.refresh();
        let _g = self.rt.enter();
        self.shortcuts(ctx);
        self.sidebar(ctx);
        self.status_bar(ctx);
        let detail = self.view == View::Downloads && self.sel.len() == 1;
        let open_t = ctx.animate_bool_with_time(egui::Id::new("detail-open"), detail, 0.22);
        if open_t > 0.01 {
            self.detail_panel(ctx, open_t);
        }
        let p = self.pal.clone();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 26, right: 26, top: 22, bottom: 10 })).show(ctx, |ui| {
            // gentle cross-fade when switching views
            let t = ctx.animate_value_with_time(egui::Id::new(("view", self.view as u8)), 1.0, 0.0);
            let _ = t;
            match self.view {
                View::Downloads => self.downloads(ui),
                View::Captchas => self.captchas(ui),
                View::Accounts => self.accounts(ui),
                View::Plugins => self.plugins(ui),
                View::Settings => self.settings_view(ui),
                View::Log => self.log_view(ui),
                View::About => self.about(ui),
            }
        });
        self.add_dialog(ctx);
        self.command_palette(ctx);
        self.small_dialogs(ctx);
        self.onboarding_dialog(ctx);
        self.drop_overlay(ctx);
        self.toasts_ui(ctx);
        let st = self.core.stats();
        ctx.request_repaint_after(Duration::from_millis(if st.active > 0 { 33 } else if !self.toasts.is_empty() { 50 } else { 400 }));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let core = self.core.clone();
        self.rt.block_on(async move { core.shutdown().await });
    }
}

// ------------------------------------------------------------------------------------------------
// scripted screenshots (FW_SHOTS)
// ------------------------------------------------------------------------------------------------

impl App {
    fn run_shots(&mut self, ctx: &egui::Context) {
        let Some(mut r) = self.shots.take() else { return };
        ctx.request_repaint();
        let now = self.frame_time;
        // a finished screenshot arrives as an event
        let img = ctx.input(|i| i.raw.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }));
        if let Some(img) = img {
            let name = format!("{:02}-{}.png", r.i, r.steps[r.i]);
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            let _ = std::fs::create_dir_all(&r.dir);
            let _ = image::save_buffer(r.dir.join(name), &bytes, img.width() as u32, img.height() as u32, image::ExtendedColorType::Rgba8);
            r.i += 1;
            r.waiting = false;
            r.since = now;
        }
        if r.i >= r.steps.len() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.shots = Some(r);
            return;
        }
        if r.since == 0.0 {
            r.since = now;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(1440.0, 880.0)));
        }
        let step = r.steps[r.i].clone();
        if !r.waiting && now - r.since < 0.05 {
            // set up the state once per step
            self.add.open = false;
            self.palette.open = false;
            self.onboarding = step == "onboarding";
            if self.dark != (step != "light") {
                self.toggle_theme();
            }
            let first = |st: Stage, s: Status| self.links.iter().find(|l| l.stage == st && (s == Status::New || l.status == s)).map(|l| l.id.clone());
            match step.as_str() {
                "queue" | "light" => { self.view = View::Downloads; self.tab = 1; self.sel.clear(); }
                "detail" => { self.view = View::Downloads; self.tab = 1; self.sel = first(Stage::Queue, Status::Downloading).or(first(Stage::Queue, Status::New)).into_iter().collect(); }
                "inbox" => { self.view = View::Downloads; self.tab = 0; self.sel.clear(); }
                "multi" => { self.view = View::Downloads; self.tab = 0; self.sel = self.links.iter().filter(|l| l.stage == Stage::Inbox).take(4).map(|l| l.id.clone()).collect(); }
                "done" => { self.view = View::Downloads; self.tab = 2; self.sel.clear(); }
                "add" => { self.view = View::Downloads; self.sel.clear(); self.add = AddDialog { open: true, text: SAMPLE_LINKS.to_string(), ..Default::default() }; }
                "palette" => { self.palette = PaletteState { open: true, query: "pau".into(), idx: 0 }; }
                "settings" => self.view = View::Settings,
                "accounts" => self.view = View::Accounts,
                "plugins" => self.view = View::Plugins,
                "captchas" => self.view = View::Captchas,
                "log" => self.view = View::Log,
                "about" => self.view = View::About,
                _ => {}
            }
        }
        if !r.waiting && now - r.since > 1.6 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            r.waiting = true;
        }
        self.shots = Some(r);
    }
}

// ------------------------------------------------------------------------------------------------
// keyboard
// ------------------------------------------------------------------------------------------------

impl App {
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        let modal = self.add.open || self.palette.open || self.onboarding || self.rename.is_some() || self.pw_edit.is_some();
        // paste anywhere -> add dialog
        let mut pasted = None;
        ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::Paste(t) = e {
                    if !typing && !modal && crate::util::http::re(r"(?i)https?://|www\.|^\s*curl\s").is_match(t) {
                        pasted = Some(t.clone());
                    }
                }
            }
        });
        if let Some(t) = pasted {
            self.add = AddDialog { open: true, text: t, focus: true, ..Default::default() };
        }
        let (cmd, k_k, k_n, k_f, k_esc) = ctx.input(|i| (i.modifiers.command, i.key_pressed(Key::K), i.key_pressed(Key::N), i.key_pressed(Key::F), i.key_pressed(Key::Escape)));
        if cmd && k_k {
            self.palette = PaletteState { open: !self.palette.open, ..Default::default() };
        }
        if cmd && k_n && !modal {
            self.add = AddDialog { open: true, focus: true, ..Default::default() };
        }
        if cmd && k_f && !modal {
            self.view = View::Downloads;
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("dl-search")));
        }
        if k_esc && !modal {
            if typing {
                ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("dl-search")));
            } else {
                self.sel.clear();
            }
        }
        if typing || modal || self.view != View::Downloads {
            return;
        }
        let order = self.visible_link_order();
        let (up, down, del, space, enter, a, d1, d2, d3) = ctx.input(|i| {
            (i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::K), i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::J), i.key_pressed(Key::Delete), i.key_pressed(Key::Space), i.key_pressed(Key::Enter), i.modifiers.command && i.key_pressed(Key::A), i.key_pressed(Key::Num1), i.key_pressed(Key::Num2), i.key_pressed(Key::Num3))
        });
        if d1 { self.tab = 0; }
        if d2 { self.tab = 1; }
        if d3 { self.tab = 2; }
        if (up || down) && !order.is_empty() {
            let cur = self.cursor.as_ref().and_then(|c| order.iter().position(|x| x == c));
            let next = match (cur, down) {
                (None, _) => 0,
                (Some(i), true) => (i + 1).min(order.len() - 1),
                (Some(i), false) => i.saturating_sub(1),
            };
            let id = order[next].clone();
            let shift = ctx.input(|i| i.modifiers.shift);
            if !shift {
                self.sel.clear();
            }
            self.sel.insert(id.clone());
            self.cursor = Some(id.clone());
            self.anchor = Some(id);
        }
        if a {
            self.sel = order.iter().cloned().collect();
        }
        if del && !self.sel.is_empty() {
            let ids = self.sel_vec();
            let n = ids.len();
            self.core.remove_links(&ids, false);
            self.core.notify("info", &format!("Removed {n} item{}", if n == 1 { "" } else { "s" }), "Files on disk were kept.", None);
            self.sel.clear();
        }
        if space && !self.sel.is_empty() {
            let ids = self.sel_vec();
            let any_running = self.links.iter().any(|l| ids.contains(&l.id) && matches!(l.status, Status::Downloading | Status::Waiting | Status::Queued | Status::Captcha));
            if any_running { self.core.pause_links(&ids) } else { self.core.confirm(&ids); self.core.resume_links(&ids) }
        }
        if enter && !self.sel.is_empty() {
            let ids = self.sel_vec();
            if self.tab == 0 { self.core.confirm(&ids); } else if let Some(p) = self.links.iter().find(|l| l.id == ids[0]).and_then(|l| l.prop_str("finalPath").map(String::from)) {
                crate::api::reveal(std::path::Path::new(&p));
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// chrome: sidebar, status bar, toasts
// ------------------------------------------------------------------------------------------------

impl App {
    fn sidebar(&mut self, ctx: &egui::Context) {
        let p = self.pal.clone();
        let st = self.core.stats();
        let caps = self.core.captcha.list().len();
        egui::SidePanel::left("nav").exact_width(236.0).resizable(false).show_separator_line(false)
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(egui::Margin { left: 14, right: 14, top: 16, bottom: 14 }).stroke(Stroke::new(1.0, p.line_soft)))
            .show(ctx, |ui| {
                // brand
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                    paint_logo(ui.painter(), r, &p);
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new("Fetchwing").font(fs(17.0)).color(p.text));
                });
                ui.add_space(16.0);
                // primary action
                let w = ui.available_width();
                let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 40.0), Sense::click());
                let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.15);
                if h > 0.0 {
                    ui.painter().rect_filled(r.expand(3.0 * h), 13.0, alpha(p.acc, 0.2 * h));
                }
                widgets::gradient_pill(ui.painter(), r.shrink2(Vec2::new(0.0, 0.0)), mix(p.acc, Color32::WHITE, 0.08 * h), mix(p.acc2, Color32::WHITE, 0.08 * h));
                ui.painter().text(Pos2::new(r.left() + 16.0, r.center().y), Align2::LEFT_CENTER, I::PLUS, fs(16.0), Color32::WHITE);
                ui.painter().text(Pos2::new(r.left() + 38.0, r.center().y), Align2::LEFT_CENTER, "Add links", fs(14.0), Color32::WHITE);
                ui.painter().text(Pos2::new(r.right() - 14.0, r.center().y), Align2::RIGHT_CENTER, "Ctrl V", fm(11.0), alpha(Color32::WHITE, 0.75));
                if resp.clicked() {
                    self.add = AddDialog { open: true, focus: true, ..Default::default() };
                }
                ui.add_space(18.0);
                ui.label(egui::RichText::new("LIBRARY").font(fm(10.5)).color(p.faint));
                ui.add_space(4.0);
                let items: [(View, &str, &str, usize, bool); 6] = [
                    (View::Downloads, I::DOWNLOAD_SIMPLE, "Downloads", st.active + st.queued + st.waiting + st.inbox, false),
                    (View::Captchas, I::SHIELD_CHECK, "Captchas", caps, true),
                    (View::Accounts, I::KEY, "Accounts", 0, false),
                    (View::Plugins, I::PUZZLE_PIECE, "Plugins", 0, false),
                    (View::Settings, I::GEAR_SIX, "Settings", 0, false),
                    (View::Log, I::LIST_BULLETS, "Activity log", 0, false),
                ];
                for (v, icon, label, n, alert) in items {
                    if nav_item(ui, icon, label, n, self.view == v, alert && n > 0, &p).clicked() {
                        self.view = v;
                    }
                }
                // speed card + footer pinned to the bottom (measured spacer, top-down layout)
                let card_h = 206.0;
                let footer_h = 34.0;
                ui.add_space((ui.available_height() - card_h - footer_h - 12.0).max(8.0));
                self.speed_card(ui, &st);
                ui.add_space(10.0);
                {
                    ui.horizontal(|ui| {
                        if widgets::icon_button(ui, if self.dark { I::SUN } else { I::MOON }, "Toggle light / dark", &p).clicked() {
                            self.toggle_theme();
                        }
                        if widgets::icon_button(ui, I::COMMAND, "Command palette (Ctrl+K)", &p).clicked() {
                            self.palette = PaletteState { open: true, ..Default::default() };
                        }
                        if widgets::icon_button(ui, I::GLOBE, &format!("Open web UI ({})", self.api_url), &p).clicked() {
                            let _ = open::that_detached(&self.api_url);
                        }
                        if widgets::icon_button(ui, I::INFO, "About Fetchwing", &p).clicked() {
                            self.view = View::About;
                        }
                    });
                }
            });
    }

    fn speed_card(&mut self, ui: &mut egui::Ui, st: &crate::events::Stats) {
        let p = self.pal.clone();
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.line_soft)).corner_radius(14).inner_margin(14).show(ui, |ui| ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            let shown = ui.ctx().animate_value_with_time(egui::Id::new("speed-num"), st.speed as f32, 0.6);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(I::ARROW_DOWN).font(fs(15.0)).color(p.acc));
                ui.label(egui::RichText::new(format!("{}/s", fmt_bytes(shown as i64))).font(fs(20.0)).color(p.text));
            });
            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::hover());
            let hist: Vec<f32> = self.speed_hist.iter().cloned().collect();
            widgets::sparkline(ui.painter(), r, &hist, &p);
            ui.add_space(4.0);
            let line = if st.paused {
                st.reason.clone().unwrap_or("Paused".into())
            } else if st.active > 0 {
                format!("{} active · {} queued · {}", st.active, st.queued, if st.eta > 0 { format!("{} left", fmt_duration(st.eta)) } else { "…".into() })
            } else if st.queued + st.waiting > 0 {
                format!("{} waiting to start", st.queued + st.waiting)
            } else {
                "Idle · ready for links".into()
            };
            ui.label(egui::RichText::new(line).font(f(12.0)).color(if st.paused { p.warn } else { p.dim }));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let tip = if st.paused { "Resume all downloads" } else { "Pause all downloads" };
                let icon = if st.paused { I::PLAY } else { I::PAUSE };
                let (r, resp) = ui.allocate_exact_size(Vec2::new(36.0, 32.0), Sense::click());
                let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip);
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                let pz = ui.ctx().animate_bool_with_time(resp.id.with("paused"), st.paused, 0.2);
                let fill = mix(mix(p.surface2, p.elevated, h), p.acc, pz);
                ui.painter().rect(r, 8.0, fill, Stroke::new(1.0, mix(mix(p.line, alpha(p.acc, 0.6), h), p.acc, pz)), StrokeKind::Inside);
                ui.painter().text(r.center(), Align2::CENTER_CENTER, icon, f(15.0), mix(p.text, Color32::WHITE, pz));
                if resp.clicked() {
                    if st.paused { self.core.start_all() } else { self.core.pause_all() }
                }
                let cur = st.limit;
                let label = if cur == 0 { "No limit".to_string() } else { format!("{}/s", fmt_bytes(cur as i64)) };
                let pid = egui::Id::new("limit-pop");
                let open = ui.memory(|m| m.is_popup_open(pid));
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
                let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Global speed limit");
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || open, 0.12);
                ui.painter().rect(r, 8.0, mix(p.surface2, p.elevated, h), Stroke::new(1.0, mix(p.line, alpha(p.acc, 0.6), h)), StrokeKind::Inside);
                ui.painter().text(Pos2::new(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, I::GAUGE, f(14.0), if cur > 0 { p.acc } else { p.dim });
                widgets::text_at(ui, ui.painter(), r.left() + 30.0, r.center().y, &label, fm(12.5), p.text, r.width() - 52.0);
                let rot = ui.ctx().animate_bool_with_time(pid, open, 0.15);
                let c = Pos2::new(r.right() - 14.0, r.center().y);
                let pts: Vec<Pos2> = [(-4.0, -2.0), (0.0, 2.0), (4.0, -2.0)].iter().map(|&(x, y): &(f32, f32)| {
                    let (s, co) = (rot * std::f32::consts::PI).sin_cos();
                    c + Vec2::new(x * co - y * s, x * s + y * co)
                }).collect();
                ui.painter().add(egui::Shape::line(pts, Stroke::new(1.5, p.dim)));
                if resp.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(pid));
                }
                egui::popup::popup_above_or_below_widget(ui, pid, &resp, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                    ui.set_min_width(150.0);
                    for (v, t) in [(0u64, "No limit"), (512 * 1024, "512 KB/s"), (1 << 20, "1 MB/s"), (2 << 20, "2 MB/s"), (5 << 20, "5 MB/s"), (10 << 20, "10 MB/s"), (25 << 20, "25 MB/s"), (50 << 20, "50 MB/s")] {
                        if ui.selectable_label(cur == v, t).clicked() {
                            self.core.set_speed_limit(v);
                        }
                    }
                });
            });
        }));
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let p = self.pal.clone();
        let sys = self.core.sys.lock().clone();
        let st = self.core.stats();
        egui::TopBottomPanel::bottom("status").exact_height(28.0).show_separator_line(false)
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(egui::Margin::symmetric(14, 0)).stroke(Stroke::new(1.0, p.line_soft)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let item = |ui: &mut egui::Ui, icon: &str, text: String, col: Color32, tip: &str| {
                        ui.label(egui::RichText::new(icon).font(f(13.0)).color(col)).on_hover_text(tip);
                        ui.label(egui::RichText::new(text).font(f(11.5)).color(p.dim)).on_hover_text(tip);
                        ui.add_space(10.0);
                    };
                    let online = !self.api_url.is_empty();
                    item(ui, I::CIRCLE, if online { "Web UI on".into() } else { "Web UI off".into() }, if online { p.ok } else { p.err }, &self.api_url);
                    item(ui, I::LINK, if sys.cnl { "Click'n'Load".into() } else { "Click'n'Load off".into() }, if sys.cnl { p.ok } else { p.faint }, "Port 9666 - link protectors can send links");
                    item(ui, I::FILM_STRIP, format!("yt-dlp {}", sys.ytdlp.clone().unwrap_or("–".into())), p.dim, "Managed yt-dlp version (auto-updated)");
                    item(ui, I::FILE_ZIP, if sys.seven_zip { "7-Zip".into() } else { "7-Zip missing".into() }, if sys.seven_zip { p.dim } else { p.warn }, "Used for automatic extraction");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!("{} free", fmt_bytes(self.disk_free.1 as i64))).font(f(11.5)).color(if self.disk_free.1 < 10 << 30 { p.warn } else { p.dim }));
                        ui.label(egui::RichText::new(I::HARD_DRIVES).font(f(13.0)).color(p.dim));
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new(format!("{} this session", fmt_bytes(st.bytes_session as i64))).font(f(11.5)).color(p.dim));
                        ui.label(egui::RichText::new(I::CHART_LINE_UP).font(f(13.0)).color(p.dim));
                    });
                });
            });
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        let p = self.pal.clone();
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        let now = self.frame_time;
        let mut remove = vec![];
        let mut go: Option<Cmd> = None;
        let n = self.toasts.len();
        let start = n.saturating_sub(4);
        egui::Area::new(egui::Id::new("toasts")).anchor(Align2::RIGHT_BOTTOM, Vec2::new(-18.0, -40.0)).order(egui::Order::Tooltip).interactable(true).show(ctx, |ui| {
            ui.set_width(340.0);
            for i in start..n {
                let t = &mut self.toasts[i];
                let appear = ((now - t.born) as f32 / 0.28).clamp(0.0, 1.0);
                let appear = 1.0 - (1.0 - appear).powi(3);
                let fade = (t.life / 0.35).clamp(0.0, 1.0);
                let a = appear.min(fade);
                let (col, icon) = match t.n.kind.as_str() {
                    "error" => (p.err, I::WARNING_CIRCLE),
                    "success" => (p.ok, I::CHECK_CIRCLE),
                    "warning" => (p.warn, I::WARNING),
                    "captcha" => (p.warn, I::SHIELD_CHECK),
                    _ => (p.acc, I::INFO),
                };
                ui.add_space(8.0 * a);
                let offset = (1.0 - appear) * 60.0;
                let resp = egui::Frame::new().fill(p.elevated).stroke(Stroke::new(1.0, p.line)).corner_radius(12).inner_margin(egui::Margin { left: 14, right: 12, top: 12, bottom: 12 })
                    .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha((90.0 * a) as u8) })
                    .multiply_with_opacity(a)
                    .show(ui, |ui| {
                        ui.set_width(312.0);
                        ui.horizontal(|ui| {
                            ui.add_space(offset);
                            ui.label(egui::RichText::new(icon).font(f(20.0)).color(col));
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&t.n.title).font(fs(13.5)).color(p.text));
                                if !t.n.body.is_empty() {
                                    ui.label(egui::RichText::new(&t.n.body).font(f(12.0)).color(p.dim));
                                }
                            });
                        });
                    }).response;
                let resp = ui.interact(resp.rect, egui::Id::new(("toast", &t.n.id)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                // life bar
                let lr = Rect::from_min_size(Pos2::new(resp.rect.left() + 12.0, resp.rect.bottom() - 3.0), Vec2::new((resp.rect.width() - 24.0) * (t.life / 6.0).clamp(0.0, 1.0), 2.0));
                ui.painter().rect_filled(lr, 1.0, alpha(col, 0.6 * a));
                if !resp.hovered() {
                    t.life -= dt;
                }
                if resp.clicked() {
                    go = Some(if t.n.kind == "captcha" { Cmd::Go(View::Captchas) } else if let Some(l) = &t.n.link_id { Cmd::Link(l.clone()) } else { Cmd::Go(View::Downloads) });
                    t.life = t.life.min(0.3);
                }
                if t.life <= 0.0 {
                    remove.push(i);
                }
            }
        });
        for i in remove.into_iter().rev() {
            self.toasts.remove(i);
        }
        if let Some(c) = go {
            self.run_cmd(c);
        }
    }

    fn drop_overlay(&mut self, ctx: &egui::Context) {
        let p = self.pal.clone();
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let t = ctx.animate_bool_with_time(egui::Id::new("drop"), hovering, 0.15);
        if t > 0.0 {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop-layer")));
            painter.rect_filled(screen, 0.0, alpha(p.bg, 0.82 * t));
            let r = screen.shrink(24.0);
            painter.rect_stroke(r, 18.0, Stroke::new(2.0, alpha(p.acc, t)), StrokeKind::Inside);
            painter.text(r.center() - Vec2::new(0.0, 18.0), Align2::CENTER_CENTER, I::TRAY_ARROW_DOWN, f(48.0), alpha(p.acc, t));
            painter.text(r.center() + Vec2::new(0.0, 28.0), Align2::CENTER_CENTER, "Drop link lists to add them", fs(16.0), alpha(p.text, t));
        }
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            let mut text = String::new();
            for d in dropped {
                if let Some(path) = d.path {
                    if let Ok(s) = std::fs::read_to_string(&path) {
                        text.push_str(&s);
                        text.push('\n');
                    }
                } else if let Some(bytes) = d.bytes {
                    text.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            if crate::crawler::extract_urls(&text).is_empty() {
                self.core.notify("warning", "No links in the dropped file", "Drop .txt / .html files that contain links.", None);
            } else {
                self.add = AddDialog { open: true, text, focus: true, ..Default::default() };
            }
        }
    }
}

fn nav_item(ui: &mut egui::Ui, icon: &str, label: &str, n: usize, on: bool, alert: bool, p: &Palette) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let h = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    let s = ui.ctx().animate_bool_with_time(resp.id.with("s"), on, 0.18);
    let painter = ui.painter();
    if h > 0.0 || s > 0.0 {
        painter.rect_filled(r, 9.0, mix(alpha(p.text, 0.05 * h), p.acc_soft, s));
    }
    if s > 0.0 {
        painter.rect_filled(Rect::from_min_size(Pos2::new(r.left(), r.center().y - 8.0 * s), Vec2::new(3.0, 16.0 * s)), 2.0, p.acc);
    }
    let col = mix(mix(p.dim, p.text, h), p.text, s);
    painter.text(Pos2::new(r.left() + 14.0, r.center().y), Align2::LEFT_CENTER, icon, f(17.0), if on { p.acc } else { col });
    painter.text(Pos2::new(r.left() + 40.0, r.center().y), Align2::LEFT_CENTER, label, fm(13.5), col);
    if n > 0 {
        let txt = if n > 999 { "999+".to_string() } else { n.to_string() };
        let g = widgets::galley(ui, &txt, fm(11.0), Color32::WHITE, 80.0);
        let br = Rect::from_min_size(Pos2::new(r.right() - g.size().x - 22.0, r.center().y - 9.0), Vec2::new(g.size().x + 12.0, 18.0));
        let bg = if alert { p.warn } else if on { p.acc } else { alpha(p.text, 0.1) };
        if alert {
            let t = ui.input(|i| i.time) as f32;
            let pulse = (t * 3.0).sin() * 0.5 + 0.5;
            painter.rect_filled(br.expand(2.0 + pulse * 2.0), 11.0, alpha(p.warn, 0.18 * (1.0 - pulse)));
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        painter.rect_filled(br, 9.0, bg);
        painter.galley(Pos2::new(br.left() + 6.0, br.center().y - g.size().y / 2.0), g, if alert || on { Color32::WHITE } else { p.dim });
    }
    resp
}

/// The Fetchwing mark (wing over a download chevron) on a gradient tile.
pub fn paint_logo(painter: &egui::Painter, r: Rect, p: &Palette) {
    let s = r.width() / 64.0;
    painter.rect_filled(r, 9.0 * s * 64.0 / 30.0, p.acc);
    widgets::gradient_pill(painter, Rect::from_min_size(r.min, Vec2::new(r.width(), 0.0)), p.acc, p.acc2);
    let tile = egui::epaint::RectShape::filled(r, (r.width() * 0.28) as u8, p.acc);
    painter.add(tile);
    let pt = |x: f32, y: f32| Pos2::new(r.left() + (8.0 + x * 0.75) * s, r.top() + (6.0 + y * 0.75) * s);
    let white = Color32::WHITE;
    for (i, poly) in [vec![pt(6.0, 38.0), pt(46.0, 6.0), pt(58.0, 8.0), pt(28.0, 30.0)], vec![pt(14.0, 43.0), pt(52.0, 18.0), pt(40.0, 30.0), pt(28.0, 40.0)], vec![pt(22.0, 47.0), pt(48.0, 30.0), pt(38.0, 42.0)], vec![pt(32.0, 64.0), pt(20.0, 52.0), pt(44.0, 52.0)]].into_iter().enumerate() {
        let a = [1.0, 0.82, 0.64, 1.0][i];
        painter.add(egui::Shape::convex_polygon(poly, alpha(white, a), Stroke::NONE));
    }
}

// ------------------------------------------------------------------------------------------------
// dialogs: add, palette, onboarding, small
// ------------------------------------------------------------------------------------------------

impl App {
    fn add_dialog(&mut self, ctx: &egui::Context) {
        let t = ctx.animate_bool_with_time(egui::Id::new("add-anim"), self.add.open, 0.18);
        if t <= 0.0 {
            return;
        }
        let p = self.pal.clone();
        let (curls, rest) = crate::crawler::parse_curl_commands(&self.add.text);
        let mut urls: Vec<(String, bool)> = curls.into_iter().map(|(u, _)| (u, true)).collect();
        urls.extend(crate::crawler::extract_urls(&rest).into_iter().map(|u| (u, false)));
        let n = urls.len();
        let mut submit = false;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("add-modal"))
            .backdrop_color(Color32::from_black_alpha((150.0 * t) as u8))
            .frame(egui::Frame::new().fill(p.elevated).stroke(Stroke::new(1.0, p.line)).corner_radius(18).inner_margin(24).shadow(egui::epaint::Shadow { offset: [0, 20], blur: 50, spread: 0, color: Color32::from_black_alpha(140) }))
            .show(ctx, |ui| {
                ui.multiply_opacity(t);
                ui.set_width(660.0);
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                    widgets::gradient_pill(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(40.0)), p.acc, p.acc2);
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, I::LINK_SIMPLE, f(20.0), Color32::WHITE);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("Add links").font(fs(19.0)).color(p.text));
                        ui.label(egui::RichText::new("Paste anything - URLs, forum posts, HTML, or a browser \"Copy as cURL\". Folders, playlists and pages are crawled.").font(f(12.5)).color(p.dim));
                    });
                });
                ui.add_space(14.0);
                let te = egui::Frame::new().fill(p.surface2).stroke(Stroke::new(1.0, p.line)).corner_radius(12).inner_margin(12).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(150.0).id_salt("add-text").show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut self.add.text).id(egui::Id::new("add-text-edit")).frame(false).desired_rows(6).desired_width(f32::INFINITY).font(egui::FontId::monospace(12.5)).hint_text("https://…"))
                    }).inner
                }).inner;
                if self.add.focus {
                    te.request_focus();
                    self.add.focus = false;
                }
                ui.add_space(10.0);
                // live preview: which plugin will take each link
                if n > 0 {
                    let reg = self.core.registry.read().clone();
                    egui::Frame::new().fill(p.surface).corner_radius(12).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{n} link{} detected", if n == 1 { "" } else { "s" })).font(fs(12.5)).color(p.text));
                            if urls.iter().any(|u| u.1) {
                                widgets::pill_ui(ui, "with browser cookies", p.ok);
                            }
                        });
                        ui.add_space(4.0);
                        for (u, curl) in urls.iter().take(5) {
                            let (who, col) = match (reg.find_crawler(u), reg.find_host(u)) {
                                (Some(c), _) => (c.meta().name.clone(), p.acc2),
                                (None, Some(h)) if h.meta().id == "generic-http" => (if *curl { "Direct (cURL)".into() } else { "Direct / page scan".into() }, p.dim),
                                (None, Some(h)) => (h.meta().name.clone(), p.acc),
                                _ => ("Unsupported".into(), p.err),
                            };
                            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
                            let host = crate::util::http::host_of(u);
                            widgets::host_badge(ui.painter(), Rect::from_min_size(Pos2::new(r.left(), r.center().y - 9.0), Vec2::splat(18.0)), &host);
                            let pill_w = widgets::galley(ui, &who, fm(11.5), col, 200.0).size().x + 16.0;
                            widgets::text_at(ui, ui.painter(), r.left() + 26.0, r.center().y, u, f(12.0), p.dim, r.width() - pill_w - 40.0);
                            widgets::pill(ui, ui.painter(), r.right() - pill_w, r.center().y, &who, col);
                        }
                        if n > 5 {
                            ui.label(egui::RichText::new(format!("and {} more…", n - 5)).font(f(12.0)).color(p.faint));
                        }
                    });
                    ui.add_space(10.0);
                }
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 8.0) / 2.0;
                    labeled_field(ui, I::PACKAGE, "Package name", "automatic", &mut self.add.pkg, w, false, &p);
                    labeled_field(ui, I::LOCK_KEY, "Archive password", "optional", &mut self.add.pw, w, true, &p);
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let mut auto = !self.add.review;
                    if widgets::switch(ui, &mut auto, &p).changed() {
                        self.add.review = !auto;
                    }
                    ui.label(egui::RichText::new(if auto { "Start downloading right away" } else { "Keep in Inbox for review" }).font(fm(13.0)).color(p.text));
                });
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    widgets::kbd(ui, "Ctrl+Enter", &p);
                    ui.label(egui::RichText::new("add").font(f(12.0)).color(p.faint));
                    ui.add_space(6.0);
                    widgets::kbd(ui, "Esc", &p);
                    ui.label(egui::RichText::new("close").font(f(12.0)).color(p.faint));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if n == 0 { "Add links".to_string() } else { format!("Add {n} link{}", if n == 1 { "" } else { "s" }) };
                        let b = ui.add_enabled_ui(n > 0, |ui| widgets::button(ui, Some(I::ARROW_RIGHT), &label, Kind::Primary, &p)).inner;
                        if b.clicked() {
                            submit = true;
                        }
                        if widgets::button(ui, None, "Cancel", Kind::Ghost, &p).clicked() {
                            close = true;
                        }
                    });
                });
                if n > 0 && ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter)) {
                    submit = true;
                }
            });
        if modal.should_close() {
            close = true;
        }
        if submit {
            let text = std::mem::take(&mut self.add.text);
            let (pkg, pw, review) = (std::mem::take(&mut self.add.pkg), std::mem::take(&mut self.add.pw), self.add.review);
            self.add_links_now(&text, Some(pkg), Some(pw), review);
            self.add.open = false;
        } else if close {
            self.add.open = false;
        }
    }

    fn commands(&self) -> Vec<(&'static str, String, String, Cmd)> {
        let st = self.core.stats();
        let mut v: Vec<(&'static str, String, String, Cmd)> = vec![
            (I::PLUS, "Add links".into(), "Ctrl+N · or paste anywhere".into(), Cmd::Add),
            (if st.paused { I::PLAY } else { I::PAUSE }, if st.paused { "Resume all downloads".into() } else { "Pause all downloads".into() }, "".into(), if st.paused { Cmd::Start } else { Cmd::Pause }),
            (I::DOWNLOAD_SIMPLE, "Go to Downloads".into(), "".into(), Cmd::Go(View::Downloads)),
            (I::SHIELD_CHECK, "Go to Captchas".into(), "".into(), Cmd::Go(View::Captchas)),
            (I::KEY, "Go to Accounts".into(), "Premium & debrid".into(), Cmd::Go(View::Accounts)),
            (I::PUZZLE_PIECE, "Go to Plugins".into(), "".into(), Cmd::Go(View::Plugins)),
            (I::GEAR_SIX, "Open Settings".into(), "".into(), Cmd::Go(View::Settings)),
            (I::LIST_BULLETS, "Open activity log".into(), "".into(), Cmd::Go(View::Log)),
            (I::ARROWS_CLOCKWISE, "Retry all failed".into(), "".into(), Cmd::RetryFailed),
            (I::BROOM, "Clear finished downloads".into(), "".into(), Cmd::ClearFinished),
            (I::FOLDER_OPEN, "Open download folder".into(), self.core.settings.str("general.downloadDir"), Cmd::OpenDownloads),
            (I::DATABASE, "Open data folder".into(), self.core.data_dir.display().to_string(), Cmd::OpenData),
            (I::GLOBE, "Open web UI".into(), self.api_url.clone(), Cmd::WebUi),
            (I::CIRCLE_HALF, "Toggle light / dark theme".into(), "".into(), Cmd::Theme),
            (I::FILM_STRIP, "Update yt-dlp now".into(), "".into(), Cmd::UpdateYtdlp),
            (I::PUZZLE_PIECE, "Reload plugins".into(), "".into(), Cmd::ReloadPlugins),
            (I::GAUGE, "Speed limit: none".into(), "".into(), Cmd::Limit(0)),
            (I::GAUGE, "Speed limit: 1 MB/s".into(), "".into(), Cmd::Limit(1 << 20)),
            (I::GAUGE, "Speed limit: 5 MB/s".into(), "".into(), Cmd::Limit(5 << 20)),
            (I::GAUGE, "Speed limit: 10 MB/s".into(), "".into(), Cmd::Limit(10 << 20)),
        ];
        for d in crate::settings::schema() {
            v.push((I::SLIDERS_HORIZONTAL, format!("Setting: {}", d.label), d.group.to_string(), Cmd::Setting(d.label.to_string())));
        }
        for l in self.links.iter().take(3000) {
            v.push((I::FILE, l.name.clone(), format!("{} · {}", l.host, widgets::status_style(l.status, &self.pal).1), Cmd::Link(l.id.clone())));
        }
        v
    }

    fn command_palette(&mut self, ctx: &egui::Context) {
        let t = ctx.animate_bool_with_time(egui::Id::new("pal-anim"), self.palette.open, 0.14);
        if t <= 0.0 {
            return;
        }
        let p = self.pal.clone();
        let q = self.palette.query.to_lowercase();
        let mut items: Vec<(i32, (&'static str, String, String, Cmd))> = self.commands().into_iter().filter_map(|c| fuzzy(&q, &c.1).map(|s| (s, c))).collect();
        items.sort_by(|a, b| b.0.cmp(&a.0));
        items.truncate(9);
        if self.palette.idx >= items.len() {
            self.palette.idx = items.len().saturating_sub(1);
        }
        let mut chosen: Option<Cmd> = None;
        let (up, down, enter) = ctx.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.key_pressed(Key::Enter)));
        if down {
            self.palette.idx = (self.palette.idx + 1).min(items.len().saturating_sub(1));
        }
        if up {
            self.palette.idx = self.palette.idx.saturating_sub(1);
        }
        if enter && !items.is_empty() {
            chosen = Some(items[self.palette.idx].1 .3.clone());
        }
        let screen = ctx.screen_rect();
        let m = egui::Modal::new(egui::Id::new("palette"))
            .backdrop_color(Color32::from_black_alpha((120.0 * t) as u8))
            .area(egui::Area::new(egui::Id::new("palette-area")).fixed_pos(Pos2::new(screen.center().x - 300.0, screen.top() + 90.0 - 10.0 * (1.0 - t))).order(egui::Order::Foreground))
            .frame(egui::Frame::new().fill(p.elevated).stroke(Stroke::new(1.0, p.line)).corner_radius(16).inner_margin(10).shadow(egui::epaint::Shadow { offset: [0, 20], blur: 50, spread: 0, color: Color32::from_black_alpha(140) }))
            .show(ctx, |ui| {
                ui.multiply_opacity(t);
                ui.set_width(600.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(I::MAGNIFYING_GLASS).font(f(18.0)).color(p.dim));
                    let r = ui.add(egui::TextEdit::singleline(&mut self.palette.query).id(egui::Id::new("pal-q")).frame(false).font(f(16.0)).hint_text("Type a command, a setting or a file name…").desired_width(ui.available_width() - 40.0).margin(Vec2::new(4.0, 10.0)));
                    if !r.has_focus() {
                        r.request_focus();
                    }
                    if r.changed() {
                        self.palette.idx = 0;
                    }
                    widgets::kbd(ui, "Esc", &p);
                });
                ui.add_space(4.0);
                let (sep, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
                ui.painter().rect_filled(sep, 0.0, p.line_soft);
                ui.add_space(4.0);
                if items.is_empty() {
                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| ui.label(egui::RichText::new("Nothing matches").color(p.dim)));
                    ui.add_space(14.0);
                }
                for (i, (_, (icon, title, sub, cmd))) in items.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
                    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
                    if resp.hovered() && ui.input(|i| i.pointer.delta().length() > 0.0) {
                        self.palette.idx = i;
                    }
                    let on = i == self.palette.idx;
                    let h = ui.ctx().animate_bool_with_time(resp.id.with("pal"), on, 0.08);
                    if h > 0.0 {
                        ui.painter().rect_filled(r, 9.0, mix(Color32::TRANSPARENT, p.acc_soft, h));
                    }
                    ui.painter().text(Pos2::new(r.left() + 16.0, r.center().y), Align2::LEFT_CENTER, *icon, f(17.0), if on { p.acc } else { p.dim });
                    widgets::text_at(ui, ui.painter(), r.left() + 44.0, r.center().y, title, fm(13.5), p.text, r.width() * 0.6);
                    widgets::text_right(ui, ui.painter(), r.right() - 14.0, r.center().y, &truncate(sub, 48), f(12.0), p.faint);
                    if resp.clicked() {
                        chosen = Some(cmd.clone());
                    }
                }
            });
        if m.should_close() {
            self.palette.open = false;
        }
        if let Some(c) = chosen {
            self.palette.open = false;
            self.run_cmd(c);
        }
    }

    fn onboarding_dialog(&mut self, ctx: &egui::Context) {
        if !self.onboarding {
            return;
        }
        let p = self.pal.clone();
        let mut done = false;
        egui::Modal::new(egui::Id::new("onboarding"))
            .backdrop_color(Color32::from_black_alpha(170))
            .frame(egui::Frame::new().fill(p.elevated).stroke(Stroke::new(1.0, p.line)).corner_radius(20).inner_margin(30))
            .show(ctx, |ui| {
                ui.set_width(560.0);
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
                    paint_logo(ui.painter(), r, &p);
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("Welcome to Fetchwing").font(fs(24.0)).color(p.text));
                    ui.label(egui::RichText::new("Three quick choices and you're ready. You can change all of this later.").font(f(13.0)).color(p.dim));
                });
                ui.add_space(22.0);
                step(ui, "1", "Where should downloads go?", &p);
                ui.horizontal(|ui| {
                    let mut dir = self.core.settings.str("general.downloadDir");
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width() - 104.0, 34.0), Sense::hover());
                    ui.painter().rect(r, 8.0, p.surface2, Stroke::new(1.0, p.line), StrokeKind::Inside);
                    ui.painter().text(Pos2::new(r.left() + 12.0, r.center().y), Align2::LEFT_CENTER, I::FOLDER_SIMPLE, f(15.0), p.acc);
                    widgets::text_at(ui, ui.painter(), r.left() + 34.0, r.center().y, &middle_ellipsis(&dir, 58), f(13.0), p.text, r.width() - 44.0);
                    resp.on_hover_text(&dir);
                    if widgets::button(ui, Some(I::FOLDER_OPEN), "Browse", Kind::Secondary, &p).clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
                            let _ = self.core.settings.set("general.downloadDir", serde_json::json!(d.to_string_lossy()));
                            self.core.settings_changed("general.downloadDir");
                        }
                    }
                });
                ui.add_space(16.0);
                step(ui, "2", "Start downloads automatically?", &p);
                ui.horizontal(|ui| {
                    let mut auto = self.core.settings.bool("general.autoStart");
                    if widgets::switch(ui, &mut auto, &p).changed() {
                        let _ = self.core.settings.set("general.autoStart", serde_json::json!(auto));
                        self.core.settings_changed("general.autoStart");
                    }
                    ui.label(egui::RichText::new(if auto { "Yes - links start right after they're checked" } else { "No - links wait in the Inbox until I start them" }).color(p.text));
                });
                ui.add_space(16.0);
                step(ui, "3", "Have a debrid service? (optional)", &p);
                ui.label(egui::RichText::new("It unlocks premium speed on hundreds of hosters and removes waits and captchas.").font(f(12.0)).color(p.dim));
                ui.horizontal(|ui| {
                    let services = ["real-debrid.com", "alldebrid.com", "premiumize.me", "torbox.app"];
                    egui::ComboBox::from_id_salt("ob-svc").selected_text(services[self.ob_token_service]).width(150.0).show_ui(ui, |ui| {
                        for (i, s) in services.iter().enumerate() {
                            ui.selectable_value(&mut self.ob_token_service, i, *s);
                        }
                    });
                    ui.add(egui::TextEdit::singleline(&mut self.ob_token).password(true).hint_text(egui::RichText::new("Paste your API token").color(p.faint)).desired_width(ui.available_width()).margin(Vec2::new(10.0, 7.0)));
                });
                ui.add_space(26.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, Some(I::ARROW_RIGHT), "Get started", Kind::Primary, &p).clicked() {
                            done = true;
                        }
                        if widgets::button(ui, None, "Skip", Kind::Ghost, &p).clicked() {
                            done = true;
                            self.ob_token.clear();
                        }
                    });
                });
            });
        if done {
            if !self.ob_token.trim().is_empty() {
                let host = ["real-debrid.com", "alldebrid.com", "premiumize.me", "torbox.app"][self.ob_token_service];
                self.core.add_account(host, "", self.ob_token.trim());
            }
            self.core.state.lock().kv.insert("ui.onboarded".into(), serde_json::json!(true));
            self.core.settings_changed("ui.onboarded");
            self.onboarding = false;
        }
    }

    fn small_dialogs(&mut self, ctx: &egui::Context) {
        let p = self.pal.clone();
        if let Some((id, mut name)) = self.rename.clone() {
            let mut keep = true;
            let m = egui::Modal::new(egui::Id::new("rename")).frame(egui::Frame::new().fill(p.elevated).corner_radius(16).inner_margin(22).stroke(Stroke::new(1.0, p.line))).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.label(egui::RichText::new("Rename package").font(fs(16.0)));
                ui.add_space(8.0);
                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                r.request_focus();
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, None, "Save", Kind::Primary, &p).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            let _ = self.core.update_package(&id, Some(name.clone()), None, None, None, None);
                            keep = false;
                        }
                        if widgets::button(ui, None, "Cancel", Kind::Ghost, &p).clicked() {
                            keep = false;
                        }
                    });
                });
            });
            if m.should_close() {
                keep = false;
            }
            self.rename = keep.then_some((id, name));
        }
        if let Some((id, mut pw)) = self.pw_edit.clone() {
            let mut keep = true;
            let m = egui::Modal::new(egui::Id::new("pwedit")).frame(egui::Frame::new().fill(p.elevated).corner_radius(16).inner_margin(22).stroke(Stroke::new(1.0, p.line))).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.label(egui::RichText::new("Archive passwords").font(fs(16.0)));
                ui.label(egui::RichText::new("One per line. Tried when extracting this package.").font(f(12.5)).color(p.dim));
                ui.add_space(8.0);
                ui.add(egui::TextEdit::multiline(&mut pw).desired_rows(4).desired_width(f32::INFINITY));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, Some(I::FILE_ZIP), "Save & extract", Kind::Primary, &p).clicked() {
                            let list: Vec<String> = pw.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
                            let _ = self.core.update_package(&id, None, None, Some(list), None, None);
                            let c = self.core.clone();
                            let pid = id.clone();
                            self.rt.spawn(async move { c.extract_package(&pid).await });
                            keep = false;
                        }
                        if widgets::button(ui, None, "Cancel", Kind::Ghost, &p).clicked() {
                            keep = false;
                        }
                    });
                });
            });
            if m.should_close() {
                keep = false;
            }
            self.pw_edit = keep.then_some((id, pw));
        }
    }
}

fn step(ui: &mut egui::Ui, n: &str, title: &str, p: &Palette) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 11.0, p.acc_soft);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, n, fs(12.0), p.acc);
        ui.label(egui::RichText::new(title).font(fs(14.0)).color(p.text));
    });
    ui.add_space(6.0);
}

fn labeled_field(ui: &mut egui::Ui, icon: &str, label: &str, hint: &str, value: &mut String, w: f32, password: bool, p: &Palette) {
    ui.allocate_ui(Vec2::new(w, 56.0), |ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(format!("{icon}  {label}")).font(fm(12.0)).color(p.dim));
            ui.add(egui::TextEdit::singleline(value).hint_text(hint).password(password).desired_width(w).margin(Vec2::new(10.0, 7.0)));
        });
    });
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("…{}", s.chars().rev().take(n - 1).collect::<Vec<_>>().into_iter().rev().collect::<String>()) }
}

/// Subsequence fuzzy match; higher is better. Prefix/word-start hits score extra.
fn fuzzy(q: &str, text: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let t = text.to_lowercase();
    if let Some(pos) = t.find(q) {
        return Some(1000 - pos as i32 * 2 - (t.len() as i32 / 8));
    }
    let mut score = 0;
    let mut ti = t.char_indices().peekable();
    let mut last = 0usize;
    for qc in q.chars() {
        loop {
            let (i, c) = ti.next()?;
            if c == qc {
                score += if i == last + 1 { 8 } else { 2 };
                if i == 0 || t.as_bytes().get(i.wrapping_sub(1)).is_some_and(|b| *b == b' ') {
                    score += 6;
                }
                last = i;
                break;
            }
        }
    }
    Some(score)
}

/// "C:/Users/me/…/Downloads" - keeps both ends of long paths readable.
fn middle_ellipsis(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head = max * 2 / 5;
    let tail = max - head - 1;
    let a: String = s.chars().take(head).collect();
    let b: String = s.chars().skip(n - tail).collect();
    format!("{a}…{b}")
}
