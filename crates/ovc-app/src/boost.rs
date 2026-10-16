//! Camera boost: makes a weak (laptop) webcam look better, cheaply.
//!
//! 1. **Light and color** — every few frames the picture is measured (darkest / brightest
//!    parts, color cast) and a gentle correction is worked out: levels stretched, too-dark
//!    pictures brightened, a color cast neutralized, a little more color. It is a color
//!    matrix folded into the one color pass the pipeline does anyway, so it costs nothing
//!    per pixel.
//! 2. **Grain** — one pass that calms sensor grain where the picture doesn't change
//!    (blends with the previous frame only where the difference is small, so moving parts
//!    stay sharp). No sharpening: on a grainy webcam it mostly sharpens the grain.

use ovc_effects::cpu::ColorMatrix;

pub struct CameraBoost {
    /// The correction in use (eased toward `target`).
    gain: [f32; 3],
    off: [f32; 3],
    target_gain: [f32; 3],
    target_off: [f32; 3],
    frame: u32,
    /// The previous output (for the grain filter).
    prev: Vec<u8>,
}

impl Default for CameraBoost {
    fn default() -> Self {
        CameraBoost { gain: [1.0; 3], off: [0.0; 3], target_gain: [1.0; 3], target_off: [0.0; 3], frame: 0, prev: Vec::new() }
    }
}

impl CameraBoost {
    /// Look at the camera picture (every 8th frame) and update the light/color correction.
    pub fn observe(&mut self, rgba: &[u8], w: u32, h: u32) {
        self.frame = self.frame.wrapping_add(1);
        if self.frame % 8 == 1 {
            self.measure(rgba, w as usize, h as usize);
        }
        // Ease toward the target (no visible jumps when the light changes).
        for c in 0..3 {
            self.gain[c] += (self.target_gain[c] - self.gain[c]) * 0.08;
            self.off[c] += (self.target_off[c] - self.off[c]) * 0.08;
        }
    }

    fn measure(&mut self, rgba: &[u8], w: usize, h: usize) {
        if w < 16 || h < 16 || rgba.len() < w * h * 4 {
            return;
        }
        let mut hist = [[0u32; 256]; 3];
        let mut luma = [0u32; 256];
        let mut sum = [0u64; 3];
        let mut n = 0u64;
        for y in (0..h).step_by(6) {
            let row = &rgba[y * w * 4..(y + 1) * w * 4];
            for px in row.chunks_exact(4).step_by(6) {
                for c in 0..3 {
                    hist[c][px[c] as usize] += 1;
                    sum[c] += px[c] as u64;
                }
                luma[((px[0] as u32 * 77 + px[1] as u32 * 150 + px[2] as u32 * 29) >> 8) as usize] += 1;
                n += 1;
            }
        }
        if n == 0 {
            return;
        }
        let pct = |hist: &[u32; 256], p: f64| -> f32 {
            let goal = (n as f64 * p) as u32;
            let mut acc = 0;
            for (v, &k) in hist.iter().enumerate() {
                acc += k;
                if acc >= goal {
                    return v as f32;
                }
            }
            255.0
        };
        let (lo, hi) = (pct(&luma, 0.01), pct(&luma, 0.995));
        let mid = pct(&luma, 0.5);
        // Levels: stretch the used range toward 6..248 (limited, so a dark room isn't turned
        // into a grey mush).
        let mut g = ((248.0 - 6.0) / (hi - lo).max(40.0)).clamp(1.0, 1.6);
        // A dark picture (weak webcam, dim room): lift it a bit more.
        if mid * g < 125.0 {
            g = (g * (125.0 / (mid * g).max(1.0)).sqrt()).min(1.9);
        }
        // Black point: pull the shadows down only a little (a weak webcam's shadows are noise).
        let o = (6.0 - lo * g).max(-25.0);
        // Color cast (grey world, gently): each channel's average toward the overall one.
        let avg = [sum[0] as f32 / n as f32, sum[1] as f32 / n as f32, sum[2] as f32 / n as f32];
        let gray = (avg[0] + avg[1] + avg[2]) / 3.0;
        for c in 0..3 {
            let wb = (gray / avg[c].max(1.0)).clamp(0.88, 1.12);
            let wb = 1.0 + (wb - 1.0) * 0.8;
            self.target_gain[c] = g * wb;
            self.target_off[c] = o;
        }
    }

    /// The light/color correction as a color matrix (plus a touch more saturation).
    pub fn matrix(&self) -> ColorMatrix {
        ColorMatrix::channels(self.gain, self.off).then(&ColorMatrix::saturation(1.08))
    }

    /// Calm the grain, in place: where this frame is close to the previous output the two
    /// are blended (recursively, so still areas come out clean); real changes pass through.
    /// Branch-free so it runs on 16–32 bytes at a time.
    pub fn detail(&mut self, rgba: &mut Vec<u8>, _tmp: &mut Vec<u8>, w: u32, h: u32) {
        let n = w as usize * h as usize * 4;
        if rgba.len() < n {
            return;
        }
        if self.prev.len() != n {
            self.prev = rgba[..n].to_vec();
            return;
        }
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 support was just checked at runtime.
            unsafe { denoise_avx2(&mut rgba[..n], &mut self.prev) };
            return;
        }
        denoise(&mut rgba[..n], &mut self.prev);
    }
    /// Forget the grain history (new camera / size).
    pub fn reset(&mut self) {
        self.prev.clear();
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn denoise_avx2(cur: &mut [u8], prev: &mut [u8]) {
    denoise(cur, prev)
}

/// out = c + (p − c)·k, k from 0.69 (identical) down to 0 (differs by 24+ levels).
#[inline(always)]
fn denoise(cur: &mut [u8], prev: &mut [u8]) {
    for (cs, ps) in cur.chunks_mut(32).zip(prev.chunks_mut(32)) {
        for (c, p) in cs.iter_mut().zip(ps.iter_mut()) {
            let (ci, pi) = (*c as i32, *p as i32);
            let d = pi - ci;
            let k = (24 - d.abs()).clamp(0, 16) * 11; // 0..176 of 256
            let v = (ci + ((d * k) >> 8)) as u8;
            *c = v;
            *p = v;
        }
    }
}