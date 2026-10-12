//! Visual foundation: palette, typography (Inter + Phosphor icons), egui style.

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, Vec2};
use std::sync::Arc;

#[derive(Clone)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub sidebar: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub elevated: Color32,
    pub line: Color32,
    pub line_soft: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub faint: Color32,
    pub acc: Color32,
    pub acc2: Color32,
    pub acc_soft: Color32,
    pub sel: Color32,
    pub hover: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
}

pub const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}
/// Translucent colour premultiplied in gamma space (CSS-like). egui's `from_rgba_unmultiplied`
/// premultiplies in linear space, which makes low-alpha tints far brighter than intended.
pub fn tint(hex: u32, a: f32) -> Color32 {
    let k = a.clamp(0.0, 1.0);
    let ch = |v: u32| ((v & 0xff) as f32 * k).round() as u8;
    Color32::from_rgba_premultiplied(ch(hex >> 16), ch(hex >> 8), ch(hex), (k * 255.0).round() as u8)
}
pub fn alpha(c: Color32, a: f32) -> Color32 {
    let [r, g, b, _] = c.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, (a.clamp(0.0, 1.0) * 255.0) as u8)
}
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

impl Palette {
    pub fn dark() -> Self {
        Palette {
            dark: true,
            bg: rgb(0x0b0d12),
            sidebar: rgb(0x0e1117),
            surface: rgb(0x13161d),
            surface2: rgb(0x191d26),
            elevated: rgb(0x1e232d),
            line: rgb(0x252b36),
            line_soft: rgb(0x1b2029),
            text: rgb(0xe9ecf2),
            dim: rgb(0x8d96a8),
            faint: rgb(0x5d6577),
            acc: rgb(0x4c9eff),
            acc2: rgb(0x8b6cff),
            acc_soft: tint(0x4c9eff, 0.16),
            sel: tint(0x4c9eff, 0.13),
            hover: Color32::from_rgba_unmultiplied(255, 255, 255, 7),
            ok: rgb(0x3ddc97),
            warn: rgb(0xffb547),
            err: rgb(0xff6b6b),
        }
    }
    pub fn light() -> Self {
        Palette {
            dark: false,
            bg: rgb(0xf4f6fa),
            sidebar: rgb(0xedf0f5),
            surface: rgb(0xffffff),
            surface2: rgb(0xf2f4f8),
            elevated: rgb(0xffffff),
            line: rgb(0xdfe3eb),
            line_soft: rgb(0xebeef3),
            text: rgb(0x161a23),
            dim: rgb(0x5b6475),
            faint: rgb(0x98a0b0),
            acc: rgb(0x2f7cf6),
            acc2: rgb(0x7352f5),
            acc_soft: tint(0x2f7cf6, 0.12),
            sel: tint(0x2f7cf6, 0.10),
            hover: Color32::from_rgba_unmultiplied(0, 0, 0, 7),
            ok: rgb(0x14a067),
            warn: rgb(0xc27400),
            err: rgb(0xd93a3a),
        }
    }
}

pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}
pub fn fill_icons() -> FontFamily {
    FontFamily::Name("icons-fill".into())
}
pub fn f(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn fm(size: f32) -> FontId {
    FontId::new(size, medium())
}
pub fn fs(size: f32) -> FontId {
    FontId::new(size, semibold())
}
pub fn fi(size: f32) -> FontId {
    FontId::new(size, fill_icons())
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("inter".into(), Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf"))));
    fonts.font_data.insert("inter-medium".into(), Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/Inter-Medium.ttf"))));
    fonts.font_data.insert("inter-semibold".into(), Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"))));
    fonts.font_data.insert("phosphor".into(), Arc::new(egui_phosphor::Variant::Regular.font_data()));
    fonts.font_data.insert("phosphor-fill".into(), Arc::new(egui_phosphor::Variant::Fill.font_data()));
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    // Inter first (metrics + text). The bundled Inter files have their private-use-area mappings
    // stripped (tools/strip_pua.py) so Phosphor's PUA icons are not shadowed.
    let fam = |main: &str| {
        let mut v = vec![main.to_string(), "phosphor".to_string()];
        v.extend(defaults.iter().cloned());
        v
    };
    fonts.families.insert(FontFamily::Proportional, fam("inter"));
    fonts.families.insert(medium(), fam("inter-medium"));
    fonts.families.insert(semibold(), fam("inter-semibold"));
    fonts.families.insert(fill_icons(), vec!["phosphor-fill".into(), "inter".into()]);
    if let Some(m) = fonts.families.get_mut(&FontFamily::Monospace) {
        m.push("phosphor".into());
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context, dark: bool) -> Palette {
    let p = if dark { Palette::dark() } else { Palette::light() };
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.elevated;
    v.extreme_bg_color = p.surface2;
    v.faint_bg_color = p.surface2;
    v.code_bg_color = p.surface2;
    v.override_text_color = None;
    v.selection.bg_fill = alpha(p.acc, 0.35);
    v.selection.stroke = Stroke::new(1.0, p.acc);
    v.hyperlink_color = p.acc;
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_shadow = egui::epaint::Shadow { offset: [0, 12], blur: 36, spread: 0, color: Color32::from_black_alpha(if dark { 130 } else { 45 }) };
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(if dark { 110 } else { 30 }) };
    v.text_cursor.stroke = Stroke::new(2.0, p.acc);
    v.striped = false;
    let w = &mut v.widgets;
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = CornerRadius::same(8);
        s.expansion = 0.0;
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.weak_bg_fill = p.surface2;
    w.inactive.bg_fill = p.surface2;
    w.inactive.bg_stroke = Stroke::new(1.0, p.line);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.weak_bg_fill = p.elevated;
    w.hovered.bg_fill = p.elevated;
    w.hovered.bg_stroke = Stroke::new(1.0, alpha(p.acc, 0.6));
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.active.weak_bg_fill = p.elevated;
    w.active.bg_fill = p.elevated;
    w.active.bg_stroke = Stroke::new(1.0, p.acc);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.open.weak_bg_fill = p.elevated;
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.interact_size = Vec2::new(40.0, 30.0);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.scroll = egui::style::ScrollStyle::thin();
        s.animation_time = 0.16;
        s.text_styles.insert(egui::TextStyle::Body, f(13.5));
        s.text_styles.insert(egui::TextStyle::Button, fm(13.5));
        s.text_styles.insert(egui::TextStyle::Small, f(11.5));
        s.text_styles.insert(egui::TextStyle::Heading, fs(20.0));
        s.text_styles.insert(egui::TextStyle::Monospace, FontId::monospace(12.5));
    });
    p
}
