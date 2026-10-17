//! "Studio voice": cleans the teacher's microphone before it is mixed with the video sound
//! (so music and videos are never touched).
//!
//! 1. A high-pass filter (80 Hz) takes out rumble, desk knocks and hum.
//! 2. Noise suppression — **Light**: RNNoise (Xiph / Mozilla, via the pure-Rust
//!    `nnnoiseless` port, ~0.5 % of a core); **Best**: DeepFilterNet3 (Hendrik Schröter,
//!    ~3–10 % of a core), closer to Krisp. Noise is lowered by at most 30 dB, never muted:
//!    no "gate" that silences the room and then lets the keyboard through as soon as you
//!    speak, and no dead silence between words.
//! 3. A gentle compressor evens out loud and quiet words — it never raises quiet sound by
//!    more than a couple of dB (no automatic gain that pumps the room up).
//! 4. A limiter keeps laughs and shouts from clipping.
//!
//! Works on 10 ms frames (480 samples at 48 kHz), so it adds 10 ms of delay.

use std::collections::VecDeque;

pub const OFF: u8 = 0;
pub const LIGHT: u8 = 1;
pub const BEST: u8 = 2;

const RATE: f32 = 48_000.0;
const FRAME: usize = 480;
/// Most the noise is lowered (dB).
const ATTEN_LIMIT_DB: f32 = 30.0;

