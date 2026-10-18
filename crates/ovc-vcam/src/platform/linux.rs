//! Linux backend: frames go straight into a `v4l2loopback` device node (`/dev/videoN`,
//! labelled "MarkCam"), which the kernel presents to apps (Zoom, Chrome, Firefox, OBS…)
//! as an ordinary webcam. No user-space consumer is needed.
//!
//! The device carries YUYV 4:2:2 (what UVC webcams send, so every app takes it); the
//! RGBA → YUYV conversion is BT.601 limited range. v4l2loopback reports how many apps are
//! streaming (a "client usage" event), so frames are only converted and written while
//! some app actually shows the camera — same as the Windows plugin's heartbeat.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use ovc_core::{CpuFrame, PixelFormat, VideoFormat};

use crate::VirtualCamera;

/// The card label MarkCam's device gets (`card_label=` in the module options).
pub const CARD_LABEL: &str = "MarkCam";

// ── V4L2 ioctls (x86_64 / aarch64 layouts) ───────────────────────────────────────────

const VIDIOC_G_FMT: libc::c_ulong = 0xc0d0_5604;
const VIDIOC_S_FMT: libc::c_ulong = 0xc0d0_5605;
const VIDIOC_DQEVENT: libc::c_ulong = 0x8088_5659;
const VIDIOC_SUBSCRIBE_EVENT: libc::c_ulong = 0x4020_565a;
const BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const FIELD_NONE: u32 = 1;
const COLORSPACE_SMPTE170M: u32 = 1;
const FOURCC_YUYV: u32 = u32::from_le_bytes(*b"YUYV");
const EVENT_SUB_FL_SEND_INITIAL: u32 = 1;
/// "Client usage" event: v4l2loopback 0.12.x uses the first private event number; later
/// versions moved it up. Both are tried.
const CLIENT_USAGE_EVENTS: [u32; 2] = [0x0800_0000, 0x0800_0000 + 0x08E0_0000 + 1];

/// `struct v4l2_format` with the `pix` member (208 bytes on 64-bit).
#[repr(C)]
#[derive(Clone, Copy)]
struct V4l2Format {
    typ: u32,
    _pad: u32,
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
    _rest: [u8; 200 - 48],
}

#[repr(C)]
struct V4l2EventSubscription {
    typ: u32,
    id: u32,
    flags: u32,
    reserved: [u32; 5],
}

/// `struct v4l2_event` (136 bytes on 64-bit).
#[repr(C)]
struct V4l2Event {
    typ: u32,
    _pad: u32,
    u: [u8; 64],
    pending: u32,
    sequence: u32,
    timestamp: [i64; 2],
    id: u32,
    reserved: [u32; 8],
    _pad2: u32,
}

const _: () = assert!(std::mem::size_of::<V4l2Format>() == 208);
const _: () = assert!(std::mem::size_of::<V4l2EventSubscription>() == 32);
const _: () = assert!(std::mem::size_of::<V4l2Event>() == 136);

/// The `/dev/videoN` node whose card label is "MarkCam", from sysfs (no device is opened,
/// so a real webcam is never touched).
pub fn find_device() -> Option<PathBuf> {
    let mut nodes: Vec<(u32, PathBuf)> = std::fs::read_dir("/sys/class/video4linux")
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n: u32 = name.strip_prefix("video")?.parse().ok()?;
            let label = std::fs::read_to_string(e.path().join("name")).ok()?;
            (label.trim() == CARD_LABEL).then(|| (n, PathBuf::from("/dev").join(name)))
        })
        .collect();
    nodes.sort();
    nodes.into_iter().next().map(|(_, p)| p)
}

pub struct LinuxVCam {
    dev: Option<File>,
    fmt: Option<VideoFormat>,
    /// The size the device really carries (it keeps its size while an app streams).
    out_w: u32,
    out_h: u32,
    yuyv: Vec<u8>,
    /// Apps streaming right now; `None` if the driver can't tell (then frames always go).
    clients: Option<u32>,
    beat: u32,
}

