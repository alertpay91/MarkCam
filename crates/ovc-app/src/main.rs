//! MarkCam — desktop GUI (egui/eframe).
//!
//! Capture runs on a background thread so a slow camera never stalls the UI. The UI thread
//! re-runs the GPU effect chain on the most recent frame every repaint, so effect sliders
//! respond instantly and the preview stays smooth regardless of camera frame rate.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod library;
mod settings;
mod theme;
mod audio;
mod autostart;

/// A build to share with everyone (MARKCAM_PUBLIC=1 at build time): no ManyCam options
/// (only MarkCam's own effects), the VB-CABLE name kept visible on the microphone.
pub const PUBLIC: bool = option_env!("MARKCAM_PUBLIC").is_some();
mod banner;
mod maker;
mod link;
mod screen;
mod tools;
mod vbg;
mod vcam;
mod video;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use eframe::egui;
use ovc_capture::{list_cameras, CamMode, CameraInfo, CameraSource, CaptureSource, TestPatternSource};
use ovc_core::{PixelFormat, VideoFormat};
use ovc_effects::{
    builtin::{BackgroundBlur, ChromaKey, ColorGrade},
    anim::AnimKind,
    cpu::{Filter, Warp, WarpKind},
    Effect,
};
use ovc_gpu::{EffectChain, GpuContext, OutputMode};

/// What the capture thread should produce. `Send` (just numbers).
#[derive(Clone, PartialEq)]
enum Spec {
    Test,
    Camera { idx: u32, w: u32, h: u32, fps: u32 },
    /// A screen area (or the whole screen), in physical pixels.
    Screen(screen::Area),
    /// A video file, looping.
    Media(PathBuf),
    /// A web link (YouTube, Twitch… live or not), with sound, at most this many lines.
    Link(String, u32),
}

/// What the capture thread reports back about the currently open source.
#[derive(Clone)]
struct CamStatus {
    negotiated: VideoFormat,
    modes: Vec<CamMode>,
}

/// Background capture: builds the source on its own thread (nokhwa `Camera` is !Send, so it
/// is never moved across threads), publishes the latest RGBA frame, the measured capture
/// fps, and the device's negotiated format + supported modes.
struct Capture {
    latest: Arc<Mutex<Option<(u64, u32, u32, Arc<[u8]>)>>>,
    spec: Arc<Mutex<Spec>>,
    gen: Arc<AtomicU64>,
    run: Arc<AtomicBool>,
    err: Arc<Mutex<Option<String>>>,
    status: Arc<Mutex<Option<CamStatus>>>,
    cam_fps: Arc<Mutex<f32>>,
    handle: Option<JoinHandle<()>>,
}

impl Capture {
    /// `ui` is woken whenever a new frame lands, so the UI repaints at the camera rate
    /// instead of polling.
    fn start(ui: egui::Context, fx: Arc<Mutex<Fx>>) -> Self {
        let latest = Arc::new(Mutex::new(None));
        let spec = Arc::new(Mutex::new(Spec::Test));
        let gen = Arc::new(AtomicU64::new(1));
        let run = Arc::new(AtomicBool::new(true));
        let err = Arc::new(Mutex::new(None));
        let status = Arc::new(Mutex::new(None));
        let cam_fps = Arc::new(Mutex::new(0.0));
        let handle = thread::spawn({
            let (latest, spec, gen, run, err, status, cam_fps) = (
                latest.clone(),
                spec.clone(),
                gen.clone(),
                run.clone(),
                err.clone(),
                status.clone(),
                cam_fps.clone(),
            );
            move || worker(latest, spec, gen, run, err, status, cam_fps, ui, fx)
        });
        Capture { latest, spec, gen, run, err, status, cam_fps, handle: Some(handle) }
    }

