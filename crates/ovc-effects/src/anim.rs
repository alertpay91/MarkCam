//! Procedural animated overlays (ManyCam's "Fire", "Matrix"… re-created): drawn with code
//! every frame, so they are smooth at any resolution and need no video files.
//!
//! Each kind keeps its own small state (a heat grid, rain columns, particles). Sizes are
//! relative to the frame height so they look the same at 720p and 1080p. All drawing is
//! clipped to the frame; nothing indexes out of bounds.

use crate::cpu::ColorMatrix;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnimKind {
    Fire,
    Matrix,
    Snow,
    Confetti,
    Hearts,
    Bubbles,
    Rain,
    Stars,
    Balloons,
    Leaves,
    Fireflies,
    Storm,
    Wind,
    Sunshine,
    Fog,
}

impl AnimKind {
    pub const ALL: [AnimKind; 15] = [
        AnimKind::Fire,
        AnimKind::Matrix,
        AnimKind::Snow,
        AnimKind::Confetti,
        AnimKind::Hearts,
        AnimKind::Bubbles,
        AnimKind::Rain,
        AnimKind::Stars,
        AnimKind::Balloons,
        AnimKind::Leaves,
        AnimKind::Fireflies,
        AnimKind::Storm,
        AnimKind::Wind,
        AnimKind::Sunshine,
        AnimKind::Fog,
    ];

    /// Weather effects (for a weather lesson): shown together in the effect list.
    pub fn is_weather(self) -> bool {
        matches!(self, AnimKind::Rain | AnimKind::Snow | AnimKind::Storm | AnimKind::Wind | AnimKind::Sunshine | AnimKind::Fog)
    }

    pub fn name(self) -> &'static str {
        match self {
            AnimKind::Fire => "Fire",
            AnimKind::Matrix => "Matrix",
            AnimKind::Snow => "Snow",
            AnimKind::Confetti => "Confetti",
            AnimKind::Hearts => "Hearts",
            AnimKind::Bubbles => "Bubbles",
            AnimKind::Rain => "Rain",
            AnimKind::Stars => "Twinkling stars",
            AnimKind::Balloons => "Balloons",
            AnimKind::Leaves => "Autumn leaves",
            AnimKind::Fireflies => "Fireflies",
            AnimKind::Storm => "Thunderstorm",
            AnimKind::Wind => "Wind",
            AnimKind::Sunshine => "Sunshine",
            AnimKind::Fog => "Fog",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            AnimKind::Fire => "fire",
            AnimKind::Matrix => "matrix",
            AnimKind::Snow => "snow",
            AnimKind::Confetti => "confetti",
            AnimKind::Hearts => "hearts",
            AnimKind::Bubbles => "bubbles",
            AnimKind::Rain => "rain",
            AnimKind::Stars => "stars",
            AnimKind::Balloons => "balloons",
            AnimKind::Leaves => "leaves",
            AnimKind::Fireflies => "fireflies",
            AnimKind::Storm => "storm",
            AnimKind::Wind => "wind",
            AnimKind::Sunshine => "sunshine",
            AnimKind::Fog => "fog",
        }
    }

    pub fn from_id(s: &str) -> Option<AnimKind> {
        AnimKind::ALL.into_iter().find(|k| k.id() == s)
    }
}

/// Small fast PRNG (xorshift32); quality is irrelevant here.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Uniform in `[a, b)`.
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (self.next() >> 8) as f32 / (1u32 << 24) as f32 * (b - a)
    }
}

/// Blend `color` over the pixel at `i` with `alpha` (0..255).
#[inline]
fn blend(px: &mut [u8], i: usize, color: [u8; 3], alpha: u32) {
    if alpha == 0 || i + 2 >= px.len() {
        return;
    }
    let a = alpha.min(255);
    for c in 0..3 {
        px[i + c] = div255(px[i + c] as u32 * (255 - a) + color[c] as u32 * a) as u8;
    }
}

/// `v / 255` (rounded) for v ≤ 255·255, without a division.
#[inline(always)]
fn div255(v: u32) -> u32 {
    let v = v + 128;
    (v + (v >> 8)) >> 8
}

/// "Screen" blend (brightens; for light: fire, glow).
#[inline]
fn screen(px: &mut [u8], i: usize, color: [u8; 3], alpha: u32) {
    if alpha == 0 || i + 2 >= px.len() {
        return;
    }
    for c in 0..3 {
        let b = px[i + c] as u32;
        let s = 255 - div255((255 - b) * (255 - color[c] as u32));
        px[i + c] = div255(b * (255 - alpha) + s * alpha) as u8;
    }
}

struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    size: f32,
    phase: f32,
    color: [u8; 3],
}

/// State + renderer for one animated overlay.
pub struct Anim {
    pub kind: AnimKind,
    rng: Rng,
    t: f32,
    // Fire: heat grid (0..=PALETTE-1), `fw` x `fh`, row 0 = top.
    heat: Vec<u8>,
    fw: usize,
    fh: usize,
    fire_acc: f32,
    // Matrix: per column head position (cells), speed (cells/s), trail length.
    cols: Vec<(f32, f32, f32)>,
    glyph_seed: Vec<u8>,
    // Particle kinds.
    parts: Vec<Particle>,
    size_key: (u32, u32),
    // Weather: rain drops / snowflakes / wind streaks in depth layers, splashes, lightning.
    drops: Vec<Drop>,
    splashes: Vec<(f32, f32, f32)>,
    flash: f32,
    bolt: Option<(Vec<(f32, f32)>, f32)>,
    next_bolt: f32,
    fog: Vec<f32>,
    lattice: Vec<f32>,
}

/// One rain drop / snowflake / wind streak. `depth` 0 = far … 1 = near (bigger, faster,
/// softer).
struct Drop {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    depth: f32,
    phase: f32,
}

const FIRE_LEVELS: usize = 37;

/// Doom-fire palette: black → red → orange → yellow → white, with alpha rising with heat.
fn fire_palette(level: usize) -> ([u8; 3], u32) {
    let t = level as f32 / (FIRE_LEVELS - 1) as f32;
    let (r, g, b) = if t < 0.35 {
        (t / 0.35 * 200.0, 0.0, 0.0)
    } else if t < 0.7 {
        (200.0 + (t - 0.35) / 0.35 * 55.0, (t - 0.35) / 0.35 * 150.0, 0.0)
    } else {
        (255.0, 150.0 + (t - 0.7) / 0.3 * 105.0, (t - 0.7) / 0.3 * 200.0)
    };
    ([r as u8, g as u8, b as u8], (t.powf(0.8) * 255.0) as u32)
}

