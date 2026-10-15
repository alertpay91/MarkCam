//! Video files (MP4, MOV, WMV, AVI…) as effect layers, decoded by Windows Media Foundation
//! (built into Windows — no codecs to ship). Each video layer streams on its own thread,
//! looping, paced to the file's own timestamps, and publishes only its newest frame, so a
//! long clip never sits in memory.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Video file extensions Media Foundation can open out of the box.
pub const VIDEO_EXTENSIONS: [&str; 5] = ["mp4", "m4v", "mov", "wmv", "avi"];

pub fn is_video_file(p: &Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Longest side of decoded frames (bigger videos are scaled down by Media Foundation —
/// an effect layer never needs more, and it keeps decode + resize cheap).
const MAX_SIDE: u32 = 960;

/// The newest decoded frame: `(sequence, rgba, w, h)`.
type Latest = Arc<Mutex<Option<(u64, Arc<Vec<u8>>, u32, u32)>>>;

/// A looping video decoded in the background.
pub struct VideoStream {
    latest: Latest,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl VideoStream {
    /// Open `path` and start decoding. Waits (briefly) for the first frame so a file that
    /// can't be decoded fails here instead of showing an empty layer.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::open_max(path, MAX_SIDE)
    }

    /// Like [`open`](Self::open) with a custom cap on the longest side.
    pub fn open_max(path: &Path, max_side: u32) -> anyhow::Result<Self> {
        let latest: Latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let err: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let handle = std::thread::Builder::new().name("markcam-video".into()).spawn({
            let (latest, stop, err, path) = (latest.clone(), stop.clone(), err.clone(), path.to_path_buf());
            move || {
                if let Err(e) = imp::decode_loop(&path, &latest, &stop, max_side) {
                    *err.lock().unwrap() = Some(e.to_string());
                }
            }
        })?;
        let me = VideoStream { latest, stop, handle: Some(handle) };
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            if me.latest.lock().unwrap().is_some() {
                return Ok(me);
            }
            if let Some(e) = err.lock().unwrap().take() {
                anyhow::bail!("can't play video: {e}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        anyhow::bail!("video didn't start (unsupported format?)")
    }

    /// The newest frame `(sequence, rgba, w, h)`.
    pub fn latest(&self) -> Option<(u64, Arc<Vec<u8>>, u32, u32)> {
        self.latest.lock().unwrap().clone()
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// A video file as the main picture (instead of the webcam), looping; effects apply to it
/// like to the camera.
pub struct MediaSource {
    stream: VideoStream,
    seq: u64,
    format: ovc_core::VideoFormat,
    start: Instant,
}

impl MediaSource {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let stream = VideoStream::open_max(path, 1280)?;
        let (_, _, w, h) = stream.latest().ok_or_else(|| anyhow::anyhow!("no video frame"))?;
        let format =
            ovc_core::VideoFormat { format: ovc_core::PixelFormat::Rgba8, width: w, height: h, fps_num: 30, fps_den: 1 };
        Ok(Self { stream, seq: 0, format, start: Instant::now() })
    }
}

impl ovc_capture::CaptureSource for MediaSource {
    fn format(&self) -> ovc_core::VideoFormat {
        self.format
    }

    fn modes(&self) -> Vec<ovc_capture::CamMode> {
        vec![ovc_capture::CamMode { width: self.format.width, height: self.format.height, fps: 30 }]
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<ovc_core::CpuFrame>> {
        // Wait for the decoder's next frame (it paces itself to the file's timing).
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(200) {
            if let Some((seq, rgba, w, h)) = self.stream.latest() {
                if seq != self.seq {
                    self.seq = seq;
                    self.format.width = w;
                    self.format.height = h;
                    return Ok(Some(ovc_core::CpuFrame {
                        format: self.format,
                        pts: ovc_core::Timestamp(self.start.elapsed().as_nanos() as i64),
                        data: Arc::from(rgba.as_slice()),
                    }));
                }
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        Ok(None)
    }

    fn stop(&mut self) {}
}

/// The first frame of a video, shrunk for a thumbnail.
pub fn first_frame(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    let s = VideoStream::open(path).ok()?;
    let (_, rgba, w, h) = s.latest()?;
    let img = image::RgbaImage::from_raw(w, h, rgba.as_ref().clone())?;
    let t = image::DynamicImage::ImageRgba8(img).thumbnail(200, 150).to_rgba8();
    let (tw, th) = t.dimensions();
    Some((t.into_raw(), tw, th))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::core::{Interface, HSTRING, PROPVARIANT};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

    pub fn decode_loop(path: &Path, latest: &Latest, stop: &AtomicBool, max_side: u32) -> anyhow::Result<()> {
        unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
            let r = run(path, latest, stop, max_side);
            let _ = MFShutdown();
            if com {
                CoUninitialize();
            }
            r
        }
    }

    unsafe fn run(path: &Path, latest: &Latest, stop: &AtomicBool, max_side: u32) -> anyhow::Result<()> {
        // The source reader decodes and, with advanced video processing on, converts to
        // RGB32 and scales for us.
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.ok_or_else(|| anyhow::anyhow!("no attributes"))?;
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs)?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(FIRST_VIDEO, true)?;

        // Native size, capped.
        let native = reader.GetCurrentMediaType(FIRST_VIDEO)?;
        let fs = native.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (nw, nh) = ((fs >> 32) as u32, fs as u32);
        let k = (max_side as f32 / nw.max(nh).max(1) as f32).min(1.0);
        let (ow, oh) = (((nw as f32 * k) as u32) & !1, ((nh as f32 * k) as u32) & !1);

        let out = MFCreateMediaType()?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, ((ow as u64) << 32) | oh as u64)?;
        if reader.SetCurrentMediaType(FIRST_VIDEO, None, &out).is_err() {
            // Some decoders refuse scaling: take RGB32 at the native size instead.
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(FIRST_VIDEO, None, &out)?;
        }
        let cur = reader.GetCurrentMediaType(FIRST_VIDEO)?;
        let fs = cur.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((fs >> 32) as u32, fs as u32);
        if w == 0 || h == 0 {
            anyhow::bail!("video has no size");
        }

        let mut seq = 0u64;
        let mut loop_start = Instant::now();
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        while !stop.load(Ordering::SeqCst) {
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                // Loop.
                reader.SetCurrentPosition(&windows::core::GUID::zeroed(), &PROPVARIANT::from(0i64))?;
                loop_start = Instant::now();
                if seq == 0 {
                    anyhow::bail!("video has no frames");
                }
                continue;
            }
            let Some(sample) = sample else { continue };
            // Pace to the file's timestamps (100 ns units).
            let due = loop_start + Duration::from_nanos((ts.max(0) as u64) * 100);
            let now = Instant::now();
            if due > now {
                std::thread::sleep((due - now).min(Duration::from_millis(500)));
            }
            let buf = sample.ConvertToContiguousBuffer()?;
            let copied = match buf.cast::<IMF2DBuffer>() {
                // 2D buffers know their pitch (negative = bottom-up); row y is at
                // scanline0 + y * pitch either way.
                Ok(b2) => {
                    let (mut p0, mut pitch) = (std::ptr::null_mut(), 0i32);
                    b2.Lock2D(&mut p0, &mut pitch)?;
                    for y in 0..h as isize {
                        let row = p0.offset(y * pitch as isize);
                        bgrx_to_rgba(std::slice::from_raw_parts(row, w as usize * 4), &mut rgba[y as usize * w as usize * 4..][..w as usize * 4]);
                    }
                    b2.Unlock2D()?;
                    true
                }
                Err(_) => {
                    let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
                    buf.Lock(&mut p, None, Some(&mut len))?;
                    let ok = len as usize >= rgba.len();
                    if ok {
                        bgrx_to_rgba(std::slice::from_raw_parts(p, rgba.len()), &mut rgba);
                    }
                    buf.Unlock()?;
                    ok
                }
            };
            if copied {
                seq += 1;
                *latest.lock().unwrap() = Some((seq, Arc::new(rgba.clone()), w, h));
            }
        }
        Ok(())
    }

    fn bgrx_to_rgba(src: &[u8], dst: &mut [u8]) {
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = 255;
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn decode_loop(_: &Path, _: &Latest, _: &AtomicBool, _: u32) -> anyhow::Result<()> {
        anyhow::bail!("video layers are Windows-only for now")
    }
}