    fn set(&self, s: Spec) {
        *self.spec.lock().unwrap() = s;
        self.gen.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.run.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn build_source(spec: &Spec) -> anyhow::Result<Box<dyn CaptureSource>> {
    match spec {
        Spec::Test => Ok(Box::new(TestPatternSource::new(1280, 720, 30, 1))),
        &Spec::Camera { idx, w, h, fps } => {
            let req = VideoFormat { format: PixelFormat::Rgba8, width: w, height: h, fps_num: fps, fps_den: 1 };
            Ok(Box::new(CameraSource::open(idx, req)?))
        }
        Spec::Screen(area) => Ok(Box::new(screen::ScreenSource::open(*area)?)),
        // With ffmpeg available a file plays with its sound; otherwise Media Foundation
        // (picture only).
        Spec::Media(path) if tools::ffmpeg().is_file() => Ok(Box::new(link::LinkSource::open_file(path)?)),
        Spec::Media(path) => Ok(Box::new(video::MediaSource::open(path)?)),
        Spec::Link(url, q) => Ok(Box::new(link::LinkSource::open_url(url, *q)?)),
    }
}

#[allow(clippy::too_many_arguments)]
fn worker(
    latest: Arc<Mutex<Option<(u64, u32, u32, Arc<[u8]>)>>>,
    spec: Arc<Mutex<Spec>>,
    gen: Arc<AtomicU64>,
    run: Arc<AtomicBool>,
    err: Arc<Mutex<Option<String>>>,
    status: Arc<Mutex<Option<CamStatus>>>,
    cam_fps: Arc<Mutex<f32>>,
    ui: egui::Context,
    fx: Arc<Mutex<Fx>>,
) {
    let mut cur_gen = 0u64;
    let mut src: Option<Box<dyn CaptureSource>> = None;
    let mut seq = 0u64;
    // Capture-fps measurement.
    let mut frames = 0u32;
    let mut tick = std::time::Instant::now();
    let mut last_frame: Option<std::time::Instant> = None;
    // Idle webcam: closed while nobody sees the picture (MarkCam minimized, no app using the
    // virtual camera) — like ManyCam, the app then costs next to nothing. Checked twice a
    // second; reopened as soon as an app starts pulling frames or the window comes back.
    let mut idle = false;
    let mut unseen_since: Option<std::time::Instant> = None;
    let mut idle_check = std::time::Instant::now();
    while run.load(Ordering::SeqCst) {
        if idle_check.elapsed() >= std::time::Duration::from_millis(if idle { 200 } else { 500 }) {
            idle_check = std::time::Instant::now();
            // Webcam (and the test pattern standing in for one); never a video or link,
            // which would lose their place.
            let is_camera = matches!(*spec.lock().unwrap(), Spec::Camera { .. } | Spec::Test);
            let unseen = is_camera && {
                let mut f = fx.lock().unwrap();
                f.idle_camera && f.ui_every == 0 && !f.vcam.watched()
            };
            if unseen {
                let since = *unseen_since.get_or_insert_with(std::time::Instant::now);
                if !idle && since.elapsed() >= std::time::Duration::from_secs(2) {
                    idle = true;
                    src = None; // closes the webcam (its light goes off)
                    *cam_fps.lock().unwrap() = 0.0;
                    fx.lock().unwrap().camera_idle = true;
                }
            } else {
                unseen_since = None;
                if idle {
                    idle = false;
                    cur_gen = 0; // reopen below
                    last_frame = None;
                    fx.lock().unwrap().camera_idle = false;
                }
            }
        }
        if idle {
            thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        let g = gen.load(Ordering::SeqCst);
        if g != cur_gen {
            cur_gen = g;
            let s = spec.lock().unwrap().clone();
            *err.lock().unwrap() = None;
            *status.lock().unwrap() = None;
            *cam_fps.lock().unwrap() = 0.0;
            frames = 0;
            tick = std::time::Instant::now();
            match build_source(&s) {
                Ok(x) => {
                    *status.lock().unwrap() =
                        Some(CamStatus { negotiated: x.format(), modes: x.modes() });
                    src = Some(x);
                }
                Err(e) => {
                    *err.lock().unwrap() = Some(e.to_string());
                    src = None;
                }
            }
        }
        match src.as_mut() {
            Some(s) => match s.next_frame() {
                Ok(Some(f)) => {
                    seq += 1;
                    // The face tracker reads the raw frame; the pipeline makes the picture
                    // (and wakes the UI when it hands over a preview).
                    *latest.lock().unwrap() = Some((seq, f.format.width, f.format.height, f.data.clone()));
                    let t_lock = std::time::Instant::now();
                    if let Some(prev) = last_frame.replace(t_lock) {
                        let gap = (t_lock - prev).as_micros() as u64;
                        JITTER[0].fetch_max(gap, Ordering::Relaxed);
                        if gap > 40_000 {
                            JITTER[3].fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    let mut guard = fx.lock().unwrap();
                    guard.in_seq = seq;
                    let t_run = std::time::Instant::now();
                    guard.run(&ui, Some((&f.data, f.format.width, f.format.height)));
                    drop(guard);
                    JITTER[1].fetch_max((t_run - t_lock).as_micros() as u64, Ordering::Relaxed);
                    JITTER[2].fetch_max(t_run.elapsed().as_micros() as u64, Ordering::Relaxed);
                    frames += 1;
                    let dt = tick.elapsed().as_secs_f32();
                    if dt >= 0.5 {
                        *cam_fps.lock().unwrap() = frames as f32 / dt;
                        frames = 0;
                        tick = std::time::Instant::now();
                    }
                }
                Ok(None) => {
                    rerun_if_due(&fx, &ui);
                    thread::sleep(std::time::Duration::from_millis(5))
                }
                Err(e) => {
                    *err.lock().unwrap() = Some(e.to_string());
                    src = None;
                    thread::sleep(std::time::Duration::from_millis(300));
                }
            },
            None => {
                rerun_if_due(&fx, &ui);
                thread::sleep(std::time::Duration::from_millis(30))
            }
        }
    }
}

/// No new frames (paused video, frozen, no camera): re-run the pipeline on the held picture
/// when settings changed or something animates on it (~30×/s at most).
fn rerun_if_due(fx: &Mutex<Fx>, ui: &egui::Context) {
    let mut fx = fx.lock().unwrap();
    let animating = fx.frozen || !fx.anims.is_empty() || fx.overlays.iter().any(|o| o.on && (o.frames.len() > 1 || o.video.is_some()));
    if (fx.dirty || animating) && fx.last_run.elapsed() >= std::time::Duration::from_millis(33) {
        fx.run(ui, None);
    }
}

/// A visual composited on top of the processed frame: a loose PNG/JPG, or a ManyCam effect
/// (still `object`/`fullscreen_object` or animated `animation`). Placed at a normalized
/// rectangle (fractions of the frame) and alpha-blended on the CPU after the GPU chain.
/// Animated effects advance frames by their per-frame GIF delays.
struct Overlay {
    name: String,
    frames: Vec<ovc_mcam::Frame>,
    repeat: bool,
    pos: [f32; 2],  // top-left, fraction of frame
    size: [f32; 2], // base size, fraction of frame
    scale: f32,     // user multiplier on size
    opacity: f32,
    on: bool,
    // animation playback
    cur: usize,
    started: std::time::Instant,
    // current frame rescaled to a pixel rect, cached by (frame index, dst w, dst h)
    cache: Vec<u8>,
    cache_key: (usize, u32, u32),
    /// Every animation frame already resized to `scaled_size` (empty = not yet), so a
    /// looping GIF is resized once, not 30 times a second.
    scaled: Vec<Vec<u8>>,
    scaled_size: (u32, u32),
    // Face mask: pivot points (normalized to the mask image) matching 68 dlib landmarks.
    // `Some` means this layer is warped onto detected landmarks instead of rectangle-placed.
    face: Option<Vec<[f32; 2]>>,
    /// Face-anchored sticker (ManyCam `type: "face"` without mask pivots: hats, glasses,
    /// beards, emoji faces): `(center offset, size)` in face units, where the origin is the
    /// point between the eyes, +y points down the face, and 1 unit ≈ 0.84 × eye distance.
    anchor: Option<([f32; 2], [f32; 2])>,
    /// Smoothed face pose `(cx, cy, unit, angle)` in pixels/radians, so stickers don't
    /// jitter with landmark noise.
    pose: Option<[f32; 4]>,
    /// Normalized bounding box `[x0, y0, x1, y1]` of where it was last drawn (for clicking
    /// / selecting face-following layers, whose position isn't `pos`).
    drawn: Option<[f32; 4]>,
    /// Video layer: frames come from a background decoder instead of `frames`.
    video: Option<video::VideoStream>,
    video_seq: u64,
    /// Key out a green-screen background (for green-screen videos / images).
    remove_green: bool,
    /// Where it was loaded from (persisted so the layer comes back on the next start).
    source: Option<settings::LayerSource>,
    /// Library thumbnail key, for the layers list.
    thumb_key: Option<String>,
}

/// Make green-screen pixels transparent (and remove green spill on the edges): alpha
/// falls from 1 to 0 as green dominates red/blue by 20 → 60 levels.
fn key_out_green(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
        let excess = g - r.max(b);
        if excess > 20 && g > 60 {
            let keep = (1.0 - (excess - 20) as f32 / 40.0).clamp(0.0, 1.0);
            px[3] = (px[3] as f32 * keep) as u8;
            px[1] = r.max(b) as u8; // despill
        }
    }
}

/// ManyCam's face unit relative to the distance between the eye centers. Derived from
/// its stickers: glasses ≈ 2.9 units wide (≈ face width ≈ 2.4 eye distances), mouth at
/// +1.19 units below the eyes (≈ one eye distance).
const FACE_UNIT_PER_IPD: f32 = 0.84;

/// A PNG / JPG / (animated) GIF as an effect layer.
fn overlay_from_file(path: &Path) -> anyhow::Result<Overlay> {
    let mut ov = overlay_from_effect(ovc_mcam::load_image_effect(path)?);
    ov.source = Some(settings::LayerSource::Image(path.to_path_buf()));
    ov.thumb_key = Some(path.to_string_lossy().into_owned());
    Ok(ov)
}

/// A video file (MP4…) as a looping effect layer.
fn overlay_from_video(path: &Path) -> anyhow::Result<Overlay> {
    let v = video::VideoStream::open(path)?;
    let (_, rgba, w, h) = v.latest().ok_or_else(|| anyhow::anyhow!("no video frame"))?;
    let e = ovc_mcam::Effect {
        name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        author: String::new(),
        kind: ovc_mcam::Kind::Animation,
        raw_type: "object".into(),
        position: [0.0, 0.0],
        size: [1.0, 1.0],
        canvas_aspect: 0.0,
        repeat: true,
        frames: vec![ovc_mcam::Frame { rgba: rgba.as_ref().clone(), w, h, delay_ms: 0 }],
        face: None,
    };
    let mut ov = overlay_from_effect(e);
    ov.video = Some(v);
    ov.source = Some(settings::LayerSource::Video(path.to_path_buf()));
    ov.thumb_key = Some(path.to_string_lossy().into_owned());
    Ok(ov)
}

fn layer_source(r: &ovc_mcam::EffectRef) -> settings::LayerSource {
    match &r.file {
        Some(p) if video::is_video_file(p) => settings::LayerSource::Video(p.clone()),
        Some(p) if ovc_mcam::is_image_file(p) => settings::LayerSource::Image(p.clone()),
        Some(p) => settings::LayerSource::McvFile(p.clone()),
        None => settings::LayerSource::PackEntry { pack: r.pack_path.clone(), mcv: r.mcv_name.clone() },
    }
}

/// Re-create a saved layer (on startup). `None` if its file is gone.
fn overlay_from_saved(s: &settings::SavedLayer) -> Option<Overlay> {
    use settings::LayerSource as L;
    let (mut ov, key) = match &s.source {
        L::Image(p) => (overlay_from_file(p).ok()?, Some(p.to_string_lossy().into_owned())),
        L::PackEntry { pack, mcv } => {
            (overlay_from_effect(ovc_mcam::load_effect(pack, mcv).ok()?), Some(format!("{}|{}", pack.display(), mcv)))
        }
        L::McvFile(p) => (overlay_from_effect(ovc_mcam::load_mcv_file(p).ok()?), Some(p.to_string_lossy().into_owned())),
        L::Video(p) => (overlay_from_video(p).ok()?, Some(p.to_string_lossy().into_owned())),
    };
    ov.remove_green = s.remove_green;
    ov.source = Some(s.source.clone());
    ov.thumb_key = key;
    ov.pos = s.pos;
    ov.size = s.size;
    ov.scale = s.scale;
    ov.opacity = s.opacity;
    ov.on = s.on;
    Some(ov)
}

fn overlay_from_effect(e: ovc_mcam::Effect) -> Overlay {
    let face = e.face.as_ref().filter(|f| f.pivots.len() >= 3).map(|f| f.pivots.clone());
    // Start at a sensible size: full-frame for borders, otherwise ~1/3 of the frame height,
    // aspect-correct (assuming a ~16:9 preview), centered. ManyCam's stored sizes are canvas
    // fractions that often come out oversized here, so we normalize instead.
    let (pos, size) = if e.raw_type == "fullscreen_object" {
        ([0.0, 0.0], [1.0, 1.0])
    } else {
        let (iw, ih) = e.frames.first().map(|f| (f.w as f32, f.h as f32)).unwrap_or((1.0, 1.0));
        let hfrac = 0.33_f32;
        let wfrac = (hfrac * (iw / ih.max(1.0)) * (9.0 / 16.0)).clamp(0.05, 0.9);
        ([0.5 - wfrac / 2.0, 0.5 - hfrac / 2.0], [wfrac, hfrac])
    };
    let anchored = e.kind == ovc_mcam::Kind::FaceMask && face.is_none();
    let anchor = anchored.then_some((e.position, e.size));
    let mut o = Overlay::new(e.name, e.frames, e.repeat, pos, size);
    o.face = face;
    o.anchor = anchor;
    o
}

impl Overlay {
    fn new(name: String, frames: Vec<ovc_mcam::Frame>, repeat: bool, pos: [f32; 2], size: [f32; 2]) -> Self {
        Overlay {
            name,
            frames,
            repeat,
            pos,
            size,
            scale: 1.0,
            opacity: 1.0,
            on: true,
            cur: 0,
            started: std::time::Instant::now(),
            cache: Vec::new(),
            cache_key: (usize::MAX, 0, 0),
            scaled: Vec::new(),
            scaled_size: (0, 0),
            face: None,
            anchor: None,
            pose: None,
            drawn: None,
            video: None,
            video_seq: 0,
            remove_green: false,
            source: None,
            thumb_key: None,
        }
    }

    /// Follows the face (mask or anchored sticker) rather than sitting at `pos`.
    fn follows_face(&self) -> bool {
        self.face.is_some() || self.anchor.is_some()
    }

    /// Normalized rect for hit-testing / the selection outline.
    fn hit_rect(&self) -> Option<[f32; 4]> {
        if self.follows_face() {
            self.drawn
        } else {
            Some([self.pos[0], self.pos[1], self.pos[0] + self.size[0] * self.scale, self.pos[1] + self.size[1] * self.scale])
        }
    }

    /// Step the GIF animation by wall-clock time.
    fn advance(&mut self) {
        if self.frames.len() > 1 {
            let d = self.frames[self.cur].delay_ms.max(20) as u128;
            if self.started.elapsed().as_millis() >= d {
                let next = self.cur + 1;
                self.cur = if next >= self.frames.len() {
                    if self.repeat {
                        0
                    } else {
                        self.frames.len() - 1
                    }
                } else {
                    next
                };
                self.started = std::time::Instant::now();
            }
        }
    }

    /// The current frame resized to `tw`x`th` in `self.cache` (green keyed out if asked).
    fn ensure_cache(&mut self, tw: u32, th: u32) -> bool {
        if self.cache_key == (self.cur, tw, th) {
            return true;
        }
        if self.scaled_size != (tw, th) || self.scaled.len() != self.frames.len() {
            self.scaled = vec![Vec::new(); self.frames.len()];
            self.scaled_size = (tw, th);
        } else if self.cache_key.0 < self.scaled.len() && (self.cache_key.1, self.cache_key.2) == (tw, th) {
            // Park the frame we leave; it comes back on the next loop.
            self.scaled[self.cache_key.0] = std::mem::take(&mut self.cache);
        }
        if self.scaled.get(self.cur).is_some_and(|v| !v.is_empty()) {
            self.cache = std::mem::take(&mut self.scaled[self.cur]);
            self.cache_key = (self.cur, tw, th);
            return true;
        }
        {
            let Some(fr) = self.frames.get(self.cur) else { return false };
            let src = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(fr.w, fr.h, fr.rgba.clone());
            self.cache = match src {
                Some(buf) => image::imageops::resize(&buf, tw, th, image::imageops::FilterType::Triangle).into_raw(),
                None => return false,
            };
            if self.remove_green {
                key_out_green(&mut self.cache);
            }
            self.cache_key = (self.cur, tw, th);
        }
        true
    }

    /// For video layers: take the decoder's newest frame (forcing a re-resize).
    fn pull_video(&mut self) {
        let Some(v) = &self.video else { return };
        if let Some((seq, rgba, w, h)) = v.latest() {
            if seq != self.video_seq {
                self.video_seq = seq;
                self.frames = vec![ovc_mcam::Frame { rgba: rgba.as_ref().clone(), w, h, delay_ms: 0 }];
                self.cur = 0;
                self.cache_key = (usize::MAX, 0, 0);
                self.scaled_size = (0, 0);
            }
        }
    }

    /// Draw a face-anchored sticker on the face described by 68 normalized landmarks:
    /// placed relative to the eyes, scaled with the face and rotated with head tilt.
    fn composite_face(&mut self, rgba: &mut [u8], w: u32, h: u32, lms: &[[f32; 2]]) {
        let Some((apos, asize)) = self.anchor else { return };
        if self.frames.is_empty() || lms.len() < 48 || w == 0 || h == 0 {
            return;
        }
        self.advance();
        let mean = |r: std::ops::Range<usize>| {
            let n = r.len() as f32;
            let (sx, sy) = lms[r].iter().fold((0.0, 0.0), |a, p| (a.0 + p[0], a.1 + p[1]));
            (sx / n * w as f32, sy / n * h as f32)
        };
        // Image-left eye (36..41) and image-right eye (42..47).
        let (l, r) = (mean(36..42), mean(42..48));
        let (dx, dy) = (r.0 - l.0, r.1 - l.1);
        let ipd = (dx * dx + dy * dy).sqrt();
        if ipd < 4.0 {
            return;
        }
        let now = [(l.0 + r.0) / 2.0, (l.1 + r.1) / 2.0, ipd * FACE_UNIT_PER_IPD, dy.atan2(dx)];
        // Smooth the pose (landmarks jitter by a pixel or two frame to frame).
        let pose = self.pose.get_or_insert(now);
        for i in 0..4 {
            pose[i] += (now[i] - pose[i]) * 0.5;
        }
        let [mx, my, unit, angle] = *pose;
        let (sn, cs) = angle.sin_cos();
        let (ox, oy) = (apos[0] * unit, apos[1] * unit);
        let (cx, cy) = (mx + ox * cs - oy * sn, my + ox * sn + oy * cs);
        // Quantize the size so slow zooming doesn't re-resize the image every frame.
        // ~3% steps: small zooms keep the resized animation frames.
        let q = |v: f32| {
            let step = (v * 0.03).max(2.0);
            ((v / step).round() * step).clamp(2.0, 4096.0) as u32
        };
        let (tw, th) = (q(asize[0] * unit * self.scale), q(asize[1] * unit * self.scale));
        if !self.ensure_cache(tw, th) {
            return;
        }
        // Rotated blit: bounding box of the rotated rect, inverse-rotate each pixel.
        let (hw, hh) = (tw as f32 / 2.0, th as f32 / 2.0);
        let ex = hw * cs.abs() + hh * sn.abs();
        let ey = hw * sn.abs() + hh * cs.abs();
        let (x0, x1) = ((cx - ex).floor().max(0.0) as usize, ((cx + ex).ceil().max(0.0) as usize).min(w as usize));
        let (y0, y1) = ((cy - ey).floor().max(0.0) as usize, ((cy + ey).ceil().max(0.0) as usize).min(h as usize));
        let op = (self.opacity.clamp(0.0, 1.0) * 256.0) as u32;
        let (twu, thu) = (tw as usize, th as usize);
        for y in y0..y1 {
            let fy = y as f32 + 0.5 - cy;
            for x in x0..x1 {
                let fx = x as f32 + 0.5 - cx;
                let u = fx * cs + fy * sn + hw;
                let v = -fx * sn + fy * cs + hh;
                if u < 0.0 || v < 0.0 || u >= tw as f32 || v >= th as f32 {
                    continue;
                }
                let si = ((v as usize).min(thu - 1) * twu + (u as usize).min(twu - 1)) * 4;
                let a = self.cache[si + 3] as u32 * op >> 8;
                if a == 0 {
                    continue;
                }
                let di = (y * w as usize + x) * 4;
                for c in 0..3 {
                    rgba[di + c] = ((rgba[di + c] as u32 * (255 - a) + self.cache[si + c] as u32 * a) / 255) as u8;
                }
            }
        }
        self.drawn = Some([x0 as f32 / w as f32, y0 as f32 / h as f32, x1 as f32 / w as f32, y1 as f32 / h as f32]);
    }

    /// Advance animation (if any) and alpha-blend the current frame onto `rgba`
    /// (packed RGBA8, `w*h*4`) at the placed, scaled rectangle.
    fn composite(&mut self, rgba: &mut [u8], w: u32, h: u32) {
        self.pull_video();
        if self.frames.is_empty() || w == 0 || h == 0 {
            return;
        }
        self.advance();
        let tw = ((self.size[0] * self.scale) * w as f32).round().clamp(1.0, 8192.0) as u32;
        let th = ((self.size[1] * self.scale) * h as f32).round().clamp(1.0, 8192.0) as u32;
        let x0 = (self.pos[0] * w as f32).round() as i32;
        let y0 = (self.pos[1] * h as f32).round() as i32;
        if !self.ensure_cache(tw, th) {
            return;
        }
        let op = self.opacity.clamp(0.0, 1.0);
        for row in 0..th as i32 {
            let fy = y0 + row;
            if fy < 0 || fy >= h as i32 {
                continue;
            }
            for col in 0..tw as i32 {
                let fx = x0 + col;
                if fx < 0 || fx >= w as i32 {
                    continue;
                }
                let si = ((row as u32 * tw + col as u32) * 4) as usize;
                let a = self.cache[si + 3] as f32 / 255.0 * op;
                if a <= 0.0 {
                    continue;
                }
                let di = ((fy as u32 * w + fx as u32) * 4) as usize;
                for c in 0..3 {
                    let base = rgba[di + c] as f32;
                    let top = self.cache[si + c] as f32;
                    rgba[di + c] = (base * (1.0 - a) + top * a).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

/// Active mouse gesture on an overlay in the preview.
#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    Move,
    Resize,
    /// Moving the center of a bulge/pinch/swirl distortion.
    WarpCenter,
}

/// The ONNX models the face tracker needs, with direct download URLs: UltraFace (MIT) finds
/// the face, Google MediaPipe Face Mesh V2 (Apache-2.0) places the points. (InsightFace's
/// 2d106det, used before, is licensed for non-commercial research only; it is still used if
/// an old install has only that.)
const MODEL_FILES: [(&str, &str); 2] = [
    ("version-RFB-320.onnx", "https://huggingface.co/onnxmodelzoo/version-RFB-320/resolve/main/version-RFB-320.onnx"),
    ("facemesh_v2.onnx", "https://huggingface.co/fernandotonon/QtMeshEditor-facemesh-onnx/resolve/main/face_landmarks.onnx"),
];
const OLD_LANDMARKS: &str = "2d106det.onnx";

fn model_search_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("OVC_MODELS_DIR") {
        v.push(PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            v.push(p.join("models"));
        }
    }
    v.push(PathBuf::from("models"));
    if let Some(d) = std::env::var_os("APPDATA") {
        v.push(Path::new(&d).join("MarkCam").join("models"));
    }
    v
}

/// First directory that holds all required model files, if any.
fn find_models() -> Option<PathBuf> {
    let ok = |d: &PathBuf| d.join(MODEL_FILES[0].0).is_file() && (d.join(MODEL_FILES[1].0).is_file() || d.join(OLD_LANDMARKS).is_file());
    let dirs = model_search_dirs();
    // Prefer a folder with Face Mesh; else one with the old landmark model.
    dirs.iter().find(|d| MODEL_FILES.iter().all(|(n, _)| d.join(n).is_file())).or_else(|| dirs.iter().find(|d| ok(d))).cloned()
}

fn models_target_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(|d| Path::new(&d).join("MarkCam").join("models"))
        .unwrap_or_else(|| PathBuf::from("models"))
}

/// Downloads the face models on a background thread.
struct Download {
    status: Arc<Mutex<String>>,
    active: Arc<AtomicBool>,
}

impl Download {
    fn start() -> Self {
        let status = Arc::new(Mutex::new("starting…".to_string()));
        let active = Arc::new(AtomicBool::new(true));
        thread::spawn({
            let (status, active) = (status.clone(), active.clone());
            move || {
                let dir = models_target_dir();
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    *status.lock().unwrap() = format!("cannot create {}: {e}", dir.display());
                    active.store(false, Ordering::SeqCst);
                    return;
                }
                for (name, url) in MODEL_FILES {
                    let dst = dir.join(name);
                    if dst.is_file() {
                        continue;
                    }
                    *status.lock().unwrap() = format!("downloading {name}…");
                    if let Err(e) = download_one(url, &dst) {
                        *status.lock().unwrap() = format!("failed {name}: {e}");
                        active.store(false, Ordering::SeqCst);
                        return;
                    }
                }
                *status.lock().unwrap() = "done".to_string();
                active.store(false, Ordering::SeqCst);
            }
        });
        Download { status, active }
    }
}

fn download_one(url: &str, dst: &Path) -> anyhow::Result<()> {
    let resp = ureq::get(url).call()?;
    let tmp = dst.with_extension("part");
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&tmp)?;
    std::io::copy(&mut reader, &mut file)?;
    drop(file);
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

/// Face-tracking worker: owns the ONNX tracker on its own thread, consumes the latest
/// captured frame, and publishes the newest 68 landmarks (normalized). Runs only while
/// `want` is set (a face mask is active), so the heavy model costs nothing otherwise.
struct FaceWorker {
    want: Arc<AtomicBool>,
    run: Arc<AtomicBool>,
    landmarks: Arc<Mutex<Option<Vec<[f32; 2]>>>>,
    err: Arc<Mutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl FaceWorker {
    fn start(dir: &Path, latest: Arc<Mutex<Option<(u64, u32, u32, Arc<[u8]>)>>>) -> Self {
        let want = Arc::new(AtomicBool::new(true));
        let run = Arc::new(AtomicBool::new(true));
        let landmarks = Arc::new(Mutex::new(None));
        let err = Arc::new(Mutex::new(None));
        let det = dir.join("version-RFB-320.onnx");
        let (lmk, kind) = if dir.join(MODEL_FILES[1].0).is_file() {
            (dir.join(MODEL_FILES[1].0), ovc_ml::face::Lmk::FaceMesh)
        } else {
            (dir.join(OLD_LANDMARKS), ovc_ml::face::Lmk::Insight106)
        };
        let handle = thread::Builder::new().name("markcam-face".into()).spawn({
            let (want, run, landmarks, err) = (want.clone(), run.clone(), landmarks.clone(), err.clone());
            move || {
                // ONNX Runtime runs Face Mesh ~5x faster than tract (tract is the fallback).
                if let Some(rt) = vbg::runtime_dir() {
                    let _ = ovc_ml::seg::init_runtime(&rt);
                }
                let tracker = match ovc_ml::face::FaceTracker::load(&det, &lmk, kind) {
                    Ok(t) => t,
                    Err(e) => {
                        *err.lock().unwrap() = Some(e.to_string());
                        return;
                    }
                };
                use std::time::{Duration, Instant};
                let mut last_seq = u64::MAX;
                let mut track: Option<ovc_ml::face::Track> = None;
                let mut since_check = 0u32;
                let mut last: Option<Vec<[f32; 2]>> = None;
                let mut last_run = Instant::now();
                let mut moving = true;
                // A tiny picture of the face at the last model run, and when that was: an
                // unchanged face reuses its points (a teacher sits still most of the time).
                let mut face_thumb: Option<Vec<u8>> = None;
                let mut model_at = Instant::now();
                while run.load(Ordering::SeqCst) {
                    if !want.load(Ordering::SeqCst) {
                        track = None;
                        thread::sleep(Duration::from_millis(60));
                        continue;
                    }
                    // While the head is still, ~12 updates/s look the same as 30 (masks are
                    // smoothed anyway) and cost less than half; when it moves, up to 25/s.
                    // No face in view: look for one 4 times a second.
                    // Moving: up to 15 updates/s (stickers are smoothed in between); still: 8/s,
                    // and most of those are skipped by the face check below.
                    let interval = Duration::from_millis(if track.is_none() { 250 } else if moving { 66 } else { 125 });
                    let wait = interval.saturating_sub(last_run.elapsed());
                    if !wait.is_zero() {
                        thread::sleep(wait);
                    }
                    let frame = latest.lock().unwrap().clone();
                    match frame {
                        Some((seq, w, h, data)) if seq != last_seq => {
                            last_seq = seq;
                            last_run = Instant::now();
                            // The detector only finds the face; after that the landmarks follow
                            // it, and the detector re-checks about once a second.
                            // Same face as last time (and not too long ago): keep the points.
                            if let (Some(t), false) = (track, since_check + 1 >= 24) {
                                let thumb = face_luma(&data, w, h, t.crop());
                                let same = face_thumb.as_ref().is_some_and(|p| {
                                    p.len() == thumb.len() && p.iter().zip(&thumb).map(|(a, b)| a.abs_diff(*b) as u32).sum::<u32>() < thumb.len() as u32 * 3
                                });
                                if same && model_at.elapsed() < Duration::from_millis(1000) {
                                    moving = false;
                                    continue;
                                }
                            }
                            let check = track.is_none() || since_check >= 24;
                            since_check = if check { 0 } else { since_check + 1 };
                            model_at = Instant::now();
                            match tracker.track68(&data, w, h, track, check) {
                                Ok(Some((l, t))) => {
                                    track = Some(t);
                                    face_thumb = Some(face_luma(&data, w, h, t.crop()));
                                    // Largest point shift since the last update, normalized.
                                    moving = last.as_ref().map_or(true, |p| {
                                        p.iter().zip(&l).map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs())).fold(0.0, f32::max) > 0.01
                                    });
                                    last = Some(l.clone());
                                    *landmarks.lock().unwrap() = Some(l);
                                }
                                Ok(None) => {
                                    track = None;
                                    last = None;
                                    moving = false;
                                    *landmarks.lock().unwrap() = None;
                                }
                                Err(e) => *err.lock().unwrap() = Some(e.to_string()),
                            }
                        }
                        _ => thread::sleep(Duration::from_millis(10)),
                    }
                }
            }
        });
        FaceWorker { want, run, landmarks, err, handle: handle.ok() }
    }
}

/// A 16x16 gray picture of the face square (to tell whether the face changed).
fn face_luma(rgba: &[u8], w: u32, h: u32, (cx, cy, side): (f32, f32, f32)) -> Vec<u8> {
    const N: usize = 16;
    let mut out = Vec::with_capacity(N * N);
    let (wu, hu) = (w as usize, h as usize);
    if wu == 0 || hu == 0 || rgba.len() < wu * hu * 4 {
        return out;
    }
    let half = side * 0.35; // the inner face, not the hair / background around it
    for j in 0..N {
        let y = ((cy - half + (j as f32 + 0.5) / N as f32 * 2.0 * half) as isize).clamp(0, hu as isize - 1) as usize;
        for i in 0..N {
            let x = ((cx - half + (i as f32 + 0.5) / N as f32 * 2.0 * half) as isize).clamp(0, wu as isize - 1) as usize;
            let p = (y * wu + x) * 4;
            out.push(((rgba[p] as u32 * 77 + rgba[p + 1] as u32 * 150 + rgba[p + 2] as u32 * 29) >> 8) as u8);
        }
    }
    out
}

impl Drop for FaceWorker {
    fn drop(&mut self) {
        self.run.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Source changes requested from the top bar (applied after the panels are drawn).
enum SourceAction {
    FullScreen,
    PickArea,
    LastArea,
    PickVideo,
    LinkDialog,
    BackToCamera,
}

/// A link shortened for the source menu ("youtube.com/watch?v=…").
fn short_link(u: &str) -> String {
    let s = u.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
    if s.chars().count() > 28 {
        format!("{}…", s.chars().take(27).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Library organization requested from the UI (applied after the panels are drawn).
enum CatAction {
    /// Move an effect to a category (`None` = back to its own).
    Move(library::Item, Option<String>),
    /// Open the "new category" dialog, optionally moving an effect into it.
    New(Option<library::Item>),
    Rename(String),
    Delete(String),
    /// Delete one effect (own file → Recycle Bin; ManyCam effect → hidden).
    DeleteItem(library::Item),
    /// Bring back deleted ManyCam effects / original category names.
    Restore,
}

/// The small name dialog for creating / renaming a category.
enum CatDialog {
    New { name: String, item: Option<library::Item> },
    Rename { old: String, name: String },
}

/// Right-panel tabs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Effects,
    Background,
    Layers,
    Adjust,
    Audio,
    Setup,
    /// Name banners (opened from Effects / the top bar; not in the tab row).
    Banner,
}

impl Tab {
    fn id(self) -> &'static str {
        match self {
            Tab::Effects => "effects",
            Tab::Background => "background",
            Tab::Layers => "layers",
            Tab::Adjust => "adjust",
            Tab::Audio => "audio",
            Tab::Setup => "setup",
            Tab::Banner => "banner",
        }
    }
    fn from_id(s: &str) -> Tab {
        [Tab::Effects, Tab::Background, Tab::Layers, Tab::Adjust, Tab::Audio, Tab::Setup, Tab::Banner].into_iter().find(|t| t.id() == s).unwrap_or(Tab::Effects)
    }
}

/// One effect in the browser grid: thumbnail, name, and a GIF/FACE badge. Clicking adds it.
fn effect_tile(ui: &mut egui::Ui, thumbs: &mut library::Thumbs, item: &library::Item, w: f32, supported: bool) -> egui::Response {
    let img_h = w * 0.72;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, img_h + 24.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered() && supported;
        let painter = ui.painter();
        painter.rect_filled(rect, 8.0, if hovered { theme::CARD_HOVER } else { theme::CARD });
        if hovered {
            painter.rect_stroke(rect, 8.0, egui::Stroke::new(1.5_f32, theme::ACCENT));
        }
        let area = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 6.0), egui::vec2(w - 12.0, img_h - 6.0));
        match thumbs.get(item) {
            Some(t) => {
                let [tw, th] = t.size().map(|v| v as f32);
                let s = (area.width() / tw).min(area.height() / th);
                let fit = egui::Rect::from_center_size(area.center(), egui::vec2(tw * s, th * s));
                let tint = if supported { egui::Color32::WHITE } else { egui::Color32::from_gray(90) };
                painter.image(t.id(), fit, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
            }
            None => {
                painter.text(area.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(16.0), theme::TEXT_DIM);
            }
        }
        let mut job = egui::text::LayoutJob::simple_singleline(
            item.name().to_string(),
            egui::FontId::proportional(11.5),
            if supported { theme::TEXT } else { theme::TEXT_DIM },
        );
        job.wrap = egui::text::TextWrapping { max_width: w - 10.0, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let galley = ui.fonts(|f| f.layout_job(job));
        let pos = egui::pos2(rect.center().x - galley.size().x / 2.0, rect.max.y - 19.0);
        painter.galley(pos, galley, theme::TEXT);
        let badge = match item.kind() {
            ovc_mcam::Kind::Animation => Some(("GIF", theme::ACCENT)),
            ovc_mcam::Kind::FaceMask => Some(("FACE", theme::WARN)),
            _ => None,
        };
        if let Some((txt, col)) = badge {
            let r = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), egui::vec2(34.0, 15.0));
            painter.rect_filled(r, 4.0, col.gamma_multiply(0.85));
            painter.text(r.center(), egui::Align2::CENTER_CENTER, txt, egui::FontId::proportional(9.5), egui::Color32::BLACK);
        }
    }
    let tip = match (supported, item.kind()) {
        (false, ovc_mcam::Kind::FaceMask) => format!("{} — download the face models in Setup first", item.name()),
        (false, _) => format!("{} — not supported yet", item.name()),
        (true, _) => format!("{} · {}\nClick to add", item.name(), item.category),
    };
    resp.on_hover_text(tip)
}

const FALLBACK_RES: [(u32, u32); 3] = [(1920, 1080), (1280, 720), (640, 480)];

fn resolutions(modes: &[CamMode]) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = modes.iter().map(|m| (m.width, m.height)).collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.dedup();
    if v.is_empty() {
        v = FALLBACK_RES.to_vec();
    }
    v
}

fn fps_for(modes: &[CamMode], res: (u32, u32)) -> Vec<u32> {
    let mut v: Vec<u32> = modes.iter().filter(|m| (m.width, m.height) == res).map(|m| m.fps).collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.dedup();
    if v.is_empty() {
        v = vec![60, 30];
    }
    v
}

/// The picture pipeline — effects, layers, virtual-camera output — shared (behind a mutex)
/// between the capture thread, which runs it for every frame, and the UI, which edits its
/// settings and shows the preview it hands over. Keeping it off the UI thread means the
/// window can repaint rarely while MarkCam is in the background (e.g. behind ClassIn)
/// without the virtual camera losing a single frame.
struct Fx {
    gpu: GpuContext,
    cg_on: bool,
    cg: ColorGrade,
    blur_on: bool,
    blur: BackgroundBlur,
    ck_on: bool,
    ck: ChromaKey,
    filter: Filter,
    filter_amount: f32,
    vignette_amount: f32,
    vignette: ovc_effects::cpu::Vignette,
    warp: Warp,
    warp_r: ovc_effects::cpu::WarpRenderer,
    warp_follow_face: bool,
    anims: Vec<ovc_effects::anim::Anim>,
    anim_amount: f32,
    last_anim: std::time::Instant,
    smooth_lms: Option<Vec<[f32; 2]>>,
    work: Vec<u8>,
    work2: Vec<u8>,
    started: std::time::Instant,
    overlays: Vec<Overlay>,
    /// The Effect Maker's effect being built, shown on the face while the window is open.
    maker_preview: Option<Overlay>,
    /// The name banner on the picture (animating in / shown / animating out).
    banner: Option<banner::Live>,
    chain: Option<EffectChain>,
    chain_sig: (bool, bool, bool, u32, u32),
    vcam: vcam::Output,
    /// The current input picture (held while frozen) and its size.
    cur_rgba: Arc<[u8]>,
    w: u32,
    h: u32,

    // Set by the UI every update.
    mirror: bool,
    frozen: bool,
    preview_px: (f32, f32),
    /// The face tracker's landmarks, while it runs.
    lms_src: Option<Arc<Mutex<Option<Vec<[f32; 2]>>>>>,
    /// Hand the UI a preview every Nth frame (0 = none: minimized).
    ui_every: u32,
    /// The UI needs a preview now (first frame, mirror toggled…).
    preview_needed: bool,
    /// Settings may have changed: re-run on the held picture when no new frames come.
    dirty: bool,
    /// Close the webcam while nobody sees the picture (Setup option).
    idle_camera: bool,
    /// Virtual background, and the buffer it composites into.
    vbg: vbg::Compositor,
    bgbuf: Vec<u8>,
    /// Sequence number of the frame being handed to `run`, and frames waiting for their
    /// matte `(seq, data, w, h)`.
    in_seq: u64,
    /// Sequence number of the frame in `cur_rgba`.
    cur_seq: u64,
    bg_queue: std::collections::VecDeque<(u64, Arc<[u8]>, u32, u32)>,

    // To the UI.
    /// The webcam is closed for now because nobody sees the picture.
    camera_idle: bool,
    preview: Option<egui::ColorImage>,
    processed: u64,
    last_run: std::time::Instant,
    /// Timing totals for OVC_PERF (seconds), taken by the UI.
    perf: (u32, f64, f64, f64),
}

impl Fx {
    fn new(gpu: GpuContext) -> Self {
        Fx {
            gpu,
            cg_on: true,
            cg: ColorGrade::default(),
            blur_on: false,
            blur: BackgroundBlur::default(),
            ck_on: false,
            ck: ChromaKey::default(),
            filter: Filter::None,
            filter_amount: 1.0,
            vignette_amount: 0.0,
            vignette: Default::default(),
            warp: Warp::default(),
            warp_r: Default::default(),
            warp_follow_face: true,
            anims: Vec::new(),
            anim_amount: 0.6,
            last_anim: std::time::Instant::now(),
            smooth_lms: None,
            work: Vec::new(),
            work2: Vec::new(),
            started: std::time::Instant::now(),
            overlays: Vec::new(),
            maker_preview: None,
            banner: None,
            chain: None,
            chain_sig: (false, false, false, 0, 0),
            vcam: vcam::Output::new(),
            cur_rgba: Arc::from(Vec::new()),
            w: 0,
            h: 0,
            mirror: false,
            frozen: false,
            preview_px: (0.0, 0.0),
            lms_src: None,
            ui_every: 1,
            preview_needed: true,
            dirty: false,
            idle_camera: true,
            vbg: vbg::Compositor::new(),
            bgbuf: Vec::new(),
            in_seq: 0,
            cur_seq: 0,
            bg_queue: std::collections::VecDeque::new(),
            camera_idle: false,
            preview: None,
            processed: 0,
            last_run: std::time::Instant::now(),
            perf: (0, 0.0, 0.0, 0.0),
        }
    }

    /// Virtual background: show each camera frame together with the matte made from that
    /// same frame, so the outline never trails behind a moving head or hand (the picture
    /// waits a frame or two for it, like in Zoom). `None` = nothing to show this time.
    fn sync_with_matte(&mut self, data: &Arc<[u8]>, w: u32, h: u32) -> Option<(u64, Arc<[u8]>)> {
        let ready = match (self.vbg.needs_matte() && self.vbg.wait_for_matte, self.vbg.matte_seq()) {
            (true, Some(s)) => s,
            _ => {
                self.bg_queue.clear();
                return Some((self.in_seq, data.clone()));
            }
        };
        if self.bg_queue.back().is_some_and(|f| (f.2, f.3) != (w, h)) {
            self.bg_queue.clear();
        }
        self.bg_queue.push_back((self.in_seq, data.clone(), w, h));
        // Its matte is there (or the model fell behind: don't wait more than ~4 frames).
        let due = self.bg_queue.front().is_some_and(|f| f.0 <= ready) || self.bg_queue.len() > 4;
        if due {
            self.bg_queue.pop_front().map(|f| (f.0, f.1))
        } else {
            None
        }
    }

    /// Color grade is on by default but a no-op at its neutral settings.
    fn cg_active(&self) -> bool {
        self.cg_on && !self.cg.is_identity()
    }

    /// Only blur / chroma key need the GPU. A GPU pass costs a full-frame upload, readback
    /// and CPU/GPU sync per frame — more than the color grade itself — so color grade alone
    /// runs on the CPU (`ColorGrade::apply_fast`) and joins the GPU chain only when that
    /// chain runs anyway.
    fn gpu_needed(&self) -> bool {
        self.blur_on || self.ck_on
    }

    /// Effects run in the GPU chain (empty when no GPU pass is needed).
    fn enabled(&self) -> Vec<&dyn Effect> {
        let mut v: Vec<&dyn Effect> = Vec::new();
        if !self.gpu_needed() {
            return v;
        }
        if self.cg_active() {
            v.push(&self.cg);
        }
        if self.blur_on {
            v.push(&self.blur);
        }
        if self.ck_on {
            v.push(&self.ck);
        }
        v
    }

    fn rebuild_chain_if_needed(&mut self) {
        let sig = (self.cg_active(), self.blur_on, self.ck_on, self.w, self.h);
        if self.chain.is_some() && sig == self.chain_sig {
            return;
        }
        self.chain_sig = sig;
        let fx = self.enabled();
        if fx.is_empty() || self.w == 0 {
            self.chain = None;
            return;
        }
        let shaders: Vec<&str> = fx.iter().map(|e| e.wgsl()).collect();
        let sizes: Vec<usize> = fx.iter().map(|e| e.uniform_bytes().len()).collect();
        self.chain = EffectChain::new(&self.gpu, self.w, self.h, &shaders, &sizes, OutputMode::Rgba).ok();
    }

    /// Run the pipeline on a new input frame (`None`: again on the held one), feed the
    /// virtual camera, and — when the UI wants one — build its preview and wake it.
    fn run(&mut self, ui: &egui::Context, input: Option<(&Arc<[u8]>, u32, u32)>) {
        if let Some((data, w, h)) = input {
            // Frozen: keep the held picture; effects still run on it every frame.
            if !self.frozen || self.cur_rgba.is_empty() {
                let Some((seq, data)) = self.sync_with_matte(data, w, h) else { return };
                self.cur_seq = seq;
                if (w, h) != (self.w, self.h) {
                    self.w = w;
                    self.h = h;
                    self.chain = None;
                }
                self.cur_rgba = data;
            }
        }
        if self.cur_rgba.is_empty() || self.w == 0 {
            return;
        }
        self.dirty = false;
        self.last_run = std::time::Instant::now();
        let t_process = std::time::Instant::now();
        self.rebuild_chain_if_needed();
        let uniforms: Vec<Vec<u8>> = self.enabled().iter().map(|e| e.uniform_bytes()).collect();
        let (w, h) = (self.w, self.h);
        let overlays_on = self.overlays.iter().any(|o| o.on) || self.maker_preview.is_some() || self.banner.is_some();
        let raw = self.lms_src.as_ref().and_then(|l| l.lock().unwrap().clone());
        // Smooth landmark jitter (masks shimmer otherwise), but jump straight to big moves
        // so masks never lag behind a turning head.
        let landmarks = if self.frozen {
            // The face in the held picture doesn't move: keep masks where they were.
            self.smooth_lms.take()
        } else {
            match (raw, self.smooth_lms.take()) {
                (Some(new), Some(mut old)) if old.len() == new.len() => {
                    let jump = new.iter().zip(&old).map(|(a, b)| (a[0] - b[0]).abs() + (a[1] - b[1]).abs()).fold(0.0, f32::max);
                    let k = if jump > 0.04 { 1.0 } else { 0.55 };
                    for (o, n) in old.iter_mut().zip(&new) {
                        o[0] += (n[0] - o[0]) * k;
                        o[1] += (n[1] - o[1]) * k;
                    }
                    Some(old)
                }
                (new, _) => new,
            }
        };
        self.smooth_lms = landmarks.clone();

        // 1. Color: grade + filter fused into ONE matrix pass (when the GPU chain runs
        //    for blur/chroma key, the grade is already in it and only the filter is left).
        let filter_m = self.filter.matrix().mix(self.filter_amount.clamp(0.0, 1.0));
        let color = if self.chain.is_some() {
            filter_m
        } else if self.cg_active() {
            self.cg.matrix().then(&filter_m)
        } else {
            filter_m
        };
        let color_on = !color.is_identity();

        // 0. Virtual background: the person in front of the chosen background (everything
        //    below then works on that picture).
        self.vbg.release_unused();
        let mut bgbuf = std::mem::take(&mut self.bgbuf);
        let bg_on = self.vbg.active() && self.vbg.composite(&self.cur_rgba, self.cur_seq, w, h, &mut bgbuf);
        let base: &[u8] = if bg_on { &bgbuf } else { &self.cur_rgba };

        // Effects into the reused `work` buffer — or, with nothing to do, use the shared
        // camera frame as-is (no copy at all).
        let mut use_work = if let Some(chain) = self.chain.as_mut() {
            let refs: Vec<&[u8]> = uniforms.iter().map(|v| v.as_slice()).collect();
            let out = chain.process(&self.gpu, base, &refs);
            self.work.clear();
            self.work.extend_from_slice(out);
            if color_on {
                self.work2.resize(self.work.len(), 0);
                color.apply(&self.work, &mut self.work2);
                std::mem::swap(&mut self.work, &mut self.work2);
            }
            true
        } else if color_on {
            self.work.resize(base.len(), 0);
            color.apply(base, &mut self.work);
            true
        } else {
            false
        };

        // 2. Distortion (reads one buffer, writes the other). Bulge/pinch/swirl can
        //    follow the face: centered on the nose tip, sized to the face width.
        if self.warp.is_active() {
            let mut warp = self.warp;
            if warp.kind.has_center() && self.warp_follow_face {
                if let Some(l) = landmarks.as_ref().filter(|l| l.len() >= 68) {
                    warp.cx = l[30][0];
                    warp.cy = l[30][1];
                    let face_w = ((l[16][0] - l[0][0]) * w as f32).abs() / h as f32;
                    warp.radius = (face_w * 0.55 * self.warp.radius / 0.35).clamp(0.05, 1.0);
                }
            }
            let src: &[u8] = if use_work { &self.work } else { base };
            self.work2.resize(src.len(), 0);
            self.warp_r.apply(&warp, src, &mut self.work2, w, h, self.started.elapsed().as_secs_f32());
            std::mem::swap(&mut self.work, &mut self.work2);
            use_work = true;
        }

        // 3. Vignette, animated overlays and effect layers draw in place.
        if (overlays_on || self.vignette_amount > 0.0 || !self.anims.is_empty()) && !use_work {
            self.work.clear();
            self.work.extend_from_slice(base);
            use_work = true;
        }
        if self.vignette_amount > 0.0 {
            self.vignette.apply(&mut self.work, w, h, self.vignette_amount.clamp(0.0, 1.0));
        }
        let dt = self.last_anim.elapsed().as_secs_f32();
        self.last_anim = std::time::Instant::now();
        for a in &mut self.anims {
            a.render(&mut self.work, w, h, dt, self.anim_amount);
        }

        for ov in self.overlays.iter_mut().filter(|o| o.on) {
            let rgba = &mut self.work[..];
            if ov.anchor.is_some() {
                // Face sticker (hat, glasses, beard…): placed relative to the eyes.
                match landmarks.as_deref() {
                    Some(lms) => ov.composite_face(rgba, w, h, lms),
                    None => ov.drawn = None, // no face in view: hidden
                }
                continue;
            }
            match &ov.face {
                Some(pivots) => {
                    // Face mask: warp the mask image onto the detected 68 landmarks.
                    if let (Some(lms), Some(fr)) = (&landmarks, ov.frames.first()) {
                        let dst: Vec<[f32; 2]> = lms.iter().map(|p| [p[0] * w as f32, p[1] * h as f32]).collect();
                        ovc_ml::warp::warp_mask_onto(rgba, w, h, &fr.rgba, fr.w, fr.h, pivots, &dst, ov.opacity);
                        let (mut x0, mut y0, mut x1, mut y1) = (1f32, 1f32, 0f32, 0f32);
                        for p in lms {
                            (x0, y0, x1, y1) = (x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1]));
                        }
                        ov.drawn = Some([x0, y0, x1, y1]);
                    } else {
                        ov.drawn = None;
                    }
                }
                None => ov.composite(rgba, w, h),
            }
        }
        // The Effect Maker's effect on the face, and the name banner on top of everything.
        if let Some(ov) = &mut self.maker_preview {
            match landmarks.as_deref() {
                Some(lms) => ov.composite_face(&mut self.work, w, h, lms),
                None => ov.drawn = None,
            }
        }
        if let Some(b) = &mut self.banner {
            if !b.draw(&mut self.work, w, h) {
                self.banner = None;
            }
        }
        let rgba: &[u8] = if use_work { &self.work } else { base };
        // Testing aid: OVC_DUMP=<dir> saves every 15th composited frame (virtual background
        // tuning on a recorded clip).
        if bg_on && self.cur_seq % 15 == 0 {
            if let Some(dir) = std::env::var_os("OVC_DUMP") {
                if let Some(img) = image::RgbaImage::from_raw(w, h, rgba.to_vec()) {
                    let _ = image::DynamicImage::ImageRgba8(img).to_rgb8().save(Path::new(&dir).join(format!("f{:05}.png", self.cur_seq)));
                }
            }
        }

        // Feed the final frame (effects + overlays, un-mirrored) to the virtual camera.
        let t_vcam = std::time::Instant::now();
        self.vcam.frame(rgba, w, h);

        // Preview: only as often as the UI shows it (every frame in front, every 3rd in
        // the background, never minimized), and only as large as it is drawn.
        let t_preview = std::time::Instant::now();
        self.processed += 1;
        let due = self.ui_every != 0 && self.processed % self.ui_every as u64 == 0;
        if due || self.preview_needed {
            self.preview_needed = false;
            self.preview = Some(preview_image(rgba, w, h, self.preview_px, self.mirror));
            // A non-zero delay: egui paints a zero-delay request twice.
            ui.request_repaint_after(std::time::Duration::from_nanos(1));
        }
        self.perf.0 += 1;
        self.perf.1 += (t_vcam - t_process).as_secs_f64();
        self.perf.2 += (t_preview - t_vcam).as_secs_f64();
        self.perf.3 += t_preview.elapsed().as_secs_f64();
        self.bgbuf = bgbuf;
    }
}

struct Studio {
    /// The picture pipeline, run by the capture thread.
    fx: Arc<Mutex<Fx>>,
    cap: Capture,
    cameras: Vec<CameraInfo>,
    current: Spec,
    sel_res: (u32, u32),
    sel_fps: u32,
    mirror: bool,
    /// Picture held still (effects keep animating on it); `freeze_tick` paces the redraws.
    frozen: bool,
    freeze_tick: std::time::Instant,
    /// OVC_PERF timing log.
    perf: Option<Perf>,
    /// Transport slider position while being dragged.
    scrub: Option<f64>,


