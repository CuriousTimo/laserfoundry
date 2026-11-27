//! Maker Connect's house style for the browser build the hub serves (`/laser-design/`): the hub's
//! colours and square corners on egui's widgets, so the tool looks like the page around it. Only the
//! widgets' look; the canvas keeps LaserFoundry's own tokens (`laserfoundry_ui_egui::theme`).
//!
//! The hub's palette (its `mc.css`): dark `--bg #111214`, `--surface #1B1C1F`, `--surface-2 #232428`,
//! `--ink #F2F1EE`, `--muted #A6A8A1`, `--line #2E3033`; light `--bg #F6F7F5`, `--surface #FFFFFF`, `--surface-2 #EEF0EC`,
//! `--ink #1D1D1B`, `--line #DCDFD8`; the house colour `#93A682` (text on white `#5E6B52`), used as
//! a tint for hover and selection, as the hub does.
//!
//! The hub's own theme switch (`localStorage["mc-theme"]`, same origin) wins over the system's: both
//! of egui's themes then get that one look.

use eframe::egui::{self, Color32, CornerRadius, Stroke, Theme, Visuals};

const HOUSE: Color32 = Color32::from_rgb(0x93, 0xA6, 0x82);
const HOUSE_TEXT: Color32 = Color32::from_rgb(0x5E, 0x6B, 0x52);

/// A colour at alpha (unmultiplied), as the hub's `rgb(... / a)` tints.
fn tint(c: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (alpha * 255.0).round() as u8)
}

struct Palette {
    bg: Color32,
    surface: Color32,
    surface_2: Color32,
    ink: Color32,
    muted: Color32,
    line: Color32,
    line_strong: Color32,
    accent_text: Color32,
}

const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x11, 0x12, 0x14),
    surface: Color32::from_rgb(0x1B, 0x1C, 0x1F),
    surface_2: Color32::from_rgb(0x23, 0x24, 0x28),
    ink: Color32::from_rgb(0xF2, 0xF1, 0xEE),
    muted: Color32::from_rgb(0xA6, 0xA8, 0xA1),
    line: Color32::from_rgb(0x2E, 0x30, 0x33),
    line_strong: Color32::from_rgb(0x76, 0x7A, 0x72),
    accent_text: HOUSE,
};

const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xF6, 0xF7, 0xF5),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    surface_2: Color32::from_rgb(0xEE, 0xF0, 0xEC),
    ink: Color32::from_rgb(0x1D, 0x1D, 0x1B),
    muted: Color32::from_rgb(0x58, 0x5C, 0x54),
    line: Color32::from_rgb(0xDC, 0xDF, 0xD8),
    line_strong: Color32::from_rgb(0x7D, 0x82, 0x78),
    accent_text: HOUSE_TEXT,
};

/// egui's visuals in the house style: square corners, the hub's surfaces and lines, the house colour
/// as a tint for hover and selection.
fn visuals(dark: bool) -> Visuals {
    let p = if dark { &DARK } else { &LIGHT };
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    let square = CornerRadius::ZERO;
    v.window_corner_radius = square;
    v.menu_corner_radius = square;
    v.panel_fill = p.surface;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.faint_bg_color = p.surface_2;
    v.extreme_bg_color = p.bg;
    v.code_bg_color = p.surface_2;
    v.hyperlink_color = p.accent_text;
    v.override_text_color = None;
    v.selection.bg_fill = tint(HOUSE, if dark { 0.30 } else { 0.28 });
    v.selection.stroke = Stroke::new(1.0, p.accent_text);
    let w = &mut v.widgets;
    for state in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        state.corner_radius = square;
    }
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.ink);
    w.inactive.bg_fill = p.surface_2;
    w.inactive.weak_bg_fill = p.surface_2;
    w.inactive.bg_stroke = Stroke::new(1.0, p.line);
    w.inactive.fg_stroke = Stroke::new(1.0, p.ink);
    w.hovered.bg_fill = tint(HOUSE, 0.18);
    w.hovered.weak_bg_fill = tint(HOUSE, 0.18);
    w.hovered.bg_stroke = Stroke::new(1.0, HOUSE);
    w.hovered.fg_stroke = Stroke::new(1.5, p.ink);
    w.active.bg_fill = tint(HOUSE, 0.30);
    w.active.weak_bg_fill = tint(HOUSE, 0.30);
    w.active.bg_stroke = Stroke::new(1.0, p.accent_text);
    w.active.fg_stroke = Stroke::new(2.0, p.ink);
    w.open.bg_fill = p.surface_2;
    w.open.weak_bg_fill = p.surface_2;
    w.open.bg_stroke = Stroke::new(1.0, p.line_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.ink);
    v.weak_text_color = Some(p.muted);
    v
}

/// The hub's own theme choice, if the person made one there ("light" or "dark").
fn hub_choice() -> Option<bool> {
    let storage = web_sys::window()?.local_storage().ok()??;
    match storage.get_item("mc-theme").ok()??.as_str() {
        "dark" => Some(true),
        "light" => Some(false),
        _ => None,
    }
}

/// Puts the house style on both of egui's themes; with a choice made on the hub, both get that one.
pub fn apply(ctx: &egui::Context) {
    let (dark, light) = match hub_choice() {
        Some(dark) => (visuals(dark), visuals(dark)),
        None => (visuals(true), visuals(false)),
    };
    ctx.set_visuals_of(Theme::Dark, dark);
    ctx.set_visuals_of(Theme::Light, light);
}
