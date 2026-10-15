//! CPU effects for the live path: one affine color matrix (color grade and color filters
//! fused into a single pass), a vignette, and geometric distortions (bulge, pinch, swirl,
//! mirrors, wave, pixelate).
//!
//! Why CPU: these touch each pixel once with a handful of operations, which is cheaper
//! than a GPU round trip (upload + readback + CPU/GPU sync) for a 720p/1080p frame.
//! Local distortions only process the pixels inside their circle.
//!
//! Every routine checks buffer sizes up front and never indexes out of bounds.

use std::f32::consts::PI;

// ─────────────────────────────────────────────────────────────────────────────────────
// Color matrix
// ─────────────────────────────────────────────────────────────────────────────────────

/// `out = m · rgb + off`, in 0..255 units, then clamped. Alpha is copied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorMatrix {
    pub m: [[f32; 3]; 3],
    pub off: [f32; 3],
}

impl ColorMatrix {
    pub const IDENTITY: ColorMatrix = ColorMatrix { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], off: [0.0; 3] };

    pub fn is_identity(&self) -> bool {
        let id = Self::IDENTITY;
        self.m.iter().flatten().zip(id.m.iter().flatten()).all(|(a, b)| (a - b).abs() < 1e-4)
            && self.off.iter().all(|o| o.abs() < 0.01)
    }

    /// `self` applied first, then `next`.
    pub fn then(&self, next: &ColorMatrix) -> ColorMatrix {
        let mut m = [[0f32; 3]; 3];
        let mut off = next.off;
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = (0..3).map(|k| next.m[i][k] * self.m[k][j]).sum();
            }
            off[i] += (0..3).map(|k| next.m[i][k] * self.off[k]).sum::<f32>();
        }
        ColorMatrix { m, off }
    }

    /// Blend between identity (`t = 0`) and `self` (`t = 1`): the filter "intensity".
    pub fn mix(&self, t: f32) -> ColorMatrix {
        let id = Self::IDENTITY;
        let mut r = *self;
        for i in 0..3 {
            for j in 0..3 {
                r.m[i][j] = id.m[i][j] + (self.m[i][j] - id.m[i][j]) * t;
            }
            r.off[i] = self.off[i] * t;
        }
        r
    }

    /// Saturation around Rec.709 luma (1 = unchanged, 0 = gray).
    pub fn saturation(s: f32) -> ColorMatrix {
        let w = [0.2126f32, 0.7152, 0.0722];
        let mut m = [[0f32; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, e) in row.iter_mut().enumerate() {
                *e = (if i == j { s } else { 0.0 }) + (1.0 - s) * w[j];
            }
        }
        ColorMatrix { m, off: [0.0; 3] }
    }

    /// Contrast around mid-gray plus a brightness lift (in 0..255 units).
    pub fn contrast(c: f32, lift: f32) -> ColorMatrix {
        let k = 127.5 * (1.0 - c) + lift;
        ColorMatrix { m: [[c, 0.0, 0.0], [0.0, c, 0.0], [0.0, 0.0, c]], off: [k; 3] }
    }

    /// Per-channel gain and offset.
    pub fn channels(gain: [f32; 3], off: [f32; 3]) -> ColorMatrix {
        ColorMatrix { m: [[gain[0], 0.0, 0.0], [0.0, gain[1], 0.0], [0.0, 0.0, gain[2]]], off }
    }

    /// Apply in place, in 16 KB pieces via a small stack copy (no frame-sized allocation).
    pub fn apply_in_place(&self, buf: &mut [u8]) {
        let mut tmp = [0u8; 16 * 1024];
        for chunk in buf.chunks_mut(16 * 1024) {
            let n = chunk.len();
            tmp[..n].copy_from_slice(chunk);
            self.apply(&tmp[..n], chunk);
        }
    }

    /// Apply to RGBA8 `src` → `dst` (same length).
    pub fn apply(&self, src: &[u8], dst: &mut [u8]) {
        if src.len() != dst.len() {
            return;
        }
        let q = |v: f32| (v * 4096.0).round() as i32;
        let m = self.m.map(|r| r.map(q));
        let off = self.off.map(|o| q(o) + 2048); // + 0.5 for rounding
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 support was just checked at runtime.
            unsafe { matrix_avx2(src, dst, &m, &off) };
            return;
        }
        matrix_generic(src, dst, &m, &off);
    }
}

