//! Linux backend. Simplest of the three: write frames straight into a `v4l2loopback`
//! device node (`/dev/videoN`), which the kernel presents as a capture device to apps.
//! No separate user-space consumer is needed, so the ring is optional here.

use ovc_core::{CpuFrame, VideoFormat};

use crate::VirtualCamera;

pub struct LinuxVCam {
    // device: std::fs::File, // /dev/videoN opened O_WRONLY
    fmt: Option<VideoFormat>,
}

impl LinuxVCam {
    pub fn new() -> anyhow::Result<Self> {
        // TODO: discover a v4l2loopback node (modprobe v4l2loopback) and open it.
        Ok(Self { fmt: None })
    }
}

impl VirtualCamera for LinuxVCam {
    fn negotiate(&mut self, preferred: &[VideoFormat]) -> anyhow::Result<VideoFormat> {
        preferred
            .first()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("no preferred formats offered"))
    }
    fn start(&mut self, fmt: VideoFormat) -> anyhow::Result<()> {
        // TODO: VIDIOC_S_FMT with fmt; then write() frames.
        self.fmt = Some(fmt);
        Ok(())
    }
    fn submit(&mut self, _frame: &CpuFrame) -> anyhow::Result<()> {
        todo!("write frame bytes to the v4l2loopback fd")
    }
    fn stop(&mut self) {}
}
