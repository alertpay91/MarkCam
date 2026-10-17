//! Input sources: physical cameras, screen/window capture, media files.
//! Cameras go through `nokhwa` (v4l2 / AVFoundation / MSMF behind one API).

use std::sync::Arc;

use ovc_core::{CpuFrame, PixelFormat, Timestamp, VideoFormat};

/// Something that yields frames in system memory (the capture thread then uploads to GPU).
///
/// Not `Send`: some backends (e.g. nokhwa's `Camera`) are single-thread-affine. The current
/// pipeline drives capture on one thread; a future threaded design will hand frames across a
/// channel rather than moving the source.
pub trait CaptureSource {
    fn format(&self) -> VideoFormat;
    /// Block until the next frame is available (or return `Ok(None)` on end-of-stream).
    fn next_frame(&mut self) -> anyhow::Result<Option<CpuFrame>>;
    fn stop(&mut self);
    /// Capture modes this source can produce (resolution × fps). Lets the UI offer only
    /// what the device actually supports. Default: empty (unknown).
    fn modes(&self) -> Vec<CamMode> {
        Vec::new()
    }
}

/// A single capture mode a camera advertises: a resolution at a frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CamMode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// Descriptor returned by [`list_cameras`].
#[derive(Debug, Clone)]
pub struct CameraInfo {
    pub index: u32,
    pub name: String,
}

/// Enumerate physical cameras. Needs the `camera` feature; otherwise returns an empty list.
pub fn list_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    #[cfg(feature = "camera")]
    {
        use nokhwa::{query, utils::ApiBackend};
        let cams = query(ApiBackend::Auto).map_err(|e| anyhow::anyhow!("camera query failed: {e}"))?;
        Ok(cams
            .into_iter()
            .map(|c| CameraInfo { index: c.index().as_index().unwrap_or(0), name: c.human_name() })
            // Linux: MarkCam's own virtual camera is a v4l2loopback device like any webcam.
            .filter(|c| !(cfg!(target_os = "linux") && c.name.trim() == "MarkCam"))
            .collect())
    }
    #[cfg(not(feature = "camera"))]
    {
        Ok(Vec::new())
    }
}

/// A physical camera decoded to RGBA8. Constructible only with the `camera` feature.
pub struct CameraSource {
    format: VideoFormat,
    modes: Vec<CamMode>,
    #[cfg(feature = "camera")]
    cam: nokhwa::Camera,
    #[cfg(feature = "camera")]
    start: std::time::Instant,
    /// Recycled RGBA frame buffers. A buffer is reused once every consumer has dropped its
    /// `Arc`, so steady-state capture does no 8 MB allocations (and no fresh-page faults).
    #[cfg(feature = "camera")]
    pool: Vec<Arc<[u8]>>,
}

/// How many decoded frames may be alive at once (UI preview, capture slot, face tracker…)
/// before the pool starts allocating instead of recycling.
#[cfg(feature = "camera")]
const POOL_SIZE: usize = 4;

/// Decode one MJPEG frame into `dst` (RGBA8). Uses libjpeg-turbo's fast integer IDCT and
/// merged (non-"fancy") chroma upsampling + color conversion — roughly 30-40% cheaper than
/// the defaults, with no visible difference on webcam-quality JPEG.
#[cfg(feature = "camera")]
fn decode_mjpeg_into(jpeg: &[u8], dst: &mut [u8]) -> anyhow::Result<()> {
    let mut d = mozjpeg::Decompress::new_mem(jpeg)?;
    d.dct_method(mozjpeg::DctMethod::IntegerFast);
    d.do_fancy_upsampling(false);
    d.do_block_smoothing(false);
    let mut started = d.rgba()?;
    if started.min_flat_buffer_size() != dst.len() {
        anyhow::bail!("MJPEG frame is {}x{}, expected {} bytes", started.width(), started.height(), dst.len());
    }
    started.read_scanlines_into::<u8>(dst)?;
    // Pixels are already in `dst`; trailing-data hiccups common in webcam MJPEG don't matter.
    let _ = started.finish();
    Ok(())
}

