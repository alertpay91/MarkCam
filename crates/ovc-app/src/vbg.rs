//! Virtual background: replace (or blur) everything behind the person.
//!
//! How it is made to look right, not like a cut-out:
//! 1. **MODNet** (portrait *matting*, Apache-2.0) gives a soft alpha matte instead of a
//!    blocky person/background mask — hair, ears, a headset band and the gap between the
//!    band and the head come out as they are. It runs through ONNX Runtime on the GPU
//!    (DirectML: any DX12 GPU, Intel HD 620 included), on its own thread.
//! 2. The matte is **refined against the real picture** (a guided filter at 640 px wide,
//!    with the frame's brightness as the guide): edges snap to where the image actually
//!    changes, so a thin strip of wall between headset and scalp is cut out instead of
//!    smeared over.
//! 3. **Temporal smoothing** keeps edges from shimmering between frames.
//! 4. Every camera frame is then composited at full resolution with the newest matte.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The segmentation model file (in a models folder).
pub const MODEL: &str = "modnet_webcam.onnx";
/// The video matting model, used instead when installed (next to [`MODEL`]).
pub const RVM_MODEL: &str = "rvm_mobilenetv3_fp32.onnx";
/// The larger, more accurate RVM (fingers, fast hands) for stronger graphics chips.
pub const RVM_BEST_MODEL: &str = "rvm_resnet50_fp32.onnx";
/// Google MediaPipe Selfie Segmentation (landscape, Apache-2.0): the fast model, for
/// laptops and the "Light" edge quality.
pub const SELFIE_MODEL: &str = "selfie_landscape.onnx";

/// Edge quality choice: 0 = auto (best if the graphics chip keeps up), 1 = best, 2 = light.
pub const QUALITY_AUTO: u8 = 0;
pub const QUALITY_BEST: u8 = 1;
pub const QUALITY_LIGHT: u8 = 2;

/// What replaces the background.
#[derive(Clone, PartialEq, Debug)]
pub enum Choice {
    Off,
    Blur,
    /// One of [`BUILTIN`].
    Builtin(usize),
    /// A picture or video file.
    File(PathBuf),
}

impl Choice {
    pub fn id(&self) -> String {
        match self {
            Choice::Off => "off".into(),
            Choice::Blur => "blur".into(),
            Choice::Builtin(i) => format!("builtin:{i}"),
            Choice::File(p) => format!("file:{}", p.display()),
        }
    }

    pub fn from_id(s: &str) -> Choice {
        if s == "blur" {
            Choice::Blur
        } else if let Some(i) = s.strip_prefix("builtin:").and_then(|i| i.parse().ok()).filter(|i| *i < BUILTIN.len()) {
            Choice::Builtin(i)
        } else if let Some(p) = s.strip_prefix("file:") {
            Choice::File(PathBuf::from(p))
        } else {
            Choice::Off
        }
    }
}

/// Backgrounds drawn by MarkCam itself (no files needed).
pub const BUILTIN: [&str; 6] = ["Soft blue", "Sunset", "Mint", "Graphite", "Bokeh lights", "Chalkboard"];

/// Draw built-in background `i` at `w`x`h`.
pub fn builtin(i: usize, w: u32, h: u32) -> Vec<u8> {
    let (wf, hf) = (w as f32, h as f32);
    let lerp = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
    // (top-left color, bottom-right color)
    let (c0, c1) = match i {
        0 => ([0.55, 0.72, 0.92], [0.20, 0.33, 0.62]),
        1 => ([0.98, 0.72, 0.45], [0.55, 0.25, 0.45]),
        2 => ([0.72, 0.92, 0.84], [0.25, 0.55, 0.52]),
        3 => ([0.32, 0.34, 0.38], [0.12, 0.13, 0.15]),
        4 => ([0.10, 0.12, 0.22], [0.03, 0.04, 0.09]),
        _ => ([0.16, 0.27, 0.22], [0.09, 0.17, 0.13]),
    };
    let mut out = vec![0u8; (w * h * 4) as usize];
    // Soft out-of-focus light circles for "Bokeh lights" (fixed pseudo-random layout).
    let mut lights: Vec<(f32, f32, f32, [f32; 3])> = Vec::new();
    if i == 4 {
        let mut s = 0x2545_f491u32;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s % 10_000) as f32 / 10_000.0
        };
        for _ in 0..26 {
            let col = [[1.0, 0.78, 0.45], [0.55, 0.75, 1.0], [1.0, 0.55, 0.65]][(rnd() * 3.0) as usize % 3];
            lights.push((rnd() * wf, rnd() * hf, (0.03 + rnd() * 0.07) * wf, col));
        }
    }
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / wf, y as f32 / hf);
            let mut c = lerp(c0, c1, (u * 0.35 + v * 0.65).clamp(0.0, 1.0));
            // Gentle vignette-like light from the upper middle.
            let d = ((u - 0.5) * (u - 0.5) + (v - 0.25) * (v - 0.25)).sqrt();
            let glow = (1.0 - d * 1.6).clamp(0.0, 1.0) * 0.12;
            c = [c[0] + glow, c[1] + glow, c[2] + glow];
            for &(lx, ly, r, col) in &lights {
                let dd = (((x as f32 - lx).powi(2) + (y as f32 - ly).powi(2)).sqrt() / r).min(1.5);
                let a = if dd < 1.0 { 0.35 * (1.0 - dd.powi(6)) } else { 0.0 };
                c = lerp(c, col, a);
            }
            if i == 5 {
                // Chalk dust: faint horizontal streaks.
                let n = ((x.wrapping_mul(73) ^ y.wrapping_mul(151)) % 97) as f32 / 97.0;
                c = [c[0] + n * 0.03, c[1] + n * 0.03, c[2] + n * 0.03];
            }
            let o = ((y * w + x) * 4) as usize;
            for k in 0..3 {
                out[o + k] = (c[k].clamp(0.0, 1.0) * 255.0) as u8;
            }
            out[o + 3] = 255;
        }
    }
    out
}

/// Folder for the user's own backgrounds.
pub fn my_backgrounds_dir() -> PathBuf {
    crate::settings::app_dir().join("Backgrounds")
}

/// Picture/video files offered as backgrounds: the user's own, then ManyCam's (if
/// installed), then any bundled with the installer. `(group, path)`.
pub fn files() -> Vec<(&'static str, PathBuf)> {
    let mut out = Vec::new();
    let mut add = |group: &'static str, dir: PathBuf| {
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let mut v: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| is_background_file(p))
            .collect();
        v.sort();
        out.extend(v.into_iter().map(|p| (group, p)));
    };
    add("My backgrounds", my_backgrounds_dir());
    if let Some(d) = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("backgrounds"))) {
        add("MarkCam pictures", d);
    }
    // Development builds: the generated set (installed builds have it next to the exe).
    add("MarkCam pictures", crate::settings::app_dir().join("PackBackgrounds"));
    if crate::library::use_manycam() {
        if let Some(d) = std::env::var_os("APPDATA") {
            add("ManyCam", Path::new(&d).join("ManyCam").join("Backgrounds"));
        }
    }
    // The installer ships ManyCam's set too: show each picture once.
    let mut seen = std::collections::HashSet::new();
    out.retain(|(_, p)| seen.insert(p.file_name().map(|n| n.to_ascii_lowercase())));
    out
}

pub fn is_background_file(p: &Path) -> bool {
    let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "bmp" | "webp") || crate::video::is_video_file(p)
}

/// Where `onnxruntime.dll` is: next to the exe (installed), else in the app data folder.
pub fn runtime_dir() -> Option<PathBuf> {
    let name = if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" };
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    [exe_dir, Some(crate::settings::app_dir().join("runtime"))].into_iter().flatten().find(|d| d.join(name).is_file())
}

/// The segmentation model, if installed.
pub fn model_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("models")));
    [exe_dir, Some(crate::settings::app_dir().join("models"))].into_iter().flatten().map(|d| d.join(MODEL)).find(|p| p.is_file())
}

/// A refined matte: alpha 0..255 at `w`x`h` (smaller than the frame; upsampled when used).
pub struct Matte {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
    /// The camera frame it was made from.
    pub seq: u64,
    /// The person's colors with the background's tint removed (RGB, same size), for the
    /// outline pixels — when the model provides them (RVM).
    pub fgr: Option<Vec<u8>>,
}

type Latest = Arc<Mutex<Option<(u64, u32, u32, Arc<[u8]>)>>>;
/// Recent mattes shared between the segmentation thread and the compositor.
pub type Mattes = Arc<Mutex<std::collections::VecDeque<Arc<Matte>>>>;

/// Runs the segmentation on its own thread while a background is chosen.
pub struct SegWorker {
    pub want: Arc<AtomicBool>,
    run: Arc<AtomicBool>,
    /// The most recent mattes, oldest first (each camera frame is shown with its own).
    pub matte: Mattes,
    /// "GPU · 21 ms" / an error, for the UI.
    pub status: Arc<Mutex<String>>,

