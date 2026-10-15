//! Name banners ("lower thirds"): a 3D-styled banner with a title (your name), a second
//! line (a title, a greeting, Chinese…) and an emoji icon, that slides in at the bottom of
//! the picture, shines now and then, and slides away.
//!
//! The banner is drawn once (tiny-skia vector graphics + real fonts) into three layers —
//! plate, text, icon — whenever its text / style / size changes; each frame only moves and
//! blends those layers, so it costs almost nothing while shown.

use std::path::PathBuf;
use std::time::Instant;

use ab_glyph::{Font, FontArc, FontVec};
use eframe::egui;
use serde::{Deserialize, Serialize};
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Point, RadialGradient, Shader, SpreadMode, Stroke, Transform,
};

// ─────────────────────────────────────────────────────────────────────────────────────
// What the user sets
// ─────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Style {
    #[default]
    Glossy,
    Candy,
    News,
    Neon,
    Bubble,
}

impl Style {
    pub const ALL: [Style; 5] = [Style::Glossy, Style::Candy, Style::News, Style::Neon, Style::Bubble];
    pub fn name(self) -> &'static str {
        match self {
            Style::Glossy => "Glossy 3D",
            Style::Candy => "Candy (kids)",
            Style::News => "TV news",
            Style::Neon => "Neon",
            Style::Bubble => "Speech bubble",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Place {
    #[default]
    Left,
    Center,
    Right,
}

/// Color themes: (light, dark).
pub const COLORS: [(&str, [u8; 3], [u8; 3]); 8] = [
    ("Blue", [79, 172, 254], [30, 91, 216]),
    ("Purple", [176, 108, 255], [106, 43, 214]),
    ("Pink", [255, 111, 174], [214, 36, 110]),
    ("Orange", [255, 179, 71], [240, 101, 47]),
    ("Green", [94, 224, 122], [31, 158, 72]),
    ("Teal", [62, 224, 208], [15, 143, 138]),
    ("Red", [255, 107, 107], [200, 30, 58]),
    ("Gold", [255, 216, 77], [224, 155, 0]),
];

/// Icons offered in the banner editor (emoji catalog keys).
pub const ICONS: [&str; 24] = [
    "1f44b", "1f9d1-200d-1f3eb", "1f4da", "1f30d", "2b50", "1f31f", "1f680", "1f4a1", "1f393", "1f3c6", "2764", "1f60a", "1f308", "1f389", "1f3a4", "270f", "1f34e", "1f981", "1f984", "1f436", "1f431",
    "1f3b5", "1f4ac", "2600",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BannerDef {
    pub title: String,
    pub subtitle: String,
    pub style: Style,
    pub color: usize,
    /// Emoji catalog key, or "" for none.
    pub icon: String,
    pub place: Place,
    /// Seconds on screen, 0 = until hidden.
    pub stay: u32,
    /// Size: banner height as a fraction of the picture height.
    pub size: f32,
}

impl Default for BannerDef {
    fn default() -> Self {
        BannerDef {
            title: "Teacher Mark".into(),
            subtitle: "English teacher · 英语老师".into(),
            style: Style::Glossy,
            color: 0,
            icon: "1f44b".into(),
            place: Place::Left,
            stay: 0,
            size: 0.17,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Fonts and text
// ─────────────────────────────────────────────────────────────────────────────────────

struct Fonts {
    round: FontArc,
    bold: FontArc,
    semi: FontArc,
    comic: FontArc,
    /// Chinese / Japanese / Korean fallback from the system, if found.
    cjk: Option<FontArc>,
}

fn fonts() -> &'static Fonts {
    static F: std::sync::OnceLock<Fonts> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let f = |b: &'static [u8]| FontArc::try_from_slice(b).expect("bundled font");
        let cjk_files: [(&str, u32); 5] = [
            (r"C:\Windows\Fonts\msyhbd.ttc", 0),
            (r"C:\Windows\Fonts\msyh.ttc", 0),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc", 0),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", 0),
        ];
        let cjk = cjk_files.iter().find_map(|(p, i)| FontVec::try_from_vec_and_index(std::fs::read(p).ok()?, *i).ok().map(FontArc::new));
        Fonts {
            round: f(include_bytes!("../assets/fonts/LilitaOne-Regular.ttf")),
            bold: f(include_bytes!("../assets/fonts/Poppins-ExtraBold.ttf")),
            semi: f(include_bytes!("../assets/fonts/Poppins-SemiBold.ttf")),
            comic: f(include_bytes!("../assets/fonts/LuckiestGuy-Regular.ttf")),
            cjk,
        }
    })
}

/// A line of text as a vector path at size `px` (em height), starting at x = 0 with the
/// baseline at y = 0. Characters the font lacks come from the CJK font. Returns the path
/// and the advance width.
fn text_path(font: &FontArc, text: &str, px: f32) -> (Option<Path>, f32) {
    let fs = fonts();
    let mut pb = PathBuilder::new();
    let mut x = 0.0f32;
    let mut prev: Option<(ab_glyph::GlyphId, *const FontArc)> = None;
    for ch in text.chars() {
        let (f, id) = {
            let id = font.glyph_id(ch);
            if id.0 == 0 && !ch.is_whitespace() {
                match fs.cjk.as_ref().map(|c| (c, c.glyph_id(ch))).filter(|(_, i)| i.0 != 0) {
                    Some((c, i)) => (c, i),
                    None => (font, id),
                }
            } else {
                (font, id)
            }
        };
        let s = px / f.units_per_em().unwrap_or(1000.0);
        if let Some((p, pf)) = prev {
            if std::ptr::eq(pf, f) {
                x += f.kern_unscaled(p, id) * s;
            }
        }
        if let Some(outline) = f.outline(id) {
            let mut last: Option<ab_glyph::Point> = None;
            let tp = |p: ab_glyph::Point| (x + p.x * s, -p.y * s);
            for c in &outline.curves {
                let (start, end) = match c {
                    ab_glyph::OutlineCurve::Line(a, b) => (*a, *b),
                    ab_glyph::OutlineCurve::Quad(a, _, b) => (*a, *b),
                    ab_glyph::OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
                };
                if last.is_none_or(|l| (l.x - start.x).abs() > 0.01 || (l.y - start.y).abs() > 0.01) {
                    if last.is_some() {
                        pb.close();
                    }
                    let (sx, sy) = tp(start);
                    pb.move_to(sx, sy);
                }
                match c {
                    ab_glyph::OutlineCurve::Line(_, b) => {
                        let (bx, by) = tp(*b);
                        pb.line_to(bx, by);
                    }
                    ab_glyph::OutlineCurve::Quad(_, c1, b) => {
                        let ((cx, cy), (bx, by)) = (tp(*c1), tp(*b));
                        pb.quad_to(cx, cy, bx, by);
                    }
                    ab_glyph::OutlineCurve::Cubic(_, c1, c2, b) => {
                        let ((c1x, c1y), (c2x, c2y), (bx, by)) = (tp(*c1), tp(*c2), tp(*b));
                        pb.cubic_to(c1x, c1y, c2x, c2y, bx, by);
                    }
                }
                last = Some(end);
            }
            if last.is_some() {
                pb.close();
            }
        }
        x += f.h_advance_unscaled(id) * s;
        prev = Some((id, f as *const FontArc));
    }
    (pb.finish(), x)
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Drawing helpers
// ─────────────────────────────────────────────────────────────────────────────────────

fn rgba(c: [u8; 3], a: f32) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn shade(c: [u8; 3], k: f32) -> [u8; 3] {
    let f = |v: u8| if k >= 1.0 { (v as f32 + (255.0 - v as f32) * (k - 1.0)).min(255.0) as u8 } else { (v as f32 * k) as u8 };
    [f(c[0]), f(c[1]), f(c[2])]
}

fn solid(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = true;
    p
}

fn vgrad(y0: f32, y1: f32, stops: &[(f32, Color)]) -> Paint<'static> {
    let mut p = Paint::default();
    p.anti_alias = true;
    p.shader = LinearGradient::new(Point::from_xy(0.0, y0), Point::from_xy(0.0, y1), stops.iter().map(|(o, c)| GradientStop::new(*o, *c)).collect(), SpreadMode::Pad, Transform::identity())
        .unwrap_or(Shader::SolidColor(stops[0].1));
    p
}

fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = 0.5523 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish().unwrap()
}

fn circle(cx: f32, cy: f32, r: f32) -> Path {
    PathBuilder::from_circle(cx, cy, r.max(0.5)).unwrap()
}

/// A 4-point sparkle.
fn sparkle(cx: f32, cy: f32, r: f32) -> Path {
    let mut pb = PathBuilder::new();
    let q = r * 0.22;
    pb.move_to(cx, cy - r);
    pb.quad_to(cx + q, cy - q, cx + r, cy);
    pb.quad_to(cx + q, cy + q, cx, cy + r);
    pb.quad_to(cx - q, cy + q, cx - r, cy);
    pb.quad_to(cx - q, cy - q, cx, cy - r);
    pb.close();
    pb.finish().unwrap()
}

/// Box blur (3 passes ≈ gaussian) of a premultiplied pixmap.
fn blur(pm: &mut Pixmap, radius: f32) {
    let r = radius.round() as i32;
    if r < 1 {
        return;
    }
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let mut a = pm.data().to_vec();
    let mut b = vec![0u8; a.len()];
    let pass = |src: &[u8], dst: &mut [u8], horiz: bool| {
        let (outer, inner) = if horiz { (h, w) } else { (w, h) };
        let idx = |o: i32, i: i32| -> usize { (if horiz { o * w + i } else { i * w + o }) as usize * 4 };
        let n = (2 * r + 1) as u32;
        for o in 0..outer {
            let mut acc = [0u32; 4];
            for i in -r..=r {
                let s = idx(o, i.clamp(0, inner - 1));
                for c in 0..4 {
                    acc[c] += src[s + c] as u32;
                }
            }
            for i in 0..inner {
                let d = idx(o, i);
                for c in 0..4 {
                    dst[d + c] = (acc[c] / n) as u8;
                }
                let (add, sub) = (idx(o, (i + r + 1).min(inner - 1)), idx(o, (i - r).max(0)));
                for c in 0..4 {
                    acc[c] = acc[c] + src[add + c] as u32 - src[sub + c] as u32;
                }
            }
        }
    };
    for _ in 0..3 {
        pass(&a, &mut b, true);
        pass(&b, &mut a, false);
    }
    pm.data_mut().copy_from_slice(&a);
}
/// A soft drop shadow of `path` (offset, blur, darkness) into `pm`.
fn shadow(pm: &mut Pixmap, path: &Path, dx: f32, dy: f32, radius: f32, alpha: f32) {
    let Some(mut s) = Pixmap::new(pm.width(), pm.height()) else { return };
    s.fill_path(path, &solid(Color::from_rgba8(0, 0, 0, (alpha * 255.0) as u8)), FillRule::Winding, Transform::from_translate(dx, dy), None);
    blur(&mut s, radius);
    pm.draw_pixmap(0, 0, s.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
}

/// A glow (blurred copy) of `path` filled or stroked in `c`.
fn glow(pm: &mut Pixmap, path: &Path, c: Color, stroke: Option<f32>, radius: f32, tf: Transform) {
    let Some(mut s) = Pixmap::new(pm.width(), pm.height()) else { return };
    match stroke {
        Some(wd) => {
            s.stroke_path(path, &solid(c), &Stroke { width: wd, line_join: LineJoin::Round, line_cap: LineCap::Round, ..Default::default() }, tf, None);
        }
        None => {
            s.fill_path(path, &solid(c), FillRule::Winding, tf, None);
        }
    }
    blur(&mut s, radius);
    pm.draw_pixmap(0, 0, s.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
}

/// Text with an outline behind it (and optional 3D extrusion / drop shadow).
#[allow(clippy::too_many_arguments)]
fn draw_text(pm: &mut Pixmap, path: &Path, at: (f32, f32), fill: &Paint, outline: Option<(Color, f32)>, extrude: Option<(Color, f32)>, drop: Option<(f32, f32)>) {
    let tf = Transform::from_translate(at.0, at.1);
    let st = |w: f32| Stroke { width: w, line_join: LineJoin::Round, line_cap: LineCap::Round, ..Default::default() };
    if let Some((dist, a)) = drop {
        let Some(mut s) = Pixmap::new(pm.width(), pm.height()) else { return };
        let w = outline.map(|o| o.1).unwrap_or(0.0);
        s.fill_path(path, &solid(Color::from_rgba8(0, 0, 0, (a * 255.0) as u8)), FillRule::Winding, Transform::from_translate(at.0 + dist * 0.3, at.1 + dist), None);
        if w > 0.0 {
            s.stroke_path(path, &solid(Color::from_rgba8(0, 0, 0, (a * 255.0) as u8)), &st(w), Transform::from_translate(at.0 + dist * 0.3, at.1 + dist), None);
        }
        blur(&mut s, dist * 0.6);
        pm.draw_pixmap(0, 0, s.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    }
    if let Some((c, depth)) = extrude {
        let steps = depth.ceil().max(1.0) as i32;
        for i in (1..=steps).rev() {
            let t = Transform::from_translate(at.0 + i as f32 * 0.35, at.1 + i as f32);
            if let Some((_, w)) = outline {
                pm.stroke_path(path, &solid(c), &st(w), t, None);
            }
            pm.fill_path(path, &solid(c), FillRule::Winding, t, None);
        }
    }
    if let Some((c, w)) = outline {
        pm.stroke_path(path, &solid(c), &st(w), tf, None);
    }
    pm.fill_path(path, fill, FillRule::Winding, tf, None);
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Building the layers
// ─────────────────────────────────────────────────────────────────────────────────────

/// The pre-drawn banner: plate (with its 3D depth and shadow), text, icon; positions are
/// relative to the banner's top-left corner.
pub struct Layers {
    pub w: u32,
    pub h: u32,
    pub plate: Pixmap,
    pub text: Pixmap,
    /// Icon pixmap and its center.
    pub icon: Option<(Pixmap, f32, f32)>,
}

/// An icon image (straight RGBA) as a pixmap of `d`x`d`, aspect kept.
fn icon_pixmap(img: &(Vec<u8>, u32, u32), d: f32) -> Option<Pixmap> {
    let (data, w, h) = img;
    let mut src = Pixmap::new(*w, *h)?;
    for (s, p) in data.chunks_exact(4).zip(src.data_mut().chunks_exact_mut(4)) {
        let a = s[3] as u32;
        p[0] = (s[0] as u32 * a / 255) as u8;
        p[1] = (s[1] as u32 * a / 255) as u8;
        p[2] = (s[2] as u32 * a / 255) as u8;
        p[3] = s[3];
    }
    let side = d.ceil().max(2.0) as u32;
    let mut out = Pixmap::new(side, side)?;
    let k = d / (*w.max(h) as f32);
    let (ox, oy) = ((d - *w as f32 * k) / 2.0, (d - *h as f32 * k) / 2.0);
    out.draw_pixmap(0, 0, src.as_ref(), &PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() }, Transform::from_row(k, 0.0, 0.0, k, ox, oy), None);
    Some(out)
}

/// Draw the banner for a picture `frame_h` pixels tall.
pub fn build(def: &BannerDef, frame_w: u32, frame_h: u32, icon: Option<&(Vec<u8>, u32, u32)>) -> Option<Layers> {
    let fs = fonts();
    let hh = (frame_h as f32 * def.size.clamp(0.08, 0.35)).round();
    let (light, dark) = (COLORS[def.color % COLORS.len()].1, COLORS[def.color % COLORS.len()].2);
    let title = if def.title.trim().is_empty() { " " } else { def.title.trim() };
    let sub = def.subtitle.trim();
    let has_icon = icon.is_some();
    let max_w = frame_w as f32 * 0.92;

    // Fonts per style.
    let (tfont, sfont) = match def.style {
        Style::Glossy | Style::Bubble => (&fs.round, &fs.semi),
        Style::Candy => (&fs.comic, &fs.round),
        Style::News | Style::Neon => (&fs.bold, &fs.semi),
    };
    let mut tpx = hh * if sub.is_empty() { 0.5 } else { 0.42 };
    let mut spx = hh * 0.2;
    let (mut tpath, mut tw) = text_path(tfont, title, tpx);
    let (mut spath, mut sw) = if sub.is_empty() { (None, 0.0) } else { text_path(sfont, sub, spx) };
    let icon_d = hh * 0.92;
    let pad = hh * 0.32;
    let lead = if has_icon { icon_d * 0.82 } else { pad * 0.4 };
    // Too wide for the picture: shrink the text.
    let need = lead + pad + tw.max(sw + hh * 0.4) + pad;
    if need > max_w {
        let k = (max_w - lead - 2.0 * pad) / (need - lead - 2.0 * pad);
        tpx *= k;
        spx *= k;
        (tpath, tw) = text_path(tfont, title, tpx);
        if !sub.is_empty() {
            (spath, sw) = text_path(sfont, sub, spx);
        }
    }

    let margin = hh * 0.25; // room for shadows / glow
    let main_h = if sub.is_empty() { hh * 0.72 } else { hh * 0.6 };
    let main_w = lead + pad + tw + pad;
    let sub_h = hh * 0.34;
    let sub_w = sw + hh * 0.4;
    let total_w = (main_w.max(lead + hh * 0.3 + sub_w) + 2.0 * margin).ceil() as u32;
    let total_h = (hh + 2.0 * margin).ceil() as u32;
    let mut plate = Pixmap::new(total_w, total_h)?;
    let mut text = Pixmap::new(total_w, total_h)?;
    let (x0, y0) = (margin, margin + if sub.is_empty() { (hh - main_h) / 2.0 } else { 0.0 });
    let main = rrect(x0, y0, main_w, main_h, main_h * 0.5);
    let (sx, sy) = (x0 + lead + hh * 0.2, y0 + main_h - hh * 0.04);
    let tag = rrect(sx, sy, sub_w, sub_h, sub_h * 0.5);
    let title_at = (x0 + lead + pad, y0 + main_h * 0.5 + tpx * 0.36);
    let sub_at = (sx + hh * 0.2, sy + sub_h * 0.5 + spx * 0.36);
    let depth = (hh * 0.07).max(2.0);

    match def.style {
        Style::Glossy => {
            shadow(&mut plate, &main, 0.0, depth + hh * 0.06, hh * 0.08, 0.45);
            plate.fill_path(&main, &solid(rgba(shade(dark, 0.62), 1.0)), FillRule::Winding, Transform::from_translate(0.0, depth), None);
            plate.fill_path(&main, &vgrad(y0, y0 + main_h, &[(0.0, rgba(shade(light, 1.15), 1.0)), (1.0, rgba(dark, 1.0))]), FillRule::Winding, Transform::identity(), None);
            // Gloss on the top half.
            let gloss = rrect(x0 + main_h * 0.25, y0 + main_h * 0.06, main_w - main_h * 0.5, main_h * 0.42, main_h * 0.21);
            plate.fill_path(&gloss, &vgrad(y0, y0 + main_h * 0.5, &[(0.0, Color::from_rgba8(255, 255, 255, 150)), (1.0, Color::from_rgba8(255, 255, 255, 10))]), FillRule::Winding, Transform::identity(), None);
            plate.stroke_path(&main, &solid(Color::from_rgba8(255, 255, 255, 110)), &Stroke { width: hh * 0.02, ..Default::default() }, Transform::identity(), None);
            if let Some(sp) = &spath {
                plate.fill_path(&tag, &solid(rgba(shade(light, 0.5), 1.0)), FillRule::Winding, Transform::from_translate(0.0, depth * 0.6), None);
                plate.fill_path(&tag, &vgrad(sy, sy + sub_h, &[(0.0, Color::WHITE), (1.0, Color::from_rgba8(232, 238, 246, 255))]), FillRule::Winding, Transform::identity(), None);
                draw_text(&mut text, sp, sub_at, &solid(rgba(shade(dark, 0.9), 1.0)), None, None, None);
            }
            if let Some(tp) = &tpath {
                draw_text(&mut text, tp, title_at, &solid(Color::WHITE), Some((rgba(shade(dark, 0.55), 1.0), tpx * 0.12)), Some((rgba(shade(dark, 0.4), 1.0), tpx * 0.06)), Some((tpx * 0.08, 0.3)));
            }
        }
        Style::Candy => {
            let accent = COLORS[(def.color + 3) % COLORS.len()];
            shadow(&mut plate, &main, 0.0, hh * 0.08, hh * 0.08, 0.4);
            plate.fill_path(&main, &solid(Color::WHITE), FillRule::Winding, Transform::identity(), None);
            let inner = rrect(x0 + hh * 0.05, y0 + hh * 0.05, main_w - hh * 0.1, main_h - hh * 0.1, (main_h - hh * 0.1) * 0.5);
            plate.fill_path(&inner, &vgrad(y0, y0 + main_h, &[(0.0, rgba(light, 1.0)), (1.0, rgba(dark, 1.0))]), FillRule::Winding, Transform::identity(), None);
            // Polka dots inside.
            if let Some(mask) = {
                let mut m = Mask::new(total_w, total_h);
                if let Some(m) = m.as_mut() {
                    m.fill_path(&inner, FillRule::Winding, true, Transform::identity());
                }
                m
            } {
                let step = hh * 0.22;
                let mut yy = y0;
                let mut row = 0;
                while yy < y0 + main_h {
                    let mut xx = x0 + if row % 2 == 0 { 0.0 } else { step / 2.0 };
                    while xx < x0 + main_w {
                        plate.fill_path(&circle(xx, yy, hh * 0.035), &solid(Color::from_rgba8(255, 255, 255, 45)), FillRule::Winding, Transform::identity(), Some(&mask));
                        xx += step;
                    }
                    yy += step * 0.8;
                    row += 1;
                }
            }
            for (fx, fy, r) in [(0.97, 0.12, 0.13), (0.9, 0.0, 0.07), (0.03, 0.92, 0.08)] {
                plate.fill_path(&sparkle(x0 + main_w * fx, y0 + main_h * fy, hh * r), &solid(Color::from_rgba8(255, 244, 150, 255)), FillRule::Winding, Transform::identity(), None);
            }
            if let Some(sp) = &spath {
                plate.fill_path(&tag, &solid(rgba(shade(accent.2, 0.55), 1.0)), FillRule::Winding, Transform::from_translate(0.0, depth * 0.6), None);
                plate.fill_path(&tag, &vgrad(sy, sy + sub_h, &[(0.0, rgba(accent.1, 1.0)), (1.0, rgba(accent.2, 1.0))]), FillRule::Winding, Transform::identity(), None);
                draw_text(&mut text, sp, sub_at, &solid(Color::WHITE), Some((rgba(shade(accent.2, 0.5), 1.0), spx * 0.14)), None, None);
            }
            if let Some(tp) = &tpath {
                let fill = vgrad(title_at.1 - tpx * 0.75, title_at.1, &[(0.0, Color::from_rgba8(255, 250, 200, 255)), (1.0, Color::from_rgba8(255, 196, 40, 255))]);
                draw_text(&mut text, tp, title_at, &fill, Some((Color::from_rgba8(60, 20, 90, 255), tpx * 0.16)), Some((Color::from_rgba8(40, 10, 60, 255), tpx * 0.09)), None);
            }
        }
        Style::News => {
            // Square block with the icon, a white bar with the name, a colored bar below.
            let block = rrect(x0, y0, lead.max(main_h), main_h + if sub.is_empty() { 0.0 } else { sub_h * 0.9 }, hh * 0.06);
            let bar = rrect(x0 + lead.max(main_h) - hh * 0.04, y0, main_w - lead.max(main_h) + hh * 0.3, main_h, hh * 0.06);
            shadow(&mut plate, &bar, 0.0, hh * 0.06, hh * 0.07, 0.4);
            shadow(&mut plate, &block, 0.0, hh * 0.06, hh * 0.07, 0.4);
            plate.fill_path(&bar, &vgrad(y0, y0 + main_h, &[(0.0, Color::WHITE), (1.0, Color::from_rgba8(226, 232, 240, 255))]), FillRule::Winding, Transform::identity(), None);
            plate.fill_path(&block, &vgrad(y0, y0 + main_h, &[(0.0, rgba(light, 1.0)), (1.0, rgba(dark, 1.0))]), FillRule::Winding, Transform::identity(), None);
            // Accent line on the bar's top.
            plate.fill_path(&rrect(x0 + lead.max(main_h), y0, main_w - lead.max(main_h) + hh * 0.26, hh * 0.05, 1.0), &solid(rgba(light, 1.0)), FillRule::Winding, Transform::identity(), None);
            if let Some(sp) = &spath {
                let sb = rrect(x0 + lead.max(main_h) - hh * 0.04, y0 + main_h, sub_w + hh * 0.2, sub_h * 0.9, hh * 0.04);
                plate.fill_path(&sb, &vgrad(y0 + main_h, y0 + main_h + sub_h, &[(0.0, rgba(dark, 1.0)), (1.0, rgba(shade(dark, 0.7), 1.0))]), FillRule::Winding, Transform::identity(), None);
                draw_text(&mut text, sp, (x0 + lead.max(main_h) + hh * 0.12, y0 + main_h + sub_h * 0.45 + spx * 0.36), &solid(Color::WHITE), None, None, None);
            }
            if let Some(tp) = &tpath {
                draw_text(&mut text, tp, (x0 + lead.max(main_h) + pad * 0.6, title_at.1), &solid(Color::from_rgba8(25, 30, 45, 255)), None, None, None);
            }
        }
        Style::Neon => {
            plate.fill_path(&main, &solid(Color::from_rgba8(12, 10, 30, 200)), FillRule::Winding, Transform::identity(), None);
            glow(&mut plate, &main, rgba(light, 1.0), Some(hh * 0.06), hh * 0.08, Transform::identity());
            plate.stroke_path(&main, &solid(rgba(shade(light, 1.5), 1.0)), &Stroke { width: hh * 0.025, ..Default::default() }, Transform::identity(), None);
            if let Some(sp) = &spath {
                plate.fill_path(&tag, &solid(Color::from_rgba8(12, 10, 30, 200)), FillRule::Winding, Transform::identity(), None);
                let accent = COLORS[(def.color + 2) % COLORS.len()].1;
                glow(&mut plate, &tag, rgba(accent, 1.0), Some(hh * 0.04), hh * 0.05, Transform::identity());
                plate.stroke_path(&tag, &solid(rgba(shade(accent, 1.5), 1.0)), &Stroke { width: hh * 0.018, ..Default::default() }, Transform::identity(), None);
                glow(&mut text, sp, rgba(accent, 1.0), None, spx * 0.25, Transform::from_translate(sub_at.0, sub_at.1));
                draw_text(&mut text, sp, sub_at, &solid(rgba(shade(accent, 1.6), 1.0)), None, None, None);
            }
            if let Some(tp) = &tpath {
                glow(&mut text, tp, rgba(light, 1.0), None, tpx * 0.18, Transform::from_translate(title_at.0, title_at.1));
                draw_text(&mut text, tp, title_at, &solid(Color::WHITE), None, None, None);
            }
        }
        Style::Bubble => {
            // A white speech bubble with a colored outline and a tail.
            let mut pb = PathBuilder::new();
            let (bx, by, bw, bh2) = (x0, y0, main_w, main_h + if sub.is_empty() { 0.0 } else { sub_h * 0.85 });
            let r = (bh2 * 0.45).min(hh * 0.4);
            pb.push_path(&rrect(bx, by, bw, bh2, r));
            let tail_x = bx + bw * 0.2;
            pb.move_to(tail_x, by + bh2 - 2.0);
            pb.line_to(tail_x - hh * 0.12, by + bh2 + hh * 0.2);
            pb.line_to(tail_x + hh * 0.22, by + bh2 - 2.0);
            pb.close();
            let bubble = pb.finish()?;
            shadow(&mut plate, &bubble, 0.0, hh * 0.07, hh * 0.08, 0.4);
            plate.stroke_path(&bubble, &solid(rgba(dark, 1.0)), &Stroke { width: hh * 0.07, line_join: LineJoin::Round, ..Default::default() }, Transform::identity(), None);
            plate.fill_path(&bubble, &vgrad(by, by + bh2, &[(0.0, Color::WHITE), (1.0, Color::from_rgba8(236, 242, 250, 255))]), FillRule::Winding, Transform::identity(), None);
            if let Some(tp) = &tpath {
                let fill = vgrad(title_at.1 - tpx * 0.75, title_at.1, &[(0.0, rgba(light, 1.0)), (1.0, rgba(dark, 1.0))]);
                draw_text(&mut text, tp, title_at, &fill, None, Some((rgba(shade(dark, 0.5), 1.0), tpx * 0.06)), None);
            }
            if let Some(sp) = &spath {
                draw_text(&mut text, sp, (x0 + lead + pad, y0 + main_h + sub_h * 0.35 + spx * 0.2), &solid(Color::from_rgba8(80, 88, 104, 255)), None, None, None);
            }
        }
    }

    // Icon: in a white circle badge (except News / Bubble, where it sits on the block).
    let icon_layer = icon.and_then(|img| {
        let (cx, cy) = match def.style {
            Style::News => (x0 + lead.max(main_h) / 2.0, y0 + main_h / 2.0),
            _ => (x0 + icon_d * 0.42, y0 + main_h / 2.0),
        };
        let d = match def.style {
            Style::News => main_h * 0.78,
            Style::Bubble => icon_d * 0.8,
            _ => icon_d * 0.72,
        };
        let badge_d = icon_d;
        let side = (badge_d + hh * 0.3).ceil() as u32;
        let mut pm = Pixmap::new(side, side)?;
        let c = side as f32 / 2.0;
        if matches!(def.style, Style::Glossy | Style::Candy | Style::Neon) {
            let badge = circle(c, c, badge_d / 2.0);
            shadow(&mut pm, &badge, 0.0, hh * 0.05, hh * 0.05, 0.4);
            let fill = if def.style == Style::Neon { solid(Color::from_rgba8(12, 10, 30, 230)) } else {
                let mut p = Paint::default();
                p.anti_alias = true;
                p.shader = RadialGradient::new(Point::from_xy(c - badge_d * 0.15, c - badge_d * 0.2), Point::from_xy(c, c), badge_d * 0.6, vec![GradientStop::new(0.0, Color::WHITE), GradientStop::new(1.0, Color::from_rgba8(220, 228, 240, 255))], SpreadMode::Pad, Transform::identity())
                    .unwrap_or(Shader::SolidColor(Color::WHITE));
                p
            };
            pm.fill_path(&badge, &fill, FillRule::Winding, Transform::identity(), None);
            let ring = if def.style == Style::Neon { rgba(light, 1.0) } else { rgba(dark, 1.0) };
            pm.stroke_path(&badge, &solid(ring), &Stroke { width: hh * 0.045, ..Default::default() }, Transform::identity(), None);
        }
        let ip = icon_pixmap(img, d)?;
        pm.draw_pixmap((c - ip.width() as f32 / 2.0) as i32, (c - ip.height() as f32 / 2.0) as i32, ip.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        Some((pm, cx, cy))
    });

    Some(Layers { w: total_w, h: total_h, plate, text, icon: icon_layer })
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Live: animation and compositing
// ─────────────────────────────────────────────────────────────────────────────────────

fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let (c1, c3) = (1.70158f32, 2.70158f32);
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn ease_in_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t
}

/// Blend a premultiplied pixmap onto a straight RGBA frame at (x, y) with opacity `ga`,
/// optionally with a light sweep (`shine` = band center x in frame px, band width).
fn blend(frame: &mut [u8], fw: u32, fh: u32, pm: &Pixmap, x: i32, y: i32, ga: f32, shine: Option<(f32, f32)>) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let (x0, x1) = (x.max(0), (x + pw).min(fw as i32));
    let (y0, y1) = (y.max(0), (y + ph).min(fh as i32));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let g = (ga.clamp(0.0, 1.0) * 256.0) as u32;
    let src = pm.data();
    for yy in y0..y1 {
        let srow = ((yy - y) * pw) as usize * 4;
        let drow = (yy as u32 * fw) as usize * 4;
        for xx in x0..x1 {
            let si = srow + (xx - x) as usize * 4;
            let a = src[si + 3] as u32 * g >> 8;
            if a == 0 {
                continue;
            }
            let di = drow + xx as usize * 4;
            let mut add = 0u32;
            if let Some((c, bw)) = shine {
                let d = ((xx as f32 + (yy - y) as f32 * 0.6) - c).abs();
                if d < bw {
                    add = ((1.0 - d / bw) * 110.0) as u32 * a / 255;
                }
            }
            for k in 0..3 {
                let s = (src[si + k] as u32 * g >> 8) + add;
                frame[di + k] = (s + frame[di + k] as u32 * (255 - a) / 255).min(255) as u8;
            }
        }
    }
}

/// Like `blend` but scaled by `s` around the pixmap center placed at (cx, cy).
fn blend_scaled(frame: &mut [u8], fw: u32, fh: u32, pm: &Pixmap, cx: f32, cy: f32, s: f32, ga: f32) {
    if s <= 0.01 {
        return;
    }
    if (s - 1.0).abs() < 0.01 {
        blend(frame, fw, fh, pm, (cx - pm.width() as f32 / 2.0).round() as i32, (cy - pm.height() as f32 / 2.0).round() as i32, ga, None);
        return;
    }
    let (pw, ph) = (pm.width() as f32 * s, pm.height() as f32 * s);
    let Some(mut tmp) = Pixmap::new(pw.ceil().max(1.0) as u32, ph.ceil().max(1.0) as u32) else { return };
    tmp.draw_pixmap(0, 0, pm.as_ref(), &PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() }, Transform::from_scale(s, s), None);
    blend(frame, fw, fh, &tmp, (cx - pw / 2.0).round() as i32, (cy - ph / 2.0).round() as i32, ga, None);
}

/// A banner on the picture: animates in, stays (or for `stay` seconds), animates out.
pub struct Live {
    pub def: BannerDef,
    icon: Option<(Vec<u8>, u32, u32)>,
    shown: Instant,
    leaving: Option<Instant>,
    layers: Option<(u32, u32, Layers)>,
}

impl Live {
    pub fn new(def: BannerDef, icon: Option<(Vec<u8>, u32, u32)>) -> Self {
        Live { def, icon, shown: Instant::now(), leaving: None, layers: None }
    }

    /// New text / look while shown: redrawn on the next frame, without a new slide-in.
    pub fn update(&mut self, def: BannerDef, icon: Option<(Vec<u8>, u32, u32)>) {
        self.def = def;
        if icon.is_some() || self.def.icon.is_empty() {
            self.icon = icon;
        }
        self.layers = None;
    }

    /// Start the exit animation.
    pub fn hide(&mut self) {
        if self.leaving.is_none() {
            self.leaving = Some(Instant::now());
        }
    }

    pub fn is_leaving(&self) -> bool {
        self.leaving.is_some()
    }

    /// Draw onto the frame; false once it has left (remove it then).
    pub fn draw(&mut self, frame: &mut [u8], w: u32, h: u32) -> bool {
        if self.layers.as_ref().is_none_or(|l| (l.0, l.1) != (w, h)) {
            self.layers = build(&self.def, w, h, self.icon.as_ref()).map(|l| (w, h, l));
        }
        let t = self.shown.elapsed().as_secs_f32();
        if self.def.stay > 0 && t > self.def.stay as f32 + 0.7 {
            self.hide();
        }
        let Some((_, _, l)) = &self.layers else { return false };
        let out = self.leaving.map(|s| s.elapsed().as_secs_f32() / 0.5).unwrap_or(0.0);
        if out >= 1.0 {
            return false;
        }
        let margin_x = (w as f32 * 0.035) as i32;
        let base_y = h as i32 - l.h as i32 - (h as f32 * 0.04) as i32;
        let base_x = match self.def.place {
            Place::Left => margin_x,
            Place::Center => (w as i32 - l.w as i32) / 2,
            Place::Right => w as i32 - l.w as i32 - margin_x,
        };
        // Slide in from the nearest side (center: rise from below).
        let e_in = ease_out_back(t / 0.65);
        let e_out = ease_in_cubic(out);
        let travel = match self.def.place {
            Place::Left => -(base_x + l.w as i32) as f32,
            Place::Right => (w as i32 - base_x) as f32,
            Place::Center => 0.0,
        };
        let (dx, dy) = match self.def.place {
            Place::Center => (0.0, (1.0 - e_in) * (l.h as f32 * 1.2) + e_out * l.h as f32 * 1.2),
            _ => ((1.0 - e_in) * travel + e_out * travel, 0.0),
        };
        let alpha = (t / 0.25).min(1.0) * (1.0 - out);
        let (px, py) = (base_x + dx.round() as i32, base_y + dy.round() as i32);
        // A light sweep every 4 s.
        let cycle = (t - 1.0).rem_euclid(4.0);
        let shine = (t > 1.0 && cycle < 0.9).then(|| {
            let p = cycle / 0.9;
            (px as f32 - l.h as f32 + p * (l.w as f32 + 2.0 * l.h as f32), l.h as f32 * 0.35)
        });
        blend(frame, w, h, &l.plate, px, py, alpha, shine);
        let ta = ((t - 0.2) / 0.3).clamp(0.0, 1.0) * (1.0 - out);
        let tdx = ((1.0 - ease_out_back((t - 0.2) / 0.5)) * l.h as f32 * 0.4).round() as i32;
        blend(frame, w, h, &l.text, px + tdx, py, ta, None);
        if let Some((ip, cx, cy)) = &l.icon {
            let s = ease_out_back((t - 0.3) / 0.45) * (1.0 - e_out);
            let bob = (t * 2.2).sin() * l.h as f32 * 0.02;
            blend_scaled(frame, w, h, ip, px as f32 + cx, py as f32 + cy + bob, s, 1.0 - out);
        }
        true
    }
}

/// The icon image for a banner (first frame of the 3D emoji, downloading it if needed).
pub fn icon_image(key: &str, fetch: &crate::maker::Fetcher, ctx: &egui::Context) -> Option<(Vec<u8>, u32, u32)> {
    type Cache = std::sync::Mutex<std::collections::HashMap<String, (Vec<u8>, u32, u32)>>;
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap().get(key) {
        return Some(v.clone());
    }
    let e = crate::maker::find_emoji(key)?;
    let file: PathBuf = fetch.emoji(e, false, ctx).or_else(|| fetch.emoji(e, true, ctx))?;
    let fr = ovc_mcam::load_image_effect(&file).ok()?.frames.into_iter().next()?;
    let v = (fr.rgba, fr.w, fr.h);
    cache.lock().unwrap().insert(key.to_string(), v.clone());
    Some(v)
}

/// A small preview (straight RGBA) of a banner, for the style picker.
pub fn preview(def: &BannerDef, w: u32, icon: Option<&(Vec<u8>, u32, u32)>) -> Option<(Vec<u8>, u32, u32)> {
    let l = build(def, w * 3, (w as f32 * 3.0 * 9.0 / 16.0) as u32, icon)?;
    let mut out = Pixmap::new(l.w, l.h)?;
    out.draw_pixmap(0, 0, l.plate.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    out.draw_pixmap(0, 0, l.text.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    if let Some((ip, cx, cy)) = &l.icon {
        out.draw_pixmap((cx - ip.width() as f32 / 2.0) as i32, (cy - ip.height() as f32 / 2.0) as i32, ip.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    }
    let mut v = out.data().to_vec();
    for p in v.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 {
            for c in 0..3 {
                p[c] = (p[c] as u32 * 255 / a).min(255) as u8;
            }
        }
    }
    Some((v, l.w, l.h))
}