impl CameraSource {
    /// Open camera `index` at the requested resolution and frame rate (MJPEG, which is what
    /// webcams stream at high fps). Falls back to the device's highest frame rate if that
    /// exact combination isn't available. The actual negotiated format is reported via
    /// [`CaptureSource::format`], and every supported mode via [`CaptureSource::modes`].
    #[cfg(feature = "camera")]
    pub fn open(index: u32, requested: VideoFormat) -> anyhow::Result<Self> {
        use nokhwa::{
            pixel_format::RgbAFormat,
            utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution},
            Camera,
        };
        // Ask for the requested resolution AND frame rate (MJPEG, widely supported at high
        // fps); fall back to the driver's highest frame rate if that combination isn't there.
        let res = Resolution::new(requested.width.max(16), requested.height.max(16));
        let want = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(
            CameraFormat::new(res, FrameFormat::MJPEG, requested.fps_num.max(1)),
        ));
        let mut cam = match Camera::new(CameraIndex::Index(index), want) {
            Ok(c) => c,
            Err(_) => Camera::new(
                CameraIndex::Index(index),
                RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
            )
            .map_err(|e| anyhow::anyhow!("open camera {index}: {e}"))?,
        };
        // Enumerate every advertised mode before streaming, so the UI can offer the real
        // resolution/fps choices this device supports. Best-effort: some backends error.
        let mut modes: Vec<CamMode> = cam
            .compatible_camera_formats()
            .map(|fmts| {
                fmts.into_iter()
                    .map(|f| CamMode { width: f.width(), height: f.height(), fps: f.frame_rate() })
                    .collect()
            })
            .unwrap_or_default();
        modes.sort_by(|a, b| (b.width, b.height, b.fps).cmp(&(a.width, a.height, a.fps)));
        modes.dedup();

        cam.open_stream().map_err(|e| anyhow::anyhow!("open stream: {e}"))?;
        let res = cam.resolution();
        let format = VideoFormat {
            format: PixelFormat::Rgba8,
            width: res.width(),
            height: res.height(),
            // The MSMF backend sometimes reports 1 here even while streaming at the
            // requested rate; fall back to what was asked for rather than show nonsense.
            fps_num: match cam.frame_rate() {
                0 | 1 => requested.fps_num.max(1),
                f => f,
            },
            fps_den: 1,
        };
        Ok(Self { format, modes, cam, start: std::time::Instant::now(), pool: Vec::new() })
    }

    #[cfg(not(feature = "camera"))]
    pub fn open(_index: u32, _requested: VideoFormat) -> anyhow::Result<Self> {
        anyhow::bail!("camera support not built in — rebuild with `--features camera`")
    }
}

impl CaptureSource for CameraSource {
    fn format(&self) -> VideoFormat {
        self.format
    }

    fn modes(&self) -> Vec<CamMode> {
        self.modes.clone()
    }

    #[cfg(feature = "camera")]
    fn next_frame(&mut self) -> anyhow::Result<Option<CpuFrame>> {
        use nokhwa::{pixel_format::RgbAFormat, utils::FrameFormat};
        let buf = self.cam.frame().map_err(|e| anyhow::anyhow!("capture frame: {e}"))?;
        let pts = Timestamp(self.start.elapsed().as_nanos() as i64);
        let res = buf.resolution();
        let len = res.width() as usize * res.height() as usize * 4;

        let data = if buf.source_frame_format() == FrameFormat::MJPEG {
            // Reuse a pooled buffer nobody else holds any more, else allocate a new one.
            let mut out = match self.pool.iter().position(|a| a.len() == len && Arc::strong_count(a) == 1) {
                Some(i) => self.pool.swap_remove(i),
                None => Arc::from(vec![0u8; len].into_boxed_slice()),
            };
            let dst = Arc::get_mut(&mut out).expect("pooled frame buffer is uniquely owned");
            decode_mjpeg_into(buf.buffer(), dst).map_err(|e| anyhow::anyhow!("decode frame: {e}"))?;
            self.pool.push(out.clone());
            if self.pool.len() > POOL_SIZE {
                self.pool.remove(0);
            }
            out
        } else {
            // Uncompressed formats (YUYV/NV12 fallbacks): nokhwa's converter is fine here.
            let img = buf.decode_image::<RgbAFormat>().map_err(|e| anyhow::anyhow!("decode frame: {e}"))?;
            Arc::from(img.into_raw().into_boxed_slice())
        };
        let format = VideoFormat { width: res.width(), height: res.height(), ..self.format };
        Ok(Some(CpuFrame { format, pts, data }))
    }

