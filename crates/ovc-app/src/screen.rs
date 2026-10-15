//! The screen as a video source: the whole screen or a user-picked area (e.g. a student's
//! video tile in ClassIn), captured with GDI at 30 fps including the mouse pointer — plus
//! the snipping-tool style area picker. Effects (face masks, moustaches…) then apply to
//! whatever is in that area, exactly as with the webcam.
//!
//! All coordinates are physical screen pixels (the capture and picker threads run
//! per-monitor-DPI aware), so areas are exact on scaled (125 % / 150 %) displays too.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ovc_capture::{CamMode, CaptureSource};
use ovc_core::{CpuFrame, PixelFormat, Timestamp, VideoFormat};

/// A screen rectangle in physical pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Frames are scaled to fit this (the virtual camera's sweet spot): small areas such as a
/// video tile are enlarged smoothly instead of being blown up blocky by the app later.
const OUT_W: u32 = 1280;
const OUT_H: u32 = 720;
const FPS: u32 = 30;

/// Output size for an area: fit inside 1280x720, keep the aspect, even dimensions.
fn out_size(a: Area) -> (u32, u32) {
    let k = (OUT_W as f32 / a.w.max(1) as f32).min(OUT_H as f32 / a.h.max(1) as f32);
    let w = ((a.w as f32 * k) as u32 & !1).max(2);
    let h = ((a.h as f32 * k) as u32 & !1).max(2);
    (w, h)
}

