//! Reusable, animated UI primitives. Everything is vector-drawn and theme-aware.

use super::theme::{alpha, f, fi, fm, fs, mix, Palette};
use crate::model::Status;
use eframe::egui::{self, epaint::Mesh, Align2, text::LayoutJob, Color32, CursorIcon, FontId, Galley, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};
use egui_phosphor::regular as I;
use std::sync::Arc;

// ------------------------------------------------------------------------------------------
// text
// ------------------------------------------------------------------------------------------

pub fn galley(ui: &Ui, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(10.0));
    ui.fonts(|fo| fo.layout_job(job))
}

/// Single-line text, truncated with "…", vertically centred on `y`.
pub fn text_at(ui: &Ui, painter: &Painter, x: f32, y: f32, text: &str, font: FontId, color: Color32, max_w: f32) -> Rect {
    let g = galley(ui, text, font, color, max_w);
    let pos = Pos2::new(x, y - g.size().y / 2.0);
    let r = Rect::from_min_size(pos, g.size());
    painter.galley(pos, g, color);
    r
}
pub fn text_right(ui: &Ui, painter: &Painter, right: f32, y: f32, text: &str, font: FontId, color: Color32) -> Rect {
    let g = galley(ui, text, font, color, 2000.0);
    let pos = Pos2::new(right - g.size().x, y - g.size().y / 2.0);
    let r = Rect::from_min_size(pos, g.size());
    painter.galley(pos, g, color);
    r
}

// ------------------------------------------------------------------------------------------
// colour helpers
// ------------------------------------------------------------------------------------------

pub fn hsv(h: f32, s: f32, v: f32) -> Color32 {
    let i = (h * 6.0).floor();
    let fr = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - fr * s), v * (1.0 - (1.0 - fr) * s));
    let (r, g, b) = match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// Stable, pleasant colour per host name.
pub fn host_color(host: &str) -> Color32 {
    let mut h: u32 = 2166136261;
    for b in host.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    hsv((h % 360) as f32 / 360.0, 0.55, 0.92)
}

pub fn file_icon(name: &str, p: &Palette) -> (&'static str, Color32) {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    if n.contains(".part") && n.ends_with(".rar") || ["zip", "rar", "7z", "gz", "tgz", "xz", "bz2", "zst", "tar", "001", "002", "003"].contains(&ext.as_str()) {
        return (I::FILE_ZIP, super::theme::rgb(0xf5a524));
    }
    match ext.as_str() {
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" | "ts" | "flv" => (I::FILE_VIDEO, super::theme::rgb(0xff6b9a)),
        "mp3" | "flac" | "wav" | "ogg" | "opus" | "m4a" | "aac" => (I::FILE_AUDIO, super::theme::rgb(0x2ec5c5)),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" => (I::FILE_IMAGE, super::theme::rgb(0x9b7bff)),
        "pdf" => (I::FILE_PDF, super::theme::rgb(0xff6b6b)),
        "epub" | "mobi" | "cbz" | "cbr" => (I::BOOK_OPEN, super::theme::rgb(0xe0a458)),
        "exe" | "msi" | "apk" | "deb" | "rpm" | "dmg" | "appimage" => (I::PACKAGE, super::theme::rgb(0x4cb1ff)),
        "iso" | "img" | "bin" => (I::DISC, super::theme::rgb(0x7f8ea3)),
        "txt" | "md" | "nfo" | "srt" | "sub" => (I::FILE_TEXT, p.dim),
        "torrent" => (I::MAGNET, p.acc),
        _ => (I::FILE, p.dim),
    }
}

pub fn status_style(s: Status, p: &Palette) -> (Color32, &'static str) {
    match s {
        Status::Finished => (p.ok, "Done"),
        Status::Failed => (p.err, "Failed"),
        Status::Offline => (p.err, "Offline"),
        Status::Downloading => (p.acc, "Downloading"),
        Status::Waiting => (p.warn, "Waiting"),
        Status::Captcha => (p.warn, "Captcha"),
        Status::Paused => (p.dim, "Paused"),
        Status::Checking => (p.dim, "Checking"),
        Status::New => (p.dim, "New"),
        Status::Postprocessing => (p.acc2, "Processing"),
        Status::Skipped => (p.dim, "Skipped"),
        Status::Online => (p.ok, "Ready"),
        Status::Unknown => (p.dim, "Unknown"),
        Status::Queued => (p.dim, "Queued"),
    }
}

