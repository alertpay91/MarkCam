//! The Studio's look: a calm dark theme with one accent colour, rounded widgets and
//! comfortable spacing (video tools read best on dark; the accent marks what is live).

use eframe::egui::{self, Color32, FontFamily, FontId, Rounding, Stroke, TextStyle};

pub const ACCENT: Color32 = Color32::from_rgb(47, 168, 255);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(28, 92, 140);
pub const LIVE: Color32 = Color32::from_rgb(72, 199, 116);
pub const WARN: Color32 = Color32::from_rgb(236, 178, 64);
pub const ERROR: Color32 = Color32::from_rgb(240, 98, 98);

pub const BG: Color32 = Color32::from_rgb(17, 19, 23);
pub const PANEL: Color32 = Color32::from_rgb(24, 27, 33);
pub const CARD: Color32 = Color32::from_rgb(31, 35, 42);
pub const CARD_HOVER: Color32 = Color32::from_rgb(40, 45, 54);
/// Interactive widget surfaces (buttons, checkboxes, slider rails) — lighter than cards so
/// they stay visible on them.
const WIDGET: Color32 = Color32::from_rgb(50, 56, 68);
const WIDGET_HOVER: Color32 = Color32::from_rgb(62, 69, 84);
pub const TEXT: Color32 = Color32::from_rgb(222, 226, 232);
pub const TEXT_DIM: Color32 = Color32::from_rgb(138, 146, 158);

pub fn apply(ctx: &egui::Context) {
    // Chinese / Japanese / Korean in text fields and labels (banner text, phrases): the
    // system's CJK font as a fallback after egui's own.
    let mut fonts = egui::FontDefinitions::default();
    for p in [r"C:\Windows\Fonts\msyh.ttc", "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc"] {
        if let Ok(bytes) = std::fs::read(p) {
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes));
            for fam in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push("cjk".into());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(14, 16, 19); // text edits, slider tracks
    v.faint_bg_color = CARD;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.window_rounding = Rounding::same(10.0);
    v.menu_rounding = Rounding::same(8.0);
    v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(44, 49, 59));

    let r = Rounding::same(6.0);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(38, 42, 51)); // separators
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_DIM);
    w.noninteractive.rounding = r;
    v.slider_trailing_fill = true; // accent fill left of the slider handle
    for (ws, fill) in [
        (&mut w.inactive, WIDGET),
        (&mut w.hovered, WIDGET_HOVER),
        (&mut w.active, WIDGET_HOVER),
        (&mut w.open, WIDGET_HOVER),
    ] {
        ws.bg_fill = fill;
        ws.weak_bg_fill = fill;
        ws.bg_stroke = Stroke::NONE;
        ws.rounding = r;
        ws.fg_stroke = Stroke::new(1.0_f32, TEXT);
    }
    w.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(60, 67, 80));
    w.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    // Always dark, whatever the OS theme is (egui would otherwise follow a light Windows
    // theme and only part of this palette would apply).
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);

    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.slider_width = 150.0;
        s.spacing.combo_width = 120.0;
        s.text_styles = [
            (TextStyle::Heading, FontId::new(18.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
    });
}

/// A section title in the side panel ("EFFECTS", "COLOR"…), ManyCam-style small caps.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title.to_uppercase()).size(11.5).strong().color(TEXT_DIM));
    ui.add_space(2.0);
}

/// A rounded card grouping related controls.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::none()
        .fill(CARD)
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A small pill-shaped status badge (fixed height, whatever the surrounding layout).
/// Clickable: it highlights on hover.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(12.5), color);
    let size = egui::vec2(galley.size().x + 22.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let p = ui.painter();
    let fill = color.gamma_multiply(if resp.hovered() { 0.28 } else { 0.16 });
    p.rect(rect, Rounding::same(12.0), fill, Stroke::new(1.0_f32, color.gamma_multiply(0.55)));
    p.galley(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// The app mark: an accent ring with a dot (drawn, so it never depends on font glyphs).
pub fn logo(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
    let c = rect.center();
    ui.painter().circle_stroke(c, 9.0, Stroke::new(2.5_f32, ACCENT));
    ui.painter().circle_filled(c, 4.0, ACCENT);
}

/// A full-width list row (category list): left-aligned label, dim count on the right.
pub fn list_row(ui: &mut egui::Ui, label: &str, count: usize) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, Rounding::same(6.0), CARD_HOVER);
        }
        // Folder glyph drawn as two rounded rects.
        let f = egui::Rect::from_min_size(rect.left_center() + egui::vec2(10.0, -6.0), egui::vec2(16.0, 12.0));
        p.rect_filled(egui::Rect::from_min_size(f.min - egui::vec2(0.0, 2.0), egui::vec2(7.0, 4.0)), 1.5, TEXT_DIM);
        p.rect_filled(f, 2.0, TEXT_DIM);
        p.text(rect.left_center() + egui::vec2(36.0, 0.0), egui::Align2::LEFT_CENTER, label, FontId::proportional(14.0), TEXT);
        p.text(rect.right_center() - egui::vec2(10.0, 0.0), egui::Align2::RIGHT_CENTER, count.to_string(), FontId::proportional(12.0), TEXT_DIM);
    }
    resp
}