/// Bilinear resample of BGRA `src` (`sw`x`sh`) into RGBA `dst` (`dw`x`dh`), fixed point.
fn resample_bgra(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    if sw < 2 || sh < 2 || src.len() < sw * sh * 4 || dst.len() < dw * dh * 4 {
        return;
    }
    let xmap: Vec<(usize, u32)> = (0..dw)
        .map(|x| {
            let fx = ((x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).clamp(0.0, (sw - 2) as f32);
            (fx as usize, ((fx - fx.floor()) * 256.0) as u32)
        })
        .collect();
    for y in 0..dh {
        let fy = ((y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).clamp(0.0, (sh - 2) as f32);
        let (y0, ay) = (fy as usize, ((fy - fy.floor()) * 256.0) as u32);
        let (r0, r1) = (&src[y0 * sw * 4..(y0 + 1) * sw * 4], &src[(y0 + 1) * sw * 4..(y0 + 2) * sw * 4]);
        let drow = &mut dst[y * dw * 4..(y + 1) * dw * 4];
        for (d, &(x0, ax)) in drow.chunks_exact_mut(4).zip(&xmap) {
            let i = x0 * 4;
            for (c, s) in [(0usize, 2usize), (1, 1), (2, 0)] {
                let top = r0[i + s] as u32 * (256 - ax) + r0[i + 4 + s] as u32 * ax;
                let bot = r1[i + s] as u32 * (256 - ax) + r1[i + 4 + s] as u32 * ax;
                d[c] = ((top * (256 - ay) + bot * ay) >> 16) as u8;
            }
            d[3] = 255;
        }
    }
}

/// Captures a screen area at 30 fps.
pub struct ScreenSource {
    area: Area,
    format: VideoFormat,
    grab: imp::Grabber,
    bgra: Vec<u8>,
    start: Instant,
    next: Instant,
}

impl ScreenSource {
    pub fn open(area: Area) -> anyhow::Result<Self> {
        if area.w < 16 || area.h < 16 {
            anyhow::bail!("screen area is too small");
        }
        let (w, h) = out_size(area);
        let format = VideoFormat { format: PixelFormat::Rgba8, width: w, height: h, fps_num: FPS, fps_den: 1 };
        Ok(Self {
            area,
            format,
            grab: imp::Grabber::new(area.w, area.h)?,
            bgra: vec![0; area.w as usize * area.h as usize * 4],
            start: Instant::now(),
            next: Instant::now(),
        })
    }
}

impl CaptureSource for ScreenSource {
    fn format(&self) -> VideoFormat {
        self.format
    }

    fn modes(&self) -> Vec<CamMode> {
        vec![CamMode { width: self.format.width, height: self.format.height, fps: FPS }]
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<CpuFrame>> {
        // Pace to 30 fps.
        let now = Instant::now();
        if self.next > now {
            std::thread::sleep(self.next - now);
        }
        self.next = Instant::now().max(self.next) + Duration::from_secs(1) / FPS;

        self.grab.grab(self.area, &mut self.bgra)?;
        let (w, h) = (self.format.width as usize, self.format.height as usize);
        let mut rgba = vec![0u8; w * h * 4];
        resample_bgra(&self.bgra, self.area.w as usize, self.area.h as usize, &mut rgba, w, h);
        Ok(Some(CpuFrame {
            format: self.format,
            pts: Timestamp(self.start.elapsed().as_nanos() as i64),
            data: Arc::from(rgba.into_boxed_slice()),
        }))
    }

    fn stop(&mut self) {}
}

/// The primary monitor's full area (physical pixels).
pub fn primary_screen() -> Area {
    imp::primary_screen()
}

/// Show the snipping overlay and let the user drag out an area. Blocks until done;
/// `None` if cancelled (Esc / right click / too small).
pub fn pick_area() -> Option<Area> {
    imp::pick_area()
}

/// Hide (or show again) a window from screen capture, so capturing an area that overlaps
/// MarkCam itself doesn't create an endless mirror-in-a-mirror.
pub fn exclude_from_capture(hwnd: isize, exclude: bool) {
    imp::exclude_from_capture(hwnd, exclude)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::core::w;
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus};
    use windows::Win32::UI::WindowsAndMessaging::*;

    fn dpi_aware() {
        unsafe {
            let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }

    /// A memory DC + top-down 32-bit DIB the screen is blitted into.
    pub struct Grabber {
        mem: HDC,
        bmp: HBITMAP,
        old: HGDIOBJ,
        bits: *mut u8,
        w: u32,
        h: u32,
    }

    // The GDI objects are only used from the capture thread that owns the source.
    unsafe impl Send for Grabber {}

    impl Grabber {
        pub fn new(w: u32, h: u32) -> anyhow::Result<Self> {
            dpi_aware();
            unsafe {
                let screen = GetDC(None);
                let mem = CreateCompatibleDC(screen);
                let bi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w as i32,
                        biHeight: -(h as i32), // top-down
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut bits = std::ptr::null_mut();
                let bmp = CreateDIBSection(screen, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
                ReleaseDC(None, screen);
                let old = SelectObject(mem, bmp);
                Ok(Grabber { mem, bmp, old, bits: bits as *mut u8, w, h })
            }
        }

        /// Copy the area (with the mouse pointer drawn in) into `out` as BGRA.
        pub fn grab(&mut self, a: Area, out: &mut [u8]) -> anyhow::Result<()> {
            let n = self.w as usize * self.h as usize * 4;
            if out.len() < n {
                anyhow::bail!("buffer too small");
            }
            unsafe {
                let screen = GetDC(None);
                let ok = BitBlt(self.mem, 0, 0, self.w as i32, self.h as i32, screen, a.x, a.y, SRCCOPY);
                ReleaseDC(None, screen);
                ok?;
                // The pointer (the teacher points at things while sharing).
                let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
                if GetCursorInfo(&mut ci).is_ok() && ci.flags.0 & CURSOR_SHOWING.0 != 0 {
                    let mut ii = ICONINFO::default();
                    if GetIconInfo(HICON(ci.hCursor.0), &mut ii).is_ok() {
                        let (hx, hy) = (ii.xHotspot as i32, ii.yHotspot as i32);
                        let _ = DrawIconEx(self.mem, ci.ptScreenPos.x - a.x - hx, ci.ptScreenPos.y - a.y - hy, HICON(ci.hCursor.0), 0, 0, 0, None, DI_NORMAL);
                        if !ii.hbmMask.is_invalid() {
                            let _ = DeleteObject(ii.hbmMask);
                        }
                        if !ii.hbmColor.is_invalid() {
                            let _ = DeleteObject(ii.hbmColor);
                        }
                    }
                }
                let _ = GdiFlush();
                std::ptr::copy_nonoverlapping(self.bits, out.as_mut_ptr(), n);
            }
            Ok(())
        }
    }

    impl Drop for Grabber {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.mem, self.old);
                let _ = DeleteObject(self.bmp);
                let _ = DeleteDC(self.mem);
            }
        }
    }

    pub fn primary_screen() -> Area {
        dpi_aware();
        unsafe { Area { x: 0, y: 0, w: GetSystemMetrics(SM_CXSCREEN).max(16) as u32, h: GetSystemMetrics(SM_CYSCREEN).max(16) as u32 } }
    }

    pub fn exclude_from_capture(hwnd: isize, exclude: bool) {
        unsafe {
            let _ = SetWindowDisplayAffinity(HWND(hwnd as *mut _), if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE });
        }
    }

    // ── The area picker ─────────────────────────────────────────────────────────────

    struct Picker {
        /// Virtual-desktop origin and size.
        vx: i32,
        vy: i32,
        vw: i32,
        vh: i32,
        /// Frozen screenshot, and a darkened copy.
        shot: HDC,
        dim: HDC,
        drag: Option<(POINT, POINT)>,
        result: Option<Area>,
        done: bool,
    }

    fn rect_of(a: POINT, b: POINT) -> RECT {
        RECT { left: a.x.min(b.x), top: a.y.min(b.y), right: a.x.max(b.x), bottom: a.y.max(b.y) }
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Picker;
        if p.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let st = &mut *p;
        let pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
        match msg {
            WM_LBUTTONDOWN => {
                st.drag = Some((pt, pt));
                SetCapture(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some((a, _)) = st.drag {
                    st.drag = Some((a, pt));
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                if let Some((a, b)) = st.drag {
                    let r = rect_of(a, b);
                    if r.right - r.left >= 32 && r.bottom - r.top >= 32 {
                        // Window coords → screen coords; even size for the video pipeline.
                        st.result = Some(Area {
                            x: r.left + st.vx,
                            y: r.top + st.vy,
                            w: ((r.right - r.left) as u32) & !1,
                            h: ((r.bottom - r.top) as u32) & !1,
                        });
                        st.done = true;
                        let _ = DestroyWindow(hwnd);
                    } else {
                        st.drag = None;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                st.done = true;
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == 0x1B => {
                // Esc
                st.done = true;
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if let Ok(c) = LoadCursorW(None, IDC_CROSS) {
                    SetCursor(c);
                }
                LRESULT(1)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                // Draw into a back buffer to avoid flicker.
                let back = CreateCompatibleDC(hdc);
                let bb = CreateCompatibleBitmap(hdc, st.vw, st.vh);
                let old = SelectObject(back, bb);
                let _ = BitBlt(back, 0, 0, st.vw, st.vh, st.dim, 0, 0, SRCCOPY);
                if let Some((a, b)) = st.drag {
                    let r = rect_of(a, b);
                    let _ = BitBlt(back, r.left, r.top, r.right - r.left, r.bottom - r.top, st.shot, r.left, r.top, SRCCOPY);
                    let pen = CreatePen(PS_SOLID, 3, COLORREF(0x00FF_A82F)); // accent (BGR)
                    let op = SelectObject(back, pen);
                    let ob = SelectObject(back, GetStockObject(NULL_BRUSH));
                    let _ = Rectangle(back, r.left - 2, r.top - 2, r.right + 2, r.bottom + 2);
                    SelectObject(back, op);
                    SelectObject(back, ob);
                    let _ = DeleteObject(pen);
                    let label: Vec<u16> = format!(" {} × {} ", r.right - r.left, r.bottom - r.top).encode_utf16().collect();
                    SetBkColor(back, COLORREF(0x00FF_A82F));
                    SetTextColor(back, COLORREF(0x00FF_FFFF));
                    let _ = TextOutW(back, r.left, (r.top - 22).max(0), &label);
                }
                // Instructions, top center.
                let hint: Vec<u16> =
                    "  Drag to select the area to show in MarkCam  ·  Esc or right-click to cancel  ".encode_utf16().collect();
                SetBkColor(back, COLORREF(0x0030_2018));
                SetTextColor(back, COLORREF(0x00FF_FFFF));
                let mut sz = SIZE::default();
                let _ = GetTextExtentPoint32W(back, &hint, &mut sz);
                let _ = TextOutW(back, (st.vw - sz.cx) / 2, 24, &hint);
                let _ = BitBlt(hdc, 0, 0, st.vw, st.vh, back, 0, 0, SRCCOPY);
                SelectObject(back, old);
                let _ = DeleteObject(bb);
                let _ = DeleteDC(back);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            // Safety net: never leave a frozen full-screen overlay up for long.
            WM_TIMER => {
                st.done = true;
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                st.done = true;
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }

    pub fn pick_area() -> Option<Area> {
        // Its own thread: a private message loop, independent of the GUI's event loop.
        std::thread::spawn(|| unsafe { pick_area_thread() }).join().ok().flatten()
    }

    unsafe fn pick_area_thread() -> Option<Area> {
        dpi_aware();
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        if vw <= 0 || vh <= 0 {
            return None;
        }
        // Freeze the screen, plus a darkened copy.
        let screen = GetDC(None);
        let shot = CreateCompatibleDC(screen);
        let shot_bmp = CreateCompatibleBitmap(screen, vw, vh);
        let shot_old = SelectObject(shot, shot_bmp);
        let _ = BitBlt(shot, 0, 0, vw, vh, screen, vx, vy, SRCCOPY);
        let dim = CreateCompatibleDC(screen);
        let dim_bmp = CreateCompatibleBitmap(screen, vw, vh);
        let dim_old = SelectObject(dim, dim_bmp);
        let _ = BitBlt(dim, 0, 0, vw, vh, shot, 0, 0, SRCCOPY);
        let black = CreateSolidBrush(COLORREF(0));
        let bdc = CreateCompatibleDC(screen);
        let bbmp = CreateCompatibleBitmap(screen, 1, 1);
        let bold = SelectObject(bdc, bbmp);
        let _ = FillRect(bdc, &RECT { left: 0, top: 0, right: 1, bottom: 1 }, black);
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 140, AlphaFormat: 0 };
        let _ = AlphaBlend(dim, 0, 0, vw, vh, bdc, 0, 0, 1, 1, blend);
        SelectObject(bdc, bold);
        let _ = DeleteObject(bbmp);
        let _ = DeleteDC(bdc);
        let _ = DeleteObject(black);
        ReleaseDC(None, screen);

        let mut state = Box::new(Picker { vx, vy, vw, vh, shot, dim, drag: None, result: None, done: false });
        let hinst = GetModuleHandleW(None).ok()?;
        let class = w!("MarkCamAreaPicker");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc); // fails harmlessly if already registered
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("Select area"),
            WS_POPUP | WS_VISIBLE,
            vx,
            vy,
            vw,
            vh,
            None,
            None,
            hinst,
            None,
        )
        .ok()?;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut *state as *mut Picker as isize);
        SetTimer(hwnd, 1, 60_000, None);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
        let _ = InvalidateRect(hwnd, None, false);

        let mut msg = MSG::default();
        while !state.done && GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let _ = DestroyWindow(hwnd);

        SelectObject(shot, shot_old);
        let _ = DeleteObject(shot_bmp);
        let _ = DeleteDC(shot);
        SelectObject(dim, dim_old);
        let _ = DeleteObject(dim_bmp);
        let _ = DeleteDC(dim);
        state.result
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub struct Grabber;
    impl Grabber {
        pub fn new(_: u32, _: u32) -> anyhow::Result<Self> {
            anyhow::bail!("screen capture is Windows-only for now")
        }
        pub fn grab(&mut self, _: Area, _: &mut [u8]) -> anyhow::Result<()> {
            Ok(())
        }
    }
    pub fn primary_screen() -> Area {
        Area { x: 0, y: 0, w: 1280, h: 720 }
    }
    pub fn pick_area() -> Option<Area> {
        None
    }
    pub fn exclude_from_capture(_: isize, _: bool) {}
}
