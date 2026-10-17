//! Virtual-camera output: publishing frames to the "MarkCam" device and installing the
//! DirectShow filter (`markcam_dshow.dll`, shipped next to the exe).

use std::collections::VecDeque;
#[cfg(windows)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ovc_core::{PixelFormat, VideoFormat};
use ovc_vcam::VirtualCamera;

/// File name of the DirectShow filter next to the exe.
#[cfg(windows)]
pub const DLL_NAME: &str = "markcam_dshow.dll";
/// Must match `CLSID_OPENVCAM` in `plugins/windows-dshow`.
#[cfg(windows)]
const CLSID_KEY: &str = r"HKLM\SOFTWARE\Classes\CLSID\{A17D78D7-3C5C-40B8-B6AF-2A1DBD964591}\InprocServer32";

/// Largest frame the virtual camera carries (see `ovc_ipc::VCAM_MAX_FRAME_BYTES`).
const MAX_W: u32 = 1920;
const MAX_H: u32 = 1080;

/// How long after the last frame an app pulled we still count it as watching.
const WATCH_WINDOW: Duration = Duration::from_secs(3);

/// How long a frame is held back so that one arriving late (webcams deliver in uneven
/// bursts: 48 ms, then 18 ms…) still goes out on the beat.
const SMOOTH_DELAY: Duration = Duration::from_millis(18);

/// Even output timing from uneven arrivals: a phase-locked timeline that follows the
/// average frame interval and only nudges its phase toward each arrival, so one late or
/// early frame doesn't move the beat.
struct Timeline {
    base: Option<Instant>,
    last: Option<Instant>,
    period: f64,
}

impl Timeline {
    fn new() -> Self {
        Timeline { base: None, last: None, period: 1.0 / 30.0 }
    }

    /// When the frame that arrived at `now` should go out.
    fn target(&mut self, now: Instant) -> Instant {
        if let Some(last) = self.last {
            let dt = (now - last).as_secs_f64().clamp(0.005, 0.2);
            self.period = self.period * 0.95 + dt * 0.05;
        }
        self.last = Some(now);
        let base = match self.base {
            Some(b) => {
                let pred = b + Duration::from_secs_f64(self.period);
                let err = secs_between(pred, now);
                // A gap (paused source, nobody watching) or a big jump: start over.
                if err.abs() > 3.0 * self.period {
                    now
                } else {
                    shift(pred, err * 0.1)
                }
            }
            None => now,
        };
        self.base = Some(base);
        base + SMOOTH_DELAY
    }
}

/// `to - from` in seconds, negative when `to` is earlier.
fn secs_between(from: Instant, to: Instant) -> f64 {
    if to >= from {
        (to - from).as_secs_f64()
    } else {
        -(from - to).as_secs_f64()
    }
}

fn shift(t: Instant, secs: f64) -> Instant {
    if secs >= 0.0 {
        t + Duration::from_secs_f64(secs)
    } else {
        t.checked_sub(Duration::from_secs_f64(-secs)).unwrap_or(t)
    }
}

type SharedCam = Arc<Mutex<Option<Box<dyn VirtualCamera>>>>;

struct PacerState {
    frames: VecDeque<(Instant, Vec<u8>)>,
    spare: Vec<Vec<u8>>,
    timeline: Timeline,
}

struct PacerShared {
    st: Mutex<PacerState>,
    cv: Condvar,
    stop: AtomicBool,
}

/// Publishes frames to the virtual camera on an even beat (its own thread).
struct Pacer {
    sh: Arc<PacerShared>,
    handle: Option<JoinHandle<()>>,
}