    // Our re-creations of ManyCam's Filters / Distortions (CPU, see ovc_effects::cpu).

    load_err: Option<String>,

    // Sources besides the camera.
    /// The camera to go back to from the screen / a video.
    last_camera: Option<u32>,
    screen_area: Option<screen::Area>,
    media_path: Option<PathBuf>,
    /// Area picking in progress: when to open the picker (after MarkCam minimizes), then
    /// the picker's answer.
    area_pick: Option<(std::time::Instant, Option<std::sync::mpsc::Receiver<Option<screen::Area>>>)>,
    /// Whether MarkCam's window is currently hidden from screen capture.
    capture_hidden: bool,

    // Audio: MarkCam Microphone + the sound of played videos / links.
    audio_ctl: Arc<audio::Controls>,
    audio_engine: Option<audio::Engine>,
    /// What the running engine was started for `(feed the cable, microphone)`.
    audio_key: Option<(bool, Option<String>)>,
    audio_route: bool,
    audio_mic: Option<String>,
    mics: Vec<audio::Device>,
    /// The virtual cable: `None` = not installed, `Some(named as MarkCam)`.
    cable: Option<bool>,
    audio_setup: Option<audio::Setup>,
    // Links.
    tools_fetch: Option<tools::Fetch>,
    link_dialog: Option<String>,
    /// A link waiting for yt-dlp / ffmpeg to finish downloading.
    pending_link: Option<String>,
    last_link: String,
    /// Max lines for links (360 / 480 / 720 / 1080).
    link_quality: u32,
    video_dl: Option<link::Download>,
    last_download: Option<PathBuf>,
    /// Folder picked for downloads (`None` = Videos\MarkCam).
    download_dir: Option<PathBuf>,

    // Performance options (Setup tab).
    /// Preview fps while MarkCam is behind another window (30 / 15 / 5 / 0).
    bg_preview_fps: u32,
    /// "Start with Windows" as last read from the registry.
    autostart: bool,
    /// Launched by the Windows Run entry: minimize on the first frame.
    start_minimized: bool,

    // Virtual background.
    seg: Option<vbg::SegWorker>,
    seg_idle_since: Option<std::time::Instant>,
    /// Background files offered (re-read when the tab opens after a change).
    bg_files: Option<Vec<(&'static str, PathBuf)>>,
    bg_thumbs: std::collections::HashMap<usize, egui::TextureHandle>,
    /// Eyedropper armed: the next click on the picture picks the key color.
    picking_key: bool,

    // Effect browser.
    lib: library::Library,
    thumbs: library::Thumbs,
    query: String,
    category: Option<String>,
    tab: Tab,
    cat_dialog: Option<CatDialog>,

    // Persistence: what was last written to settings.json, and when we last checked.
    saved: settings::Settings,
    save_check: std::time::Instant,
    shot_requested: bool,

    // Preview manipulation of overlays.
    selected: Option<usize>,
    drag_mode: Option<DragMode>,
    grab: (f32, f32),

    // Virtual camera output ("MarkCam" in ClassIn/Zoom).
    vcam_state: vcam::InstallState,
    vcam_installing: Option<Arc<AtomicBool>>,

    // Face tracking (for mask effects).
    face_models: Option<PathBuf>,
    tracker: Option<FaceWorker>,
    download: Option<Download>,

    /// Physical pixel size the preview is drawn at (from the last layout), so the preview
    /// texture is only as large as it is shown.
    preview_px: (f32, f32),
    /// When the last face mask was turned off; the tracker is shut down after a grace period.
    face_idle_since: Option<std::time::Instant>,


    // Effect Maker window and name banners.
    maker: Option<maker::Maker>,
    fetch: maker::Fetcher,
    banners: Vec<banner::BannerDef>,
    banner_sel: usize,
    banner_previews: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    /// Show the banner as soon as its icon has downloaded.
    banner_pending: bool,
    banner_wait: std::time::Instant,
    about_open: bool,

    tex: Option<egui::TextureHandle>,
    fps: f32,
    last: std::time::Instant,
    last_processed: u64,
}

impl Studio {
    fn new(ctx: egui::Context) -> anyhow::Result<Self> {
        theme::apply(&ctx);
        let cfg = settings::load();
        library::set_use_manycam(cfg.use_manycam && !PUBLIC);
        let fx = Arc::new(Mutex::new(Fx::new(GpuContext::new()?)));
        let mut s = Self {
            fx: fx.clone(),
            cap: Capture::start(ctx, fx),
            cameras: list_cameras().unwrap_or_default(),
            current: Spec::Test,
            sel_res: (1280, 720),
            sel_fps: 30,
            mirror: false,
            frozen: false,
            freeze_tick: std::time::Instant::now(),
            perf: Perf::from_env(),
            scrub: None,
            load_err: None,
            last_camera: None,
            screen_area: None,
            media_path: None,
            area_pick: None,
            capture_hidden: false,
            audio_ctl: audio::Controls::new(),
            audio_engine: None,
            audio_key: None,
            audio_route: true,
            audio_mic: None,
            mics: audio::microphones(),
            cable: audio::cable().map(|_| audio::cable_named()),
            audio_setup: None,
            tools_fetch: None,
            link_dialog: None,
            pending_link: None,
            last_link: String::new(),
            link_quality: 360,
            video_dl: None,
            last_download: None,
            download_dir: None,
            bg_preview_fps: 15,
            autostart: autostart::enabled(),
            start_minimized: std::env::args().any(|a| a == autostart::MINIMIZED_FLAG),
            seg: None,
            seg_idle_since: None,
            bg_files: None,
            bg_thumbs: std::collections::HashMap::new(),
            picking_key: false,
            lib: library::Library::start(None),
            thumbs: library::Thumbs::start(),
            query: String::new(),
            category: None,
            tab: Tab::Effects,
            cat_dialog: None,
            saved: cfg.clone(),
            save_check: std::time::Instant::now(),
            shot_requested: false,
            selected: None,
            drag_mode: None,
            grab: (0.0, 0.0),
            vcam_state: vcam::install_state(),
            vcam_installing: None,
            face_models: find_models(),
            tracker: None,
            download: None,
            preview_px: (0.0, 0.0),
            face_idle_since: None,
            maker: None,
            fetch: maker::Fetcher::default(),
            banners: vec![banner::BannerDef::default()],
            banner_sel: 0,
            banner_previews: std::collections::HashMap::new(),
            banner_pending: false,
            banner_wait: std::time::Instant::now(),
            about_open: false,
            tex: None,
            fps: 0.0,
            last: std::time::Instant::now(),
            last_processed: 0,
        };
        s.apply_settings(&cfg);
        // A virtual-cable install may have made itself Windows' default speakers /
        // microphone (then nothing is heard): put the real devices back.
        let fixed = audio::fix_defaults();
        if !fixed.is_empty() {
            s.load_err = Some(format!("restored Windows sound devices: {}", fixed.join(", ")));
        }
        // Test runs (OVC_TEST=1): never open the real webcam, never feed the virtual camera
        // — whatever the settings say — so a test can't disturb a call in progress.
        if std::env::var_os("OVC_TEST").is_some() {
            s.fx.lock().unwrap().vcam.on = false;
            // OVC_TEST_SCREEN=x,y,w,h tests the screen source (never the webcam).
            let area = std::env::var("OVC_TEST_SCREEN").ok().and_then(|v| {
                let n: Vec<i64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (n.len() == 4).then(|| screen::Area { x: n[0] as i32, y: n[1] as i32, w: n[2] as u32, h: n[3] as u32 })
            });
            s.current = area.map_or(Spec::Test, Spec::Screen);
            // OVC_TEST_MEDIA=<file> tests the video source / transport bar.
            if let Some(p) = std::env::var_os("OVC_TEST_MEDIA") {
                s.current = Spec::Media(PathBuf::from(p));
            }
            // OVC_TEST_BANNER=1 shows the name banner; OVC_TEST_MAKER=<project.maker.json>
            // opens the Effect Maker with that project (screenshot tests).
            if std::env::var_os("OVC_TEST_BANNER").is_some() {
                s.banner_pending = true;
            }
            s.about_open = std::env::var_os("OVC_TEST_ABOUT").is_some();
            if let Some(p) = std::env::var_os("OVC_TEST_MAKER") {
                if let Ok(project) = maker::load_project(Path::new(&p)) {
                    s.maker = Some(maker::Maker::open(project));
                }
            }
            // OVC_TEST_LINK=<url> tests links with the installed tools.
            if let Ok(u) = std::env::var("OVC_TEST_LINK") {
                s.current = Spec::Link(u, 360);
            }
            s.cap.set(s.current.clone());
        }
        // Explicit override for testing: OVC_CAMERA=<index>.
        if let Ok(idx) = std::env::var("OVC_CAMERA").unwrap_or_default().parse::<u32>() {
            let spec = Spec::Camera { idx, w: s.sel_res.0, h: s.sel_res.1, fps: s.sel_fps };
            s.current = spec.clone();
            s.cap.set(spec);
        }
        Ok(s)
    }

    /// Restore everything from saved settings: camera (matched by name, then index),
    /// format, effect parameters and the effect layers.
    fn apply_settings(&mut self, c: &settings::Settings) {
        let fx_arc = self.fx.clone();
        let mut fx_guard = fx_arc.lock().unwrap();
        let fx: &mut Fx = &mut fx_guard;
        self.sel_res = c.res;
        self.sel_fps = c.fps;
        self.mirror = c.mirror;
        fx.cg_on = c.cg_on;
        let [brightness, contrast, saturation, temperature] = c.cg;
        fx.cg.params = ovc_effects::builtin::ColorGradeParams { brightness, contrast, saturation, temperature };
        fx.blur_on = c.blur_on;
        fx.blur.params.radius = c.blur_radius;
        fx.ck_on = c.ck_on;
        fx.ck.params.key_rgb = c.ck_rgb;
        fx.ck.params.similarity = c.ck_similarity;
        fx.ck.params.smoothness = c.ck_smoothness;
        fx.filter = Filter::from_id(&c.filter);
        fx.filter_amount = c.filter_amount;
        fx.vignette_amount = c.vignette;
        fx.warp = Warp {
            kind: WarpKind::from_id(&c.warp),
            cx: c.warp_center[0],
            cy: c.warp_center[1],
            radius: c.warp_radius,
            strength: c.warp_strength,
        };
        fx.warp_follow_face = c.warp_follow_face;
        fx.anims = c.anims.iter().filter_map(|s| AnimKind::from_id(s)).map(ovc_effects::anim::Anim::new).collect();
        fx.anim_amount = c.anim_amount;
        self.audio_route = c.audio_route;
        self.audio_mic = c.audio_mic.clone().filter(|id| self.mics.iter().any(|m| &m.id == id));
        audio::Controls::set(&self.audio_ctl.mic_gain, c.mic_gain);
        audio::Controls::set(&self.audio_ctl.media_gain, c.media_gain);
        self.audio_ctl.monitor.store(c.hear_media, Ordering::Relaxed);
        self.last_link = c.last_link.clone();
        self.link_quality = c.link_quality;
        // Never on in a test run, not even for a moment (see `Studio::new`).
        fx.vcam.on = c.vcam_on && std::env::var_os("OVC_TEST").is_none();
        self.tab = Tab::from_id(&c.tab);
        if !c.banners.is_empty() {
            self.banners = c.banners.clone();
            self.banner_sel = c.banner_sel.min(self.banners.len() - 1);
        }
        self.category = c.category.clone();
        self.bg_preview_fps = c.background_preview_fps;
        fx.idle_camera = c.idle_camera;
        fx.vbg.choice = vbg::Choice::from_id(&c.background);
        fx.vbg.blur = c.background_blur;
        fx.vbg.quality = c.background_quality;
        fx.vbg.key_mode = c.key_mode;
        fx.vbg.keys = c.key_colors.clone();
        fx.vbg.key_sim = c.key_similarity;
        fx.vbg.key_smooth = c.key_smoothness;
        fx.vbg.key_ai = c.key_ai;
        self.download_dir = c.download_dir.clone();
        link::set_downloads_dir(self.download_dir.clone());
        fx.overlays = c.layers.iter().filter_map(overlay_from_saved).collect();

        let cam = c
            .camera_name
            .as_ref()
            .and_then(|n| self.cameras.iter().find(|ci| &ci.name == n))
            .or_else(|| c.camera_index.and_then(|i| self.cameras.iter().find(|ci| ci.index == i)))
            .map(|ci| ci.index);
        // First run (nothing saved): use the first real camera rather than the test pattern.
        let cam = if c.camera_name.is_none() && c.camera_index.is_none() && *c == settings::Settings::default() {
            self.cameras.first().map(|ci| ci.index)
        } else {
            cam
        };
        self.last_camera = cam;
        self.screen_area = c.screen_area;
        self.media_path = c.media_path.clone();
        // Test runs (OVC_TEST) never open the real webcam (or the screen).
        if std::env::var_os("OVC_TEST").is_some() {
            return;
        }
        let spec = match (c.source.as_str(), c.screen_area, &c.media_path) {
            ("screen", Some(a), _) => Some(Spec::Screen(a)),
            ("media", _, Some(p)) if p.is_file() => Some(Spec::Media(p.clone())),
            _ => cam.map(|idx| self.open_camera(idx)),
        };
        if let Some(spec) = spec {
            self.current = spec.clone();
            self.cap.set(spec);
        }
    }

    fn set_frozen(&mut self, on: bool) {
        self.frozen = on;
        self.freeze_tick = std::time::Instant::now();
    }

    /// Switch the picture to `spec` (remembering the camera for "Back to camera").
    fn switch_to(&mut self, spec: Spec) {
        self.frozen = false;
        if let Spec::Camera { idx, .. } = spec {
            self.last_camera = Some(idx);
        }
        if spec != self.current {
            self.current = spec.clone();
            self.cap.set(spec);
        }
    }

    /// Testing aid: with `OVC_SCREENSHOT=<file.png>` the app captures its own window ~6 s
    /// after start, saves it and exits (`OVC_QUERY` pre-fills the effect search).
    fn test_screenshot(&mut self, ctx: &egui::Context, fx: &Fx) {
        let Some(path) = std::env::var_os("OVC_SCREENSHOT") else { return };
        if fx.processed < 3 {
            if let Ok(q) = std::env::var("OVC_QUERY") {
                self.query = q;
            }
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            // The framebuffer's alpha is meaningless here; save it opaque.
            let raw: Vec<u8> = img.pixels.iter().flat_map(|c| [c.r(), c.g(), c.b(), 255]).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                let _ = buf.save(&path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.last.elapsed().as_secs_f32() < 0.6 && fx.processed > 150 && !self.shot_requested {
            self.shot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
    }

    /// "Play from link": paste a YouTube / Twitch / … link. The first time, the helper
    /// programs are downloaded first (progress shown here).
    fn show_link_dialog(&mut self, ctx: &egui::Context) {
        let fetching = self.tools_fetch.as_ref().map(|f| f.status.lock().unwrap().clone());
        if self.link_dialog.is_none() && fetching.is_none() {
            return;
        }
        let (mut play, mut close, mut download) = (false, false, false);
        egui::Window::new("Play from link")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if let Some(s) = &fetching {
                    ui.horizontal(|ui| {
                        spinner(ui);
                        ui.label(format!("Getting the video tools (one time, ~130 MB): {s}"));
                    });
                    if ui.button("Cancel").clicked() {
                        self.pending_link = None;
                        close = true;
                    }
                    return;
                }
                let Some(url) = self.link_dialog.as_mut() else { return };
                ui.label("Paste a link — YouTube, Twitch, Facebook, Vimeo… (live streams too).");
                ui.label(egui::RichText::new("The picture goes to the MarkCam camera, the sound to MarkCam Microphone.").small().color(theme::TEXT_DIM));
                let te = ui.add(egui::TextEdit::singleline(url).hint_text("https://www.youtube.com/watch?v=…").desired_width(420.0));
                te.request_focus();
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    play = true;
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Quality");
                    for q in link::QUALITIES {
                        ui.selectable_value(&mut self.link_quality, q, format!("{q}p"));
                    }
                });
                ui.label(
                    egui::RichText::new("360p starts fastest and is lightest on the laptop; 720p+ needs a good connection.")
                        .small()
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(egui::RichText::new("▶  Play").color(egui::Color32::WHITE)).fill(theme::ACCENT)).clicked() {
                        play = true;
                    }
                    if ui
                        .button("⬇  Download")
                        .on_hover_text(format!("Save as MP4 in {} — then play it any time, offline", link::downloads_dir().display()))
                        .clicked()
                    {
                        download = true;
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
            });
        if close {
            self.link_dialog = None;
            if fetching.is_some() {
                self.tools_fetch = None;
            }
            return;
        }
        if download {
            let url = self.link_dialog.clone().unwrap_or_default().trim().to_string();
            if !url.starts_with("http") {
                self.load_err = Some("that doesn't look like a link (it should start with https://)".into());
                return;
            }
            if !tools::ready() {
                // Get the tools first (the dialog shows progress), then the user hits Download again.
                self.tools_fetch = Some(tools::Fetch::start());
                return;
            }
            self.last_link = url.clone();
            self.link_dialog = None;
            self.video_dl = Some(link::Download::start(&url, self.link_quality));
            self.tab = Tab::Audio; // where the download's progress is shown
            return;
        }
        if play {
            let url = self.link_dialog.take().unwrap_or_default().trim().to_string();
            if !url.starts_with("http") {
                self.load_err = Some("that doesn't look like a link (it should start with https://)".into());
                self.link_dialog = Some(url);
                return;
            }
            self.last_link = url.clone();
            if tools::ready() {
                self.switch_to(Spec::Link(url, self.link_quality));
            } else {
                self.pending_link = Some(url);
                self.tools_fetch = Some(tools::Fetch::start());
            }
        }
        if fetching.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// The small modal for naming a new category or renaming one.
    fn show_cat_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = self.cat_dialog.as_mut() else { return };
        let (title, name) = match d {
            CatDialog::New { name, .. } => ("New category", name),
            CatDialog::Rename { name, .. } => ("Rename category", name),
        };
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let te = ui.add(egui::TextEdit::singleline(name).hint_text("Category name").desired_width(260.0));
                te.request_focus();
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    ok = true;
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(egui::RichText::new("OK").color(egui::Color32::WHITE)).fill(theme::ACCENT)).clicked() {
                        ok = true;
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    }
                });
            });
        if cancel {
            self.cat_dialog = None;
            return;
        }
        if !ok {
            return;
        }
        match self.cat_dialog.take() {
            Some(CatDialog::New { name, item }) => {
                if self.lib.create_category(&name) {
                    let name = name.trim().to_string();
                    if let Some(it) = item {
                        self.lib.move_item(&it, Some(&name));
                    }
                    self.category = Some(name);
                    self.query.clear();
                } else {
                    self.load_err = Some("that category name is empty or already exists".into());
                    self.cat_dialog = Some(CatDialog::New { name, item });
                }
            }
            Some(CatDialog::Rename { old, name }) => {
                if self.lib.rename_category(&old, &name) {
                    if self.category.as_deref() == Some(old.as_str()) {
                        self.category = Some(name.trim().to_string());
                    }
                } else {
                    self.load_err = Some("that category name is empty or already exists".into());
                    self.cat_dialog = Some(CatDialog::Rename { old, name });
                }
            }
            None => {}
        }
    }

    /// The current state as settings.
    fn snapshot(&self, fx: &Fx) -> settings::Settings {
        // The camera is remembered even while the screen / a video is on the picture, so
        // "Back to camera" (and the next start) knows which one.
        let (camera_index, camera_name) = match self.last_camera {
            Some(idx) => (Some(idx), self.cameras.iter().find(|c| c.index == idx).map(|c| c.name.clone())),
            None => (None, None),
        };
        let source = match self.current {
            Spec::Camera { .. } => "camera",
            Spec::Screen(_) => "screen",
            Spec::Media(_) => "media",
            Spec::Link(..) => "link",
            Spec::Test => "test",
        };
        let p = &fx.cg.params;
        settings::Settings {
            camera_name,
            camera_index,
            res: self.sel_res,
            fps: self.sel_fps,
            mirror: self.mirror,
            source: source.into(),
            screen_area: self.screen_area,
            media_path: self.media_path.clone(),
            cg_on: fx.cg_on,
            cg: [p.brightness, p.contrast, p.saturation, p.temperature],
            blur_on: fx.blur_on,
            blur_radius: fx.blur.params.radius,
            ck_on: fx.ck_on,
            ck_rgb: fx.ck.params.key_rgb,
            ck_similarity: fx.ck.params.similarity,
            ck_smoothness: fx.ck.params.smoothness,
            filter: fx.filter.id().into(),
            filter_amount: fx.filter_amount,
            vignette: fx.vignette_amount,
            warp: fx.warp.kind.id().into(),
            warp_center: [fx.warp.cx, fx.warp.cy],
            warp_radius: fx.warp.radius,
            warp_strength: fx.warp.strength,
            warp_follow_face: fx.warp_follow_face,
            anims: fx.anims.iter().map(|a| a.kind.id().to_string()).collect(),
            anim_amount: fx.anim_amount,
            audio_route: self.audio_route,
            audio_mic: self.audio_mic.clone(),
            mic_gain: audio::Controls::get(&self.audio_ctl.mic_gain),
            media_gain: audio::Controls::get(&self.audio_ctl.media_gain),
            hear_media: self.audio_ctl.monitor.load(Ordering::Relaxed),
            last_link: self.last_link.clone(),
            link_quality: self.link_quality,
            vcam_on: fx.vcam.on,
            layers: fx
                .overlays
                .iter()
                .filter_map(|o| {
                    Some(settings::SavedLayer {
                        source: o.source.clone()?,
                        pos: o.pos,
                        size: o.size,
                        scale: o.scale,
                        opacity: o.opacity,
                        on: o.on,
                        remove_green: o.remove_green,
                    })
                })
                .collect(),
            tab: self.tab.id().into(),
            category: self.category.clone(),
            background_preview_fps: self.bg_preview_fps,
            idle_camera: fx.idle_camera,
            download_dir: self.download_dir.clone(),
            background: fx.vbg.choice.id(),
            background_blur: fx.vbg.blur,
            background_quality: fx.vbg.quality,
            key_mode: fx.vbg.key_mode,
            key_colors: fx.vbg.keys.clone(),
            key_similarity: fx.vbg.key_sim,
            key_smoothness: fx.vbg.key_smooth,
            key_ai: fx.vbg.key_ai,
            use_manycam: library::use_manycam(),
            banners: self.banners.clone(),
            banner_sel: self.banner_sel,
        }
    }

    /// Write settings when something changed (checked twice a second; slider drags settle
    /// into one write).
    fn autosave(&mut self, fx: &Fx, force: bool) {
        if !force && self.save_check.elapsed() < std::time::Duration::from_millis(500) {
            return;
        }
        self.save_check = std::time::Instant::now();
        let now = self.snapshot(fx);
        if now != self.saved {
            if let Err(e) = settings::save(&now) {
                self.load_err = Some(format!("could not save settings: {e}"));
            }
            self.saved = now;
        }
    }

    /// Switch to a camera at the currently selected resolution/fps.
    fn open_camera(&mut self, idx: u32) -> Spec {
        Spec::Camera { idx, w: self.sel_res.0, h: self.sel_res.1, fps: self.sel_fps }
    }
}

