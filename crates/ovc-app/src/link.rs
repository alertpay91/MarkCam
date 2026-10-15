//! Playing a link (YouTube, Twitch, Facebook… — live streams too) or a local video, with
//! sound, as the picture: **yt-dlp** turns a page link into stream URLs, **ffmpeg** decodes
//! them in real time into 1280x720 RGBA frames (stdout) and 48 kHz stereo audio (a local
//! TCP socket → [`crate::audio::push_media`], i.e. MarkCam Microphone + your headphones).

use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ovc_capture::{CamMode, CaptureSource};
use ovc_core::{CpuFrame, PixelFormat, Timestamp, VideoFormat};

const W: u32 = 1280;
const H: u32 = 720;
const FPS: u32 = 30;

#[cfg(windows)]
fn hidden(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
}
#[cfg(not(windows))]
fn hidden(cmd: &mut Command) -> &mut Command {
    cmd
}

/// Qualities offered for links (max picture height).
pub const QUALITIES: [u32; 4] = [360, 480, 720, 1080];

/// yt-dlp format choice for at most `max_h` lines: the largest picture up to that, in H.264
/// (VP9 / AV1 at 720p+ are heavy to decode on a laptop — that made higher qualities
/// stutter), and among equal ones ≤30 fps (MarkCam shows 30 fps anyway). A sort rather
/// than a filter, so a video without such a format still plays.
fn format_args(max_h: u32) -> [String; 4] {
    ["-f".into(), "bv*+ba/b".into(), "-S".into(), format!("res:{max_h},vcodec:h264,fps:30")]
}

/// Stream URLs for a page link: one (video+audio) or two (video, audio).
fn resolve(url: &str, max_h: u32) -> anyhow::Result<Vec<String>> {
    match resolve_once(url, max_h) {
        // YouTube changed something: update yt-dlp and try once more.
        Err(_) if crate::tools::update_ytdlp(true) => resolve_once(url, max_h),
        r => r,
    }
}

fn resolve_once(url: &str, max_h: u32) -> anyhow::Result<Vec<String>> {
    let out = hidden(Command::new(crate::tools::ytdlp()).args([
        "-g",
        "--no-playlist",
        "--no-warnings",
    ])
    .args(format_args(max_h))
    .arg(url))
    .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        anyhow::bail!("can't open this link: {}", line.trim_start_matches("ERROR: "));
    }
    let urls: Vec<String> =
        String::from_utf8_lossy(&out.stdout).lines().map(str::trim).filter(|l| l.starts_with("http")).map(String::from).collect();
    if urls.is_empty() {
        anyhow::bail!("no playable stream found for this link");
    }
    Ok(urls)
}

/// The folder picked in Setup, if any.
static DOWNLOADS: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// Where downloaded videos go: the folder picked in Setup, else `Videos\MarkCam`.
pub fn downloads_dir() -> std::path::PathBuf {
    if let Some(d) = DOWNLOADS.lock().unwrap().clone() {
        return d;
    }
    default_downloads_dir()
}

pub fn default_downloads_dir() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|h| std::path::PathBuf::from(h).join("Videos").join("MarkCam"))
        .unwrap_or_else(|| crate::settings::app_dir().join("Downloads"))
}

pub fn set_downloads_dir(dir: Option<std::path::PathBuf>) {
    *DOWNLOADS.lock().unwrap() = dir;
}

/// The most recently written .mp4/.mkv/.webm in `dir`, if written after `since`.
fn newest_since(dir: &std::path::Path, since: std::time::SystemTime) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| {
            let p = e.path();
            matches!(p.extension().and_then(|x| x.to_str()), Some("mp4" | "mkv" | "webm"))
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(t, _)| *t >= since)
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// Saving a link as an MP4 (yt-dlp, merged by ffmpeg) in the background.
pub struct Download {
    pub status: Arc<Mutex<String>>,
    pub active: Arc<AtomicBool>,
    /// The saved file, once done.
    pub file: Arc<Mutex<Option<std::path::PathBuf>>>,
}