impl LinuxVCam {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { dev: None, fmt: None, out_w: 0, out_h: 0, yuyv: Vec::new(), clients: None, beat: 0 })
    }

    fn poll_events(&mut self) {
        let Some(dev) = &self.dev else { return };
        if self.clients.is_none() {
            return;
        }
        loop {
            // SAFETY: a zeroed v4l2_event the kernel fills in; the fd is non-blocking.
            let mut ev: V4l2Event = unsafe { std::mem::zeroed() };
            let r = unsafe { libc::ioctl(dev.as_raw_fd(), VIDIOC_DQEVENT as _, &mut ev) };
            if r != 0 {
                break;
            }
            if CLIENT_USAGE_EVENTS.contains(&ev.typ) {
                self.clients = Some(u32::from_ne_bytes([ev.u[0], ev.u[1], ev.u[2], ev.u[3]]));
            }
        }
    }
}

impl VirtualCamera for LinuxVCam {
    fn negotiate(&mut self, preferred: &[VideoFormat]) -> anyhow::Result<VideoFormat> {
        preferred
            .iter()
            .find(|f| f.format == PixelFormat::Rgba8)
            .or_else(|| preferred.first())
            .copied()
            .ok_or_else(|| anyhow::anyhow!("no formats offered"))
    }

    fn start(&mut self, fmt: VideoFormat) -> anyhow::Result<()> {
        if fmt.format != PixelFormat::Rgba8 {
            anyhow::bail!("the virtual camera takes RGBA8 frames, got {:?}", fmt.format);
        }
        self.stop();
        let path = find_device().ok_or_else(|| anyhow::anyhow!("the MarkCam camera device is missing (click \"Install virtual camera\")"))?;
        let dev = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        let (w, h) = (fmt.width & !1, fmt.height & !1);
        // SAFETY: plain-data structs passed to the documented V4L2 ioctls.
        let mut f: V4l2Format = unsafe { std::mem::zeroed() };
        f.typ = BUF_TYPE_VIDEO_OUTPUT;
        f.width = w;
        f.height = h;
        f.pixelformat = FOURCC_YUYV;
        f.field = FIELD_NONE;
        f.bytesperline = w * 2;
        f.sizeimage = w * h * 2;
        f.colorspace = COLORSPACE_SMPTE170M;
        let set = unsafe { libc::ioctl(dev.as_raw_fd(), VIDIOC_S_FMT as _, &mut f) };
        if set != 0 {
            // An app is still streaming the old size: keep writing that size.
            f = unsafe { std::mem::zeroed() };
            f.typ = BUF_TYPE_VIDEO_OUTPUT;
            if unsafe { libc::ioctl(dev.as_raw_fd(), VIDIOC_G_FMT as _, &mut f) } != 0 || f.width == 0 {
                // Seen with v4l2loopback-dkms 0.12.7 on kernel 7.0 after a reopen; reloading
                // the driver (the app's "Repair camera") fixes it.
                anyhow::bail!(
                    "the camera driver is stuck ({}: {}) — click \"Repair camera\"",
                    path.display(),
                    std::io::Error::last_os_error()
                );
            }
        }
        if f.pixelformat != FOURCC_YUYV {
            anyhow::bail!("{}: the device is busy with another format", path.display());
        }
        self.out_w = f.width & !1;
        self.out_h = f.height;
        self.yuyv = vec![0; self.out_w as usize * self.out_h as usize * 2];
        // How many apps are streaming (tells us when to send frames).
        self.clients = None;
        for typ in CLIENT_USAGE_EVENTS {
            let sub = V4l2EventSubscription { typ, id: 0, flags: EVENT_SUB_FL_SEND_INITIAL, reserved: [0; 5] };
            if unsafe { libc::ioctl(dev.as_raw_fd(), VIDIOC_SUBSCRIBE_EVENT as _, &sub) } == 0 {
                self.clients = Some(0);
                break;
            }
        }
        self.dev = Some(dev);
        self.fmt = Some(fmt);
        self.poll_events();
        tracing::info!("virtual camera {} ready ({}x{} YUYV)", path.display(), self.out_w, self.out_h);
        // One frame right away: with exclusive_caps the device becomes a camera only once
        // a picture has been written.
        let black = vec![0u8; w as usize * h as usize * 4];
        let _ = self.submit_bytes(&black, 0);
        Ok(())
    }

    fn submit(&mut self, frame: &CpuFrame) -> anyhow::Result<()> {
        self.submit_bytes(&frame.data, frame.pts.as_nanos())
    }

