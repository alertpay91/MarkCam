//! "Studio voice": cleans the teacher's microphone before it is mixed with the video sound
//! (so music and videos are never touched).
//!
//! 1. A high-pass filter (80 Hz) takes out rumble, desk knocks and hum.
//! 2. Noise suppression — **Light**: RNNoise (Xiph / Mozilla, via the pure-Rust
//!    `nnnoiseless` port, ~0.5 % of a core); **Best**: DeepFilterNet3 (Hendrik Schröter,
//!    ~3–10 % of a core), closer to Krisp. Noise is lowered by at most 15–55 dB (the
//!    Strength slider), never muted: no "gate" that silences the room and then lets the
//!    keyboard through as soon as you speak, and no dead silence between words.
//!    A voice guard watches the model: where the raw microphone clearly holds a voice (its
//!    pitch, 80–400 Hz) but the model took the voice band down by more than ~3 dB, the
//!    raw voice band is mixed back in — the voice is never thinned out by the cleaning.
//! 3. Keyboard and mouse clicks: sharp, very short bursts in the high tones are found (with
//!    3 ms of look-ahead) and only the high tones are lowered for ~25 ms. While you talk
//!    the cut is kept small so consonants (t, k, p, s) stay crisp.
//! 4. A touch of presence (+2 dB around 3 kHz): words stay clear through the lesson app's
//!    own audio compression.
//! 5. A gentle compressor evens out loud and quiet words — it never raises quiet sound by
//!    more than a couple of dB (no automatic gain that pumps the room up).
//! 6. A limiter keeps laughs and shouts from clipping.
//!
//! The rule everywhere: the voice comes first — when in doubt, less noise is removed.
//!
//! Works on 10 ms frames (480 samples at 48 kHz); the delay is 23 ms with Light and 43 ms
//! with Best (DeepFilterNet looks 20 ms ahead) — less than the lesson app's own audio delay.

use std::collections::VecDeque;

pub const OFF: u8 = 0;
pub const LIGHT: u8 = 1;
pub const BEST: u8 = 2;

const RATE: f32 = 48_000.0;
const FRAME: usize = 480;
/// White noise amplitude before the dither's 30 Hz low-pass (~-60 dB after it).
const DITHER_AMP: f32 = 0.07;
/// Strength steps 1..=5: the most the noise is lowered (dB) and DeepFilterNet's post-filter
/// (cleans what is left in loud noise; 0 = off). Stronger takes out louder typing but can
/// color the voice when the noise is loud.
const STRENGTHS: [(f32, f32); 5] = [(15.0, 0.0), (22.0, 0.0), (30.0, 0.0), (40.0, 0.02), (55.0, 0.04)];
/// Best: DeepFilterNet already removes more by itself, so its limits are a little lower; the
/// post-filter only on Strong.
const STRENGTHS_BEST: [(f32, f32); 5] = [(14.0, 0.0), (20.0, 0.0), (26.0, 0.0), (32.0, 0.0), (40.0, 0.02)];
pub const DEFAULT_STRENGTH: u8 = 3;

fn strength(mode: u8, level: u8) -> (f32, f32) {
    let table = if mode == BEST { &STRENGTHS_BEST } else { &STRENGTHS };
    table[(level.clamp(1, 5) - 1) as usize]
}

enum Denoiser {
    Light(Box<nnnoiseless::DenoiseState<'static>>),
    Best(Box<df::tract::DfTract>),
}

impl Denoiser {
    /// The model's own delay (measured by `measure_delays`): the dry signal is delayed by
    /// the same amount before the two are mixed.
    fn delay(&self) -> usize {
        match self {
            Denoiser::Light(_) => 480,
            Denoiser::Best(_) => 1440,
        }
    }
}

/// A biquad filter (RBJ cookbook, transposed direct form II).
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn coeffs(freq: f32, q: f32) -> (f32, f32) {
        let (sin, cos) = (2.0 * std::f32::consts::PI * freq / RATE).sin_cos();
        (sin / (2.0 * q), cos)
    }