impl Download {
    pub fn start(url: &str, max_h: u32) -> Self {
        let status = Arc::new(Mutex::new("starting download…".to_string()));
        let active = Arc::new(AtomicBool::new(true));
        let file = Arc::new(Mutex::new(None));
        let (url, st, act, fl) = (url.to_string(), status.clone(), active.clone(), file.clone());
        std::thread::spawn(move || {
            let r = (|| -> anyhow::Result<()> {
                let dir = downloads_dir();
                std::fs::create_dir_all(&dir)?;
                let started = std::time::SystemTime::now();
                let mut child = hidden(
                    Command::new(crate::tools::ytdlp())
                        // Titles with emoji / non-Latin letters must come back intact on the pipe.
                        .env("PYTHONIOENCODING", "utf-8")
                        .env("PYTHONUTF8", "1"),
                )
                .args([
                    "--encoding",
                    "utf-8",
                    "--no-playlist",
                    "--no-mtime",
                    "--newline",
                    "--no-simulate",
                    "--progress",
                    "--merge-output-format",
                    "mp4",
                    "--ffmpeg-location",
                    &crate::tools::ffmpeg_dir().to_string_lossy(),
                    "-o",
                    &dir.join("%(title).80s [%(height)sp].%(ext)s").to_string_lossy(),
                    "--print",
                    "after_move:filepath",
                ])
                .args(format_args(max_h))
                .arg(&url)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
                // Drain stderr on its own thread so a chatty yt-dlp can't block on a full pipe.
                let mut errpipe = child.stderr.take();
                let errs = std::thread::spawn(move || {
                    let mut err = String::new();
                    if let Some(e) = errpipe.as_mut() {
                        let mut b = Vec::new();
                        let _ = e.read_to_end(&mut b);
                        err = String::from_utf8_lossy(&b).into_owned();
                    }
                    err
                });
                let out = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no output"))?;
                for line in std::io::BufRead::split(std::io::BufReader::new(out), b'\n').map_while(Result::ok) {
                    let line = String::from_utf8_lossy(&line);
                    let l = line.trim();
                    if let Some(pct) = l.split_whitespace().find(|w| w.ends_with('%')) {
                        *st.lock().unwrap() = format!("downloading… {pct}");
                    } else if std::path::Path::new(l).is_file() {
                        *fl.lock().unwrap() = Some(std::path::PathBuf::from(l));
                    }
                }
                let ok = child.wait()?.success();
                let err = errs.join().unwrap_or_default();
                if ok && fl.lock().unwrap().is_none() {
                    // The path didn't come back readable: take the newest video saved since we started.
                    *fl.lock().unwrap() = newest_since(&dir, started);
                }
                if !ok || fl.lock().unwrap().is_none() {
                    let line = err
                        .lines()
                        .rev()
                        .find(|l| l.contains("ERROR"))
                        .or_else(|| err.lines().rev().find(|l| !l.trim().is_empty()))
                        .unwrap_or("yt-dlp stopped without saving a file")
                        .to_string();
                    anyhow::bail!("{}", line.trim_start_matches("ERROR: "));
                }
                Ok(())
            })();
            *st.lock().unwrap() = match r {
                Ok(()) => "downloaded".into(),
                Err(e) => format!("download failed: {e}"),
            };
            act.store(false, Ordering::SeqCst);
        });
        Download { status, active, file }
    }
}

/// Decoded frames waiting to be shown (filled by the reader thread, ~2.5 s max).
struct FrameQueue {
    q: Mutex<VecDeque<Arc<[u8]>>>,
    cv: Condvar,
    eof: AtomicBool,
}

const QUEUE_MAX: usize = 75; // 2.5 s at 30 fps
const PREBUFFER: usize = 15; // start / resume once 0.5 s is ready

