//! Downloads view: stage switcher, filters, virtualized package/link list, floating selection
//! bar and the detail drawer.

use super::theme::{alpha, f, fi, fm, fs, mix};
use super::widgets::{self, Kind};
use super::{App, QFilter, View};
use crate::model::{Link, Package, Stage, Status};
use crate::util::{fmt_bytes, fmt_duration};
use eframe::egui::{self, Align2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use egui_phosphor::regular as I;
use std::collections::HashMap;

const H_PKG: f32 = 58.0;
const H_LINK: f32 = 52.0;
const H_GAP: f32 = 12.0;

enum Row {
    Pkg(usize, Agg),
    Link(usize, bool), // index, last in package
    Gap,
}

#[derive(Default, Clone, Copy)]
struct Agg {
    n: usize,
    done: i64,
    total: i64,
    speed: u64,
    active: usize,
    failed: usize,
    finished: usize,
}

enum Act {
    Links(&'static str, Vec<String>),
    Pkg(&'static str, String),
    Toggle(String, bool),
    Click(String, egui::Modifiers),
}

fn stage_of(tab: usize) -> Stage {
    match tab {
        0 => Stage::Inbox,
        2 => Stage::Done,
        _ => Stage::Queue,
    }
}

fn qmatch(q: QFilter, s: Status) -> bool {
    match q {
        QFilter::All => true,
        QFilter::Active => matches!(s, Status::Downloading | Status::Postprocessing | Status::Captcha),
        QFilter::Waiting => matches!(s, Status::Waiting | Status::Queued | Status::Paused),
        QFilter::Failed => matches!(s, Status::Failed | Status::Offline),
    }
}

impl App {
    fn filtered(&self) -> (Vec<usize>, HashMap<&str, Vec<usize>>) {
        let stage = stage_of(self.tab);
        let q = self.filter.to_lowercase();
        let mut by: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, l) in self.links.iter().enumerate() {
            if l.stage != stage || (stage == Stage::Queue && !qmatch(self.qfilter, l.status)) {
                continue;
            }
            if !q.is_empty() && !l.name.to_lowercase().contains(&q) && !l.host.contains(&q) {
                continue;
            }
            by.entry(l.package_id.as_str()).or_default().push(i);
        }
        let mut pk: Vec<usize> = (0..self.packages.len()).filter(|&i| by.contains_key(self.packages[i].id.as_str())).collect();
        pk.sort_by(|&a, &b| {
            let (pa, pb) = (&self.packages[a], &self.packages[b]);
            pb.priority.cmp(&pa.priority).then(pa.order.cmp(&pb.order))
        });
        (pk, by)
    }

    pub(super) fn visible_link_order(&self) -> Vec<String> {
        let (pk, by) = self.filtered();
        let mut out = vec![];
        for p in pk {
            let pkg = &self.packages[p];
            if pkg.expanded || !self.filter.is_empty() {
                for &i in &by[pkg.id.as_str()] {
                    out.push(self.links[i].id.clone());
                }
            }
        }
        out
    }

    pub(super) fn downloads(&mut self, ui: &mut egui::Ui) {
        let p = self.pal.clone();
        let st = self.core.stats();
        let counts = |pred: &dyn Fn(&Link) -> bool| self.links.iter().filter(|l| pred(l)).count();
        let n_active = counts(&|l| l.stage == Stage::Queue && qmatch(QFilter::Active, l.status));
        let n_wait = counts(&|l| l.stage == Stage::Queue && qmatch(QFilter::Waiting, l.status));
        let n_fail = counts(&|l| l.stage == Stage::Queue && qmatch(QFilter::Failed, l.status));
        let n_queue = counts(&|l| l.stage == Stage::Queue);

        // ---------- header ----------
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Downloads").font(fs(24.0)).color(p.text));
            ui.add_space(14.0);
            let items = vec![("Inbox".to_string(), st.inbox), ("Queue".to_string(), n_queue), ("Done".to_string(), st.finished)];
            if widgets::segmented(ui, "dl-tabs", &items, &mut self.tab, &p) {
                self.sel.clear();
            }
            // right-aligned search (explicit spacing: nested right-to-left layouts misplace widgets)
            let w = (ui.available_width() - 90.0).clamp(150.0, 300.0);
            ui.add_space((ui.available_width() - w - 74.0).max(8.0));
            widgets::search(ui, "dl-search", &mut self.filter, "Search files and hosts…", w, &p);
            widgets::kbd(ui, "Ctrl+F", &p);
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            match self.tab {
                1 if n_queue > 0 => {
                    for (q, label, n, col) in [(QFilter::All, "All".to_string(), n_queue, p.dim), (QFilter::Active, format!("Active {n_active}"), n_active, p.acc), (QFilter::Waiting, format!("Waiting {n_wait}"), n_wait, p.warn), (QFilter::Failed, format!("Failed {n_fail}"), n_fail, p.err)] {
                        if q != QFilter::All && n == 0 && self.qfilter != q {
                            continue;
                        }
                        if widgets::chip(ui, &label, self.qfilter == q, col, &p).clicked() {
                            self.qfilter = q;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if n_fail > 0 && widgets::button(ui, Some(I::ARROWS_CLOCKWISE), "Retry failed", Kind::Ghost, &p).clicked() {
                            let f: Vec<String> = self.links.iter().filter(|l| l.stage == Stage::Queue && qmatch(QFilter::Failed, l.status)).map(|l| l.id.clone()).collect();
                            self.core.resume_links(&f);
                        }
                    });
                }
                1 => {}
                0 => {
                    ui.label(egui::RichText::new(format!("{} Links wait here for review before they download.", I::TRAY)).font(f(13.0)).color(p.dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if st.inbox > 0 {
                            if widgets::button(ui, Some(I::PLAY), "Start all", Kind::Primary, &p).clicked() {
                                let all: Vec<String> = self.links.iter().filter(|l| l.stage == Stage::Inbox).map(|l| l.id.clone()).collect();
                                self.core.confirm(&all);
                                self.tab = 1;
                            }
                            if widgets::button(ui, Some(I::TRASH), "Discard all", Kind::Ghost, &p).clicked() {
                                let all: Vec<String> = self.links.iter().filter(|l| l.stage == Stage::Inbox).map(|l| l.id.clone()).collect();
                                self.core.remove_links(&all, false);
                            }
                        }
                    });
                }
                _ => {
                    ui.label(egui::RichText::new(format!("{} Finished downloads. Double-click to show the file.", I::CHECK_CIRCLE)).font(f(13.0)).color(p.dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if st.finished > 0 && widgets::button(ui, Some(I::BROOM), "Clear list", Kind::Ghost, &p).clicked() {
                            self.core.cleanup("finished");
                        }
                    });
                }
            }
        });
        ui.add_space(10.0);

        // ---------- rows ----------
        let (pk, by) = self.filtered();
        let mut rows: Vec<Row> = vec![];
        for (n, &pi) in pk.iter().enumerate() {
            let pkg = &self.packages[pi];
            let ls = &by[pkg.id.as_str()];
            let mut a = Agg { n: ls.len(), ..Default::default() };
            for &i in ls {
                let l = &self.links[i];
                a.done += l.bytes_done as i64;
                a.total += l.size.max(0);
                a.speed += l.speed;
                if l.status == Status::Downloading { a.active += 1; }
                if matches!(l.status, Status::Failed | Status::Offline) { a.failed += 1; }
                if l.status == Status::Finished { a.finished += 1; }
            }
            if n > 0 {
                rows.push(Row::Gap);
            }
            rows.push(Row::Pkg(pi, a));
            if pkg.expanded || !self.filter.is_empty() {
                for (k, &i) in ls.iter().enumerate() {
                    rows.push(Row::Link(i, k + 1 == ls.len()));
                }
            }
        }
        if rows.is_empty() {
            let (icon, title, sub) = match (self.tab, self.filter.is_empty()) {
                (_, false) => (I::MAGNIFYING_GLASS, "No matches", "Try another name or host."),
                (0, _) => (I::TRAY, "Inbox zero", "Links you choose to review land here before downloading."),
                (2, _) => (I::CHECK_CIRCLE, "Nothing finished yet", "Completed downloads show up here."),
                _ if self.qfilter != QFilter::All => (I::FUNNEL, "Nothing in this filter", "Switch back to All to see everything."),
                _ => (I::CLOUD_ARROW_DOWN, "Ready when you are", "Copy a link anywhere and press Ctrl+V - or drop a .txt with links."),
            };
            widgets::empty_state(ui, icon, title, sub, &p);
            if self.tab == 1 && self.filter.is_empty() && self.qfilter == QFilter::All {
                ui.vertical_centered(|ui| {
                    ui.add_space(14.0);
                    if widgets::button(ui, Some(I::PLUS), "Add links", Kind::Primary, &p).clicked() {
                        self.add.open = true;
                        self.add.focus = true;
                    }
                });
            }
            return;
        }
        let heights: Vec<f32> = rows.iter().map(|r| match r { Row::Pkg(..) => H_PKG, Row::Link(..) => H_LINK, Row::Gap => H_GAP }).collect();
        let total: f32 = heights.iter().sum::<f32>() + 90.0; // room for the floating bar
        let mut acts: Vec<Act> = vec![];
        let now = ui.input(|i| i.time) as f32;
        egui::ScrollArea::vertical().auto_shrink(false).show_viewport(ui, |ui, vp| {
            ui.set_height(total);
            let origin = ui.max_rect().min;
            let width = ui.available_width();
            let mut y = 0.0;
            for (ri, row) in rows.iter().enumerate() {
                let h = heights[ri];
                if y + h < vp.min.y - 10.0 || y > vp.max.y + 10.0 {
                    y += h;
                    continue;
                }
                let rect = Rect::from_min_size(origin + Vec2::new(0.0, y), Vec2::new(width, h));
                y += h;
                match row {
                    Row::Gap => {}
                    Row::Pkg(pi, a) => self.paint_pkg(ui, rect, &self.packages[*pi], a, &mut acts),
                    Row::Link(i, last) => self.paint_link(ui, rect, &self.links[*i], *last, now, &mut acts),
                }
            }
        });

        // ---------- apply ----------
        let order = self.visible_link_order();
        for a in acts {
            match a {
                Act::Click(id, m) => {
                    if m.shift {
                        if let (Some(a), Some(b)) = (self.anchor.as_ref().and_then(|x| order.iter().position(|o| o == x)), order.iter().position(|o| *o == id)) {
                            let (lo, hi) = (a.min(b), a.max(b));
                            for x in &order[lo..=hi] {
                                self.sel.insert(x.clone());
                            }
                        }
                    } else if m.command {
                        if !self.sel.remove(&id) {
                            self.sel.insert(id.clone());
                        }
                        self.anchor = Some(id.clone());
                    } else {
                        let only = self.sel.len() == 1 && self.sel.contains(&id);
                        self.sel.clear();
                        if !only {
                            self.sel.insert(id.clone());
                        }
                        self.anchor = Some(id.clone());
                    }
                    self.cursor = Some(id);
                }
                Act::Toggle(pid, exp) => {
                    let _ = self.core.update_package(&pid, None, None, None, None, Some(exp));
                    if let Some(p) = self.packages.iter_mut().find(|p| p.id == pid) {
                        p.expanded = exp;
                    }
                }
                Act::Links(a, ids) => self.link_action(a, ids, ui.ctx()),
                Act::Pkg(a, pid) => self.pkg_action(a, pid),
            }
        }
        self.selection_bar(ui.ctx());
    }

    fn link_action(&mut self, a: &str, ids: Vec<String>, ctx: &egui::Context) {
        match a {
            "confirm" => {
                self.core.confirm(&ids);
            }
            "force" => self.core.force_start(&ids),
            "pause" => self.core.pause_links(&ids),
            "resume" => {
                self.core.confirm(&ids);
                self.core.resume_links(&ids)
            }
            "reset" => self.core.reset_links(&ids),
            "check" => self.core.queue_checks(ids),
            "remove" => {
                let n = ids.len();
                self.core.remove_links(&ids, false);
                self.sel.retain(|s| !ids.contains(s));
                self.core.notify("info", &format!("Removed {n} item{}", if n == 1 { "" } else { "s" }), "Files on disk were kept.", None);
            }
            "removefiles" => {
                self.core.remove_links(&ids, true);
                self.sel.retain(|s| !ids.contains(s));
            }
            "toggle" => {
                let on = self.links.iter().find(|l| l.id == ids[0]).is_some_and(|l| !l.enabled);
                self.core.set_enabled(&ids, on);
            }
            "up" => self.core.set_priority(&ids, 1),
            "down" => self.core.set_priority(&ids, -1),
            "copy" => {
                let urls: Vec<String> = self.links.iter().filter(|l| ids.contains(&l.id)).map(|l| l.url.clone()).collect();
                ctx.copy_text(urls.join("\n"));
                self.core.notify("success", "Copied to clipboard", &format!("{} URL(s)", urls.len()), None);
            }
            "open" => {
                if let Some(l) = self.links.iter().find(|l| l.id == ids[0]) {
                    let p = l.prop_str("finalPath").map(std::path::PathBuf::from).or_else(|| self.packages.iter().find(|p| p.id == l.package_id).map(|p| std::path::PathBuf::from(&p.dir)));
                    if let Some(p) = p {
                        let _ = std::fs::create_dir_all(if p.is_dir() { &p } else { p.parent().unwrap_or(&p) });
                        crate::api::reveal(&p);
                    }
                }
            }
            "openfile" => {
                if let Some(path) = self.links.iter().find(|l| l.id == ids[0]).and_then(|l| l.prop_str("finalPath").map(String::from)) {
                    let _ = open::that_detached(path);
                }
            }
            _ => {}
        }
    }

    fn pkg_action(&mut self, a: &str, pid: String) {
        let ids = self.core.links_of(&[pid.clone()]);
        match a {
            "start" => {
                self.core.confirm(&ids);
                self.core.resume_links(&ids);
            }
            "pause" => self.core.pause_links(&ids),
            "extract" => {
                let c = self.core.clone();
                self.rt.spawn(async move { c.extract_package(&pid).await });
            }
            "passwords" => {
                let pw = self.packages.iter().find(|p| p.id == pid).map(|p| p.passwords.join("\n")).unwrap_or_default();
                self.pw_edit = Some((pid, pw));
            }
            "rename" => {
                let n = self.packages.iter().find(|p| p.id == pid).map(|p| p.name.clone()).unwrap_or_default();
                self.rename = Some((pid, n));
            }
            "open" => {
                if let Some(p) = self.packages.iter().find(|p| p.id == pid) {
                    let _ = std::fs::create_dir_all(&p.dir);
                    crate::api::reveal(std::path::Path::new(&p.dir));
                }
            }
            "select" => {
                for i in ids {
                    self.sel.insert(i);
                }
            }
            "up" | "down" => {
                let cur = self.packages.iter().find(|p| p.id == pid).map(|p| p.priority).unwrap_or(0);
                let _ = self.core.update_package(&pid, None, None, None, Some(cur + if a == "up" { 1 } else { -1 }), None);
            }
            "remove" => self.core.remove_packages(&[pid], false),
            "removefiles" => self.core.remove_packages(&[pid], true),
            _ => {}
        }
    }

    fn paint_pkg(&self, ui: &mut egui::Ui, rect: Rect, pkg: &Package, a: &Agg, acts: &mut Vec<Act>) {
        let p = &self.pal;
        let id = egui::Id::new(("pkg", &pkg.id));
        let resp = ui.interact(rect, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        let h = ui.ctx().animate_bool_with_time(id.with("h"), resp.hovered(), 0.12);
        let painter = ui.painter_at(rect.expand(1.0));
        let open = pkg.expanded || !self.filter.is_empty();
        let cr = if open { egui::CornerRadius { nw: 14, ne: 14, sw: 0, se: 0 } } else { egui::CornerRadius::same(14) };
        painter.rect_filled(rect, cr, mix(p.surface, p.surface2, h * 0.6));
        painter.rect_stroke(rect, cr, Stroke::new(1.0, p.line_soft), StrokeKind::Inside);
        let y = rect.center().y;
        // chevron (rotates when expanding)
        let rot = ui.ctx().animate_bool_with_time(id.with("rot"), open, 0.18);
        let c = Pos2::new(rect.left() + 20.0, y);
        let ang = rot * std::f32::consts::FRAC_PI_2;
        let rotp = |x: f32, yv: f32| Pos2::new(c.x + x * ang.cos() - yv * ang.sin(), c.y + x * ang.sin() + yv * ang.cos());
        painter.add(egui::Shape::convex_polygon(vec![rotp(-2.5, -4.5), rotp(3.5, 0.0), rotp(-2.5, 4.5)], p.dim, Stroke::NONE));
        // progress ring
        let pct = if a.total > 0 { a.done as f32 / a.total as f32 } else if a.finished == a.n { 1.0 } else { 0.0 };
        let shown = ui.ctx().animate_value_with_time(id.with("pct"), pct, 0.5);
        let ring_col = if a.failed > 0 && a.active == 0 { Some(p.err) } else if a.finished == a.n { Some(p.ok) } else { None };
        widgets::ring(&painter, Pos2::new(rect.left() + 52.0, y), 13.0, 3.0, shown, p, ring_col);
        if a.finished == a.n {
            painter.text(Pos2::new(rect.left() + 52.0, y), Align2::CENTER_CENTER, I::CHECK, fs(12.0), p.ok);
        } else {
            painter.text(Pos2::new(rect.left() + 52.0, y), Align2::CENTER_CENTER, I::FOLDER_SIMPLE, f(12.0), p.dim);
        }
        let right_w = 330.0;
        let name_w = rect.width() - 80.0 - right_w;
        widgets::text_at(ui, &painter, rect.left() + 76.0, y - 8.0, &pkg.name, fs(14.5), p.text, name_w);
        let mut sub = format!("{} file{}", a.n, if a.n == 1 { "" } else { "s" });
        if a.active > 0 { sub += &format!(" · {} downloading", a.active); }
        if a.failed > 0 { sub += &format!(" · {} failed", a.failed); }
        if pkg.priority > 0 { sub += " · high priority"; }
        if pkg.priority < 0 { sub += " · low priority"; }
        widgets::text_at(ui, &painter, rect.left() + 76.0, y + 10.0, &sub, f(12.0), if a.failed > 0 { p.err } else { p.dim }, name_w);
        // right side: totals or hover actions
        let rx = rect.right() - 14.0;
        if h > 0.05 {
            let mut x = rx - 30.0;
            let mut btn = |icon: &str, tip: &str, act: &'static str, ui: &mut egui::Ui| {
                let r = Rect::from_min_size(Pos2::new(x, y - 15.0), Vec2::splat(30.0));
                let b = ui.interact(r, id.with(act), Sense::click()).on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand);
                if b.hovered() {
                    ui.painter().rect_filled(r, 8.0, alpha(p.text, 0.08));
                }
                ui.painter().text(r.center(), Align2::CENTER_CENTER, icon, f(16.0), alpha(if b.hovered() { p.text } else { p.dim }, h));
                if b.clicked() {
                    acts.push(Act::Pkg(act, pkg.id.clone()));
                }
                x -= 32.0;
            };
            btn(I::DOTS_THREE, "More", "menu", ui);
            btn(I::FOLDER_OPEN, "Open folder", "open", ui);
            if a.active > 0 { btn(I::PAUSE, "Pause package", "pause", ui) } else if a.finished < a.n { btn(I::PLAY, "Start package", "start", ui) }
        }
        let totals = if a.total > 0 { format!("{} of {}", fmt_bytes(a.done), fmt_bytes(a.total)) } else { String::new() };
        let speed = if a.speed > 0 { format!("{}/s", fmt_bytes(a.speed as i64)) } else { String::new() };
        let off = 110.0 * h;
        widgets::text_right(ui, &painter, rx - off, y - 8.0, &totals, fm(12.5), alpha(p.text, 1.0 - h * 0.6));
        widgets::text_right(ui, &painter, rx - off, y + 10.0, &speed, f(12.0), alpha(p.acc, 1.0 - h * 0.6));
        let bar = Rect::from_min_size(Pos2::new(rect.right() - right_w + 10.0, y - 2.5), Vec2::new(150.0, 5.0));
        if self.tab != 0 || a.done > 0 {
            widgets::progress_bar(ui, &painter, bar, shown, a.active > 0, ring_col, p);
        }
        if resp.clicked() {
            acts.push(Act::Toggle(pkg.id.clone(), !pkg.expanded));
        }
        let menu_open = ui.memory(|m| m.data.get_temp::<bool>(id.with("menu-open")).unwrap_or(false));
        let _ = menu_open;
        let pid = pkg.id.clone();
        let p2 = p.clone();
        resp.context_menu(|ui| pkg_menu(ui, &pid, &p2, acts));
        // the "…" button opens the same menu as a popup
        let popup_id = id.with("popup");
        if acts.iter().any(|x| matches!(x, Act::Pkg("menu", i) if *i == pkg.id)) {
            acts.retain(|x| !matches!(x, Act::Pkg("menu", _)));
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        let anchor = ui.interact(Rect::from_min_size(Pos2::new(rx - 30.0, y + 12.0), Vec2::new(30.0, 4.0)), id.with("anchor"), Sense::hover());
        egui::popup::popup_below_widget(ui, popup_id, &anchor, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(220.0);
            pkg_menu(ui, &pid, &p2, acts);
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_link(&self, ui: &mut egui::Ui, rect: Rect, l: &Link, last: bool, now: f32, acts: &mut Vec<Act>) {
        let p = &self.pal;
        let id = egui::Id::new(("link", &l.id));
        let resp = ui.interact(rect, id, Sense::click());
        let selected = self.sel.contains(&l.id);
        let h = ui.ctx().animate_bool_with_time(id.with("h"), resp.hovered(), 0.10);
        let s = ui.ctx().animate_bool_with_time(id.with("s"), selected, 0.14);
        let painter = ui.painter_at(rect.expand(1.0));
        let cr = if last { egui::CornerRadius { nw: 0, ne: 0, sw: 14, se: 14 } } else { egui::CornerRadius::ZERO };
        painter.rect_filled(rect, cr, p.surface);
        if h > 0.0 || s > 0.0 {
            painter.rect_filled(rect, cr, mix(alpha(p.text, 0.035 * h), p.sel, s));
        }
        // side borders + separator
        painter.vline(rect.left() + 0.5, rect.y_range(), Stroke::new(1.0, p.line_soft));
        painter.vline(rect.right() - 0.5, rect.y_range(), Stroke::new(1.0, p.line_soft));
        if last {
            painter.rect_stroke(rect, cr, Stroke::new(1.0, p.line_soft), StrokeKind::Inside);
        }
        painter.hline((rect.left() + 64.0)..=(rect.right() - 14.0), rect.top() + 0.5, Stroke::new(1.0, p.line_soft));
        if s > 0.0 {
            painter.rect_filled(Rect::from_min_size(Pos2::new(rect.left() + 1.0, rect.top() + 10.0), Vec2::new(3.0, rect.height() - 20.0)), 2.0, alpha(p.acc, s));
        }
        let y = rect.center().y;
        // file tile
        let (ficon, fcol) = widgets::file_icon(&l.name, p);
        let tile = Rect::from_center_size(Pos2::new(rect.left() + 38.0, y), Vec2::splat(32.0));
        painter.rect_filled(tile, 9.0, alpha(fcol, 0.14));
        painter.text(tile.center(), Align2::CENTER_CENTER, ficon, fi(17.0), fcol);
        let (scol, slabel) = widgets::status_style(l.status, p);
        // status dot on the tile corner
        let dot = Pos2::new(tile.right() - 2.0, tile.bottom() - 2.0);
        painter.circle_filled(dot, 5.5, p.surface);
        if l.status == Status::Downloading {
            let pulse = (now * 4.0).sin() * 0.5 + 0.5;
            painter.circle_filled(dot, 3.5 + pulse, alpha(scol, 0.35));
        }
        painter.circle_filled(dot, 3.5, scol);
        // name + subline
        let right_w = 430.0;
        let name_w = (rect.width() - 74.0 - right_w).max(120.0);
        let ncol = if l.enabled { p.text } else { p.faint };
        widgets::text_at(ui, &painter, rect.left() + 64.0, y - 8.5, &l.name, fm(13.5), ncol, name_w);
        let hb = Rect::from_min_size(Pos2::new(rect.left() + 64.0, y + 3.0), Vec2::splat(14.0));
        widgets::host_badge(&painter, hb, &l.host);
        let mut sub = l.host.clone();
        let mut subcol = p.dim;
        if let Some(e) = l.error.as_ref().filter(|_| matches!(l.status, Status::Failed | Status::Offline)) {
            sub = format!("{} · {}", l.host, e.hint.clone().unwrap_or(e.message.clone()));
            subcol = p.err;
        } else if let Some(m) = &l.message {
            sub = format!("{} · {}", l.host, m);
        }
        if let Some(w) = l.wait_until {
            let secs = ((w - crate::model::now_ms()) / 1000).max(0);
            sub = format!("{sub} · {}", fmt_duration(secs));
            subcol = p.warn;
        }
        if !l.enabled {
            sub = format!("{} · disabled", l.host);
        }
        widgets::text_at(ui, &painter, rect.left() + 84.0, y + 10.0, &sub, f(12.0), subcol, name_w - 20.0);

        // right block: status pill | progress | size  (hover shows quick actions)
        let rx = rect.right() - 16.0;
        let shown = ui.ctx().animate_value_with_time(id.with("p"), l.progress(), if l.status == Status::Downloading { 0.6 } else { 0.25 });
        let bar = Rect::from_min_size(Pos2::new(rect.right() - right_w + 112.0, y + 4.0), Vec2::new(160.0, 5.0));
        let pill_x = rect.right() - right_w + 4.0;
        let bar_col = match l.status {
            Status::Finished => Some(p.ok),
            Status::Failed | Status::Offline => Some(p.err),
            Status::Paused => Some(p.faint),
            Status::Waiting | Status::Captcha => Some(p.warn),
            _ => None,
        };
        widgets::pill(ui, &painter, pill_x, y, slabel, scol);
        let top_line = if l.status == Status::Downloading && l.speed > 0 {
            let eta = if l.size > 0 { ((l.size as u64).saturating_sub(l.bytes_done) / l.speed.max(1)) as i64 } else { -1 };
            format!("{:.0}%  ·  {}/s  ·  {}", shown * 100.0, fmt_bytes(l.speed as i64), fmt_duration(eta))
        } else if l.status == Status::Finished {
            "Completed".into()
        } else if l.size > 0 {
            format!("{:.0}%", shown * 100.0)
        } else {
            String::new()
        };
        if l.stage == Stage::Inbox && l.bytes_done == 0 {
            // nothing transferred yet: show what will handle the link instead of an empty 0% bar
            let via = if l.plugin_id.is_empty() { "–".to_string() } else { l.plugin_id.replace('-', " ") };
            widgets::text_at(ui, &painter, bar.left(), y, &format!("{} {}", I::PLUG, via), f(11.5), p.faint, 170.0);
        } else {
            widgets::text_at(ui, &painter, bar.left(), y - 8.0, &top_line, f(11.5), if l.status == Status::Downloading { p.text } else { p.dim }, 170.0);
            widgets::progress_bar(ui, &painter, bar, shown, l.status == Status::Downloading, bar_col, p);
        }
        if h < 0.5 {
            let size = if l.size > 0 && l.status != Status::Finished && l.bytes_done > 0 { format!("{} / {}", fmt_bytes(l.bytes_done as i64), fmt_bytes(l.size)) } else if l.size < 0 { "–".into() } else { fmt_bytes(l.size) };
            widgets::text_right(ui, &painter, rx, y, &size, fm(12.5), alpha(p.dim, 1.0 - h * 2.0));
        } else {
            let mut x = rx - 30.0;
            let ids = vec![l.id.clone()];
            let mut btn = |icon: &str, tip: &str, act: &'static str, ui: &mut egui::Ui| {
                let r = Rect::from_min_size(Pos2::new(x, y - 15.0), Vec2::splat(30.0));
                let b = ui.interact(r, id.with(act), Sense::click()).on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand);
                if b.hovered() {
                    ui.painter().rect_filled(r, 8.0, alpha(p.text, 0.08));
                }
                ui.painter().text(r.center(), Align2::CENTER_CENTER, icon, f(16.0), alpha(if b.hovered() { p.text } else { p.dim }, (h - 0.5) * 2.0));
                if b.clicked() {
                    acts.push(Act::Links(act, ids.clone()));
                }
                x -= 32.0;
            };
            btn(I::X, "Remove", "remove", ui);
            match l.status {
                Status::Finished => {
                    btn(I::FOLDER_OPEN, "Show in folder", "open", ui);
                    btn(I::ARROW_SQUARE_OUT, "Open file", "openfile", ui);
                }
                Status::Downloading | Status::Waiting | Status::Queued | Status::Captcha => btn(I::PAUSE, "Pause", "pause", ui),
                _ if l.stage == Stage::Inbox => btn(I::PLAY, "Start", "confirm", ui),
                _ => btn(I::ARROW_CLOCKWISE, "Retry / resume", "resume", ui),
            }
        }
        let resp = resp.on_hover_text_at_pointer(match &l.error {
            Some(e) => format!("{}\n\n{}{}", l.url, e.message, e.hint.as_ref().map(|h| format!("\n→ {h}")).unwrap_or_default()),
            None => l.url.clone(),
        });
        if resp.clicked() {
            acts.push(Act::Click(l.id.clone(), ui.input(|i| i.modifiers)));
        }
        if resp.double_clicked() {
            acts.push(Act::Links(if l.status == Status::Finished { "openfile" } else if l.stage == Stage::Inbox { "confirm" } else { "force" }, vec![l.id.clone()]));
        }
        let ids = if selected && self.sel.len() > 1 { self.sel.iter().cloned().collect() } else { vec![l.id.clone()] };
        let n = ids.len();
        let stage = l.stage;
        let enabled = l.enabled;
        resp.context_menu(|ui| {
            ui.set_min_width(230.0);
            if n > 1 {
                ui.label(egui::RichText::new(format!("{n} selected")).font(fm(12.0)).color(p.dim));
                ui.separator();
            }
            let mut m = |icon: &str, t: &str, a: &'static str, ui: &mut egui::Ui| {
                if menu_item(ui, icon, t, p).clicked() {
                    acts.push(Act::Links(a, ids.clone()));
                    ui.close_menu();
                }
            };
            if stage == Stage::Inbox {
                m(I::PLAY, "Start download", "confirm", ui);
            }
            m(I::LIGHTNING, "Start now (skip queue)", "force", ui);
            m(I::PAUSE, "Pause", "pause", ui);
            m(I::ARROW_CLOCKWISE, "Retry / resume", "resume", ui);
            ui.separator();
            m(I::ARROW_UP, "Higher priority", "up", ui);
            m(I::ARROW_DOWN, "Lower priority", "down", ui);
            m(I::MAGNIFYING_GLASS, "Check availability", "check", ui);
            m(if enabled { I::PROHIBIT } else { I::CHECK_CIRCLE }, if enabled { "Disable" } else { "Enable" }, "toggle", ui);
            ui.separator();
            m(I::FOLDER_OPEN, "Show in folder", "open", ui);
            m(I::COPY, "Copy URL", "copy", ui);
            m(I::ARROW_COUNTER_CLOCKWISE, "Reset (delete progress)", "reset", ui);
            ui.separator();
            m(I::X, "Remove from list", "remove", ui);
            m(I::TRASH, "Remove and delete file", "removefiles", ui);
        });
    }

    fn selection_bar(&mut self, ctx: &egui::Context) {
        let p = self.pal.clone();
        let n = self.sel.len();
        let t = ctx.animate_bool_with_time(egui::Id::new("selbar"), n > 1, 0.2);
        if t <= 0.0 {
            return;
        }
        let ease = 1.0 - (1.0 - t).powi(3);
        let central = ctx.available_rect();
        let pos = Pos2::new(central.center().x - 330.0, central.bottom() - 70.0 + (1.0 - ease) * 40.0);
        let mut act: Option<&'static str> = None;
        egui::Area::new(egui::Id::new("selbar-area")).fixed_pos(pos).order(egui::Order::Foreground).show(ctx, |ui| {
            ui.multiply_opacity(ease);
            egui::Frame::new().fill(p.elevated).stroke(Stroke::new(1.0, p.line)).corner_radius(16).inner_margin(egui::Margin::symmetric(10, 8))
                .shadow(egui::epaint::Shadow { offset: [0, 12], blur: 34, spread: 0, color: Color32::from_black_alpha(120) })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(30.0, 30.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 14.0, p.acc);
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, n.to_string(), fs(12.5), Color32::WHITE);
                        ui.label(egui::RichText::new("selected").font(fm(13.0)).color(p.dim));
                        ui.add_space(6.0);
                        if self.tab == 0 && widgets::button(ui, Some(I::PLAY), "Start", Kind::Primary, &p).clicked() {
                            act = Some("confirm");
                        }
                        for (icon, label, a) in [(I::LIGHTNING, "Now", "force"), (I::PAUSE, "Pause", "pause"), (I::ARROW_CLOCKWISE, "Retry", "resume"), (I::COPY, "Copy", "copy")] {
                            if widgets::button(ui, Some(icon), label, Kind::Ghost, &p).clicked() {
                                act = Some(a);
                            }
                        }
                        if widgets::button(ui, Some(I::TRASH), "Remove", Kind::Danger, &p).clicked() {
                            act = Some("remove");
                        }
                        if widgets::icon_button(ui, I::X, "Clear selection (Esc)", &p).clicked() {
                            act = Some("clear");
                        }
                    });
                });
        });
        match act {
            Some("clear") => self.sel.clear(),
            Some(a) => {
                let ids = self.sel_vec();
                self.link_action(a, ids, ctx);
            }
            None => {}
        }
    }

    pub(super) fn detail_panel(&mut self, ctx: &egui::Context, t: f32) {
        let p = self.pal.clone();
        let l = self.sel.iter().next().and_then(|id| self.links.iter().find(|l| &l.id == id)).cloned();
        let w = 370.0 * (1.0 - (1.0 - t).powi(3));
        egui::SidePanel::right("detail").exact_width(w.max(1.0)).resizable(false).show_separator_line(false)
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(egui::Margin { left: 20, right: 20, top: 22, bottom: 14 }).stroke(Stroke::new(1.0, p.line_soft)))
            .show(ctx, |ui| {
                ui.multiply_opacity(t);
                let Some(l) = l else { return };
                egui::TopBottomPanel::bottom(egui::Id::new("detail-actions")).show_separator_line(false)
                    .frame(egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 12, bottom: 4 }))
                    .show_inside(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            let ids = vec![l.id.clone()];
                            match l.status {
                                Status::Finished => {
                                    if widgets::button(ui, Some(I::ARROW_SQUARE_OUT), "Open", Kind::Primary, &p).clicked() { self.link_action("openfile", ids.clone(), ctx); }
                                    if widgets::button(ui, Some(I::FOLDER_OPEN), "Show in folder", Kind::Secondary, &p).clicked() { self.link_action("open", ids.clone(), ctx); }
                                }
                                Status::Downloading | Status::Waiting | Status::Queued | Status::Captcha => {
                                    if widgets::button(ui, Some(I::PAUSE), "Pause", Kind::Secondary, &p).clicked() { self.link_action("pause", ids.clone(), ctx); }
                                }
                                _ => {
                                    if widgets::button(ui, Some(I::PLAY), if l.stage == Stage::Inbox { "Start" } else { "Retry" }, Kind::Primary, &p).clicked() { self.link_action("resume", ids.clone(), ctx); }
                                }
                            }
                            if widgets::button(ui, Some(I::TRASH), "Remove", Kind::Danger, &p).clicked() { self.link_action("remove", ids.clone(), ctx); }
                        });
                    });
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // header
                    ui.horizontal(|ui| {
                        let (ficon, fcol) = widgets::file_icon(&l.name, &p);
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
                        ui.painter().rect_filled(r, 12.0, alpha(fcol, 0.15));
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, ficon, fi(23.0), fcol);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if widgets::icon_button(ui, I::X, "Close (Esc)", &p).clicked() {
                                self.sel.clear();
                            }
                        });
                    });
                    ui.add_space(10.0);
                    ui.add(egui::Label::new(egui::RichText::new(&l.name).font(fs(16.0)).color(p.text)).wrap());
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let (c, s) = widgets::status_style(l.status, &p);
                        widgets::pill_ui(ui, s, c);
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                        widgets::host_badge(ui.painter(), r, &l.host);
                        ui.label(egui::RichText::new(&l.host).font(f(12.5)).color(p.dim));
                    });
                    ui.add_space(18.0);
                    // ring
                    ui.vertical_centered(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(150.0, 150.0), Sense::hover());
                        let v = ui.ctx().animate_value_with_time(egui::Id::new(("dring", &l.id)), l.progress(), 0.6);
                        let col = match l.status { Status::Finished => Some(p.ok), Status::Failed | Status::Offline => Some(p.err), _ => None };
                        widgets::ring(ui.painter(), r.center(), 64.0, 8.0, v, &p, col);
                        ui.painter().text(r.center() - Vec2::new(0.0, 8.0), Align2::CENTER_CENTER, format!("{:.0}%", v * 100.0), fs(28.0), p.text);
                        let sub = if l.speed > 0 { format!("{}/s", fmt_bytes(l.speed as i64)) } else { fmt_bytes(l.size) };
                        ui.painter().text(r.center() + Vec2::new(0.0, 20.0), Align2::CENTER_CENTER, sub, f(12.5), if l.speed > 0 { p.acc } else { p.dim });
                    });
                    ui.add_space(16.0);
                    // stat tiles
                    let eta = if l.speed > 0 && l.size > 0 { fmt_duration(((l.size as u64).saturating_sub(l.bytes_done) / l.speed) as i64) } else { "–".into() };
                    let tiles = [
                        (I::DOWNLOAD_SIMPLE, "Downloaded", fmt_bytes(l.bytes_done as i64)),
                        (I::HARD_DRIVE, "Size", fmt_bytes(l.size)),
                        (I::TIMER, "Time left", eta),
                        (I::GIT_FORK, "Connections", if l.status == Status::Downloading { l.segments.iter().filter(|s| !s.done()).count().to_string() } else { "–".into() }),
                    ];
                    let gap = 8.0;
                    let tw = ((ui.available_width() - gap) / 2.0).floor();
                    let th = 58.0;
                    let (area, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), th * 2.0 + gap), Sense::hover());
                    for (i, (icon, k, v)) in tiles.iter().enumerate() {
                        let x = area.left() + (i % 2) as f32 * (tw + gap);
                        let y = area.top() + (i / 2) as f32 * (th + gap);
                        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(tw, th));
                        let painter = ui.painter();
                        painter.rect(r, 12.0, p.surface, Stroke::new(1.0, p.line_soft), egui::StrokeKind::Inside);
                        painter.text(Pos2::new(r.left() + 12.0, r.top() + 17.0), Align2::LEFT_CENTER, *icon, f(12.5), p.faint);
                        let kg = widgets::galley(ui, k, f(11.5), p.dim, tw - 44.0);
                        painter.galley(Pos2::new(r.left() + 30.0, r.top() + 17.0 - kg.size().y / 2.0), kg, p.dim);
                        let vg = widgets::galley(ui, v, fs(15.5), p.text, tw - 24.0);
                        painter.galley(Pos2::new(r.left() + 12.0, r.bottom() - 12.0 - vg.size().y), vg, p.text);
                    }
                    // segment map
                    if !l.segments.is_empty() && l.size > 0 {
                        ui.add_space(14.0);
                        ui.label(egui::RichText::new("Connections map").font(fm(12.0)).color(p.dim));
                        ui.add_space(4.0);
                        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
                        let painter = ui.painter();
                        painter.rect_filled(r, 4.0, alpha(p.text, 0.06));
                        let total = l.size as f32;
                        let now = ui.input(|i| i.time) as f32;
                        for s in &l.segments {
                            let x0 = r.left() + r.width() * s.start as f32 / total;
                            let x1 = r.left() + r.width() * s.pos as f32 / total;
                            let xe = r.left() + r.width() * s.end.map(|e| e + 1).unwrap_or(l.size as u64) as f32 / total;
                            let fill = Rect::from_x_y_ranges(x0..=x1.max(x0 + 1.0), r.y_range());
                            painter.rect_filled(fill, 2.0, mix(p.acc, p.acc2, (x0 - r.left()) / r.width()));
                            if !s.done() && l.status == Status::Downloading {
                                let pulse = (now * 5.0 + x0 * 0.05).sin() * 0.5 + 0.5;
                                painter.vline(x1, r.y_range(), Stroke::new(2.0, alpha(Color32::WHITE, 0.4 + 0.5 * pulse)));
                            }
                            painter.vline(xe, r.y_range(), Stroke::new(1.0, p.sidebar));
                        }
                    }
                    // error card
                    if let Some(e) = &l.error {
                        ui.add_space(14.0);
                        egui::Frame::new().fill(alpha(p.err, 0.08)).stroke(Stroke::new(1.0, alpha(p.err, 0.3))).corner_radius(12).inner_margin(12).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let (title, technical) = humanize_error(&e.message);
                            ui.label(egui::RichText::new(format!("{}  {}", I::WARNING_CIRCLE, title)).font(fs(13.0)).color(p.err));
                            if let Some(tech) = technical {
                                let did = egui::Id::new(("err-details", &l.id));
                                let open = ui.memory(|m| m.data.get_temp::<bool>(did).unwrap_or(false));
                                let r = ui.add(egui::Label::new(egui::RichText::new(format!("{} Technical details", if open { I::CARET_DOWN } else { I::CARET_RIGHT })).font(f(11.5)).color(p.dim)).sense(Sense::click())).on_hover_cursor(CursorIcon::PointingHand);
                                if r.clicked() {
                                    ui.memory_mut(|m| m.data.insert_temp(did, !open));
                                }
                                if open {
                                    ui.add(egui::Label::new(egui::RichText::new(tech).font(egui::FontId::monospace(11.0)).color(p.dim)).wrap());
                                }
                            }
                            if let Some(h) = &e.hint {
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(h).font(f(12.5)).color(p.text));
                            }
                            if e.code == "PREMIUM_ONLY" || e.code == "QUOTA" {
                                ui.add_space(6.0);
                                if widgets::button(ui, Some(I::KEY), "Add an account", Kind::Secondary, &p).clicked() {
                                    self.view = View::Accounts;
                                }
                            }
                        });
                    }
                    ui.add_space(14.0);
                    // info rows
                    let mut info = vec![("Source", l.url.clone(), true)];
                    if let Some(fp) = l.prop_str("finalPath") {
                        info.push(("Saved to", fp.to_string(), true));
                    }
                    if let Some(h) = &l.hash {
                        info.push(("Checksum", format!("{:?} {}", h.algo, match h.verified { Some(true) => "verified ✓", Some(false) => "mismatch", None => "will be verified" }), false));
                    }
                    info.push(("Plugin", l.plugin_id.clone(), false));
                    if let Some(o) = &l.origin {
                        info.push(("Found on", o.clone(), true));
                    }
                    for (k, v, copy) in info {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(k).font(f(12.0)).color(p.faint));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if copy && widgets::icon_button(ui, I::COPY, "Copy", &p).clicked() {
                                    ui.ctx().copy_text(v.clone());
                                    self.core.notify("success", "Copied", "", None);
                                }
                            });
                        });
                        ui.add(egui::Label::new(egui::RichText::new(&v).font(f(12.5)).color(p.text)).truncate()).on_hover_text(&v);
                        ui.add_space(6.0);
                    }
                });
            });
    }
}