    #[cfg(not(feature = "camera"))]
    fn next_frame(&mut self) -> anyhow::Result<Option<CpuFrame>> {
        unreachable!("CameraSource is not constructible without the `camera` feature")
    }

    fn stop(&mut self) {
        #[cfg(feature = "camera")]
        {
            let _ = self.cam.stop_stream();
        }
    }
}

/// A synthetic animated source (RGBA8) that needs no hardware. Produces scrolling color
/// bars plus a bouncing box, so the whole `source → effects → sink` path can run and be
/// verified end-to-end. Timestamps are derived from the frame index and the target fps.
pub struct TestPatternSource {
    format: VideoFormat,
    frame: u64,
    /// Wall-clock start, so `next_frame` paces itself to the nominal fps like a camera.
    start: Option<std::time::Instant>,
}

impl TestPatternSource {
    /// `width`/`height` should be even. The source always emits RGBA8 regardless of the
    /// requested output format (conversion happens at the sink).
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Self {
        Self {
            format: VideoFormat { format: PixelFormat::Rgba8, width, height, fps_num, fps_den },
            frame: 0,
            start: None,
        }
    }
}

impl CaptureSource for TestPatternSource {
    fn format(&self) -> VideoFormat {
        self.format
    }

    fn modes(&self) -> Vec<CamMode> {
        [(640, 480), (1280, 720), (1920, 1080)]
            .into_iter()
            .flat_map(|(w, h)| [30, 60].map(move |fps| CamMode { width: w, height: h, fps }))
            .collect()
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<CpuFrame>> {
        // Block until this frame is due, like a real camera.
        let start = *self.start.get_or_insert_with(std::time::Instant::now);
        let due = start + std::time::Duration::from_secs_f64(self.frame as f64 / self.format.fps().max(1.0));
        if let Some(wait) = due.checked_duration_since(std::time::Instant::now()) {
            std::thread::sleep(wait);
        }

        let w = self.format.width as usize;
        let h = self.format.height as usize;
        let t = self.frame as f32 / self.format.fps().max(1.0) as f32;
        let mut buf = vec![0u8; w * h * 4];

        // 8 SMPTE-ish bars scrolling horizontally.
        const BARS: [[u8; 3]; 8] = [
            [255, 255, 255], [255, 255, 0], [0, 255, 255], [0, 255, 0],
            [255, 0, 255], [255, 0, 0], [0, 0, 255], [16, 16, 16],
        ];
        let scroll = ((t * 60.0) as usize) % w;
        // Bouncing box.
        let bw = w / 8;
        let bh = h / 8;
        let bx = (((t * 0.37).sin() * 0.5 + 0.5) * (w - bw) as f32) as usize;
        let by = (((t * 0.53).cos() * 0.5 + 0.5) * (h - bh) as f32) as usize;

        // Cheap enough for a slow laptop: the bar of each column once per frame, the 8
        // gradient-scaled colors once per row, then plain pixel stores.
        let bar_of: Vec<u8> = (0..w).map(|x| (((x + scroll) * 8 / w) % 8) as u8).collect();
        for (y, row) in buf.chunks_exact_mut(w * 4).enumerate() {
            // vertical brightness gradient
            let g = 128 + (127 * y / h.max(1)) as u32;
            let colors: [[u8; 4]; 8] = BARS.map(|c| [(c[0] as u32 * g / 255) as u8, (c[1] as u32 * g / 255) as u8, (c[2] as u32 * g / 255) as u8, 255]);
            for (px, &b) in row.chunks_exact_mut(4).zip(&bar_of) {
                px.copy_from_slice(&colors[b as usize]);
            }
            if y >= by && y < by + bh {
                for px in row[bx * 4..(bx + bw) * 4].chunks_exact_mut(4) {
                    px.copy_from_slice(&[250, 250, 250, 255]);
                }
            }
        }

        let dur = 1_000_000_000i64 * self.format.fps_den as i64 / self.format.fps_num.max(1) as i64;
        let pts = Timestamp(self.frame as i64 * dur);
        self.frame += 1;
        Ok(Some(CpuFrame { format: self.format, pts, data: Arc::from(buf.into_boxed_slice()) }))
    }

    fn stop(&mut self) {}
}
