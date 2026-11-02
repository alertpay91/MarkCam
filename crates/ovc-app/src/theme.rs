//! The Studio's look: a few themes (one accent colour each), rounded widgets and
//! comfortable spacing.

use std::sync::atomic::{AtomicUsize, Ordering};

use eframe::egui::{self, Color32, FontFamily, FontId, Rounding, Stroke, TextStyle};

/// One look of the app: the colours every panel, card and widget is drawn with.
pub struct Palette {
    pub id: &'static str,
    pub name: &'static str,
    /// Light window (dark text) or dark window (light text).
    pub light: bool,
    pub accent: Color32,
    /// Selected buttons / selection fill (text is drawn on it).
    pub accent_dim: Color32,
    pub live: Color32,
    pub warn: Color32,
    pub error: Color32,
    /// Around the preview picture.
    pub bg: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub card_hover: Color32,
    /// Interactive widget surfaces (buttons, checkboxes, slider rails): set off from cards so
    /// they stay visible on them.
    pub widget: Color32,
    pub widget_hover: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    /// Separators, the window outline and the hovered-widget outline.
    pub line: Color32,
    pub outline: Color32,
    pub hover_line: Color32,
    /// Text edits and slider tracks.
    pub field: Color32,
}

const fn c(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const THEMES: [Palette; 5] = [
    // The original calm dark look (video tools read best on dark; the accent marks what is live).
    Palette {
        id: "midnight",
        name: "Midnight",
        light: false,
        accent: c(47, 168, 255),
        accent_dim: c(28, 92, 140),
        live: c(72, 199, 116),
        warn: c(236, 178, 64),
        error: c(240, 98, 98),
        bg: c(17, 19, 23),
        panel: c(24, 27, 33),
        card: c(31, 35, 42),
        card_hover: c(40, 45, 54),
        widget: c(50, 56, 68),
        widget_hover: c(62, 69, 84),
        text: c(222, 226, 232),
        text_dim: c(138, 146, 158),
        line: c(38, 42, 51),
        outline: c(44, 49, 59),
        hover_line: c(60, 67, 80),
        field: c(14, 16, 19),
    },
    // Bright and friendly, like a classroom: white cards, fresh green.
    Palette {
        id: "classroom",
        name: "Classroom",
        light: true,
        accent: c(46, 150, 24),
        accent_dim: c(200, 235, 180),
        live: c(34, 150, 74),
        warn: c(196, 120, 0),
        error: c(208, 52, 52),
        bg: c(218, 232, 224),
        panel: c(244, 248, 245),
        card: c(255, 255, 255),
        card_hover: c(233, 243, 236),
        widget: c(226, 234, 229),
        widget_hover: c(208, 222, 214),
        text: c(30, 42, 38),
        text_dim: c(100, 116, 110),
        line: c(216, 226, 220),
        outline: c(200, 214, 206),
        hover_line: c(170, 190, 180),
        field: c(255, 255, 255),
    },
    // Purple night with a pink accent.
    Palette {
        id: "candy",
        name: "Candy",
        light: false,
        accent: c(255, 102, 178),
        accent_dim: c(128, 46, 102),
        live: c(96, 220, 150),
        warn: c(255, 196, 80),
        error: c(255, 96, 110),
        bg: c(20, 14, 32),
        panel: c(29, 21, 45),
        card: c(39, 29, 60),
        card_hover: c(51, 39, 77),
        widget: c(62, 48, 92),
        widget_hover: c(77, 61, 110),
        text: c(242, 234, 252),
        text_dim: c(168, 152, 194),
        line: c(48, 37, 70),
        outline: c(58, 46, 86),
        hover_line: c(92, 74, 128),
        field: c(14, 10, 24),
    },
    // Deep sea blue-green with a turquoise accent.
    Palette {
        id: "ocean",
        name: "Ocean",
        light: false,
        accent: c(40, 212, 200),
        accent_dim: c(16, 104, 108),
        live: c(96, 224, 130),
        warn: c(240, 190, 80),
        error: c(246, 104, 104),
        bg: c(9, 21, 28),
        panel: c(13, 29, 38),
        card: c(19, 40, 51),
        card_hover: c(27, 52, 65),
        widget: c(33, 62, 77),
        widget_hover: c(43, 76, 93),
        text: c(222, 238, 242),
        text_dim: c(128, 160, 170),
        line: c(25, 48, 59),
        outline: c(30, 56, 69),
        hover_line: c(52, 86, 101),
        field: c(7, 16, 22),
    },
    // Warm dark with an orange-gold accent.
    Palette {
        id: "sunset",
        name: "Sunset",
        light: false,
        accent: c(255, 142, 60),
        accent_dim: c(138, 66, 28),
        live: c(120, 214, 110),
        warn: c(255, 200, 70),
        error: c(255, 98, 98),
        bg: c(21, 16, 18),
        panel: c(31, 23, 26),
        card: c(41, 31, 35),
        card_hover: c(53, 40, 45),
        widget: c(65, 49, 55),
        widget_hover: c(80, 60, 67),
        text: c(242, 230, 224),
        text_dim: c(172, 150, 142),
        line: c(50, 38, 43),
        outline: c(58, 44, 50),
        hover_line: c(90, 67, 75),
        field: c(15, 11, 13),
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The theme in use.
pub fn current() -> &'static Palette {
    &THEMES[CURRENT.load(Ordering::Relaxed).min(THEMES.len() - 1)]
}

pub fn accent() -> Color32 {
    current().accent
}
pub fn accent_dim() -> Color32 {
    current().accent_dim
}
pub fn live() -> Color32 {
    current().live
}
pub fn warn() -> Color32 {
    current().warn
}
pub fn error() -> Color32 {
    current().error
}
pub fn bg() -> Color32 {
    current().bg
}
pub fn panel() -> Color32 {
    current().panel
}
pub fn card_fill() -> Color32 {
    current().card
}
pub fn card_hover() -> Color32 {
    current().card_hover
}
pub fn text() -> Color32 {
    current().text
}
pub fn text_dim() -> Color32 {
    current().text_dim
}

/// Switch to the theme `id` (unknown ids: the first one) and restyle the widgets.
pub fn set(ctx: &egui::Context, id: &str) {
    CURRENT.store(THEMES.iter().position(|t| t.id == id).unwrap_or(0), Ordering::Relaxed);
    visuals(ctx);
}

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
    // Linux without a CJK font: symbols such as ● from DejaVu (on every Linux desktop).
    if !fonts.font_data.contains_key("cjk") {
        if let Ok(bytes) = std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            fonts.font_data.insert("symbols".into(), egui::FontData::from_owned(bytes));
            for fam in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push("symbols".into());
            }
        }
    }
    ctx.set_fonts(fonts);
    visuals(ctx);
}

