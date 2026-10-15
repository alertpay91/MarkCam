//! Effect Maker: build your own face effect from pieces (emoji, images, GIFs, cut-out
//! photos), place them on your face live, give them an animation, and save the result as an
//! ordinary face sticker in "My effects" (an animated PNG + `<name>.face.json`), plus the
//! project (`<name>.maker.json`) so it can be edited again or shared as one `.markcam` file.
//!
//! Positions are in *face units* (see `FACE_UNIT_PER_IPD`): origin between the eyes, +x to
//! the picture's right, +y down the face; eyes at x = ±0.6, mouth ≈ 1.27, chin ≈ 2.25, top of
//! the head ≈ -2.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui;
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────────────
// Project model
// ─────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Source {
    /// An emoji by catalog key ("1f600"): `animated` = Noto's animation, else the 3D one.
    Emoji { key: String, animated: bool },
    /// An image / GIF / animated WebP on disk.
    File(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Motion {
    #[default]
    None,
    Pulse,
    Wiggle,
    Swing,
    Bounce,
    Float,
    Shake,
    Spin,
    Twinkle,
    Orbit,
    Rain,
    Rise,
    PopIn,
}

impl Motion {
    pub const ALL: [Motion; 13] = [
        Motion::None,
        Motion::Pulse,
        Motion::Wiggle,
        Motion::Swing,
        Motion::Bounce,
        Motion::Float,
        Motion::Shake,
        Motion::Spin,
        Motion::Twinkle,
        Motion::Orbit,
        Motion::Rain,
        Motion::Rise,
        Motion::PopIn,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Motion::None => "No animation",
            Motion::Pulse => "Pulse (grow / shrink)",
            Motion::Wiggle => "Wiggle",
            Motion::Swing => "Swing (hanging)",
            Motion::Bounce => "Bounce",
            Motion::Float => "Float",
            Motion::Shake => "Shake",
            Motion::Spin => "Spin",
            Motion::Twinkle => "Twinkle",
            Motion::Orbit => "Fly around the head",
            Motion::Rain => "Fall down (rain)",
            Motion::Rise => "Float up (bubbles)",
            Motion::PopIn => "Pop in",
        }
    }
    /// Uses several copies (orbit / rain / rise).
    pub fn has_copies(self) -> bool {
        matches!(self, Motion::Orbit | Motion::Rain | Motion::Rise)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub source: Source,
    pub label: String,
    /// Center, face units.
    pub x: f32,
    pub y: f32,
    /// Height, face units.
    pub size: f32,
    /// Degrees, clockwise.
    pub rot: f32,
    pub flip: bool,
    /// Also draw a mirrored copy on the other side of the face (ears, horns, cheeks).
    pub pair: bool,
    pub opacity: f32,
    pub on: bool,
    pub motion: Motion,
    /// Cycles per loop: 1 slow, 2 normal, 3 fast.
    pub speed: u8,
    /// 0..1.
    pub amount: f32,
    pub copies: u8,
}

impl Part {
    pub fn new(source: Source, label: String) -> Self {
        Part { source, label, x: 0.0, y: -2.2, size: 1.4, rot: 0.0, flip: false, pair: false, opacity: 1.0, on: true, motion: Motion::None, speed: 2, amount: 0.5, copies: 5 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub parts: Vec<Part>,
}

/// Where a part goes: (label, x, y, size, pair).
pub const SPOTS: [(&str, f32, f32, f32, bool); 13] = [
    ("Whole face", 0.0, 0.5, 4.0, false),
    ("Forehead", 0.0, -0.95, 1.0, false),
    ("Both eyes", 0.0, 0.05, 2.6, false),
    ("Eyes (pair)", 0.6, 0.0, 0.85, true),
    ("Nose", 0.0, 0.62, 0.8, false),
    ("Mouth", 0.0, 1.27, 1.0, false),
    ("Cheeks", 1.0, 0.85, 0.8, true),
    ("Ears", 1.85, 0.2, 1.8, true),
    ("Horns / antennae", 0.9, -2.1, 1.3, true),
    ("Chin / beard", 0.0, 1.95, 1.7, false),
    ("On the head", 0.0, -2.2, 2.0, false),
    ("Above the head", 0.0, -2.9, 1.6, false),
    ("Beside the head", 1.9, -1.6, 1.3, false),
];

pub fn projects_dir() -> PathBuf {
    crate::library::my_effects_dir()
}

fn assets_dir(name: &str) -> PathBuf {
    projects_dir().join(".maker").join(safe_name(name))
}

/// A file name from a shared file: no folders, no hidden files, extension kept.
fn safe_file(s: &str) -> String {
    let base = s.rsplit(['/', '\\']).next().unwrap_or(s);
    let t: String = base.chars().map(|c| if c.is_alphanumeric() || " -_()'.".contains(c) { c } else { '_' }).collect();
    let t = t.trim_start_matches('.').trim().to_string();
    if t.is_empty() {
        "piece.png".into()
    } else {
        t
    }
}

pub fn safe_name(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_alphanumeric() || " -_()'".contains(c) { c } else { '_' }).collect();
    let t = t.trim().to_string();
    if t.is_empty() {
        "My effect".into()
    } else {
        t
    }
}

/// Saved projects (name, path of the `.maker.json`).
pub fn list_projects() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(projects_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| {
            let n = p.file_name()?.to_string_lossy().into_owned();
            n.strip_suffix(".maker.json").map(|s| (s.to_string(), p.clone()))
        })
        .collect();
    v.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    v
}

pub fn load_project(path: &Path) -> anyhow::Result<Project> {
    let bytes = std::fs::read(path)?;
    // Tolerate a UTF-8 byte order mark (files written by Notepad / PowerShell).
    let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Ok(serde_json::from_slice(b)?)
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Emoji catalog + downloads
// ─────────────────────────────────────────────────────────────────────────────────────

pub struct Emoji {
    /// Noto's animation id ("1f600", "2764_fe0f"), if animated.
    pub noto: Option<String>,
    pub key: String,
    /// File name (no extension) in the 3D package, if any.
    pub three: Option<String>,
    pub name: String,
    hay: String,
}

pub fn catalog() -> &'static [Emoji] {
    static CAT: std::sync::OnceLock<Vec<Emoji>> = std::sync::OnceLock::new();
    CAT.get_or_init(|| {
        include_str!("../assets/emoji-catalog.tsv")
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                let ids = f.next()?;
                let name = f.next()?.to_string();
                let kw = f.next().unwrap_or("");
                let mut id = ids.split('|');
                let noto = id.next().filter(|s| !s.is_empty()).map(str::to_string);
                let key = id.next()?.to_string();
                let three = id.next().filter(|s| !s.is_empty()).map(str::to_string);
                let hay = format!(" {} {} ", name, kw).to_lowercase();
                Some(Emoji { noto, key, three, name, hay })
            })
            .collect()
    })
}

