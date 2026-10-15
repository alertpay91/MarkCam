//! Procedural, realistic hair: thousands of thin shaded strands instead of flat shapes.
//!
//! In a face SVG, `<strands kind="…" …/>` is replaced by the generated strands (SVG paths,
//! in face units). Kinds and their attributes:
//! - `afro`: tight coils filling a rounded volume.
//! - `long`: hair from a side parting over the skull, falling to `length` (y), with
//!   `wave` (amplitude), `curl` (ends turning in, for a bob) and `fringe` (bangs).
//! - `beard` / `mustache` / `stubble`: strands rooted on the lower face, falling down and
//!   out, mouth left open; `length` scales them, `wave` makes them wavy (a Santa beard).
//!
//! Common: `color` (base), `tip` (lighter: lit parts and highlights), `seed`, `count`.
//! Lighting comes from the top left; strands are shaded by their position on the head
//! (a rounded volume), darker at the roots and underneath, with a sheen band.

use anyhow::{anyhow, Result};
use std::fmt::Write;

/// Replace every `<strands …/>` in `svg` with generated hair.
pub fn expand(svg: &str) -> Result<String> {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(i) = rest.find("<strands") {
        out.push_str(&rest[..i]);
        let j = rest[i..].find("/>").ok_or_else(|| anyhow!("unclosed <strands"))? + i;
        out.push_str(&generate(&rest[i..j])?);
        rest = &rest[j + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let i = tag.find(&key)? + key.len();
    let j = i + tag[i..].find('"')?;
    Some(tag[i..j].to_string())
}

fn num(tag: &str, name: &str, default: f32) -> f32 {
    attr(tag, name).and_then(|s| s.parse().ok()).unwrap_or(default)
}

fn color(tag: &str, name: &str, default: &str) -> Result<[f32; 3]> {
    let s = attr(tag, name).unwrap_or_else(|| default.to_string());
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return Err(anyhow!("bad color {s}"));
    }
    let c = |k: usize| u8::from_str_radix(&s[k..k + 2], 16).map(|v| v as f32 / 255.0);
    Ok([c(0)?, c(2)?, c(4)?])
}

/// xorshift64*; deterministic per seed so the art renders the same every time.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn f(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32) / (1u64 << 24) as f32
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
    /// Roughly normal, mean 0, sd 1.
    fn gauss(&mut self) -> f32 {
        (self.f() + self.f() + self.f() + self.f() - 2.0) * 1.73
    }
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn scale(c: [f32; 3], k: f32) -> [f32; 3] {
    [c[0] * k, c[1] * k, c[2] * k]
}

fn hex(c: [f32; 3]) -> String {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2]))
}

/// Light from the top left, slightly in front.
fn lambert(nx: f32, ny: f32) -> f32 {
    let nz = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
    let (lx, ly, lz) = (-0.45f32, -0.6f32, 0.66f32);
    (nx * lx + ny * ly + nz * lz).max(0.0)
}

/// SVG output: polyline strokes, split into a few pieces so a strand can taper and
/// change shade along its length.
struct Out {
    s: String,
    phase: f32,
}

impl Out {
    fn line(&mut self, pts: &[(f32, f32)], width: f32, c: [f32; 3], opacity: f32) {
        if pts.len() < 2 || opacity <= 0.01 {
            return;
        }
        self.s.push_str("<path d=\"M");
        for (k, (x, y)) in pts.iter().enumerate() {
            if k > 0 {
                self.s.push('L');
            }
            let _ = write!(self.s, "{x:.3} {y:.3}");
        }
        let _ = write!(self.s, "\" stroke=\"{}\" stroke-width=\"{width:.4}\"", hex(c));
        if opacity < 0.99 {
            let _ = write!(self.s, " stroke-opacity=\"{opacity:.2}\"");
        }
        self.s.push_str("/>");
    }