/// Widget colours and spacing from the current theme.
fn visuals(ctx: &egui::Context) {
    let t = current();
    let mut v = if t.light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = t.panel;
    v.window_fill = t.panel;
    v.extreme_bg_color = t.field; // text edits, slider tracks
    v.faint_bg_color = t.card;
    v.override_text_color = Some(t.text);
    v.hyperlink_color = t.accent;
    v.selection.bg_fill = t.accent_dim;
    v.selection.stroke = Stroke::new(1.0_f32, t.accent);
    v.window_rounding = Rounding::same(10.0);
    v.menu_rounding = Rounding::same(8.0);
    v.window_stroke = Stroke::new(1.0_f32, t.outline);

    let r = Rounding::same(6.0);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.panel;
    w.noninteractive.weak_bg_fill = t.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, t.line); // separators
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, t.text_dim);
    w.noninteractive.rounding = r;
    v.slider_trailing_fill = true; // accent fill left of the slider handle
    for (ws, fill) in [
        (&mut w.inactive, t.widget),
        (&mut w.hovered, t.widget_hover),
        (&mut w.active, t.widget_hover),
        (&mut w.open, t.widget_hover),
    ] {
        ws.bg_fill = fill;
        ws.weak_bg_fill = fill;
        ws.bg_stroke = Stroke::NONE;
        ws.rounding = r;
        ws.fg_stroke = Stroke::new(1.0_f32, t.text);
    }
    w.hovered.bg_stroke = Stroke::new(1.0_f32, t.hover_line);
    w.active.bg_stroke = Stroke::new(1.0_f32, t.accent);
    // The palette always goes into egui's dark slot and that slot is always used, whatever
    // the OS theme is (egui would otherwise follow Windows and only part of it would apply).
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