/// Playback state shared with the UI: where the playing video is, how long it is, and the
/// pause / seek requests from the transport bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playback {
    /// A link / video source is on the picture.
    pub active: bool,
    /// Position in seconds.
    pub pos: f64,
    /// Length in seconds (`None` = live stream / unknown).
    pub duration: Option<f64>,
    pub paused: bool,
    /// Requested jump (seconds), consumed by the source.
    pub seek: Option<f64>,
    /// Waiting for frames after a start / seek / resume.
    pub buffering: bool,
    owner: u64,
}

pub static PLAYBACK: Mutex<Playback> =
    Mutex::new(Playback { active: false, pos: 0.0, duration: None, paused: false, seek: None, buffering: false, owner: 0 });

static NEXT_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Current playback state (for the UI).
pub fn playback() -> Playback {
    *PLAYBACK.lock().unwrap()
}

pub fn set_paused(paused: bool) {
    PLAYBACK.lock().unwrap().paused = paused;
}

pub fn seek(to: f64) {
    let mut p = PLAYBACK.lock().unwrap();
    p.seek = Some(to.max(0.0));
    p.pos = to.max(0.0);
}

/// One running ffmpeg: frames into a queue, sound to the mixer.
struct Running {
    child: Child,
    frames: Arc<FrameQueue>,
    stop: Arc<AtomicBool>,
    /// Parsed from ffmpeg's input info.
    duration: Arc<Mutex<Option<f64>>>,
    /// ffmpeg's last message (shown if it stops).
    last_msg: Arc<Mutex<String>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.frames.cv.notify_all();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct LinkSource {
    inputs: Vec<String>,
    looped: bool,
    run: Option<Running>,
    format: VideoFormat,
    start: Instant,
    /// When the next frame is due (steady 30 fps from the buffer).
    next_due: Option<Instant>,
    buffering: bool,
    /// Frames to wait for before (re)starting: half a second at the very start; after a
    /// network hiccup just one, so the picture never falls behind the sound.
    prebuffer: usize,
    /// Where the current ffmpeg run started (seconds) and how many frames it has shown.
    offset: f64,
    shown: u64,
    duration: Option<f64>,
    owner: u64,
}

impl LinkSource {
    /// A page link (YouTube…), resolved with yt-dlp, at most `max_h` lines.
    pub fn open_url(url: &str, max_h: u32) -> anyhow::Result<Self> {
        let urls = resolve(url, max_h)?;
        Self::new(urls, false)
    }

    /// A local video file, looping.
    pub fn open_file(path: &Path) -> anyhow::Result<Self> {
        Self::new(vec![path.to_string_lossy().into_owned()], true)
    }

    fn new(inputs: Vec<String>, looped: bool) -> anyhow::Result<Self> {
        let run = spawn(&inputs, looped, 0.0)?;
        let owner = NEXT_OWNER.fetch_add(1, Ordering::SeqCst);
        *PLAYBACK.lock().unwrap() =
            Playback { active: true, pos: 0.0, duration: None, paused: false, seek: None, buffering: true, owner };
        let format = VideoFormat { format: PixelFormat::Rgba8, width: W, height: H, fps_num: FPS, fps_den: 1 };
        Ok(LinkSource {
            inputs,
            looped,
            run: Some(run),
            format,
            start: Instant::now(),
            next_due: None,
            buffering: true,
            prebuffer: PREBUFFER,
            offset: 0.0,
            shown: 0,
            duration: None,
            owner,
        })
    }

    /// Seconds into the video of the last frame shown.
    fn position(&self) -> f64 {
        let p = self.offset + self.shown as f64 / FPS as f64;
        match self.duration {
            Some(d) if self.looped && d > 0.5 => p % d,
            Some(d) => p.min(d),
            None => p,
        }
    }