    /// A strand in `pieces` parts: width `w0` → `w1`, color/opacity from `shade(t)`.
    /// The piece boundaries are shifted per strand (`self.phase`) so neighbouring strands
    /// don't change shade at the same place (no visible facets).
    fn strand(&mut self, pts: &[(f32, f32)], pieces: usize, w0: f32, w1: f32, shade: &mut dyn FnMut(f32) -> ([f32; 3], f32)) {
        let n = pts.len();
        if n < 2 {
            return;
        }
        let pieces = pieces.clamp(1, n - 1);
        self.phase = (self.phase + 0.618_034).fract();
        let phase = self.phase;
        let bound = |p: usize| -> usize {
            if p == 0 {
                0
            } else if p >= pieces {
                n - 1
            } else {
                (((p as f32 - phase) / pieces as f32) * (n - 1) as f32).round().clamp(1.0, (n - 2) as f32) as usize
            }
        };
        for p in 0..pieces {
            let a = bound(p);
            let b = bound(p + 1).max(a + 1).min(n - 1);
            if a >= b {
                continue;
            }
            let t = ((a + b) as f32 * 0.5) / (n - 1) as f32;
            let (c, o) = shade(t);
            self.line(&pts[a..=b], w0 + (w1 - w0) * t, c, o);
        }
    }
}

fn generate(tag: &str) -> Result<String> {
    let kind = attr(tag, "kind").ok_or_else(|| anyhow!("<strands> needs kind"))?;
    let seed = num(tag, "seed", 1.0) as u64;
    let mut rng = Rng::new(seed);
    let base = color(tag, "color", "#3a2414")?;
    let tip = color(tag, "tip", &hex(scale(base, 1.8)))?;
    let mut out = Out { phase: 0.0, s: String::from("<g fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\">") };
    match kind.as_str() {
        "afro" => afro(tag, &mut rng, base, tip, &mut out),
        "long" => long(tag, &mut rng, base, tip, &mut out),
        "beard" | "mustache" | "stubble" => beard(tag, &kind, &mut rng, base, tip, &mut out),
        k => return Err(anyhow!("unknown strands kind {k}")),
    }
    out.s.push_str("</g>");
    Ok(out.s)
}

/// Tight coils in a rounded volume (center `cx cy`, radii `rx ry`), back to front, with a
/// dark core so no gaps show through.
fn afro(tag: &str, rng: &mut Rng, base: [f32; 3], tip: [f32; 3], out: &mut Out) {
    let (cx, cy) = (num(tag, "cx", 0.0), num(tag, "cy", -1.0));
    let (rx, ry) = (num(tag, "rx", 2.1), num(tag, "ry", 1.95));
    let count = num(tag, "count", 14000.0) as usize;
    // Dark core, a little smaller than the volume.
    let _ = write!(
        out.s,
        "<ellipse cx=\"{cx}\" cy=\"{cy}\" rx=\"{:.3}\" ry=\"{:.3}\" fill=\"{}\" stroke=\"none\"/>",
        rx * 0.9,
        ry * 0.9,
        hex(scale(base, 0.45))
    );
    let mut coils: Vec<(f32, f32, f32)> = Vec::with_capacity(count);
    while coils.len() < count {
        let (x, y) = (rng.range(-1.0, 1.0), rng.range(-1.0, 1.0));
        let d2 = x * x + y * y;
        // A fuzzy edge: a few coils stick out past the outline.
        let lim = 1.0 + 0.06 * rng.f();
        if d2 > lim * lim {
            continue;
        }
        let nz = (1.0 - d2.min(1.0)).sqrt();
        coils.push((x, y, nz));
    }
    coils.sort_by(|a, b| a.2.total_cmp(&b.2));
    for (nx, ny, nz) in coils {
        let (x, y) = (cx + nx * rx, cy + ny * ry);
        let r = rng.range(0.035, 0.06);
        let turns = rng.range(1.3, 2.4);
        let a0 = rng.range(0.0, std::f32::consts::TAU);
        // Coils lean outward a little, like springs.
        let len = rng.range(0.02, 0.08);
        let (ox, oy) = (nx * len, ny * len);
        let steps = 14;
        let pts: Vec<(f32, f32)> = (0..=steps)
            .map(|k| {
                let t = k as f32 / steps as f32;
                let a = a0 + t * turns * std::f32::consts::TAU;
                let rr = r * (1.0 - 0.3 * t);
                (x + ox * t + a.cos() * rr, y + oy * t + a.sin() * rr * 0.8)
            })
            .collect();
        let light = lambert(nx * 0.95, ny * 0.95);
        let jitter = 1.0 + 0.18 * rng.gauss();
        // Coils deep inside (low nz near the edge are seen side-on) get a little darker.
        let k = (0.35 + 0.8 * light) * jitter * (0.75 + 0.25 * nz);
        let c = mix(scale(base, k), tip, (light - 0.55).max(0.0) * 1.2 * rng.f());
        out.line(&pts, rng.range(0.014, 0.022), c, 0.95);
    }
}

