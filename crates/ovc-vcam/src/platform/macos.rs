//! macOS backend. The native consumer is a CoreMediaIO **camera extension** (modern
//! replacement for DAL plugins) under `plugins/macos-cmio`, which maps the `ovc-ipc` ring
//! and vends frames as an `AVCaptureDevice`.

use ovc_core::{CpuFrame, VideoFormat};

use crate::VirtualCamera;

pub struct MacosVCam {
    fmt: Option<VideoFormat>,
}

impl MacosVCam {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { fmt: None })
    }
}

impl VirtualCamera for MacosVCam {
    fn negotiate(&mut self, preferred: &[VideoFormat]) -> anyhow::Result<VideoFormat> {
        preferred
            .first()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("no preferred formats offered"))
    }
    fn start(&mut self, fmt: VideoFormat) -> anyhow::Result<()> {
        self.fmt = Some(fmt);
        Ok(())
    }
    fn submit(&mut self, _frame: &CpuFrame) -> anyhow::Result<()> {
        todo!("publish into ring; extension picks it up via CMIO stream")
    }
    fn stop(&mut self) {}
}