fn menu_item(ui: &mut egui::Ui, icon: &str, text: &str, p: &super::theme::Palette) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(210.0), 30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 7.0, p.acc_soft);
    }
    let danger = text.contains("delete") || text.starts_with("Remove");
    let col = if danger { p.err } else { p.text };
    ui.painter().text(Pos2::new(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, icon, f(15.0), if danger { p.err } else { p.dim });
    ui.painter().text(Pos2::new(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, text, f(13.0), col);
    resp
}

fn pkg_menu(ui: &mut egui::Ui, pid: &str, p: &super::theme::Palette, acts: &mut Vec<Act>) {
    ui.set_min_width(230.0);
    for (icon, t, a) in [
        (I::PLAY, "Start / resume package", "start"),
        (I::PAUSE, "Pause package", "pause"),
        (I::CHECKS, "Select all files", "select"),
        (I::FILE_ZIP, "Extract now", "extract"),
        (I::LOCK_KEY, "Archive passwords…", "passwords"),
        (I::PENCIL_SIMPLE, "Rename…", "rename"),
        (I::FOLDER_OPEN, "Open folder", "open"),
        (I::ARROW_UP, "Higher priority", "up"),
        (I::ARROW_DOWN, "Lower priority", "down"),
        (I::X, "Remove package", "remove"),
        (I::TRASH, "Remove package and delete files", "removefiles"),
    ] {
        if matches!(a, "up" | "remove") {
            ui.separator();
        }
        if menu_item(ui, icon, t, p).clicked() {
            acts.push(Act::Pkg(a, pid.to_string()));
            ui.close_menu();
        }
    }
}

/// Turns raw transport errors into a short human sentence plus optional technical details.
fn humanize_error(msg: &str) -> (String, Option<String>) {
    let m = msg.to_lowercase();
    let human = if m.contains("10060") || m.contains("timed out") || m.contains("timeout") {
        Some("Couldn't reach the server - the connection timed out")
    } else if m.contains("10061") || m.contains("connection refused") {
        Some("The server refused the connection")
    } else if m.contains("10054") || m.contains("connection reset") || m.contains("connection closed") {
        Some("The server dropped the connection")
    } else if m.contains("dns error") || m.contains("failed to lookup") || m.contains("no such host") || m.contains("11001") {
        Some("Couldn't find the server (DNS lookup failed)")
    } else if m.contains("certificate") || m.contains("tls") {
        Some("Secure connection failed (TLS/certificate problem)")
    } else if m.contains("tcp connect error") || m.contains("error sending request") {
        Some("Couldn't connect to the server")
    } else {
        None
    };
    match human {
        Some(h) => (h.to_string(), Some(msg.to_string())),
        None if msg.chars().count() > 160 => (format!("{}…", msg.chars().take(160).collect::<String>().trim_end()), Some(msg.to_string())),
        None => (msg.to_string(), None),
    }
}