// ------------------------------------------------------------------------------------------
// painting primitives
// ------------------------------------------------------------------------------------------

/// Horizontal gradient with round caps (pill shape). `rect.height()` is the diameter of the caps.
pub fn gradient_pill(painter: &Painter, rect: Rect, a: Color32, b: Color32) {
    let r = rect.height() / 2.0;
    if rect.width() <= rect.height() {
        painter.circle_filled(rect.center(), r.min(rect.width() / 2.0), a);
        return;
    }
    let inner = Rect::from_min_max(Pos2::new(rect.left() + r, rect.top()), Pos2::new(rect.right() - r, rect.bottom()));
    let mut m = Mesh::default();
    m.colored_vertex(inner.left_top(), a);
    m.colored_vertex(inner.right_top(), b);
    m.colored_vertex(inner.right_bottom(), b);
    m.colored_vertex(inner.left_bottom(), a);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(m));
    painter.circle_filled(Pos2::new(inner.left(), rect.center().y), r, a);
    painter.circle_filled(Pos2::new(inner.right(), rect.center().y), r, b);
}

/// Smooth gradient progress bar. `active` adds a moving shimmer.
pub fn progress_bar(ui: &Ui, painter: &Painter, rect: Rect, value: f32, active: bool, color: Option<Color32>, p: &Palette) {
    let r = rect.height() / 2.0;
    painter.rect_filled(rect, r, if p.dark { alpha(Color32::WHITE, 0.06) } else { alpha(Color32::BLACK, 0.07) });
    let v = value.clamp(0.0, 1.0);
    if v <= 0.0 {
        return;
    }
    let w = (rect.width() * v).max(rect.height());
    let fill = Rect::from_min_size(rect.min, Vec2::new(w, rect.height()));
    match color {
        Some(c) => {
            painter.rect_filled(fill, r, c);
        }
        None => gradient_pill(painter, fill, p.acc, mix(p.acc, p.acc2, v.max(0.35))),
    }
    if active && fill.width() > 24.0 {
        let t = ui.input(|i| i.time) as f32;
        let band = 60.0;
        let x = fill.left() - band + ((t * 0.9).fract()) * (fill.width() + band * 2.0);
        let clip = painter.with_clip_rect(fill.intersect(painter.clip_rect()));
        let mut m = Mesh::default();
        let c0 = Color32::TRANSPARENT;
        let c1 = alpha(Color32::WHITE, if p.dark { 0.35 } else { 0.45 });
        for (i, (xx, c)) in [(x, c0), (x + band / 2.0, c1), (x + band, c0)].iter().enumerate() {
            m.colored_vertex(Pos2::new(*xx, fill.top()), *c);
            m.colored_vertex(Pos2::new(*xx, fill.bottom()), *c);
            if i > 0 {
                let k = (i as u32) * 2;
                m.add_triangle(k - 2, k - 1, k);
                m.add_triangle(k - 1, k, k + 1);
            }
        }
        clip.add(Shape::mesh(m));
    }
}

