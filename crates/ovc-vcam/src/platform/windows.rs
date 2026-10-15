//! Windows backend — the **producer** side of the virtual camera.
//!
//! It publishes the app's final RGBA frames into the well-known `ovc-ipc` ring
//! ([`ovc_ipc::VCAM_RING_NAME`]). The **consumer** that makes apps like ClassIn/Zoom/OBS
//! list "OpenVCam" as a webcam is the DirectShow filter in `plugins/windows-dshow`, which
//! the app process loads and which opens the ring by name.

use ovc_core::{CpuFrame, PixelFormat, VideoFormat};
use ovc_ipc::{mapping_size, Producer, Region, RingConfig, VCAM_MAX_FRAME_BYTES, VCAM_RING_NAME, VCAM_SLOTS};

use crate::VirtualCamera;

pub struct WindowsVCam {
    producer: Option<Producer>,
    fmt: Option<VideoFormat>,
}

impl WindowsVCam {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { producer: None, fmt: None })
    }
}

impl VirtualCamera for WindowsVCam {
    fn negotiate(&mut self, preferred: &[VideoFormat]) -> anyhow::Result<VideoFormat> {
        // The filter converts to whatever the app asks for, so the ring always carries RGBA.
        preferred
            .iter()
            .find(|f| f.format == PixelFormat::Rgba8)
            .or_else(|| preferred.first())
            .copied()
            .ok_or_else(|| anyhow::anyhow!("no formats offered"))
    }

    fn start(&mut self, fmt: VideoFormat) -> anyhow::Result<()> {
        if fmt.format != PixelFormat::Rgba8 {
            anyhow::bail!("virtual camera ring carries RGBA8 only, got {:?}", fmt.format);
        }
        if fmt.frame_size() > VCAM_MAX_FRAME_BYTES as usize {
            anyhow::bail!("virtual camera supports up to 1920x1080, got {}x{}", fmt.width, fmt.height);
        }
        self.producer = None; // release the old ring before re-creating the header
        let region = Region::shared(VCAM_RING_NAME, mapping_size(VCAM_MAX_FRAME_BYTES, VCAM_SLOTS), true)?;
        let cfg = RingConfig {
            width: fmt.width,
            height: fmt.height,
            fourcc: fmt.format.fourcc().as_u32(),
            stride: fmt.format.stride(fmt.width),
            slot_size: VCAM_MAX_FRAME_BYTES,
            slot_count: VCAM_SLOTS,
            fps_num: fmt.fps_num,
            fps_den: fmt.fps_den,
        };
        self.producer = Some(Producer::create(region, cfg)?);
        self.fmt = Some(fmt);
        tracing::info!("virtual camera ring '{VCAM_RING_NAME}' ready ({fmt:?})");
        Ok(())
    }

    fn submit(&mut self, frame: &CpuFrame) -> anyhow::Result<()> {
        self.submit_bytes(&frame.data, frame.pts.as_nanos())
    }

    fn submit_bytes(&mut self, data: &[u8], pts_ns: i64) -> anyhow::Result<()> {
        let p = self.producer.as_mut().ok_or_else(|| anyhow::anyhow!("start() not called"))?;
        p.publish(data, pts_ns)?;
        Ok(())
    }

    fn consumer(&self) -> Option<(u32, u32, u32)> {
        self.producer.as_ref().map(|p| p.consumer())
    }

    fn stop(&mut self) {
        self.producer = None;
        self.fmt = None;
    }
}