    fn submit_bytes(&mut self, data: &[u8], _pts_ns: i64) -> anyhow::Result<()> {
        let fmt = self.fmt.ok_or_else(|| anyhow::anyhow!("start() not called"))?;
        let (sw, sh) = (fmt.width as usize, fmt.height as usize);
        if data.len() < sw * sh * 4 {
            anyhow::bail!("frame too small");
        }
        rgba_to_yuyv(data, sw, sh, &mut self.yuyv, self.out_w as usize, self.out_h as usize);
        let dev = self.dev.as_mut().ok_or_else(|| anyhow::anyhow!("device closed"))?;
        match dev.write(&self.yuyv) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn consumer_mut(&mut self) -> Option<(u32, u32, u32)> {
        self.dev.as_ref()?;
        self.poll_events();
        match self.clients {
            // Some app streams: advance the beat (watched), report the size it gets.
            Some(n) if n > 0 => {
                self.beat = self.beat.wrapping_add(1).max(1);
                Some((self.out_w, self.out_h, self.beat))
            }
            // Nobody: the beat stands still, so the app stops sending after a moment.
            Some(_) => Some((0, 0, self.beat.max(1))),
            // The driver can't tell: always send.
            None => {
                self.beat = self.beat.wrapping_add(1).max(1);
                Some((0, 0, self.beat))
            }
        }
    }

    fn stop(&mut self) {
        self.dev = None;
        self.fmt = None;
        self.clients = None;
    }
}

/// RGBA (`sw`x`sh`) → YUYV (`dw`x`dh`, nearest-neighbour scaled when the sizes differ),
/// BT.601 limited range.
fn rgba_to_yuyv(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    if dw < 2 || dh == 0 || dst.len() < dw * dh * 2 {
        return;
    }
    let same = sw == dw && sh == dh;
    let xmap: Vec<usize> = (0..dw).map(|x| if same { x } else { (x * sw / dw).min(sw - 1) }).collect();
    for y in 0..dh {
        let sy = if same { y } else { (y * sh / dh).min(sh - 1) };
        let srow = &src[sy * sw * 4..(sy + 1) * sw * 4];
        let drow = &mut dst[y * dw * 2..(y + 1) * dw * 2];
        for (x, out) in drow.chunks_exact_mut(4).enumerate() {
            let a = &srow[xmap[2 * x] * 4..][..3];
            let b = &srow[xmap[2 * x + 1] * 4..][..3];
            let (r0, g0, b0) = (a[0] as i32, a[1] as i32, a[2] as i32);
            let (r1, g1, b1) = (b[0] as i32, b[1] as i32, b[2] as i32);
            let y0 = ((66 * r0 + 129 * g0 + 25 * b0 + 128) >> 8) + 16;
            let y1 = ((66 * r1 + 129 * g1 + 25 * b1 + 128) >> 8) + 16;
            let (r, g, bb) = (r0 + r1, g0 + g1, b0 + b1);
            let u = ((-38 * r - 74 * g + 112 * bb + 256) >> 9) + 128;
            let v = ((112 * r - 94 * g - 18 * bb + 256) >> 9) + 128;
            out[0] = y0 as u8;
            out[1] = u.clamp(16, 240) as u8;
            out[2] = y1 as u8;
            out[3] = v.clamp(16, 240) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuyv_colors() {
        // White, black, red, blue pixel pairs.
        let px = |c: [u8; 3]| [c[0], c[1], c[2], 255, c[0], c[1], c[2], 255];
        let mut src = Vec::new();
        for c in [[255, 255, 255], [0, 0, 0], [255, 0, 0], [0, 0, 255]] {
            src.extend(px(c));
        }
        let mut dst = vec![0u8; 8 * 2];
        rgba_to_yuyv(&src, 8, 1, &mut dst, 8, 1);
        assert_eq!(&dst[0..4], &[235, 128, 235, 128]);
        assert_eq!(&dst[4..8], &[16, 128, 16, 128]);
        assert_eq!((dst[8], dst[11]), (82, 240)); // red: Y 82, V high
        assert_eq!((dst[12], dst[13]), (41, 240)); // blue: Y 41, U high
    }

    #[test]
    fn scales_to_the_device_size() {
        let src = vec![255u8; 64 * 36 * 4];
        let mut dst = vec![0u8; 32 * 18 * 2];
        rgba_to_yuyv(&src, 64, 36, &mut dst, 32, 18);
        assert!(dst.chunks_exact(2).all(|p| p[0] == 235));
    }
}