/// 5x7 pseudo-glyph bitmap from a seed (Matrix rain "characters").
fn glyph_bits(seed: u8) -> u64 {
    let mut r = Rng(0x9E37_79B9 ^ (seed as u32).wrapping_mul(2_654_435_761));
    let mut bits = 0u64;
    for row in 0..7 {
        // Mirror-ish rows look more like characters than pure noise.
        let half = r.next() & 0b111;
        let mid = (r.next() & 1) as u64;
        let line = (half as u64) | (mid << 2) | (((half & 1) as u64) << 4) | ((((half >> 1) & 1) as u64) << 3);
        bits |= (line & 0x1F) << (row * 5);
    }
    bits
}

impl Anim {
    pub fn new(kind: AnimKind) -> Self {
        Anim {
            kind,
            rng: Rng(0x1234_5678 ^ kind as u32 * 7919),
            t: 0.0,
            heat: Vec::new(),
            fw: 0,
            fh: 0,
            fire_acc: 0.0,
            cols: Vec::new(),
            glyph_seed: Vec::new(),
            parts: Vec::new(),
            size_key: (0, 0),
            drops: Vec::new(),
            splashes: Vec::new(),
            flash: 0.0,
            bolt: None,
            next_bolt: 3.0,
            fog: Vec::new(),
            lattice: Vec::new(),
        }
    }

    /// Advance by `dt` seconds and draw onto `rgba` (`w*h*4`). `amount` 0..1 scales
    /// density / opacity.
    pub fn render(&mut self, rgba: &mut [u8], w: u32, h: u32, dt: f32, amount: f32) {
        let (wu, hu) = (w as usize, h as usize);
        if wu < 16 || hu < 16 || rgba.len() < wu * hu * 4 {
            return;
        }
        let dt = dt.clamp(0.0, 0.1);
        self.t += dt;
        let amount = amount.clamp(0.0, 1.0);
        if self.size_key != (w, h) {
            self.size_key = (w, h);
            self.heat.clear();
            self.cols.clear();
            self.parts.clear();
            self.drops.clear();
            self.splashes.clear();
            self.fog.clear();
        }
        match self.kind {
            AnimKind::Fire => self.fire(rgba, wu, hu, dt, amount),
            AnimKind::Matrix => self.matrix(rgba, wu, hu, dt, amount),
            AnimKind::Rain => self.rain(rgba, wu, hu, dt, amount, false),
            AnimKind::Storm => self.rain(rgba, wu, hu, dt, amount, true),
            AnimKind::Snow => self.snow(rgba, wu, hu, dt, amount),
            AnimKind::Wind => self.wind(rgba, wu, hu, dt, amount),
            AnimKind::Sunshine => self.sunshine(rgba, wu, hu, amount),
            AnimKind::Fog => self.fog(rgba, wu, hu, dt, amount),
            _ => self.particles(rgba, wu, hu, dt, amount),
        }
    }

