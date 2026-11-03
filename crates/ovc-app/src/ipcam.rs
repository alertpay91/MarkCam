//! IP cameras and phones as the picture: **ffmpeg** reads the camera's stream (RTSP, or an
//! HTTP MJPEG / H.264 stream like the ones phone camera apps serve) and decodes it into
//! 1280x720 RGBA frames. Unlike a link, nothing is buffered: only the newest frame is kept,
//! so the picture stays as close to live as the network allows. When the stream drops
//! (Wi-Fi hiccup, phone screen locked), ffmpeg is started again by itself and the last
//! picture stays on until the camera is back.

use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ovc_capture::{CamMode, CaptureSource};
use ovc_core::{CpuFrame, PixelFormat, Timestamp, VideoFormat};

const W: u32 = 1280;
const H: u32 = 720;
const FPS: u32 = 30;

/// How long the first picture may take (an RTSP camera can need a few seconds).
const FIRST_FRAME: Duration = Duration::from_secs(15);
/// No new frame for this long (after it was running) = the stream is gone: reconnect.
const STALL: Duration = Duration::from_secs(6);

#[cfg(windows)]
fn hidden(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
}
#[cfg(not(windows))]
fn hidden(cmd: &mut Command) -> &mut Command {
    cmd
}

/// What the camera is doing, for the status bar (`None` = showing the picture).
pub static STATUS: Mutex<Option<String>> = Mutex::new(None);

pub fn status() -> Option<String> {
    STATUS.lock().unwrap().clone()
}

/// The address as typed → a URL ffmpeg can open.
/// - no scheme → `http://` (what phone apps show: `192.168.1.23:8080`)
/// - an `http` address without a path → `/video`, the stream path of the common phone
///   camera apps
/// - a user name / password, when given, go into the address.
pub fn normalize(addr: &str, user: &str, pass: &str) -> String {
    let a = addr.trim();
    let (scheme, rest) = match a.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r.to_string()),
        None => ("http".to_string(), a.to_string()),
    };
    let (host, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.clone(), String::new()),
    };
    let path = if (scheme == "http" || scheme == "https") && (path.is_empty() || path == "/") { "/video".to_string() } else { path };
    let user = user.trim();
    let host = if !user.is_empty() && !host.contains('@') {
        if pass.is_empty() {
            format!("{}@{host}", encode(user))
        } else {
            format!("{}:{}@{host}", encode(user), encode(pass))
        }
    } else {
        host
    };
    format!("{scheme}://{host}{path}")
}