enum Denoiser {
    Light(Box<nnnoiseless::DenoiseState<'static>>),
    Best(Box<df::tract::DfTract>),
}

/// Second-order Butterworth high-pass (biquad, transposed direct form II).
struct HighPass {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl HighPass {
    fn new(freq: f32) -> Self {
        let w = 2.0 * std::f32::consts::PI * freq / RATE;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let b0 = (1.0 + cos) / 2.0 / a0;
        HighPass { b: [b0, -2.0 * b0, b0], a: [-2.0 * cos / a0, (1.0 - alpha) / a0], z: [0.0; 2] }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Compressor (soft knee) followed by a peak limiter.
struct Dynamics {
    env_db: f32,
    lim_gain: f32,
}

impl Dynamics {
    const THRESHOLD_DB: f32 = -20.0;
    const RATIO: f32 = 2.5;
    const KNEE_DB: f32 = 8.0;
    /// Small make-up gain: the compressor only takes peaks down.
    const MAKEUP_DB: f32 = 2.0;
    const CEILING: f32 = 0.89; // -1 dBFS

    fn new() -> Self {
        Dynamics { env_db: -90.0, lim_gain: 1.0 }
    }

    fn run(&mut self, x: f32) -> f32 {
        let level_db = 20.0 * x.abs().max(1e-6).log10();
        // Attack 5 ms, release 150 ms.
        let k = if level_db > self.env_db { 1.0 - (-1.0 / (0.005 * RATE)).exp() } else { 1.0 - (-1.0 / (0.150 * RATE)).exp() };
        self.env_db += (level_db - self.env_db) * k;
        let over = self.env_db - Self::THRESHOLD_DB;
        let reduction = if over <= -Self::KNEE_DB / 2.0 {
            0.0
        } else if over >= Self::KNEE_DB / 2.0 {
            over * (1.0 - 1.0 / Self::RATIO)
        } else {
            let t = over + Self::KNEE_DB / 2.0;
            (1.0 - 1.0 / Self::RATIO) * t * t / (2.0 * Self::KNEE_DB)
        };
        let y = x * 10f32.powf((Self::MAKEUP_DB - reduction) / 20.0);
        // Limiter: instant attack, 60 ms release.
        let need = if y.abs() > Self::CEILING { Self::CEILING / y.abs() } else { 1.0 };
        if need < self.lim_gain {
            self.lim_gain = need;
        } else {
            self.lim_gain += (1.0 - self.lim_gain) * (1.0 - (-1.0 / (0.060 * RATE)).exp());
        }
        (y * self.lim_gain).clamp(-Self::CEILING, Self::CEILING)
    }
}

pub struct Voice {
    mode: u8,
    denoise: Denoiser,
    hp: HighPass,
    dyn_: Dynamics,
    /// Mono input waiting for a full frame, and processed output (one frame behind).
    pending: Vec<f32>,
    ready: VecDeque<f32>,
    frame_in: Vec<f32>,
    frame_out: Vec<f32>,
}

impl Voice {
    /// `mode`: [`LIGHT`] or [`BEST`] (Best falls back to Light if its model can't load).
    pub fn new(mode: u8) -> Self {
        let denoise = if mode == BEST {
            let rp = df::tract::RuntimeParams::default_with_ch(1).with_atten_lim(ATTEN_LIMIT_DB);
            match df::tract::DfTract::new(df::tract::DfParams::default(), &rp) {
                Ok(m) => Denoiser::Best(Box::new(m)),
                Err(e) => {
                    tracing::warn!("Studio voice Best: {e}; using Light");
                    Denoiser::Light(nnnoiseless::DenoiseState::new())
                }
            }
        } else {
            Denoiser::Light(nnnoiseless::DenoiseState::new())
        };
        let mode = if matches!(denoise, Denoiser::Best(_)) { BEST } else { LIGHT };
        Voice {
            mode,
            denoise,
            hp: HighPass::new(80.0),
            dyn_: Dynamics::new(),
            pending: Vec::with_capacity(FRAME * 2),
            ready: std::iter::repeat(0.0).take(FRAME).collect(),
            frame_in: vec![0.0; FRAME],
            frame_out: vec![0.0; FRAME],
        }
    }

    pub fn mode(&self) -> u8 {
        self.mode
    }

    /// Clean interleaved stereo microphone samples in place (both channels get the result).
    pub fn process(&mut self, stereo: &mut [f32]) {
        let n = stereo.len() / 2;
        // A microphone that delivers only one channel: use that one.
        let (mut el, mut er) = (0f32, 0f32);
        for f in stereo.chunks_exact(2) {
            el += f[0] * f[0];
            er += f[1] * f[1];
        }
        let pick = |f: &[f32]| if er < el * 0.01 { f[0] } else if el < er * 0.01 { f[1] } else { (f[0] + f[1]) * 0.5 };
        for f in stereo.chunks_exact(2) {
            let x = self.hp.run(pick(f));
            self.pending.push(x);
        }
        while self.pending.len() >= FRAME {
            self.frame_in.copy_from_slice(&self.pending[..FRAME]);
            self.pending.drain(..FRAME);
            self.denoise_frame();
            for i in 0..FRAME {
                let y = self.dyn_.run(self.frame_out[i]);
                self.ready.push_back(y);
            }
        }
        for f in stereo.chunks_exact_mut(2).take(n) {
            let y = self.ready.pop_front().unwrap_or(0.0);
            f[0] = y;
            f[1] = y;
        }
    }

    fn denoise_frame(&mut self) {
        match &mut self.denoise {
            Denoiser::Light(st) => {
                // RNNoise works on 16-bit-scaled samples.
                let input: Vec<f32> = self.frame_in.iter().map(|v| v * 32767.0).collect();
                let mut out = vec![0f32; FRAME];
                st.process_frame(&mut out, &input);
                let floor = 10f32.powf(-ATTEN_LIMIT_DB / 20.0);
                for i in 0..FRAME {
                    let den = out[i] / 32767.0;
                    // Never lower the noise by more than the limit.
                    self.frame_out[i] = den + floor * (self.frame_in[i] - den);
                }
            }
            Denoiser::Best(m) => {
                let noisy = ndarray::ArrayView2::from_shape((1, FRAME), &self.frame_in[..]).expect("frame shape");
                let mut enh = ndarray::Array2::<f32>::zeros((1, FRAME));
                if m.process(noisy, enh.view_mut()).is_ok() {
                    self.frame_out.copy_from_slice(enh.as_slice().unwrap_or(&self.frame_in));
                } else {
                    self.frame_out.copy_from_slice(&self.frame_in);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_plus_noise(secs: f32, tone: f32, noise: f32) -> Vec<f32> {
        let n = (RATE * secs) as usize;
        let mut seed = 7u32;
        (0..n)
            .flat_map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let r = (seed >> 9) as f32 / (1 << 23) as f32 - 0.5;
                let v = tone * (i as f32 * 2.0 * std::f32::consts::PI * 300.0 / RATE).sin() + noise * r;
                [v, v]
            })
            .collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    #[test]
    fn light_lowers_noise_but_never_mutes() {
        let mut v = Voice::new(LIGHT);
        let mut noise = tone_plus_noise(3.0, 0.0, 0.1);
        let before = rms(&noise[noise.len() / 2..]);
        for c in noise.chunks_mut(960) {
            v.process(c);
        }
        let after = rms(&noise[noise.len() / 2..]);
        assert!(after < before * 0.5, "noise not lowered: {before} -> {after}");
        assert!(after > before * 0.005, "noise muted completely: {before} -> {after}");
    }

    #[test]
    fn limiter_holds_the_ceiling() {
        let mut v = Voice::new(LIGHT);
        let mut loud = tone_plus_noise(1.0, 1.5, 0.0);
        for c in loud.chunks_mut(441 * 2) {
            v.process(c);
        }
        assert!(loud.iter().all(|s| s.abs() <= Dynamics::CEILING + 1e-6));
    }

    #[test]
    fn best_runs() {
        let mut v = Voice::new(BEST);
        assert_eq!(v.mode(), BEST);
        let mut s = tone_plus_noise(1.0, 0.3, 0.05);
        for c in s.chunks_mut(960) {
            v.process(c);
        }
        assert!(rms(&s) > 0.01);
    }
}
