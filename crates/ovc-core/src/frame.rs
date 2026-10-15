//! Frames: the unit of data flowing through the graph.
//!
//! A frame is **GPU-resident by default**. It is only materialized on the CPU at a
//! sink that needs system memory (the virtual-camera boundary), which is also where
//! GPU→NV12 color conversion + readback happen in one step.

use std::sync::Arc;

use crate::format::{PixelFormat, VideoFormat};
use crate::time::Timestamp;

/// Opaque handle to a GPU texture owned by `ovc-gpu`. Kept abstract here so this
/// crate stays free of `wgpu`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureHandle(pub u64);

/// A frame living in system memory.
#[derive(Debug, Clone)]
pub struct CpuFrame {
    pub format: VideoFormat,
    pub pts: Timestamp,
    pub data: Arc<[u8]>,
}

/// A frame either on the GPU or on the CPU.
#[derive(Debug, Clone)]
pub enum Frame {
    Gpu {
        texture: TextureHandle,
        format: PixelFormat,
        width: u32,
        height: u32,
        pts: Timestamp,
    },
    Cpu(CpuFrame),
}

impl Frame {
    pub fn pts(&self) -> Timestamp {
        match self {
            Frame::Gpu { pts, .. } => *pts,
            Frame::Cpu(c) => c.pts,
        }
    }
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Frame::Gpu { width, height, .. } => (*width, *height),
            Frame::Cpu(c) => (c.format.width, c.format.height),
        }
    }
}

/// Cheap, reference-counted frame passed along graph edges.
pub type FrameRef = Arc<Frame>;