/// Circular progress ring.
pub fn ring(painter: &Painter, c: Pos2, radius: f32, thick: f32, value: f32, p: &Palette, color: Option<Color32>) {
    painter.circle_stroke(c, radius, Stroke::new(thick, if p.dark { alpha(Color32::WHITE, 0.07) } else { alpha(Color32::BLACK, 0.07) }));
    let v = value.clamp(0.0, 1.0);
    if v <= 0.0 {
        return;
    }
    let n = (72.0 * v).ceil().max(2.0) as usize;
    let start = -std::f32::consts::FRAC_PI_2;
    let pts: Vec<Pos2> = (0..=n).map(|i| {
        let a = start + std::f32::consts::TAU * v * i as f32 / n as f32;
        Pos2::new(c.x + radius * a.cos(), c.y + radius * a.sin())
    }).collect();
    for (i, w) in pts.windows(2).enumerate() {
        let col = color.unwrap_or_else(|| mix(p.acc, p.acc2, i as f32 / n as f32));
        painter.line_segment([w[0], w[1]], Stroke::new(thick, col));
    }
    let end = *pts.last().unwrap();
    painter.circle_filled(end, thick / 2.0, color.unwrap_or(p.acc2));
    painter.circle_filled(pts[0], thick / 2.0, color.unwrap_or(p.acc));
}

/// Area chart with a vertical fade.
pub fn sparkline(painter: &Painter, rect: Rect, values: &[f32], p: &Palette) {
    if values.len() < 2 || values.iter().all(|v| *v <= 0.0) {
        let y = rect.bottom() - 1.0;
        let mut x = rect.left();
        while x < rect.right() {
            painter.hline(x..=(x + 3.0).min(rect.right()), y, Stroke::new(1.0, alpha(p.faint, 0.5)));
            x += 7.0;
        }
        painter.text(rect.center() - Vec2::new(0.0, 4.0), Align2::CENTER_CENTER, "No transfers yet", f(11.5), alpha(p.faint, 0.8));
        return;
    }
    let max = values.iter().cloned().fold(1.0f32, f32::max) * 1.15;
    let n = values.len();
    let pts: Vec<Pos2> = values.iter().enumerate().map(|(i, v)| Pos2::new(rect.left() + rect.width() * i as f32 / (n - 1) as f32, rect.bottom() - rect.height() * (v / max))).collect();
    let mut m = Mesh::default();
    let top = alpha(p.acc, 0.32);
    let bot = alpha(p.acc, 0.0);
    for (i, pt) in pts.iter().enumerate() {
        m.colored_vertex(*pt, top);
        m.colored_vertex(Pos2::new(pt.x, rect.bottom()), bot);
        if i > 0 {
            let k = (i as u32) * 2;
            m.add_triangle(k - 2, k - 1, k);
            m.add_triangle(k - 1, k, k + 1);
        }
    }
    painter.add(Shape::mesh(m));
    painter.add(Shape::line(pts, Stroke::new(1.6, p.acc)));
}