    /// Edge quality (`QUALITY_*`); the worker switches models when it changes.
    pub quality: Arc<AtomicU8>,
    /// Mattes come fast enough (a fast graphics chip) for each frame to wait for its own;
    /// otherwise frames go on with the newest matte (the picture never stalls).
    pub fast: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl SegWorker {
    /// `integrated_gpu`: the graphics chip is built into the processor (Intel HD / UHD…).
    /// DirectML runs the matting model there 10–100× too slowly, so it goes on the CPU.
    pub fn start(latest: Latest, quality: u8, integrated_gpu: bool) -> Self {
        let quality = Arc::new(AtomicU8::new(quality));
        let fast = Arc::new(AtomicBool::new(false));
        let want = Arc::new(AtomicBool::new(true));
        let run = Arc::new(AtomicBool::new(true));
        let matte: Mattes = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let status = Arc::new(Mutex::new("starting…".to_string()));

        let handle = std::thread::Builder::new()
            .name("markcam-segment".into())
            .spawn({
                let (want, run, matte, status, q, fast) = (want.clone(), run.clone(), matte.clone(), status.clone(), quality.clone(), fast.clone());
                move || {
                    if let Err(e) = worker(&latest, &want, &run, &matte, &status, &q, &fast, integrated_gpu) {
                        *status.lock().unwrap() = format!("not available: {e}");
                    }
                }
            })
            .ok();
        SegWorker { want, run, matte, status, quality, fast, handle }
    }
}

impl Drop for SegWorker {
    fn drop(&mut self) {
        self.run.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn worker(
    latest: &Latest,
    want: &AtomicBool,
    run: &AtomicBool,
    out: &Mutex<std::collections::VecDeque<Arc<Matte>>>,
    status: &Mutex<String>,
    quality: &AtomicU8,
    fast: &AtomicBool,
    integrated_gpu: bool,
) -> anyhow::Result<()> {
    let dir = runtime_dir().ok_or_else(|| anyhow::anyhow!("ONNX Runtime is missing (reinstall MarkCam)"))?;
    let model = model_path().ok_or_else(|| anyhow::anyhow!("the background model is missing (reinstall MarkCam)"))?;
    ovc_ml::seg::init_runtime(&dir)?;
    let mut want_quality = quality.load(Ordering::SeqCst);
    let mut cpu_only = integrated_gpu || std::env::var_os("OVC_VBG_CPU").is_some();
    let mut engine = Engine::load_for(&model, want_quality, cpu_only)?;
    let mut gpu = engine.gpu();
    // Auto: the best model only while it keeps up with the camera (~30 fps).
    let mut best_checked = 0u32;
    let mut best_sum = 0f32;
    // RVM wants every frame (it is recurrent and fast on a GPU); MODNet / the CPU are rate
    // limited.
    let gap_for = |e: &Engine, gpu: bool| {
        Duration::from_millis(match (e, gpu) {
            (Engine::Rvm(..), true) => 0,
            // The fast model: up to 20 mattes a second.
            (Engine::Selfie(_), _) => 50,
            // CPU: ~10 mattes a second (the frames in between use the nearest one).
            _ => 100,
        })
    };
    let mut min_gap = gap_for(&engine, gpu);
    // Model time of the first frames on the graphics chip (to give up on a slow one).
    let mut gpu_probe: Vec<f32> = Vec::new();
    let mut last_seq = u64::MAX;
    let mut last_size = (0, 0);
    let mut still_thumb: Option<Vec<u8>> = None;
    let mut skipped = 0u32;
    let mut prev: Option<Vec<f32>> = None;
    let mut last_run = Instant::now() - min_gap;
    let mut ms_avg = 0f32;
    let mut model_avg = 0f32;
    while run.load(Ordering::SeqCst) {
        let q = quality.load(Ordering::SeqCst);
        if q != want_quality {
            want_quality = q;
            engine = Engine::load_for(&model, q, cpu_only)?;
            gpu = engine.gpu();
            min_gap = gap_for(&engine, gpu);
            gpu_probe.clear();
            best_checked = 0;
            best_sum = 0.0;
            model_avg = 0.0;
            ms_avg = 0.0;
        }
        // Judged on frames 10..40 (the first ones include the graphics chip's warm-up).
        let best_ms = best_sum / 30.0;
        if want_quality == QUALITY_AUTO && engine.is_best() && best_checked == 40 && best_ms > 28.0 {
            tracing::warn!("best background model too slow here ({best_ms:.0} ms): using the light one");
            engine = Engine::load(&model, false, cpu_only)?;
            model_avg = 0.0;
            ms_avg = 0.0;
            gpu_probe.clear();
        }
        // A graphics chip that is much too slow for the model (after its warm-up): use the
        // CPU instead, for good.
        if gpu && gpu_probe.len() >= 6 {
            let mut v = gpu_probe[2..].to_vec();
            v.sort_by(f32::total_cmp);
            let median = v[v.len() / 2];
            if median > 45.0 {
                tracing::warn!("graphics chip too slow for the background ({median:.0} ms): using the CPU");
                cpu_only = true;
                engine = Engine::load_for(&model, want_quality, true)?;
                gpu = engine.gpu();
                min_gap = gap_for(&engine, gpu);
                model_avg = 0.0;
                ms_avg = 0.0;
            }
            gpu_probe.clear();
            if !cpu_only {
                gpu_probe.push(f32::NAN); // probe done: never again
            }
        }
        if !want.load(Ordering::SeqCst) {
            prev = None;
            engine.reset();
            std::thread::sleep(Duration::from_millis(60));
            continue;
        }
        let frame = latest.lock().unwrap().clone();
        let Some((seq, fw, fh, rgba)) = frame.filter(|f| f.0 != last_seq) else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        if last_run.elapsed() < min_gap {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        // Sitting still: a new matte every 3rd frame is enough (the frames in between use
        // the nearest one). Any movement → every frame again.
        let thumb = tiny_luma(&rgba, fw as usize, fh as usize);
        let moving = still_thumb.as_ref().map_or(true, |p: &Vec<u8>| {
            p.len() != thumb.len() || p.iter().zip(&thumb).map(|(a, b)| a.abs_diff(*b) as u32).sum::<u32>() > thumb.len() as u32 * 2
        });
        if !moving && skipped < 2 {
            skipped += 1;
            last_seq = seq;
            continue;
        }
        skipped = 0;
        still_thumb = Some(thumb);
        if (fw, fh) != last_size {
            // A new source / resolution: start the video memory over.
            engine.reset();
            last_size = (fw, fh);
            prev = None;
        }
        last_seq = seq;
        last_run = Instant::now();
        let (rw, rh) = refine_size(fw, fh, engine.width());
        let mut prof = [0f32; 5];
        let tp = Instant::now();
        let rgb = small_rgb(&rgba, fw, fh, rw, rh);
        prof[0] = tp.elapsed().as_secs_f32() * 1000.0;
        let tm = Instant::now();
        let mut fgr = None;
        let raw = engine.matte(&rgba, fw, fh, &rgb, rw, rh, &mut fgr)?;
        let t_model = tm.elapsed();
        let mut q = match &engine {
            // RVM refines its own edges at this size (a learned guided filter).
            // OVC_VBG_GUIDED=1: also snap RVM's edges to the picture (experiment).
            Engine::Rvm(..) if std::env::var_os("OVC_VBG_GUIDED").is_some() => fast_guided_filter_rgb(&rgb, &raw, rw, rh, 4, 0.0015),
            Engine::Rvm(..) => raw,
            Engine::Modnet(s) | Engine::Selfie(s) => refine(&raw, s.w, s.h, &rgb, rw, rh),
        };
        let tp = Instant::now();
        keep_main_person(&mut q, rw, rh);
        prof[1] = tp.elapsed().as_secs_f32() * 1000.0;
        // MODNet works frame by frame: smooth it over time so edges don't shimmer. (RVM
        // has its own memory; smoothing would only make it lag.)
        if let (Engine::Modnet(_) | Engine::Selfie(_), Some(p)) = (&engine, prev.as_ref().filter(|p| p.len() == q.len())) {
            for (a, b) in q.iter_mut().zip(p) {
                let k = if (*a - *b).abs() > 0.3 { 0.85 } else { 0.6 };
                *a = *a * k + *b * (1.0 - k);
            }
        }
        let tp = Instant::now();
        let data = q.iter().map(|v| (shape(*v) * 255.0 + 0.5) as u8).collect();
        prof[3] = tp.elapsed().as_secs_f32() * 1000.0;
        if std::env::var_os("OVC_SEG_PROFILE").is_some() {
            eprintln!("model {:.1} rgb {:.1} keep {:.1} shape {:.1}", t_model.as_secs_f32() * 1000.0, prof[0], prof[1], prof[3]);
        }
        prev = Some(q);
        {
            let mut q = out.lock().unwrap();
            q.push_back(Arc::new(Matte { w: rw as u32, h: rh as u32, data, seq, fgr }));
            while q.len() > 8 {
                q.pop_front();
            }
        }
        let ms = last_run.elapsed().as_secs_f32() * 1000.0;
        let model_ms = t_model.as_secs_f32() * 1000.0;
        if gpu && gpu_probe.len() < 6 && !gpu_probe.iter().any(|v| v.is_nan()) {
            gpu_probe.push(model_ms);
        }
        // Each frame may wait for its own matte only when they come at camera speed.
        fast.store(gpu && model_avg > 0.0 && model_avg < 25.0, Ordering::SeqCst);
        best_checked += 1;
        if (11..=40).contains(&best_checked) {
            best_sum += model_ms;
        }
        ms_avg = if ms_avg == 0.0 { ms } else { ms_avg * 0.9 + ms * 0.1 };
        model_avg = if model_avg == 0.0 { model_ms } else { model_avg * 0.9 + model_ms * 0.1 };
        *status.lock().unwrap() = format!(
            "{} {} · {:.0} ms per frame (model {:.0}, edges {:.0})",
            engine.name(),
            if gpu { "on the graphics chip" } else { "on the CPU" },
            ms_avg,
            model_avg,
            ms_avg - model_avg
        );
    }
    Ok(())
}

/// The matting model in use: RVM when installed (video matting, best), else MODNet.
enum Engine {
    /// RVM, and whether it is the best (ResNet50) one.
    Rvm(ovc_ml::seg::VideoMatter, bool),
    Modnet(ovc_ml::seg::PersonSegmenter),
    /// MediaPipe Selfie Segmentation: tiny, every frame, edges snapped to the picture.
    Selfie(ovc_ml::seg::PersonSegmenter),
}

impl Engine {
    /// The engine for an edge quality: Light = the fast model; Best = RVM (the strong one on
    /// a graphics chip, the light RVM on the CPU); Auto = Best on a real graphics chip,
    /// the fast model on laptops (integrated graphics / no usable GPU).
    fn load_for(modnet: &Path, quality: u8, cpu: bool) -> anyhow::Result<Engine> {
        let selfie = modnet.parent().map(|d| d.join(SELFIE_MODEL)).filter(|p| p.is_file());
        if quality == QUALITY_LIGHT || (quality == QUALITY_AUTO && cpu) {
            if let Some(p) = &selfie {
                match ovc_ml::seg::PersonSegmenter::load_selfie(p, 1) {
                    Ok(s) => return Ok(Engine::Selfie(s)),
                    Err(e) => tracing::warn!("selfie model: {e}"),
                }
            }
        }
        Engine::load(modnet, quality != QUALITY_LIGHT, cpu)
    }

    /// `cpu`: don't try the graphics chip (integrated, or measured too slow).
    fn load(modnet: &Path, best: bool, cpu: bool) -> anyhow::Result<Engine> {
        let dir = modnet.parent().map(Path::to_path_buf).unwrap_or_default();
        let best_file = Some(dir.join(RVM_BEST_MODEL)).filter(|p| best && p.is_file());
        let rvm_file = std::env::var_os("OVC_VBG_RVM").map(PathBuf::from).or(best_file.clone()).or_else(|| Some(dir.join(RVM_MODEL)));
        if let Some(rvm) = rvm_file.filter(|p| p.is_file()) {
            let is_best = rvm.file_name().is_some_and(|n| n == RVM_BEST_MODEL);
            // Input at the refine size; the model looks at half of it and refines back up.
            if cpu {
                // The light model on the CPU, small (448 wide): ~15 ms on a desktop, ~30 ms
                // on a 7th-gen laptop.
                let light = dir.join(RVM_MODEL);
                if light.is_file() {
                    return Ok(Engine::Rvm(ovc_ml::seg::VideoMatter::load(&light, CPU_WIDTH, CPU_WIDTH * 9 / 16, 0.35, false, 1)?, false));
                }
            }
            match ovc_ml::seg::VideoMatter::load(&rvm, 640, 360, tune().ratio, !cpu, 2) {
                Ok(m) if m.backend == ovc_ml::seg::Backend::Gpu => return Ok(Engine::Rvm(m, is_best)),
                // No usable graphics chip: the light model on the CPU.
                Ok(_) if is_best => return Engine::load(modnet, false, true),
                Ok(_) => return Ok(Engine::Rvm(ovc_ml::seg::VideoMatter::load(&rvm, CPU_WIDTH, CPU_WIDTH * 9 / 16, 0.35, false, 1)?, false)),
                Err(e) => tracing::warn!("RVM: {e}; falling back to MODNet"),
            }
        }
        let seg = ovc_ml::seg::PersonSegmenter::load(modnet, 512, 288, !cpu, 2)?;
        if seg.backend == ovc_ml::seg::Backend::Gpu {
            return Ok(Engine::Modnet(seg));
        }
        Ok(Engine::Modnet(ovc_ml::seg::PersonSegmenter::load(modnet, 320, 192, false, 2)?))
    }

    /// Matte width: the GPU works at the full refine width, the CPU smaller.
    fn width(&self) -> usize {
        if matches!(self, Engine::Selfie(_)) {
            // Edges are snapped to the picture at this size: sharper than the model.
            return SELFIE_WIDTH.min(tune().width);
        }
        if self.gpu() {
            tune().width
        } else {
            CPU_WIDTH.min(tune().width)
        }
    }

    fn is_best(&self) -> bool {
        matches!(self, Engine::Rvm(_, true))
    }

    fn gpu(&self) -> bool {
        match self {
            Engine::Rvm(m, _) => m.backend == ovc_ml::seg::Backend::Gpu,
            Engine::Modnet(m) => m.backend == ovc_ml::seg::Backend::Gpu,
            Engine::Selfie(_) => false,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Engine::Rvm(_, true) => "RVM (best)",
            Engine::Rvm(_, false) => "RVM (light)",
            Engine::Modnet(_) => "MODNet",
            Engine::Selfie(_) => "Fast model (MediaPipe)",
        }
    }

    fn reset(&mut self) {
        if let Engine::Rvm(m, _) = self {
            m.reset();
        }
    }

    /// The matte at `rw`x`rh` for RVM (its own size), or at the model size for MODNet.
    fn matte(
        &mut self,
        rgba: &[u8],
        fw: u32,
        fh: u32,
        rgb: &[Vec<f32>; 3],
        rw: usize,
        rh: usize,
        self_fgr: &mut Option<Vec<u8>>,
    ) -> anyhow::Result<Vec<f32>> {
        *self_fgr = None;
        match self {
            Engine::Rvm(m, _) => {
                if (m.w, m.h) != (rw, rh) {
                    m.w = rw;
                    m.h = rh;
                    m.reset();
                }
                let (a, fgr) = m.matte_and_color(rgb)?;
                let plane = rw * rh;
                let mut rgb8 = vec![0u8; plane * 3];
                for k in 0..plane {
                    for c in 0..3 {
                        rgb8[k * 3 + c] = (fgr[c * plane + k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                }
                *self_fgr = Some(rgb8);
                Ok(a)
            }
            Engine::Modnet(s) | Engine::Selfie(s) => s.matte(rgba, fw, fh),
        }
    }
}
/// A 64x36 brightness thumbnail, to tell whether anything moved.
fn tiny_luma(rgba: &[u8], fw: usize, fh: usize) -> Vec<u8> {
    let (w, h) = (64usize, 36usize);
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let sy = (y * fh + fh / (2 * h)) / h;
        for x in 0..w {
            let sx = (x * fw + fw / (2 * w)) / w;
            let i = (sy.min(fh - 1) * fw + sx.min(fw - 1)) * 4;
            out.push(((rgba[i] as u32 * 77 + rgba[i + 1] as u32 * 150 + rgba[i + 2] as u32 * 29) >> 8) as u8);
        }
    }
    out
}

/// Keep the person (the biggest foreground region) and whatever touches it; drop separate
/// blobs elsewhere — a cartoon on a poster, the back of an empty chair.
fn keep_main_person(alpha: &mut [f32], w: usize, h: usize) {
    // Regions are found on a half-size grid (4x less work; blobs are big anyway).
    let (lw, lh) = (w.div_ceil(2), h.div_ceil(2));
    let n = lw * lh;
    let fg: Vec<bool> = (0..n).map(|k| alpha[(2 * (k / lw)).min(h - 1) * w + (2 * (k % lw)).min(w - 1)] >= 0.5).collect();
    let mut label = vec![0u32; n];
    let mut sizes = vec![0usize]; // label 0 = background
    let mut stack = Vec::new();
    for start in 0..n {
        if !fg[start] || label[start] != 0 {
            continue;
        }
        let id = sizes.len() as u32;
        let mut size = 0;
        label[start] = id;
        stack.push(start);
        while let Some(k) = stack.pop() {
            size += 1;
            let (x, y) = (k % lw, k / lw);
            let mut visit = |j: usize| {
                if fg[j] && label[j] == 0 {
                    label[j] = id;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(k - 1);
            }
            if x + 1 < lw {
                visit(k + 1);
            }
            if y > 0 {
                visit(k - lw);
            }
            if y + 1 < lh {
                visit(k + lw);
            }
        }
        sizes.push(size);
    }
    let biggest = sizes.iter().copied().max().unwrap_or(0);
    if biggest == 0 {
        return;
    }
    // Other people in the picture (big regions) stay; small separate blobs go.
    let keep: Vec<bool> = sizes.iter().map(|&s| s * 4 >= biggest && s * 200 >= n).collect();
    if keep.iter().skip(1).all(|k| *k) {
        return; // nothing to drop
    }
    let kept: Vec<f32> = label.iter().map(|&l| if l != 0 && keep[l as usize] { 1.0 } else { 0.0 }).collect();
    // Soft edges around kept regions survive (within a few pixels).
    let near = box_filter(&kept, lw, lh, 2);
    for y in 0..h {
        for x in 0..w {
            let k = (y / 2) * lw + x / 2;
            let l = label[k];
            if near[k] < 1e-4 || (l != 0 && !keep[l as usize]) {
                alpha[y * w + x] = 0.0;
            }
        }
    }
}
/// Tuning knobs (defaults; `OVC_VBG_WIDTH`, `OVC_VBG_RATIO`, `OVC_VBG_SHAPE=lo,hi` override
/// them for experiments on recorded clips).
struct Tune {
    width: usize,
    ratio: f32,
    shape: (f32, f32),
}

fn tune() -> &'static Tune {
    static T: std::sync::OnceLock<Tune> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let var = |k: &str| std::env::var(k).ok();
        Tune {
            width: var("OVC_VBG_WIDTH").and_then(|v| v.parse().ok()).unwrap_or(640),
            ratio: var("OVC_VBG_RATIO").and_then(|v| v.parse().ok()).unwrap_or(0.5),
            shape: var("OVC_VBG_SHAPE")
                .and_then(|v| {
                    let (a, b) = v.split_once(',')?;
                    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
                })
                .unwrap_or((0.12, 0.88)),
        }
    })
}

/// Firm up the matte: solid inside, solid outside, a soft but narrow edge.
fn shape(v: f32) -> f32 {
    let (lo, hi) = tune().shape;
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Matte width on the CPU.
const CPU_WIDTH: usize = 384;
/// Edge refinement width for the fast model.
const SELFIE_WIDTH: usize = 384;

/// Matte resolution: `width` wide (640 on a graphics chip), frame aspect (even).
fn refine_size(fw: u32, fh: u32, width: usize) -> (usize, usize) {
    let rw = width.min(fw as usize) & !1;
    let rh = ((rw as f32 * fh as f32 / fw as f32).round() as usize).max(2) & !1;
    (rw, rh)
}

/// The frame at `rw`x`rh` as planar RGB 0..1.
fn small_rgb(rgba: &[u8], fw: u32, fh: u32, rw: usize, rh: usize) -> [Vec<f32>; 3] {
    let (fw, fh) = (fw as usize, fh as usize);
    let n = rw * rh;
    let mut out = [vec![0f32; n], vec![0f32; n], vec![0f32; n]];
    for y in 0..rh {
        let (sy0, sy1) = (y * fh / rh, ((y + 1) * fh / rh).max(y * fh / rh + 1).min(fh));
        for x in 0..rw {
            let (sx0, sx1) = (x * fw / rw, ((x + 1) * fw / rw).max(x * fw / rw + 1).min(fw));
            // Average of the box's corners (cheap, enough for a 2x downscale).
            let mut acc = [0u32; 3];
            for (yy, xx) in [(sy0, sx0), (sy0, sx1 - 1), (sy1 - 1, sx0), (sy1 - 1, sx1 - 1)] {
                let i = (yy * fw + xx) * 4;
                for c in 0..3 {
                    acc[c] += rgba[i + c] as u32;
                }
            }
            for c in 0..3 {
                out[c][y * rw + x] = acc[c] as f32 / (4.0 * 255.0);
            }
        }
    }
    out
}

/// Upsample the model's matte to `rw`x`rh` and run a color guided filter with the frame as
/// guide (He et al.): the output follows the picture's real edges — by color, so a white
/// wall next to light skin still separates.
fn refine(raw: &[f32], mw: usize, mh: usize, rgb: &[Vec<f32>; 3], rw: usize, rh: usize) -> Vec<f32> {
    let n = rw * rh;
    // Bilinear upsample of the matte.
    let mut p = vec![0f32; n];
    for y in 0..rh {
        let my = ((y as f32 + 0.5) * mh as f32 / rh as f32 - 0.5).clamp(0.0, (mh - 1) as f32);
        let (y0, fy) = (my as usize, my.fract());
        let y1 = (y0 + 1).min(mh - 1);
        for x in 0..rw {
            let mx = ((x as f32 + 0.5) * mw as f32 / rw as f32 - 0.5).clamp(0.0, (mw - 1) as f32);
            let (x0, fx) = (mx as usize, mx.fract());
            let x1 = (x0 + 1).min(mw - 1);
            let top = raw[y0 * mw + x0] * (1.0 - fx) + raw[y0 * mw + x1] * fx;
            let bot = raw[y1 * mw + x0] * (1.0 - fx) + raw[y1 * mw + x1] * fx;
            p[y * rw + x] = top * (1.0 - fy) + bot * fy;
        }
    }
    fast_guided_filter_rgb(rgb, &p, rw, rh, 4, 0.0015)
}

/// The guided filter's linear model computed at half resolution (4x less work) and applied
/// at full resolution — "fast guided filter" (He & Sun 2015); edges stay full-res sharp.
fn fast_guided_filter_rgb(i: &[Vec<f32>; 3], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let (lw, lh) = ((w / 2).max(1), (h / 2).max(1));
    let half = |v: &[f32]| -> Vec<f32> {
        let mut o = vec![0f32; lw * lh];
        for y in 0..lh {
            let (y0, y1) = ((2 * y).min(h - 1), (2 * y + 1).min(h - 1));
            for x in 0..lw {
                let (x0, x1) = ((2 * x).min(w - 1), (2 * x + 1).min(w - 1));
                o[y * lw + x] = (v[y0 * w + x0] + v[y0 * w + x1] + v[y1 * w + x0] + v[y1 * w + x1]) * 0.25;
            }
        }
        o
    };
    let il = [half(&i[0]), half(&i[1]), half(&i[2])];
    let (a, b) = guided_coefficients(&il, &half(p), lw, lh, (r / 2).max(1), eps);
    // Bilinear upsample of a, b and q = a·I + b at full resolution.
    let mut q = vec![0f32; w * h];
    for y in 0..h {
        let ly = ((y as f32 + 0.5) * 0.5 - 0.5).clamp(0.0, (lh - 1) as f32);
        let (y0, fy) = (ly as usize, ly.fract());
        let y1 = (y0 + 1).min(lh - 1);
        for x in 0..w {
            let lx = ((x as f32 + 0.5) * 0.5 - 0.5).clamp(0.0, (lw - 1) as f32);
            let (x0, fx) = (lx as usize, lx.fract());
            let x1 = (x0 + 1).min(lw - 1);
            let up = |m: &[f32]| {
                let top = m[y0 * lw + x0] * (1.0 - fx) + m[y0 * lw + x1] * fx;
                let bot = m[y1 * lw + x0] * (1.0 - fx) + m[y1 * lw + x1] * fx;
                top * (1.0 - fy) + bot * fy
            };
            let k = y * w + x;
            q[k] = (up(&a[0]) * i[0][k] + up(&a[1]) * i[1][k] + up(&a[2]) * i[2][k] + up(&b)).clamp(0.0, 1.0);
        }
    }
    q
}

/// Color-guided filter (3-channel guide), box radius `r`, regularization `eps`.
#[cfg(test)]
fn guided_filter_rgb(i: &[Vec<f32>; 3], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let (a, b) = guided_coefficients(i, p, w, h, r, eps);
    (0..w * h).map(|k| (a[0][k] * i[0][k] + a[1][k] * i[1][k] + a[2][k] * i[2][k] + b[k]).clamp(0.0, 1.0)).collect()
}

/// The guided filter's box-averaged linear coefficients `(a per channel, b)`: the output is
/// `a·I + b`.
fn guided_coefficients(i: &[Vec<f32>; 3], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> ([Vec<f32>; 3], Vec<f32>) {
    let n = w * h;
    let bf = |v: &[f32]| box_filter(v, w, h, r);
    let mul = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).collect::<Vec<f32>>();
    let m_i = [bf(&i[0]), bf(&i[1]), bf(&i[2])];
    let m_p = bf(p);
    let m_ip = [bf(&mul(&i[0], p)), bf(&mul(&i[1], p)), bf(&mul(&i[2], p))];
    let pairs = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];
    let m_ii: Vec<Vec<f32>> = pairs.iter().map(|&(a, b)| bf(&mul(&i[a], &i[b]))).collect();
    let mut a = [vec![0f32; n], vec![0f32; n], vec![0f32; n]];
    let mut b = vec![0f32; n];
    for k in 0..n {
        let mi = [m_i[0][k], m_i[1][k], m_i[2][k]];
        let cov = [m_ip[0][k] - mi[0] * m_p[k], m_ip[1][k] - mi[1] * m_p[k], m_ip[2][k] - mi[2] * m_p[k]];
        let s = |q: usize, a_: usize, b_: usize| m_ii[q][k] - mi[a_] * mi[b_];
        let (rr, rg, rb, gg, gb, bb) = (s(0, 0, 0) + eps, s(1, 0, 1), s(2, 0, 2), s(3, 1, 1) + eps, s(4, 1, 2), s(5, 2, 2) + eps);
        // Inverse of the symmetric 3x3 covariance (cofactors).
        let (c00, c01, c02) = (gg * bb - gb * gb, gb * rb - rg * bb, rg * gb - gg * rb);
        let (c11, c12, c22) = (rr * bb - rb * rb, rb * rg - rr * gb, rr * gg - rg * rg);
        let det = rr * c00 + rg * c01 + rb * c02;
        if det.abs() < 1e-12 {
            b[k] = m_p[k];
            continue;
        }
        let inv = 1.0 / det;
        let a0 = (c00 * cov[0] + c01 * cov[1] + c02 * cov[2]) * inv;
        let a1 = (c01 * cov[0] + c11 * cov[1] + c12 * cov[2]) * inv;
        let a2 = (c02 * cov[0] + c12 * cov[1] + c22 * cov[2]) * inv;
        a[0][k] = a0;
        a[1][k] = a1;
        a[2][k] = a2;
        b[k] = m_p[k] - a0 * mi[0] - a1 * mi[1] - a2 * mi[2];
    }
    ([bf(&a[0]), bf(&a[1]), bf(&a[2])], bf(&b))
}

/// Mean over a (2r+1)² window (shrunk at the edges): sliding sums, rows then columns.
fn box_filter(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    // Horizontal window sums.
    let mut hs = vec![0f32; w * h];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let out = &mut hs[y * w..(y + 1) * w];
        let mut s: f32 = row[..(r + 1).min(w)].iter().sum();
        for x in 0..w {
            out[x] = s;
            if x + r + 1 < w {
                s += row[x + r + 1];
            }
            if x >= r {
                s -= row[x - r];
            }
        }
    }
    // Vertical window sums of those, divided by the window area.
    let xcount: Vec<f32> = (0..w).map(|x| ((x + r + 1).min(w) - x.saturating_sub(r)) as f32).collect();
    let mut col = vec![0f32; w];
    for y in 0..(r + 1).min(h) {
        for x in 0..w {
            col[x] += hs[y * w + x];
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let ycount = ((y + r + 1).min(h) - y.saturating_sub(r)) as f32;
        for x in 0..w {
            out[y * w + x] = col[x] / (xcount[x] * ycount);
        }
        if y + r + 1 < h {
            let add = &hs[(y + r + 1) * w..(y + r + 2) * w];
            for x in 0..w {
                col[x] += add[x];
            }
        }
        if y >= r {
            let sub = &hs[(y - r) * w..(y - r + 1) * w];
            for x in 0..w {
                col[x] -= sub[x];
            }
        }
    }
    out
}

/// The per-frame side: holds the chosen background ready at frame size and blends.
pub struct Compositor {
    pub choice: Choice,
    /// Blur strength 0..1 (for [`Choice::Blur`]).
    pub blur: f32,
    /// Edge quality (`QUALITY_*`).
    pub quality: u8,
    /// Hold each camera frame until its own matte is ready (only when mattes come at
    /// camera speed; set from [`SegWorker::fast`]).
    pub wait_for_matte: bool,
    /// Color key instead of AI: everything the color of `key` (a green screen, a plain
    /// wall) is background. `key_sim` = how close counts as that color, `key_smooth` = the
    /// soft edge width (both 0..1). `key_ai`: AI keeps the person solid even where clothes
    /// match the key, and clears everything outside the person (beyond the screen's edge).
    pub key_mode: bool,
    /// Picked background colors (each eyedropper click adds one): a pixel close to *any*
    /// of them is background.
    pub keys: Vec<[u8; 3]>,
    pub key_sim: f32,
    pub key_smooth: f32,
    pub key_ai: bool,
    /// AI masks for the color key, at matte size: (matte, outside-person, solid-person).
    ai_masks: Option<(Arc<Matte>, Vec<bool>, Vec<bool>)>,
    /// Alpha for every color (6 bits per channel) under the current key settings.
    key_lut: Option<((u32, u32), Vec<[u8; 3]>, Vec<u8>)>,
    pub matte_src: Option<Mattes>,
    /// The picture background at frame size, for (choice, w, h).
    still: Option<(Choice, u32, u32, Vec<u8>)>,
    video: Option<(PathBuf, crate::video::VideoStream)>,
    pub err: Option<String>,
    small: Vec<u8>,
    tmp: Vec<u8>,
    /// The matte upsampled to frame size, and which matte / size it is for.
    alpha: Vec<u8>,
    alpha_of: Option<(Arc<Matte>, u32, u32)>,
    /// Frames since the blurred background was last made (every 3rd frame), and it at
    /// frame size.
    blur_age: u32,
    blur_full: Vec<u8>,
}

impl Compositor {
    pub fn new() -> Self {
        Compositor {
            choice: Choice::Off,
            blur: 0.6,
            quality: QUALITY_AUTO,
            wait_for_matte: false,
            key_mode: false,
            keys: vec![[0, 177, 64]],
            key_sim: 0.2,
            key_smooth: 0.08,
            key_ai: true,
            ai_masks: None,
            key_lut: None,
            matte_src: None,
            still: None,
            video: None,
            err: None,
            small: Vec::new(),
            tmp: Vec::new(),
            alpha: Vec::new(),
            alpha_of: None,
            blur_age: 0,
            blur_full: Vec::new(),
        }
    }

    pub fn active(&self) -> bool {
        self.choice != Choice::Off
    }

    /// Whether the AI segmentation is needed (always, except a color key without AI help).
    pub fn needs_matte(&self) -> bool {
        self.active() && (!self.key_mode || self.key_ai)
    }

    /// Let go of what only the current choice needs: a background video keeps decoding on
    /// its own thread until it is dropped (turning the background off or choosing a picture
    /// must stop it).
    pub fn release_unused(&mut self) {
        let video_choice = matches!(&self.choice, Choice::File(p) if crate::video::is_video_file(p));
        if !video_choice && self.video.is_some() {
            self.video = None;
        }
        if self.choice == Choice::Off {
            self.still = None;
        }
    }

    /// The camera frame the newest matte was made from, if there is one.
    pub fn matte_seq(&self) -> Option<u64> {
        self.matte_src.as_ref().and_then(|m| m.lock().unwrap().back().map(|m| m.seq))
    }

    /// Put `src` (the camera frame) in front of the chosen background into `out`. Returns
    /// `false` (and leaves `out` alone) while there is no matte yet.
    pub fn composite(&mut self, src: &[u8], seq: u64, w: u32, h: u32, out: &mut Vec<u8>) -> bool {
        // The matte made from this very frame, else the nearest one in time.
        let matte = self.matte_src.as_ref().and_then(|m| m.lock().unwrap().iter().min_by_key(|m| m.seq.abs_diff(seq)).cloned());
        let matte = matte.filter(|m| m.w >= 2 && m.h >= 2);
        let key = self.key_mode;
        if !key && matte.is_none() {
            return false;
        }
        let (wu, hu) = (w as usize, h as usize);
        if src.len() < wu * hu * 4 {
            return false;
        }
        out.resize(wu * hu * 4, 0);

        // The background, as a sampler over a buffer: (data, bw, bh).
        let mut video_frame = None;
        match self.choice.clone() {
            Choice::Off => return false,
            Choice::Blur => {
                // A third of the frame rate is plenty for a blurred backdrop: made small,
                // blurred, then enlarged once into a frame-size picture.
                if self.blur_age % 3 == 0 || self.blur_full.len() != wu * hu * 4 {
                    self.make_blur(src, w, h);
                    let (sw, sh) = ((wu / 8).max(1), (hu / 8).max(1));
                    Self::enlarge(&self.small, sw, sh, wu, hu, &mut self.blur_full);
                }
                self.blur_age = self.blur_age.wrapping_add(1);
            }
            Choice::Builtin(i) => {
                if !matches!(&self.still, Some((c, sw, sh, _)) if *c == self.choice && *sw == w && *sh == h) {
                    self.still = Some((self.choice.clone(), w, h, builtin(i, w, h)));
                }
            }
            Choice::File(p) if crate::video::is_video_file(&p) => {
                if self.video.as_ref().map(|(vp, _)| vp != &p).unwrap_or(true) {
                    self.video = None;
                    match crate::video::VideoStream::open_max(&p, 1280) {
                        Ok(v) => self.video = Some((p.clone(), v)),
                        Err(e) => self.err = Some(format!("background video: {e}")),
                    }
                }
                video_frame = self.video.as_ref().and_then(|(_, v)| v.latest());
                if video_frame.is_none() {
                    return false;
                }
            }
            Choice::File(p) => {
                if !matches!(&self.still, Some((c, sw, sh, _)) if *c == self.choice && *sw == w && *sh == h) {
                    match load_cover(&p, w, h) {
                        Ok(img) => self.still = Some((self.choice.clone(), w, h, img)),
                        Err(e) => {
                            self.err = Some(format!("background picture: {e}"));
                            self.choice = Choice::Off;
                            return false;
                        }
                    }
                }
            }
        }
        if key {
            // Color key: alpha from this frame's colors (no waiting for a model).
            self.key_alpha(src, wu, hu, if self.key_ai { matte.as_ref() } else { None });
            self.alpha_of = None;
        } else if let Some(m) = &matte {
            // The matte at frame size: made once per new matte, reused by every frame.
            if !matches!(&self.alpha_of, Some((am, aw, ah)) if Arc::ptr_eq(am, m) && (*aw, *ah) == (w, h)) {
                Self::upsample_matte(m, wu, hu, &mut self.alpha);
                self.alpha_of = Some((m.clone(), w, h));
            }
        }
        let (bg, bw, bh): (&[u8], usize, usize) = match (&self.choice, &video_frame) {
            (Choice::Blur, _) => (&self.blur_full, wu, hu),
            (_, Some((_, data, vw, vh))) => (data.as_slice(), *vw as usize, *vh as usize),
            _ => match &self.still {
                Some((_, _, _, img)) => (img.as_slice(), wu, hu),
                None => return false,
            },
        };

        // Background sampling map (cover fit; identity for stills made at frame size),
        // per column once instead of per pixel.
        let (scale, ox, oy) = {
            let s = (bw as f32 / wu as f32).min(bh as f32 / hu as f32);
            (s, (bw as f32 - wu as f32 * s) * 0.5, (bh as f32 - hu as f32 * s) * 0.5)
        };
        let identity = (bw, bh) == (wu, hu);
        let bxf: Vec<f32> = (0..wu).map(|x| (ox + (x as f32 + 0.5) * scale).clamp(0.0, (bw - 1) as f32)).collect();
        let bxi: Vec<usize> = bxf.iter().map(|v| *v as usize * 4).collect();


        let spill = if key { key_chroma_dir(&self.keys) } else { None };
        // With a colored screen, the screen's tint also sits on solid pixels right at the
        // outline: they get de-spilled too (a 3 px band, found on a quarter-size grid).
        let spill_band: Option<(usize, Vec<bool>)> = spill.map(|_| {
            let (qw, qh) = (wu.div_ceil(4), hu.div_ceil(4));
            let mut open = vec![0f32; qw * qh];
            for y in 0..hu {
                for x in 0..wu {
                    if self.alpha[y * wu + x] < 250 {
                        open[(y / 4) * qw + x / 4] = 1.0;
                    }
                }
            }
            (qw, box_filter(&open, qw, qh, 1).iter().map(|v| *v > 0.0).collect())
        });

        let (mw, mh, fgr) = match (&matte, key) {
            (Some(m), false) => (m.w as usize, m.h as usize, m.fgr.as_deref()),
            _ => (1, 1, None),
        };
        let fg_cols: Vec<usize> = (0..wu).map(|x| (x * mw / wu).min(mw - 1) * 3).collect();
        let mut bgrow = vec![0u8; wu * 4];
        for y in 0..hu {
            let arow = &self.alpha[y * wu..(y + 1) * wu];
            let row = &src[y * wu * 4..(y + 1) * wu * 4];
            let orow = &mut out[y * wu * 4..(y + 1) * wu * 4];
            if arow.iter().all(|a| *a == 255) {
                orow.copy_from_slice(row);
                continue;
            }
            // This row of the background, RGBA.
            let by = (oy + (y as f32 + 0.5) * scale).clamp(0.0, (bh - 1) as f32);
            let brow: &[u8] = if identity {
                &bg[y * wu * 4..(y + 1) * wu * 4]
            } else {
                let src_row = &bg[by as usize * bw * 4..(by as usize + 1) * bw * 4];
                for x in 0..wu {
                    let i = bxi[x];
                    bgrow[x * 4..x * 4 + 3].copy_from_slice(&src_row[i..i + 3]);
                    bgrow[x * 4 + 3] = 255;
                }
                &bgrow
            };
            let fg_row: Option<&[u8]> = fgr.map(|f| {
                let my = (y * mh / hu).min(mh - 1);
                &f[my * mw * 3..(my + 1) * mw * 3]
            });
            // Runs of sure-person / sure-background are plain copies; only the outline blends.
            let band_row = spill_band.as_ref().map(|(qw, b)| &b[(y / 4) * qw..(y / 4 + 1) * qw]);
            let mut x = 0;
            while x < wu {
                let a = arow[x];
                if let (255, Some(br), Some(dir)) = (a, band_row, spill) {
                    if br[x / 4] {
                        let o = x * 4;
                        let c = despill([row[o], row[o + 1], row[o + 2]], dir);
                        orow[o..o + 3].copy_from_slice(&c);
                        orow[o + 3] = 255;
                        x += 1;
                        continue;
                    }
                }
                if a == 255 || a == 0 {
                    let mut e = x + 1;
                    while e < wu && arow[e] == a && !(a == 255 && spill.is_some() && band_row.is_some_and(|br| br[e / 4])) {
                        e += 1;
                    }
                    let from = if a == 255 { row } else { brow };
                    orow[x * 4..e * 4].copy_from_slice(&from[x * 4..e * 4]);
                    x = e;
                    continue;
                }
                let (a, na) = (a as u32, 255 - a as u32);
                let o = x * 4;
                // The person's own color at the outline (room tint removed), when known.
                let fg: [u8; 3] = match fg_row {
                    Some(r) => {
                        let i = fg_cols[x];
                        [r[i], r[i + 1], r[i + 2]]
                    }
                    // Color key: take the key color's tint off the edge (green fringe).
                    None => match spill {
                        Some(dir) => despill([row[o], row[o + 1], row[o + 2]], dir),
                        None => [row[o], row[o + 1], row[o + 2]],
                    },
                };
                for c in 0..3 {
                    // x/255 ≈ (x * 257 + 257) >> 16 for x < 65536.
                    let v = fg[c] as u32 * a + brow[o + c] as u32 * na;
                    orow[o + c] = ((v * 257 + 257) >> 16) as u8;
                }
                orow[o + 3] = 255;
                x += 1;
            }
        }
        true
    }

    /// Alpha (frame size, into `self.alpha`) from the key color: 0 = that color, 255 =
    /// clearly something else, a soft ramp between. Distance in YCbCr: chroma first, plus
    /// brightness (more of it for an unsaturated key like a white wall, where chroma alone
    /// can't tell wall from skin). With `ai`: outside the (widened) person → 0, inside the
    /// (narrowed) person → 255, the key only decides along the outline.
    fn key_alpha(&mut self, src: &[u8], w: usize, h: usize, ai: Option<&Arc<Matte>>) {
        // The answer for every color (6 bits per channel) is computed once per key setting;
        // each frame then only looks it up.
        // A pixel's alpha is the lowest over all picked colors. One more pick only needs
        // that color's table folded in; other changes rebuild it.
        let (sim, smooth) = (self.key_sim, self.key_smooth);
        let table = |k: &KeyRef| -> Vec<u8> {
            (0..1usize << 18)
                .map(|i| {
                    let q = |s: usize| (((i >> s) & 63) << 2 | 2) as u8;
                    key_alpha_of([q(12), q(6), q(0)], k, sim, smooth)
                })
                .collect()
        };
        let params = (sim.to_bits(), smooth.to_bits());
        let current = self.key_lut.as_ref().map(|(p, ks, _)| (*p, ks.clone()));
        match current {
            Some((p, ks)) if p == params && ks == self.keys => {}
            Some((p, ks)) if p == params && self.keys.len() == ks.len() + 1 && self.keys.starts_with(&ks) => {
                let add = table(&KeyRef::new(*self.keys.last().expect("one more")));
                let (_, ks, lut) = self.key_lut.as_mut().expect("present");
                for (a, b) in lut.iter_mut().zip(add) {
                    *a = (*a).min(b);
                }
                *ks = self.keys.clone();
            }
            _ => {
                let mut lut = vec![255u8; 1 << 18];
                for k in &self.keys {
                    for (a, b) in lut.iter_mut().zip(table(&KeyRef::new(*k))) {
                        *a = (*a).min(b);
                    }
                }
                self.key_lut = Some((params, self.keys.clone(), lut));
            }
        }
        if let Some(m) = ai {
            if !matches!(&self.ai_masks, Some((am, _, _)) if Arc::ptr_eq(am, m)) {
                let (mw, mh) = (m.w as usize, m.h as usize);
                let some: Vec<f32> = m.data.iter().map(|a| if *a > 12 { 1.0 } else { 0.0 }).collect();
                let solid: Vec<f32> = m.data.iter().map(|a| if *a > 250 { 1.0 } else { 0.0 }).collect();
                let outside = box_filter(&some, mw, mh, 12).iter().map(|v| *v < 1e-4).collect();
                let inside = box_filter(&solid, mw, mh, 3).iter().map(|v| *v > 0.999).collect();
                self.ai_masks = Some((m.clone(), outside, inside));
            }
        }
        let masks = ai.and(self.ai_masks.as_ref()).map(|(m, o, i)| (m.w as usize, m.h as usize, o, i));
        let lut = &self.key_lut.as_ref().expect("built above").2;
        self.alpha.resize(w * h, 0);
        for y in 0..h {
            let row = &src[y * w * 4..(y + 1) * w * 4];
            let arow = &mut self.alpha[y * w..(y + 1) * w];
            let mrow = masks.map(|(mw, mh, o, i)| {
                let my = (y * mh / h).min(mh - 1);
                (mw, &o[my * mw..(my + 1) * mw], &i[my * mw..(my + 1) * mw])
            });
            for (x, a) in arow.iter_mut().enumerate() {
                if let Some((mw, out, inn)) = mrow {
                    let mx = (x * mw / w).min(mw - 1);
                    if out[mx] {
                        *a = 0;
                        continue;
                    }
                    if inn[mx] {
                        *a = 255;
                        continue;
                    }
                }
                let o = x * 4;
                let i = (row[o] as usize >> 2) << 12 | (row[o + 1] as usize >> 2) << 6 | row[o + 2] as usize >> 2;
                *a = lut[i];
            }
        }
    }

    /// Bilinear enlargement of a small RGBA picture to `w`x`h` (into `out`).
fn enlarge(small: &[u8], sw: usize, sh: usize, w: usize, h: usize, out: &mut Vec<u8>) {
    out.resize(w * h * 4, 255);
    let cols: Vec<(usize, usize, u32)> = (0..w)
        .map(|x| {
            let v = ((x as f32 + 0.5) * sw as f32 / w as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let x0 = v as usize;
            (x0 * 4, (x0 + 1).min(sw - 1) * 4, (v.fract() * 256.0) as u32)
        })
        .collect();
    for y in 0..h {
        let v = ((y as f32 + 0.5) * sh as f32 / h as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, fy) = (v as usize, (v.fract() * 256.0) as u32);
        let y1 = (y0 + 1).min(sh - 1);
        let (ra, rb) = (&small[y0 * sw * 4..(y0 + 1) * sw * 4], &small[y1 * sw * 4..(y1 + 1) * sw * 4]);
        let orow = &mut out[y * w * 4..(y + 1) * w * 4];
        for (o, &(x0, x1, fx)) in orow.chunks_exact_mut(4).zip(&cols) {
            for c in 0..3 {
                let top = ra[x0 + c] as u32 * (256 - fx) + ra[x1 + c] as u32 * fx;
                let bot = rb[x0 + c] as u32 * (256 - fx) + rb[x1 + c] as u32 * fx;
                o[c] = ((top * (256 - fy) + bot * fy) >> 16) as u8;
            }
            o[3] = 255;
        }
    }
}

/// Bilinear upsample of a matte to `w`x`h` (into `out`).
fn upsample_matte(m: &Matte, w: usize, h: usize, out: &mut Vec<u8>) {
    let (mw, mh) = (m.w as usize, m.h as usize);
    out.resize(w * h, 0);
    let xmap: Vec<(usize, usize, u32)> = (0..w)
        .map(|x| {
            let mx = ((x as f32 + 0.5) * mw as f32 / w as f32 - 0.5).clamp(0.0, (mw - 1) as f32);
            let x0 = mx as usize;
            (x0, (x0 + 1).min(mw - 1), (mx.fract() * 256.0) as u32)
        })
        .collect();
    for y in 0..h {
        let my = ((y as f32 + 0.5) * mh as f32 / h as f32 - 0.5).clamp(0.0, (mh - 1) as f32);
        let (y0, fy) = (my as usize, (my.fract() * 256.0) as u32);
        let y1 = (y0 + 1).min(mh - 1);
        let (r0, r1) = (&m.data[y0 * mw..(y0 + 1) * mw], &m.data[y1 * mw..(y1 + 1) * mw]);
        for (a, &(x0, x1, fx)) in out[y * w..(y + 1) * w].iter_mut().zip(&xmap) {
            let top = r0[x0] as u32 * (256 - fx) + r0[x1] as u32 * fx;
            let bot = r1[x0] as u32 * (256 - fx) + r1[x1] as u32 * fx;
            *a = ((top * (256 - fy) + bot * fy) >> 16) as u8;
        }
    }
}

    /// The frame shrunk 8x and blurred (sampled bilinearly when compositing).
    fn make_blur(&mut self, src: &[u8], w: u32, h: u32) {
        let (wu, hu) = (w as usize, h as usize);
        let (sw, sh) = ((wu / 8).max(1), (hu / 8).max(1));
        self.small.resize(sw * sh * 4, 255);
        for y in 0..sh {
            for x in 0..sw {
                let mut acc = [0u32; 3];
                for dy in [1usize, 5] {
                    for dx in [1usize, 5] {
                        let i = (((y * 8 + dy).min(hu - 1)) * wu + (x * 8 + dx).min(wu - 1)) * 4;
                        for c in 0..3 {
                            acc[c] += src[i + c] as u32;
                        }
                    }
                }
                let o = (y * sw + x) * 4;
                for c in 0..3 {
                    self.small[o + c] = (acc[c] / 4) as u8;
                }
            }
        }
        let r = 1 + (self.blur.clamp(0.0, 1.0) * 5.0) as usize;
        for _ in 0..2 {
            box_blur_rgba(&mut self.small, &mut self.tmp, sw, sh, r);
        }
    }
}

/// BT.601 Y, Cb, Cr (Cb/Cr centered on 0, ±128).
fn ycc(c: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (c[0] as f32, c[1] as f32, c[2] as f32);
    (0.299 * r + 0.587 * g + 0.114 * b, -0.1687 * r - 0.3313 * g + 0.5 * b, 0.5 * r - 0.4187 * g - 0.0813 * b)
}

/// The color key's alpha for one color: 0 = the key color, 255 = clearly something else.
/// A colored screen is compared by hue (chromaticity), so its shadows and bright spots are
/// still "the same green"; a white/grey wall by chroma *and* brightness (chroma alone can't
/// tell a white wall from skin).
fn key_alpha_of(px: [u8; 3], k: &KeyRef, sim: f32, smooth: f32) -> u8 {
    let wl = if k.saturated { 0.08 } else { 0.6 };
    let (py, pcb, pcr) = ycc(px);
    let dc = if k.saturated {
        // Very dark pixels have no reliable hue: count them as not the screen.
        if py < 18.0 {
            1.0
        } else {
            let pc = chromaticity(px);
            ((pc.0 - k.chroma.0).powi(2) + (pc.1 - k.chroma.1).powi(2)).sqrt() * 2.5
        }
    } else {
        ((pcb - k.cb).powi(2) + (pcr - k.cr).powi(2)).sqrt() / 128.0
    };
    let dl = wl * (py - k.y) / 255.0;
    let d = (dc * dc + dl * dl).sqrt();
    let (lo, hi) = (sim.max(0.0), sim.max(0.0) + smooth.max(0.005));
    let t = ((d - lo) / (hi - lo)).clamp(0.0, 1.0);
    (t * t * (3.0 - 2.0 * t) * 255.0 + 0.5) as u8
}

/// A picked key color, prepared for comparisons.
struct KeyRef {
    y: f32,
    cb: f32,
    cr: f32,
    /// A real color (green / blue screen) rather than a white or grey wall.
    saturated: bool,
    chroma: (f32, f32),
}

impl KeyRef {
    fn new(c: [u8; 3]) -> Self {
        let (y, cb, cr) = ycc(c);
        KeyRef { y, cb, cr, saturated: (cb * cb + cr * cr).sqrt() / 128.0 > 0.15, chroma: chromaticity(c) }
    }
}

/// (r, g) / (r + g + b): color with brightness taken out.
fn chromaticity(c: [u8; 3]) -> (f32, f32) {
    let s = (c[0] as f32 + c[1] as f32 + c[2] as f32).max(1.0);
    (c[0] as f32 / s, c[1] as f32 / s)
}

/// The average chroma direction (unit Cb, Cr) of the colored keys — the cast to take off
/// edges. `None` when no picked color is a real color (a white/grey wall casts no tint).
fn key_chroma_dir(keys: &[[u8; 3]]) -> Option<(f32, f32)> {
    let (mut sx, mut sy) = (0f32, 0f32);
    for k in keys.iter().map(|k| KeyRef::new(*k)).filter(|k| k.saturated) {
        let n = (k.cb * k.cb + k.cr * k.cr).sqrt().max(1e-3);
        sx += k.cb / n;
        sy += k.cr / n;
    }
    let n = (sx * sx + sy * sy).sqrt();
    (n > 1e-3).then(|| (sx / n, sy / n))
}

/// Remove the part of a color that leans toward the key's chroma (the green/blue cast on
/// hair and shoulders), keeping its brightness.
fn despill(c: [u8; 3], dir: (f32, f32)) -> [u8; 3] {
    let (y, cb, cr) = ycc(c);
    let s = cb * dir.0 + cr * dir.1;
    if s <= 0.0 {
        return c;
    }
    let (cb, cr) = (cb - dir.0 * s, cr - dir.1 * s);
    let r = y + 1.402 * cr;
    let g = y - 0.344_136 * cb - 0.714_136 * cr;
    let b = y + 1.772 * cb;
    [r.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, b.clamp(0.0, 255.0) as u8]
}

/// The most common color along the picture's top and side edges (where the wall / screen
/// usually is): the "Auto" key.
pub fn auto_key(src: &[u8], w: usize, h: usize) -> [u8; 3] {
    let mut hist = std::collections::HashMap::<u32, (u32, [u64; 3])>::new();
    let mut add = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        let c = [src[i], src[i + 1], src[i + 2]];
        let bin = (c[0] as u32 >> 4) << 8 | (c[1] as u32 >> 4) << 4 | (c[2] as u32 >> 4);
        let e = hist.entry(bin).or_insert((0, [0; 3]));
        e.0 += 1;
        for k in 0..3 {
            e.1[k] += c[k] as u64;
        }
    };
    for y in (0..h / 6).step_by(2) {
        for x in (0..w).step_by(4) {
            add(x, y);
        }
    }
    for y in (h / 6..h).step_by(4) {
        for x in (0..w / 8).chain(w - w / 8..w).step_by(2) {
            add(x, y);
        }
    }
    let (_, (n, sum)) = hist.into_iter().max_by_key(|(_, (n, _))| *n).unwrap_or((0, (1, [0; 3])));
    [(sum[0] / n as u64) as u8, (sum[1] / n as u64) as u8, (sum[2] / n as u64) as u8]
}

/// Average color of a 9x9 patch around (`x`, `y`) — the eyedropper.
pub fn pick_color(src: &[u8], w: usize, h: usize, x: usize, y: usize) -> [u8; 3] {
    let mut sum = [0u32; 3];
    let mut n = 0;
    for yy in y.saturating_sub(4)..(y + 5).min(h) {
        for xx in x.saturating_sub(4)..(x + 5).min(w) {
            let i = (yy * w + xx) * 4;
            for k in 0..3 {
                sum[k] += src[i + k] as u32;
            }
            n += 1;
        }
    }
    let n = n.max(1);
    [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]
}

/// Separable box blur of radius `r` in place (`tmp` is scratch).
fn box_blur_rgba(img: &mut [u8], tmp: &mut Vec<u8>, w: usize, h: usize, r: usize) {
    tmp.resize(img.len(), 255);
    let pass = |src: &[u8], dst: &mut [u8], len: usize, count: usize, stride_i: usize, stride_j: usize| {
        for j in 0..count {
            for i in 0..len {
                let (a, b) = (i.saturating_sub(r), (i + r + 1).min(len));
                let mut acc = [0u32; 3];
                for k in a..b {
                    let o = (k * stride_i + j * stride_j) * 4;
                    for c in 0..3 {
                        acc[c] += src[o + c] as u32;
                    }
                }
                let o = (i * stride_i + j * stride_j) * 4;
                for c in 0..3 {
                    dst[o + c] = (acc[c] / (b - a) as u32) as u8;
                }
            }
        }
    };
    pass(img, tmp, w, h, 1, w);
    pass(tmp, img, h, w, w, 1);
}

/// Load a picture and scale it to cover `w`x`h` (cropping the overflow, centered).
fn load_cover(p: &Path, w: u32, h: u32) -> anyhow::Result<Vec<u8>> {
    let img = image::open(p)?.to_rgba8();
    let (iw, ih) = img.dimensions();
    let s = (w as f32 / iw as f32).max(h as f32 / ih as f32);
    let (sw, sh) = (((iw as f32 * s).ceil() as u32).max(w), ((ih as f32 * s).ceil() as u32).max(h));
    let scaled = image::imageops::resize(&img, sw, sh, image::imageops::FilterType::Triangle);
    let (x0, y0) = ((sw - w) / 2, (sh - h) / 2);
    Ok(image::imageops::crop_imm(&scaled, x0, y0, w, h).to_image().into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sharp step in the picture pulls a blurry matte edge onto it.
    #[test]
    fn guided_filter_snaps_to_edges() {
        let (w, h) = (64usize, 8usize);
        // Picture: dark left half, bright right half (edge at x = 32).
        let guide: Vec<f32> = (0..w * h).map(|k| if k % w < 32 { 0.1 } else { 0.9 }).collect();
        // Matte: a soft ramp from x = 24 to x = 40.
        let p: Vec<f32> = (0..w * h).map(|k| ((k % w) as f32 - 24.0) / 16.0).map(|v| v.clamp(0.0, 1.0)).collect();
        let q = guided_filter_rgb(&[guide.clone(), guide.clone(), guide], &p, w, h, 5, 0.004);
        let at = |x: usize| q[4 * w + x];
        assert!(at(28) < 0.25, "left of the edge should be background, got {}", at(28));
        assert!(at(36) > 0.75, "right of the edge should be person, got {}", at(36));
    }

    /// A green screen (in light and in shadow) keys out; skin, grey cloth and a dark
    /// shirt stay; the green cast on an edge pixel is taken off.
    #[test]
    fn color_key() {
        let mut c = Compositor::new();
        c.key_mode = true;
        c.key_ai = false;
        c.keys = vec![[44, 150, 66]];
        let px: [[u8; 3]; 6] = [[44, 150, 66], [30, 100, 44], [200, 160, 140], [170, 170, 175], [25, 25, 30], [60, 140, 70]];
        let src: Vec<u8> = px.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        c.key_alpha(&src, 6, 1, None);
        assert_eq!(c.alpha[0], 0, "screen");
        assert_eq!(c.alpha[1], 0, "screen in shadow");
        assert_eq!(c.alpha[2], 255, "skin");
        assert_eq!(c.alpha[3], 255, "grey cloth");
        assert_eq!(c.alpha[4], 255, "dark shirt");
        let dir = key_chroma_dir(&c.keys).expect("green is a color");
        let d = despill([120, 170, 110], dir);
        assert!(d[1] < 150, "green cast should drop, got {d:?}");
        let skin = despill([200, 160, 140], dir);
        assert_eq!(skin, [200, 160, 140], "skin has no green cast to remove");
    }

    /// A white wall with shades: one pick misses the shadowed part, a second pick gets it,
    /// and skin survives both.
    #[test]
    fn several_picks() {
        let mut c = Compositor::new();
        c.key_mode = true;
        c.key_ai = false;
        c.keys = vec![[235, 235, 238]];
        c.key_sim = 0.08;
        let src: Vec<u8> = [[235u8, 235, 238], [150, 150, 158], [210, 170, 150]].iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        c.key_alpha(&src, 3, 1, None);
        assert_eq!(c.alpha[0], 0);
        assert!(c.alpha[1] > 200, "the shadow isn't covered by one pick yet");
        c.keys.push([150, 150, 158]);
        c.key_alpha(&src, 3, 1, None);
        assert_eq!(c.alpha[1], 0, "second pick covers the shadow");
        assert_eq!(c.alpha[2], 255, "skin stays");
        c.keys.pop(); // undo
        c.key_alpha(&src, 3, 1, None);
        assert!(c.alpha[1] > 200, "undo brings it back");
    }

    #[test]
    fn auto_key_finds_the_wall() {
        let (w, h) = (64usize, 36usize);
        let mut src = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let person = (20..44).contains(&x) && y > 10;
                let c = if person { [190, 150, 130] } else { [40, 160, 70] };
                src[i..i + 3].copy_from_slice(&c);
                src[i + 3] = 255;
            }
        }
        let k = auto_key(&src, w, h);
        assert!(k[1] > 140 && k[0] < 60, "got {k:?}");
    }

    #[test]
    fn choice_ids_round_trip() {
        for c in [Choice::Off, Choice::Blur, Choice::Builtin(3), Choice::File(PathBuf::from(r"C:\x\y.jpg"))] {
            assert_eq!(Choice::from_id(&c.id()), c);
        }
    }
}