    fn make(b: [f32; 3], a0: f32, a1: f32, a2: f32) -> Self {
        Biquad { b: [b[0] / a0, b[1] / a0, b[2] / a0], a: [a1 / a0, a2 / a0], z: [0.0; 2] }
    }

    /// Second-order Butterworth high-pass.
    fn highpass(freq: f32) -> Self {
        let (alpha, cos) = Self::coeffs(freq, std::f32::consts::FRAC_1_SQRT_2);
        let b0 = (1.0 + cos) / 2.0;
        Self::make([b0, -2.0 * b0, b0], 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    /// Second-order Butterworth low-pass.
    fn lowpass(freq: f32) -> Self {
        let (alpha, cos) = Self::coeffs(freq, std::f32::consts::FRAC_1_SQRT_2);
        let b0 = (1.0 - cos) / 2.0;
        Self::make([b0, 2.0 * b0, b0], 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    /// Peaking EQ: `gain_db` around `freq`.
    fn peaking(freq: f32, q: f32, gain_db: f32) -> Self {
        let (alpha, cos) = Self::coeffs(freq, q);
        let a = 10f32.powf(gain_db / 40.0);
        Self::make([1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a], 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Keyboard / mouse click softener (see the module notes).
struct Clicks {
    split: Biquad,
    /// The signal, split into low and high tones, delayed by the look-ahead.
    delay: VecDeque<(f32, f32)>,
    fast: f32,
    slow: f32,
    low_env: f32,
    hold: usize,
    gain: f32,
    /// Deepest cut (linear) in pauses and while talking.
    cut_pause: f32,
    cut_talk: f32,
    /// Detections so far (diagnostics).
    hits: usize,
    jump: f32,
    talk_db: f32,
}

impl Clicks {
    const LOOKAHEAD: usize = 144; // 3 ms
    const HOLD: usize = 1200; // 25 ms
    /// Cut in pauses (dB) per strength step; while talking at most `TALK_DB`.
    const PAUSE_DB: [f32; 5] = [6.0, 10.0, 14.0, 18.0, 24.0];
    const TALK_DB: f32 = 10.0;
    /// How far the high tones must jump above their recent level to count as a click.
    const JUMP: f32 = 3.0;

    fn new(level: u8) -> Self {
        let mut c = Clicks {
            split: Biquad::lowpass(1800.0),
            delay: std::iter::repeat((0.0, 0.0)).take(Self::LOOKAHEAD).collect(),
            fast: 0.0,
            slow: 0.0,
            low_env: 0.0,
            hold: 0,
            gain: 1.0,
            cut_pause: 1.0,
            cut_talk: 1.0,
            hits: 0,
            jump: Self::JUMP,
            talk_db: Self::TALK_DB,
        };
        #[cfg(test)]
        {
            let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
            if let Some(v) = env("OVC_CLICK_SPLIT") {
                c.split = Biquad::lowpass(v);
            }
            if let Some(v) = env("OVC_CLICK_JUMP") {
                c.jump = v;
            }
            if let Some(v) = env("OVC_CLICK_TALK_DB") {
                c.talk_db = v;
            }
        }
        c.set_level(level);
        c
    }

    fn set_level(&mut self, level: u8) {
        let db = Self::PAUSE_DB[(level.clamp(1, 5) - 1) as usize];
        self.cut_pause = 10f32.powf(-db / 20.0);
        self.cut_talk = 10f32.powf(-db.min(self.talk_db) / 20.0);
    }

    fn env(e: &mut f32, x: f32, attack_s: f32, release_s: f32) {
        let t = if x > *e { attack_s } else { release_s };
        *e += (x - *e) * (1.0 - (-1.0 / (t * RATE)).exp());
    }

    fn run(&mut self, x: f32) -> f32 {
        let low = self.split.run(x);
        let high = x - low;
        let h = high.abs();
        Self::env(&mut self.fast, h, 0.0002, 0.003);
        Self::env(&mut self.slow, h, 0.020, 0.150);
        Self::env(&mut self.low_env, low.abs(), 0.005, 0.060);
        // A click: the high tones jump far above their recent level within a fraction of a
        // millisecond (speech, even "t" / "k", rises more slowly).
        if self.fast > self.jump * self.slow.max(1e-5) && self.fast > 0.002 {
            if self.hold == 0 {
                self.hits += 1;
            }
            self.hold = Self::HOLD + Self::LOOKAHEAD;
        }
        let target = if self.hold > 0 {
            self.hold -= 1;
            // Talking (strong low tones under the click): only a small cut.
            let talking = self.low_env > 0.01 && self.low_env > 0.5 * self.fast;
            if talking { self.cut_talk } else { self.cut_pause }
        } else {
            1.0
        };
        // Down in 0.5 ms, back up in 8 ms (no audible pumping).
        let k = if target < self.gain { 1.0 - (-1.0 / (0.0005 * RATE)).exp() } else { 1.0 - (-1.0 / (0.008 * RATE)).exp() };
        self.gain += (target - self.gain) * k;
        self.delay.push_back((low, high));
        let (l, hh) = self.delay.pop_front().unwrap_or((0.0, 0.0));
        l + hh * self.gain
    }
}

/// Gives the voice band back where the model took a clearly present voice down too much.
struct VoiceGuard {
    dry_lp: Biquad,
    wet_lp: Biquad,
    /// The dry voice band, every 4th sample (12 kHz), for the pitch check.
    hist: VecDeque<f32>,
    mix: f32,
    /// Frames the guard stepped in (diagnostics).
    hits: usize,
}

impl VoiceGuard {
    /// Keep at least this much of the voice band's energy (-3 dB).
    const KEEP: f32 = 0.5;

    fn new() -> Self {
        VoiceGuard { dry_lp: Biquad::lowpass(2500.0), wet_lp: Biquad::lowpass(2500.0), hist: VecDeque::with_capacity(320), mix: 0.0, hits: 0 }
    }

    /// Periodicity at 80–400 Hz (0..1) of the recent dry voice band.
    fn voicing(&self) -> f32 {
        let x: Vec<f32> = self.hist.iter().copied().collect();
        let e0: f32 = x.iter().map(|v| v * v).sum::<f32>();
        if e0 < 1e-7 {
            return 0.0;
        }
        (30..150)
            .map(|lag| {
                let (a, b) = (&x[..x.len() - lag], &x[lag..]);
                let num: f32 = a.iter().zip(b).map(|(p, q)| p * q).sum();
                let den = (a.iter().map(|v| v * v).sum::<f32>() * b.iter().map(|v| v * v).sum::<f32>()).sqrt();
                num / den.max(1e-9)
            })
            .fold(0f32, f32::max)
    }

    /// `wet` (the model's output) gets back some of `dry`'s voice band if needed.
    fn run(&mut self, dry: &[f32], wet: &mut [f32]) {
        let dlb: Vec<f32> = dry.iter().map(|&v| self.dry_lp.run(v)).collect();
        let wlb: Vec<f32> = wet.iter().map(|&v| self.wet_lp.run(v)).collect();
        for v in dlb.iter().step_by(4) {
            if self.hist.len() == 320 {
                self.hist.pop_front();
            }
            self.hist.push_back(*v);
        }
        let ed: f32 = dlb.iter().map(|v| v * v).sum::<f32>() / dlb.len() as f32;
        let ew: f32 = wlb.iter().map(|v| v * v).sum::<f32>() / wlb.len() as f32;
        // A clear voice (above -55 dB) that lost more than 3 dB in the voice band.
        let mut target = 0.0;
        if ed > 3e-6 && ew < Self::KEEP * ed && self.hist.len() == 320 && self.voicing() > 0.6 {
            let aw = (ew / ed).sqrt();
            let keep = Self::KEEP.sqrt();
            target = ((keep - aw) / (1.0 - aw).max(1e-3)).clamp(0.0, 1.0);
            self.hits += 1;
        }
        // Step in at once, let go over ~50 ms.
        let start = self.mix;
        let end = if target > self.mix { target } else { self.mix + (target - self.mix) * 0.2 };
        let n = wet.len() as f32;
        for (i, w) in wet.iter_mut().enumerate() {
            let m = start + (end - start) * (i as f32 + 1.0) / n;
            *w += m * (dlb[i] - wlb[i]);
        }
        self.mix = end;
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
    level: u8,
    atten_db: f32,
    denoise: Denoiser,
    hp: Biquad,
    guard: VoiceGuard,
    clicks: Clicks,
    presence: Biquad,
    dyn_: Dynamics,
    /// Mono input waiting for a full frame, and processed output (one frame behind).
    pending: Vec<f32>,
    ready: VecDeque<f32>,
    frame_in: Vec<f32>,
    frame_out: Vec<f32>,
    /// The input, delayed like the model's output.
    dry: VecDeque<f32>,
    /// Noise generator state and its filters (see denoise_frame).
    dither: u32,
    dither_lp: [Biquad; 2],
    /// Takes the sub-audible dither back out after the model.
    post_hp: [Biquad; 2],
}

impl Voice {
    /// `mode`: [`LIGHT`] or [`BEST`] (Best falls back to Light if its model can't load).
    pub fn new(mode: u8, level: u8) -> Self {
        let (atten_db, beta) = strength(mode, level);
        let denoise = if mode == BEST {
            let rp = df::tract::RuntimeParams::default_with_ch(1).with_atten_lim(100.0).with_post_filter(beta);
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
        let dry = std::iter::repeat(0.0).take(denoise.delay()).collect();
        Voice {
            mode,
            level: level.clamp(1, 5),
            atten_db,
            denoise,
            hp: Biquad::highpass(80.0),
            guard: VoiceGuard::new(),
            clicks: Clicks::new(level),
            presence: Biquad::peaking(3000.0, 1.0, 2.0),
            dyn_: Dynamics::new(),
            pending: Vec::with_capacity(FRAME * 2),
            ready: std::iter::repeat(0.0).take(FRAME).collect(),
            frame_in: vec![0.0; FRAME],
            frame_out: vec![0.0; FRAME],
            dry,
            dither: 0x1234_5678,
            dither_lp: [Biquad::lowpass(20.0), Biquad::lowpass(20.0)],
            post_hp: [Biquad::highpass(70.0), Biquad::highpass(70.0)],
        }
    }

    /// Total delay this adds (samples at 48 kHz).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn latency(&self) -> usize {
        FRAME + self.denoise.delay() + Clicks::LOOKAHEAD
    }

    pub fn mode(&self) -> u8 {
        self.mode
    }

    /// Change the strength (1..=5) while running.
    pub fn set_strength(&mut self, level: u8) {
        let level = level.clamp(1, 5);
        if level == self.level {
            return;
        }
        let (atten_db, beta) = strength(self.mode, level);
        self.level = level;
        self.atten_db = atten_db;
        self.clicks.set_level(level);
        if let Denoiser::Best(m) = &mut self.denoise {
            m.set_pf_beta(beta);
        }
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
                let y = self.clicks.run(self.frame_out[i]);
                let y = self.presence.run(y);
                let y = self.dyn_.run(y);
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
        // The dry input for this output frame (delayed like the model).
        self.dry.extend(self.frame_in.iter().copied());
        let dry: Vec<f32> = self.dry.drain(..FRAME).collect();
        match &mut self.denoise {
            Denoiser::Light(st) => {
                // RNNoise works on 16-bit-scaled samples.
                let input: Vec<f32> = self.frame_in.iter().map(|v| v * 32767.0).collect();
                let mut out = vec![0f32; FRAME];
                st.process_frame(&mut out, &input);
                for (o, v) in self.frame_out.iter_mut().zip(&out) {
                    *o = v / 32767.0;
                }
            }
            Denoiser::Best(m) => {
                // DeepFilterNet outputs hard zeros for input below -70 dB and jumps back when
                // a sound comes: in a quiet room that turns every mouse click into a pop. A
                // faint rumble below hearing (~-60 dB, under 30 Hz) keeps it running; a high-
                // pass after the model takes it out again. (White noise did the same job but
                // the model lets its highs through while you talk: a hiss on the voice.)
                if !(cfg!(test) && std::env::var_os("OVC_NO_DITHER").is_some()) {
                    for v in self.frame_in.iter_mut() {
                        self.dither = self.dither.wrapping_mul(1664525).wrapping_add(1013904223);
                        let w = ((self.dither >> 9) as f32 / (1 << 23) as f32 - 0.5) * DITHER_AMP;
                        let [lp0, lp1] = &mut self.dither_lp;
                        *v += lp1.run(lp0.run(w));
                    }
                }
                let noisy = ndarray::ArrayView2::from_shape((1, FRAME), &self.frame_in[..]).expect("frame shape");
                let mut enh = ndarray::Array2::<f32>::zeros((1, FRAME));
                match m.process(noisy, enh.view_mut()) {
                    Ok(_) => self.frame_out.copy_from_slice(enh.as_slice().unwrap_or(&dry)),
                    Err(_) => self.frame_out.copy_from_slice(&dry),
                }
                let [hp0, hp1] = &mut self.post_hp;
                for o in self.frame_out.iter_mut() {
                    *o = hp1.run(hp0.run(*o));
                }
            }
        }
        if !(cfg!(test) && std::env::var_os("OVC_NO_GUARD").is_some()) {
            self.guard.run(&dry, &mut self.frame_out);
        }
        // Never lower the noise by more than the limit: a little of the room stays, so
        // pauses don't fall into dead silence (that sounds like a gate).
        let floor = 10f32.powf(-self.atten_db / 20.0);
        for (o, d) in self.frame_out.iter_mut().zip(&dry) {
            *o += floor * (d - *o);
        }
    }
}

/// Save interleaved stereo 48 kHz samples as a 16-bit WAV (the voice test, for tuning).
pub fn write_wav(path: &std::path::Path, stereo: &[f32]) -> std::io::Result<()> {
    let data_len = (stereo.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&2u16.to_le_bytes()); // stereo
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in stereo {
        b.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, b)
}

/// Read a WAV written by [`write_wav`] (16-bit stereo 48 kHz).
#[cfg(test)]
fn read_wav(path: &std::path::Path) -> Vec<f32> {
    let b = std::fs::read(path).expect("read wav");
    b[44..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32767.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tuning on a real recording: OVC_VOICE_WAV=<voice-test.wav> writes every mode ×
    /// strength next to it and prints how much of the voice (100–4000 Hz energy where the
    /// raw recording is loud) is kept and how much of the click / noise energy is left.
    #[test]
    #[ignore]
    fn tune_on_recording() {
        let Some(p) = std::env::var_os("OVC_VOICE_WAV") else { return };
        let p = std::path::PathBuf::from(p);
        let raw = read_wav(&p);
        let mono = |s: &[f32]| s.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect::<Vec<f32>>();
        let r = mono(&raw);
        // 20 ms blocks: loud ones are speech, quiet ones are pauses (noise, typing).
        let blk = 960usize;
        let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt();
        let levels: Vec<f32> = r.chunks(blk).map(rms).collect();
        let mut sorted = levels.clone();
        sorted.sort_by(f32::total_cmp);
        let loud = sorted[sorted.len() * 7 / 10];
        let quiet = sorted[sorted.len() * 3 / 10];
        for mode in [LIGHT, BEST] {
            for level in 1..=5u8 {
                let mut s = raw.clone();
                let mut v = Voice::new(mode, level);
                let t0 = std::time::Instant::now();
                for c in s.chunks_mut(960) {
                    v.process(c);
                }
                let cpu = t0.elapsed().as_secs_f32() / (raw.len() as f32 / 2.0 / RATE) * 100.0;
                println!("  ({cpu:.1}% of a core)");
                let o = mono(&s);
                let o = &o[v.latency()..];
                let (mut speech_in, mut speech_out, mut pause_in, mut pause_out) = (0f32, 0f32, 0f32, 0f32);
                for (i, &l) in levels.iter().enumerate() {
                    let a = &r[i * blk..((i + 1) * blk).min(r.len())];
                    let b = &o[(i * blk).min(o.len())..((i + 1) * blk).min(o.len())];
                    if l >= loud {
                        speech_in += rms(a).powi(2);
                        speech_out += rms(b).powi(2);
                    } else if l <= quiet {
                        pause_in += rms(a).powi(2);
                        pause_out += rms(b).powi(2);
                    }
                }
                let db = |a: f32, b: f32| 10.0 * (b.max(1e-12) / a.max(1e-12)).log10();
                println!(
                    "{} strength {level}: speech {:+.1} dB, pauses (noise / typing) {:+.1} dB",
                    if mode == BEST { "Best " } else { "Light" },
                    db(speech_in, speech_out),
                    db(pause_in, pause_out)
                );
                let out = p.with_file_name(format!("voice-{}-{level}.wav", if mode == BEST { "best" } else { "light" }));
                write_wav(&out, &s).expect("write");
            }
        }
    }

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
        let mut v = Voice::new(LIGHT, DEFAULT_STRENGTH);
        let mut noise = tone_plus_noise(3.0, 0.0, 0.1);
        let before = rms(&noise[noise.len() / 2..]);
        for c in noise.chunks_mut(960) {
            v.process(c);
        }
        let after = rms(&noise[noise.len() / 2..]);
        assert!(after < before * 0.5, "noise not lowered: {before} -> {after}");
        assert!(after > before * 0.005, "noise muted completely: {before} -> {after}");
    }

    /// Prints each denoiser's own delay (samples), found by cross-correlating a noisy
    /// "voice" (pulsed harmonic tone) with the output.
    #[test]
    #[ignore]
    fn measure_delays() {
        let n = RATE as usize * 3;
        let mut seed = 11u32;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let r = (seed >> 9) as f32 / (1 << 23) as f32 - 0.5;
                let on = if (i / 6000) % 2 == 0 { 1.0 } else { 0.0 };
                let ph = i as f32 * 2.0 * std::f32::consts::PI * 180.0 / RATE;
                on * 0.2 * (ph.sin() + 0.5 * (2.0 * ph).sin() + 0.3 * (3.0 * ph).sin()) + 0.005 * r
            })
            .collect();
        for (name, mut d) in [
            ("rnnoise", Denoiser::Light(nnnoiseless::DenoiseState::new())),
            ("deepfilternet", Denoiser::Best(Box::new(df::tract::DfTract::new(df::tract::DfParams::default(), &df::tract::RuntimeParams::default_with_ch(1)).unwrap()))),
        ] {
            let mut y = Vec::with_capacity(n);
            for c in x.chunks_exact(FRAME) {
                let mut out = vec![0f32; FRAME];
                match &mut d {
                    Denoiser::Light(st) => {
                        let inp: Vec<f32> = c.iter().map(|v| v * 32767.0).collect();
                        st.process_frame(&mut out, &inp);
                        out.iter_mut().for_each(|v| *v /= 32767.0);
                    }
                    Denoiser::Best(m) => {
                        let noisy = ndarray::ArrayView2::from_shape((1, FRAME), c).unwrap();
                        let mut enh = ndarray::Array2::<f32>::zeros((1, FRAME));
                        m.process(noisy, enh.view_mut()).unwrap();
                        out.copy_from_slice(enh.as_slice().unwrap());
                    }
                }
                y.extend(out);
            }
            let (mut best, mut lag) = (f32::MIN, 0);
            for l in 0..3000 {
                let c: f32 = (24000..96000).map(|i| x[i] * y[i + l]).sum();
                if c > best {
                    best = c;
                    lag = l;
                }
            }
            println!("{name}: delay {lag} samples ({:.1} ms)", lag as f32 / 48.0);
        }
    }

    /// OVC_VOICE_WAV=<wav> OVC_VOICE_MODE=best|light OVC_VOICE_LEVEL=1..5: a 50 ms timeline
    /// of the recording — voice band (150–2500 Hz) and click band (>3 kHz) in and out, and
    /// whether the raw block holds a sharp transient (keyboard / mouse).
    #[test]
    #[ignore]
    fn analyze_recording() {
        let Some(p) = std::env::var_os("OVC_VOICE_WAV") else { return };
        let raw = read_wav(std::path::Path::new(&p));
        let mode = if std::env::var("OVC_VOICE_MODE").as_deref() == Ok("light") { LIGHT } else { BEST };
        let level: u8 = std::env::var("OVC_VOICE_LEVEL").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
        let mut s = raw.clone();
        let mut v = Voice::new(mode, level);
        for c in s.chunks_mut(960) {
            v.process(c);
        }
        let mono = |s: &[f32]| s.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect::<Vec<f32>>();
        let r = mono(&raw);
        let o: Vec<f32> = mono(&s)[v.latency()..].to_vec();
        let band = |x: &[f32], lo: f32, hi: f32| {
            let (mut a, mut b) = (Biquad::highpass(lo), Biquad::lowpass(hi));
            x.iter().map(|&v| b.run(a.run(v))).collect::<Vec<f32>>()
        };
        let (rv, ov) = (band(&r, 150.0, 2500.0), band(&o, 150.0, 2500.0));
        let (rc, oc) = (band(&r, 3000.0, 20000.0), band(&o, 3000.0, 20000.0));
        let db = |x: &[f32]| 10.0 * (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32 + 1e-12).log10();
        let peak_ratio = |x: &[f32]| {
            let pk = x.iter().fold(0f32, |m, v| m.max(v.abs()));
            let rms = (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
            pk / rms.max(1e-9)
        };
        // Voiced speech: strong periodicity at 80–400 Hz in the voice band.
        let voicing = |x: &[f32]| {
            let e0: f32 = x.iter().map(|v| v * v).sum::<f32>().max(1e-9);
            (120..600).step_by(2).map(|lag| x.iter().zip(&x[lag..]).map(|(a, b)| a * b).sum::<f32>() / e0).fold(0f32, f32::max)
        };
        println!("click detections: {}, voice guard frames: {}", v.clicks.hits, v.guard.hits);
        let blk = 2400;
        let n = r.len().min(o.len()) / blk;
        let (mut voiced_in, mut voiced_out, mut voiced_n, mut voiced_lost) = (0f32, 0f32, 0, 0);
        let (mut talk_click_in, mut talk_click_out, mut voice_in, mut voice_out) = (0f32, 0f32, 0f32, 0f32);
        let pk = |x: &[f32]| 20.0 * x.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6).log10();
        let (mut pk_talk, mut pk_pause, mut n_talk, mut n_pause) = (0f32, 0f32, 0, 0);
        let (mut pause_in, mut pause_out) = (0f32, 0f32);
        let (mut cons_in, mut cons_out) = (0f32, 0f32);
        let (rh, oh) = (band(&r, 8000.0, 16000.0), band(&o, 8000.0, 16000.0));
        let (mut hiss_in, mut hiss_out) = (0f32, 0f32);
        println!(" time  voice in/out   clicks in/out  crest  ");
        for i in 0..n {
            let rg = i * blk..(i + 1) * blk;
            let (vi, vo, ci, co) = (db(&rv[rg.clone()]), db(&ov[rg.clone()]), db(&rc[rg.clone()]), db(&oc[rg.clone()]));
            let crest = peak_ratio(&rc[rg.clone()]);
            let voiced = vi > -55.0 && voicing(&rv[rg.clone()]) > 0.5;
            if voiced {
                voiced_in += 10f32.powf(vi / 10.0);
                voiced_out += 10f32.powf(vo / 10.0);
                voiced_n += 1;
                if vo < vi - 4.0 {
                    voiced_lost += 1;
                }
            }
            let talking = vi > -45.0;
            let click = crest > 6.0 && ci > -60.0;
            if click {
                let dp = pk(&oc[rg.clone()]) - pk(&rc[rg.clone()]);
                if talking {
                    pk_talk += dp;
                    n_talk += 1;
                } else {
                    pk_pause += dp;
                    n_pause += 1;
                }
            }
            if voiced && !click {
                hiss_in += 10f32.powf(db(&rh[rg.clone()]) / 10.0);
                hiss_out += 10f32.powf(db(&oh[rg.clone()]) / 10.0);
                cons_in += 10f32.powf(ci / 10.0);
                cons_out += 10f32.powf(co / 10.0);
            }
            if vi < -60.0 {
                pause_in += 10f32.powf(db(&r[rg.clone()]) / 10.0);
                pause_out += 10f32.powf(db(&o[rg.clone()]) / 10.0);
            }
            if talking {
                voice_in += 10f32.powf(vi / 10.0);
                voice_out += 10f32.powf(vo / 10.0);
                if click {
                    talk_click_in += 10f32.powf(ci / 10.0);
                    talk_click_out += 10f32.powf(co / 10.0);
                }
            }
            println!(
                "{:5.2}s {:6.1} {:6.1}   {:6.1} {:6.1}  {:4.1} {}{}",
                i as f32 * 0.05,
                vi,
                vo,
                ci,
                co,
                crest,
                if voiced { "VOICE " } else if talking { "talk  " } else { "      " },
                if click { "CLICK" } else { "" }
            );
        }
        let d = |a: f32, b: f32| 10.0 * (b.max(1e-12) / a.max(1e-12)).log10();
        println!("voice band while talking: {:+.1} dB; clicks while talking: {:+.1} dB", d(voice_in, voice_out), d(talk_click_in, talk_click_out));
        println!(
            "click peaks: while talking {:+.1} dB ({n_talk}), in pauses {:+.1} dB ({n_pause}); quiet room {:+.1} dB; consonants {:+.1} dB",
            pk_talk / n_talk.max(1) as f32,
            pk_pause / n_pause.max(1) as f32,
            d(pause_in, pause_out),
            d(cons_in, cons_out)
        );
        println!("8-16 kHz while talking: in {:.1} dB, out {:.1} dB", 10.0 * hiss_in.max(1e-12).log10(), 10.0 * hiss_out.max(1e-12).log10());
        println!("voiced speech ({voiced_n} blocks): {:+.1} dB, {voiced_lost} blocks lost more than 4 dB", d(voiced_in, voiced_out));
    }

    #[test]
    fn clicks_are_softened_but_steady_sound_is_not() {
        let n = RATE as usize * 2;
        let mut seed = 3u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 9) as f32 / (1 << 23) as f32 - 0.5
        };
        // A steady low tone (voice-like) plus a click every 250 ms (5 ms decaying noise burst).
        let tone: Vec<f32> = (0..n).map(|i| 0.05 * (i as f32 * 2.0 * std::f32::consts::PI * 200.0 / RATE).sin()).collect();
        let mut x = tone.clone();
        let mut click_at = Vec::new();
        for c in (0..n).step_by(12000).skip(1) {
            click_at.push(c);
            for j in 0..240 {
                x[c + j] += 0.4 * rnd() * (-(j as f32) / 50.0).exp();
            }
        }
        let mut f = Clicks::new(DEFAULT_STRENGTH);
        let y: Vec<f32> = x.iter().map(|&s| f.run(s)).collect();
        let d = Clicks::LOOKAHEAD;
        let click_energy = |s: &[f32], off: usize| click_at.iter().map(|&c| (0..240).map(|j| (s[c + j + off] - tone[c + j]).powi(2)).sum::<f32>()).sum::<f32>();
        let before = click_energy(&x, 0);
        let after = click_energy(&y, d);
        assert!(after < before * 0.5, "clicks not softened: {before} -> {after}");
        // Between clicks the tone passes unchanged (after the delay).
        let mid = 6000;
        let err: f32 = (mid..mid + 2000).map(|i| (y[i + d] - tone[i]).abs()).fold(0.0, f32::max);
        assert!(err < 0.01, "steady sound changed by {err}");
    }

    #[test]
    fn limiter_holds_the_ceiling() {
        let mut v = Voice::new(LIGHT, 5);
        let mut loud = tone_plus_noise(1.0, 1.5, 0.0);
        for c in loud.chunks_mut(441 * 2) {
            v.process(c);
        }
        assert!(loud.iter().all(|s| s.abs() <= Dynamics::CEILING + 1e-6));
    }

    #[test]
    fn best_runs() {
        let mut v = Voice::new(BEST, 1);
        assert_eq!(v.mode(), BEST);
        v.set_strength(5);
        let mut s = tone_plus_noise(1.0, 0.3, 0.05);
        for c in s.chunks_mut(960) {
            v.process(c);
        }
        assert!(rms(&s) > 0.01);
    }
}