// ------------------------------------------------------------------------------------------
// interactive widgets
// ------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// Button with optional leading icon. Primary = gradient, others = subtle.
pub fn button(ui: &mut Ui, icon: Option<&str>, label: &str, kind: Kind, p: &Palette) -> Response {
    let font = fm(13.5);
    let text_col = match kind {
        Kind::Primary => Color32::WHITE,
        Kind::Danger => p.err,
        _ => p.text,
    };
    let lg = galley(ui, label, font.clone(), text_col, 400.0);
    let ig = icon.map(|i| galley(ui, i, f(15.5), text_col, 40.0));
    let pad = if label.is_empty() { 8.0 } else { 14.0 };
    let gap = if ig.is_some() && !label.is_empty() { 7.0 } else { 0.0 };
    let w = pad * 2.0 + lg.size().x + gap + ig.as_ref().map(|g| g.size().x).unwrap_or(0.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 32.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let h = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    let press = if resp.is_pointer_button_down_on() { 0.97 } else { 1.0 };
    let rect = Rect::from_center_size(rect.center(), rect.size() * press);
    let painter = ui.painter();
    match kind {
        Kind::Primary => {
            if h > 0.0 {
                painter.rect_filled(rect.expand(3.0 * h), 11.0, alpha(p.acc, 0.18 * h));
            }
            let mut m = Mesh::default();
            let (a, b) = (mix(p.acc, Color32::WHITE, 0.08 * h), mix(p.acc2, Color32::WHITE, 0.08 * h));
            // rounded gradient: draw rounded rect in a, overlay gradient mesh inset by radius
            painter.rect_filled(rect, 9.0, a);
            let inner = rect.shrink2(Vec2::new(9.0, 0.0));
            m.colored_vertex(inner.left_top(), a);
            m.colored_vertex(inner.right_top(), b);
            m.colored_vertex(inner.right_bottom(), b);
            m.colored_vertex(inner.left_bottom(), a);
            m.add_triangle(0, 1, 2);
            m.add_triangle(0, 2, 3);
            painter.rect_filled(Rect::from_min_max(Pos2::new(inner.right() - 1.0, rect.top()), rect.right_bottom()), egui::CornerRadius { nw: 0, sw: 0, ne: 9, se: 9 }, b);
            painter.add(Shape::mesh(m));
        }
        Kind::Secondary => {
            painter.rect_filled(rect, 9.0, mix(p.surface2, p.elevated, h));
            painter.rect_stroke(rect, 9.0, Stroke::new(1.0, mix(p.line, alpha(p.acc, 0.7), h)), StrokeKind::Inside);
        }
        Kind::Ghost | Kind::Danger => {
            if h > 0.0 {
                painter.rect_filled(rect, 9.0, alpha(if kind == Kind::Danger { p.err } else { p.text }, 0.07 * h));
            }
        }
    }
    let mut x = rect.left() + pad;
    if let Some(g) = ig {
        let y = rect.center().y - g.size().y / 2.0;
        let gw = g.size().x;
        painter.galley(Pos2::new(x, y), g, text_col);
        x += gw + gap;
    }
    painter.galley(Pos2::new(x, rect.center().y - lg.size().y / 2.0), lg, text_col);
    resp
}

/// Square icon-only button with tooltip.
pub fn icon_button(ui: &mut Ui, icon: &str, tip: &str, p: &Palette) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip);
    let h = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 {
        ui.painter().rect_filled(rect, 8.0, alpha(p.text, 0.08 * h));
    }
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, f(16.0), mix(p.dim, p.text, h));
    resp
}

/// iOS-style animated switch.
pub fn switch(ui: &mut Ui, on: &mut bool, p: &Palette) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(38.0, 22.0), Sense::click());
    let resp2 = resp.clone().on_hover_cursor(CursorIcon::PointingHand);
    if resp2.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.16);
    let painter = ui.painter();
    let track = mix(if p.dark { alpha(Color32::WHITE, 0.14) } else { alpha(Color32::BLACK, 0.16) }, p.acc, t);
    painter.rect_filled(rect, 11.0, track);
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    painter.circle_filled(Pos2::new(x, rect.center().y + 0.5), 8.5, Color32::from_black_alpha(40));
    painter.circle_filled(Pos2::new(x, rect.center().y), 8.5, Color32::WHITE);
    resp
}