/// A loading spinner that redraws 20×/s; egui's own asks for a repaint every frame, which
/// without vsync means hundreds of repaints per second while anything is loading.
fn spinner(ui: &mut egui::Ui) {
    let size = ui.style().spacing.interact_size.y;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    let time = ui.input(|i| i.time);
    let (start, sweep) = (time * std::f64::consts::TAU, 240f64.to_radians() * time.sin());
    let radius = rect.height() / 2.0 - 2.0;
    let points: Vec<egui::Pos2> = (0..20)
        .map(|i| {
            let (s, c) = (start + sweep * i as f64 / 20.0).sin_cos();
            rect.center() + radius * egui::vec2(c as f32, s as f32)
        })
        .collect();
    ui.painter().add(egui::Shape::line(points, egui::Stroke::new(3.0f32, ui.visuals().strong_text_color())));
}

/// Capture-thread smoothness for the Perf log, worst since the last line (µs): frame gap,
/// wait for the Fx lock, pipeline run; plus the count of gaps over 40 ms.
static JITTER: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// Where the UI thread's time goes (OVC_PERF=<file>: a line every 5 s) — for tuning on
/// slow machines.
struct Perf {
    path: PathBuf,
    since: std::time::Instant,
    calls: u32,
    frames: u32,
    update: f64,
    process: f64,
    vcam: f64,
    preview: f64,
}

impl Perf {
    fn from_env() -> Option<Self> {
        let path = PathBuf::from(std::env::var_os("OVC_PERF")?);
        Some(Perf { path, since: std::time::Instant::now(), calls: 0, frames: 0, update: 0.0, process: 0.0, vcam: 0.0, preview: 0.0 })
    }

    fn flush_if_due(&mut self, cam_fps: f32) {
        let dt = self.since.elapsed().as_secs_f64();
        if dt < 5.0 {
            return;
        }
        let ms = |t: f64| t * 1000.0 / dt; // ms per second
        let line = format!(
            "{:>8.1}s  repaints {:>5.1}/s  frames {:>5.1}/s  cam {:>4.1} fps | UI {:>5.1} ms/s | capture thread pipeline: processing {:>5.1}, vcam {:>4.1}, preview {:>4.1} ms/s{}\n",
            dt,
            self.calls as f64 / dt,
            self.frames as f64 / dt,
            cam_fps,
            ms(self.update),
            ms(self.process),
            ms(self.vcam),
            ms(self.preview),
            format!(
                " | worst: frame gap {:.0} ms ({} over 40 ms), waiting for the UI {:.1} ms, pipeline {:.1} ms",
                JITTER[0].swap(0, Ordering::Relaxed) as f64 / 1000.0,
                JITTER[3].swap(0, Ordering::Relaxed),
                JITTER[1].swap(0, Ordering::Relaxed) as f64 / 1000.0,
                JITTER[2].swap(0, Ordering::Relaxed) as f64 / 1000.0,
            ),
        );
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = std::io::Write::write_all(&mut f, line.as_bytes());
        }
        *self = Perf { path: std::mem::take(&mut self.path), ..Perf { path: PathBuf::new(), since: std::time::Instant::now(), calls: 0, frames: 0, update: 0.0, process: 0.0, vcam: 0.0, preview: 0.0 } };
    }
}

impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let t_update = std::time::Instant::now();
        self.update_inner(ctx, frame);
        let cam_fps = *self.cap.cam_fps.lock().unwrap();
        if let Some(p) = self.perf.as_mut() {
            // Pipeline timings come from the capture thread.
            let (frames, process, vcam, preview) = std::mem::take(&mut self.fx.lock().unwrap().perf);
            p.frames += frames;
            p.process += process;
            p.vcam += vcam;
            p.preview += preview;
            p.calls += 1;
            p.update += t_update.elapsed().as_secs_f64();
            if std::env::var_os("OVC_PERF_CAUSES").is_some() && p.calls % 30 == 0 {
                let causes: Vec<String> = ctx.repaint_causes().iter().map(|c| c.to_string()).collect();
                let line = format!("discard={} causes={causes:?}\n", ctx.will_discard());
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p.path) {
                    let _ = std::io::Write::write_all(&mut f, line.as_bytes());
                }
            }
            p.flush_if_due(cam_fps);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let fx = self.fx.clone();
        let fx = fx.lock().unwrap();
        self.autosave(&fx, true);
    }
}