/// The matrix loop compiled with AVX2 (8-wide 32-bit integer multiply). Baseline x86-64
/// only has SSE2, which lacks a packed 32-bit multiply, so without this the loop stays
/// scalar (~3x slower). Every Intel/AMD CPU since ~2013-2015 has AVX2.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn matrix_avx2(src: &[u8], dst: &mut [u8], m: &[[i32; 3]; 3], off: &[i32; 3]) {
    matrix_generic(src, dst, m, off)
}

/// `out = (m·rgb + off) >> 12` in Q12, alpha copied. Batches are deinterleaved into
/// planar lanes so the math vectorizes.
#[inline(always)]
fn matrix_generic(src: &[u8], dst: &mut [u8], m: &[[i32; 3]; 3], off: &[i32; 3]) {
    const N: usize = 16;
    let out = |v: i32| (v >> 12).clamp(0, 255) as u8;
    let mut s_chunks = src.chunks_exact(N * 4);
    let mut d_chunks = dst.chunks_exact_mut(N * 4);
    for (sb, db) in (&mut s_chunks).zip(&mut d_chunks) {
        let (mut r, mut g, mut b) = ([0i32; N], [0i32; N], [0i32; N]);
        for i in 0..N {
            r[i] = sb[4 * i] as i32;
            g[i] = sb[4 * i + 1] as i32;
            b[i] = sb[4 * i + 2] as i32;
        }
        let mut o = [[0u8; N]; 3];
        for (c, oc) in o.iter_mut().enumerate() {
            for i in 0..N {
                oc[i] = out(m[c][0] * r[i] + m[c][1] * g[i] + m[c][2] * b[i] + off[c]);
            }
        }
        for i in 0..N {
            db[4 * i] = o[0][i];
            db[4 * i + 1] = o[1][i];
            db[4 * i + 2] = o[2][i];
            db[4 * i + 3] = sb[4 * i + 3];
        }
    }
    for (si, di) in s_chunks.remainder().chunks_exact(4).zip(d_chunks.into_remainder().chunks_exact_mut(4)) {
        let (r, g, b) = (si[0] as i32, si[1] as i32, si[2] as i32);
        di[0] = out(m[0][0] * r + m[0][1] * g + m[0][2] * b + off[0]);
        di[1] = out(m[1][0] * r + m[1][1] * g + m[1][2] * b + off[1]);
        di[2] = out(m[2][0] * r + m[2][1] * g + m[2][2] * b + off[2]);
        di[3] = si[3];
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Color filters (ManyCam's "Filters" category, re-created)
// ─────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    None,
    BlackWhite,
    Noir,
    Sepia,
    Vintage,
    Warm,
    Cool,
    Vivid,
    Fade,
    Invert,
}

impl Filter {
    pub const ALL: [Filter; 10] = [
        Filter::None,
        Filter::BlackWhite,
        Filter::Noir,
        Filter::Sepia,
        Filter::Vintage,
        Filter::Warm,
        Filter::Cool,
        Filter::Vivid,
        Filter::Fade,
        Filter::Invert,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Filter::None => "None",
            Filter::BlackWhite => "Black & white",
            Filter::Noir => "Noir",
            Filter::Sepia => "Sepia",
            Filter::Vintage => "Vintage",
            Filter::Warm => "Warm",
            Filter::Cool => "Cool",
            Filter::Vivid => "Vivid",
            Filter::Fade => "Fade",
            Filter::Invert => "Invert",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Filter::None => "none",
            Filter::BlackWhite => "bw",
            Filter::Noir => "noir",
            Filter::Sepia => "sepia",
            Filter::Vintage => "vintage",
            Filter::Warm => "warm",
            Filter::Cool => "cool",
            Filter::Vivid => "vivid",
            Filter::Fade => "fade",
            Filter::Invert => "invert",
        }
    }

    pub fn from_id(s: &str) -> Filter {
        Filter::ALL.into_iter().find(|f| f.id() == s).unwrap_or(Filter::None)
    }

    /// The filter as a color matrix at full intensity.
    pub fn matrix(self) -> ColorMatrix {
        let sepia = ColorMatrix {
            m: [[0.393, 0.769, 0.189], [0.349, 0.686, 0.168], [0.272, 0.534, 0.131]],
            off: [0.0; 3],
        };
        match self {
            Filter::None => ColorMatrix::IDENTITY,
            Filter::BlackWhite => ColorMatrix::saturation(0.0),
            Filter::Noir => ColorMatrix::saturation(0.0).then(&ColorMatrix::contrast(1.45, -6.0)),
            Filter::Sepia => sepia,
            Filter::Vintage => sepia
                .mix(0.55)
                .then(&ColorMatrix::contrast(0.86, 10.0))
                .then(&ColorMatrix::channels([1.04, 1.0, 0.9], [6.0, 2.0, 0.0])),
            Filter::Warm => ColorMatrix::channels([1.08, 1.02, 0.86], [8.0, 2.0, 0.0]),
            Filter::Cool => ColorMatrix::channels([0.88, 1.0, 1.1], [0.0, 2.0, 10.0]),
            Filter::Vivid => ColorMatrix::saturation(1.5).then(&ColorMatrix::contrast(1.08, 0.0)),
            Filter::Fade => ColorMatrix::saturation(0.75).then(&ColorMatrix::contrast(0.78, 18.0)),
            Filter::Invert => ColorMatrix::channels([-1.0; 3], [255.0; 3]),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Vignette
// ─────────────────────────────────────────────────────────────────────────────────────

/// Darkened corners. The per-pixel factors are cached and rebuilt only when the frame
/// size or strength changes.
#[derive(Default)]
pub struct Vignette {
    key: (u32, u32, u32),
    /// Q8 multiplier per pixel (256 = unchanged).
    factor: Vec<u16>,
}

impl Vignette {
    /// Darken `rgba` (`w*h*4`) in place; `amount` in 0..1.
    pub fn apply(&mut self, rgba: &mut [u8], w: u32, h: u32, amount: f32) {
        let n = w as usize * h as usize;
        if rgba.len() < n * 4 || n == 0 {
            return;
        }
        let key = (w, h, (amount * 1000.0) as u32);
        if key != self.key || self.factor.len() != n {
            self.key = key;
            let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
            let rmax = (cx * cx + cy * cy).sqrt();
            self.factor = (0..n)
                .map(|i| {
                    let (x, y) = ((i % w as usize) as f32 + 0.5 - cx, (i / w as usize) as f32 + 0.5 - cy);
                    let r = ((x * x + y * y).sqrt() / rmax - 0.45).max(0.0) / 0.55; // 0 inside, 1 at corners
                    let s = r * r * (3.0 - 2.0 * r); // smoothstep
                    ((1.0 - amount * s) * 256.0) as u16
                })
                .collect();
        }
        for (px, &f) in rgba.chunks_exact_mut(4).zip(self.factor.iter()) {
            let f = f as u32;
            px[0] = ((px[0] as u32 * f) >> 8) as u8;
            px[1] = ((px[1] as u32 * f) >> 8) as u8;
            px[2] = ((px[2] as u32 * f) >> 8) as u8;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Distortions (ManyCam's "Distortions" category, re-created)
// ─────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpKind {
    None,
    Bulge,
    Pinch,
    Swirl,
    Fisheye,
    MirrorLeft,
    MirrorRight,
    MirrorTop,
    Quad,
    Wave,
    Pixelate,
}

impl WarpKind {
    pub const ALL: [WarpKind; 11] = [
        WarpKind::None,
        WarpKind::Bulge,
        WarpKind::Pinch,
        WarpKind::Swirl,
        WarpKind::Fisheye,
        WarpKind::MirrorLeft,
        WarpKind::MirrorRight,
        WarpKind::MirrorTop,
        WarpKind::Quad,
        WarpKind::Wave,
        WarpKind::Pixelate,
    ];

    pub fn name(self) -> &'static str {
        match self {
            WarpKind::None => "None",
            WarpKind::Bulge => "Bulge",
            WarpKind::Pinch => "Pinch",
            WarpKind::Swirl => "Swirl",
            WarpKind::Fisheye => "Fisheye",
            WarpKind::MirrorLeft => "Mirror left",
            WarpKind::MirrorRight => "Mirror right",
            WarpKind::MirrorTop => "Mirror top",
            WarpKind::Quad => "Kaleidoscope",
            WarpKind::Wave => "Wave",
            WarpKind::Pixelate => "Pixelate",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            WarpKind::None => "none",
            WarpKind::Bulge => "bulge",
            WarpKind::Pinch => "pinch",
            WarpKind::Swirl => "swirl",
            WarpKind::Fisheye => "fisheye",
            WarpKind::MirrorLeft => "mirror_left",
            WarpKind::MirrorRight => "mirror_right",
            WarpKind::MirrorTop => "mirror_top",
            WarpKind::Quad => "quad",
            WarpKind::Wave => "wave",
            WarpKind::Pixelate => "pixelate",
        }
    }

    pub fn from_id(s: &str) -> WarpKind {
        WarpKind::ALL.into_iter().find(|k| k.id() == s).unwrap_or(WarpKind::None)
    }

    /// Whether it acts around a movable center (bulge, pinch, swirl).
    pub fn has_center(self) -> bool {
        matches!(self, WarpKind::Bulge | WarpKind::Pinch | WarpKind::Swirl)
    }
}

/// A configured distortion. `cx`/`cy` are fractions of the frame; `radius` is a fraction
/// of the frame height; `strength` is 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Warp {
    pub kind: WarpKind,
    pub cx: f32,
    pub cy: f32,
    pub radius: f32,
    pub strength: f32,
}

impl Default for Warp {
    fn default() -> Self {
        Warp { kind: WarpKind::None, cx: 0.5, cy: 0.45, radius: 0.35, strength: 0.6 }
    }
}

impl Warp {
    pub fn is_active(&self) -> bool {
        self.kind != WarpKind::None && (self.strength > 0.001 || matches!(self.kind, WarpKind::MirrorLeft | WarpKind::MirrorRight | WarpKind::MirrorTop | WarpKind::Quad))
    }

    /// Distort `src` → `dst` (both `w*h*4` RGBA). `t` is time in seconds (animated kinds).
    pub fn apply(&self, src: &[u8], dst: &mut [u8], w: u32, h: u32, t: f32) {
        let (w, h) = (w as usize, h as usize);
        if w < 2 || h < 2 || src.len() < w * h * 4 || dst.len() < w * h * 4 {
            return;
        }
        let (src, dst) = (&src[..w * h * 4], &mut dst[..w * h * 4]);
        let s = self.strength.clamp(0.0, 1.0);
        match self.kind {
            WarpKind::None => dst.copy_from_slice(src),
            WarpKind::Bulge | WarpKind::Pinch | WarpKind::Swirl | WarpKind::Fisheye => {
                // One-off use (tests): build the map and apply it.
                WarpRenderer::default().apply(self, src, dst, w as u32, h as u32, t);
            }
            WarpKind::MirrorLeft | WarpKind::MirrorRight => {
                let keep_left = self.kind == WarpKind::MirrorLeft;
                for (srow, drow) in src.chunks_exact(w * 4).zip(dst.chunks_exact_mut(w * 4)) {
                    for x in 0..w {
                        let sx = match (keep_left, x < w / 2) {
                            (true, true) | (false, false) => x,
                            _ => w - 1 - x,
                        };
                        drow[x * 4..x * 4 + 4].copy_from_slice(&srow[sx * 4..sx * 4 + 4]);
                    }
                }
            }
            WarpKind::MirrorTop => {
                for y in 0..h {
                    let sy = if y < h / 2 { y } else { h - 1 - y };
                    dst[y * w * 4..(y + 1) * w * 4].copy_from_slice(&src[sy * w * 4..(sy + 1) * w * 4]);
                }
            }
            WarpKind::Quad => {
                // Top-left quarter reflected into all four (kaleidoscope).
                for y in 0..h {
                    let sy = if y < h / 2 { y } else { h - 1 - y };
                    for x in 0..w {
                        let sx = if x < w / 2 { x } else { w - 1 - x };
                        let (d, s) = ((y * w + x) * 4, (sy * w + sx) * 4);
                        dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
                    }
                }
            }
            WarpKind::Wave => {
                // Each row shifted sideways by a moving sine: cheap (row copies).
                let amp = s * 0.04 * w as f32;
                let wavelength = h as f32 / 3.0;
                for y in 0..h {
                    let shift = (amp * ((y as f32 / wavelength) * 2.0 * PI + t * 3.0).sin()).round() as isize;
                    let (srow, drow) = (&src[y * w * 4..(y + 1) * w * 4], &mut dst[y * w * 4..(y + 1) * w * 4]);
                    for x in 0..w {
                        let sx = (x as isize - shift).clamp(0, w as isize - 1) as usize;
                        drow[x * 4..x * 4 + 4].copy_from_slice(&srow[sx * 4..sx * 4 + 4]);
                    }
                }
            }
            WarpKind::Pixelate => {
                let block = (2.0 + s * 38.0) as usize;
                for by in (0..h).step_by(block) {
                    for bx in (0..w).step_by(block) {
                        let (cx, cy) = ((bx + block / 2).min(w - 1), (by + block / 2).min(h - 1));
                        let c = (cy * w + cx) * 4;
                        let px = [src[c], src[c + 1], src[c + 2], src[c + 3]];
                        for y in by..(by + block).min(h) {
                            for x in bx..(bx + block).min(w) {
                                let d = (y * w + x) * 4;
                                dst[d..d + 4].copy_from_slice(&px);
                            }
                        }
                    }
                }
            }
        }
    }

}

/// Renders the radial distortions (bulge, pinch, swirl, fisheye) from a cached
/// displacement map, so the per-frame cost is just a bilinear gather.
///
/// For bulge/pinch/swirl the map covers the circle's bounding square and is *relative to
/// the center*, so it stays valid while the center moves (following a face); it is rebuilt
/// only when kind / radius / strength change (radius is quantized so face-size jitter
/// doesn't trigger rebuilds). Fisheye's map covers the whole frame.
#[derive(Default)]
pub struct WarpRenderer {
    key: Option<(WarpKind, u32, u32, u32, u32)>,
    /// Map side (local warps: 2R) or frame size (fisheye).
    mw: usize,
    mh: usize,
    /// Source offset per map cell in 1/16 px; `[i16::MIN, _]` = untouched pixel.
    offs: Vec<[i16; 2]>,
}

const OUTSIDE: i16 = i16::MIN;

impl WarpRenderer {
    /// Distort `src` → `dst` (both `w*h*4` RGBA) with `warp`.
    pub fn apply(&mut self, warp: &Warp, src: &[u8], dst: &mut [u8], w: u32, h: u32, t: f32) {
        let (wu, hu) = (w as usize, h as usize);
        if wu < 2 || hu < 2 || src.len() < wu * hu * 4 || dst.len() < wu * hu * 4 {
            return;
        }
        let local = warp.kind.has_center();
        if !local && warp.kind != WarpKind::Fisheye {
            warp.apply(src, dst, w, h, t);
            return;
        }
        let s = warp.strength.clamp(0.0, 1.0);
        // Radius in px, quantized to 1% of the height.
        let r = if local { ((warp.radius.max(0.02) * 100.0).round() / 100.0 * h as f32).max(2.0) as usize } else { 0 };
        let key = (warp.kind, r as u32, (s * 1000.0) as u32, w, h);
        if self.key != Some(key) {
            self.key = Some(key);
            self.build(warp.kind, s, r, wu, hu);
        }
        dst[..wu * hu * 4].copy_from_slice(&src[..wu * hu * 4]);
        // Where the map's top-left lands in the frame.
        let (ox, oy) = if local {
            ((warp.cx * w as f32).round() as isize - r as isize, (warp.cy * h as f32).round() as isize - r as isize)
        } else {
            (0, 0)
        };
        for my in 0..self.mh {
            let y = oy + my as isize;
            if y < 0 || y >= hu as isize {
                continue;
            }
            let x_start = (-ox).max(0) as usize;
            let x_end = (self.mw as isize).min(wu as isize - ox).max(0) as usize;
            for mx in x_start..x_end {
                let [dx, dy] = self.offs[my * self.mw + mx];
                if dx == OUTSIDE {
                    continue;
                }
                let x = ox + mx as isize;
                // Source position in 1/16 px, clamped inside the frame.
                let fx = ((x << 4) + dx as isize).clamp(0, ((wu - 1) << 4) as isize - 1) as usize;
                let fy = ((y << 4) + dy as isize).clamp(0, ((hu - 1) << 4) as isize - 1) as usize;
                let (x0, y0, ax, ay) = (fx >> 4, fy >> 4, (fx & 15) as u32, (fy & 15) as u32);
                let a = (y0 * wu + x0) * 4;
                let (b, c) = (a + 4, a + wu * 4);
                let d = c + 4;
                let o = (y as usize * wu + x as usize) * 4;
                for ch in 0..4 {
                    let top = src[a + ch] as u32 * (16 - ax) + src[b + ch] as u32 * ax;
                    let bot = src[c + ch] as u32 * (16 - ax) + src[d + ch] as u32 * ax;
                    dst[o + ch] = ((top * (16 - ay) + bot * ay) >> 8) as u8;
                }
            }
        }
    }

    fn build(&mut self, kind: WarpKind, s: f32, r: usize, w: usize, h: usize) {
        let (mw, mh, cx, cy, rad, s) = if kind == WarpKind::Fisheye {
            // A bulge centered on the frame, reaching the corners.
            let rad = ((w * w + h * h) as f32).sqrt() / 2.0;
            (w, h, w as f32 / 2.0, h as f32 / 2.0, rad, s * 0.8)
        } else {
            (2 * r, 2 * r, r as f32, r as f32, r as f32, s)
        };
        let kind = if kind == WarpKind::Fisheye { WarpKind::Bulge } else { kind };
        self.mw = mw;
        self.mh = mh;
        self.offs.clear();
        self.offs.reserve(mw * mh);
        let inv_r = 1.0 / rad;
        for y in 0..mh {
            let dy = y as f32 + 0.5 - cy;
            for x in 0..mw {
                let dx = x as f32 + 0.5 - cx;
                let d2 = dx * dx + dy * dy;
                if d2 >= rad * rad {
                    self.offs.push([OUTSIDE, 0]);
                    continue;
                }
                let rn = d2.sqrt() * inv_r; // 0 at center .. 1 at the edge
                let (sx, sy) = match kind {
                    // Sample closer to the center → the middle is magnified. rn^k → identity
                    // at the edge, so there is no seam.
                    WarpKind::Bulge => {
                        let k = rn.powf(s * 1.4);
                        (dx * k, dy * k)
                    }
                    // Sample farther out → the middle shrinks.
                    WarpKind::Pinch => {
                        let k = rn.max(1e-3).powf(-s * 0.7).min(1.0 / rn.max(1e-3));
                        (dx * k, dy * k)
                    }
                    WarpKind::Swirl => {
                        let a = s * 2.5 * PI * (1.0 - rn) * (1.0 - rn);
                        let (sn, cs) = a.sin_cos();
                        (dx * cs - dy * sn, dx * sn + dy * cs)
                    }
                    _ => (dx, dy),
                };
                let q = |v: f32| (v * 16.0).round().clamp(-32000.0, 32000.0) as i16;
                self.offs.push([q(sx - dx), q(sy - dy)]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_compose_and_identity() {
        let a = ColorMatrix::contrast(1.2, 5.0);
        assert!(a.then(&ColorMatrix::IDENTITY).m == a.m);
        assert!(Filter::None.matrix().is_identity());
        let mut out = vec![0u8; 8];
        Filter::Invert.matrix().apply(&[10, 20, 30, 255, 0, 0, 0, 7], &mut out);
        assert_eq!(out, vec![245, 235, 225, 255, 255, 255, 255, 7]);
    }

    #[test]
    fn warps_keep_buffers_in_bounds() {
        let (w, h) = (64u32, 48u32);
        let src: Vec<u8> = (0..w * h * 4).map(|i| i as u8).collect();
        let mut dst = vec![0u8; src.len()];
        for kind in WarpKind::ALL {
            for (cx, cy) in [(0.5, 0.5), (0.0, 0.0), (1.0, 1.0), (-0.3, 1.4)] {
                Warp { kind, cx, cy, radius: 0.8, strength: 1.0 }.apply(&src, &mut dst, w, h, 1.0);
            }
        }
        // Bulge at zero strength is the identity.
        Warp { kind: WarpKind::Bulge, cx: 0.5, cy: 0.5, radius: 0.3, strength: 0.0 }.apply(&src, &mut dst, w, h, 0.0);
        assert_eq!(src, dst);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore]
    fn bench_warp_parts() {
        let (w, h) = (1920u32, 1080u32);
        let src: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let mut dst = vec![0u8; src.len()];
        let warp = Warp { kind: WarpKind::Bulge, cx: 0.5, cy: 0.45, radius: 0.35, strength: 0.7 };
        let mut r = WarpRenderer::default();
        let s = std::time::Instant::now();
        r.apply(&warp, &src, &mut dst, w, h, 0.0);
        println!("first (build + apply): {:.2} ms, map {}x{}", s.elapsed().as_secs_f64() * 1e3, r.mw, r.mh);
        let s = std::time::Instant::now();
        for _ in 0..30 {
            r.apply(&warp, &src, &mut dst, w, h, 0.0);
        }
        println!("apply only: {:.2} ms", s.elapsed().as_secs_f64() * 1e3 / 30.0);
    }
}