/// The theme chooser: a row of little window previews. Returns true when `id` changed.
pub fn picker(ui: &mut egui::Ui, id: &mut String) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
        for t in &THEMES {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(96.0, 84.0), egui::Sense::click());
            let on = *id == t.id;
            if ui.is_rect_visible(rect) {
                let p = ui.painter();
                let win = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 58.0));
                p.rect_filled(win, Rounding::same(8.0), t.panel);
                // The picture area and a side panel with two cards.
                let pic = egui::Rect::from_min_size(win.min + egui::vec2(6.0, 14.0), egui::vec2(46.0, 38.0));
                p.rect_filled(pic, Rounding::same(4.0), t.bg);
                for i in 0..2 {
                    let y = win.min.y + 14.0 + i as f32 * 20.0;
                    p.rect_filled(egui::Rect::from_min_size(egui::pos2(win.min.x + 58.0, y), egui::vec2(32.0, 16.0)), Rounding::same(3.0), t.card);
                }
                p.circle_filled(win.min + egui::vec2(10.0, 7.0), 3.0, t.accent);
                p.rect_filled(egui::Rect::from_min_size(win.min + egui::vec2(18.0, 5.0), egui::vec2(28.0, 4.0)), Rounding::same(2.0), t.text_dim);
                p.rect_filled(egui::Rect::from_min_size(win.min + egui::vec2(66.0, 4.0), egui::vec2(24.0, 6.0)), Rounding::same(3.0), t.accent);
                let ring = if on {
                    Stroke::new(2.5_f32, accent())
                } else if resp.hovered() {
                    Stroke::new(1.5_f32, current().hover_line)
                } else {
                    Stroke::new(1.0_f32, current().outline)
                };
                p.rect_stroke(win, Rounding::same(8.0), ring);
                let label = egui::RichText::new(t.name).size(13.0).color(if on { accent() } else { text() });
                let galley = egui::WidgetText::from(if on { label.strong() } else { label }).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Body);
                p.galley(egui::pos2(rect.center().x - galley.size().x / 2.0, win.max.y + 6.0), galley, text());
            }
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
                *id = t.id.to_string();
                changed = true;
            }
        }
    });
    changed
}

/// A section title in the side panel ("EFFECTS", "COLOR"…) in small caps.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title.to_uppercase()).size(11.5).strong().color(text_dim()));
    ui.add_space(2.0);
}

/// A rounded card grouping related controls.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::none()
        .fill(card_fill())
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
    ui.painter().circle_stroke(c, 9.0, Stroke::new(2.5_f32, accent()));
    ui.painter().circle_filled(c, 4.0, accent());
}

/// A full-width list row (category list): left-aligned label, dim count on the right.
pub fn list_row(ui: &mut egui::Ui, label: &str, count: usize) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, Rounding::same(6.0), card_hover());
        }
        // Folder glyph drawn as two rounded rects.
        let f = egui::Rect::from_min_size(rect.left_center() + egui::vec2(10.0, -6.0), egui::vec2(16.0, 12.0));
        p.rect_filled(egui::Rect::from_min_size(f.min - egui::vec2(0.0, 2.0), egui::vec2(7.0, 4.0)), 1.5, text_dim());
        p.rect_filled(f, 2.0, text_dim());
        p.text(rect.left_center() + egui::vec2(36.0, 0.0), egui::Align2::LEFT_CENTER, label, FontId::proportional(14.0), text());
        p.text(rect.right_center() - egui::vec2(10.0, 0.0), egui::Align2::RIGHT_CENTER, count.to_string(), FontId::proportional(12.0), text_dim());
    }
    resp
}

/// What happened on a [`layer_tile`] this frame.
#[derive(Default)]
pub struct TileClicks {
    pub select: bool,
    pub toggle: bool,
    pub remove: bool,
}

/// An eye (open, or crossed out when `hidden`), drawn so it never depends on font glyphs.
pub fn eye_icon(p: &egui::Painter, c: egui::Pos2, r: f32, color: Color32, hidden: bool) {
    let stroke = Stroke::new(1.4_f32, color);
    let n = 14;
    let mut pts = Vec::with_capacity(2 * n + 2);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        pts.push(egui::pos2(c.x - r + 2.0 * r * t, c.y - r * 0.62 * (std::f32::consts::PI * t).sin()));
    }
    for i in 0..=n {
        let t = i as f32 / n as f32;
        pts.push(egui::pos2(c.x + r - 2.0 * r * t, c.y + r * 0.62 * (std::f32::consts::PI * t).sin()));
    }
    p.add(egui::Shape::closed_line(pts, stroke));
    p.circle_filled(c, r * 0.3, color);
    if hidden {
        p.line_segment([c + egui::vec2(-r, r * 0.85), c + egui::vec2(r, -r * 0.85)], Stroke::new(1.8_f32, color));
    }
}