/// Segmented control with a sliding highlight. Returns true if changed.
pub fn segmented(ui: &mut Ui, id: &str, items: &[(String, usize)], selected: &mut usize, p: &Palette) -> bool {
    let font = fm(13.0);
    let widths: Vec<f32> = items.iter().map(|(l, n)| {
        let w = galley(ui, l, font.clone(), p.text, 300.0).size().x;
        w + 26.0 + if *n > 0 { galley(ui, &n.to_string(), f(11.5), p.text, 100.0).size().x + 14.0 } else { 0.0 }
    }).collect();
    let total: f32 = widths.iter().sum::<f32>() + 6.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(total, 34.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 10.0, p.surface2);
    painter.rect_stroke(rect, 10.0, Stroke::new(1.0, p.line_soft), StrokeKind::Inside);
    let mut xs = vec![];
    let mut x = rect.left() + 3.0;
    for w in &widths {
        xs.push(x);
        x += w;
    }
    let sid = egui::Id::new(id);
    let tx = ui.ctx().animate_value_with_time(sid.with("x"), xs[*selected], 0.2);
    let tw = ui.ctx().animate_value_with_time(sid.with("w"), widths[*selected], 0.2);
    let hl = Rect::from_min_size(Pos2::new(tx, rect.top() + 3.0), Vec2::new(tw, rect.height() - 6.0));
    painter.rect_filled(hl, 8.0, p.elevated);
    painter.rect_stroke(hl, 8.0, Stroke::new(1.0, p.line), StrokeKind::Inside);
    let mut changed = false;
    for (i, ((label, n), w)) in items.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(Pos2::new(xs[i], rect.top()), Vec2::new(*w, rect.height()));
        let resp = ui.interact(r, sid.with(i), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        if resp.clicked() && *selected != i {
            *selected = i;
            changed = true;
        }
        let on = *selected == i;
        let col = if on { p.text } else if resp.hovered() { mix(p.dim, p.text, 0.5) } else { p.dim };
        let lr = text_at(ui, &painter, r.left() + 13.0, r.center().y, label, font.clone(), col, 300.0);
        if *n > 0 {
            let badge = galley(ui, &n.to_string(), f(11.5), if on { Color32::WHITE } else { p.dim }, 100.0);
            let br = Rect::from_min_size(Pos2::new(lr.right() + 7.0, r.center().y - 9.0), Vec2::new(badge.size().x + 12.0, 18.0));
            painter.rect_filled(br, 9.0, if on { p.acc } else { alpha(p.text, 0.08) });
            painter.galley(Pos2::new(br.left() + 6.0, br.center().y - badge.size().y / 2.0), badge, Color32::WHITE);
        }
    }
    changed
}

/// Filter chip.
pub fn chip(ui: &mut Ui, label: &str, on: bool, color: Color32, p: &Palette) -> Response {
    let g = galley(ui, label, fm(12.5), if on { p.text } else { p.dim }, 300.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 30.0, 28.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let h = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered() || on, 0.12);
    let painter = ui.painter();
    painter.rect_filled(rect, 14.0, if on { alpha(color, 0.16) } else { alpha(p.text, 0.04 + 0.04 * h) });
    painter.rect_stroke(rect, 14.0, Stroke::new(1.0, if on { alpha(color, 0.5) } else { p.line_soft }), StrokeKind::Inside);
    painter.circle_filled(Pos2::new(rect.left() + 12.0, rect.center().y), 3.5, color);
    painter.galley(Pos2::new(rect.left() + 21.0, rect.center().y - g.size().y / 2.0), g, p.text);
    resp
}

/// Small coloured label.
pub fn pill(ui: &Ui, painter: &Painter, x: f32, y: f32, text: &str, color: Color32) -> Rect {
    let g = galley(ui, text, fm(11.5), color, 200.0);
    let r = Rect::from_min_size(Pos2::new(x, y - 10.0), Vec2::new(g.size().x + 16.0, 20.0));
    painter.rect_filled(r, 10.0, alpha(color, 0.14));
    painter.galley(Pos2::new(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, color);
    r
}
pub fn pill_ui(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let g = galley(ui, text, fm(11.5), color, 300.0);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 16.0, 20.0), Sense::hover());
    ui.painter().rect_filled(r, 10.0, alpha(color, 0.14));
    ui.painter().galley(Pos2::new(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, color);
    resp
}

