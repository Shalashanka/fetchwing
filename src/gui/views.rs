//! Secondary views: captchas, accounts, plugins, settings, activity log, about.

use super::theme::{alpha, f, fm, fs, mix};
use super::widgets::{self, Kind};
use super::{paint_logo, App};
use crate::captcha::CaptchaRequest;
use crate::model::AccountStatus;
use crate::settings::{schema, Kind as SKind, SettingDef};
use crate::util::fmt_duration;
use eframe::egui::{self, Align2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use egui_phosphor::regular as I;

const DEBRID: [(&str, &str, &str); 4] = [
    ("real-debrid.com", "Real-Debrid", "https://real-debrid.com/apitoken"),
    ("alldebrid.com", "AllDebrid", "https://alldebrid.com/apikeys/"),
    ("premiumize.me", "Premiumize", "https://www.premiumize.me/account"),
    ("torbox.app", "TorBox", "https://torbox.app/settings"),
];

impl App {
    // ------------------------------------------------------------------ captchas
    pub(super) fn captchas(&mut self, ui: &mut egui::Ui) {
        let p = self.pal.clone();
        widgets::view_title(ui, I::SHIELD_CHECK, "Captchas", "They never pop up over your games or videos - they wait here. A solver service (Settings → Captcha) can handle them automatically.", &p);
        let jobs = self.core.captcha.list();
        if jobs.is_empty() {
            widgets::empty_state(ui, I::SHIELD_CHECK, "All clear", "No captcha needs your attention right now.", &p);
            return;
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (n, j) in jobs.iter().enumerate() {
                widgets::card(ui, &p, |ui| {
                    let total = (j.expires_at - j.created_at).max(1) as f32;
                    let left = (j.expires_at - crate::model::now_ms()).max(0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                        widgets::ring(ui.painter(), r.center(), 16.0, 3.0, left as f32 / total, &p, Some(if left < 30_000 { p.err } else { p.warn }));
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, (left / 1000).to_string(), fs(10.5), p.text);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&j.host).font(fs(15.0)));
                            ui.label(egui::RichText::new(format!("{} · expires in {}", j.request.describe(), fmt_duration(left / 1000))).font(f(12.0)).color(p.dim));
                        });
                    });
                    ui.add_space(10.0);
                    let input = self.cap_inputs.entry(j.id.clone()).or_default();
                    match &j.request {
                        CaptchaRequest::Image { .. } => {
                            if let Some(bytes) = j.request.image_bytes() {
                                egui::Frame::new().fill(Color32::WHITE).corner_radius(10).inner_margin(8).show(ui, |ui| {
                                    ui.add(egui::Image::from_bytes(format!("bytes://captcha-{}.png", j.id), bytes).fit_to_original_size(2.5).max_height(170.0).texture_options(egui::TextureOptions::NEAREST));
                                });
                            }
                        }
                        CaptchaRequest::Recaptcha2 { page_url, .. } | CaptchaRequest::Recaptcha3 { page_url, .. } | CaptchaRequest::Hcaptcha { page_url, .. } | CaptchaRequest::Turnstile { page_url, .. } => {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(page_url).font(f(12.5)).color(p.dim));
                                if widgets::button(ui, Some(I::ARROW_SQUARE_OUT), "Open page", Kind::Secondary, &p).clicked() {
                                    let _ = open::that_detached(page_url);
                                }
                            });
                        }
                    }
                    ui.add_space(10.0);
                    let mut submit = false;
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(input).hint_text("Type the answer (or paste a token)").desired_width(300.0).font(fm(15.0)).margin(Vec2::new(10.0, 8.0)));
                        if n == 0 && !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            submit = true;
                        }
                        if widgets::button(ui, Some(I::CHECK), "Submit", Kind::Primary, &p).clicked() {
                            submit = true;
                        }
                        if widgets::button(ui, None, "Skip", Kind::Ghost, &p).clicked() {
                            self.core.captcha.skip(&j.id);
                        }
                    });
                    if submit && !input.trim().is_empty() {
                        self.core.captcha.submit(&j.id, input.trim());
                    }
                });
            }
        });
    }

    // ------------------------------------------------------------------ accounts
    pub(super) fn accounts(&mut self, ui: &mut egui::Ui) {
        let p = self.pal.clone();
        widgets::view_title(ui, I::KEY, "Accounts", "Premium accounts are used automatically. A debrid service unlocks hundreds of hosters at once.", &p);
        let accounts = self.core.accounts();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.label(egui::RichText::new("DEBRID SERVICES").font(fm(11.0)).color(p.faint));
            ui.add_space(6.0);
            ui.columns(4, |cols| {
                for (i, (host, name, url)) in DEBRID.iter().enumerate() {
                    let ui = &mut cols[i];
                    let have = accounts.iter().find(|a| &a.host == host);
                    egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, if have.is_some() { alpha(p.ok, 0.5) } else { p.line_soft })).corner_radius(14).inner_margin(14).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                            widgets::host_badge(ui.painter(), r, name);
                            ui.label(egui::RichText::new(*name).font(fs(14.0)));
                        });
                        ui.add_space(6.0);
                        match have {
                            Some(a) => {
                                let (c, t) = acc_status(a.status.clone(), &p);
                                widgets::pill_ui(ui, t, c);
                                ui.label(egui::RichText::new(if a.supported_hosts.is_empty() { "checking…".into() } else { format!("unlocks {} hosts", a.supported_hosts.len()) }).font(f(12.0)).color(p.dim));
                            }
                            None => {
                                if ui.link(egui::RichText::new(format!("Get API token {}", I::ARROW_SQUARE_OUT)).font(f(12.0))).clicked() {
                                    let _ = open::that_detached(url);
                                }
                                ui.add_space(4.0);
                                if widgets::button(ui, Some(I::PLUS), "Connect", Kind::Secondary, &p).clicked() {
                                    self.acc_host = host.to_string();
                                    self.acc_user.clear();
                                    ui.memory_mut(|m| m.request_focus(egui::Id::new("acc-pw")));
                                }
                            }
                        }
                    });
                }
            });
            ui.add_space(16.0);
            widgets::card(ui, &p, |ui| {
                ui.label(egui::RichText::new("Add an account").font(fs(14.0)));
                ui.label(egui::RichText::new("For debrid services put the API token in the password field (user can stay empty).").font(f(12.0)).color(p.dim));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.acc_host).hint_text("host, e.g. real-debrid.com").desired_width(210.0).margin(Vec2::new(10.0, 7.0)));
                    ui.add(egui::TextEdit::singleline(&mut self.acc_user).hint_text("user").desired_width(170.0).margin(Vec2::new(10.0, 7.0)));
                    ui.add(egui::TextEdit::singleline(&mut self.acc_pw).id(egui::Id::new("acc-pw")).password(true).hint_text("password / API token").desired_width(230.0).margin(Vec2::new(10.0, 7.0)));
                    let ok = !self.acc_host.trim().is_empty() && !self.acc_pw.is_empty();
                    if ui.add_enabled_ui(ok, |ui| widgets::button(ui, Some(I::PLUS), "Add", Kind::Primary, &p)).inner.clicked() {
                        self.core.add_account(&self.acc_host, &self.acc_user, &self.acc_pw);
                        self.acc_pw.clear();
                        self.acc_user.clear();
                        self.acc_host.clear();
                    }
                });
            });
            if accounts.is_empty() {
                return;
            }
            ui.label(egui::RichText::new("YOUR ACCOUNTS").font(fm(11.0)).color(p.faint));
            ui.add_space(6.0);
            for a in accounts {
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.line_soft)).corner_radius(12).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
                        widgets::host_badge(ui.painter(), r, &a.host);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&a.host).font(fs(14.0)));
                                let (c, t) = acc_status(a.status.clone(), &p);
                                widgets::pill_ui(ui, t, c);
                                if a.premium == Some(true) {
                                    widgets::pill_ui(ui, "premium", p.acc2);
                                }
                            });
                            let mut s = if a.user.is_empty() { "token".to_string() } else { a.user.clone() };
                            if let Some(v) = a.valid_until {
                                s += &format!(" · until {}", chrono::DateTime::from_timestamp_millis(v).map(|d| d.format("%d %b %Y").to_string()).unwrap_or_default());
                            }
                            if !a.supported_hosts.is_empty() {
                                s += &format!(" · unlocks {} hosts", a.supported_hosts.len());
                            }
                            if let Some(m) = &a.message {
                                s += &format!(" · {m}");
                            }
                            ui.label(egui::RichText::new(s).font(f(12.0)).color(p.dim));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::icon_button(ui, I::TRASH, "Remove account", &p).clicked() {
                                self.core.remove_account(&a.id);
                            }
                            if widgets::icon_button(ui, I::ARROWS_CLOCKWISE, "Check now", &p).clicked() {
                                let c = self.core.clone();
                                let id = a.id.clone();
                                self.rt.spawn(async move { c.check_account(&id).await });
                            }
                            let mut on = a.enabled;
                            if widgets::switch(ui, &mut on, &p).on_hover_text("Use this account").changed() {
                                let id = a.id.clone();
                                self.core.update_account(&id, |x| x.enabled = on);
                            }
                        });
                    });
                });
                ui.add_space(8.0);
            }
        });
    }

    // ------------------------------------------------------------------ plugins
    pub(super) fn plugins(&mut self, ui: &mut egui::Ui) {
        let p = self.pal.clone();
        let reg = self.core.registry.read().clone();
        let (x, y, k) = crate::plugins::templates::stats();
        widgets::view_title(ui, I::PUZZLE_PIECE, "Plugins", "Hosters, folder crawlers, debrid services and template sites.", &p);
        let all = reg.all();
        let hosts = all.iter().filter(|p| p.meta().kind == crate::plugins::Kind::Host).count();
        ui.columns(4, |c| {
            for (i, (n, label, icon)) in [(all.len(), "plugins", I::PUZZLE_PIECE), (hosts, "hosters", I::CLOUD_ARROW_DOWN), (all.len() - hosts, "crawlers", I::TREE_STRUCTURE), (x + y + k, "template sites", I::STACK)].iter().enumerate() {
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.line_soft)).corner_radius(14).inner_margin(14).show(&mut c[i], |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new(*icon).font(f(18.0)).color(p.acc));
                    ui.label(egui::RichText::new(n.to_string()).font(fs(22.0)));
                    ui.label(egui::RichText::new(*label).font(f(12.0)).color(p.dim));
                });
            }
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            widgets::search(ui, "pl-search", &mut self.plugin_filter, "Search plugins or domains…", 320.0, &p);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, Some(I::FOLDER_OPEN), "Plugin folder", Kind::Secondary, &p).on_hover_text("Drop JSON site definitions here (see docs/plugin-authoring.md)").clicked() {
                    let d = self.core.data_dir.join("plugins");
                    let _ = std::fs::create_dir_all(&d);
                    let _ = open::that_detached(d);
                }
                if widgets::button(ui, Some(I::ARROWS_CLOCKWISE), "Reload", Kind::Ghost, &p).clicked() {
                    self.core.reload_plugins();
                }
            });
        });
        for e in &reg.user_errors {
            ui.label(egui::RichText::new(format!("{}  {e}", I::WARNING)).color(p.err));
        }
        ui.add_space(8.0);
        let q = self.plugin_filter.to_lowercase();
        let list: Vec<_> = all.iter().filter(|x| q.is_empty() || x.meta().id.contains(&q) || x.meta().name.to_lowercase().contains(&q) || x.meta().domains.iter().any(|d| d.contains(&q))).collect();
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, 40.0, list.len(), |ui, range| {
            for pl in &list[range] {
                let m = pl.meta();
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::hover());
                let painter = ui.painter();
                if resp.hovered() {
                    painter.rect_filled(r, 8.0, alpha(p.text, 0.04));
                }
                let host = m.domains.first().cloned().unwrap_or(m.name.clone());
                widgets::host_badge(painter, Rect::from_min_size(Pos2::new(r.left() + 8.0, r.center().y - 11.0), Vec2::splat(22.0)), &host);
                widgets::text_at(ui, painter, r.left() + 40.0, r.center().y, &m.name, fm(13.5), p.text, 230.0);
                let (kc, kt) = if pl.is_multihost() { (p.acc2, "debrid") } else if m.kind == crate::plugins::Kind::Crawler { (p.ok, "crawler") } else { (p.acc, "hoster") };
                widgets::pill(ui, painter, r.left() + 290.0, r.center().y, kt, kc);
                widgets::pill(ui, painter, r.left() + 370.0, r.center().y, m.source, if m.source == "user" { p.warn } else { p.dim });
                widgets::text_at(ui, painter, r.left() + 450.0, r.center().y, &m.domains.iter().take(6).cloned().collect::<Vec<_>>().join(", "), f(12.0), p.dim, r.width() - 560.0);
                widgets::text_right(ui, painter, r.right() - 8.0, r.center().y, &m.version, f(11.5), p.faint);
            }
        });
    }

    // ------------------------------------------------------------------ settings
    pub(super) fn settings_view(&mut self, ui: &mut egui::Ui) {
        let p = self.pal.clone();
        widgets::view_title(ui, I::GEAR_SIX, "Settings", "Changes apply immediately.", &p);
        let mut groups: Vec<&str> = vec![];
        for d in schema() {
            if !groups.contains(&d.group) {
                groups.push(d.group);
            }
        }
        ui.horizontal_top(|ui| {
            // sub-nav
            ui.allocate_ui(Vec2::new(190.0, ui.available_height()), |ui| {
                ui.vertical(|ui| {
                    ui.set_width(190.0);
                    widgets::search(ui, "set-search", &mut self.settings_filter, "Search settings…", 186.0, &p);
                    ui.add_space(10.0);
                    for g in &groups {
                        let icon = match *g {
                            "General" => I::HOUSE,
                            "Downloads" => I::DOWNLOAD_SIMPLE,
                            "Connection" => I::PLUGS_CONNECTED,
                            "Link handling" => I::LINK,
                            "Captcha" => I::SHIELD_CHECK,
                            "Archives" => I::FILE_ZIP,
                            "Interface" => I::PALETTE,
                            "Remote access" => I::GLOBE,
                            _ => I::CALENDAR,
                        };
                        let on = self.settings_filter.is_empty() && self.settings_group == *g;
                        if sub_nav(ui, icon, g, on, &p).clicked() {
                            self.settings_group = g.to_string();
                            self.settings_filter.clear();
                        }
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        widgets::switch(ui, &mut self.show_advanced, &p);
                        ui.label(egui::RichText::new("Advanced").font(f(12.5)).color(p.dim));
                    });
                });
            });
            ui.add_space(18.0);
            // content
            let q = self.settings_filter.to_lowercase();
            let defs: Vec<&SettingDef> = schema().iter().filter(|d| {
                if !q.is_empty() {
                    format!("{} {} {} {}", d.label, d.help, d.key, d.group).to_lowercase().contains(&q)
                } else {
                    d.group == self.settings_group && (!d.advanced || self.show_advanced)
                }
            }).collect();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 12.0).min(820.0));
                if q.is_empty() {
                    ui.label(egui::RichText::new(&self.settings_group).font(fs(17.0)));
                    ui.add_space(8.0);
                } else {
                    ui.label(egui::RichText::new(format!("{} result{} for \"{}\"", defs.len(), if defs.len() == 1 { "" } else { "s" }, self.settings_filter)).font(f(13.0)).color(p.dim));
                    ui.add_space(8.0);
                }
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.line_soft)).corner_radius(14).inner_margin(egui::Margin::symmetric(18, 6)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for (i, d) in defs.iter().enumerate() {
                        if i > 0 {
                            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
                            ui.painter().rect_filled(r, 0.0, p.line_soft);
                        }
                        ui.add_space(12.0);
                        let full = ui.available_width();
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width((full - 380.0).max(220.0));
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(d.label).font(fm(13.5)));
                                    if d.advanced {
                                        widgets::pill_ui(ui, "advanced", p.faint);
                                    }
                                });
                                if !d.help.is_empty() {
                                    ui.add(egui::Label::new(egui::RichText::new(d.help).font(f(12.0)).color(p.dim)).wrap());
                                }
                                if !q.is_empty() {
                                    ui.label(egui::RichText::new(d.group).font(f(11.0)).color(p.faint));
                                }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.setting_widget(ui, d));
                        });
                        ui.add_space(12.0);
                    }
                });
                ui.add_space(24.0);
            }));
        });
    }

    fn set(&mut self, key: &str, v: serde_json::Value) {
        match self.core.settings.set(key, v) {
            Ok(_) => self.core.settings_changed(key),
            Err(e) => self.core.notify("error", "Invalid value", &e, None),
        }
    }

    fn setting_widget(&mut self, ui: &mut egui::Ui, d: &SettingDef) {
        let p = self.pal.clone();
        let s = self.core.settings.clone();
        match d.kind {
            SKind::Bool => {
                let mut b = s.bool(d.key);
                if widgets::switch(ui, &mut b, &p).changed() {
                    self.set(d.key, serde_json::json!(b));
                }
            }
            SKind::Enum => {
                let cur = s.str(d.key);
                let label = d.options.iter().find(|o| o.0 == cur).map(|o| o.1).unwrap_or("?");
                egui::ComboBox::from_id_salt(d.key).selected_text(label).width(260.0).show_ui(ui, |ui| {
                    for (k, t) in d.options {
                        if ui.selectable_label(cur == *k, *t).clicked() {
                            self.set(d.key, serde_json::json!(k));
                        }
                    }
                });
            }
            SKind::Number if d.max.is_some_and(|m| m <= 100.0) => {
                let mut v = s.num(d.key) as f32;
                let r = ui.add(egui::Slider::new(&mut v, d.min.unwrap_or(0.0) as f32..=d.max.unwrap_or(100.0) as f32).step_by(1.0).suffix(if d.unit.is_empty() { String::new() } else { format!(" {}", d.unit) }));
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    self.set(d.key, serde_json::json!(v));
                }
            }
            SKind::List => {
                let e = self.edits.entry(d.key.to_string()).or_insert_with(|| s.list(d.key).join("\n"));
                let r = ui.add(egui::TextEdit::multiline(e).desired_rows(3).desired_width(320.0).hint_text("one per line"));
                if r.lost_focus() {
                    let v = serde_json::json!(e.clone());
                    self.set(d.key, v);
                }
            }
            _ => {
                let cur = if d.kind == SKind::Number { fmt_num(s.num(d.key), d.unit) } else { s.str(d.key) };
                let reveal_id = egui::Id::new(("reveal", d.key));
                let reveal = ui.memory(|m| m.data.get_temp::<bool>(reveal_id).unwrap_or(false));
                if d.kind == SKind::Path && widgets::button(ui, Some(I::FOLDER_OPEN), "Browse", Kind::Secondary, &p).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_directory(&cur).pick_folder() {
                        self.set(d.key, serde_json::json!(dir.to_string_lossy()));
                        self.edits.remove(d.key);
                    }
                }
                if d.kind == SKind::Secret {
                    if widgets::icon_button(ui, I::COPY, "Copy", &p).clicked() {
                        ui.ctx().copy_text(s.str(d.key));
                    }
                    if widgets::icon_button(ui, if reveal { I::EYE_SLASH } else { I::EYE }, "Show / hide", &p).clicked() {
                        ui.memory_mut(|m| m.data.insert_temp(reveal_id, !reveal));
                    }
                }
                let e = self.edits.entry(d.key.to_string()).or_insert(cur.clone());
                let r = ui.add(egui::TextEdit::singleline(e).password(d.kind == SKind::Secret && !reveal).desired_width(if d.kind == SKind::Number { 160.0 } else { 300.0 }).margin(Vec2::new(10.0, 7.0)));
                if r.lost_focus() && *e != cur {
                    let raw = e.trim().to_string();
                    let v = if d.kind == SKind::Number {
                        let n = if d.unit.contains("bytes") { crate::util::parse_bytes(&raw.replace("/s", "")).map(|b| b as f64) } else { raw.parse::<f64>().ok() };
                        serde_json::json!(n.unwrap_or(f64::NAN).to_string())
                    } else {
                        serde_json::json!(raw)
                    };
                    self.set(d.key, v);
                    let fresh = if d.kind == SKind::Number { fmt_num(self.core.settings.num(d.key), d.unit) } else { self.core.settings.str(d.key) };
                    self.edits.insert(d.key.to_string(), fresh);
                }
            }
        }
    }

    // ------------------------------------------------------------------ log
    pub(super) fn log_view(&mut self, ui: &mut egui::Ui) {
        let p = self.pal.clone();
        widgets::view_title(ui, I::LIST_BULLETS, "Activity log", &format!("Full log: {}", self.core.data_dir.join("logs").join("fetchwing.log").display()), &p);
        ui.horizontal(|ui| {
            for (i, (l, c)) in [("All", p.dim), ("Warnings", p.warn), ("Errors", p.err)].iter().enumerate() {
                if widgets::chip(ui, l, self.log_level == i, *c, &p).clicked() {
                    self.log_level = i;
                }
            }
            ui.add_space(8.0);
            widgets::search(ui, "log-search", &mut self.log_filter, "Filter…", 260.0, &p);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, Some(I::FOLDER_OPEN), "Open log folder", Kind::Ghost, &p).clicked() {
                    let _ = open::that_detached(self.core.data_dir.join("logs"));
                }
            });
        });
        ui.add_space(10.0);
        let q = self.log_filter.to_lowercase();
        let lines: Vec<_> = crate::log::recent().into_iter().filter(|l| match self.log_level { 1 => l.level != "info", 2 => l.level == "error", _ => true }).filter(|l| q.is_empty() || l.msg.to_lowercase().contains(&q) || l.scope.contains(&q)).collect();
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.line_soft)).corner_radius(14).inner_margin(10).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(true).show_rows(ui, 24.0, lines.len(), |ui, range| {
                for l in &lines[range] {
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, 6.0, p.hover);
                    }
                    if resp.on_hover_text("Click to copy").clicked() {
                        ui.ctx().copy_text(format!("[{}] {}", l.scope, l.msg));
                        self.core.notify("success", "Log line copied", "", None);
                    }
                    let col = match l.level { "error" => p.err, "warn" => p.warn, _ => p.dim };
                    let t = chrono::DateTime::from_timestamp_millis(l.t).map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string()).unwrap_or_default();
                    let painter = ui.painter();
                    painter.text(Pos2::new(r.left() + 4.0, r.center().y), Align2::LEFT_CENTER, t, egui::FontId::monospace(12.0), p.faint);
                    painter.circle_filled(Pos2::new(r.left() + 76.0, r.center().y), 3.5, col);
                    widgets::text_at(ui, painter, r.left() + 88.0, r.center().y, &l.scope, egui::FontId::monospace(12.0), p.acc, 84.0);
                    widgets::text_at(ui, painter, r.left() + 184.0, r.center().y, &l.msg, egui::FontId::monospace(12.0), if l.level == "info" { p.text } else { col }, r.width() - 190.0);
                }
            });
        });
    }

    // ------------------------------------------------------------------ about
    pub(super) fn about(&mut self, ui: &mut egui::Ui) {
        let p = self.pal.clone();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| ui.vertical_centered(|ui| {
            ui.add_space(50.0);
            let (r, _) = ui.allocate_exact_size(Vec2::splat(96.0), Sense::hover());
            let t = ui.input(|i| i.time) as f32;
            ui.painter().circle_filled(r.center(), 62.0 + (t * 1.5).sin() * 3.0, alpha(p.acc, 0.08));
            paint_logo(ui.painter(), r, &p);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Fetchwing").font(fs(30.0)));
            ui.label(egui::RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).font(f(13.0)).color(p.dim));
            ui.add_space(10.0);
            ui.label(egui::RichText::new("A modern download manager - a research rebuild of the JDownloader 2 idea in Rust.").font(f(14.0)));
            ui.add_space(18.0);
            for (icon, t) in [(I::LIGHTNING, "Segmented, resumable downloads with adaptive connections"), (I::PUZZLE_PIECE, "470+ hoster & crawler plugins, debrid services, yt-dlp for 1,800+ media sites"), (I::SHIELD_CHECK, "No ads, no telemetry, no cloud - captchas never interrupt you"), (I::GLOBE, "Web UI, REST API and CLI for remote and headless use")] {
                ui.label(egui::RichText::new(format!("{icon}   {t}")).font(f(13.0)).color(p.dim));
                ui.add_space(4.0);
            }
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 470.0).max(0.0) / 2.0);
                if widgets::button(ui, Some(I::DATABASE), "Data folder", Kind::Secondary, &p).clicked() {
                    let _ = open::that_detached(&self.core.data_dir);
                }
                if widgets::button(ui, Some(I::GLOBE), "Web UI", Kind::Secondary, &p).clicked() {
                    let _ = open::that_detached(&self.api_url);
                }
                if widgets::button(ui, Some(I::COMMAND), "Shortcuts: Ctrl+K", Kind::Ghost, &p).clicked() {
                    self.palette.open = true;
                }
            });
            ui.add_space(26.0);
            egui::Frame::new().fill(p.surface).corner_radius(14).inner_margin(16).stroke(Stroke::new(1.0, p.line_soft)).show(ui, |ui| {
                ui.set_width(470.0);
                ui.label(egui::RichText::new("Keyboard").font(fs(13.5)));
                ui.add_space(6.0);
                egui::Grid::new("keys").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
                    for (k, t) in [("Ctrl+V", "Add copied links"), ("Ctrl+K", "Command palette"), ("Ctrl+F", "Search downloads"), ("Up / Down", "Move selection"), ("Space", "Pause / resume selected"), ("Enter", "Start (Inbox) / show file"), ("Delete", "Remove selected"), ("1  2  3", "Inbox / Queue / Done"), ("Esc", "Clear selection / close")] {
                        ui.horizontal(|ui| widgets::kbd(ui, k, &p));
                        ui.label(egui::RichText::new(t).font(f(12.5)).color(p.dim));
                        ui.end_row();
                    }
                });
            });
            ui.add_space(40.0);
        }));
    }
}