pub fn find_emoji(key: &str) -> Option<&'static Emoji> {
    catalog().iter().find(|e| e.key == key)
}

/// Search by words; whole-word matches in the name first. Empty query: popular faces,
/// animals, hands, weather (the catalog order).
pub fn search(query: &str, animated_only: bool, limit: usize) -> Vec<&'static Emoji> {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    let mut hits: Vec<(i32, usize, &Emoji)> = catalog()
        .iter()
        .enumerate()
        .filter(|(_, e)| !animated_only || e.noto.is_some())
        .filter_map(|(i, e)| {
            let mut score = 0;
            for w in &words {
                if e.hay.contains(&format!(" {w} ")) {
                    score += 3;
                } else if e.hay.contains(w.as_str()) {
                    score += 1;
                } else {
                    return None;
                }
                if e.name.to_lowercase() == *w {
                    score += 5;
                }
            }
            Some((score, i, e))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().take(limit).map(|h| h.2).collect()
}

fn emoji_cache() -> PathBuf {
    crate::settings::app_dir().join("emoji")
}

/// URL and cache file of an emoji: the animation (Noto, 512 px WebP) or the 3D image.
fn emoji_url(e: &Emoji, animated: bool) -> Option<(String, PathBuf)> {
    if animated {
        let id = e.noto.as_ref()?;
        Some((format!("https://fonts.gstatic.com/s/e/notoemoji/latest/{id}/512.webp"), emoji_cache().join("anim").join(format!("{id}.webp"))))
    } else {
        let f = e.three.as_ref()?;
        Some((format!("https://cdn.jsdelivr.net/npm/@lobehub/fluent-emoji-3d@1.1.0/assets/{f}.webp"), emoji_cache().join("3d").join(format!("{f}.webp"))))
    }
}

fn download(url: &str, dst: &Path) -> anyhow::Result<()> {
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d)?;
    }
    let resp = ureq::get(url).timeout(std::time::Duration::from_secs(30)).call()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes)?;
    if bytes.len() < 32 {
        anyhow::bail!("empty download");
    }
    let tmp = dst.with_extension("part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

#[derive(Clone)]
enum Fetch {
    Pending,
    Ready(PathBuf),
    Failed,
}

/// Background downloads (emoji images), a few at a time.
#[derive(Clone, Default)]
pub struct Fetcher {
    state: Arc<Mutex<HashMap<String, Fetch>>>,
    queue: Arc<Mutex<Vec<(String, String, PathBuf)>>>,
    workers: Arc<Mutex<usize>>,
}

impl Fetcher {
    /// The local file for (url, cache path), starting a download when needed.
    fn get(&self, id: &str, url: &str, file: &Path, ctx: &egui::Context) -> Option<PathBuf> {
        if file.is_file() {
            return Some(file.to_path_buf());
        }
        let mut st = self.state.lock().unwrap();
        match st.get(id) {
            Some(Fetch::Ready(p)) => return Some(p.clone()),
            Some(Fetch::Pending) | Some(Fetch::Failed) => return None,
            None => {}
        }
        st.insert(id.to_string(), Fetch::Pending);
        self.queue.lock().unwrap().push((id.to_string(), url.to_string(), file.to_path_buf()));
        let mut n = self.workers.lock().unwrap();
        if *n < 4 {
            *n += 1;
            let (state, queue, workers, ctx) = (self.state.clone(), self.queue.clone(), self.workers.clone(), ctx.clone());
            std::thread::spawn(move || loop {
                let job = queue.lock().unwrap().pop();
                let Some((id, url, file)) = job else {
                    *workers.lock().unwrap() -= 1;
                    break;
                };
                let r = download(&url, &file);
                state.lock().unwrap().insert(id, if r.is_ok() { Fetch::Ready(file) } else { Fetch::Failed });
                ctx.request_repaint();
            });
        }
        None
    }

    pub fn failed(&self, id: &str) -> bool {
        matches!(self.state.lock().unwrap().get(id), Some(Fetch::Failed))
    }

    pub fn emoji(&self, e: &Emoji, animated: bool, ctx: &egui::Context) -> Option<PathBuf> {
        let (url, file) = emoji_url(e, animated)?;
        self.get(&format!("{}{}", if animated { "a:" } else { "3:" }, e.key), &url, &file, ctx)
    }
}

/// The file a source draws from (downloading an emoji if it isn't cached yet).
pub fn source_file(src: &Source, fetch: &Fetcher, ctx: &egui::Context) -> Option<PathBuf> {
    match src {
        Source::File(p) => Some(p.clone()),
        Source::Emoji { key, animated } => {
            let e = find_emoji(key)?;
            fetch.emoji(e, *animated && e.noto.is_some(), ctx).or_else(|| if *animated { None } else { fetch.emoji(e, true, ctx) })
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Sprites + rendering
// ─────────────────────────────────────────────────────────────────────────────────────

/// A decoded source: frames (straight RGBA) and the total loop length.
pub struct Sprite {
    pub frames: Vec<ovc_mcam::Frame>,
    pub total_ms: u32,
}

impl Sprite {
    pub fn load(path: &Path) -> anyhow::Result<Sprite> {
        let mut frames = ovc_mcam::load_image_effect(path)?.frames;
        // Big images: shrink (a part never needs more than ~512 px).
        for f in &mut frames {
            let long = f.w.max(f.h);
            if long > 512 {
                let s = 512.0 / long as f32;
                let (nw, nh) = (((f.w as f32 * s) as u32).max(1), ((f.h as f32 * s) as u32).max(1));
                if let Some(img) = image::RgbaImage::from_raw(f.w, f.h, std::mem::take(&mut f.rgba)) {
                    f.rgba = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle).into_raw();
                    (f.w, f.h) = (nw, nh);
                }
            }
        }
        let total_ms = if frames.len() > 1 { frames.iter().map(|f| f.delay_ms.max(20)).sum() } else { 0 };
        Ok(Sprite { frames, total_ms })
    }

    fn at(&self, ms: f32) -> &ovc_mcam::Frame {
        if self.frames.len() <= 1 || self.total_ms == 0 {
            return &self.frames[0];
        }
        let mut t = (ms as u32) % self.total_ms;
        for f in &self.frames {
            let d = f.delay_ms.max(20);
            if t < d {
                return f;
            }
            t -= d;
        }
        self.frames.last().unwrap()
    }

    fn aspect(&self) -> f32 {
        let f = &self.frames[0];
        f.w as f32 / f.h.max(1) as f32
    }
}

/// One drawn copy at time `phase` (0..1 over the loop): center, height, rotation (rad),
/// flip, opacity, and the pivot (top center for Swing).
struct Placed {
    x: f32,
    y: f32,
    h: f32,
    rot: f32,
    flip: bool,
    alpha: f32,
}

fn placements(p: &Part, phase: f32, out: &mut Vec<Placed>) {
    use std::f32::consts::{PI, TAU};
    let sides: &[f32] = if p.pair { &[1.0, -1.0] } else { &[1.0] };
    let cyc = p.speed.clamp(1, 3) as f32;
    let a = p.amount.clamp(0.0, 1.0);
    let ph = (phase * cyc).fract();
    let copies = if p.motion.has_copies() { p.copies.clamp(1, 12) as usize } else { 1 };
    for &side in sides {
        for k in 0..copies {
            let kp = k as f32 / copies as f32;
            let (mut x, mut y, mut h, mut rot, mut alpha) = (p.x * side, p.y, p.size, p.rot.to_radians() * side, p.opacity);
            match p.motion {
                Motion::None => {}
                Motion::Pulse => h *= 1.0 + 0.25 * a * (TAU * ph).sin(),
                Motion::Wiggle => rot += (20.0 * a).to_radians() * (TAU * ph).sin() * side,
                Motion::Swing => {
                    // Pendulum hanging from its top center.
                    let d = (25.0 * a).to_radians() * (TAU * ph).sin() * side;
                    let (px, py) = (x + (h / 2.0) * rot.sin(), y - (h / 2.0) * rot.cos());
                    rot += d;
                    x = px - (h / 2.0) * rot.sin();
                    y = py + (h / 2.0) * rot.cos();
                }
                Motion::Bounce => y -= p.size * 0.45 * a * (PI * ph).sin().abs(),
                Motion::Float => {
                    y += p.size * 0.18 * a * (TAU * ph).sin();
                    x += p.size * 0.08 * a * (TAU * ph).cos() * side;
                }
                Motion::Shake => x += p.size * 0.08 * a * (TAU * ph * 4.0).sin(),
                Motion::Spin => rot += TAU * ph * side,
                Motion::Twinkle => {
                    let t = 0.5 + 0.5 * (TAU * ph).cos();
                    alpha *= 0.25 + 0.75 * t;
                    h *= 0.85 + 0.15 * t;
                }
                Motion::Orbit => {
                    // Copies circle around (x, y), in perspective: bigger and brighter in front.
                    let ang = TAU * (ph + kp);
                    let r = 1.2 + 1.4 * a;
                    x = p.x * side + r * ang.cos();
                    y = p.y + r * 0.22 * ang.sin();
                    let front = 0.5 + 0.5 * ang.sin();
                    h *= 0.65 + 0.45 * front;
                    alpha *= 0.45 + 0.55 * front;
                }
                Motion::Rain | Motion::Rise => {
                    let t = (ph + kp).fract();
                    let dist = 1.0 + 2.5 * a;
                    let spread = (k as f32 * 0.618).fract() - 0.5;
                    x += spread * p.size * 1.2;
                    y += if p.motion == Motion::Rain { t * dist } else { -t * dist };
                    alpha *= if t < 0.15 { t / 0.15 } else { (1.0 - t) / 0.85 };
                    h *= if p.motion == Motion::Rise { 0.6 + 0.5 * t } else { 1.0 };
                }
                Motion::PopIn => {
                    // Grows in with a little overshoot, stays, shrinks away at the end.
                    let s = if ph < 0.25 {
                        let u = ph / 0.25;
                        1.0 + 1.70158 * 1.4 * (u - 1.0).powi(3) + 1.70158 * (u - 1.0).powi(2)
                    } else if ph > 0.9 {
                        (1.0 - ph) / 0.1
                    } else {
                        1.0
                    };
                    h *= s.max(0.0);
                }
            }
            out.push(Placed { x, y, h, rot, flip: p.flip ^ (side < 0.0), alpha: alpha.clamp(0.0, 1.0) });
        }
    }
}

/// Draw `f` (straight RGBA) centered at (cx, cy) px, `w`x`h` px, rotated, onto the canvas
/// (straight RGBA, "over").
#[allow(clippy::too_many_arguments)]
fn blit(canvas: &mut [u8], cw: usize, ch: usize, f: &ovc_mcam::Frame, cx: f32, cy: f32, w: f32, h: f32, rot: f32, flip: bool, alpha: f32) {
    if w < 1.0 || h < 1.0 || alpha <= 0.0 {
        return;
    }
    let (sn, cs) = rot.sin_cos();
    let (hw, hh) = (w / 2.0, h / 2.0);
    let ex = hw * cs.abs() + hh * sn.abs();
    let ey = hw * sn.abs() + hh * cs.abs();
    let (x0, x1) = ((cx - ex).floor().max(0.0) as usize, ((cx + ex).ceil().max(0.0) as usize).min(cw));
    let (y0, y1) = ((cy - ey).floor().max(0.0) as usize, ((cy + ey).ceil().max(0.0) as usize).min(ch));
    let (fw, fh) = (f.w as usize, f.h as usize);
    let (sx, sy) = (fw as f32 / w, fh as f32 / h);
    let sample = |u: f32, v: f32| -> [f32; 4] {
        // Bilinear, premultiplied.
        let (u, v) = ((u - 0.5).clamp(0.0, fw as f32 - 1.0), (v - 0.5).clamp(0.0, fh as f32 - 1.0));
        let (iu, iv) = (u as usize, v as usize);
        let (fu, fv) = (u - iu as f32, v - iv as f32);
        let (iu1, iv1) = ((iu + 1).min(fw - 1), (iv + 1).min(fh - 1));
        let mut acc = [0f32; 4];
        for (xx, yy, wt) in [(iu, iv, (1.0 - fu) * (1.0 - fv)), (iu1, iv, fu * (1.0 - fv)), (iu, iv1, (1.0 - fu) * fv), (iu1, iv1, fu * fv)] {
            let i = (yy * fw + xx) * 4;
            let a = f.rgba[i + 3] as f32 / 255.0 * wt;
            acc[0] += f.rgba[i] as f32 * a;
            acc[1] += f.rgba[i + 1] as f32 * a;
            acc[2] += f.rgba[i + 2] as f32 * a;
            acc[3] += a;
        }
        acc
    };
    for y in y0..y1 {
        let dy = y as f32 + 0.5 - cy;
        for x in x0..x1 {
            let dx = x as f32 + 0.5 - cx;
            let lx = dx * cs + dy * sn;
            let ly = -dx * sn + dy * cs;
            if lx.abs() > hw || ly.abs() > hh {
                continue;
            }
            let mut u = (lx + hw) * sx;
            if flip {
                u = fw as f32 - u;
            }
            let v = (ly + hh) * sy;
            let s = sample(u, v);
            let a = s[3] * alpha;
            if a <= 0.001 {
                continue;
            }
            let i = (y * cw + x) * 4;
            let da = canvas[i + 3] as f32 / 255.0;
            let oa = a + da * (1.0 - a);
            for c in 0..3 {
                let src = s[c] / s[3].max(1e-6);
                let dst = canvas[i + c] as f32;
                canvas[i + c] = ((src * a + dst * da * (1.0 - a)) / oa.max(1e-6)).round().clamp(0.0, 255.0) as u8;
            }
            canvas[i + 3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// The rendered effect: frames plus where it sits on the face (center, size in face units).
pub struct Rendered {
    pub frames: Vec<ovc_mcam::Frame>,
    pub center: [f32; 2],
    pub size: [f32; 2],
}

/// Render the project. `px_per_unit` sets the resolution; `one_frame` renders only the
/// first frame (fast, while dragging).
pub fn render(project: &Project, sprites: &HashMap<PathBuf, Sprite>, files: &[Option<PathBuf>], px_per_unit: f32, one_frame: bool) -> Option<Rendered> {
    let parts: Vec<(&Part, &Sprite)> = project
        .parts
        .iter()
        .zip(files)
        .filter(|(p, _)| p.on)
        .filter_map(|(p, f)| Some((p, sprites.get(f.as_ref()?)?)))
        .collect();
    if parts.is_empty() {
        return None;
    }
    let animated = parts.iter().any(|(p, s)| p.motion != Motion::None || s.frames.len() > 1);
    let loop_ms = if animated {
        let longest = parts.iter().map(|(_, s)| s.total_ms).max().unwrap_or(0) as f32;
        if longest > 0.0 {
            longest.clamp(1200.0, 3000.0)
        } else {
            1600.0
        }
    } else {
        0.0
    };
    let n = if !animated || one_frame { 1 } else { (loop_ms / 33.3).round().max(2.0) as usize };
    // Bounds over all frames.
    let (mut bx0, mut by0, mut bx1, mut by1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut placed = Vec::new();
    let mut per_frame: Vec<Vec<(usize, Placed)>> = Vec::with_capacity(n);
    for i in 0..n {
        let phase = i as f32 / n as f32;
        let mut fr = Vec::new();
        for (pi, (p, s)) in parts.iter().enumerate() {
            placed.clear();
            placements(p, phase, &mut placed);
            for q in placed.drain(..) {
                let w = q.h * s.aspect();
                let (sn, cs) = q.rot.sin_cos();
                let ex = w / 2.0 * cs.abs() + q.h / 2.0 * sn.abs();
                let ey = w / 2.0 * sn.abs() + q.h / 2.0 * cs.abs();
                (bx0, by0, bx1, by1) = (bx0.min(q.x - ex), by0.min(q.y - ey), bx1.max(q.x + ex), by1.max(q.y + ey));
                fr.push((pi, q));
            }
        }
        per_frame.push(fr);
    }
    if bx1 <= bx0 || by1 <= by0 {
        return None;
    }
    let (uw, uh) = (bx1 - bx0, by1 - by0);
    let ppu = px_per_unit.min(1024.0 / uw.max(uh));
    let (cw, ch) = (((uw * ppu).ceil() as usize).max(2), ((uh * ppu).ceil() as usize).max(2));
    let mut frames = Vec::with_capacity(n);
    let delay = if n > 1 { (loop_ms / n as f32).round() as u32 } else { 100 };
    for (i, fr) in per_frame.iter().enumerate() {
        let ms = i as f32 / n.max(1) as f32 * loop_ms;
        let mut canvas = vec![0u8; cw * ch * 4];
        for (pi, q) in fr {
            let s = parts[*pi].1;
            let f = s.at(ms);
            let w = q.h * s.aspect();
            blit(&mut canvas, cw, ch, f, (q.x - bx0) * ppu, (q.y - by0) * ppu, w * ppu, q.h * ppu, q.rot, q.flip, q.alpha);
        }
        frames.push(ovc_mcam::Frame { rgba: canvas, w: cw as u32, h: ch as u32, delay_ms: delay });
    }
    // Size in face units matching the canvas exactly (it was rounded up to whole pixels).
    Some(Rendered { frames, center: [bx0 + cw as f32 / ppu / 2.0, by0 + ch as f32 / ppu / 2.0], size: [cw as f32 / ppu, ch as f32 / ppu] })
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Saving, sharing
// ─────────────────────────────────────────────────────────────────────────────────────

fn write_apng(path: &Path, frames: &[ovc_mcam::Frame]) -> anyhow::Result<()> {
    let f0 = frames.first().ok_or_else(|| anyhow::anyhow!("nothing to save"))?;
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, f0.w, f0.h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    if frames.len() > 1 {
        enc.set_animated(frames.len() as u32, 0)?;
    }
    let mut wr = enc.write_header()?;
    for f in frames {
        if frames.len() > 1 {
            wr.set_frame_delay(f.delay_ms.min(65535) as u16, 1000)?;
        }
        wr.write_image_data(&f.rgba)?;
    }
    wr.finish()?;
    Ok(())
}

/// Save into "My effects": `<name>.png` (+ `.face.json`, `.maker.json`). Pieces from files
/// are copied next to it (`.maker/<name>/`) so the project keeps working. Returns the image.
pub fn save(project: &mut Project, rendered: &Rendered) -> anyhow::Result<PathBuf> {
    let name = safe_name(&project.name);
    project.name = name.clone();
    let dir = projects_dir();
    std::fs::create_dir_all(&dir)?;
    let assets = assets_dir(&name);
    for part in &mut project.parts {
        if let Source::File(p) = &part.source {
            if !p.starts_with(&assets) && p.is_file() {
                std::fs::create_dir_all(&assets)?;
                let mut dst = assets.join(p.file_name().unwrap_or_default());
                let mut k = 2;
                while dst.exists() {
                    dst = assets.join(format!("{k}-{}", p.file_name().unwrap_or_default().to_string_lossy()));
                    k += 1;
                }
                std::fs::copy(p, &dst)?;
                part.source = Source::File(dst);
            }
        }
    }
    let img = dir.join(format!("{name}.png"));
    write_apng(&img, &rendered.frames)?;
    let face = serde_json::json!({ "center": rendered.center, "size": rendered.size });
    std::fs::write(dir.join(format!("{name}.face.json")), serde_json::to_vec_pretty(&face)?)?;
    std::fs::write(dir.join(format!("{name}.maker.json")), serde_json::to_vec_pretty(project)?)?;
    Ok(img)
}

/// One file to share: the effect, its placement, the project and its pieces.
pub fn export(project: &Project, to: &Path) -> anyhow::Result<()> {
    use std::io::Write;
    let name = safe_name(&project.name);
    let dir = projects_dir();
    let mut zip = zip::ZipWriter::new(std::fs::File::create(to)?);
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for f in [format!("{name}.png"), format!("{name}.face.json")] {
        zip.start_file(f.clone(), opt)?;
        zip.write_all(&std::fs::read(dir.join(&f))?)?;
    }
    // The project with pieces referenced by bare file name.
    let mut p = project.clone();
    for part in &mut p.parts {
        if let Source::File(src) = &part.source {
            let fname = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
            zip.start_file(format!("pieces/{fname}"), opt)?;
            zip.write_all(&std::fs::read(src)?)?;
            part.source = Source::File(PathBuf::from(fname));
        }
    }
    zip.start_file(format!("{name}.maker.json"), opt)?;
    zip.write_all(&serde_json::to_vec_pretty(&p)?)?;
    zip.finish()?;
    Ok(())
}

/// Import a `.markcam` file into "My effects"; returns the effect image.
pub fn import(file: &Path) -> anyhow::Result<PathBuf> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(file)?)?;
    let dir = projects_dir();
    std::fs::create_dir_all(&dir)?;
    let mut project: Option<Project> = None;
    let mut image = None;
    let mut name = String::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let fname = f.name().to_string();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut bytes)?;
        if let Some(n) = fname.strip_suffix(".maker.json") {
            name = safe_name(n);
            project = serde_json::from_slice(&bytes).ok();
        } else if let Some(piece) = fname.strip_prefix("pieces/") {
            let n = file.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let a = assets_dir(&n);
            std::fs::create_dir_all(&a)?;
            std::fs::write(a.join(safe_file(piece)), &bytes)?;
        } else if !fname.contains('/') && !fname.contains('\\') {
            let out = dir.join(safe_file(&fname));
            std::fs::write(&out, &bytes)?;
            if fname.ends_with(".png") {
                image = Some(out);
            }
        }
    }
    if let Some(mut p) = project {
        let a = assets_dir(&file.file_stem().unwrap_or_default().to_string_lossy());
        for part in &mut p.parts {
            if let Source::File(src) = &part.source {
                part.source = Source::File(a.join(safe_file(&src.to_string_lossy())));
            }
        }
        if name.is_empty() {
            name = p.name.clone();
        }
        std::fs::write(dir.join(format!("{name}.maker.json")), serde_json::to_vec_pretty(&p)?)?;
    }
    image.ok_or_else(|| anyhow::anyhow!("not a MarkCam effect file"))
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Photo cut-out
// ─────────────────────────────────────────────────────────────────────────────────────

/// Cut the background out of a photo: `person` uses the portrait AI model (people, faces),
/// otherwise the color around the edges is taken as the background (logos, drawings,
/// objects on a plain background). Saves a PNG in the emoji cache and returns it.
pub fn cut_out(path: &Path, person: bool) -> anyhow::Result<PathBuf> {
    let img = image::open(path)?.to_rgba8();
    let img = {
        let long = img.width().max(img.height());
        if long > 900 {
            let s = 900.0 / long as f32;
            image::imageops::resize(&img, (img.width() as f32 * s) as u32, (img.height() as f32 * s) as u32, image::imageops::FilterType::Triangle)
        } else {
            img
        }
    };
    let (w, h) = img.dimensions();
    let mut rgba = img.into_raw();
    if person {
        let dir = crate::vbg::runtime_dir().ok_or_else(|| anyhow::anyhow!("ONNX Runtime is missing"))?;
        let model = crate::vbg::model_path().ok_or_else(|| anyhow::anyhow!("the background model is missing"))?;
        ovc_ml::seg::init_runtime(&dir)?;
        let (mw, mh) = (512usize, 512usize);
        let mut seg = ovc_ml::seg::PersonSegmenter::load(&model, mw, mh, false, 2)?;
        let m = seg.matte(&rgba, w, h)?;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let (fx, fy) = ((x as f32 + 0.5) / w as f32 * mw as f32 - 0.5, (y as f32 + 0.5) / h as f32 * mh as f32 - 0.5);
                let (ix, iy) = (fx.clamp(0.0, mw as f32 - 1.0) as usize, fy.clamp(0.0, mh as f32 - 1.0) as usize);
                let (ix1, iy1) = ((ix + 1).min(mw - 1), (iy + 1).min(mh - 1));
                let (tx, ty) = ((fx - ix as f32).clamp(0.0, 1.0), (fy - iy as f32).clamp(0.0, 1.0));
                let a = m[iy * mw + ix] * (1.0 - tx) * (1.0 - ty) + m[iy * mw + ix1] * tx * (1.0 - ty) + m[iy1 * mw + ix] * (1.0 - tx) * ty + m[iy1 * mw + ix1] * tx * ty;
                let i = (y * w as usize + x) * 4 + 3;
                rgba[i] = ((rgba[i] as f32) * a.clamp(0.0, 1.0)) as u8;
            }
        }
    } else {
        // Background = median color of the border pixels; alpha by color distance.
        let mut border: Vec<[u8; 3]> = Vec::new();
        for x in 0..w as usize {
            for y in [0, h as usize - 1] {
                let i = (y * w as usize + x) * 4;
                border.push([rgba[i], rgba[i + 1], rgba[i + 2]]);
            }
        }
        for y in 0..h as usize {
            for x in [0, w as usize - 1] {
                let i = (y * w as usize + x) * 4;
                border.push([rgba[i], rgba[i + 1], rgba[i + 2]]);
            }
        }
        let med = |c: usize| {
            let mut v: Vec<u8> = border.iter().map(|p| p[c]).collect();
            v.sort_unstable();
            v[v.len() / 2] as f32
        };
        let bg = [med(0), med(1), med(2)];
        for px in rgba.chunks_exact_mut(4) {
            let d = ((px[0] as f32 - bg[0]).powi(2) + (px[1] as f32 - bg[1]).powi(2) + (px[2] as f32 - bg[2]).powi(2)).sqrt();
            let a = ((d - 30.0) / 40.0).clamp(0.0, 1.0);
            px[3] = (px[3] as f32 * a) as u8;
        }
    }
    // Crop to what's left.
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if rgba[((y * w + x) * 4 + 3) as usize] > 12 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        anyhow::bail!("nothing left after removing the background");
    }
    let full = image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| anyhow::anyhow!("image"))?;
    let crop = image::imageops::crop_imm(&full, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image();
    let out_dir = emoji_cache().join("photos");
    std::fs::create_dir_all(&out_dir)?;
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let out = out_dir.join(format!("{}-cut.png", safe_name(&stem)));
    crop.save(&out)?;
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────────────
// The Effect Maker window
// ─────────────────────────────────────────────────────────────────────────────────────

pub enum MakerEvent {
    /// The effect changed: show this on the face (None: nothing to show).
    Preview(Option<Rendered>),
    /// Saved to "My effects" (the image), to list and use.
    Saved(PathBuf),
    Closed,
}

/// What the "Add" area shows.
#[derive(PartialEq)]
enum AddMode {
    None,
    Emoji,
}

pub struct Maker {
    pub project: Project,
    pub selected: Option<usize>,
    sprites: HashMap<PathBuf, Sprite>,
    files: Vec<Option<PathBuf>>,
    fetch: Fetcher,
    thumbs: HashMap<String, Option<egui::TextureHandle>>,
    add: AddMode,
    query: String,
    animated_only: bool,
    /// Re-render after this moment (debounced while sliders move).
    dirty_at: Option<std::time::Instant>,
    /// Being dragged on the preview: cheap one-frame renders.
    pub dragging: bool,
    status: String,
    /// Waiting for downloads to show the preview.
    waiting: bool,
    cutting: Option<std::sync::mpsc::Receiver<anyhow::Result<PathBuf>>>,
    saved_project: Option<Project>,
}

impl Maker {
    pub fn new() -> Self {
        Maker {
            project: Project { name: "My effect".into(), parts: Vec::new() },
            selected: None,
            sprites: HashMap::new(),
            files: Vec::new(),
            fetch: Fetcher::default(),
            thumbs: HashMap::new(),
            add: AddMode::Emoji,
            query: String::new(),
            animated_only: false,
            dirty_at: Some(std::time::Instant::now()),
            dragging: false,
            status: String::new(),
            waiting: false,
            cutting: None,
            saved_project: None,
        }
    }

    pub fn open(project: Project) -> Self {
        let mut m = Maker::new();
        m.saved_project = Some(project.clone());
        m.project = project;
        m.add = AddMode::None;
        m.selected = (!m.project.parts.is_empty()).then_some(0);
        m
    }

    pub fn touch(&mut self) {
        self.dirty_at = Some(std::time::Instant::now());
    }

    /// Move the selected part by a face-unit delta (dragging on the preview).
    pub fn nudge(&mut self, dx: f32, dy: f32) {
        if let Some(p) = self.selected.and_then(|i| self.project.parts.get_mut(i)) {
            p.x += dx;
            p.y += dy;
            self.touch();
        }
    }

    /// Resize the selected part (scroll wheel on the preview).
    pub fn grow(&mut self, k: f32) {
        if let Some(p) = self.selected.and_then(|i| self.project.parts.get_mut(i)) {
            p.size = (p.size * k).clamp(0.1, 14.0);
            self.touch();
        }
    }

    fn add_part(&mut self, src: Source, label: String) {
        let mut p = Part::new(src, label);
        // Next to the previous one, so new parts don't stack invisibly.
        if let Some(last) = self.project.parts.last() {
            p.x = last.x;
            p.y = last.y;
            p.size = last.size;
        }
        self.project.parts.push(p);
        self.selected = Some(self.project.parts.len() - 1);
        self.touch();
    }

    fn thumb(&mut self, ctx: &egui::Context, e: &Emoji) -> Option<egui::TextureHandle> {
        if let Some(t) = self.thumbs.get(&e.key) {
            return t.clone();
        }
        let file = self.fetch.emoji(e, false, ctx).or_else(|| if e.three.is_none() { self.fetch.emoji(e, true, ctx) } else { None })?;
        let tex = image::open(&file).ok().map(|img| {
            let img = img.thumbnail(72, 72).to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
            ctx.load_texture(format!("emoji:{}", e.key), ci, egui::TextureOptions::LINEAR)
        });
        self.thumbs.insert(e.key.clone(), tex.clone());
        tex
    }

    /// Make sure every part's source is downloaded and decoded; true when all are.
    fn ensure_sprites(&mut self, ctx: &egui::Context) -> bool {
        let mut all = true;
        self.files = self.project.parts.iter().map(|p| source_file(&p.source, &self.fetch, ctx)).collect();
        for f in self.files.iter().flatten() {
            if !self.sprites.contains_key(f) {
                match Sprite::load(f) {
                    Ok(s) => {
                        self.sprites.insert(f.clone(), s);
                    }
                    Err(e) => self.status = format!("Can't use {}: {e}", f.display()),
                }
            }
        }
        for (p, f) in self.project.parts.iter().zip(&self.files) {
            if f.is_none() {
                all = false;
                if let Source::Emoji { key, animated } = &p.source {
                    if self.fetch.failed(&format!("{}{}", if *animated { "a:" } else { "3:" }, key)) {
                        self.status = "A download failed — check the internet connection.".into();
                    }
                }
            }
        }
        all
    }

    /// The render to show / save (None when there is nothing yet).
    fn render_now(&mut self, ctx: &egui::Context, ppu: f32, one: bool) -> Option<Rendered> {
        self.waiting = !self.ensure_sprites(ctx);
        render(&self.project, &self.sprites, &self.files, ppu, one)
    }

    /// The window. Returns what the app should do.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<MakerEvent> {
        let mut ev = Vec::new();
        let mut open = true;
        let mut save = false;
        let mut export = false;
        egui::Window::new("Effect Maker")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_width(420.0)
            .default_height(640.0)
            .default_pos(ctx.screen_rect().right_top() + egui::vec2(-436.0, 52.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.window_body(ui, ctx, &mut save, &mut export);
                });
            });
        // Finished photo cut-outs.
        if let Some(rx) = &self.cutting {
            if let Ok(r) = rx.try_recv() {
                self.cutting = None;
                match r {
                    Ok(p) => {
                        let label = p.file_stem().unwrap_or_default().to_string_lossy().trim_end_matches("-cut").to_string();
                        self.add_part(Source::File(p), label);
                        if let Some(part) = self.project.parts.last_mut() {
                            (part.x, part.y, part.size) = (0.0, 0.5, 4.0);
                        }
                        self.status.clear();
                    }
                    Err(e) => self.status = format!("Couldn't cut out the photo: {e}"),
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        // Re-render: right away while dragging (one frame), else 120 ms after the last change.
        let due = self.dirty_at.is_some_and(|t| t.elapsed() > std::time::Duration::from_millis(if self.dragging { 0 } else { 120 }));
        if due {
            let r = self.render_now(ctx, 90.0, self.dragging);
            ev.push(MakerEvent::Preview(r));
            self.dirty_at = if self.dragging || self.waiting { Some(std::time::Instant::now() + std::time::Duration::from_millis(if self.waiting { 300 } else { 0 })) } else { None };
            if !self.dragging && self.waiting {
                self.dirty_at = Some(std::time::Instant::now());
            }
        } else if self.dirty_at.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
        if save {
            if self.project.parts.is_empty() {
                self.status = "Add at least one piece first.".into();
            } else if let Some(r) = self.render_now(ctx, 120.0, false) {
                match save_project(&mut self.project, &r) {
                    Ok(img) => {
                        self.status = format!("Saved to My effects as \"{}\".", self.project.name);
                        self.saved_project = Some(self.project.clone());
                        ev.push(MakerEvent::Saved(img));
                    }
                    Err(e) => self.status = format!("Couldn't save: {e}"),
                }
            } else {
                self.status = "Still downloading the pieces — try again in a moment.".into();
            }
        }
        if export {
            if self.saved_project.as_ref() != Some(&self.project) {
                self.status = "Save first, then share.".into();
            } else if let Some(to) = rfd::FileDialog::new()
                .set_title("Share this effect")
                .set_file_name(format!("{}.markcam", safe_name(&self.project.name)))
                .add_filter("MarkCam effect", &["markcam"])
                .save_file()
            {
                self.status = match export_project(&self.project, &to) {
                    Ok(()) => format!("Saved {} — send it to anyone with MarkCam (Effects → Import).", to.display()),
                    Err(e) => format!("Couldn't export: {e}"),
                };
            }
        }
        if !open {
            ev.push(MakerEvent::Closed);
        }
        ev
    }

    fn window_body(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, save: &mut bool, export: &mut bool) {
        use crate::theme;
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut self.project.name).desired_width(170.0));
            egui::ComboBox::from_id_salt("maker-open").selected_text("Open…").width(90.0).show_ui(ui, |ui| {
                if ui.selectable_label(false, "➕ New effect").clicked() {
                    *self = Maker::new();
                }
                for (name, path) in list_projects() {
                    if ui.selectable_label(false, &name).clicked() {
                        match load_project(&path) {
                            Ok(p) => *self = Maker::open(p),
                            Err(e) => self.status = format!("Couldn't open: {e}"),
                        }
                    }
                }
            });
        });
        ui.label(egui::RichText::new("Build it from pieces, then drag the selected piece on the picture to place it (scroll = size). It follows your face.").small().color(theme::TEXT_DIM));

        // ── Pieces ──
        theme::section(ui, "Pieces");
        theme::card(ui, |ui| {
            if self.project.parts.is_empty() {
                ui.label(egui::RichText::new("No pieces yet — add an emoji, an image / GIF or a photo below.").color(theme::TEXT_DIM));
            }
            let mut action: Option<(usize, i32)> = None;
            for i in 0..self.project.parts.len() {
                let sel = self.selected == Some(i);
                ui.horizontal(|ui| {
                    let p = &mut self.project.parts[i];
                    if ui.checkbox(&mut p.on, "").changed() {
                        action = Some((i, 0));
                    }
                    let label = format!("{}{}{}", p.label, if p.pair { " (pair)" } else { "" }, if p.motion != Motion::None { " ★" } else { "" });
                    if ui.selectable_label(sel, label).clicked() {
                        self.selected = Some(i);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                            action = Some((i, 9));
                        }
                        if ui.small_button("📋").on_hover_text("Duplicate").clicked() {
                            action = Some((i, 5));
                        }
                        if ui.small_button("⏷").on_hover_text("Behind").clicked() {
                            action = Some((i, -1));
                        }
                        if ui.small_button("⏶").on_hover_text("In front").clicked() {
                            action = Some((i, 1));
                        }
                    });
                });
            }
            if let Some((i, a)) = action {
                let n = self.project.parts.len();
                match a {
                    9 => {
                        self.project.parts.remove(i);
                        self.selected = if self.project.parts.is_empty() { None } else { Some(i.min(self.project.parts.len() - 1)) };
                    }
                    5 => {
                        let mut c = self.project.parts[i].clone();
                        c.x += 0.4;
                        self.project.parts.insert(i + 1, c);
                        self.selected = Some(i + 1);
                    }
                    1 if i + 1 < n => {
                        self.project.parts.swap(i, i + 1);
                        self.selected = Some(i + 1);
                    }
                    -1 if i > 0 => {
                        self.project.parts.swap(i, i - 1);
                        self.selected = Some(i - 1);
                    }
                    _ => {}
                }
                self.touch();
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(self.add == AddMode::Emoji, "😀 Add emoji").clicked() {
                    self.add = if self.add == AddMode::Emoji { AddMode::None } else { AddMode::Emoji };
                }
                if ui.button("🖼 Add image / GIF").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Images and animations", &["png", "apng", "gif", "webp", "jpg", "jpeg"]).pick_file() {
                        let label = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                        self.add_part(Source::File(p), label);
                    }
                }
                ui.menu_button("📷 Photo, cut out", |ui| {
                    ui.label(egui::RichText::new("Removes the photo's background:").small());
                    for (label, person) in [("A person / face (AI)", true), ("Something on a plain background", false)] {
                        if ui.button(label).clicked() {
                            ui.close_menu();
                            if let Some(p) = rfd::FileDialog::new().add_filter("Photos", &["png", "jpg", "jpeg", "webp", "bmp"]).pick_file() {
                                let (tx, rx) = std::sync::mpsc::channel();
                                let ctx = ctx.clone();
                                std::thread::spawn(move || {
                                    let _ = tx.send(cut_out(&p, person));
                                    ctx.request_repaint();
                                });
                                self.cutting = Some(rx);
                                self.status = "Cutting out the photo…".into();
                            }
                        }
                    }
                });
            });
            if self.add == AddMode::Emoji {
                self.emoji_picker(ui, ctx);
            }
        });

        // ── The selected piece ──
        if let Some(i) = self.selected.filter(|&i| i < self.project.parts.len()) {
            let mut changed = false;
            theme::section(ui, "Where on the face");
            theme::card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (label, x, y, size, pair) in SPOTS {
                        if ui.button(label).clicked() {
                            let p = &mut self.project.parts[i];
                            (p.x, p.y, p.size, p.pair) = (x, y, size, pair);
                            changed = true;
                        }
                    }
                });
                let p = &mut self.project.parts[i];
                changed |= ui.add(egui::Slider::new(&mut p.x, -4.0..=4.0).text("Left / right")).changed();
                changed |= ui.add(egui::Slider::new(&mut p.y, -4.5..=4.0).text("Up / down")).changed();
                changed |= ui.add(egui::Slider::new(&mut p.size, 0.1..=10.0).logarithmic(true).text("Size")).changed();
                changed |= ui.add(egui::Slider::new(&mut p.rot, -180.0..=180.0).suffix("°").text("Turn")).changed();
                changed |= ui.add(egui::Slider::new(&mut p.opacity, 0.1..=1.0).text("Opacity")).changed();
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut p.flip, "Mirror").changed();
                    changed |= ui.checkbox(&mut p.pair, "Also on the other side").on_hover_text("Ears, horns, cheeks: a mirrored copy on the other side of the face").changed();
                });
                if let Source::Emoji { key, animated } = &mut p.source {
                    if find_emoji(key).is_some_and(|e| e.noto.is_some() && e.three.is_some()) {
                        ui.horizontal(|ui| {
                            ui.label("Look");
                            changed |= ui.selectable_value(animated, false, "3D").changed();
                            changed |= ui.selectable_value(animated, true, "Animated").changed();
                        });
                    }
                }
            });
            theme::section(ui, "Animation");
            theme::card(ui, |ui| {
                let p = &mut self.project.parts[i];
                egui::ComboBox::from_id_salt("maker-motion").selected_text(p.motion.name()).width(220.0).show_ui(ui, |ui| {
                    for m in Motion::ALL {
                        changed |= ui.selectable_value(&mut p.motion, m, m.name()).changed();
                    }
                });
                if p.motion != Motion::None {
                    ui.horizontal(|ui| {
                        ui.label("Speed");
                        for (v, l) in [(1u8, "Slow"), (2, "Normal"), (3, "Fast")] {
                            changed |= ui.selectable_value(&mut p.speed, v, l).changed();
                        }
                    });
                    changed |= ui.add(egui::Slider::new(&mut p.amount, 0.0..=1.0).text("Strength")).changed();
                    if p.motion.has_copies() {
                        changed |= ui.add(egui::Slider::new(&mut p.copies, 1..=12).text("How many")).changed();
                    }
                }
            });
            if changed {
                self.touch();
            }
        }

        ui.add_space(8.0);
        if !self.status.is_empty() {
            ui.label(egui::RichText::new(&self.status).color(theme::WARN));
        }
        if self.waiting || self.cutting.is_some() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label(if self.cutting.is_some() { "Cutting out the photo…" } else { "Downloading…" });
            });
        }
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(egui::RichText::new("💾 Save to My effects").strong()).fill(theme::ACCENT_DIM)).clicked() {
                *save = true;
            }
            if ui.button("📤 Share…").on_hover_text("Save as one .markcam file to send to colleagues").clicked() {
                *export = true;
            }
        });
    }

    fn emoji_picker(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search: cat, star, rain, crown, ear…").desired_width(230.0));
            ui.checkbox(&mut self.animated_only, "Animated only");
        });
        let hits = search(&self.query, self.animated_only, 80);
        let mut picked: Option<&Emoji> = None;
        egui::ScrollArea::vertical().id_salt("emoji-grid").max_height(220.0).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                for e in hits {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::click());
                    if ui.is_rect_visible(rect) {
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, 6.0, crate::theme::CARD_HOVER);
                        }
                        match self.thumb(ctx, e) {
                            Some(t) => {
                                egui::Image::new(egui::load::SizedTexture::new(t.id(), egui::vec2(40.0, 40.0))).paint_at(ui, rect.shrink(4.0));
                            }
                            None => {
                                ui.painter().circle_filled(rect.center(), 6.0, crate::theme::TEXT_DIM.gamma_multiply(0.4));
                            }
                        }
                        if e.noto.is_some() {
                            ui.painter().circle_filled(rect.right_top() + egui::vec2(-6.0, 6.0), 3.5, crate::theme::LIVE);
                        }
                    }
                    if resp.on_hover_text(format!("{}{}", e.name, if e.noto.is_some() { " (animated version too)" } else { "" })).clicked() {
                        picked = Some(e);
                    }
                }
            });
        });
        ui.label(egui::RichText::new("● = also animated. Emoji art: Microsoft Fluent (MIT), Google Noto (CC BY 4.0).").small().color(crate::theme::TEXT_DIM));
        if let Some(e) = picked {
            let animated = e.noto.is_some() && (self.animated_only || e.three.is_none());
            self.add_part(Source::Emoji { key: e.key.clone(), animated }, e.name.clone());
        }
    }
}