impl Studio {
    fn update_inner(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Started with Windows: go straight to the taskbar (once the window is up; a
        // minimize sent with the very first frame is lost).
        if self.start_minimized && ctx.cumulative_pass_nr() > 2 {
            self.start_minimized = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        // F toggles freeze (unless typing in a text field).
        if !ctx.wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::F) && i.modifiers.is_none()) {
            self.set_frozen(!self.frozen);
        }
        // The pipeline (effects → virtual camera) runs on the capture thread; the UI edits
        // its settings and shows its preview under the same lock (released below before
        // anything slow, like file dialogs).
        let fx_arc = self.fx.clone();
        let mut fx_guard = fx_arc.lock().unwrap();
        let fx: &mut Fx = &mut fx_guard;
        // How often the capture thread hands over a preview: every frame while MarkCam is
        // in front, at the Setup choice behind another window (15 fps by default: 10 looked
        // choppy next to ClassIn, 30 doubled the app's CPU), never while minimized. The
        // virtual camera gets every frame regardless.
        let (minimized, focused) =
            ctx.input(|i| (i.viewport().minimized.unwrap_or(false), i.viewport().focused.unwrap_or(true)));
        fx.ui_every = if minimized {
            0
        } else if focused {
            1
        } else {
            match self.bg_preview_fps {
                0 => 0,
                fps => (30 / fps.clamp(1, 30)).max(1),
            }
        };
        if fx.mirror != self.mirror || fx.frozen != self.frozen || self.tex.is_none() {
            fx.preview_needed = true;
        }
        fx.mirror = self.mirror;
        fx.frozen = self.frozen;
        fx.preview_px = self.preview_px;
        fx.lms_src = self.tracker.as_ref().map(|t| t.landmarks.clone());
        // N shows / hides the name banner.
        if !ctx.wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::N) && i.modifiers.is_none()) {
            self.toggle_banner(fx, ctx);
        }
        self.banner_tick(fx, ctx);
        // Something may change the picture: re-run on the held frame if no new ones come
        // (paused video), so sliders still show their effect.
        if ctx.input(|i| !i.events.is_empty() || i.pointer.any_down()) {
            fx.dirty = true;
        }
        if let Some(image) = fx.preview.take() {
            match &mut self.tex {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => self.tex = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR)),
            }
        }
        let dt = self.last.elapsed().as_secs_f32();
        if dt >= 0.5 {
            self.fps = (fx.processed - self.last_processed) as f32 / dt;
            self.last = std::time::Instant::now();
            self.last_processed = fx.processed;
        }
        let cams = self.cameras.clone();
        let err = self.cap.err.lock().unwrap().clone();
        let status = self.cap.status.lock().unwrap().clone();
        let cam_fps = *self.cap.cam_fps.lock().unwrap();
        let modes = status.as_ref().map(|s| s.modes.clone()).unwrap_or_default();
        let mut pending: Option<Spec> = None;
        let have_face_models = self.face_models.is_some();
        let face_active = self.tracker.is_some();
        let dl_status = self.download.as_ref().map(|d| d.status.lock().unwrap().clone());
        let mut want_download = false;
        let mut vcam_action: Option<bool> = None; // Some(uninstall?)
        let mut add_items: Vec<library::Item> = Vec::new();
        let mut add_image = false;
        let mut do_import = false;
        let mut open_maker = false;
        let mut do_refresh = false;
        let mut locate_folder = false;
        let mut pick_dl_dir = false;
        let mut add_backgrounds = false;
        let mut cat_action: Option<CatAction> = None;
        let mut source_action: Option<SourceAction> = None;
        self.thumbs.pump(ctx);

        // What the open device supports. Only correct the selection once the device has
        // reported its modes — otherwise a restored setting would be clobbered at startup.
        let res_list = resolutions(&modes);
        let is_cam = matches!(self.current, Spec::Camera { .. });
        if is_cam && !modes.is_empty() && !res_list.contains(&self.sel_res) {
            self.sel_res = res_list[0];
        }
        let fps_list = fps_for(&modes, self.sel_res);
        if is_cam && !modes.is_empty() && !fps_list.contains(&self.sel_fps) {
            self.sel_fps = fps_list[0];
        }

        // ── Top bar: source, format, mirror · virtual-camera status ─────────────────────
        egui::TopBottomPanel::top("topbar")
            .exact_height(50.0)
            .frame(egui::Frame::none().fill(theme::BG).inner_margin(egui::Margin::symmetric(14.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    theme::logo(ui);
                    ui.label(egui::RichText::new("MarkCam").strong().size(17.0));
                    ui.add_space(18.0);

                    let cur_name = match &self.current {
                        Spec::Test => "Test pattern".to_string(),
                        Spec::Camera { idx, .. } => cams
                            .iter()
                            .find(|c| c.index == *idx)
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| format!("Camera {idx}")),
                        Spec::Screen(a) if Some(*a) == Some(screen::primary_screen()) => "Screen: full screen".to_string(),
                        Spec::Screen(a) => format!("Screen: area {}×{}", a.w, a.h),
                        Spec::Media(p) => format!(
                            "Video: {}",
                            p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                        ),
                        Spec::Link(u, q) => format!("Link {q}p: {}", short_link(u)),
                    };
                    egui::ComboBox::from_id_salt("source")
                        .width(230.0)
                        .selected_text(format!("📷  {cur_name}"))
                        .show_ui(ui, |ui| {
                            ui.label(egui::RichText::new("CAMERAS").small().color(theme::TEXT_DIM));
                            for c in &cams {
                                let on = matches!(self.current, Spec::Camera { idx, .. } if idx == c.index);
                                if ui.selectable_label(on, &c.name).clicked() {
                                    pending = Some(self.open_camera(c.index));
                                }
                            }
                            ui.separator();
                            ui.label(egui::RichText::new("SCREEN").small().color(theme::TEXT_DIM));
                            if ui.selectable_label(false, "Full screen").clicked() {
                                source_action = Some(SourceAction::FullScreen);
                            }
                            if ui
                                .selectable_label(false, "Select area…")
                                .on_hover_text("Drag a rectangle anywhere on the screen — e.g. a student's video in ClassIn")
                                .clicked()
                            {
                                source_action = Some(SourceAction::PickArea);
                            }
                            if let Some(a) = self.screen_area {
                                if ui.selectable_label(false, format!("Last area ({}×{})", a.w, a.h)).clicked() {
                                    source_action = Some(SourceAction::LastArea);
                                }
                            }
                            ui.separator();
                            ui.label(egui::RichText::new("VIDEO").small().color(theme::TEXT_DIM));
                            if ui.selectable_label(false, "Play a video file…").clicked() {
                                source_action = Some(SourceAction::PickVideo);
                            }
                            if ui
                                .selectable_label(false, "Play from link (YouTube…)…")
                                .on_hover_text("YouTube, Twitch, Facebook and many more — live streams too, with sound")
                                .clicked()
                            {
                                source_action = Some(SourceAction::LinkDialog);
                            }
                            ui.separator();
                            if ui.selectable_label(matches!(self.current, Spec::Test), "Test pattern").clicked() {
                                pending = Some(Spec::Test);
                            }
                        })
                        .response
                        .on_hover_text("What is on the picture: a camera, your screen, or a video");

                    // Not the camera: quick ways back / to re-pick, instead of format menus.
                    if !is_cam {
                        if matches!(self.current, Spec::Screen(_))
                            && ui.button("Change area").on_hover_text("Pick another part of the screen").clicked()
                        {
                            source_action = Some(SourceAction::PickArea);
                        }
                        if ui
                            .add(egui::Button::new(egui::RichText::new("Back to camera").color(egui::Color32::WHITE)).fill(theme::ACCENT))
                            .clicked()
                        {
                            source_action = Some(SourceAction::BackToCamera);
                        }
                    }

                    if is_cam {
                    egui::ComboBox::from_id_salt("res")
                        .width(104.0)
                        .selected_text(format!("{}×{}", self.sel_res.0, self.sel_res.1))
                        .show_ui(ui, |ui| {
                            for rc in &res_list {
                                if ui.selectable_label(self.sel_res == *rc, format!("{}×{}", rc.0, rc.1)).clicked() {
                                    self.sel_res = *rc;
                                    let fl = fps_for(&modes, *rc);
                                    if !fl.contains(&self.sel_fps) {
                                        // Prefer <=60 so we don't jump to an exotic rate.
                                        self.sel_fps = *fl.iter().find(|f| **f <= 60).unwrap_or(&fl[0]);
                                    }
                                    if let Spec::Camera { idx, .. } = self.current {
                                        pending = Some(self.open_camera(idx));
                                    }
                                }
                            }
                        })
                        .response
                        .on_hover_text("Capture resolution — 1280×720 is all ClassIn uses and saves battery");

                    egui::ComboBox::from_id_salt("fps")
                        .width(78.0)
                        .selected_text(format!("{} fps", self.sel_fps))
                        .show_ui(ui, |ui| {
                            for f in &fps_list {
                                if ui.selectable_label(self.sel_fps == *f, format!("{f} fps")).clicked() {
                                    self.sel_fps = *f;
                                    if let Spec::Camera { idx, .. } = self.current {
                                        pending = Some(self.open_camera(idx));
                                    }
                                }
                            }
                        });
                    }

                    if ui
                        .add(egui::Button::new("Mirror").selected(self.mirror))
                        .on_hover_text("Flip the preview like a mirror. Others always see you un-mirrored.")
                        .clicked()
                    {
                        self.mirror = !self.mirror;
                    }

                    let freeze_label = if self.frozen {
                        egui::RichText::new("❄ Frozen").color(egui::Color32::WHITE)
                    } else {
                        egui::RichText::new("❄ Freeze")
                    };
                    let mut freeze_btn = egui::Button::new(freeze_label).selected(self.frozen);
                    if self.frozen {
                        freeze_btn = freeze_btn.fill(theme::WARN);
                    }
                    if ui
                        .add(freeze_btn)
                        .on_hover_text(
                            "Hold the picture still (key F) — effects, GIFs and masks keep moving on it. \
                             Handy for a quick stretch during a lesson. Click again to go live.",
                        )
                        .clicked()
                    {
                        self.set_frozen(!self.frozen);
                    }

                    // Name banner: click shows / hides it, right-click opens its editor.
                    let shown = fx.banner.as_ref().is_some_and(|b| !b.is_leaving()) || self.banner_pending;
                    let resp = ui
                        .add(egui::Button::new("🏷 Name").selected(shown))
                        .on_hover_text("Show / hide your name banner (key N). Right-click to edit it.");
                    if resp.clicked() {
                        self.toggle_banner(fx, ctx);
                    }
                    if resp.secondary_clicked() {
                        self.tab = Tab::Banner;
                    }

                    // Right side: virtual camera.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.vcam_installing.is_some() {
                            theme::pill(ui, "Installing… confirm the Windows prompt", theme::WARN);
                        } else if self.vcam_state == vcam::InstallState::Missing {
                            if ui
                                .add(egui::Button::new(egui::RichText::new("Install virtual camera").color(egui::Color32::WHITE)).fill(theme::ACCENT))
                                .on_hover_text("One-time, needs admin: makes \"MarkCam\" appear as a camera in ClassIn / Zoom")
                                .clicked()
                            {
                                vcam_action = Some(false);
                            }
                        } else {
                            let resp = match (fx.vcam.on, fx.vcam.consumer()) {
                                (false, _) => theme::pill(ui, "MarkCam  off", theme::TEXT_DIM)
                                    .on_hover_text("Virtual camera is off — apps see a dark picture. Click to turn on."),
                                (true, Some((cw, ch))) => theme::pill(ui, &format!("●  LIVE  {cw}×{ch}"), theme::LIVE)
                                    .on_hover_text("An app (ClassIn…) is showing the MarkCam camera. Click to turn off."),
                                (true, None) => theme::pill(ui, "MarkCam  ready", theme::ACCENT)
                                    .on_hover_text("Pick \"MarkCam\" as the camera in ClassIn / Zoom. Click to turn off."),
                            };
                            if resp.clicked() {
                                fx.vcam.on = !fx.vcam.on;
                            }
                            // About / support, always in sight next to the camera status.
                            if ui
                                .add(egui::Button::new(egui::RichText::new("♥ About MarkCam · Support").color(egui::Color32::from_rgb(255, 170, 190))).fill(egui::Color32::from_rgb(58, 34, 46)))
                                .on_hover_text("Who made MarkCam, credits — and buy the author a coffee")
                                .clicked()
                            {
                                self.about_open = true;
                            }
                            if self.vcam_state == vcam::InstallState::Outdated
                                && ui
                                    .add(egui::Button::new(egui::RichText::new("Update camera").color(egui::Color32::WHITE)).fill(theme::ACCENT))
                                    .on_hover_text(
                                        "The installed virtual camera is an older copy (e.g. still named OpenVCam). \
                                         Click once (admin) to switch it to this MarkCam, then restart ClassIn.",
                                    )
                                    .clicked()
                            {
                                vcam_action = Some(false);
                            }
                            // The app takes a smaller picture than we capture: capturing at its
                            // size cuts decode + processing roughly in proportion.
                            if let (Some((cw, ch)), Spec::Camera { idx, .. }) = (fx.vcam.consumer(), &self.current) {
                                let idx = *idx;
                                if (cw as u64 * ch as u64) < (fx.w as u64 * fx.h as u64) && res_list.contains(&(cw, ch)) {
                                    if ui
                                        .add(egui::Button::new(egui::RichText::new(format!("Use {cw}×{ch}")).color(theme::WARN)))
                                        .on_hover_text(format!(
                                            "The app only uses {cw}×{ch}; capturing {}×{} wastes CPU and battery.",
                                            fx.w, fx.h
                                        ))
                                        .clicked()
                                    {
                                        self.sel_res = (cw, ch);
                                        pending = Some(self.open_camera(idx));
                                    }
                                }
                            }
                        }
                    });
                });
            });

        // ── Status bar ─────────────────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("status")
            .exact_height(26.0)
            .frame(egui::Frame::none().fill(theme::BG).inner_margin(egui::Margin::symmetric(14.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let neg = status
                        .as_ref()
                        .map(|s| format!("{}×{}", s.negotiated.width, s.negotiated.height))
                        .unwrap_or_else(|| "—".into());
                    let dim = |t: String| egui::RichText::new(t).small().color(theme::TEXT_DIM);
                    // The new source is still starting (a link takes a few seconds to resolve).
                    if status.is_none() && err.is_none() && !matches!(self.current, Spec::Test) {
                        spinner(ui);
                        let what = if matches!(self.current, Spec::Link(..)) { "Opening the link…" } else { "Opening…" };
                        ui.label(egui::RichText::new(what).small().color(theme::ACCENT));
                    }
                    if fx.camera_idle {
                        ui.label(dim("Camera resting — nothing is showing the picture".into()))
                            .on_hover_text("Turns back on by itself when an app (ClassIn…) uses MarkCam or this window is in front.");
                    } else {
                        ui.label(dim(format!("Camera {neg} · {cam_fps:.0} fps")));
                    }
                    ui.label(dim(format!("Preview {:.0} fps", self.fps)));
                    ui.label(dim(fx.gpu.adapter_info.to_string()));
                    let problem = err.clone().or_else(|| self.load_err.clone()).or_else(|| fx.vcam.err.clone());
                    if let Some(e) = problem {
                        ui.label(egui::RichText::new(format!("⚠ {e}")).small().color(theme::ERROR));
                    }
                });
            });

        // ── Right panel: Effects · Layers · Adjust · Setup ──────────────────────────────
        egui::SidePanel::right("side")
            .resizable(true)
            .default_width(392.0)
            .width_range(300.0..=600.0)
            .frame(egui::Frame::none().fill(theme::PANEL).inner_margin(egui::Margin::same(12.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let n = fx.overlays.len();
                    for (tab, label) in [
                        (Tab::Effects, "Effects".to_string()),
                        (Tab::Background, "Background".to_string()),
                        (Tab::Layers, if n > 0 { format!("Layers ({n})") } else { "Layers".into() }),
                        (Tab::Adjust, "Adjust".to_string()),
                        (Tab::Audio, "Audio".to_string()),
                        (Tab::Setup, "Setup".to_string()),
                    ] {
                        ui.selectable_value(&mut self.tab, tab, label);
                    }
                });
                ui.separator();

                match self.tab {
                    Tab::Banner => self.banner_panel(ui, ctx, fx),
                    Tab::Effects => {
                        // Buttons first (right-aligned), the search box takes what is left —
                        // inside a one-row strip so the layout doesn't claim the whole panel.
                        let row = egui::vec2(ui.available_width(), 30.0);
                        ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Import").on_hover_text(if PUBLIC { "Import a shared MarkCam effect (.markcam)" } else { "Import a MarkCam .markcam or a ManyCam .mcv / .mcep file" }).clicked() {
                                do_import = true;
                            }
                            if ui.button("+").on_hover_text("Add your own images or animations (PNG, JPG, GIF, WebP, APNG) — or drag files onto the window").clicked() {
                                add_image = true;
                            }
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.query)
                                    .hint_text("🔍  Search all effects…")
                                    .desired_width(ui.available_width()),
                            );
                            if resp.changed() && !self.query.is_empty() {
                                self.category = None;
                            }
                        });
                        ui.add_space(4.0);

                        let query = self.query.trim().to_string();
                        let shown: Option<(String, Vec<library::Item>)> = if !query.is_empty() {
                            let r = self.lib.search(&query);
                            Some((format!("{} results for “{query}”", r.len()), r))
                        } else {
                            self.category.clone().map(|c| (c.clone(), self.lib.in_category(&c)))
                        };

                        match shown {
                            Some((title, items)) => {
                                ui.horizontal(|ui| {
                                    if self.category.is_some() && query.is_empty() && ui.small_button("‹ All").clicked() {
                                        self.category = None;
                                    }
                                    if !query.is_empty() && ui.small_button("× Clear").clicked() {
                                        self.query.clear();
                                    }
                                    ui.label(egui::RichText::new(&title).strong());
                                    // Any category (except My effects) can be renamed / deleted here.
                                    if let Some(c) = self.category.clone().filter(|c| query.is_empty() && c != library::MY_EFFECTS) {
                                        let hint = if self.lib.is_user_category(&c) {
                                            "Delete this category — its effects go back to their own categories"
                                        } else {
                                            "Remove this category and its effects from MarkCam (restorable)"
                                        };
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.small_button("Delete").on_hover_text(hint).clicked() {
                                                cat_action = Some(CatAction::Delete(c.clone()));
                                            }
                                            if ui.small_button("Rename").clicked() {
                                                cat_action = Some(CatAction::Rename(c.clone()));
                                            }
                                        });
                                    }
                                });
                                if !items.is_empty() {
                                    ui.label(
                                        egui::RichText::new("Right-click an effect to move or delete it.")
                                            .small()
                                            .color(theme::TEXT_DIM),
                                    );
                                }
                                // Where effects can be moved: your categories first, then the rest.
                                let mut move_targets: Vec<String> = self.lib.user_categories().to_vec();
                                let others: Vec<String> = self.lib.categories().iter().map(|(c, _)| c.clone()).collect();
                                move_targets.extend(others.into_iter().filter(|c| !self.lib.is_user_category(c)));
                                ui.add_space(4.0);
                                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                                    let gap = 8.0;
                                    let avail = ui.available_width();
                                    let cols = ((avail + gap) / (104.0 + gap)).floor().max(2.0) as usize;
                                    let tile = (avail - gap * (cols as f32 - 1.0)) / cols as f32;
                                    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                                    for row in items.chunks(cols) {
                                        ui.horizontal(|ui| {
                                            for it in row {
                                                let ok = match it.kind() {
                                                    ovc_mcam::Kind::Object | ovc_mcam::Kind::Animation => true,
                                                    ovc_mcam::Kind::FaceMask => have_face_models,
                                                    ovc_mcam::Kind::Other | ovc_mcam::Kind::Locked => false,
                                                };
                                                let moved = self.lib.is_moved(it);
                                                let here = self.lib.category_of(it).to_string();
                                                let resp = effect_tile(ui, &mut self.thumbs, it, tile, ok);
                                                if resp.clicked() {
                                                    if ok {
                                                        add_items.push(it.clone());
                                                    } else if it.kind() == ovc_mcam::Kind::FaceMask {
                                                        self.load_err = Some("face masks need the face models — see Setup".into());
                                                    }
                                                }
                                                resp.context_menu(|ui| {
                                                    ui.label(egui::RichText::new(it.name()).strong());
                                                    ui.menu_button("Move to category", |ui| {
                                                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                                            for c in move_targets.iter().filter(|c| **c != here) {
                                                                if ui.button(c).clicked() {
                                                                    cat_action = Some(CatAction::Move(it.clone(), Some(c.clone())));
                                                                    ui.close_menu();
                                                                }
                                                            }
                                                        });
                                                    });
                                                    if ui.button("New category with this effect…").clicked() {
                                                        cat_action = Some(CatAction::New(Some(it.clone())));
                                                        ui.close_menu();
                                                    }
                                                    if moved && ui.button(format!("Back to “{}”", it.category)).clicked() {
                                                        cat_action = Some(CatAction::Move(it.clone(), None));
                                                        ui.close_menu();
                                                    }
                                                    ui.separator();
                                                    let own = it.category == library::MY_EFFECTS;
                                                    let del = ui.button(egui::RichText::new("🗑  Delete effect").color(theme::ERROR)).on_hover_text(if own {
                                                        "Your file goes to the Recycle Bin"
                                                    } else {
                                                        "Removed from MarkCam (restore any time: “Restore deleted effects” at the bottom of the category list)"
                                                    });
                                                    if del.clicked() {
                                                        cat_action = Some(CatAction::DeleteItem(it.clone()));
                                                        ui.close_menu();
                                                    }
                                                });
                                            }
                                        });
                                    }
                                    if items.is_empty() {
                                        ui.label(egui::RichText::new("Nothing found.").color(theme::TEXT_DIM));
                                    }
                                });
                            }
                            None => {
                                if self.lib.scanning() {
                                    ui.horizontal(|ui| {
                                        spinner(ui);
                                        ui.label(egui::RichText::new("Indexing effects…").color(theme::TEXT_DIM));
                                    });
                                }
                                let cats: Vec<(String, usize)> = self.lib.categories().to_vec();
                                let own_cats = self.lib.own_categories();
                                ui.add_space(2.0);
                                ui.columns(2, |c| {
                                    let big = |t: &str| egui::Button::new(egui::RichText::new(t).strong().size(14.5)).min_size(egui::vec2(0.0, 38.0)).fill(theme::ACCENT_DIM);
                                    if c[0].add_sized([c[0].available_width(), 38.0], big("🎨 Make your own effect")).on_hover_text("Effect Maker: build a face effect from emoji, pictures or photos, animate it, save it").clicked() {
                                        open_maker = true;
                                    }
                                    if c[1].add_sized([c[1].available_width(), 38.0], big("🏷 Name banner")).on_hover_text("Your name at the bottom of the picture, animated (key N)").clicked() {
                                        self.tab = Tab::Banner;
                                    }
                                });
                                theme::section(ui, "MarkCam effects");
                                ui.scope(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    if theme::list_row(ui, "Weather (Rain, Snow, Storm, Sun…)", AnimKind::ALL.iter().filter(|k| k.is_weather()).count()).clicked() {
                                        self.tab = Tab::Adjust;
                                    }
                                    if theme::list_row(ui, "Animated (Fire, Matrix, Stars…)", AnimKind::ALL.iter().filter(|k| !k.is_weather()).count()).clicked() {
                                        self.tab = Tab::Adjust;
                                    }
                                    if theme::list_row(ui, "Distortions", WarpKind::ALL.len() - 1).clicked() {
                                        self.tab = Tab::Adjust;
                                    }
                                    if theme::list_row(ui, "Color filters", Filter::ALL.len() - 1).clicked() {
                                        self.tab = Tab::Adjust;
                                    }
                                    let mine = cats.iter().find(|(n, _)| n == library::MY_EFFECTS).map_or(0, |c| c.1);
                                    if theme::list_row(ui, library::MY_EFFECTS, mine)
                                        .on_hover_text("Your own images and GIF animations. Add with + or drag files onto the window.")
                                        .clicked()
                                    {
                                        self.category = Some(library::MY_EFFECTS.into());
                                    }
                                    // MarkCam's own packs (hats, hair, glasses, stickers…).
                                    for name in &own_cats {
                                        let count = cats.iter().find(|(c, _)| c == name).map_or(0, |c| c.1);
                                        if count > 0 && theme::list_row(ui, name, count).clicked() {
                                            self.category = Some(name.clone());
                                        }
                                    }
                                });
                                ui.add_space(6.0);
                                // Your own categories (right-click to rename / delete).
                                ui.horizontal(|ui| {
                                    theme::section(ui, "My categories");
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.small_button("+ New category").clicked() {
                                            cat_action = Some(CatAction::New(None));
                                        }
                                    });
                                });
                                let user_cats: Vec<String> = self.lib.user_categories().to_vec();
                                ui.scope(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    for name in &user_cats {
                                        let count = cats.iter().find(|(c, _)| c == name).map_or(0, |c| c.1);
                                        let resp = theme::list_row(ui, name, count);
                                        if resp.clicked() {
                                            self.category = Some(name.clone());
                                        }
                                        resp.context_menu(|ui| {
                                            if ui.button("Rename…").clicked() {
                                                cat_action = Some(CatAction::Rename(name.clone()));
                                                ui.close_menu();
                                            }
                                            if ui.button("Delete (effects go back)").clicked() {
                                                cat_action = Some(CatAction::Delete(name.clone()));
                                                ui.close_menu();
                                            }
                                        });
                                    }
                                    if user_cats.is_empty() {
                                        ui.label(
                                            egui::RichText::new("Make your own categories, then right-click effects to move them in.")
                                                .small()
                                                .color(theme::TEXT_DIM),
                                        );
                                    }
                                });
                                ui.add_space(6.0);
                                // Everything else: ManyCam's (when turned on in Setup) and imports.
                                let others: Vec<&(String, usize)> = cats
                                    .iter()
                                    .filter(|(n, c)| n != library::MY_EFFECTS && !user_cats.contains(n) && !own_cats.contains(n) && *c > 0)
                                    .collect();
                                if !others.is_empty() || library::use_manycam() {
                                    let n: usize = others.iter().map(|c| c.1).sum();
                                    let title = if library::use_manycam() { "ManyCam categories" } else { "Imported" };
                                    theme::section(ui, &format!("{title} · {n} effects"));
                                }
                                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    for (name, count) in others.iter().map(|c| (&c.0, &c.1)) {
                                        let resp = theme::list_row(ui, name, *count).on_hover_text("Right-click to rename or delete");
                                        if resp.clicked() {
                                            self.category = Some(name.clone());
                                        }
                                        resp.context_menu(|ui| {
                                            if ui.button("Rename…").clicked() {
                                                cat_action = Some(CatAction::Rename(name.clone()));
                                                ui.close_menu();
                                            }
                                            if ui.button(egui::RichText::new("🗑  Delete category").color(theme::ERROR)).clicked() {
                                                cat_action = Some(CatAction::Delete(name.clone()));
                                                ui.close_menu();
                                            }
                                        });
                                    }
                                    let deleted = self.lib.deleted_count();
                                    if deleted > 0 {
                                        ui.add_space(8.0);
                                        if ui
                                            .small_button(format!("↺ Restore deleted effects ({deleted})"))
                                            .on_hover_text("Bring back ManyCam effects / categories you deleted, and their original names")
                                            .clicked()
                                        {
                                            cat_action = Some(CatAction::Restore);
                                        }
                                    }
                                });
                            }
                        }
                    }

                    Tab::Background => {
                        if self.bg_files.is_none() {
                            self.bg_files = Some(vbg::files());
                        }
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            theme::section(ui, "Virtual background");
                            theme::card(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(
                                        "MarkCam finds you in the picture with an AI matting model and replaces \
                                         everything behind you. Hair, ears, headsets and the gaps between them keep \
                                         their real outline.",
                                    )
                                    .small()
                                    .color(theme::TEXT_DIM),
                                );
                                ui.horizontal(|ui| {
                                    ui.label("Find me by");
                                    ui.selectable_value(&mut fx.vbg.key_mode, false, "AI")
                                        .on_hover_text("Works with any room behind you.");
                                    ui.selectable_value(&mut fx.vbg.key_mode, true, "Color key")
                                        .on_hover_text("For a green screen or a plain, evenly lit wall: everything that color is replaced. The sharpest edges, no delay, almost no CPU.");
                                });
                                if fx.vbg.key_mode {
                                    // The picked colors, newest last.
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing.x = 3.0;
                                        for &[r, g, b] in &fx.vbg.keys {
                                            let (sw, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                                            ui.painter().rect_filled(sw, 3.0, egui::Color32::from_rgb(r, g, b));
                                            ui.painter().rect_stroke(sw, 3.0, egui::Stroke::new(1.0f32, theme::TEXT_DIM));
                                        }
                                        if fx.vbg.keys.is_empty() {
                                            ui.label(egui::RichText::new("No colors picked yet").small().color(theme::TEXT_DIM));
                                        }
                                    });
                                    ui.horizontal_wrapped(|ui| {
                                        let pick = if self.picking_key { "✔ Done picking" } else { "💧 Pick colors" };
                                        if ui
                                            .selectable_label(self.picking_key, pick)
                                            .on_hover_text("Then click on the wall in the picture — as many times as you like: every click adds that shade (light, shadow, a fold of the screen…).")
                                            .clicked()
                                        {
                                            self.picking_key = !self.picking_key;
                                        }
                                        if ui.add_enabled(!fx.vbg.keys.is_empty(), egui::Button::new("⟲ Undo")).on_hover_text("Remove the last picked color.").clicked() {
                                            fx.vbg.keys.pop();
                                        }
                                        if ui.add_enabled(!fx.vbg.keys.is_empty(), egui::Button::new("Clear")).clicked() {
                                            fx.vbg.keys.clear();
                                        }
                                        if ui.button("Auto").on_hover_text("Start over with the most common color along the top and sides of the picture.").clicked() {
                                            if !fx.cur_rgba.is_empty() {
                                                fx.vbg.keys = vec![vbg::auto_key(&fx.cur_rgba, fx.w as usize, fx.h as usize)];
                                            }
                                        }
                                    });
                                    if self.picking_key {
                                        ui.label(egui::RichText::new("Click on the wall in the picture. Each click adds a shade.").small().strong().color(theme::ACCENT));
                                    }
                                    ui.add(egui::Slider::new(&mut fx.vbg.key_sim, 0.0..=0.45).text("Similarity").show_value(false))
                                        .on_hover_text("How close to the picked color still counts as background. Raise it until the wall is gone.");
                                    ui.add(egui::Slider::new(&mut fx.vbg.key_smooth, 0.0..=0.3).text("Edge softness").show_value(false))
                                        .on_hover_text("Width of the soft edge around you.");
                                    ui.checkbox(&mut fx.vbg.key_ai, "AI helps")
                                        .on_hover_text("Keeps you solid even where your clothes match the wall, and removes everything outside you (past the edge of the screen). Uses the graphics chip.");
                                }
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(fx.vbg.choice == vbg::Choice::Off, "None").clicked() {
                                        fx.vbg.choice = vbg::Choice::Off;
                                    }
                                    if ui.selectable_label(fx.vbg.choice == vbg::Choice::Blur, "Blur").clicked() {
                                        fx.vbg.choice = vbg::Choice::Blur;
                                    }
                                });
                                if fx.vbg.choice == vbg::Choice::Blur {
                                    ui.add(egui::Slider::new(&mut fx.vbg.blur, 0.0..=1.0).text("Strength").show_value(false));
                                }
                                ui.horizontal(|ui| {
                                    ui.label("Edge quality");
                                    for (q, name, tip) in [
                                        (vbg::QUALITY_AUTO, "Auto", "Best when the graphics chip keeps up with the camera, else Light."),
                                        (vbg::QUALITY_BEST, "Best", "The larger model: cleaner fingers and hands in motion. Needs a strong graphics chip."),
                                        (vbg::QUALITY_LIGHT, "Light", "The small model: for laptops and longer battery life."),
                                    ] {
                                        ui.selectable_value(&mut fx.vbg.quality, q, name).on_hover_text(tip);
                                    }
                                });
                                if let (true, Some(sw)) = (fx.vbg.active(), &self.seg) {
                                    let st = sw.status.lock().unwrap().clone();
                                    ui.label(egui::RichText::new(format!("Person detection {st}")).small().color(theme::TEXT_DIM));
                                }
                                if let Some(e) = fx.vbg.err.take() {
                                    self.load_err = Some(e);
                                }
                            });
                            ui.add_space(6.0);
                            let tile = egui::vec2(108.0, 61.0);
                            let selected_stroke = egui::Stroke::new(2.5f32, theme::ACCENT);
                            theme::section(ui, "MarkCam");
                            ui.horizontal_wrapped(|ui| {
                                for (i, name) in vbg::BUILTIN.iter().enumerate() {
                                    let tex = self.bg_thumbs.entry(i).or_insert_with(|| {
                                        let img = vbg::builtin(i, 160, 90);
                                        ctx.load_texture(format!("bg:{i}"), egui::ColorImage::from_rgba_unmultiplied([160, 90], &img), egui::TextureOptions::LINEAR)
                                    });
                                    let sel = fx.vbg.choice == vbg::Choice::Builtin(i);
                                    let r = ui
                                        .add(egui::ImageButton::new(egui::Image::new(&*tex).fit_to_exact_size(tile).rounding(6.0)).selected(sel))
                                        .on_hover_text(*name);
                                    if sel {
                                        ui.painter().rect_stroke(r.rect, 6.0, selected_stroke);
                                    }
                                    if r.clicked() {
                                        fx.vbg.choice = vbg::Choice::Builtin(i);
                                    }
                                }
                            });
                            let files = self.bg_files.clone().unwrap_or_default();
                            let mut groups: Vec<&str> = vec!["My backgrounds"];
                            for (g, _) in &files {
                                if !groups.contains(g) {
                                    groups.push(g);
                                }
                            }
                            for g in groups {
                                ui.add_space(6.0);
                                theme::section(ui, g);
                                ui.horizontal_wrapped(|ui| {
                                    for (_, p) in files.iter().filter(|(gg, _)| *gg == g) {
                                        let key = format!("bgfile:{}", p.display());
                                        let src = if video::is_video_file(p) {
                                            settings::LayerSource::Video(p.clone())
                                        } else {
                                            settings::LayerSource::Image(p.clone())
                                        };
                                        let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                                        let sel = fx.vbg.choice == vbg::Choice::File(p.clone());
                                        let r = match self.thumbs.get_for(&key, || library::effect_ref_of(&src)) {
                                            Some(tex) => ui.add(
                                                egui::ImageButton::new(egui::Image::new(tex).fit_to_exact_size(tile).rounding(6.0)).selected(sel),
                                            ),
                                            None => ui.add_sized(tile + egui::vec2(8.0, 8.0), egui::Button::new(egui::RichText::new(&name).small())),
                                        };
                                        let hover = if video::is_video_file(p) { format!("{name} (video)") } else { name.clone() };
                                        let r = r.on_hover_text(hover);
                                        if sel {
                                            ui.painter().rect_stroke(r.rect, 6.0, selected_stroke);
                                        }
                                        if r.clicked() {
                                            fx.vbg.choice = vbg::Choice::File(p.clone());
                                        }
                                        if g == "My backgrounds" {
                                            r.context_menu(|ui| {
                                                if ui.button("🗑 Delete background").clicked() {
                                                    if sel {
                                                        fx.vbg.choice = vbg::Choice::Off;
                                                    }
                                                    let _ = library::to_recycle_bin(p);
                                                    self.bg_files = None;
                                                    ui.close_menu();
                                                }
                                            });
                                        }
                                    }
                                    if g == "My backgrounds"
                                        && ui
                                            .add_sized(tile + egui::vec2(8.0, 8.0), egui::Button::new("+ Add your own…"))
                                            .on_hover_text("Pictures (JPG, PNG) or videos (MP4…). They are copied into MarkCam.")
                                            .clicked()
                                    {
                                        add_backgrounds = true;
                                    }
                                });
                            }
                        });
                    }
                    Tab::Layers => {
                        if fx.overlays.is_empty() {
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("No effects on the picture yet.").color(theme::TEXT_DIM));
                            if ui.button("Browse effects").clicked() {
                                self.tab = Tab::Effects;
                            }
                        } else {
                            ui.label(
                                egui::RichText::new("On the preview: click to select, drag to move, scroll or corner handle to resize, Delete to remove")
                                    .small()
                                    .color(theme::TEXT_DIM),
                            );
                            ui.add_space(4.0);
                        }
                        let sel = self.selected;
                        let mut select: Option<usize> = None;
                        let mut remove: Option<usize> = None;
                        let mut center: Option<usize> = None;
                        let mut clear_all = false;
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            for (i, ov) in fx.overlays.iter_mut().enumerate() {
                                let is_sel = sel == Some(i);
                                let fill = if is_sel { theme::CARD_HOVER } else { theme::CARD };
                                egui::Frame::none()
                                    .fill(fill)
                                    .rounding(8.0)
                                    .stroke(if is_sel { egui::Stroke::new(1.0_f32, theme::ACCENT) } else { egui::Stroke::NONE })
                                    .inner_margin(egui::Margin::same(8.0))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.add(egui::Checkbox::without_text(&mut ov.on)).on_hover_text("Show / hide");
                                            let src = ov.source.clone();
                                            let thumb = ov.thumb_key.as_deref().and_then(|k| {
                                                self.thumbs.get_for(k, || src.as_ref().and_then(library::effect_ref_of))
                                            });
                                            if let Some(t) = thumb {
                                                ui.add(egui::Image::new((t.id(), egui::vec2(36.0, 27.0))).rounding(4.0));
                                            }
                                            let name = egui::RichText::new(&ov.name).color(if ov.on { theme::TEXT } else { theme::TEXT_DIM });
                                            if ui.add(egui::Label::new(name).truncate().sense(egui::Sense::click())).clicked() {
                                                select = Some(i);
                                            }
                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if ui.small_button("×").on_hover_text("Remove").clicked() {
                                                    remove = Some(i);
                                                }
                                            });
                                        });
                                        if is_sel {
                                            ui.add(egui::Slider::new(&mut ov.scale, 0.05..=4.0).text("size"));
                                            ui.add(egui::Slider::new(&mut ov.opacity, 0.0..=1.0).text("opacity"));
                                            if ui
                                                .checkbox(&mut ov.remove_green, "Remove green background")
                                                .on_hover_text("For green-screen videos and images")
                                                .changed()
                                            {
                                                ov.cache_key = (usize::MAX, 0, 0);
                                                ov.scaled_size = (0, 0);
                                            }
                                            if ui.small_button("Center").clicked() {
                                                center = Some(i);
                                            }
                                        }
                                    });
                                ui.add_space(2.0);
                            }
                            if !fx.overlays.is_empty() {
                                ui.add_space(6.0);
                                if ui.button("Remove all").clicked() {
                                    clear_all = true;
                                }
                            }
                        });
                        if let Some(i) = select {
                            self.selected = Some(i);
                        }
                        if let Some(i) = center {
                            if let Some(ov) = fx.overlays.get_mut(i) {
                                ov.pos = [0.5 - ov.size[0] * ov.scale / 2.0, 0.5 - ov.size[1] * ov.scale / 2.0];
                            }
                        }
                        if let Some(i) = remove {
                            fx.overlays.remove(i);
                            self.selected = None;
                        }
                        if clear_all {
                            fx.overlays.clear();
                            self.selected = None;
                        }
                    }

                    Tab::Adjust => {
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            for (title, weather) in [("Weather", true), ("Animated", false)] {
                                theme::section(ui, title);
                                theme::card(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        for k in AnimKind::ALL.into_iter().filter(|k| k.is_weather() == weather) {
                                            let on = fx.anims.iter().any(|a| a.kind == k);
                                            if ui.selectable_label(on, k.name()).on_hover_text("Click to turn on / off (several can run together)").clicked() {
                                                if on {
                                                    fx.anims.retain(|a| a.kind != k);
                                                } else {
                                                    fx.anims.push(ovc_effects::anim::Anim::new(k));
                                                }
                                            }
                                        }
                                    });
                                });
                                ui.add_space(6.0);
                            }
                            if !fx.anims.is_empty() {
                                theme::card(ui, |ui| {
                                    ui.add(egui::Slider::new(&mut fx.anim_amount, 0.0..=1.0).text("Intensity"));
                                });
                                ui.add_space(6.0);
                            }
                            theme::section(ui, "Distortion");
                            theme::card(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    for k in WarpKind::ALL {
                                        if ui.selectable_label(fx.warp.kind == k, k.name()).clicked() {
                                            fx.warp.kind = k;
                                        }
                                    }
                                });
                                if fx.warp.kind != WarpKind::None {
                                    ui.add_space(4.0);
                                    if !matches!(
                                        fx.warp.kind,
                                        WarpKind::MirrorLeft | WarpKind::MirrorRight | WarpKind::MirrorTop | WarpKind::Quad
                                    ) {
                                        ui.add(egui::Slider::new(&mut fx.warp.strength, 0.0..=1.0).text("Strength"));
                                    }
                                    if fx.warp.kind.has_center() {
                                        ui.add(egui::Slider::new(&mut fx.warp.radius, 0.08..=0.8).text("Size"));
                                        if have_face_models {
                                            ui.checkbox(&mut fx.warp_follow_face, "Follow my face");
                                        } else {
                                            fx.warp_follow_face = false;
                                            ui.label(
                                                egui::RichText::new("Download the face models (Setup) to make it follow your face.")
                                                    .small()
                                                    .color(theme::TEXT_DIM),
                                            );
                                        }
                                        if !fx.warp_follow_face {
                                            ui.label(
                                                egui::RichText::new("Drag on the preview to move the center.")
                                                    .small()
                                                    .color(theme::TEXT_DIM),
                                            );
                                        }
                                    }
                                }
                            });
                            ui.add_space(6.0);
                            theme::section(ui, "Filter");
                            theme::card(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    for f in Filter::ALL {
                                        if ui.selectable_label(fx.filter == f, f.name()).clicked() {
                                            fx.filter = f;
                                        }
                                    }
                                });
                                if fx.filter != Filter::None {
                                    ui.add(egui::Slider::new(&mut fx.filter_amount, 0.0..=1.0).text("Intensity"));
                                }
                                let mut vig = fx.vignette_amount > 0.0;
                                if ui.checkbox(&mut vig, "Vignette (dark corners)").changed() {
                                    fx.vignette_amount = if vig { 0.5 } else { 0.0 };
                                }
                                if vig {
                                    ui.add(egui::Slider::new(&mut fx.vignette_amount, 0.05..=1.0).text("Amount"));
                                }
                            });
                            ui.add_space(6.0);
                            theme::section(ui, "Color");
                            theme::card(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut fx.cg_on, "Color adjustments");
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.small_button("Reset").clicked() {
                                            fx.cg = ColorGrade::default();
                                        }
                                    });
                                });
                                ui.add_enabled_ui(fx.cg_on, |ui| {
                                    ui.add(egui::Slider::new(&mut fx.cg.params.brightness, -0.5..=0.5).text("Brightness"));
                                    ui.add(egui::Slider::new(&mut fx.cg.params.contrast, 0.0..=2.0).text("Contrast"));
                                    ui.add(egui::Slider::new(&mut fx.cg.params.saturation, 0.0..=2.0).text("Saturation"));
                                    ui.add(egui::Slider::new(&mut fx.cg.params.temperature, -1.0..=1.0).text("Warmth"));
                                });
                            });
                            ui.add_space(6.0);
                            theme::section(ui, "Background");
                            theme::card(ui, |ui| {
                                ui.checkbox(&mut fx.blur_on, "Blur (whole picture)");
                                if fx.blur_on {
                                    ui.add(egui::Slider::new(&mut fx.blur.params.radius, 0.0..=16.0).text("Strength"));
                                }
                                ui.label(egui::RichText::new("Uses the GPU while on.").small().color(theme::TEXT_DIM));
                            });
                            ui.add_space(6.0);
                            theme::card(ui, |ui| {
                                ui.checkbox(&mut fx.ck_on, "Green screen (chroma key to black)");
                                if fx.ck_on {
                                    ui.horizontal(|ui| {
                                        ui.label("Key color");
                                        ui.color_edit_button_rgb(&mut fx.ck.params.key_rgb);
                                    });
                                    ui.add(egui::Slider::new(&mut fx.ck.params.similarity, 0.0..=1.0).text("Similarity"));
                                    ui.add(egui::Slider::new(&mut fx.ck.params.smoothness, 0.0..=0.5).text("Smoothness"));
                                }
                            });
                        });
                    }

                    Tab::Audio => {
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            theme::section(ui, "MarkCam Microphone");
                            theme::card(ui, |ui| {
                                if let Some(s) = &self.audio_setup {
                                    ui.horizontal(|ui| {
                                        spinner(ui);
                                        ui.label(s.status.lock().unwrap().clone());
                                    });
                                } else {
                                    match self.cable {
                                        None => {
                                            ui.label("A microphone named \"MarkCam Microphone\" for ClassIn: it carries your voice plus the sound of videos you play.");
                                            ui.label(
                                                egui::RichText::new(
                                                    "One-time install (admin): MarkCam sets up the free VB-CABLE virtual audio cable and names it MarkCam Microphone.",
                                                )
                                                .small()
                                                .color(theme::TEXT_DIM),
                                            );
                                            if ui
                                                .add(egui::Button::new(egui::RichText::new("Install MarkCam Microphone").color(egui::Color32::WHITE)).fill(theme::ACCENT))
                                                .clicked()
                                            {
                                                self.audio_setup = Some(audio::Setup::start(false));
                                            }
                                        }
                                        Some(false) => {
                                            ui.label("The virtual cable is installed but not named yet.");
                                            if ui.button("Name it \"MarkCam Microphone\" (admin)").clicked() {
                                                self.audio_setup = Some(audio::Setup::start(true));
                                            }
                                        }
                                        Some(true) => {
                                            ui.label(egui::RichText::new("✔ Installed").color(theme::LIVE));
                                            ui.label(
                                                egui::RichText::new("In ClassIn: microphone → \"MarkCam Microphone\". Switch back to your own microphone when MarkCam is closed.")
                                                    .small()
                                                    .color(theme::TEXT_DIM),
                                            );
                                        }
                                    }
                                }
                            });
                            ui.add_space(6.0);
                            theme::section(ui, "Mixer");
                            theme::card(ui, |ui| {
                                ui.add_enabled_ui(self.cable.is_some(), |ui| {
                                    ui.checkbox(&mut self.audio_route, "Send to MarkCam Microphone");
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Microphone");
                                    let cur = self
                                        .audio_mic
                                        .as_ref()
                                        .and_then(|id| self.mics.iter().find(|m| &m.id == id))
                                        .map(|m| m.name.clone())
                                        .unwrap_or_else(|| "Windows default".into());
                                    egui::ComboBox::from_id_salt("mic").width(200.0).selected_text(cur).show_ui(ui, |ui| {
                                        if ui.selectable_label(self.audio_mic.is_none(), "Windows default").clicked() {
                                            self.audio_mic = None;
                                        }
                                        for m in &self.mics {
                                            if ui.selectable_label(self.audio_mic.as_ref() == Some(&m.id), &m.name).clicked() {
                                                self.audio_mic = Some(m.id.clone());
                                            }
                                        }
                                    });
                                });
                                let ctl = self.audio_ctl.clone();
                                let mut mic_on = ctl.mic_on.load(Ordering::Relaxed);
                                let mut mic_g = audio::Controls::get(&ctl.mic_gain);
                                let mut media_g = audio::Controls::get(&ctl.media_gain);
                                let mut hear = ctl.monitor.load(Ordering::Relaxed);
                                let meter = |ui: &mut egui::Ui, level: f32| {
                                    let l = level.clamp(0.0, 1.0);
                                    let color = if l > 0.95 { theme::ERROR } else if l > 0.7 { theme::WARN } else { theme::LIVE };
                                    ui.add(egui::ProgressBar::new(l).desired_width(ui.available_width()).desired_height(6.0).fill(color));
                                };
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut mic_on, "");
                                    ui.add(egui::Slider::new(&mut mic_g, 0.0..=2.0).text("My voice"));
                                });
                                meter(ui, audio::Controls::get(&ctl.mic_level));
                                ui.add(egui::Slider::new(&mut media_g, 0.0..=2.0).text("Video sound"));
                                meter(ui, audio::Controls::get(&ctl.media_level));
                                ui.checkbox(&mut hear, "Hear the video sound on my headphones");
                                ctl.mic_on.store(mic_on, Ordering::Relaxed);
                                audio::Controls::set(&ctl.mic_gain, mic_g);
                                audio::Controls::set(&ctl.media_gain, media_g);
                                ctl.monitor.store(hear, Ordering::Relaxed);
                                if let Some(e) = &self.audio_engine {
                                    ui.label(egui::RichText::new(e.status.lock().unwrap().clone()).small().color(theme::TEXT_DIM));
                                }
                                if ui.small_button("Refresh devices").clicked() {
                                    self.mics = audio::microphones();
                                    self.cable = audio::cable().map(|_| audio::cable_named());
                                    self.audio_key = None;
                                }
                            });
                            ui.add_space(6.0);
                            theme::section(ui, "Play from link");
                            theme::card(ui, |ui| {
                                ui.label("YouTube, Twitch, Facebook… live or recorded, with sound.");
                                if ui.button("▶  Play or download a link…").clicked() {
                                    self.link_dialog = Some(self.last_link.clone());
                                }
                                if let Some(d) = &self.video_dl {
                                    let st = d.status.lock().unwrap().clone();
                                    if d.active.load(Ordering::SeqCst) {
                                        ui.horizontal(|ui| {
                                            spinner(ui);
                                            ui.label(st);
                                        });
                                    } else {
                                        let file = d.file.lock().unwrap().clone();
                                        if file.is_some() {
                                            self.last_download = file;
                                        } else {
                                            self.load_err = Some(st);
                                        }
                                        self.video_dl = None;
                                    }
                                }
                                if let Some(f) = self.last_download.clone() {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Downloaded: {}",
                                            f.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                                        ))
                                        .small()
                                        .color(theme::LIVE),
                                    );
                                    ui.horizontal(|ui| {
                                        if ui.button("▶  Play it").clicked() {
                                            self.media_path = Some(f.clone());
                                            pending = Some(Spec::Media(f.clone()));
                                        }
                                        if ui.button("Open folder").clicked() {
                                            let _ = std::process::Command::new("explorer").arg(link::downloads_dir()).spawn();
                                        }
                                    });
                                }
                            });
                        });
                        ctx.request_repaint_after(std::time::Duration::from_millis(60)); // meters
                    }

                    Tab::Setup => {
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            if ui
                                .add_sized([ui.available_width(), 34.0], egui::Button::new(egui::RichText::new("♥  About MarkCam · Support the project").strong()).fill(theme::ACCENT_DIM))
                                .clicked()
                            {
                                self.about_open = true;
                            }
                            theme::section(ui, "Virtual camera");
                            theme::card(ui, |ui| {
                                if self.vcam_state != vcam::InstallState::Missing {
                                    ui.checkbox(&mut fx.vcam.on, "Send picture to \"MarkCam\"");
                                    let line = match (fx.vcam.on, fx.vcam.consumer()) {
                                        (false, _) => "Off — apps see a dark picture.".to_string(),
                                        (true, Some((cw, ch))) => format!("Live in an app at {cw}×{ch}."),
                                        (true, None) if fx.vcam.plugin_reports() => {
                                            "Ready. No app is using it right now (costs nothing).".to_string()
                                        }
                                        (true, None) => "Ready — pick \"MarkCam\" as the camera in ClassIn.".to_string(),
                                    };
                                    ui.label(egui::RichText::new(line).color(theme::TEXT_DIM));
                                    ui.horizontal(|ui| {
                                        if self.vcam_state == vcam::InstallState::Outdated
                                            && ui.button("Update to this MarkCam (admin)").clicked()
                                        {
                                            vcam_action = Some(false);
                                        }
                                        if ui.small_button("Uninstall").clicked() {
                                            vcam_action = Some(true);
                                        }
                                    });
                                } else if self.vcam_installing.is_some() {
                                    ui.label("Installing… confirm the Windows prompt.");
                                } else {
                                    ui.label("Makes \"MarkCam\" appear as a camera in ClassIn, Zoom, OBS…");
                                    if ui.button("Install (admin)").clicked() {
                                        vcam_action = Some(false);
                                    }
                                }
                            });
                            ui.add_space(6.0);
                            theme::section(ui, "Performance & battery");
                            theme::card(ui, |ui| {
                                let note = |ui: &mut egui::Ui, t: &str| {
                                    ui.label(egui::RichText::new(t).small().color(theme::TEXT_DIM));
                                };
                                ui.label("Preview while MarkCam is behind another window");
                                ui.horizontal_wrapped(|ui| {
                                    for (fps, name) in [(30, "Smooth · 30 fps"), (15, "Balanced · 15 fps"), (5, "Saver · 5 fps"), (0, "Off")] {
                                        ui.selectable_value(&mut self.bg_preview_fps, fps, name);
                                    }
                                });
                                note(
                                    ui,
                                    "Only changes how the picture moves inside this window. ClassIn, Zoom and other \
                                     apps always get the full 30 fps. Lower = less CPU and longer battery life. \
                                     Minimized, the preview always stops.",
                                );
                                ui.add_space(6.0);
                                ui.checkbox(&mut fx.idle_camera, "Rest the webcam when nothing shows the picture");
                                note(
                                    ui,
                                    "While MarkCam is minimized and no app is using the MarkCam camera, the webcam is \
                                     turned off and MarkCam uses almost no CPU. It turns back on by itself (in about a \
                                     second) when ClassIn opens the camera or you bring this window back.",
                                );
                                ui.add_space(6.0);
                                let mut auto = self.autostart;
                                if ui.checkbox(&mut auto, "Start MarkCam with Windows (minimized)").changed() {
                                    match autostart::set(auto) {
                                        Ok(()) => self.autostart = autostart::enabled(),
                                        Err(e) => self.load_err = Some(format!("start with Windows: {e}")),
                                    }
                                }
                                note(
                                    ui,
                                    "MarkCam is then always ready in ClassIn without opening it first, and with the \
                                     webcam resting it costs next to nothing until a lesson starts.",
                                );
                            });
                            ui.add_space(6.0);
                            theme::section(ui, "Downloads");
                            theme::card(ui, |ui| {
                                ui.label("Videos downloaded from links are saved in:");
                                ui.label(egui::RichText::new(link::downloads_dir().display().to_string()).small().monospace());
                                ui.horizontal(|ui| {
                                    if ui.button("Change folder…").clicked() {
                                        pick_dl_dir = true;
                                    }
                                    if ui.button("Open folder").clicked() {
                                        let dir = link::downloads_dir();
                                        let _ = std::fs::create_dir_all(&dir);
                                        let _ = std::process::Command::new("explorer").arg(dir).spawn();
                                    }
                                    if self.download_dir.is_some() && ui.small_button("Reset to default").clicked() {
                                        self.download_dir = None;
                                        link::set_downloads_dir(None);
                                    }
                                });
                            });
                            ui.add_space(6.0);
                            theme::section(ui, "Face tracking");
                            theme::card(ui, |ui| {
                                if have_face_models && face_active {
                                    ui.label(egui::RichText::new("● Active (a face mask is on)").color(theme::LIVE));
                                } else if have_face_models {
                                    ui.label("Ready — starts only while a face mask is on.");
                                } else if let Some(s) = &dl_status {
                                    ui.label(format!("Face models: {s}"));
                                } else {
                                    ui.label("Face masks need the face models (~6 MB).");
                                    if ui.button("Download face models").clicked() {
                                        want_download = true;
                                    }
                                }
                            });
                            ui.add_space(6.0);
                            theme::section(ui, "Effect library");
                            theme::card(ui, |ui| {
                                let mut mc = library::use_manycam();
                                if !PUBLIC
                                    && ui
                                    .checkbox(&mut mc, "Also use effects and backgrounds from an installed ManyCam")
                                    .on_hover_text("MarkCam has its own effects and backgrounds. Turn this on to also list ManyCam's (from its folders on this computer).")
                                    .changed()
                                {
                                    library::set_use_manycam(mc);
                                    do_refresh = true;
                                    self.bg_files = None;
                                }
                                ui.label(egui::RichText::new(format!("{} effects in the library", self.lib.total())).small().color(theme::TEXT_DIM));
                                let (locked, other) = self.lib.hidden();
                                if locked + other > 0 {
                                    let mut why = Vec::new();
                                    if locked > 0 {
                                        why.push(format!("{locked} are encrypted by ManyCam (they only work inside ManyCam)"));
                                    }
                                    if other > 0 {
                                        why.push(format!("{other} use effect types not supported yet"));
                                    }
                                    ui.label(
                                        egui::RichText::new(format!("Hidden: {}.", why.join("; ")))
                                            .small()
                                            .color(theme::TEXT_DIM),
                                    );
                                }
                                ui.horizontal(|ui| {
                                    if ui.button("Rescan").clicked() {
                                        do_refresh = true;
                                    }
                                    if !PUBLIC && ui.button("Locate folder…").clicked() {
                                        locate_folder = true;
                                    }
                                    if ui.button("Import file…").clicked() {
                                        do_import = true;
                                    }
                                });
                            });
                        });
                    }
                }
            });

        // ── Preview ────────────────────────────────────────────────────────────────────
        let tex_id = self.tex.as_ref().map(|t| t.id());
        let mirror = self.mirror;
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG).inner_margin(egui::Margin::same(12.0)))
            .show(ctx, |ui| {
            let Some(tex_id) = tex_id else {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("Waiting for the camera…").color(theme::TEXT_DIM).size(16.0))
                });
                return;
            };
            let (fw, fh) = (fx.w.max(1) as f32, fx.h.max(1) as f32);
            let mut full = ui.available_rect_before_wrap();
            // Playing a link / video: a transport bar (play/pause, scrub) under the picture.
            let pb = link::playback();
            if pb.active && matches!(self.current, Spec::Link(..) | Spec::Media(_)) {
                let bar = egui::Rect::from_min_max(egui::pos2(full.min.x, full.max.y - 34.0), full.max);
                full.max.y -= 42.0;
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(bar), |ui| transport_bar(ui, &pb, &mut self.scrub));
            }
            let scale = (full.width() / fw).min(full.height() / fh).max(0.05);
            let img_size = egui::vec2(fw * scale, fh * scale);
            let ppp = ui.ctx().pixels_per_point();
            self.preview_px = (img_size.x * ppp, img_size.y * ppp);
            let rect = egui::Rect::from_center_size(full.center(), img_size);
            let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

            // (The texture itself is already mirrored when `mirror` is on.)
            egui::Image::new(egui::load::SizedTexture::new(tex_id, img_size)).rounding(10.0).paint_at(ui, rect);

            // Screen<->image-normalized mapping (accounts for the mirror flip).
            let s2n = |p: egui::Pos2| -> (f32, f32) {
                let mut u = (p.x - rect.min.x) / img_size.x;
                let v = (p.y - rect.min.y) / img_size.y;
                if mirror {
                    u = 1.0 - u;
                }
                (u, v)
            };
            let n2s = |u: f32, v: f32| -> egui::Pos2 {
                let uu = if mirror { 1.0 - u } else { u };
                egui::pos2(rect.min.x + uu * img_size.x, rect.min.y + v * img_size.y)
            };

            // Eyedropper: the next click picks the color key from the camera picture.
            if self.picking_key {
                let response = response.clone().on_hover_cursor(egui::CursorIcon::Crosshair);
                if response.clicked() {
                    if let Some(p) = response.interact_pointer_pos() {
                        let (u, v) = s2n(p);
                        let (w, h) = (fx.w as usize, fx.h as usize);
                        if w > 0 && !fx.cur_rgba.is_empty() {
                            let x = ((u.clamp(0.0, 1.0) * w as f32) as usize).min(w - 1);
                            let y = ((v.clamp(0.0, 1.0) * h as f32) as usize).min(h - 1);
                            // Each click adds one more background color (until "Done").
                            let c = vbg::pick_color(&fx.cur_rgba, w, h, x, y);
                            if !fx.vbg.keys.contains(&c) && fx.vbg.keys.len() < 32 {
                                fx.vbg.keys.push(c);
                            }
                        }
                    }
                }
            }
            // Effect Maker open with a piece selected: dragging moves that piece on the face
            // (in face units, so it stays put relative to the face), scrolling resizes it.
            let maker_drag = self.maker.as_ref().is_some_and(|m| m.selected.is_some());
            if let Some(m) = self.maker.as_mut().filter(|_| maker_drag) {
                if response.hovered() {
                    let sy = ui.input(|i| i.raw_scroll_delta.y);
                    if sy != 0.0 {
                        m.grow(1.0 + sy * 0.0015);
                    }
                }
                if response.dragged() {
                    let d = response.drag_delta();
                    let (mut dx, dy) = (d.x / img_size.x * fw, d.y / img_size.y * fh);
                    if mirror {
                        dx = -dx;
                    }
                    if let Some([_, _, unit, ang]) = fx.maker_preview.as_ref().and_then(|o| o.pose) {
                        let (sn, cs) = ang.sin_cos();
                        m.nudge((dx * cs + dy * sn) / unit.max(1.0), (-dx * sn + dy * cs) / unit.max(1.0));
                    }
                    m.dragging = true;
                }
                if response.drag_stopped() {
                    m.dragging = false;
                    m.touch();
                }
                let hint = if fx.maker_preview.as_ref().is_some_and(|o| o.drawn.is_some()) { "Drag to place the selected piece · scroll to resize" } else { "Look at the camera — the effect shows on your face" };
                ui.painter_at(rect).text(rect.center_top() + egui::vec2(0.0, 18.0), egui::Align2::CENTER_CENTER, hint, egui::FontId::proportional(14.0), egui::Color32::WHITE);
            }
            // Scroll wheel over the preview resizes the selected overlay.
            if response.hovered() && !maker_drag {
                let sy = ui.input(|i| i.raw_scroll_delta.y);
                if sy != 0.0 {
                    if let Some(ov) = self.selected.and_then(|i| fx.overlays.get_mut(i)) {
                        ov.scale = (ov.scale * (1.0 + sy * 0.0015)).clamp(0.05, 6.0);
                    }
                }
            }

            // Begin a drag: a corner handle on the selected overlay resizes; a body hit moves.
            if response.drag_started() && !maker_drag {
                if let Some(p) = response.interact_pointer_pos() {
                    let (u, v) = s2n(p);
                    self.drag_mode = None;
                    // Face-following layers are placed by the face: no move / corner resize
                    // (scroll still scales them).
                    if let Some(ov) = self.selected.and_then(|i| fx.overlays.get(i)).filter(|o| !o.follows_face()) {
                        let hx = ov.pos[0] + ov.size[0] * ov.scale;
                        let hy = ov.pos[1] + ov.size[1] * ov.scale;
                        let ru = 14.0 / img_size.x;
                        let rv = 14.0 / img_size.y;
                        if (u - hx).abs() <= ru && (v - hy).abs() <= rv {
                            self.drag_mode = Some(DragMode::Resize);
                        }
                    }
                    if self.drag_mode.is_none() {
                        let hit = fx.overlays.iter().enumerate().rev().find(|(_, ov)| {
                            ov.on && ov.hit_rect().is_some_and(|r| u >= r[0] && u <= r[2] && v >= r[1] && v <= r[3])
                        });
                        if let Some((i, ov)) = hit {
                            self.selected = Some(i);
                            if !ov.follows_face() {
                                self.grab = (u - ov.pos[0], v - ov.pos[1]);
                                self.drag_mode = Some(DragMode::Move);
                            }
                        } else if fx.warp.is_active() && fx.warp.kind.has_center() && !fx.warp_follow_face {
                            self.drag_mode = Some(DragMode::WarpCenter);
                            fx.warp.cx = u.clamp(0.0, 1.0);
                            fx.warp.cy = v.clamp(0.0, 1.0);
                        }
                    }
                }
            }
            if response.dragged() && !maker_drag {
                if let (Some(mode), Some(p)) = (self.drag_mode, response.interact_pointer_pos()) {
                    let (u, v) = s2n(p);
                    if mode == DragMode::WarpCenter {
                        fx.warp.cx = u.clamp(0.0, 1.0);
                        fx.warp.cy = v.clamp(0.0, 1.0);
                    } else if let Some(ov) = self.selected.and_then(|i| fx.overlays.get_mut(i)) {
                        match mode {
                            DragMode::Move => ov.pos = [u - self.grab.0, v - self.grab.1],
                            DragMode::Resize => {
                                let want = (u - ov.pos[0]).max(0.01);
                                ov.scale = (want / ov.size[0].max(1e-4)).clamp(0.05, 6.0);
                            }
                            DragMode::WarpCenter => {}
                        }
                    }
                }
            }
            if response.drag_stopped() {
                self.drag_mode = None;
            }
            // A plain click selects the effect under the pointer (topmost), or clears the
            // selection on empty picture.
            if response.clicked() {
                if let Some(p) = response.interact_pointer_pos() {
                    let (u, v) = s2n(p);
                    self.selected = fx.overlays.iter().enumerate().rev().find_map(|(i, ov)| {
                        let hit = ov.on && ov.hit_rect().is_some_and(|r| u >= r[0] && u <= r[2] && v >= r[1] && v <= r[3]);
                        hit.then_some(i)
                    });
                }
            }
            // Delete / Backspace removes the selected effect, Esc deselects — unless a text
            // field (the effect search) has the keyboard.
            if !ui.ctx().wants_keyboard_input() {
                let (del, esc) = ui.input(|i| {
                    (i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace), i.key_pressed(egui::Key::Escape))
                });
                if let (true, Some(i)) = (del, self.selected) {
                    if i < fx.overlays.len() {
                        fx.overlays.remove(i);
                    }
                    self.selected = None;
                    self.drag_mode = None;
                }
                if esc {
                    self.selected = None;
                }
            }

            // Selection outline + resize handle.
            if let Some(ov) = self.selected.and_then(|i| fx.overlays.get(i)) {
                if let Some(r) = ov.hit_rect() {
                    let (a, b) = (n2s(r[0], r[1]), n2s(r[2], r[3]));
                    let painter = ui.painter_at(rect);
                    painter.rect_stroke(egui::Rect::from_two_pos(a, b), 4.0_f32, egui::Stroke::new(2.0_f32, theme::ACCENT));
                    // Corner handle only for freely placed layers (face ones follow the face).
                    if !ov.follows_face() {
                        painter.circle_filled(b, 7.0, theme::ACCENT);
                        painter.circle_stroke(b, 7.0, egui::Stroke::new(1.5_f32, egui::Color32::WHITE));
                    }
                }
            }
        });

        // Everything below may block (file dialogs, loading effects): let the capture thread
        // keep feeding the virtual camera meanwhile; re-lock only briefly where needed.
        drop(fx_guard);
        if let Some(s) = pending {
            self.switch_to(s);
        }
        match source_action {
            Some(SourceAction::FullScreen) => self.switch_to(Spec::Screen(screen::primary_screen())),
            Some(SourceAction::LastArea) => {
                if let Some(a) = self.screen_area {
                    self.switch_to(Spec::Screen(a));
                }
            }
            Some(SourceAction::PickArea) if self.area_pick.is_none() => {
                // Get MarkCam out of the way first, so the area under it can be picked.
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                self.area_pick = Some((std::time::Instant::now(), None));
            }
            Some(SourceAction::PickVideo) => {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Videos", &video::VIDEO_EXTENSIONS)
                    .set_title("Play a video as the picture")
                    .pick_file()
                {
                    self.media_path = Some(p.clone());
                    self.switch_to(Spec::Media(p));
                }
            }
            Some(SourceAction::LinkDialog) => self.link_dialog = Some(self.last_link.clone()),
            Some(SourceAction::BackToCamera) => {
                if let Some(idx) = self.last_camera.or(self.cameras.first().map(|c| c.index)) {
                    let spec = self.open_camera(idx);
                    self.switch_to(spec);
                }
            }
            _ => {}
        }
        self.show_link_dialog(ctx);
        // Helper programs finished downloading: play the waiting link.
        if self.tools_fetch.as_ref().is_some_and(|f| !f.active.load(Ordering::SeqCst)) {
            let f = self.tools_fetch.take();
            if tools::ready() && !f.as_ref().is_some_and(|f| f.failed.load(Ordering::SeqCst)) {
                if let Some(url) = self.pending_link.take() {
                    self.switch_to(Spec::Link(url, self.link_quality));
                }
            } else if let Some(f) = f {
                self.load_err = Some(f.status.lock().unwrap().clone());
                self.pending_link = None;
            }
        }
        // MarkCam Microphone setup finished: look at the devices again.
        if self.audio_setup.as_ref().is_some_and(|s| !s.active.load(Ordering::SeqCst)) {
            self.audio_setup = None;
            self.cable = audio::cable().map(|_| audio::cable_named());
            self.mics = audio::microphones();
            self.audio_key = None; // restart the engine with the cable
        }
        // The audio engine runs while it has something to do: feeding MarkCam Microphone,
        // or playing a video / link's sound. (Never in test runs: it opens the microphone.)
        if std::env::var_os("OVC_TEST").is_none() {
            let media_playing = matches!(self.current, Spec::Link(..) | Spec::Media(_));
            let feed = self.audio_route && self.cable.is_some();
            let key = (feed || media_playing).then(|| (feed, self.audio_mic.clone()));
            if key != self.audio_key {
                self.audio_engine = None;
                if let Some((feed, mic)) = &key {
                    self.audio_engine = Some(audio::Engine::start(self.audio_ctl.clone(), mic.clone(), *feed));
                }
                self.audio_key = key;
            }
        }

        // Area picking: open the picker once MarkCam has minimized, then take its answer.
        if let Some((since, rx)) = &mut self.area_pick {
            if rx.is_none() && since.elapsed() >= std::time::Duration::from_millis(400) {
                let (tx, r) = std::sync::mpsc::channel();
                thread::spawn(move || {
                    let _ = tx.send(screen::pick_area());
                });
                *rx = Some(r);
            }
            let answer = rx.as_ref().and_then(|r| r.try_recv().ok());
            if let Some(res) = answer {
                self.area_pick = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                if let Some(a) = res {
                    self.screen_area = Some(a);
                    self.switch_to(Spec::Screen(a));
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        // While the screen is the source, hide MarkCam itself from capture (no endless
        // mirror when the area overlaps this window).
        let want_hidden = matches!(self.current, Spec::Screen(_));
        if want_hidden != self.capture_hidden {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = frame.window_handle() {
                if let RawWindowHandle::Win32(wh) = h.as_raw() {
                    screen::exclude_from_capture(wh.hwnd.get(), want_hidden);
                    self.capture_hidden = want_hidden;
                }
            }
        }
        match cat_action {
            Some(CatAction::Move(item, cat)) => self.lib.move_item(&item, cat.as_deref()),
            Some(CatAction::New(item)) => self.cat_dialog = Some(CatDialog::New { name: String::new(), item }),
            Some(CatAction::Rename(old)) => self.cat_dialog = Some(CatDialog::Rename { name: old.clone(), old }),
            Some(CatAction::Delete(name)) => {
                self.lib.delete_category(&name);
                if self.category.as_deref() == Some(name.as_str()) {
                    self.category = None;
                }
            }
            Some(CatAction::DeleteItem(item)) => {
                // Take it off the picture first (a playing video keeps its file open).
                if let Some(p) = item.r.file.clone() {
                    use settings::LayerSource as L;
                    let removed = {
                        let mut fx = fx_arc.lock().unwrap();
                        let n = fx.overlays.len();
                        fx.overlays.retain(|o| !matches!(&o.source, Some(L::Image(x) | L::Video(x) | L::McvFile(x)) if *x == p));
                        fx.overlays.len() != n
                    };
                    if removed {
                        self.selected = None;
                        // Give a video decoder a moment to let go of the file.
                        std::thread::sleep(std::time::Duration::from_millis(150));
                    }
                }
                if let Err(e) = self.lib.delete_item(&item) {
                    self.load_err = Some(format!("could not delete “{}”: {e} (is it in use as a layer?)", item.name()));
                }
            }
            Some(CatAction::Restore) => self.lib.restore_deleted(),
            None => {}
        }
        self.show_cat_dialog(ctx);
        self.show_about(ctx);
        if add_backgrounds {
            if let Some(picked) = rfd::FileDialog::new()
                .set_title("Add backgrounds (pictures or videos)")
                .add_filter("Pictures and videos", &["jpg", "jpeg", "png", "bmp", "webp", "mp4", "mov", "m4v", "wmv", "avi", "mkv", "webm"])
                .pick_files()
            {
                let dir = vbg::my_backgrounds_dir();
                let _ = std::fs::create_dir_all(&dir);
                let mut last = None;
                for p in picked {
                    if let Some(name) = p.file_name() {
                        let dst = dir.join(name);
                        match std::fs::copy(&p, &dst) {
                            Ok(_) => last = Some(dst),
                            Err(e) => self.load_err = Some(format!("couldn't add {}: {e}", p.display())),
                        }
                    }
                }
                self.bg_files = None;
                // Use the (last) one just added.
                if let Some(p) = last {
                    self.fx.lock().unwrap().vbg.choice = vbg::Choice::File(p);
                }
            }
        }
        if pick_dl_dir {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("Where should downloaded videos be saved?")
                .set_directory(link::downloads_dir())
                .pick_folder()
            {
                link::set_downloads_dir(Some(dir.clone()));
                self.download_dir = Some(dir);
            }
        }
        if locate_folder {
            if let Some(dir) = rfd::FileDialog::new().set_title("Select a folder with ManyCam .mcep packs").pick_folder() {
                self.lib = library::Library::start(Some(dir));
                self.category = None;
            }
        }
        if do_refresh {
            self.lib = library::Library::start(None);
            self.category = None;
        }
        // Own images / GIFs: from "+", or files dragged onto the window. They are copied
        // into "My effects" so they stay in the browser, and added to the picture.
        let mut incoming: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if add_image {
            incoming.extend(
                rfd::FileDialog::new()
                    .add_filter("Images, animations and videos", &[&ovc_mcam::IMAGE_EXTENSIONS[..], &video::VIDEO_EXTENSIONS[..]].concat())
                    .set_title("Add your own effects")
                    .pick_files()
                    .unwrap_or_default(),
            );
        }
        for path in incoming {
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
            if ovc_mcam::is_image_file(&path) || video::is_video_file(&path) {
                match self.lib.add_my_effect(&path) {
                    Ok(item) => add_items.push(item),
                    Err(e) => self.load_err = Some(format!("could not add {}: {e}", path.display())),
                }
            } else if ext == "markcam" {
                match maker::import(&path).and_then(|img| self.lib.add_my_effect(&img).map_err(Into::into)) {
                    Ok(item) => add_items.push(item),
                    Err(e) => self.load_err = Some(format!("could not import {}: {e}", path.display())),
                }
            } else if ext == "mcv" && !PUBLIC {
                if let Some(item) = self.lib.add_imported(path) {
                    add_items.push(item);
                }
            } else {
                self.load_err = Some(format!("unsupported file: {}", path.display()));
            }
        }
        if do_import {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Effects", if PUBLIC { &["markcam"][..] } else { &["markcam", "mcv", "mcep"][..] })
                .set_title(if PUBLIC { "Import a shared MarkCam effect (.markcam)" } else { "Import an effect (MarkCam .markcam, ManyCam .mcv / .mcep)" })
                .pick_file()
            {
                let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
                if ext == "markcam" {
                    match maker::import(&path).and_then(|img| self.lib.add_my_effect(&img).map_err(Into::into)) {
                        Ok(item) => add_items.push(item),
                        Err(e) => self.load_err = Some(format!("could not import {}: {e}", path.display())),
                    }
                } else if ext == "mcep" {
                    // Index every pack in that folder alongside the defaults.
                    self.lib = library::Library::start(path.parent().map(Path::to_path_buf));
                    self.category = None;
                } else if let Some(item) = self.lib.add_imported(path) {
                    add_items.push(item);
                }
            }
        }
        // Decode new layers without holding the pipeline, then add them.
        let mut new_layers: Vec<Overlay> = Vec::new();
        for item in add_items {
            // Videos play through Media Foundation, not the ManyCam/image loader.
            if let Some(p) = item.r.file.as_deref().filter(|p| video::is_video_file(p)) {
                match overlay_from_video(p) {
                    Ok(ov) => {
                        self.load_err = None;
                        new_layers.push(ov);
                    }
                    Err(e) => self.load_err = Some(format!("{}: {e}", p.display())),
                }
                continue;
            }
            match ovc_mcam::load_effect_ref(&item.r) {
                Ok(e) if matches!(e.kind, ovc_mcam::Kind::FaceMask) && self.face_models.is_none() => {
                    self.load_err = Some("face masks need the face models — see Setup".into());
                }
                Ok(e) => {
                    self.load_err = None;
                    let mut ov = overlay_from_effect(e);
                    ov.source = Some(layer_source(&item.r));
                    ov.thumb_key = Some(item.key.clone());
                    new_layers.push(ov);
                }
                Err(e) => self.load_err = Some(format!("load failed: {e}")),
            }
        }
        let mut fx_guard = fx_arc.lock().unwrap();
        let fx: &mut Fx = &mut fx_guard;
        for ov in new_layers {
            fx.overlays.push(ov);
            self.selected = Some(fx.overlays.len() - 1);
        }
        // Effect Maker window.
        if open_maker && self.maker.is_none() {
            self.maker = Some(maker::Maker::new());
            self.selected = None;
        }
        let mut close_maker = false;
        if let Some(m) = &mut self.maker {
            for ev in m.ui(ctx) {
                match ev {
                    maker::MakerEvent::Preview(r) => {
                        let pose = fx.maker_preview.as_ref().and_then(|o| o.pose);
                        fx.maker_preview = r.map(|r| {
                            let mut ov = Overlay::new("Effect Maker".into(), r.frames, true, [0.0, 0.0], [1.0, 1.0]);
                            ov.anchor = Some((r.center, r.size));
                            ov.pose = pose;
                            ov
                        });
                    }
                    maker::MakerEvent::Saved(img) => {
                        self.thumbs.forget(&img.to_string_lossy());
                        if let Err(e) = self.lib.add_my_effect(&img) {
                            self.load_err = Some(format!("saved, but couldn't list it: {e}"));
                        }
                    }
                    maker::MakerEvent::Closed => close_maker = true,
                }
            }
        }
        if close_maker {
            self.maker = None;
            fx.maker_preview = None;
        }
        if let Some(uninstall) = vcam_action {
            match vcam::install(uninstall) {
                Ok(busy) => self.vcam_installing = Some(busy),
                Err(e) => fx.vcam.err = Some(e),
            }
        }
        if self.vcam_installing.as_ref().is_some_and(|b| !b.load(Ordering::SeqCst)) {
            self.vcam_installing = None;
            self.vcam_state = vcam::install_state();
        }
        if want_download && self.download.is_none() {
            self.download = Some(Download::start());
        }
        // When a download finishes, pick up the freshly available models.
        if let Some(d) = &self.download {
            if !d.active.load(Ordering::SeqCst) {
                self.face_models = find_models();
                if self.face_models.is_some() {
                    self.download = None;
                }
            }
        }
        // Spin up / signal the face tracker based on whether any face mask is active.
        let want_face = fx.overlays.iter().any(|o| o.on && o.follows_face())
            || fx.maker_preview.is_some()
            || (fx.warp.is_active() && fx.warp.kind.has_center() && fx.warp_follow_face && self.face_models.is_some());
        if want_face && self.tracker.is_none() {
            if self.face_models.is_none() {
                self.face_models = find_models();
            }
            if let Some(dir) = self.face_models.clone() {
                self.tracker = Some(FaceWorker::start(&dir, self.cap.latest.clone()));
            }
        }
        if let Some(t) = &self.tracker {
            // A frozen picture keeps its masks where they were: no tracking needed.
            t.want.store(want_face && !self.frozen, Ordering::SeqCst);
        }
        // Person segmentation, while a virtual background is chosen (shut down 3 s after).
        let want_bg = fx.vbg.needs_matte();
        if want_bg && self.seg.is_none() {
            let integrated = fx.gpu.adapter_info.to_string().contains("IntegratedGpu");
            let w = vbg::SegWorker::start(self.cap.latest.clone(), fx.vbg.quality, integrated);
            fx.vbg.matte_src = Some(w.matte.clone());
            self.seg = Some(w);
        }
        if let Some(sw) = &self.seg {
            sw.want.store(want_bg && !self.frozen, Ordering::SeqCst);
            sw.quality.store(fx.vbg.quality, Ordering::SeqCst);
            fx.vbg.wait_for_matte = sw.fast.load(Ordering::SeqCst);
        }
        if want_bg {
            self.seg_idle_since = None;
        } else if self.seg.is_some() {
            let since = *self.seg_idle_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= std::time::Duration::from_secs(3) {
                self.seg = None;
                fx.vbg.matte_src = None;
                self.seg_idle_since = None;
            }
        }
        if let Some(t) = &self.tracker {
            if let Some(e) = t.err.lock().unwrap().clone() {
                self.load_err = Some(format!("face tracking: {e}"));
            }
        }
        // Fully shut the tracker down (thread + ONNX models freed) once no face mask has
        // been active for a few seconds; it is restarted on demand when one is enabled.
        if want_face {
            self.face_idle_since = None;
        } else if self.tracker.is_some() {
            let since = *self.face_idle_since.get_or_insert_with(std::time::Instant::now);
            // Kept a while (idle it costs nothing but memory): switching between face effects
            // must not reload the models each time.
            if since.elapsed() >= std::time::Duration::from_secs(120) {
                self.tracker = None;
                self.face_idle_since = None;
            }
        }
        self.autosave(fx, false);
        self.test_screenshot(ctx, fx);

        // New frames wake the UI from the capture thread; this is only a slow fallback for
        // status changes (download progress, camera errors) while no frames are flowing.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// The preview texture at roughly the size it is shown (`shown`, physical px): an integer
/// k×k box-filter downscale when the frame is at least twice as large. Saves both the
/// per-frame image copy and the texture upload (1080p shown at ~550 px wide → k = 3).
/// "1:05:09" / "3:07".
/// Where donations go (PayPal) and who made MarkCam.
const AUTHOR: &str = "Marko Stevanović";
const DONATE_EMAIL: &str = "alertpay91@gmail.com";
/// Where the source code is published (GPL-3.0). Empty: the copy installed with MarkCam.
const SOURCE_URL: &str = "https://github.com/alertpay91/MarkCam";
/// PayPal's "Donate" button doesn't work for Serbia ("Donations aren't supported in this
/// organization's country"); a plain payment ("Buy Now") link does.
fn coffee_url(eur: u32) -> String {
    format!("https://www.paypal.com/cgi-bin/webscr?cmd=_xclick&business=alertpay91%40gmail.com&item_name=Coffee%20for%20MarkCam&amount={eur}.00&currency_code=EUR&no_shipping=1")
}

impl Studio {
    /// About / support window: author, the AI credit, a PayPal "buy me a coffee" button and
    /// the third-party credits that their licenses ask for.
    fn show_about(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        egui::Window::new("About MarkCam")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::logo(ui);
                    ui.label(egui::RichText::new(format!("MarkCam {}", env!("CARGO_PKG_VERSION"))).strong().size(20.0));
                });
                ui.add_space(4.0);
                ui.label(format!("Made by {AUTHOR}, a teacher, for teachers who teach online — free to use."));
                ui.label(
                    egui::RichText::new("Built together with Claude, an AI assistant made by Anthropic.")
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(10.0);
                theme::card(ui, |ui| {
                    ui.label(egui::RichText::new("☕  Buy me a coffee").strong().size(16.0));
                    ui.label("If MarkCam makes your lessons more fun, a small donation helps me keep improving it. Thank you!");
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        for (eur, cups) in [(3u32, "☕"), (5, "☕☕"), (10, "☕☕☕")] {
                            let b = egui::Button::new(egui::RichText::new(format!("{cups}  {eur} €")).strong().color(egui::Color32::WHITE)).fill(egui::Color32::from_rgb(0, 112, 186)).min_size(egui::vec2(0.0, 34.0));
                            if ui.add(b).on_hover_text("Opens PayPal in your browser").clicked() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(coffee_url(eur)));
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("PayPal: {DONATE_EMAIL}")).color(theme::TEXT_DIM));
                        if ui.small_button("Copy").on_hover_text("Copy the PayPal address").clicked() {
                            ui.ctx().copy_text(DONATE_EMAIL.into());
                        }
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Free software (GNU GPL v3): you may use, share and change it.").small());
                    if ui.small_button("Source code").clicked() {
                        if SOURCE_URL.is_empty() {
                            let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("source"))).filter(|d| d.is_dir());
                            match dir {
                                Some(d) => {
                                    let _ = std::process::Command::new("explorer").arg(d).spawn();
                                }
                                None => self.load_err = Some("the source code is not installed with this copy".into()),
                            }
                        } else {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(SOURCE_URL));
                        }
                    }
                });
                egui::CollapsingHeader::new("Credits and licenses").show(ui, |ui| {
                    let line = |ui: &mut egui::Ui, t: &str| {
                        ui.label(egui::RichText::new(t).small());
                    };
                    line(ui, "Emoji art: Noto Animated Emoji © Google (CC BY 4.0, resized and re-encoded); Fluent Emoji 3D © Microsoft (MIT).");
                    line(ui, "Fonts: Lilita One and Poppins (SIL Open Font License), Luckiest Guy (Apache 2.0).");
                    line(ui, "MarkCam Microphone uses VB-CABLE by VB-Audio Software — donationware; if you find it useful, support it at www.vb-cable.com.");
                    line(ui, "Virtual background: Robust Video Matting (GPL-3.0), MODNet (Apache 2.0), ONNX Runtime (MIT), DirectML (Microsoft).");
                    line(ui, "Face tracking: MediaPipe Face Mesh © Google (Apache 2.0), Ultra-Light face detector (MIT).");
                    line(ui, "Video links and downloads: yt-dlp (Unlicense) and FFmpeg (GPL). Only download videos you are allowed to use.");
                    line(ui, "Full license texts are in the \"licenses\" folder next to MarkCam.exe.");
                    ui.add_space(4.0);
                    line(ui, "MarkCam is an independent project, not affiliated with or endorsed by Anthropic, ManyCam, VB-Audio, Microsoft or Google. Their names belong to their owners.");
                });
            });
        if !open {
            self.about_open = false;
        }
    }
}
// ── Name banners ───────────────────────────────────────────────────────────────────────
impl Studio {
    /// Show the selected banner, or hide the one on the picture.
    fn toggle_banner(&mut self, fx: &mut Fx, ctx: &egui::Context) {
        if self.banner_pending {
            self.banner_pending = false;
            return;
        }
        match &mut fx.banner {
            Some(b) if !b.is_leaving() => b.hide(),
            _ => {
                self.banner_pending = true;
                self.banner_wait = std::time::Instant::now();
                self.banner_tick(fx, ctx);
            }
        }
    }