/// Long hair from a side parting (x = `part`) over the skull (an ellipse, center `cx cy`,
/// radii `rx ry`), leaving the head at the sides and falling to `length`.
///
/// Seen from the front, each strand runs from the parting to one side; `v` says how far
/// back on the head it lies: v = 0 follows the hairline over the forehead to the temple,
/// v = 1 runs along the outline of the head. Back strands are drawn first.
fn long(tag: &str, rng: &mut Rng, base: [f32; 3], tip: [f32; 3], out: &mut Out) {
    let (cx, cy) = (num(tag, "cx", 0.0), num(tag, "cy", -0.55));
    let (rx, ry) = (num(tag, "rx", 1.5), num(tag, "ry", 1.45));
    let length = num(tag, "length", 3.3);
    let wave = num(tag, "wave", 0.0);
    let curl = num(tag, "curl", 0.0);
    let fringe = attr(tag, "fringe").and_then(|s| s.parse::<f32>().ok());
    let part = num(tag, "part", -0.3);
    let count = num(tag, "count", 4200.0) as usize;
    let only_fringe = attr(tag, "only").as_deref() == Some("fringe");
    let sheen_y = cy - ry * 0.6;
    let lerp = |a: (f32, f32), b: (f32, f32), t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    // (layer, v, side): a darker back layer, the main hair, then a few flyaways.
    let mut strands: Vec<(u8, f32, f32)> = Vec::new();
    if !only_fringe {
        for _ in 0..count / 3 {
            strands.push((0, rng.range(0.75, 1.0), if rng.f() < 0.5 { -1.0 } else { 1.0 }));
        }
        let mut main: Vec<(u8, f32, f32)> = (0..count).map(|_| (1, rng.f().powf(0.9), if rng.f() < 0.5 { -1.0 } else { 1.0 })).collect();
        main.sort_by(|a, b| b.1.total_cmp(&a.1));
        strands.extend(main);
        for _ in 0..count / 60 {
            strands.push((2, rng.range(0.6, 1.0), if rng.f() < 0.5 { -1.0 } else { 1.0 }));
        }
    }
    for (layer, v, side) in strands {
        let lift = 1.0 + rng.range(0.0, 0.08) + if layer == 2 { 0.08 } else { 0.0 };
        let (sx, top) = (rx * lift, cy - ry * lift);
        let r0 = (part + rng.gauss() * 0.02, -1.1 + (top + 1.1) * v);
        let ctrl = lerp((side * 0.9 + part * 0.25, -1.2), (side * sx * 0.98, top + 0.1), v);
        let end = lerp((side * 1.22, -0.2), (side * sx, cy + 0.35), v);
        let end = (end.0 + rng.gauss() * 0.03, end.1 + rng.range(-0.05, 0.2));
        let mut pts = Vec::with_capacity(40);
        let arc_steps = 12;
        for k in 0..=arc_steps {
            let t = k as f32 / arc_steps as f32;
            let a = lerp(r0, ctrl, t);
            let b = lerp(ctrl, end, t);
            pts.push(lerp(a, b, t));
        }
        // Falling part.
        let (x0, y0) = end;
        let stop = length + rng.range(-0.3, 0.2) * if curl > 0.0 { 0.4 } else { 1.0 };
        let flare = rng.range(0.05, 0.25) * (0.6 + 0.4 * v);
        let phase = rng.range(0.0, 0.6) + side * 0.4;
        let tilt = rng.gauss() * 0.04;
        let fall_steps = 22;
        for k in 1..=fall_steps {
            let t = k as f32 / fall_steps as f32;
            let y = y0 + (stop - y0) * t;
            let mut x = x0 + side * flare * t.sqrt() + tilt * t + wave * ((y - y0) * 4.2 + phase).sin() * (t / 0.3).min(1.0);
            if curl > 0.0 && t > 0.75 {
                // Ends turn under, toward the face.
                let u = (t - 0.75) / 0.25;
                x -= side * curl * u * u;
            }
            pts.push((x, y));
        }
        if layer == 2 {
            // Flyaways: loose and irregular.
            let (a, b) = (rng.range(0.0, 6.28), rng.range(0.02, 0.06));
            for (k, p) in pts.iter_mut().enumerate() {
                let s = k as f32 * 0.45 + a;
                p.0 += s.sin() * b;
                p.1 += (s * 0.7).cos() * b * 0.5;
            }
        }
        let w = if layer == 2 { 0.006 } else { rng.range(0.011, 0.017) };
        let jitter = 1.0 + 0.15 * rng.gauss();
        let my_sheen_y = sheen_y + rng.gauss() * 0.12;
        let dark = if layer == 0 { 0.6 } else { 1.0 };
        let total = pts.len();
        let arc_frac = (arc_steps as f32 + 0.5) / total as f32;
        let pts_ref = &pts;
        let mut shade = |t: f32| {
            let (x, y) = pts_ref[((t * (total - 1) as f32) as usize).min(total - 1)];
            // 1 on the head, 0 once falling, blended smoothly between.
            let h = (1.0 - (t - arc_frac + 0.06) / 0.12).clamp(0.0, 1.0);
            let (nx, ny) = (((x - cx) / (rx * 1.12)).clamp(-0.98, 0.98), ((y - cy) / (ry * 1.12)).clamp(-0.98, 0.98));
            let k_head = 0.45 + 0.75 * lambert(nx, ny);
            // Falling: a little shadow just under the head.
            let k_fall = (0.6 + 0.2 * ((y - y0) / 0.6).clamp(0.0, 1.0)) * (0.8 + 0.2 * (1.0 - v));
            let k = (k_fall + (k_head - k_fall) * h) * jitter * dark;
            let s_head = (-((y - my_sheen_y) / 0.3).powi(2)).exp() * (1.0 - ((x - cx) / rx).abs()).max(0.0);
            let s_fall = if wave > 0.0 { (((y - y0) * 4.2 + phase).cos().max(0.0)).powi(6) * 0.6 } else { 0.15 };
            let sheen = s_fall + (s_head - s_fall) * h;
            let c = mix(scale(base, k), tip, sheen * 0.8 * dark);
            (c, if layer == 2 { 0.3 } else if t > 0.9 { 0.7 } else { 0.95 })
        };
        out.strand(&pts, 16, w, w * 0.45, &mut shade);
    }
    if let Some(fringe) = fringe {
        // Bangs: strands from the front of the head over the forehead, down to y = fringe.
        for _ in 0..count / 3 {
            let x0 = rng.range(-1.1, 1.1);
            let top = cy - ry * (1.0 - (x0 / rx).powi(2)).max(0.0).sqrt() * 0.92;
            let stop = fringe + rng.range(-0.07, 0.05) + 0.35 * (x0 / 1.1).powi(2);
            let bend = rng.range(0.5, 1.5) * 0.08 * ((x0 - part) / 1.1).clamp(-1.0, 1.0);
            let pts: Vec<(f32, f32)> = (0..=12)
                .map(|k| {
                    let t = k as f32 / 12.0;
                    (x0 + bend * t * t + x0 * 0.1 * t, top + (stop - top) * t)
                })
                .collect();
            let jitter = 1.0 + 0.15 * rng.gauss();
            let w = rng.range(0.011, 0.016);
            let mut shade = |t: f32| {
                let k = (0.5 + 0.6 * lambert((x0 / rx) * 0.8, -0.5 + t * 0.6)) * jitter;
                (mix(scale(base, k), tip, (1.0 - t) * 0.3), if t > 0.85 { 0.7 } else { 0.95 })
            };
            out.strand(&pts, 4, w, w * 0.5, &mut shade);
        }
    }
}
/// Beard / mustache / stubble on the lower face. Face geometry (face units): mouth at
/// (0, 1.27), nose bottom ~0.65, jaw at x ≈ ±1.35, chin ≈ 2.25.
fn beard(tag: &str, kind: &str, rng: &mut Rng, base: [f32; 3], tip: [f32; 3], out: &mut Out) {
    let length = num(tag, "length", 1.0);
    let wave = num(tag, "wave", 0.0);
    let count = num(tag, "count", if kind == "mustache" { 2200.0 } else if kind == "stubble" { 16000.0 } else { 9000.0 }) as usize;
    let in_face = |x: f32, y: f32| (x / 1.33).powi(2) + ((y - 0.75) / 1.55).powi(2) < 1.0;
    let in_mouth = |x: f32, y: f32| (x / 0.4).powi(2) + ((y - 1.27) / 0.15).powi(2) < 1.0;
    // Upper edge of the beard on the cheeks: from the sideburn (x 1.3, y 0.25) down to
    // the mouth corner (x 0.55, y 1.0); above the lip, the mustache from y 0.72.
    let cheek = |ax: f32| if ax > 0.55 { 1.0 - (ax - 0.55) / 0.75 * 0.75 } else { 0.72 };
    let mustache_zone = |x: f32, y: f32| x.abs() < 0.62 && y < 1.15;
    let mut roots: Vec<(f32, f32)> = Vec::with_capacity(count);
    let mut guard = 0;
    while roots.len() < count && guard < count * 200 {
        guard += 1;
        let (x, y) = (rng.range(-1.35, 1.35), rng.range(0.2, 2.35));
        if !in_face(x, y) || in_mouth(x, y) {
            continue;
        }
        // Thinning out toward the upper edge on the cheeks (no hard line).
        let above = (y - cheek(x.abs())) / 0.18;
        if above < 0.0 || rng.f() > above.min(1.0) {
            continue;
        }
        let m = mustache_zone(x, y);
        if kind == "mustache" && !(m && y < 1.12) {
            continue;
        }
        roots.push((x, y));
    }
    // Sides first, front (center) last; mustache on top of everything.
    roots.sort_by(|a, b| {
        let ka = if mustache_zone(a.0, a.1) { 9.0 } else { -a.0.abs() };
        let kb = if mustache_zone(b.0, b.1) { 9.0 } else { -b.0.abs() };
        ka.total_cmp(&kb)
    });
    if kind == "stubble" {
        // A faint shadow where the stubble grows.
        for &(x, y) in roots.iter().step_by(3) {
            out.line(&[(x, y), (x, y + 0.01)], 0.07, scale(base, 0.9), 0.05);
        }
    } else {
        // A soft dark underlayer so the skin doesn't show between strands.
        for &(x, y) in roots.iter().step_by(4) {
            let m = mustache_zone(x, y);
            let (dx, dy) = if m { (x.signum() * 0.8, 0.6) } else { (-x * 0.2, 1.0) };
            let l = 0.08 * length;
            out.line(&[(x, y), (x + dx * l, y + dy * l)], 0.05, scale(base, 0.5), 0.35);
        }
    }
    for (x, y) in roots {
        let m = mustache_zone(x, y);
        let (mut dx, mut dy) = if m { (x.signum() * 0.85 + rng.gauss() * 0.15, 0.55) } else { (-x * 0.22 + rng.gauss() * 0.12, 1.0) };
        let n = (dx * dx + dy * dy).sqrt();
        dx /= n;
        dy /= n;
        let len = match kind {
            "stubble" => rng.range(0.01, 0.022),
            "mustache" => rng.range(0.12, 0.24),
            _ if m => rng.range(0.1, 0.2) * length.sqrt(),
            // Longer toward the chin, so the beard has a rounded bottom.
            _ => (0.16 + 0.32 * ((y - 0.6) / 1.6).clamp(0.0, 1.0)) * length * rng.range(0.8, 1.2),
        };
        let steps = if kind == "stubble" { 1 } else { 8 };
        let phase = rng.range(0.0, 6.28);
        let bendx = rng.gauss() * 0.25;
        let pts: Vec<(f32, f32)> = (0..=steps)
            .map(|k| {
                let t = k as f32 / steps as f32;
                // Hair curves down under its weight, each strand a little its own way.
                let gy = 0.25 * t * t * len;
                let wv = wave * (t * 9.0 + phase).sin() * t * len + bendx * t * t * len;
                (x + dx * len * t + dy * wv, y + dy * len * t + gy - dx * wv)
            })
            .collect();
        let (nx, ny) = ((x / 1.4).clamp(-0.98, 0.98), ((y - 0.9) / 1.6).clamp(-0.98, 0.98));
        let light = lambert(nx, ny * 0.7);
        let jitter = 1.0 + 0.2 * rng.gauss();
        let grey = if rng.f() < 0.04 { 0.6 } else { 0.0 };
        if kind == "stubble" {
            out.line(&pts, 0.009, scale(base, jitter), 0.28);
            continue;
        }
        let w = if m { 0.015 } else { rng.range(0.009, 0.015) };
        let mut shade = |t: f32| {
            let k = (0.45 + 0.7 * light) * jitter * (0.8 + 0.25 * t);
            let c = mix(mix(scale(base, k), tip, (light - 0.5).max(0.0) * t), [0.75, 0.72, 0.7], grey);
            (c, if t > 0.8 { 0.8 } else { 0.95 })
        };
        out.strand(&pts, 3, w, w * 0.4, &mut shade);
    }
}
