//! The output formats the filter advertises, and conversion to/from DirectShow's
//! `AM_MEDIA_TYPE` / `VIDEOINFOHEADER` / `VIDEO_STREAM_CONFIG_CAPS`.

use std::mem::{size_of, ManuallyDrop};

use windows::core::GUID;
use windows::Win32::Foundation::{BOOL, RECT, SIZE};
use windows::Win32::Graphics::Gdi::BITMAPINFOHEADER;
use windows::Win32::Media::DirectShow::VIDEO_STREAM_CONFIG_CAPS;
use windows::Win32::Media::MediaFoundation::{
    AM_MEDIA_TYPE, FORMAT_VideoInfo, MEDIASUBTYPE_NV12, MEDIASUBTYPE_RGB32, MEDIASUBTYPE_YUY2, MEDIATYPE_Video,
    VIDEOINFOHEADER,
};
use windows::Win32::System::Com::{CoTaskMemAlloc, CoTaskMemFree};

/// Output frame rate (fixed; the app side runs at the camera rate, typically 30).
pub const FPS: i64 = 30;
/// One frame in DirectShow's 100 ns units.
pub const FRAME_100NS: i64 = 10_000_000 / FPS;

/// Resolutions offered, most useful first (apps that just take the first entry get 720p).
pub const RESOLUTIONS: [(u32, u32); 3] = [(1280, 720), (1920, 1080), (640, 480)];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sub {
    Nv12,
    Yuy2,
    Rgb32,
}

impl Sub {
    pub const ALL: [Sub; 3] = [Sub::Nv12, Sub::Yuy2, Sub::Rgb32];

    pub fn guid(self) -> GUID {
        match self {
            Sub::Nv12 => MEDIASUBTYPE_NV12,
            Sub::Yuy2 => MEDIASUBTYPE_YUY2,
            Sub::Rgb32 => MEDIASUBTYPE_RGB32,
        }
    }

    fn from_guid(g: &GUID) -> Option<Sub> {
        Sub::ALL.into_iter().find(|s| s.guid() == *g)
    }

    fn bits(self) -> u16 {
        match self {
            Sub::Nv12 => 12,
            Sub::Yuy2 => 16,
            Sub::Rgb32 => 32,
        }
    }