    /// (Re)start decoding at `at` seconds.
    fn restart(&mut self, at: f64) -> anyhow::Result<()> {
        self.run = None; // stop the old ffmpeg first
        crate::audio::clear_media();
        let at = match self.duration {
            Some(d) if d > 1.0 => at.clamp(0.0, d - 0.5),
            _ => at.max(0.0),
        };
        self.run = Some(spawn(&self.inputs, self.looped, at)?);
        self.offset = at;
        self.shown = 0;
        self.buffering = true;
        // After a seek a short buffer is enough (the stream is already open and close).
        self.prebuffer = 8;
        Ok(())
    }

    fn publish(&self, buffering: bool) {
        let mut p = PLAYBACK.lock().unwrap();
        if p.owner == self.owner {
            p.pos = self.position();
            p.duration = self.duration;
            p.buffering = buffering;
        }
    }
}

fn spawn(inputs: &[String], looped: bool, at: f64) -> anyhow::Result<Running> {
    // Audio arrives on a local socket ffmpeg connects to.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();

    let mut cmd = Command::new(crate::tools::ffmpeg());
    // "info" only for the input description (the duration); -nostats keeps it quiet after.
    cmd.args(["-hide_banner", "-loglevel", "info", "-nostats", "-nostdin"]);
    for i in inputs {
        if looped {
            cmd.args(["-stream_loop", "-1"]);
        }
        if i.starts_with("http") {
            cmd.args(["-user_agent", "Mozilla/5.0", "-reconnect", "1", "-reconnect_streamed", "1"]);
        }
        if at > 0.05 {
            cmd.args(["-ss", &format!("{at:.2}")]);
        }
        // Decode on the graphics chip when there is one (Intel Quick Sync on the laptop):
        // 720p/1080p then barely touch the CPU. ffmpeg falls back to software by itself.
        cmd.args(["-hwaccel", "auto"]);
        // Real-time pace, but grab 2 s ahead at the start and catch up after a network
        // hiccup — the buffer that keeps playback smooth.
        cmd.args(["-readrate", "1", "-readrate_initial_burst", "2", "-readrate_catchup", "1.5", "-i", i]);
    }
    let audio_input = if inputs.len() > 1 { "1:a:0?" } else { "0:a:0?" };
    // ffmpeg refuses to start when an output gets no stream, so a file without sound
    // (many clips and effect videos) needs the audio output left out.
    let with_audio = inputs.len() > 1 || inputs.first().is_some_and(|i| i.starts_with("http") || has_audio(i));
    cmd.args([
        "-map",
        "0:v:0",
        "-vf",
        &format!("scale={W}:{H}:force_original_aspect_ratio=decrease,pad={W}:{H}:(ow-iw)/2:(oh-ih)/2,fps={FPS}"),
        "-pix_fmt",
        "rgba",
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    if with_audio {
        cmd.args([
            "-map",
            audio_input,
            "-ac",
            "2",
            "-ar",
            &crate::audio::RATE.to_string(),
            "-f",
            "f32le",
            &format!("tcp://127.0.0.1:{port}"),
        ]);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = hidden(&mut cmd).spawn()?;
    let video = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no video pipe"))?;
    let stop = Arc::new(AtomicBool::new(false));

    // ffmpeg's messages: pick out the first input's duration, drain the rest.
    let duration = Arc::new(Mutex::new(None));
    let last_msg = Arc::new(Mutex::new(String::new()));
    if let Some(err) = child.stderr.take() {
        let (duration, last_msg) = (duration.clone(), last_msg.clone());
        std::thread::spawn(move || {
            let mut found = false;
            for line in std::io::BufRead::split(std::io::BufReader::new(err), b'\n').map_while(Result::ok) {
                let line = String::from_utf8_lossy(&line);
                if !found {
                    if let Some(d) = parse_duration(&line) {
                        *duration.lock().unwrap() = Some(d);
                        found = true;
                    }
                }
                let l = line.trim();
                if !l.is_empty() && !l.starts_with("Stream") && !l.starts_with("Metadata") && !line.starts_with("    ") {
                    *last_msg.lock().unwrap() = l.to_string();
                }
            }
        });
    }

    // Video reader: whole frames into the queue; blocks (and so throttles ffmpeg)
    // while the queue is full.
    let frames = Arc::new(FrameQueue { q: Mutex::new(VecDeque::new()), cv: Condvar::new(), eof: AtomicBool::new(false) });
    std::thread::spawn({
        let (frames, stop) = (frames.clone(), stop.clone());
        move || read_frames(video, &frames, &stop)
    });

    // Audio reader: accept ffmpeg's connection, forward samples to the mixer.
    listener.set_nonblocking(true)?;
    std::thread::spawn({
        let stop = stop.clone();
        move || {
            let t0 = Instant::now();
            let mut sock = loop {
                if stop.load(Ordering::SeqCst) || t0.elapsed().as_secs() > 30 {
                    return;
                }
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
                }
            };
            let _ = sock.set_nonblocking(false);
            let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(500)));
            let mut buf = vec![0u8; 4096 * 8];
            let mut carry: Vec<u8> = Vec::new();
            crate::audio::clear_media();
            while !stop.load(Ordering::SeqCst) {
                match sock.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        carry.extend_from_slice(&buf[..n]);
                        let whole = carry.len() / 4 * 4;
                        let samples: Vec<f32> =
                            carry[..whole].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
                        carry.drain(..whole);
                        crate::audio::push_media(&samples);
                    }
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                    Err(_) => break,
                }
            }
            crate::audio::clear_media();
        }
    });

    Ok(Running { child, frames, stop, duration, last_msg })
}