    /// A banner waiting for its icon to download: show it once ready (or without the icon
    /// after a few seconds).
    fn banner_tick(&mut self, fx: &mut Fx, ctx: &egui::Context) {
        if !self.banner_pending {
            return;
        }
        let Some(def) = self.banners.get(self.banner_sel).cloned() else {
            self.banner_pending = false;
            return;
        };
        let icon = if def.icon.is_empty() { None } else { banner::icon_image(&def.icon, &self.fetch, ctx) };
        if icon.is_some() || def.icon.is_empty() || self.banner_wait.elapsed() > std::time::Duration::from_secs(5) {
            fx.banner = Some(banner::Live::new(def, icon));
            self.banner_pending = false;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Small rendering of a banner (style picker / list), cached by its look.
    fn banner_thumb(&mut self, ctx: &egui::Context, def: &banner::BannerDef) -> Option<egui::TextureHandle> {
        let icon = if def.icon.is_empty() { None } else { banner::icon_image(&def.icon, &self.fetch, ctx) };
        let key = format!("{}|{}|{:?}|{}|{}|{}", def.title, def.subtitle, def.style, def.color, def.icon, icon.is_some());
        if let Some(t) = self.banner_previews.get(&key) {
            return t.clone();
        }
        let mut d = def.clone();
        d.size = 0.3;
        let tex = banner::preview(&d, 140, icon.as_ref()).map(|(rgba, w, h)| {
            ctx.load_texture(format!("banner:{key}"), egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba), egui::TextureOptions::LINEAR)
        });
        if self.banner_previews.len() > 64 {
            self.banner_previews.clear();
        }
        self.banner_previews.insert(key, tex.clone());
        tex
    }

    fn banner_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, fx: &mut Fx) {
        ui.horizontal(|ui| {
            if ui.small_button("‹ Effects").clicked() {
                self.tab = Tab::Effects;
            }
            ui.label(egui::RichText::new("Name banner").strong().size(16.0));
        });
        ui.label(egui::RichText::new("Your name (or anything) at the bottom of the picture — slides in, shines, slides out. Key N or the 🏷 Name button shows / hides it.").small().color(theme::TEXT_DIM));
        ui.add_space(4.0);
        let shown = fx.banner.as_ref().is_some_and(|b| !b.is_leaving()) || self.banner_pending;
        let label = if shown { "⏹  Hide banner" } else { "▶  Show banner" };
        if ui
            .add_sized([ui.available_width(), 40.0], egui::Button::new(egui::RichText::new(label).strong().size(16.0)).fill(if shown { theme::WARN.gamma_multiply(0.6) } else { theme::ACCENT_DIM }))
            .clicked()
        {
            self.toggle_banner(fx, ctx);
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // Saved banners.
            theme::section(ui, "My banners");
            let mut pick = None;
            let mut remove = None;
            theme::card(ui, |ui| {
                for i in 0..self.banners.len() {
                    let def = self.banners[i].clone();
                    ui.horizontal(|ui| {
                        let sel = i == self.banner_sel;
                        let r = ui.selectable_label(sel, if def.title.trim().is_empty() { "(no text)".to_string() } else { def.title.clone() });
                        if r.clicked() {
                            pick = Some(i);
                        }
                        if !def.subtitle.is_empty() {
                            ui.label(egui::RichText::new(&def.subtitle).small().color(theme::TEXT_DIM));
                        }
                        if self.banners.len() > 1 {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("🗑").on_hover_text("Delete this banner").clicked() {
                                    remove = Some(i);
                                }
                            });
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if ui.button("➕ New banner").clicked() {
                        let mut d = self.banners.get(self.banner_sel).cloned().unwrap_or_default();
                        d.title = "Welcome to class!".into();
                        d.subtitle = "欢迎来上课！".into();
                        self.banners.push(d);
                        pick = Some(self.banners.len() - 1);
                    }
                    if ui.button("📋 Copy").clicked() {
                        let d = self.banners[self.banner_sel].clone();
                        self.banners.push(d);
                        pick = Some(self.banners.len() - 1);
                    }
                });
            });
            if let Some(i) = remove {
                self.banners.remove(i);
                self.banner_sel = self.banner_sel.min(self.banners.len() - 1);
            }
            if let Some(i) = pick {
                self.banner_sel = i;
                // Switching while one is shown: show the new one.
                if let Some(b) = &mut fx.banner {
                    if !b.is_leaving() {
                        fx.banner = None;
                        self.banner_pending = true;
                        self.banner_wait = std::time::Instant::now();
                    }
                }
            }

            // The selected banner.
            let i = self.banner_sel.min(self.banners.len().saturating_sub(1));
            let mut d = self.banners[i].clone();
            theme::section(ui, "Text");
            theme::card(ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut d.title).hint_text("Your name, e.g. Teacher Mark").desired_width(f32::INFINITY).font(egui::FontId::proportional(17.0)));
                ui.add(egui::TextEdit::singleline(&mut d.subtitle).hint_text("Second line (optional), e.g. English teacher · 英语老师").desired_width(f32::INFINITY));
                ui.label(egui::RichText::new("Any text works, also Chinese. Leave the second line empty for a one-line banner.").small().color(theme::TEXT_DIM));
            });
            theme::section(ui, "Style");
            theme::card(ui, |ui| {
                let w = (ui.available_width() - 8.0) / 2.0;
                for row in banner::Style::ALL.chunks(2) {
                    ui.horizontal(|ui| {
                        for &st in row {
                            let mut look = d.clone();
                            look.style = st;
                            let tex = self.banner_thumb(ctx, &look);
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, w * 0.42), egui::Sense::click());
                            let p = ui.painter();
                            p.rect_filled(rect, 8.0, egui::Color32::from_rgb(70, 82, 100));
                            if let Some(t) = tex {
                                let s = t.size_vec2();
                                let k = ((rect.width() - 8.0) / s.x).min((rect.height() - 18.0) / s.y);
                                let r = egui::Rect::from_center_size(rect.center() - egui::vec2(0.0, 6.0), s * k);
                                p.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                            }
                            p.text(rect.center_bottom() - egui::vec2(0.0, 8.0), egui::Align2::CENTER_CENTER, st.name(), egui::FontId::proportional(11.5), theme::TEXT);
                            if d.style == st {
                                p.rect_stroke(rect, 8.0, egui::Stroke::new(2.5_f32, theme::ACCENT));
                            }
                            if resp.clicked() {
                                d.style = st;
                            }
                        }
                    });
                }
            });
            theme::section(ui, "Color, icon, place");
            theme::card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (k, (name, light, dark)) in banner::COLORS.iter().enumerate() {
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::click());
                        let p = ui.painter();
                        p.circle_filled(rect.center(), 13.0, egui::Color32::from_rgb(dark[0], dark[1], dark[2]));
                        p.circle_filled(rect.center() - egui::vec2(3.0, 3.0), 8.0, egui::Color32::from_rgb(light[0], light[1], light[2]));
                        if d.color == k {
                            p.circle_stroke(rect.center(), 15.0, egui::Stroke::new(2.0_f32, egui::Color32::WHITE));
                        }
                        if resp.on_hover_text(*name).clicked() {
                            d.color = k;
                        }
                    }
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
                    if ui.add(egui::Button::new("none").selected(d.icon.is_empty()).min_size(egui::vec2(36.0, 36.0))).clicked() {
                        d.icon.clear();
                    }
                    for key in banner::ICONS {
                        let Some(e) = maker::find_emoji(key) else { continue };
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::click());
                        if d.icon == key {
                            ui.painter().rect_filled(rect, 6.0, theme::ACCENT_DIM);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, 6.0, theme::CARD_HOVER);
                        }
                        if let Some(img) = banner::icon_image(key, &self.fetch, ctx) {
                            let tex = self.banner_previews.entry(format!("icon:{key}")).or_insert_with(|| {
                                let im = image::RgbaImage::from_raw(img.1, img.2, img.0).map(|i| image::DynamicImage::ImageRgba8(i).thumbnail(48, 48).to_rgba8());
                                im.map(|i| ctx.load_texture(format!("bicon:{key}"), egui::ColorImage::from_rgba_unmultiplied([i.width() as usize, i.height() as usize], i.as_raw()), egui::TextureOptions::LINEAR))
                            });
                            if let Some(t) = tex {
                                egui::Image::new(egui::load::SizedTexture::new(t.id(), egui::vec2(28.0, 28.0))).paint_at(ui, rect.shrink(4.0));
                            }
                        }
                        if resp.on_hover_text(&e.name).clicked() {
                            d.icon = key.to_string();
                        }
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Place");
                    ui.selectable_value(&mut d.place, banner::Place::Left, "Left");
                    ui.selectable_value(&mut d.place, banner::Place::Center, "Center");
                    ui.selectable_value(&mut d.place, banner::Place::Right, "Right");
                });
                ui.add(egui::Slider::new(&mut d.size, 0.1..=0.3).text("Size").show_value(false));
                ui.horizontal(|ui| {
                    ui.label("Stays");
                    for (v, l) in [(0u32, "until hidden"), (5, "5 s"), (10, "10 s"), (20, "20 s")] {
                        ui.selectable_value(&mut d.stay, v, l);
                    }
                });
            });
            ui.label(egui::RichText::new("Fonts: Lilita One, Poppins (SIL OFL), Luckiest Guy (Apache 2.0).").small().color(theme::TEXT_DIM));
            // Edited while on the picture: redraw it with the change (no new slide-in).
            if d != self.banners[i] {
                self.banners[i] = d.clone();
                if let Some(b) = &mut fx.banner {
                    if !b.is_leaving() {
                        let icon = if d.icon.is_empty() { None } else { banner::icon_image(&d.icon, &self.fetch, ctx) };
                        b.update(d, icon);
                    }
                }
            }
        });
    }
}
fn clock(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Play/pause, −10 s / +10 s and a scrub slider for the playing link / video. `scrub` holds
/// the slider position while it is being dragged (the jump happens on release).
fn transport_bar(ui: &mut egui::Ui, pb: &link::Playback, scrub: &mut Option<f64>) {
    ui.horizontal_centered(|ui| {
        let at_end = pb.duration.is_some_and(|d| pb.pos >= d - 1.0);
        let label = if pb.paused { "▶" } else { "⏸" };
        let play = ui
            .add(egui::Button::new(egui::RichText::new(label).size(16.0).color(egui::Color32::WHITE)).fill(theme::ACCENT).min_size(egui::vec2(38.0, 28.0)))
            .on_hover_text(if pb.paused { "Play (Space)" } else { "Pause (Space)" });
        let space = !ui.ctx().wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Space));
        if play.clicked() || space {
            if pb.paused && at_end {
                link::seek(0.0); // finished: play again from the start
            }
            link::set_paused(!pb.paused);
        }
        let Some(dur) = pb.duration.filter(|d| *d > 0.5) else {
            ui.label(egui::RichText::new("● LIVE").color(theme::LIVE).strong());
            if pb.buffering {
                spinner(ui);
            }
            return;
        };
        if ui.button("⟲ 10").on_hover_text("Back 10 seconds").clicked() {
            link::seek((pb.pos - 10.0).max(0.0));
        }
        if ui.button("10 ⟳").on_hover_text("Forward 10 seconds").clicked() {
            link::seek((pb.pos + 10.0).min(dur - 1.0));
        }
        let shown = scrub.unwrap_or(pb.pos);
        ui.label(egui::RichText::new(clock(shown)).monospace());
        let right = 60.0;
        let mut v = shown;
        ui.spacing_mut().slider_width = (ui.available_width() - right).max(60.0);
        let r = ui.add(egui::Slider::new(&mut v, 0.0..=dur).show_value(false).trailing_fill(true));
        if r.dragged() || r.changed() {
            *scrub = Some(v);
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            link::seek(v);
            *scrub = None;
        }
        ui.label(egui::RichText::new(clock(dur)).monospace().color(theme::TEXT_DIM));
        if pb.buffering && !pb.paused {
            spinner(ui);
        }
    });
}