fn acc_status(s: AccountStatus, p: &super::theme::Palette) -> (Color32, &'static str) {
    match s {
        AccountStatus::Ok => (p.ok, "active"),
        AccountStatus::Unchecked => (p.dim, "unchecked"),
        AccountStatus::Invalid => (p.err, "invalid"),
        AccountStatus::Expired => (p.warn, "expired"),
        AccountStatus::Error => (p.err, "error"),
    }
}

fn sub_nav(ui: &mut egui::Ui, icon: &str, label: &str, on: bool, p: &super::theme::Palette) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(186.0, 32.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let h = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.1);
    let s = ui.ctx().animate_bool_with_time(resp.id.with("s"), on, 0.15);
    if h > 0.0 || s > 0.0 {
        ui.painter().rect_filled(r, 8.0, mix(alpha(p.text, 0.05 * h), p.acc_soft, s));
    }
    let col = mix(p.dim, p.text, s.max(h));
    ui.painter().text(Pos2::new(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, icon, f(15.0), if on { p.acc } else { col });
    ui.painter().text(Pos2::new(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, label, fm(13.0), col);
    let _ = StrokeKind::Inside;
    resp
}

fn fmt_num(n: f64, unit: &str) -> String {
    if unit.contains("bytes") && n >= 1024.0 {
        let s = crate::util::fmt_bytes(n as i64);
        return if unit.ends_with("/s") { format!("{s}/s") } else { s };
    }
    if n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{n}") }
}