/// Search box with icon, hint and clear button.
pub fn search(ui: &mut Ui, id: &str, text: &mut String, hint: &str, width: f32, p: &Palette) -> Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::hover());
    let tid = egui::Id::new(id);
    let focused = ui.memory(|m| m.has_focus(tid));
    let fo = ui.ctx().animate_bool_with_time(tid.with("f"), focused, 0.15);
    ui.painter().rect_filled(rect, 9.0, mix(p.surface2, p.elevated, fo));
    ui.painter().rect_stroke(rect, 9.0, Stroke::new(1.0, mix(p.line_soft, p.acc, fo)), StrokeKind::Inside);
    ui.painter().text(Pos2::new(rect.left() + 12.0, rect.center().y), egui::Align2::LEFT_CENTER, I::MAGNIFYING_GLASS, f(14.0), p.dim);
    let te_rect = Rect::from_min_max(Pos2::new(rect.left() + 32.0, rect.top() + 1.0), Pos2::new(rect.right() - if text.is_empty() { 8.0 } else { 30.0 }, rect.bottom()));
    let resp = ui.put(te_rect, egui::TextEdit::singleline(text).id(tid).frame(false).hint_text(egui::RichText::new(hint).color(p.faint)).font(f(13.5)).vertical_align(egui::Align::Center).margin(Vec2::new(0.0, 6.0)));
    if !text.is_empty() {
        let cr = Rect::from_center_size(Pos2::new(rect.right() - 16.0, rect.center().y), Vec2::splat(20.0));
        let c = ui.interact(cr, tid.with("x"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        ui.painter().text(cr.center(), egui::Align2::CENTER_CENTER, I::X_CIRCLE, f(15.0), if c.hovered() { p.text } else { p.faint });
        if c.clicked() {
            text.clear();
        }
    }
    resp
}

/// Keyboard key cap hint.
pub fn kbd(ui: &mut Ui, keys: &str, p: &Palette) {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| kbd_inner(ui, keys, p));
}
fn kbd_inner(ui: &mut Ui, keys: &str, p: &Palette) {
    ui.spacing_mut().item_spacing.x = 3.0;
    for (i, k) in keys.split('+').enumerate() {
        if i > 0 {
            ui.add_space(2.0);
        }
        let g = galley(ui, k, fm(11.0), p.dim, 100.0);
        let (r, _) = ui.allocate_exact_size(Vec2::new(g.size().x + 10.0, 20.0), Sense::hover());
        ui.painter().rect_filled(r, 5.0, alpha(p.text, 0.06));
        ui.painter().rect_stroke(r, 5.0, Stroke::new(1.0, p.line), StrokeKind::Inside);
        ui.painter().galley(Pos2::new(r.left() + 5.0, r.center().y - g.size().y / 2.0), g, p.dim);
    }
}

pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.line_soft)).corner_radius(14).inner_margin(egui::Margin::same(18)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    });
    ui.add_space(12.0);
    r.inner
}

/// Section header inside views.
pub fn view_title(ui: &mut Ui, icon: &str, title: &str, subtitle: &str, p: &Palette) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
        ui.painter().rect_filled(r, 10.0, p.acc_soft);
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, icon, f(19.0), p.acc);
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.add_space(-2.0);
            ui.label(egui::RichText::new(title).font(fs(20.0)).color(p.text));
            if !subtitle.is_empty() {
                ui.label(egui::RichText::new(subtitle).font(f(12.5)).color(p.dim));
            }
        });
    });
    ui.add_space(14.0);
}

/// Centered empty state with a soft icon badge.
pub fn empty_state(ui: &mut Ui, icon: &str, title: &str, subtitle: &str, p: &Palette) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.22).max(30.0));
        let (r, _) = ui.allocate_exact_size(Vec2::splat(88.0), Sense::hover());
        let t = ui.input(|i| i.time) as f32;
        let bob = (t * 1.6).sin() * 3.0;
        let c = r.center() + Vec2::new(0.0, bob);
        ui.painter().circle_filled(c, 44.0, p.acc_soft);
        ui.painter().circle_stroke(c, 44.0, Stroke::new(1.0, alpha(p.acc, 0.25)));
        ui.painter().text(c, egui::Align2::CENTER_CENTER, icon, fi(38.0), p.acc);
        ui.add_space(16.0);
        ui.label(egui::RichText::new(title).font(fs(17.0)).color(p.text));
        ui.add_space(4.0);
        ui.label(egui::RichText::new(subtitle).font(f(13.0)).color(p.dim));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    });
}

/// Host avatar: coloured rounded square with the first letter.
pub fn host_badge(painter: &Painter, rect: Rect, host: &str) {
    let c = host_color(host);
    painter.rect_filled(rect, 6.0, alpha(c, 0.18));
    let letter = host.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_ascii_uppercase()).unwrap_or('?');
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, letter, fs(rect.height() * 0.55), c);
}