/// Percent-encode the characters that would break a URL's user / password part.
fn encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '@' | ':' | '/' | '?' | '#' | '%' | ' ' | '[' | ']' => format!("%{:02X}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

/// The address without user name / password, for showing on screen.
pub fn display(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let rest = match rest.find('/') {
        Some(i) => {
            let (host, path) = rest.split_at(i);
            format!("{}{path}", host.rsplit('@').next().unwrap_or(host))
        }
        None => rest.rsplit('@').next().unwrap_or(rest).to_string(),
    };
    if scheme.eq_ignore_ascii_case("http") {
        rest.trim_end_matches("/video").to_string()
    } else {
        format!("{scheme}://{rest}")
    }
}

/// The address without user name / password (what it is remembered by in the list).
fn without_credentials(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let (host, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    format!("{scheme}://{}{path}", host.rsplit('@').next().unwrap_or(host))
}

/// Same camera (ignoring the user name / password).
pub fn same_camera(a: &str, b: &str) -> bool {
    without_credentials(a).eq_ignore_ascii_case(&without_credentials(b))
}

/// ffmpeg's error → something a person can act on.
fn friendly(msg: &str) -> String {
    let m = msg.to_ascii_lowercase();
    if m.contains("401") || m.contains("unauthorized") {
        "the camera wants a user name and password (or they are wrong)".into()
    } else if m.contains("404") || m.contains("not found") {
        "the camera answered, but not at this address — check the part after the port (e.g. /video)".into()
    } else if m.contains("refused") || m.contains("-10061") {
        "the camera refused the connection — is the camera app running, and is the port right?".into()
    } else if m.contains("timed out")
        || m.contains("timeout")
        || m.contains("unreachable")
        || m.contains("no route")
        // Windows: ETIMEDOUT / WSAETIMEDOUT, as ffmpeg prints them
        || m.contains("-138")
        || m.contains("-10060") {
        "no answer from the camera — is it on, and on the same Wi-Fi as this computer?".into()
    } else if m.contains("invalid data") {
        "the camera answered, but not with a video stream — check the address".into()
    } else if msg.trim().is_empty() {
        "no picture from the camera".into()
    } else {
        msg.rsplit(" | ").next().unwrap_or(msg).trim().to_string()
    }
}

/// The newest frame and a counter, shared with the reader thread.
struct Shared {
    latest: Mutex<(u64, Option<Arc<[u8]>>)>,
    cv: Condvar,
    stop: AtomicBool,
    /// ffmpeg's error lines (the last few).
    last_msg: Mutex<String>,
}

pub struct IpCamSource {
    shared: Arc<Shared>,
    supervisor: Option<JoinHandle<()>>,
    format: VideoFormat,
    start: Instant,
    shown: u64,
}

impl IpCamSource {
    /// Connect to `url` and wait for the first picture; fails with a readable reason if
    /// none comes.
    pub fn open(url: &str) -> anyhow::Result<Self> {
        if !crate::tools::ffmpeg().is_file() {
            anyhow::bail!("ffmpeg is missing (needed for IP cameras)");
        }
        *STATUS.lock().unwrap() = Some("connecting…".into());
        let shared = Arc::new(Shared {
            latest: Mutex::new((0, None)),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            last_msg: Mutex::new(String::new()),
        });
        // First connection here, so a wrong address is reported right away.
        let mut child = spawn(url, &shared)?;
        let t0 = Instant::now();
        loop {
            if shared.latest.lock().unwrap().0 > 0 {
                break;
            }
            let exited = child.try_wait().ok().flatten().is_some();
            if exited || t0.elapsed() > FIRST_FRAME {
                let _ = child.kill();
                let _ = child.wait();
                std::thread::sleep(Duration::from_millis(100)); // let the error reader finish
                let msg = shared.last_msg.lock().unwrap().clone();
                *STATUS.lock().unwrap() = None;
                let why = if exited { friendly(&msg) } else { friendly("timed out") };
                anyhow::bail!("IP camera {}: {why}", display(url));
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        *STATUS.lock().unwrap() = None;
        let supervisor = std::thread::Builder::new().name("markcam-ipcam".into()).spawn({
            let (shared, url) = (shared.clone(), url.to_string());
            move || supervise(&url, &shared, child)
        })?;
        Ok(IpCamSource {
            shared,
            supervisor: Some(supervisor),
            format: VideoFormat { format: PixelFormat::Rgba8, width: W, height: H, fps_num: FPS, fps_den: 1 },
            start: Instant::now(),
            shown: 0,
        })
    }
}

/// Keep the stream going: watch for a stall, restart ffmpeg after it stops.
fn supervise(url: &str, shared: &Arc<Shared>, first: Child) {
    let mut child = Some(first);
    let mut last_seq = shared.latest.lock().unwrap().0;
    let mut last_change = Instant::now();
    let mut connected = true;
    while !shared.stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        let seq = shared.latest.lock().unwrap().0;
        if seq != last_seq {
            last_seq = seq;
            last_change = Instant::now();
            if !connected {
                connected = true;
                *STATUS.lock().unwrap() = None;
            }
        }
        let exited = child.as_mut().map_or(true, |c| c.try_wait().ok().flatten().is_some());
        let limit = if connected { STALL } else { FIRST_FRAME };
        if exited || last_change.elapsed() > limit {
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            connected = false;
            *STATUS.lock().unwrap() = Some("connection lost — reconnecting…".into());
            tracing::warn!("ip camera stopped ({}), reconnecting", shared.last_msg.lock().unwrap());
            // A short pause so a camera that is off isn't hammered.
            for _ in 0..10 {
                if shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            child = spawn(url, shared).ok();
            last_change = Instant::now();
        }
    }
    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Start ffmpeg on `url`; its frames go into `shared`.
fn spawn(url: &str, shared: &Arc<Shared>) -> anyhow::Result<Child> {
    let mut cmd = Command::new(crate::tools::ffmpeg());
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    // Low delay: don't buffer, don't wait long to look at the stream.
    cmd.args(["-fflags", "nobuffer", "-flags", "low_delay", "-probesize", "1000000", "-analyzeduration", "1000000"]);
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("rtsp") {
        // TCP: no lost packets (= no grey smears) on Wi-Fi; 5 s socket timeout.
        cmd.args(["-rtsp_transport", "tcp", "-timeout", "5000000"]);
    } else if lower.starts_with("http") {
        cmd.args(["-rw_timeout", "5000000"]);
    }
    cmd.args(["-hwaccel", "auto", "-i", url, "-an"]);
    cmd.args([
        "-vf",
        &format!("scale={W}:{H}:force_original_aspect_ratio=decrease,pad={W}:{H}:(ow-iw)/2:(oh-ih)/2"),
        "-fps_mode",
        "passthrough",
        "-pix_fmt",
        "rgba",
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = hidden(&mut cmd).spawn()?;
    let video = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no video pipe"))?;
    if let Some(err) = child.stderr.take() {
        let shared = shared.clone();
        std::thread::spawn(move || {
            for line in std::io::BufRead::split(std::io::BufReader::new(err), b'\n').map_while(Result::ok) {
                let l = String::from_utf8_lossy(&line).trim().to_string();
                if !l.is_empty() {
                    let mut m = shared.last_msg.lock().unwrap();
                    if m.len() > 2000 {
                        m.clear();
                    }
                    if !m.is_empty() {
                        m.push_str(" | ");
                    }
                    m.push_str(&l);
                }
            }
        });
    }
    std::thread::spawn({
        let shared = shared.clone();
        move || read_frames(video, &shared)
    });
    Ok(child)
}

fn read_frames(mut video: ChildStdout, shared: &Shared) {
    let n = (W * H * 4) as usize;
    loop {
        let mut buf = vec![0u8; n];
        if video.read_exact(&mut buf).is_err() || shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let mut l = shared.latest.lock().unwrap();
        l.0 += 1;
        l.1 = Some(Arc::from(buf.into_boxed_slice()));
        shared.cv.notify_all();
    }
}

impl CaptureSource for IpCamSource {
    fn format(&self) -> VideoFormat {
        self.format
    }

    fn modes(&self) -> Vec<CamMode> {
        vec![CamMode { width: W, height: H, fps: FPS }]
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<CpuFrame>> {
        let mut l = self.shared.latest.lock().unwrap();
        if l.0 == self.shown {
            l = self.shared.cv.wait_timeout(l, Duration::from_millis(100)).unwrap().0;
        }
        if l.0 == self.shown {
            return Ok(None); // nothing new (yet): the last picture stays
        }
        self.shown = l.0;
        let Some(data) = l.1.clone() else { return Ok(None) };
        drop(l);
        Ok(Some(CpuFrame { format: self.format, pts: Timestamp(self.start.elapsed().as_nanos() as i64), data }))
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for IpCamSource {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.supervisor.take() {
            let _ = h.join();
        }
        *STATUS.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(normalize("192.168.1.23:8080", "", ""), "http://192.168.1.23:8080/video");
        assert_eq!(normalize(" http://192.168.1.23:4747/ ", "", ""), "http://192.168.1.23:4747/video");
        assert_eq!(normalize("http://cam/mjpg/video.mjpg", "", ""), "http://cam/mjpg/video.mjpg");
        assert_eq!(normalize("rtsp://10.0.0.5:554", "admin", "p@ss:1"), "rtsp://admin:p%40ss%3A1@10.0.0.5:554");
        assert_eq!(normalize("RTSP://10.0.0.5/stream1", "admin", ""), "rtsp://admin@10.0.0.5/stream1");
        assert_eq!(display("http://u:p@192.168.1.23:8080/video"), "192.168.1.23:8080");
        assert_eq!(display("rtsp://admin:x@10.0.0.5:554/s1"), "rtsp://10.0.0.5:554/s1");
        assert!(same_camera("rtsp://a:b@10.0.0.5/s", "rtsp://10.0.0.5/s"));
        assert!(!same_camera("rtsp://10.0.0.5/s", "rtsp://10.0.0.6/s"));
    }

    /// A live stream (network; ignored by default): `OVC_IPCAM_URL=<url>` gets pictures for
    /// `OVC_IPCAM_SECS` seconds (default 5), reporting how many arrived. Stop and restart
    /// the stream meanwhile to see it reconnect.
    #[test]
    #[ignore]
    fn live_stream() {
        let url = std::env::var("OVC_IPCAM_URL").expect("OVC_IPCAM_URL");
        let secs: u64 = std::env::var("OVC_IPCAM_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
        let t_open = Instant::now();
        let mut src = IpCamSource::open(&url).expect("open");
        eprintln!("first picture after {:?}", t_open.elapsed());
        let t0 = Instant::now();
        let (mut n, mut last_report) = (0u32, Instant::now());
        while t0.elapsed() < Duration::from_secs(secs) {
            if let Some(f) = src.next_frame().expect("frame") {
                assert_eq!(f.data.len(), (W * H * 4) as usize);
                n += 1;
            }
            if last_report.elapsed() >= Duration::from_secs(1) {
                eprintln!("{:>3}s: {n} pictures, status {:?}", t0.elapsed().as_secs(), status());
                last_report = Instant::now();
            }
        }
        assert!(n > 0);
    }
}
