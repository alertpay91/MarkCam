//! Virtual-camera sink: one trait, three OS backends. The Rust side is always the
//! **producer** writing into the `ovc-ipc` ring; the native OS plugin is the consumer
//! that serves frames to apps (Zoom, OBS, browsers).

use ovc_core::{CpuFrame, VideoFormat};

pub mod platform;

/// A virtual camera device the engine pushes frames into.
pub trait VirtualCamera: Send {
    /// Pick an output format from what the installed plugin advertises.
    fn negotiate(&mut self, preferred: &[VideoFormat]) -> anyhow::Result<VideoFormat>;
    /// Create the shared ring and signal readiness.
    fn start(&mut self, fmt: VideoFormat) -> anyhow::Result<()>;
    /// Publish one frame (already in the negotiated pixel format) into the ring.
    fn submit(&mut self, frame: &CpuFrame) -> anyhow::Result<()>;
    /// Publish one frame from a borrowed buffer (avoids building a `CpuFrame` copy).
    fn submit_bytes(&mut self, _data: &[u8], _pts_ns: i64) -> anyhow::Result<()> {
        anyhow::bail!("submit_bytes is not supported by this backend yet")
    }
    /// What the OS plugin reports back: `(width, height, beat)`, where `beat` advances while
    /// some app is actually pulling frames. `None` if the backend can't tell.
    fn consumer(&self) -> Option<(u32, u32, u32)> {
        None
    }
    /// Like [`consumer`](Self::consumer), for backends that must read the OS to tell
    /// (v4l2loopback's event queue).
    fn consumer_mut(&mut self) -> Option<(u32, u32, u32)> {
        self.consumer()
    }
    fn stop(&mut self);
}

/// Open the virtual camera backend for the current OS.
pub fn open_default() -> anyhow::Result<Box<dyn VirtualCamera>> {
    platform::open()
}