    /// `biCompression`: the FOURCC for YUV, `BI_RGB` (0) for RGB32.
    fn compression(self) -> u32 {
        match self {
            Sub::Nv12 => u32::from_le_bytes(*b"NV12"),
            Sub::Yuy2 => u32::from_le_bytes(*b"YUY2"),
            Sub::Rgb32 => 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Format {
    pub w: u32,
    pub h: u32,
    pub sub: Sub,
}

/// Every advertised format, in preference order.
pub fn all() -> Vec<Format> {
    RESOLUTIONS
        .iter()
        .flat_map(|&(w, h)| Sub::ALL.into_iter().map(move |sub| Format { w, h, sub }))
        .collect()
}

/// `first` (if any) followed by the rest of [`all`], without duplicates.
pub fn ordered(first: Option<Format>) -> Vec<Format> {
    let mut v: Vec<Format> = first.into_iter().collect();
    v.extend(all().into_iter().filter(|f| Some(*f) != first));
    v
}

impl Format {
    pub fn image_size(&self) -> usize {
        let px = self.w as usize * self.h as usize;
        match self.sub {
            Sub::Nv12 => px * 3 / 2,
            Sub::Yuy2 => px * 2,
            Sub::Rgb32 => px * 4,
        }
    }

    fn video_info(&self) -> VIDEOINFOHEADER {
        VIDEOINFOHEADER {
            rcSource: RECT::default(),
            rcTarget: RECT::default(),
            dwBitRate: (self.image_size() as u64 * 8 * FPS as u64).min(u32::MAX as u64) as u32,
            dwBitErrorRate: 0,
            AvgTimePerFrame: FRAME_100NS,
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: self.w as i32,
                // Positive: YUV is always top-down; RGB32 is bottom-up (handled in convert).
                biHeight: self.h as i32,
                biPlanes: 1,
                biBitCount: self.sub.bits(),
                biCompression: self.sub.compression(),
                biSizeImage: self.image_size() as u32,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
        }
    }

    /// A media type whose `pbFormat` is `CoTaskMemAlloc`ed (the caller frees it — DirectShow
    /// convention — or we do via [`free_media_type`]).
    pub fn to_media_type(&self) -> AM_MEDIA_TYPE {
        let vih = self.video_info();
        // SAFETY: fresh allocation of exactly one VIDEOINFOHEADER, fully initialized below.
        let pb = unsafe { CoTaskMemAlloc(size_of::<VIDEOINFOHEADER>()) } as *mut VIDEOINFOHEADER;
        let cb = if pb.is_null() {
            0
        } else {
            unsafe { pb.write(vih) };
            size_of::<VIDEOINFOHEADER>() as u32
        };
        AM_MEDIA_TYPE {
            majortype: MEDIATYPE_Video,
            subtype: self.sub.guid(),
            bFixedSizeSamples: BOOL(1),
            bTemporalCompression: BOOL(0),
            lSampleSize: self.image_size() as u32,
            formattype: FORMAT_VideoInfo,
            pUnk: ManuallyDrop::new(None),
            cbFormat: cb,
            pbFormat: pb as *mut u8,
        }
    }

    /// Parse a complete media type we can produce. Any even size up to 1920x1080 is
    /// accepted (we scale); RGB32 must be bottom-up (positive height).
    pub fn from_media_type(mt: &AM_MEDIA_TYPE) -> Option<Format> {
        if mt.majortype != MEDIATYPE_Video || mt.formattype != FORMAT_VideoInfo {
            return None;
        }
        let sub = Sub::from_guid(&mt.subtype)?;
        if mt.pbFormat.is_null() || (mt.cbFormat as usize) < size_of::<VIDEOINFOHEADER>() {
            return None;
        }
        // SAFETY: checked non-null and large enough; read unaligned to be safe.
        let vih = unsafe { (mt.pbFormat as *const VIDEOINFOHEADER).read_unaligned() };
        let w = vih.bmiHeader.biWidth;
        let h = vih.bmiHeader.biHeight;
        if sub == Sub::Rgb32 && h < 0 {
            return None;
        }
        let h = h.unsigned_abs();
        if w <= 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || w > 1920 || h > 1080 {
            return None;
        }
        Some(Format { w: w as u32, h, sub })
    }

    /// Whether a possibly *partial* media type (null GUIDs = "any") admits this format.
    pub fn matches_partial(&self, mt: &AM_MEDIA_TYPE) -> bool {
        let null = GUID::zeroed();
        (mt.majortype == null || mt.majortype == MEDIATYPE_Video)
            && (mt.subtype == null || mt.subtype == self.sub.guid())
            && (mt.formattype == null
                || match Format::from_media_type(mt) {
                    Some(f) => f == *self,
                    None => false,
                })
    }

    pub fn caps(&self) -> VIDEO_STREAM_CONFIG_CAPS {
        let size = SIZE { cx: self.w as i32, cy: self.h as i32 };
        let bps = (self.image_size() as u64 * 8 * FPS as u64).min(i32::MAX as u64) as i32;
        VIDEO_STREAM_CONFIG_CAPS {
            guid: FORMAT_VideoInfo,
            VideoStandard: 0,
            InputSize: size,
            MinCroppingSize: size,
            MaxCroppingSize: size,
            CropGranularityX: 1,
            CropGranularityY: 1,
            CropAlignX: 1,
            CropAlignY: 1,
            MinOutputSize: size,
            MaxOutputSize: size,
            OutputGranularityX: 1,
            OutputGranularityY: 1,
            StretchTapsX: 0,
            StretchTapsY: 0,
            ShrinkTapsX: 0,
            ShrinkTapsY: 0,
            MinFrameInterval: FRAME_100NS,
            MaxFrameInterval: FRAME_100NS,
            MinBitsPerSecond: bps,
            MaxBitsPerSecond: bps,
        }
    }
}

/// `CoTaskMemAlloc` a whole `AM_MEDIA_TYPE` (for `GetFormat` / `GetStreamCaps` /
/// `IEnumMediaTypes::Next`, whose callers free it with `DeleteMediaType`).
pub fn alloc_media_type(f: &Format) -> *mut AM_MEDIA_TYPE {
    // SAFETY: fresh allocation, written in full before being handed out.
    unsafe {
        let p = CoTaskMemAlloc(size_of::<AM_MEDIA_TYPE>()) as *mut AM_MEDIA_TYPE;
        if !p.is_null() {
            p.write(f.to_media_type());
        }
        p
    }
}

/// Free the format block of a media type we built with [`Format::to_media_type`].
pub fn free_media_type(mt: &mut AM_MEDIA_TYPE) {
    // SAFETY: pbFormat came from CoTaskMemAlloc (or is null); pUnk is always None here.
    unsafe {
        if !mt.pbFormat.is_null() {
            CoTaskMemFree(Some(mt.pbFormat as *const _));
        }
        ManuallyDrop::drop(&mut mt.pUnk);
    }
    mt.pbFormat = std::ptr::null_mut();
    mt.cbFormat = 0;
}