impl Pacer {
    fn start(cam: SharedCam) -> Self {
        let sh = Arc::new(PacerShared {
            st: Mutex::new(PacerState { frames: VecDeque::new(), spare: Vec::new(), timeline: Timeline::new() }),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let handle = std::thread::Builder::new()
            .name("vcam-pacer".into())
            .spawn({
                let sh = sh.clone();
                move || loop {
                    let (due, buf) = {
                        let mut st = sh.st.lock().unwrap();
                        while st.frames.is_empty() && !sh.stop.load(Ordering::SeqCst) {
                            st = sh.cv.wait_timeout(st, Duration::from_millis(200)).unwrap().0;
                        }
                        if sh.stop.load(Ordering::SeqCst) {
                            return;
                        }
                        st.frames.pop_front().expect("not empty")
                    };
                    let wait = due.saturating_duration_since(Instant::now()).min(Duration::from_millis(100));
                    if !wait.is_zero() {
                        std::thread::sleep(wait);
                    }
                    if let Some(c) = cam.lock().unwrap().as_mut() {
                        let _ = c.submit_bytes(&buf, 0);
                    }
                    sh.st.lock().unwrap().spare.push(buf);
                }
            })
            .ok();
        Pacer { sh, handle }
    }

    fn push(&self, rgba: &[u8]) {
        let mut st = self.sh.st.lock().unwrap();
        let due = st.timeline.target(Instant::now());
        let mut buf = match st.spare.iter().position(|b| b.len() == rgba.len()) {
            Some(i) => st.spare.swap_remove(i),
            None => vec![0u8; rgba.len()],
        };
        buf.copy_from_slice(rgba);
        st.frames.push_back((due, buf));
        // Never more than ~2 frames behind.
        while st.frames.len() > 2 {
            let (_, old) = st.frames.pop_front().expect("not empty");
            st.spare.push(old);
        }
        st.spare.truncate(3);
        self.sh.cv.notify_one();
    }

    /// Drop queued frames (e.g. the ring is being replaced).
    fn clear(&self) {
        let mut st = self.sh.st.lock().unwrap();
        let PacerState { frames, spare, timeline } = &mut *st;
        spare.extend(frames.drain(..).map(|(_, b)| b));
        *timeline = Timeline::new();
    }
}

impl Drop for Pacer {
    fn drop(&mut self) {
        self.sh.stop.store(true, Ordering::SeqCst);
        self.sh.cv.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// The virtual-camera output of the Studio.
pub struct Output {
    /// User toggle.
    pub on: bool,
    pub err: Option<String>,
    cam: SharedCam,
    pacer: Pacer,
    size: (u32, u32),
    beat: u32,
    beat_at: Instant,
    /// Size an app is currently pulling from "MarkCam", if any app is.
    consumer: Option<(u32, u32)>,
}

impl Output {
    pub fn new() -> Self {
        let cam: SharedCam = Arc::new(Mutex::new(None));
        let pacer = Pacer::start(cam.clone());
        // Off until the settings say otherwise: the capture thread runs before they are
        // applied, and a test instance must never touch the ring ClassIn is watching.
        Self { on: false, err: None, cam, pacer, size: (0, 0), beat: 0, beat_at: Instant::now(), consumer: None }
    }

    /// The size some app (ClassIn…) is pulling right now, if any.
    pub fn consumer(&self) -> Option<(u32, u32)> {
        self.consumer
    }

    /// Whether the installed plugin reports back at all (older builds don't), i.e. whether
    /// `consumer() == None` really means "no app is using it".
    pub fn plugin_reports(&self) -> bool {
        self.beat != 0
    }

    /// Release the ring (apps then show the placeholder).
    pub fn stop(&mut self) {
        self.pacer.clear();
        *self.cam.lock().unwrap() = None;
        self.consumer = None;
    }

    /// Hand one final RGBA frame to the virtual camera — but only while some app is
    /// actually pulling frames (the plugin's heartbeat), so an idle virtual camera costs
    /// no per-frame copy. The ring itself stays up so an app can start pulling any time.
    pub fn frame(&mut self, rgba: &[u8], w: u32, h: u32) {
        if !self.on {
            if self.cam.lock().unwrap().is_some() {
                self.stop();
            }
            return;
        }
        if w > MAX_W || h > MAX_H {
            self.err = Some(format!("virtual camera: {w}x{h} is above 1920x1080 — pick a lower resolution"));
            self.stop();
            return;
        }
        if self.cam.lock().unwrap().is_none() || self.size != (w, h) {
            self.stop();
            let started = ovc_vcam::open_default().and_then(|mut v| {
                let fmt = VideoFormat { format: PixelFormat::Rgba8, width: w, height: h, fps_num: 30, fps_den: 1 };
                v.start(fmt)?;
                Ok(v)
            });
            match started {
                Ok(v) => {
                    *self.cam.lock().unwrap() = Some(v);
                    self.size = (w, h);
                    self.err = None;
                    // Treat a fresh ring as watched for a moment so an app already
                    // streaming from the plugin gets frames before it reports back.
                    self.beat_at = Instant::now();
                }
                Err(e) => {
                    self.err = Some(format!("virtual camera: {e}"));
                    return;
                }
            }
        }
        if !self.watched() {
            return; // nobody is watching: skip the copy
        }
        // Published by the pacer thread, on an even beat.
        self.pacer.push(rgba);
    }

    /// Whether an app may be pulling frames, from the plugin's heartbeat (every plugin build
    /// shipped with an installer reports it; a fresh ring counts as watched for a moment).
    /// Also works between frames, so a paused webcam can be woken when an app starts using
    /// the camera.
    pub fn watched(&mut self) -> bool {
        if !self.on {
            return false;
        }
        let report = match self.cam.lock().unwrap().as_mut() {
            Some(cam) => cam.consumer_mut(),
            None => return true, // no ring yet: can't tell
        };
        let now = Instant::now();
        if let Some((cw, ch, beat)) = report {
            if beat != self.beat {
                self.beat = beat;
                self.beat_at = now;
                self.consumer = (cw > 0 && ch > 0).then_some((cw, ch));
            }
        }
        if now - self.beat_at > WATCH_WINDOW {
            self.consumer = None;
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uneven_arrivals_go_out_evenly() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        // A webcam at 30 fps that delivers in bursts: every 3rd frame 15 ms late.
        let mut prev = None;
        let mut worst = 0f64;
        for i in 0..120u64 {
            let late = if i % 3 == 0 { 15 } else { 0 };
            let at = t0 + Duration::from_micros(i * 33_333) + Duration::from_millis(late);
            // The pacer can't send a frame before it has it.
            let due = tl.target(at).max(at);
            if let Some(p) = prev.filter(|_| i > 30) {
                worst = worst.max((secs_between(p, due) * 1000.0 - 33.333).abs());
            }
            prev = Some(due);
        }
        assert!(worst < 4.0, "output interval off by up to {worst:.1} ms");
    }
}

#[cfg(windows)]
fn dll_path() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join(DLL_NAME))
}

/// Registration state of the DirectShow filter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstallState {
    /// Not registered: apps don't see the camera.
    Missing,
    /// Registered, but pointing at another DLL (an older install, e.g. from before the
    /// rename, or a moved folder): works, but should be re-registered to this one.
    Outdated,
    /// Registered to the DLL next to this exe.
    Current,
}

/// Where the registered filter DLL is and whether it is this exe's.
#[cfg(windows)]
pub fn install_state() -> InstallState {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(out) = std::process::Command::new("reg")
        .args(["query", CLSID_KEY, "/ve"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return InstallState::Missing;
    };
    if !out.status.success() {
        return InstallState::Missing;
    }
    // "    (Default)    REG_SZ    C:\path\to\the.dll"
    let text = String::from_utf8_lossy(&out.stdout);
    let registered = text
        .lines()
        .find_map(|l| l.split_once("REG_SZ").map(|(_, v)| v.trim().to_string()))
        .unwrap_or_default();
    let ours = dll_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    if !ours.is_empty() && registered.eq_ignore_ascii_case(&ours) {
        InstallState::Current
    } else {
        InstallState::Outdated
    }
}

/// Linux: whether the "MarkCam" v4l2loopback device exists (the package loads it at boot).
#[cfg(not(windows))]
pub fn install_state() -> InstallState {
    if ovc_vcam::platform::linux_device().is_some() {
        InstallState::Current
    } else {
        InstallState::Missing
    }
}

/// Register (or with `uninstall`, unregister) the filter via an elevated `regsvr32`
/// (one UAC prompt). Runs in the background; the returned flag clears when it finishes.
#[cfg(windows)]
pub fn install(uninstall: bool) -> Result<Arc<AtomicBool>, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let dll = dll_path().filter(|p| p.is_file()).ok_or_else(|| format!("{DLL_NAME} not found next to the exe"))?;
    let flag = if uninstall { "'/u', " } else { "" };
    let script = format!(
        "Start-Process -FilePath regsvr32.exe -Verb RunAs -Wait -ArgumentList {flag}'/s', '\"{}\"'",
        dll.display()
    );
    let busy = Arc::new(AtomicBool::new(true));
    let done = busy.clone();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        done.store(false, Ordering::SeqCst);
    });
    Ok(busy)
}

/// Linux: create the "MarkCam" device now (normally done at boot by the package) with one
/// password prompt (pkexec): v4l2loopback is (re)loaded with MarkCam's options from
/// /etc/modprobe.d. Uninstall unloads it.
#[cfg(not(windows))]
pub fn install(uninstall: bool) -> Result<Arc<AtomicBool>, String> {
    let opts = "devices=1 video_nr=42 card_label=MarkCam exclusive_caps=1";
    let script = if uninstall {
        "modprobe -r v4l2loopback".to_string()
    } else {
        format!("modprobe -r v4l2loopback 2>/dev/null; modprobe v4l2loopback {opts}")
    };
    let busy = Arc::new(AtomicBool::new(true));
    let done = busy.clone();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("pkexec").args(["sh", "-c", &script]).status();
        done.store(false, Ordering::SeqCst);
    });
    Ok(busy)
}