/// Whether a local media file has a sound track (ffmpeg lists its streams, then stops
/// because no output is given).
fn has_audio(path: &str) -> bool {
    let mut cmd = Command::new(crate::tools::ffmpeg());
    cmd.args(["-hide_banner", "-nostdin", "-i", path]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    match hidden(&mut cmd).output() {
        Ok(out) => String::from_utf8_lossy(&out.stderr).lines().any(|l| l.trim_start().starts_with("Stream #") && l.contains("Audio:")),
        Err(_) => true,
    }
}

/// "  Duration: 00:03:21.05, start: …" → 201.05 s (`N/A` for live streams → None).
fn parse_duration(line: &str) -> Option<f64> {
    let rest = line.trim_start().strip_prefix("Duration:")?.trim_start();
    let hms = rest.split(',').next()?.trim();
    let mut it = hms.split(':');
    let (h, m, s) = (it.next()?.parse::<f64>().ok()?, it.next()?.parse::<f64>().ok()?, it.next()?.parse::<f64>().ok()?);
    Some(h * 3600.0 + m * 60.0 + s)
}

fn read_frames(mut video: ChildStdout, frames: &FrameQueue, stop: &AtomicBool) {
    let n = (W * H * 4) as usize;
    loop {
        let mut buf = vec![0u8; n];
        if video.read_exact(&mut buf).is_err() || stop.load(Ordering::SeqCst) {
            break;
        }
        let mut q = frames.q.lock().unwrap();
        while q.len() >= QUEUE_MAX && !stop.load(Ordering::SeqCst) {
            q = frames.cv.wait_timeout(q, std::time::Duration::from_millis(100)).unwrap().0;
        }
        q.push_back(Arc::from(buf.into_boxed_slice()));
        frames.cv.notify_all();
    }
    frames.eof.store(true, Ordering::SeqCst);
    frames.cv.notify_all();
}

impl CaptureSource for LinkSource {
    fn format(&self) -> VideoFormat {
        self.format
    }

    fn modes(&self) -> Vec<CamMode> {
        vec![CamMode { width: W, height: H, fps: FPS }]
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<CpuFrame>> {
        // Transport requests from the UI.
        let (paused, seek) = {
            let mut p = PLAYBACK.lock().unwrap();
            if p.owner == self.owner {
                (p.paused, p.seek.take())
            } else {
                (false, None)
            }
        };
        if let Some(d) = self.run.as_ref().and_then(|r| *r.duration.lock().unwrap()) {
            self.duration = Some(d);
        }
        if let Some(t) = seek {
            if paused {
                // Jump while paused: remember it, start there on Play.
                self.run = None;
                crate::audio::clear_media();
                self.offset = t;
                self.shown = 0;
            } else {
                self.restart(t)?;
            }
        }
        if paused {
            // Stop decoding (and the sound) while paused; the last frame stays on the picture.
            if self.run.is_some() {
                self.offset = self.position();
                self.shown = 0;
                self.run = None;
                crate::audio::clear_media();
            }
            self.publish(false);
            std::thread::sleep(Duration::from_millis(30));
            return Ok(None);
        }
        if self.run.is_none() {
            self.restart(self.offset)?;
        }
        let period = Duration::from_secs(1) / FPS;
        let frames = self.run.as_ref().map(|r| r.frames.clone()).ok_or_else(|| anyhow::anyhow!("not running"))?;
        let mut q = frames.q.lock().unwrap();
        // (Re)buffer: wait until half a second is ready (or the stream ended).
        if self.buffering {
            let t0 = Instant::now();
            while q.len() < self.prebuffer && !frames.eof.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_millis(200) {
                q = frames.cv.wait_timeout(q, Duration::from_millis(50)).unwrap().0;
            }
            if q.len() < self.prebuffer && !frames.eof.load(Ordering::SeqCst) {
                drop(q);
                self.publish(true);
                return Ok(None); // still filling; the capture loop calls again shortly
            }
            self.buffering = false;
            self.prebuffer = 1;
            self.next_due = Some(Instant::now());
        }
        let Some(frame) = q.pop_front() else {
            if frames.eof.load(Ordering::SeqCst) {
                drop(q);
                // A finished (non-live) video: hold the last picture, paused at the end,
                // so it can be replayed / scrubbed back instead of erroring out.
                if self.duration.is_some() && self.shown > 0 {
                    self.offset = self.position();
                    self.shown = 0;
                    self.run = None;
                    let mut p = PLAYBACK.lock().unwrap();
                    if p.owner == self.owner {
                        p.paused = true;
                        p.pos = self.duration.unwrap_or(self.offset);
                    }
                    return Ok(None);
                }
                let msg = self.run.as_ref().map(|r| r.last_msg.lock().unwrap().clone()).unwrap_or_default();
                if self.shown == 0 && !msg.is_empty() {
                    anyhow::bail!("the video could not be played: {msg}");
                }
                anyhow::bail!("the video has ended");
            }
            self.buffering = true; // underrun: rebuffer instead of stuttering frame by frame
            return Ok(None);
        };
        frames.cv.notify_all();
        drop(q);
        self.shown += 1;
        self.publish(false);
        // Steady 30 fps from the buffer.
        let due = self.next_due.unwrap_or_else(Instant::now);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        self.next_due = Some(due.max(now - period) + period);
        Ok(Some(CpuFrame { format: self.format, pts: Timestamp(self.start.elapsed().as_nanos() as i64), data: frame }))
    }

    fn stop(&mut self) {
        self.run = None;
    }
}

impl Drop for LinkSource {
    fn drop(&mut self) {
        self.run = None;
        let mut p = PLAYBACK.lock().unwrap();
        if p.owner == self.owner {
            p.active = false;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations() {
        assert_eq!(super::parse_duration("  Duration: 00:03:21.50, start: 0.000000, bitrate: 1 kb/s"), Some(201.5));
        assert_eq!(super::parse_duration("  Duration: N/A, start: 0.000000"), None);
        assert_eq!(super::parse_duration("Stream #0:0"), None);
    }
}