/// Save (see [`save`]); a separate name so the window method can call it.
fn save_project(p: &mut Project, r: &Rendered) -> anyhow::Result<PathBuf> {
    save(p, r)
}

fn export_project(p: &Project, to: &Path) -> anyhow::Result<()> {
    export(p, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_save_roundtrip() {
        let dir = std::env::temp_dir().join("markcam-maker-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A red 40x60 piece.
        let img = image::RgbaImage::from_fn(40, 60, |_, _| image::Rgba([255, 0, 0, 255]));
        let piece = dir.join("red.png");
        img.save(&piece).unwrap();
        let mut sprites = HashMap::new();
        sprites.insert(piece.clone(), Sprite::load(&piece).unwrap());
        let mut part = Part::new(Source::File(piece.clone()), "red".into());
        (part.x, part.y, part.size, part.pair, part.motion) = (1.0, 0.0, 1.0, true, Motion::Orbit);
        let project = Project { name: "t".into(), parts: vec![part] };
        let r = render(&project, &sprites, &[Some(piece.clone())], 60.0, false).expect("rendered");
        assert!(r.frames.len() > 10, "animated: {} frames", r.frames.len());
        assert!(r.size[0] > 2.0 && r.size[1] > 1.0, "size {:?}", r.size);
        // Something red was drawn in every frame.
        for f in &r.frames {
            assert!(f.rgba.chunks_exact(4).any(|p| p[0] > 200 && p[3] > 200));
        }
        let out = dir.join("t.png");
        write_apng(&out, &r.frames).unwrap();
        let back = ovc_mcam::load_image_effect(&out).unwrap();
        assert_eq!(back.frames.len(), r.frames.len());
        assert_eq!(safe_file("../x/My hat.png"), "My hat.png");
        assert!(!search("cat", false, 10).is_empty());
        assert!(search("star", true, 10).iter().all(|e| e.noto.is_some()));
    }

    #[test]
    #[ignore]
    fn photo_cut_out() {
        let p = std::path::PathBuf::from(std::env::var("OVC_CUT_PHOTO").expect("OVC_CUT_PHOTO"));
        for person in [true, false] {
            let out = cut_out(&p, person).unwrap();
            println!("{person}: {}", out.display());
            std::fs::copy(&out, p.with_file_name(format!("cut-{person}.png"))).unwrap();
        }
    }
}