/// One effect on the picture, as a small card: its picture, its name, an eye to hide /
/// show it and × to take it off. Hidden ones are dimmed, with a crossed-out eye.
pub fn layer_tile(ui: &mut egui::Ui, id: egui::Id, thumb: Option<(egui::TextureId, egui::Vec2)>, name: &str, on: bool, selected: bool) -> TileClicks {
    let size = egui::vec2(78.0, 74.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let mut out = TileClicks { select: resp.clicked(), ..Default::default() };
    let t = current();
    let eye_rect = egui::Rect::from_center_size(rect.left_top() + egui::vec2(13.0, 13.0), egui::vec2(20.0, 20.0));
    let x_rect = egui::Rect::from_center_size(rect.right_top() + egui::vec2(-13.0, 13.0), egui::vec2(20.0, 20.0));
    let eye = ui.interact(eye_rect, id.with("eye"), egui::Sense::click()).on_hover_text(if on { "Hide (keep it here for later)" } else { "Show again" });
    let x = ui.interact(x_rect, id.with("x"), egui::Sense::click()).on_hover_text("Remove from the picture");
    out.toggle = eye.clicked();
    out.remove = x.clicked();
    let hot = resp.hovered() || eye.hovered() || x.hovered();
    resp.on_hover_text(name);
    if !ui.is_rect_visible(rect) {
        return out;
    }
    let p = ui.painter();
    let fill = if selected || hot { t.card_hover } else { t.card };
    let ring = if selected { Stroke::new(2.0_f32, t.accent) } else { Stroke::new(1.0_f32, t.outline) };
    p.rect(rect, Rounding::same(10.0), fill, ring);
    // The picture, fitted into the top part.
    let pic = egui::Rect::from_min_max(rect.min + egui::vec2(8.0, 8.0), egui::pos2(rect.max.x - 8.0, rect.min.y + 50.0));
    if let Some((tex, tsize)) = thumb {
        let k = (pic.width() / tsize.x.max(1.0)).min(pic.height() / tsize.y.max(1.0));
        let r = egui::Rect::from_center_size(pic.center(), tsize * k);
        let tint = if on { Color32::WHITE } else { Color32::WHITE.gamma_multiply(0.45) };
        p.image(tex, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
    }
    // The name under it, shortened to fit.
    let font = FontId::proportional(11.0);
    let color = if on { t.text } else { t.text_dim };
    let max_w = rect.width() - 10.0;
    let fits = |s: &str| p.layout_no_wrap(s.to_owned(), font.clone(), color).size().x <= max_w;
    let mut shown = name.to_string();
    if !fits(&shown) {
        let chars: Vec<char> = name.chars().collect();
        let mut n = chars.len();
        while n > 1 && !fits(&format!("{}…", chars[..n].iter().collect::<String>().trim_end())) {
            n -= 1;
        }
        shown = format!("{}…", chars[..n].iter().collect::<String>().trim_end());
    }
    p.text(egui::pos2(rect.center().x, rect.max.y - 12.0), egui::Align2::CENTER_CENTER, shown, font, color);
    // The round buttons show on hover; a hidden one always shows its crossed-out eye.
    for (r, h, is_eye) in [(eye_rect, eye.hovered(), true), (x_rect, x.hovered(), false)] {
        if hot || selected || (is_eye && !on) {
            let bg = if h { if is_eye { t.accent_dim } else { t.error.gamma_multiply(0.85) } } else { t.panel.gamma_multiply(0.9) };
            p.circle(r.center(), 9.0, bg, Stroke::new(1.0_f32, t.outline));
            let fg = if h && !is_eye { Color32::WHITE } else { t.text };
            if is_eye {
                eye_icon(p, r.center(), 5.5, fg, !on);
            } else {
                let d = 3.2;
                let s = Stroke::new(1.5_f32, fg);
                p.line_segment([r.center() + egui::vec2(-d, -d), r.center() + egui::vec2(d, d)], s);
                p.line_segment([r.center() + egui::vec2(-d, d), r.center() + egui::vec2(d, -d)], s);
            }
        }
    }
    out
}