/// With `mirror`, the image is flipped left↔right here (flipping the UVs instead paints a
/// white box with egui's rounded images).
fn preview_image(rgba: &[u8], w: u32, h: u32, shown: (f32, f32), mirror: bool) -> egui::ColorImage {
    let mut img = preview_image_unflipped(rgba, w, h, shown);
    if mirror {
        let ow = img.size[0];
        for row in img.pixels.chunks_exact_mut(ow.max(1)) {
            row.reverse();
        }
    }
    img
}

fn preview_image_unflipped(rgba: &[u8], w: u32, h: u32, shown: (f32, f32)) -> egui::ColorImage {
    let (w, h) = (w as usize, h as usize);
    let k = if shown.0 >= 1.0 && shown.1 >= 1.0 {
        ((w as f32 / shown.0).min(h as f32 / shown.1).floor() as usize).clamp(1, 4)
    } else {
        1
    };
    if k == 1 || rgba.len() < w * h * 4 {
        // Camera frames are opaque, so premultiplied == unmultiplied: a plain copy.
        return egui::ColorImage::from_rgba_premultiplied([w, h], rgba);
    }
    // Average a 2x2 block at each k-th pixel: 4 reads per output pixel instead of k², which
    // keeps this well under a millisecond; plenty for an on-screen preview.
    let (ow, oh) = (w / k, h / k);
    let mut pixels = Vec::with_capacity(ow * oh);
    for oy in 0..oh {
        let r0 = &rgba[oy * k * w * 4..(oy * k + 1) * w * 4];
        let r1 = &rgba[(oy * k + 1) * w * 4..(oy * k + 2) * w * 4];
        pixels.extend((0..ow).map(|ox| {
            let i = ox * k * 4;
            let avg = |c: usize| ((r0[i + c] as u16 + r0[i + 4 + c] as u16 + r1[i + c] as u16 + r1[i + 4 + c] as u16) / 4) as u8;
            egui::Color32::from_rgb(avg(0), avg(1), avg(2))
        }));
    }
    egui::ColorImage { size: [ow, oh], pixels }
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::WARN).init();
    // Formerly "OpenVCam": carry its settings / My Effects / models over (once).
    settings::migrate_old_dir();
    // The elevated half of the MarkCam Microphone setup (no window).
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--setup-audio" || a == "--rename-audio") {
        let dir = args.get(i + 1).map(PathBuf::from).unwrap_or_default();
        audio::elevated_setup(&dir, args[i] == "--setup-audio");
        return Ok(());
    }
    // One MarkCam at a time (a second launch, e.g. the shortcut while the autostarted one
    // runs, brings the running one to the front). Test runs are separate instances.
    if std::env::var_os("OVC_TEST").is_none() && std::env::var_os("OVC_SETTINGS").is_none() && !autostart::first_instance() {
        return Ok(());
    }
    // Installed with bundled effects: give a fresh user their "My effects" and categories.
    library::seed_from_bundle();
    // Keep yt-dlp current (weekly, in the background) so links keep working.
    if std::env::var_os("OVC_TEST").is_none() {
        thread::spawn(|| tools::update_ytdlp(false));
    }
    let mut viewport = egui::ViewportBuilder::default().with_inner_size([1240.0, 760.0]).with_min_inner_size([900.0, 560.0]);
    // Testing aid: OVC_WINDOW_POS=x,y places the window (e.g. off-screen for screenshots).
    if let Some((x, y)) = std::env::var("OVC_WINDOW_POS").ok().and_then(|s| {
        let (x, y) = s.split_once(',')?;
        Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
    }) {
        viewport = viewport.with_position([x, y]);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "MarkCam",
        options,
        Box::new(|cc| Ok(Box::new(Studio::new(cc.egui_ctx.clone()).expect("init MarkCam")))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adds and removes the real "start with Windows" entry (restores the previous state).
    #[test]
    #[ignore]
    fn autostart_roundtrip() {
        let before = autostart::enabled();
        autostart::set(true).unwrap();
        assert!(autostart::enabled());
        autostart::set(false).unwrap();
        assert!(!autostart::enabled());
        if before {
            autostart::set(true).unwrap();
        }
    }

    /// Prints the Windows default speakers / microphone and the virtual cable (diagnostics).
    #[test]
    #[ignore]
    fn audio_devices() {
        #[cfg(windows)]
        println!("defaults (speakers, mic): {:?}", audio::imp_defaults());
        #[cfg(windows)]
        for l in audio::probe_all() {
            println!("probe: {l}");
        }
        println!("fix_defaults: {:?}", audio::fix_defaults());
        #[cfg(windows)]
        println!("defaults now (speakers, mic): {:?}", audio::imp_defaults());
        println!("cable: {:?}, named: {}", audio::cable().map(|c| (c.0.name, c.1.name)), audio::cable_named());
        println!("mics: {:?}", audio::microphones().iter().map(|m| m.name.clone()).collect::<Vec<_>>());
    }

    /// Media sound really reaches the MarkCam Microphone: a near-inaudible 19.5 kHz tone
    /// goes through the engine into the cable and is heard back on the cable's mic end.
    /// Mic mixing and headphone monitoring are off (nothing reaches the user's ears).
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn audio_loopback() {
        let ctl = audio::Controls::new();
        ctl.mic_on.store(false, Ordering::SeqCst);
        ctl.monitor.store(false, Ordering::SeqCst);
        audio::Controls::set(&ctl.media_gain, 1.0);
        let engine = audio::Engine::start(ctl.clone(), None, true);
        std::thread::sleep(std::time::Duration::from_millis(400));
        println!("engine: {}", engine.status.lock().unwrap());
        let feeder = std::thread::spawn(|| {
            let mut t = 0usize;
            for _ in 0..30 {
                let block: Vec<f32> = (0..960)
                    .flat_map(|i| {
                        let s = 0.2 * ((t + i) as f32 * 19_500.0 * std::f32::consts::TAU / 48_000.0).sin();
                        [s, s]
                    })
                    .collect();
                t += 960;
                audio::push_media(&block);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
        let heard = audio::listen_peak(500).unwrap();
        feeder.join().unwrap();
        println!("peak heard on MarkCam Microphone: {heard:.3} (sent 0.200)");
        drop(engine);
        assert!(heard > 0.05, "nothing came through the virtual cable");
    }

    /// End-to-end link playback: fetches yt-dlp + ffmpeg if missing, plays a short public
    /// YouTube video, checks frames and audio arrive (network; ignored by default).
    #[test]
    #[ignore]
    fn link_plays() {
        if !tools::ready() {
            let f = tools::Fetch::start();
            while f.active.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            assert!(tools::ready(), "{}", f.status.lock().unwrap());
        }
        audio::clear_media();
        let t_open = std::time::Instant::now();
        let mut src = link::LinkSource::open_url("https://www.youtube.com/watch?v=jNQXAC9IVRw", 360).expect("open link");
        let mut frames = 0;
        let mut nonblack = false;
        let mut first: Option<std::time::Instant> = None;
        let mut last = std::time::Instant::now();
        let mut worst_gap = 0f32;
        while frames < 120 {
            let Some(f) = src.next_frame().expect("frame") else { continue }; // buffering
            let now = std::time::Instant::now();
            if first.is_none() {
                first = Some(now);
            } else {
                worst_gap = worst_gap.max((now - last).as_secs_f32() * 1000.0);
            }
            last = now;
            assert_eq!((f.format.width, f.format.height), (1280, 720));
            nonblack |= f.data.chunks_exact(4).step_by(997).any(|p| p[0] > 40 || p[1] > 40);
            frames += 1;
        }
        let startup = (first.unwrap() - t_open).as_secs_f32();
        let fps = 119.0 / (last - first.unwrap()).as_secs_f32();
        let audio = audio::media_ring().lock().unwrap().len();
        println!("first frame after {startup:.1}s, then {fps:.1} fps, worst gap {worst_gap:.0} ms, {audio} audio samples queued, non-black: {nonblack}");
        assert!(nonblack, "picture is black");
        assert!(audio > 0, "no audio arrived");
        assert!(worst_gap < 60.0, "stutter: a {worst_gap:.0} ms gap");

        // Download the same clip (into a temp USERPROFILE so nothing lands in the user's Videos).
        let tmp = std::env::temp_dir().join("markcam-dl-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("USERPROFILE", &tmp);
        let d = link::Download::start("https://www.youtube.com/watch?v=jNQXAC9IVRw", 360);
        while d.active.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        let file = d.file.lock().unwrap().clone();
        println!("download: {} → {:?} ({} bytes)", d.status.lock().unwrap(), file, file.as_ref().and_then(|f| std::fs::metadata(f).ok()).map_or(0, |m| m.len()));
        assert!(file.is_some_and(|f| f.is_file()), "download failed");
    }

    /// Higher quality + the transport: plays a long YouTube video (OVC_LINK_URL, default Big
    /// Buck Bunny) at OVC_LINK_Q (default 720), measures smoothness, then seeks, pauses and
    /// resumes (network; ignored by default).
    #[test]
    #[ignore]
    fn link_transport() {
        assert!(tools::ready(), "video tools missing");
        let url = std::env::var("OVC_LINK_URL").unwrap_or_else(|_| "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into());
        let q: u32 = std::env::var("OVC_LINK_Q").ok().and_then(|s| s.parse().ok()).unwrap_or(720);
        let t_open = std::time::Instant::now();
        let mut src = link::LinkSource::open_url(&url, q).expect("open link");
        // Frames until `n` arrived (or `limit` passed): (startup s, fps, worst gap ms).
        let run = |src: &mut link::LinkSource, n: usize| {
            let t0 = std::time::Instant::now();
            let (mut first, mut last, mut worst, mut got) = (None::<std::time::Instant>, t0, 0f32, 0usize);
            while got < n && t0.elapsed().as_secs() < 40 {
                let Some(_f) = src.next_frame().expect("frame") else { continue };
                let now = std::time::Instant::now();
                if first.is_some() {
                    worst = worst.max((now - last).as_secs_f32() * 1000.0);
                } else {
                    first = Some(now);
                }
                last = now;
                got += 1;
            }
            let first = first.expect("no frames");
            ((first - t0).as_secs_f32(), (got - 1) as f32 / (last - first).as_secs_f32().max(0.001), worst)
        };
        let (_, fps, worst) = run(&mut src, 300);
        let pb = link::playback();
        println!(
            "{q}p: first frame after {:.1}s, 10 s at {fps:.1} fps, worst gap {worst:.0} ms, pos {:.1}/{:?}",
            t_open.elapsed().as_secs_f32() - 10.0,
            pb.pos,
            pb.duration
        );
        assert!(pb.duration.is_some_and(|d| d > 60.0), "no duration");
        assert!(worst < 80.0, "stutter at {q}p: a {worst:.0} ms gap");

        link::seek(120.0);
        let (wait, _, _) = run(&mut src, 30);
        let pos = link::playback().pos;
        println!("seek to 120 s: picture after {wait:.1}s, now at {pos:.1}");
        assert!((120.0..124.0).contains(&pos), "seek landed at {pos}");

        link::set_paused(true);
        let t0 = std::time::Instant::now();
        let mut while_paused = 0;
        while t0.elapsed().as_millis() < 1500 {
            if src.next_frame().expect("frame").is_some() {
                while_paused += 1;
            }
        }
        let held = link::playback().pos;
        println!("paused: {while_paused} frames in 1.5 s, holding at {held:.1}");
        assert_eq!(while_paused, 0);

        link::set_paused(false);
        let (wait, fps, worst) = run(&mut src, 90);
        let pos = link::playback().pos;
        println!("resumed: picture after {wait:.1}s, {fps:.1} fps, worst gap {worst:.0} ms, at {pos:.1}");
        assert!(pos >= held && pos < held + 5.0, "resume jumped to {pos}");
    }

    /// A local video file (OVC_VIDEO_FILE): plays, knows its length, seeks, pauses.
    #[test]
    #[ignore]
    fn file_transport() {
        let path = std::path::PathBuf::from(std::env::var("OVC_VIDEO_FILE").expect("OVC_VIDEO_FILE"));
        let mut src = link::LinkSource::open_file(&path).expect("open file");
        let mut next = |src: &mut link::LinkSource, n: usize| {
            let t0 = std::time::Instant::now();
            let mut got = 0;
            while got < n && t0.elapsed().as_secs() < 20 {
                if src.next_frame().expect("frame").is_some() {
                    got += 1;
                }
            }
            got
        };
        assert_eq!(next(&mut src, 45), 45);
        let pb = link::playback();
        println!("file: at {:.1} of {:?}", pb.pos, pb.duration);
        let d = pb.duration.expect("duration");
        let target = d * 0.5;
        link::seek(target);
        assert_eq!(next(&mut src, 10), 10);
        let pos = link::playback().pos;
        println!("seek to {target:.1}: now at {pos:.1}");
        assert!((pos - target).abs() < 1.5);
        link::set_paused(true);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let t0 = std::time::Instant::now();
        let mut frames = 0;
        while t0.elapsed().as_millis() < 800 {
            frames += src.next_frame().expect("frame").is_some() as u32;
        }
        assert_eq!(frames, 0, "frames while paused");
        link::set_paused(false);
        assert_eq!(next(&mut src, 10), 10);
        println!("resumed at {:.1}", link::playback().pos);
    }

    /// Visual check of face masks on a real photo: tracks the face in a ManyCam thumbnail
    /// and warps masks onto it, saving PNGs (path in OVC_MASK_OUT). Needs the face models.
    #[test]
    #[ignore]
    fn face_mask_visual() {
        let out = std::path::PathBuf::from(std::env::var("OVC_MASK_OUT").expect("OVC_MASK_OUT"));
        let models = find_models().expect("face models");
        let tracker = ovc_ml::face::FaceTracker::load(
            &models.join("version-RFB-320.onnx"),
            &models.join("2d106det.onnx"),
            ovc_ml::face::Lmk::Insight106,
        )
        .unwrap();
        let dir = std::path::PathBuf::from(std::env::var("APPDATA").unwrap()).join(r"ManyCam\Effects\Video\Face Masks");
        let mut thumb_zip = zip::ZipArchive::new(std::fs::File::open(dir.join("Kitty.mcv")).unwrap()).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut thumb_zip.by_name("images/thumbnail/large.jpg").unwrap(), &mut bytes).unwrap();
        let photo = image::load_from_memory(&bytes).unwrap().to_rgba8();
        let (w, h) = photo.dimensions();
        let lms = tracker.landmarks68(photo.as_raw(), w, h).unwrap().expect("a face");
        for name in ["Kitty", "Devil Mask", "Hellboy"] {
            let Ok(e) = ovc_mcam::load_mcv_file(&dir.join(format!("{name}.mcv"))) else { continue };
            let mut ov = overlay_from_effect(e);
            let mut frame = photo.as_raw().clone();
            if let Some(piv) = ov.face.clone() {
                let fr = &ov.frames[0];
                let dst: Vec<[f32; 2]> = lms.iter().map(|p| [p[0] * w as f32, p[1] * h as f32]).collect();
                ovc_ml::warp::warp_mask_onto(&mut frame, w, h, &fr.rgba, fr.w, fr.h, &piv, &dst, 1.0);
            } else {
                ov.composite_face(&mut frame, w, h, &lms);
            }
            image::RgbaImage::from_raw(w, h, frame).unwrap().save(out.join(format!("mask-{name}.png"))).unwrap();
        }
    }

    /// Face stickers land where they belong on a synthetic face (needs the user's ManyCam
    /// effects, hence ignored by default).
    #[test]
    #[ignore]
    fn face_stickers_placement() {
        let (w, h) = (1280u32, 720u32);
        // 68 landmarks: everything at the face center, eyes at y = 0.45, 0.12 w apart.
        let mut lms = vec![[0.5f32, 0.5f32]; 68];
        for p in &mut lms[36..42] {
            *p = [0.44, 0.45];
        }
        for p in &mut lms[42..48] {
            *p = [0.56, 0.45];
        }
        let dir = std::path::PathBuf::from(std::env::var("APPDATA").unwrap()).join(r"ManyCam\Effects\Video");
        for (file, expect) in [
            (r"Hats & Hair\GGlalal.mcv", "above"),
            (r"Eye Accessories\King Crown.mcv", "above"),
            (r"Eye Accessories\Moustache.mcv", "below"),
            (r"Eye Accessories\Summer Glasses.mcv", "at"),
        ] {
            let Ok(e) = ovc_mcam::load_mcv_file(&dir.join(file)) else { continue };
            let mut ov = overlay_from_effect(e);
            assert!(ov.anchor.is_some(), "{file} should be face-anchored");
            let mut frame = vec![0u8; (w * h * 4) as usize];
            ov.composite_face(&mut frame, w, h, &lms);
            let r = ov.drawn.expect("drawn");
            let cy = (r[1] + r[3]) / 2.0;
            println!("{file:40} center y {cy:.3} (eyes 0.450), height {:.3}", r[3] - r[1]);
            match expect {
                "above" => assert!(cy < 0.40, "{file} center {cy}"),
                "below" => assert!(cy > 0.47, "{file} center {cy}"),
                // The glasses image has pineapples on top, so its center sits above the eyes;
                // its lenses (at ~72% of the image height) land on the eye line.
                _ => {
                    let lens_y = r[1] + (r[3] - r[1]) * 0.72;
                    assert!((lens_y - 0.45).abs() < 0.03, "{file} lenses at {lens_y}")
                }
            }
        }
    }

    /// Per-frame cost of the CPU stages at 1080p (run with `--ignored --nocapture`).
    #[test]
    #[ignore]
    fn bench_1080p_stages() {
        let (w, h) = (1920u32, 1080u32);
        let src: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let mut dst = vec![0u8; src.len()];
        let cg = ColorGrade { params: ovc_effects::builtin::ColorGradeParams { brightness: 0.05, contrast: 1.1, saturation: 1.2, temperature: 0.1 } };
        let t = |name: &str, f: &mut dyn FnMut()| {
            f();
            let n = 60;
            let s = std::time::Instant::now();
            for _ in 0..n {
                f();
            }
            println!("{name:32} {:6.2} ms/frame", s.elapsed().as_secs_f64() * 1000.0 / n as f64);
        };
        t("color grade (fast, CPU)", &mut || cg.apply_fast(&src, &mut dst));
        t("preview full-size (old)", &mut || drop(preview_image(&src, w, h, (0.0, 0.0), false)));
        t("preview downscaled k=3 (new)", &mut || drop(preview_image(&src, w, h, (545.0, 306.0), false)));
        t("8 MB copy (vcam publish)", &mut || dst.copy_from_slice(&src));
        let look = cg.matrix().then(&Filter::Vintage.matrix());
        t("grade + filter (one matrix)", &mut || look.apply(&src, &mut dst));
        for kind in [WarpKind::Bulge, WarpKind::Swirl, WarpKind::Fisheye, WarpKind::Wave, WarpKind::MirrorLeft, WarpKind::Pixelate] {
            // The center moves every frame, like when following a face.
            let mut r = ovc_effects::cpu::WarpRenderer::default();
            let mut n = 0.0f32;
            t(&format!("distortion {}", kind.name()), &mut || {
                n += 1.0;
                let warp = Warp { kind, cx: 0.45 + (n * 0.37).sin() * 0.05, cy: 0.45, radius: 0.35, strength: 0.7 };
                r.apply(&warp, &src, &mut dst, w, h, 1.0)
            });
        }
        for kind in AnimKind::ALL {
            let mut a = ovc_effects::anim::Anim::new(kind);
            t(&format!("animated {}", kind.name()), &mut || a.render(&mut dst, w, h, 1.0 / 30.0, 0.7));
        }
        let mut vig = ovc_effects::cpu::Vignette::default();
        t("vignette", &mut || vig.apply(&mut dst, w, h, 0.5));
    }
}
