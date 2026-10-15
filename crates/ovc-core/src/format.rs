//! Pixel formats and negotiated video formats.

/// A FourCC code (e.g. `NV12`), stored as little-endian bytes for the wire protocol.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCc(pub [u8; 4]);

impl FourCc {
    pub const fn new(s: &[u8; 4]) -> Self {
        FourCc(*s)
    }
    pub const fn as_u32(self) -> u32 {
        u32::from_le_bytes(self.0)
    }
}

impl std::fmt::Debug for FourCc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.0))
    }
}

/// Pixel formats we move through the pipeline and expose to consuming apps.
///
/// The webcam-facing set (NV12/I420/YUY2) matches what the ManyCam filter advertised,
/// for maximum app compatibility; RGBA/BGRA are used internally on the GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 4:2:0, Y plane then interleaved UV. 12 bpp.
    Nv12,
    /// 4:2:0 planar, Y then U then V. 12 bpp.
    I420,
    /// 4:2:2 packed YUYV. 16 bpp.
    Yuy2,
    /// 8-bit RGBA, straight alpha. 32 bpp.
    Rgba8,
    /// 8-bit BGRA. 32 bpp.
    Bgra8,
}

impl PixelFormat {
    pub const fn fourcc(self) -> FourCc {
        match self {
            PixelFormat::Nv12 => FourCc::new(b"NV12"),
            PixelFormat::I420 => FourCc::new(b"I420"),
            PixelFormat::Yuy2 => FourCc::new(b"YUY2"),
            PixelFormat::Rgba8 => FourCc::new(b"RGBA"),
            PixelFormat::Bgra8 => FourCc::new(b"BGRA"),
        }
    }

    /// Bytes required for one `width`×`height` frame in this format.
    pub const fn frame_size(self, width: u32, height: u32) -> usize {
        let px = (width * height) as usize;
        match self {
            PixelFormat::Nv12 | PixelFormat::I420 => px * 3 / 2,
            PixelFormat::Yuy2 => px * 2,
            PixelFormat::Rgba8 | PixelFormat::Bgra8 => px * 4,
        }
    }

    /// Byte stride of plane 0 (no row padding).
    pub const fn stride(self, width: u32) -> u32 {
        match self {
            PixelFormat::Nv12 | PixelFormat::I420 => width,
            PixelFormat::Yuy2 => width * 2,
            PixelFormat::Rgba8 | PixelFormat::Bgra8 => width * 4,
        }
    }
}

/// A fully negotiated output format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoFormat {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
}

impl VideoFormat {
    pub fn frame_size(&self) -> usize {
        self.format.frame_size(self.width, self.height)
    }
    pub fn fps(&self) -> f64 {
        self.fps_num as f64 / self.fps_den.max(1) as f64
    }
}