    fn fire(&mut self, rgba: &mut [u8], w: usize, h: usize, dt: f32, amount: f32) {
        // Simulate on a coarse grid covering the bottom ~45% of the frame.
        const CELL: usize = 4;
        let (fw, fh) = (w / CELL + 2, (h * 45 / 100) / CELL);
        if self.heat.len() != fw * fh {
            self.fw = fw;
            self.fh = fh;
            self.heat = vec![0; fw * fh];
        }
        // Step the simulation at ~30 Hz regardless of the frame rate.
        self.fire_acc += dt;
        while self.fire_acc >= 1.0 / 30.0 {
            self.fire_acc -= 1.0 / 30.0;
            let base = (((FIRE_LEVELS - 1) as f32) * (0.55 + 0.45 * amount)) as u8;
            for x in 0..fw {
                // Flickering source row.
                let flick = (self.rng.next() % 4) as u8;
                self.heat[(fh - 1) * fw + x] = base.saturating_sub(flick);
            }
            for y in 1..fh {
                for x in 0..fw {
                    let src = self.heat[y * fw + x];
                    let r = self.rng.next();
                    let decay = (r & 3) as usize;
                    let dx = (x + fw + 1 - decay % 3) % fw; // slight random sideways drift
                    let cool = ((r >> 2) & 1) as u8 + if amount < 0.5 { 1 } else { 0 };
                    self.heat[(y - 1) * fw + dx] = src.saturating_sub(cool);
                }
            }
        }
        // Draw: bilinear-upscaled heat → palette → screen-blend (fire adds light). The heat
        // is interpolated once per 2x2 pixel block (flames are soft anyway), in Q8 integers.
        let top = h.saturating_sub(fh * CELL);
        let pal: Vec<([u8; 3], u32)> = (0..FIRE_LEVELS).map(fire_palette).collect();
        let heat = &self.heat;
        for y in (top..h).step_by(2) {
            let gy = ((y - top) * 256 / CELL).min((fh - 1) * 256);
            let (y0, fy) = (gy >> 8, (gy & 255) as u32);
            let y1 = (y0 + 1).min(fh - 1);
            for x in (0..w).step_by(2) {
                let gx = (x * 256 / CELL).min((fw - 2) * 256);
                let (x0, fx) = (gx >> 8, (gx & 255) as u32);
                let hv = |xx: usize, yy: usize| heat[yy * fw + xx] as u32;
                let top_v = hv(x0, y0) * (256 - fx) + hv(x0 + 1, y0) * fx;
                let bot_v = hv(x0, y1) * (256 - fx) + hv(x0 + 1, y1) * fx;
                let level = ((top_v * (256 - fy) + bot_v * fy) >> 16) as usize;
                if level < 2 {
                    continue;
                }
                let (c, a) = pal[level.min(FIRE_LEVELS - 1)];
                for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                    if y + dy < h && x + dx < w {
                        screen(rgba, ((y + dy) * w + x + dx) * 4, c, a);
                    }
                }
            }
        }
    }

    fn matrix(&mut self, rgba: &mut [u8], w: usize, h: usize, dt: f32, amount: f32) {
        // Tint the picture dark green first (one matrix pass), then draw the rain.
        let tint = ColorMatrix::saturation(0.0)
            .then(&ColorMatrix::channels([0.18, 0.62, 0.25], [0.0, 6.0, 0.0]))
            .mix(0.55 + 0.4 * amount);
        tint.apply_in_place(&mut rgba[..w * h * 4]);

        let cell = (h / 38).max(8);
        let dot = (cell / 7).max(1);
        let ncols = w / cell;
        let nrows = h / cell + 1;
        if self.cols.len() != ncols {
            self.cols = (0..ncols)
                .map(|_| (self.rng.range(-(nrows as f32), nrows as f32), self.rng.range(6.0, 16.0), self.rng.range(6.0, 22.0)))
                .collect();
            self.glyph_seed = (0..ncols * nrows).map(|_| (self.rng.next() & 0xFF) as u8).collect();
        }
        // Occasionally mutate glyphs (the flicker of the real thing).
        for _ in 0..(ncols / 3).max(1) {
            let i = self.rng.next() as usize % self.glyph_seed.len();
            self.glyph_seed[i] = (self.rng.next() & 0xFF) as u8;
        }
        let density = 0.35 + 0.65 * amount;
        for c in 0..ncols {
            // Some columns stay idle at lower density.
            if (c as f32 * 0.618).fract() > density {
                continue;
            }
            let (head, speed, len) = self.cols[c];
            let head = head + speed * dt;
            let head = if head - len > nrows as f32 { -self.rng.range(0.0, nrows as f32 * 0.5) } else { head };
            self.cols[c].0 = head;
            let hrow = head.floor() as isize;
            for k in 0..len as isize {
                let row = hrow - k;
                if row < 0 || row as usize >= nrows {
                    continue;
                }
                let fade = 1.0 - k as f32 / len;
                let color = if k == 0 { [210, 255, 210] } else { [40, (110.0 + 145.0 * fade) as u8, 70] };
                let alpha = (255.0 * fade.powf(0.6)) as u32;
                let bits = glyph_bits(self.glyph_seed[row as usize * ncols + c]);
                let (cx, cy) = (c * cell + (cell - 5 * dot) / 2, row as usize * cell);
                for gy in 0..7 {
                    for gx in 0..5 {
                        if bits >> (gy * 5 + gx) & 1 == 0 {
                            continue;
                        }
                        for py in 0..dot {
                            let y = cy + gy * dot + py;
                            if y >= h {
                                continue;
                            }
                            for px in 0..dot {
                                let x = cx + gx * dot + px;
                                if x < w {
                                    blend(rgba, (y * w + x) * 4, color, alpha);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn particles(&mut self, rgba: &mut [u8], w: usize, h: usize, dt: f32, amount: f32) {
        let s = h as f32 / 720.0; // scale reference
        let (count, rising) = match self.kind {
            AnimKind::Snow => ((60.0 + 200.0 * amount) as usize, false),
            AnimKind::Confetti => ((40.0 + 160.0 * amount) as usize, false),
            AnimKind::Hearts => ((8.0 + 32.0 * amount) as usize, true),
            AnimKind::Bubbles => ((10.0 + 40.0 * amount) as usize, true),
            AnimKind::Rain => ((80.0 + 260.0 * amount) as usize, false),
            AnimKind::Stars => ((15.0 + 45.0 * amount) as usize, false),
            AnimKind::Balloons => ((5.0 + 15.0 * amount) as usize, true),
            AnimKind::Leaves => ((10.0 + 30.0 * amount) as usize, false),
            AnimKind::Fireflies => ((12.0 + 40.0 * amount) as usize, true),
            _ => (0, false),
        };
        let (wf, hf) = (w as f32, h as f32);
        // Spawn / trim to the target count; new ones start spread over the frame the
        // first time, then enter from the edge.
        let first = self.parts.is_empty();
        while self.parts.len() < count {
            let p = self.spawn(wf, hf, s, first, rising);
            self.parts.push(p);
        }
        self.parts.truncate(count);

        let t = self.t;
        for i in 0..self.parts.len() {
            let p = &mut self.parts[i];
            p.x += (p.vx + (t * 1.3 + p.phase).sin() * 18.0 * s) * dt;
            p.y += p.vy * dt;
            let gone = if rising { p.y < -p.size * 2.0 } else { p.y > hf + p.size * 2.0 };
            if gone {
                let np = self.spawn(wf, hf, s, false, rising);
                self.parts[i] = np;
            }
        }
        for p in &self.parts {
            match self.kind {
                AnimKind::Snow => disc(rgba, w, h, p.x, p.y, p.size, p.color, 230, true),
                AnimKind::Confetti => {
                    // A flipping rectangle: its width follows cos(rotation).
                    let flip = (t * 4.0 + p.phase * 6.0).cos().abs().max(0.15);
                    rect(rgba, w, h, p.x, p.y, p.size * flip, p.size * 0.6, p.color, 240);
                }
                AnimKind::Hearts => {
                    // Fade out over the top third.
                    let a = ((p.y / (hf * 0.33)).clamp(0.0, 1.0) * 235.0) as u32;
                    heart(rgba, w, h, p.x, p.y, p.size, p.color, a);
                }
                AnimKind::Bubbles => ring(rgba, w, h, p.x, p.y, p.size, p.color),
                AnimKind::Rain => streak(rgba, w, h, p.x, p.y, p.vx, p.vy, p.size, p.color, 170),
                AnimKind::Stars => {
                    let twinkle = 0.55 + 0.45 * (t * 5.0 + p.phase * 3.0).sin();
                    sparkle(rgba, w, h, p.x, p.y, p.size * (0.7 + 0.3 * twinkle), p.color, (230.0 * twinkle) as u32);
                }
                AnimKind::Balloons => balloon(rgba, w, h, p.x, p.y, p.size, p.color),
                AnimKind::Leaves => {
                    let flip = (t * 2.5 + p.phase * 4.0).cos();
                    leaf(rgba, w, h, p.x, p.y, p.size, flip, p.color);
                }
                AnimKind::Fireflies => {
                    let glow = 0.4 + 0.6 * ((t * 2.0 + p.phase * 2.0).sin() * 0.5 + 0.5);
                    glow_dot(rgba, w, h, p.x, p.y, p.size, p.color, glow);
                }
                _ => {}
            }
        }
    }

    /// Rain in three depth layers (far: fine and faint; near: long, thick, soft), a cool,
    /// slightly darker grade (a rainy day), and splashes along the bottom. `storm` adds
    /// heavier rain, a darker sky and lightning.
    fn rain(&mut self, rgba: &mut [u8], w: usize, h: usize, dt: f32, amount: f32, storm: bool) {
        let (wf, hf) = (w as f32, h as f32);
        let s = hf / 720.0;
        let dark = if storm { 0.55 + 0.25 * amount } else { 0.25 + 0.25 * amount };
        let grade = ColorMatrix::saturation(1.0 - dark * 0.6)
            .then(&ColorMatrix::channels([1.0 - dark * 0.35, 1.0 - dark * 0.3, 1.0 - dark * 0.15], [0.0, 2.0, 8.0 * dark]));
        // Lightning lifts the whole picture for a moment.
        let lit = self.flash.clamp(0.0, 1.0);
        let grade = if lit > 0.0 { grade.then(&ColorMatrix::channels([1.0 + lit * 0.9, 1.0 + lit * 0.9, 1.0 + lit], [lit * 40.0, lit * 40.0, lit * 55.0])) } else { grade };
        grade.apply_in_place(&mut rgba[..w * h * 4]);

        let count = ((if storm { 900.0 } else { 520.0 }) * (0.4 + amount) * (wf / 1280.0)) as usize;
        let wind = if storm { -220.0 } else { -70.0 } * s;
        while self.drops.len() < count {
            let depth = self.rng.range(0.0, 1.0).powf(1.6);
            let x = self.rng.range(-0.1 * wf, 1.1 * wf);
            let y = if self.drops.is_empty() || self.drops.len() < count / 2 { self.rng.range(0.0, hf) } else { -self.rng.range(0.0, 200.0) * s };
            let vy = (650.0 + 900.0 * depth) * s * self.rng.range(0.85, 1.15);
            let phase = self.rng.range(0.0, 1.0);
            self.drops.push(Drop { x, y, vx: wind * (0.6 + depth), vy, depth, phase });
        }
        self.drops.truncate(count);
        for i in 0..self.drops.len() {
            let d = &mut self.drops[i];
            d.x += d.vx * dt;
            d.y += d.vy * dt;
            if d.y > hf + 40.0 * s {
                // Near drops splash at the bottom of the picture.
                if d.depth > 0.55 && self.splashes.len() < 60 {
                    self.splashes.push((d.x, hf - self.rng.range(4.0, 60.0) * s * d.depth, 0.0));
                }
                d.y = -self.rng.range(10.0, 150.0) * s;
                d.x = self.rng.range(-0.1 * wf, 1.1 * wf);
            }
        }
        for d in &self.drops {
            let len = (18.0 + 60.0 * d.depth) * s;
            let width = 1.0 + 2.6 * d.depth * s;
            let a = (80.0 + 120.0 * d.depth * (0.6 + 0.4 * amount)) as u32;
            let n = (d.vx * d.vx + d.vy * d.vy).sqrt().max(1.0);
            let (ux, uy) = (d.vx / n * len, d.vy / n * len);
            // Motion blur: bright head, fading tail.
            soft_line(rgba, w, h, d.x - ux, d.y - uy, d.x - ux * 0.5, d.y - uy * 0.5, width * 0.8, [205, 220, 240], a / 2);
            soft_line(rgba, w, h, d.x - ux * 0.5, d.y - uy * 0.5, d.x, d.y, width, [215, 228, 245], a);
        }
        // Splashes: little rings that widen and fade.
        for sp in &mut self.splashes {
            sp.2 += dt;
        }
        self.splashes.retain(|sp| sp.2 < 0.25);
        for &(x, y, age) in &self.splashes {
            let r = (3.0 + 30.0 * age) * s;
            let a = ((1.0 - age / 0.25) * 150.0) as u32;
            ellipse_ring(rgba, w, h, x, y, r, r * 0.35, [215, 230, 245], a);
        }
        if storm {
            self.next_bolt -= dt;
            if self.next_bolt <= 0.0 {
                self.next_bolt = self.rng.range(3.0, 8.0) * (1.4 - amount * 0.6);
                self.flash = 1.0;
                // A jagged bolt from the top, wandering down.
                let mut pts = vec![(self.rng.range(0.2 * wf, 0.8 * wf), 0.0)];
                let mut p = pts[0];
                while p.1 < hf * 0.75 {
                    p = (p.0 + self.rng.range(-60.0, 60.0) * s, p.1 + self.rng.range(30.0, 70.0) * s);
                    pts.push(p);
                }
                self.bolt = Some((pts, 0.0));
            }
            self.flash = (self.flash - dt * 3.5).max(0.0);
            if let Some((pts, age)) = &mut self.bolt {
                *age += dt;
                let a = ((1.0 - *age / 0.35).max(0.0) * 255.0) as u32;
                for seg in pts.windows(2) {
                    soft_line(rgba, w, h, seg[0].0, seg[0].1, seg[1].0, seg[1].1, 9.0 * s, [170, 190, 255], a / 3);
                    soft_line(rgba, w, h, seg[0].0, seg[0].1, seg[1].0, seg[1].1, 2.5 * s, [255, 255, 255], a);
                }
                if *age > 0.35 {
                    self.bolt = None;
                }
            }
        }
    }

    /// Snow in three depth layers: far flakes small and slow, near ones big and soft
    /// (out of focus), all swaying with a gentle wind; the picture a touch cooler/brighter.
    fn snow(&mut self, rgba: &mut [u8], w: usize, h: usize, dt: f32, amount: f32) {
        let (wf, hf) = (w as f32, h as f32);
        let s = hf / 720.0;
        ColorMatrix::channels([0.97, 1.0, 1.05], [6.0, 8.0, 12.0]).mix(0.6).apply_in_place(&mut rgba[..w * h * 4]);
        let count = (240.0 * (0.35 + amount) * (wf / 1280.0)) as usize;
        while self.drops.len() < count {
            let depth = self.rng.range(0.0, 1.0).powf(1.5);
            let anywhere = self.drops.len() < count / 2 || self.t < 0.1;
            let y = if anywhere { self.rng.range(0.0, hf) } else { -self.rng.range(5.0, 80.0) * s };
            let x = self.rng.range(0.0, wf);
            let phase = self.rng.range(0.0, 6.28);
            self.drops.push(Drop { x, y, vx: self.rng.range(-10.0, 10.0) * s, vy: (30.0 + 110.0 * depth) * s, depth, phase });
        }
        self.drops.truncate(count);
        let t = self.t;
        let gust = 25.0 * s * (t * 0.35).sin() + 12.0 * s;
        for d in &mut self.drops {
            d.x += (d.vx + gust * (0.4 + d.depth) + (t * 1.2 + d.phase).sin() * 22.0 * s * (0.5 + d.depth)) * dt;
            d.y += d.vy * dt;
            if d.y > hf + 30.0 * s || d.x > wf + 40.0 * s || d.x < -40.0 * s {
                d.y = -self.rng.range(5.0, 60.0) * s;
                d.x = self.rng.range(-0.1 * wf, wf);
            }
        }
        for d in &self.drops {
            let r = (1.5 + 9.0 * d.depth * d.depth) * s + 0.8;
            let a = (150.0 + 90.0 * (1.0 - d.depth * 0.5)) as u32;
            disc(rgba, w, h, d.x, d.y, r, [255, 255, 255], a, true);
        }
    }

    /// Wind: gusts (faint curved streaks rushing across) and leaves / bits blown sideways,
    /// strength rising and falling.
    fn wind(&mut self, rgba: &mut [u8], w: usize, h: usize, dt: f32, amount: f32) {
        let (wf, hf) = (w as f32, h as f32);
        let s = hf / 720.0;
        let t = self.t;
        let gust = 0.55 + 0.45 * (t * 0.6).sin() * (t * 0.23).cos();
        // Streaks.
        let count = (26.0 * (0.4 + amount)) as usize;
        while self.drops.len() < count {
            let depth = self.rng.range(0.0, 1.0);
            let first = self.drops.len() < count / 2 && t < 0.1;
            let x = if first { self.rng.range(0.0, wf) } else { -self.rng.range(50.0, 600.0) * s };
            let y = self.rng.range(0.0, hf);
            let phase = self.rng.range(0.0, 6.28);
            self.drops.push(Drop { x, y, vx: (900.0 + 900.0 * depth) * s, vy: 0.0, depth, phase });
        }
        self.drops.truncate(count);
        for d in &mut self.drops {
            d.x += d.vx * gust * dt;
            if d.x > wf + 500.0 * s {
                d.x = -self.rng.range(50.0, 700.0) * s;
                d.y = self.rng.range(0.0, hf);
            }
        }
        for d in &self.drops {
            let len = (160.0 + 260.0 * d.depth) * s;
            let a = ((70.0 + 90.0 * d.depth) * gust) as u32;
            // A gently curving streak: a few segments along a sine.
            let segs = 8;
            let mut prev = (d.x - len, d.y + (d.phase).sin() * 10.0 * s);
            for k in 1..=segs {
                let f = k as f32 / segs as f32;
                let x = d.x - len * (1.0 - f);
                let y = d.y + (d.phase + f * 3.0 + t * 2.0).sin() * 10.0 * s;
                let fade = (f * (1.0 - f) * 4.0).min(1.0);
                soft_line(rgba, w, h, prev.0, prev.1, x, y, (1.6 + 2.4 * d.depth) * s, [245, 248, 255], (a as f32 * fade) as u32);
                prev = (x, y);
            }
        }
        // Leaves riding the wind.
        let leaves = (10.0 + 18.0 * amount) as usize;
        if self.parts.len() < leaves {
            const C: [[u8; 3]; 4] = [[234, 88, 12], [250, 204, 21], [190, 120, 30], [132, 160, 40]];
            let color = C[self.rng.next() as usize % C.len()];
            let p = Particle {
                x: -self.rng.range(20.0, 400.0) * s,
                y: self.rng.range(0.0, hf * 0.9),
                vx: self.rng.range(260.0, 520.0) * s,
                vy: self.rng.range(-30.0, 50.0) * s,
                size: self.rng.range(28.0, 48.0) * s,
                phase: self.rng.range(0.0, 6.28),
                color,
            };
            self.parts.push(p);
        }
        for p in &mut self.parts {
            p.x += p.vx * (0.4 + gust) * dt;
            p.y += (p.vy + (t * 3.0 + p.phase).sin() * 60.0 * s) * dt;
        }
        self.parts.retain(|p| p.x < wf + 60.0 * s && p.y < hf + 60.0 * s);
        for p in &self.parts {
            pointed_leaf(rgba, w, h, p.x, p.y, p.size, t * 5.0 + p.phase, (t * 3.0 + p.phase).cos(), p.color);
        }
    }

    /// Sunshine: a warm, bright grade, the sun with a glow and slowly turning rays in the
    /// top-right corner, and a lens flare across the picture. The light is worked out on a
    /// coarse grid and smoothly stretched over the picture (cheap at 1080p).
    fn sunshine(&mut self, rgba: &mut [u8], w: usize, h: usize, amount: f32) {
        let (wf, hf) = (w as f32, h as f32);
        let t = self.t;
        ColorMatrix::channels([1.06, 1.02, 0.92], [8.0, 4.0, -4.0]).mix(0.4 + 0.5 * amount).apply_in_place(&mut rgba[..w * h * 4]);
        let (sx, sy) = (wf * 0.9, hf * 0.04);
        let reach = (wf * wf + hf * hf).sqrt();
        let shimmer = 0.92 + 0.08 * (t * 2.3).sin();
        let k = (0.6 + 0.4 * amount) * shimmer;
        let (mw, mh) = (w / LIGHT_CELL + 2, h / LIGHT_CELL + 2);
        self.fog.resize(mw * mh, 0.0);
        for j in 0..mh {
            for i in 0..mw {
                let (dx, dy) = ((i * LIGHT_CELL) as f32 - sx, (j * LIGHT_CELL) as f32 - sy);
                let d = (dx * dx + dy * dy).sqrt() / reach;
                let fall = (1.0 - d * 1.2).max(0.0);
                let a = if fall <= 0.0 {
                    0.0
                } else {
                    let r = (dy.atan2(dx) * 11.0 + t * 0.25).sin() * 0.5 + 0.5;
                    let rays = r * r * r * fall * fall.sqrt();
                    let g = (1.0 - d * 2.0).max(0.0);
                    let glow = g * g * g.sqrt().sqrt().max(0.0);
                    let core = (1.0 - d / 0.07).clamp(0.0, 1.0).sqrt();
                    ((glow * 255.0 + rays * 150.0) * k + core * 255.0).min(255.0)
                };
                self.fog[j * mw + i] = a;
            }
        }
        apply_light_map(rgba, w, h, &self.fog, mw, Light::Screen([255, 236, 170]));
        // Lens flare: soft discs along the line from the sun through the center.
        let (cx, cy) = (wf * 0.5, hf * 0.5);
        for (n, (f, r, col, a)) in [(0.35f32, 0.035f32, [255u8, 220, 150], 70u32), (0.6, 0.06, [180, 220, 255], 45), (0.95, 0.02, [255, 180, 120], 80), (1.3, 0.09, [200, 255, 210], 30)].into_iter().enumerate() {
            let x = sx + (cx - sx) * f * 2.0;
            let y = sy + (cy - sy) * f * 2.0;
            let wob = 1.0 + 0.05 * (t * 1.5 + n as f32).sin();
            glow_dot(rgba, w, h, x, y, r * hf * wob * 0.5, col, a as f32 / 255.0 * (0.6 + 0.4 * amount));
        }
    }

    /// Fog: soft clouds drifting across (smooth noise, a few octaves), whitening the
    /// picture where they pass; denser toward the bottom.
    fn fog(&mut self, rgba: &mut [u8], w: usize, h: usize, _dt: f32, amount: f32) {
        // A random lattice once; sampled with smooth interpolation, scrolled over time.
        const N: usize = 64;
        if self.lattice.len() != N * N {
            self.lattice = (0..N * N).map(|_| self.rng.range(0.0, 1.0)).collect();
        }
        let t = self.t;
        let lat = &self.lattice;
        let noise = |x: f32, y: f32| -> f32 {
            let (xi, yi) = (x.floor(), y.floor());
            let (fx, fy) = (x - xi, y - yi);
            let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
            let at = |i: f32, j: f32| lat[((j.rem_euclid(N as f32)) as usize) * N + (i.rem_euclid(N as f32)) as usize];
            let top = at(xi, yi) * (1.0 - sx) + at(xi + 1.0, yi) * sx;
            let bot = at(xi, yi + 1.0) * (1.0 - sx) + at(xi + 1.0, yi + 1.0) * sx;
            top * (1.0 - sy) + bot * sy
        };
        let (mw, mh) = (w / LIGHT_CELL + 2, h / LIGHT_CELL + 2);
        let scale = 4.0 / h as f32;
        let mut map = std::mem::take(&mut self.fog);
        map.resize(mw * mh, 0.0);
        for j in 0..mh {
            for i in 0..mw {
                let (x, y) = ((i * LIGHT_CELL) as f32, (j * LIGHT_CELL) as f32);
                let (u, v) = (x * scale, y * scale);
                let n = noise(u + t * 0.12, v + t * 0.02) * 0.6 + noise(u * 2.1 - t * 0.2, v * 2.1) * 0.3 + noise(u * 4.3 + t * 0.35, v * 4.3) * 0.1;
                let base = 0.25 + 0.35 * (y / h as f32).min(1.0);
                map[j * mw + i] = ((n - 0.35) * 2.2 + base - 0.3).clamp(0.0, 1.0) * (120.0 + 110.0 * amount);
            }
        }
        apply_light_map(rgba, w, h, &map, mw, Light::Over([232, 236, 240]));
        self.fog = map;
    }
    fn spawn(&mut self, wf: f32, hf: f32, s: f32, anywhere: bool, rising: bool) -> Particle {
        let r = &mut self.rng;
        let x = r.range(0.0, wf);
        let y = if anywhere {
            r.range(0.0, hf)
        } else if rising {
            hf + r.range(10.0, 80.0) * s
        } else {
            -r.range(10.0, 80.0) * s
        };
        let phase = r.range(0.0, 6.28);
        match self.kind {
            AnimKind::Snow => {
                let size = r.range(1.5, 4.5) * s;
                Particle { x, y, vx: r.range(-8.0, 8.0) * s, vy: (35.0 + size * 12.0) * s, size, phase, color: [255, 255, 255] }
            }
            AnimKind::Confetti => {
                const C: [[u8; 3]; 6] = [[255, 70, 90], [255, 200, 40], [70, 200, 120], [60, 160, 255], [190, 90, 255], [255, 140, 40]];
                let color = C[r.next() as usize % C.len()];
                Particle { x, y, vx: r.range(-20.0, 20.0) * s, vy: r.range(90.0, 170.0) * s, size: r.range(7.0, 13.0) * s, phase, color }
            }
            AnimKind::Hearts => {
                const C: [[u8; 3]; 3] = [[255, 60, 100], [255, 110, 160], [230, 30, 70]];
                let color = C[r.next() as usize % C.len()];
                Particle { x, y, vx: r.range(-10.0, 10.0) * s, vy: -r.range(60.0, 120.0) * s, size: r.range(12.0, 28.0) * s, phase, color }
            }
            AnimKind::Rain => {
                Particle { x, y, vx: -60.0 * s, vy: r.range(700.0, 1000.0) * s, size: r.range(18.0, 36.0) * s, phase, color: [150, 175, 215] }
            }
            AnimKind::Stars => {
                const C: [[u8; 3]; 3] = [[255, 230, 90], [255, 255, 255], [255, 200, 120]];
                let color = C[r.next() as usize % C.len()];
                Particle { x, y, vx: r.range(-6.0, 6.0) * s, vy: r.range(25.0, 60.0) * s, size: r.range(12.0, 24.0) * s, phase, color }
            }
            AnimKind::Balloons => {
                const C: [[u8; 3]; 6] = [[240, 60, 80], [255, 190, 40], [60, 170, 255], [80, 200, 110], [190, 90, 255], [255, 120, 180]];
                let color = C[r.next() as usize % C.len()];
                let y = if anywhere { y } else { hf + r.range(60.0, 160.0) * s };
                Particle { x, y, vx: r.range(-8.0, 8.0) * s, vy: -r.range(45.0, 90.0) * s, size: r.range(22.0, 38.0) * s, phase, color }
            }
            AnimKind::Leaves => {
                const C: [[u8; 3]; 4] = [[234, 88, 12], [250, 204, 21], [220, 38, 38], [245, 158, 11]];
                let color = C[r.next() as usize % C.len()];
                Particle { x, y, vx: r.range(-30.0, 30.0) * s, vy: r.range(50.0, 100.0) * s, size: r.range(10.0, 18.0) * s, phase, color }
            }
            AnimKind::Fireflies => {
                let y = if anywhere { y } else { hf + r.range(5.0, 40.0) * s };
                Particle { x, y, vx: r.range(-15.0, 15.0) * s, vy: -r.range(10.0, 30.0) * s, size: r.range(4.0, 8.0) * s, phase, color: [255, 236, 90] }
            }
            _ => {
                let size = r.range(8.0, 26.0) * s;
                Particle { x, y, vx: r.range(-6.0, 6.0) * s, vy: -r.range(50.0, 110.0) * s, size, phase, color: [200, 240, 255] }
            }
        }
    }
}

/// Grid spacing (px) of the coarse light / fog maps.
const LIGHT_CELL: usize = 8;

/// How a light map is laid over the picture.
#[derive(Clone, Copy)]
enum Light {
    /// Brighten toward the color ("screen"): sunlight.
    Screen([u8; 3]),
    /// Plain alpha blend toward the color: fog.
    Over([u8; 3]),
}

/// Stretch a coarse alpha map (`mw` wide, one value per LIGHT_CELL px, corners included)
/// smoothly over the picture, where it is not ~0. Fixed-point inner loop (no divisions).
fn apply_light_map(rgba: &mut [u8], w: usize, h: usize, map: &[f32], mw: usize, light: Light) {
    let c = LIGHT_CELL;
    for y in 0..h {
        let j = y / c;
        let ty = (y % c) as f32 / c as f32;
        let (r0, r1) = (&map[j * mw..(j + 1) * mw], &map[(j + 1) * mw..(j + 2) * mw]);
        for i in 0..w.div_ceil(c) {
            let a0 = r0[i] + (r1[i] - r0[i]) * ty;
            let a1 = r0[i + 1] + (r1[i + 1] - r0[i + 1]) * ty;
            if a0 < 1.5 && a1 < 1.5 {
                continue;
            }
            let (xs, xe) = (i * c, ((i + 1) * c).min(w));
            // Alpha in 16.16 fixed point, stepping across the cell.
            let mut a = (a0.clamp(0.0, 255.0) * 65536.0) as i32;
            let step = ((a1.clamp(0.0, 255.0) - a0.clamp(0.0, 255.0)) / c as f32 * 65536.0) as i32;
            let span = &mut rgba[(y * w + xs) * 4..(y * w + xe) * 4];
            match light {
                Light::Screen(col) => {
                    for p in span.chunks_exact_mut(4) {
                        let al = (a >> 16) as u32; // 0..255
                        for k in 0..3 {
                            // b + (255 - b) · col · al / 255²
                            let kc = (col[k] as u32 * al * 257) >> 8; // ≈ col·al·65536/255²
                            let b = p[k] as u32;
                            p[k] = (b + (((255 - b) * kc) >> 16)) as u8;
                        }
                        a += step;
                    }
                }
                Light::Over(col) => {
                    for p in span.chunks_exact_mut(4) {
                        let al = ((a >> 16) as i32 * 257) >> 8; // 0..256
                        for k in 0..3 {
                            let b = p[k] as i32;
                            p[k] = (b + (((col[k] as i32 - b) * al) >> 8)) as u8;
                        }
                        a += step;
                    }
                }
            }
        }
    }
}

/// An anti-aliased line of `width` px from (x0, y0) to (x1, y1), blended. Walks the long
/// axis and only touches the pixels near the line.
#[allow(clippy::too_many_arguments)]
fn soft_line(px: &mut [u8], w: usize, h: usize, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: [u8; 3], alpha: u32) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    if alpha == 0 || len < 0.01 {
        return;
    }
    let hw = width * 0.5;
    // Swap axes for flat lines so the loop always runs along the longer one.
    let steep = dy.abs() >= dx.abs();
    let (a0, b0, a1, b1, la, lb) = if steep { (y0, x0, y1, x1, h, w) } else { (x0, y0, x1, y1, w, h) };
    let (a0, b0, a1, b1) = if a0 <= a1 { (a0, b0, a1, b1) } else { (a1, b1, a0, b0) };
    let slope = (b1 - b0) / (a1 - a0).max(1e-4);
    let cosf = (a1 - a0) / len; // perpendicular distance = |Δb| · cosf
    let rb = (hw + 0.5) / cosf.max(0.05) + 0.5;
    let start = (a0 - hw).floor().max(0.0) as usize;
    let end = ((a1 + hw).ceil().max(0.0) as usize).min(la);
    for a in start..end {
        let ac = a as f32 + 0.5;
        let at = ac.clamp(a0, a1);
        let cap = (hw + 0.5 - (ac - at).abs()).clamp(0.0, 1.0);
        if cap <= 0.0 {
            continue;
        }
        let bc = b0 + (at - a0) * slope;
        let bs = (bc - rb).floor().max(0.0) as usize;
        let be = ((bc + rb).ceil().max(0.0) as usize).min(lb);
        for b in bs..be {
            let d = (b as f32 + 0.5 - bc).abs() * cosf;
            let cov = (hw + 0.5 - d).clamp(0.0, 1.0) * cap;
            if cov > 0.0 {
                let i = if steep { (a * w + b) * 4 } else { (b * w + a) * 4 };
                blend(px, i, color, (alpha as f32 * cov) as u32);
            }
        }
    }
}
/// A pointed leaf `len` long, turned by `angle`, tumbling (`tumble` -1..1 squashes its
/// width, like a leaf seen edge-on), shaded darker at the edges, with a midrib.
#[allow(clippy::too_many_arguments)]
fn pointed_leaf(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, len: f32, angle: f32, tumble: f32, color: [u8; 3]) {
    let half = len * 0.5;
    let wid = len * 0.32 * tumble.abs().max(0.2);
    let (c, s) = (angle.cos(), angle.sin());
    let r = half + 2.0;
    let (x0, x1) = ((cx - r).floor().max(0.0) as usize, ((cx + r).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - r).floor().max(0.0) as usize, ((cy + r).ceil().max(0.0) as usize).min(h));
    let dark = [color[0] / 2, color[1] / 2, color[2] / 2];
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let u = dx * c + dy * s;
            let v = -dx * s + dy * c;
            if u.abs() >= half {
                continue;
            }
            let hw = wid * (std::f32::consts::PI * (u / half * 0.5 + 0.5)).sin() * (1.0 - 0.25 * (u / half));
            let cov = (hw - v.abs() + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let i = (y * w + x) * 4;
            let edge = (v.abs() / hw.max(0.5)).min(1.0);
            let shade = 1.0 - 0.35 * edge * edge;
            let col = if v.abs() < 0.7 { dark } else { [(color[0] as f32 * shade) as u8, (color[1] as f32 * shade) as u8, (color[2] as f32 * shade) as u8] };
            blend(px, i, col, (235.0 * cov) as u32);
        }
    }
}

/// A thin ellipse outline (a splash ring).
#[allow(clippy::too_many_arguments)]
fn ellipse_ring(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, rx: f32, ry: f32, color: [u8; 3], alpha: u32) {
    let (x0, x1) = ((cx - rx - 1.0).floor().max(0.0) as usize, ((cx + rx + 1.0).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - ry - 1.0).floor().max(0.0) as usize, ((cy + ry + 1.0).ceil().max(0.0) as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = ((x as f32 + 0.5 - cx) / rx.max(0.5), (y as f32 + 0.5 - cy) / ry.max(0.5));
            let d = (dx * dx + dy * dy).sqrt();
            let cov = (1.0 - (d - 1.0).abs() * rx.max(1.0) / 1.5).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend(px, (y * w + x) * 4, color, (alpha as f32 * cov) as u32);
            }
        }
    }
}

/// A rain streak along the direction of motion.
#[allow(clippy::too_many_arguments)]
fn streak(px: &mut [u8], w: usize, h: usize, x: f32, y: f32, vx: f32, vy: f32, len: f32, color: [u8; 3], alpha: u32) {
    let n = (vx * vx + vy * vy).sqrt().max(1.0);
    let (dx, dy) = (vx / n, vy / n);
    let steps = len.max(1.0) as usize;
    for i in 0..steps {
        let (sx, sy) = (x - dx * i as f32, y - dy * i as f32);
        if sx >= 0.0 && sy >= 0.0 && (sx as usize) < w && (sy as usize) < h {
            let a = alpha * (steps - i) as u32 / steps as u32;
            // Two pixels wide, blended (visible on bright and dark pictures alike).
            let i = ((sy as usize) * w + sx as usize) * 4;
            blend(px, i, color, a);
            if (sx as usize) + 1 < w {
                blend(px, i + 4, color, a / 2);
            }
        }
    }
}

/// A four-pointed twinkle: a soft core with a cross of light.
#[allow(clippy::too_many_arguments)]
fn sparkle(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, r: f32, color: [u8; 3], alpha: u32) {
    let (x0, x1) = ((cx - r).floor().max(0.0) as usize, ((cx + r).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - r).floor().max(0.0) as usize, ((cy + r).ceil().max(0.0) as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = ((x as f32 + 0.5 - cx).abs() / r, (y as f32 + 0.5 - cy).abs() / r);
            // Distance in a "star" metric: thin arms along the axes.
            let v = (dx * dy).sqrt() * 3.0 + (dx * dx + dy * dy).sqrt() * 0.6;
            if v < 1.0 {
                screen(px, (y * w + x) * 4, color, ((1.0 - v) * alpha as f32) as u32);
            }
        }
    }
}

/// A balloon: a shaded ellipse with a knot and a string.
fn balloon(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, r: f32, color: [u8; 3]) {
    let (rx, ry) = (r * 0.82, r);
    let (x0, x1) = ((cx - rx).floor().max(0.0) as usize, ((cx + rx).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - ry).floor().max(0.0) as usize, ((cy + ry).ceil().max(0.0) as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
            let d = dx * dx + dy * dy;
            if d < 1.0 {
                // Highlight upper left, darker toward the rim.
                let hl = (1.0 - ((dx + 0.35).powi(2) + (dy + 0.4).powi(2)) * 4.0).max(0.0);
                let shade = 1.0 - d * 0.25;
                let c = [
                    ((color[0] as f32 * shade) + hl * 120.0).min(255.0) as u8,
                    ((color[1] as f32 * shade) + hl * 120.0).min(255.0) as u8,
                    ((color[2] as f32 * shade) + hl * 120.0).min(255.0) as u8,
                ];
                blend(px, (y * w + x) * 4, c, 235);
            }
        }
    }
    // Knot and string.
    let (kx, ky) = (cx, cy + ry);
    for i in 0..(r * 2.2) as usize {
        let sy = ky + i as f32;
        let sx = kx + (i as f32 * 0.15).sin() * r * 0.12;
        if sx >= 0.0 && (sx as usize) < w && (sy as usize) < h {
            blend(px, ((sy as usize) * w + sx as usize) * 4, [230, 230, 235], 180);
        }
    }
}

/// A falling leaf: an ellipse whose width follows the tumble (`flip`), with a midrib.
#[allow(clippy::too_many_arguments)]
fn leaf(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, r: f32, flip: f32, color: [u8; 3]) {
    let rx = (r * flip.abs()).max(r * 0.15);
    let ry = r * 0.55;
    let (x0, x1) = ((cx - rx).floor().max(0.0) as usize, ((cx + rx).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - ry).floor().max(0.0) as usize, ((cy + ry).ceil().max(0.0) as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
            if dx * dx + dy * dy < 1.0 {
                let c = if dy.abs() < 0.12 { [120, 53, 15] } else { color };
                blend(px, (y * w + x) * 4, c, 235);
            }
        }
    }
}

/// A firefly: a bright core in a soft glow (screen blend).
#[allow(clippy::too_many_arguments)]
fn glow_dot(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, r: f32, color: [u8; 3], glow: f32) {
    let rr = r * 4.0;
    let (x0, x1) = ((cx - rr).floor().max(0.0) as usize, ((cx + rr).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - rr).floor().max(0.0) as usize, ((cy + rr).ceil().max(0.0) as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt() / rr;
            if d < 1.0 {
                let a = ((1.0 - d).powi(3) * 255.0 * glow) as u32;
                screen(px, (y * w + x) * 4, color, a);
            }
        }
    }
}

/// Soft disc (snowflake): alpha falls off toward the rim.
#[allow(clippy::too_many_arguments)]
fn disc(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, r: f32, color: [u8; 3], alpha: u32, soft: bool) {
    let (x0, x1) = ((cx - r).floor().max(0.0) as usize, ((cx + r).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - r).floor().max(0.0) as usize, ((cy + r).ceil().max(0.0) as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let d2 = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)) / (r * r);
            if d2 < 1.0 {
                let a = if soft { (1.0 - d2) * alpha as f32 } else { alpha as f32 };
                blend(px, (y * w + x) * 4, color, a as u32);
            }
        }
    }
}

/// Bubble: thin bright ring with a faint fill and a highlight.
fn ring(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, r: f32, color: [u8; 3]) {
    let (x0, x1) = ((cx - r).floor().max(0.0) as usize, ((cx + r).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - r).floor().max(0.0) as usize, ((cy + r).ceil().max(0.0) as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = (dx * dx + dy * dy).sqrt() / r;
            if d >= 1.0 {
                continue;
            }
            let rim = ((d - 0.8) / 0.2).clamp(0.0, 1.0);
            let hi = if dx < -r * 0.25 && dy < -r * 0.25 && d > 0.45 && d < 0.65 { 140.0 } else { 0.0 };
            let a = 25.0 + rim * 170.0 + hi;
            blend(px, (y * w + x) * 4, color, a as u32);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rect(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, rw: f32, rh: f32, color: [u8; 3], alpha: u32) {
    let (x0, x1) = ((cx - rw / 2.0).max(0.0) as usize, ((cx + rw / 2.0).max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - rh / 2.0).max(0.0) as usize, ((cy + rh / 2.0).max(0.0) as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            blend(px, (y * w + x) * 4, color, alpha);
        }
    }
}

/// Heart from the implicit curve (x² + y² − 1)³ − x²·y³ ≤ 0.
#[allow(clippy::too_many_arguments)]
fn heart(px: &mut [u8], w: usize, h: usize, cx: f32, cy: f32, size: f32, color: [u8; 3], alpha: u32) {
    let r = size;
    let (x0, x1) = ((cx - r).floor().max(0.0) as usize, ((cx + r).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((cy - r).floor().max(0.0) as usize, ((cy + r).ceil().max(0.0) as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let hx = (x as f32 + 0.5 - cx) / (r * 0.8);
            let hy = -(y as f32 + 0.5 - cy) / (r * 0.8);
            let v = (hx * hx + hy * hy - 1.0).powi(3) - hx * hx * hy * hy * hy;
            if v <= 0.0 {
                blend(px, (y * w + x) * 4, color, alpha);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn weather_parts_cost() {
        let (w, h) = (1920usize, 1080usize);
        let mut buf: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let time = |name: &str, f: &mut dyn FnMut()| {
            let t0 = std::time::Instant::now();
            for _ in 0..30 {
                f();
            }
            println!("{name:30} {:.2} ms", t0.elapsed().as_secs_f64() * 1000.0 / 30.0);
        };
        let m = ColorMatrix::channels([1.06, 1.02, 0.92], [8.0, 4.0, -4.0]);
        time("matrix", &mut || m.apply_in_place(&mut buf));
        let (mw, mh) = (w / LIGHT_CELL + 2, h / LIGHT_CELL + 2);
        let map: Vec<f32> = (0..mw * mh).map(|i| (i % 300) as f32 * 0.8).collect();
        time("map screen", &mut || apply_light_map(&mut buf, w, h, &map, mw, Light::Screen([255, 236, 170])));
        time("map over", &mut || apply_light_map(&mut buf, w, h, &map, mw, Light::Over([232, 236, 240])));
        time("glow dots", &mut || {
            for r in [19.0f32, 32.0, 11.0, 48.0] {
                glow_dot(&mut buf, w, h, 900.0, 500.0, r, [255, 220, 150], 0.3)
            }
        });
    }

    #[test]
    fn all_anims_render_in_bounds() {
        for (w, h) in [(64u32, 48u32), (1280, 720)] {
            let mut buf = vec![128u8; (w * h * 4) as usize];
            for k in AnimKind::ALL {
                let mut a = Anim::new(k);
                for _ in 0..40 {
                    a.render(&mut buf, w, h, 1.0 / 30.0, 1.0);
                }
            }
        }
    }
}
