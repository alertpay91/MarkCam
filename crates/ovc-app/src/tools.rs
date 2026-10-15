//! Helper programs fetched on demand (the first time a link is played), never bundled:
//! **yt-dlp** (turns a YouTube / Twitch / Facebook… page link, live or not, into stream
//! URLs) and **ffmpeg** (decodes any of them — and any local video — into raw frames and
//! audio for MarkCam). Both are free, open source, and kept in `%APPDATA%\MarkCam\tools`.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
const FFMPEG_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

pub fn dir() -> PathBuf {
    crate::settings::app_dir().join("tools")
}

/// Tools shipped with the installer (`<install dir>\tools`).
fn bundled(name: &str) -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?.parent()?.join("tools").join(name);
    p.is_file().then_some(p)
}

/// yt-dlp lives in the user's folder so it can update itself (YouTube changes often); the
/// installer's copy is moved there on first use.
pub fn ytdlp() -> PathBuf {
    let p = dir().join("yt-dlp.exe");
    if !p.is_file() {
        if let Some(b) = bundled("yt-dlp.exe") {
            let _ = std::fs::create_dir_all(dir());
            let _ = std::fs::copy(b, &p);
        }
    }
    p
}

pub fn ffmpeg() -> PathBuf {
    let p = dir().join("ffmpeg.exe");
    if p.is_file() {
        return p;
    }
    bundled("ffmpeg.exe").unwrap_or(p)
}

/// The folder ffmpeg is in (for yt-dlp's `--ffmpeg-location`).
pub fn ffmpeg_dir() -> PathBuf {
    ffmpeg().parent().map(PathBuf::from).unwrap_or_else(dir)
}

/// Bring yt-dlp up to date (it must follow YouTube's changes). `force` = now (a link just
/// failed); otherwise at most once a week. Blocking; call from a background thread.
pub fn update_ytdlp(force: bool) -> bool {
    let exe = ytdlp();
    if !exe.is_file() {
        return false;
    }
    let stamp = dir().join("yt-dlp.updated");
    let fresh = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(7 * 24 * 3600));
    if fresh && !force {
        return false;
    }
    let mut cmd = std::process::Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let ok = cmd.arg("-U").output().is_ok_and(|o| o.status.success());
    let _ = std::fs::write(&stamp, b"");
    ok
}

pub fn ready() -> bool {
    ytdlp().is_file() && ffmpeg().is_file()
}

/// A background download of whichever tools are missing.
pub struct Fetch {
    pub status: Arc<Mutex<String>>,
    pub active: Arc<AtomicBool>,
    pub failed: Arc<AtomicBool>,
}

impl Fetch {
    pub fn start() -> Self {
        let status = Arc::new(Mutex::new("starting…".to_string()));
        let active = Arc::new(AtomicBool::new(true));
        let failed = Arc::new(AtomicBool::new(false));
        std::thread::spawn({
            let (status, active, failed) = (status.clone(), active.clone(), failed.clone());
            move || {
                let r = (|| -> anyhow::Result<()> {
                    std::fs::create_dir_all(dir())?;
                    if !ytdlp().is_file() {
                        download(YTDLP_URL, &ytdlp(), "yt-dlp", &status)?;
                    }
                    if !ffmpeg().is_file() {
                        let zip = dir().join("ffmpeg.zip");
                        download(FFMPEG_URL, &zip, "ffmpeg", &status)?;
                        *status.lock().unwrap() = "unpacking ffmpeg…".into();
                        extract_ffmpeg(&zip)?;
                        let _ = std::fs::remove_file(&zip);
                    }
                    Ok(())
                })();
                match r {
                    Ok(()) => *status.lock().unwrap() = "ready".into(),
                    Err(e) => {
                        *status.lock().unwrap() = format!("download failed: {e}");
                        failed.store(true, Ordering::SeqCst);
                    }
                }
                active.store(false, Ordering::SeqCst);
            }
        });
        Fetch { status, active, failed }
    }
}

fn download(url: &str, dst: &std::path::Path, what: &str, status: &Mutex<String>) -> anyhow::Result<()> {
    let resp = ureq::get(url).call()?;
    let total: Option<u64> = resp.header("Content-Length").and_then(|s| s.parse().ok());
    let tmp = dst.with_extension("part");
    let mut file = std::fs::File::create(&tmp)?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n])?;
        got += n as u64;
        *status.lock().unwrap() = match total {
            Some(t) if t > 0 => format!("downloading {what}… {}%", got * 100 / t),
            _ => format!("downloading {what}… {} MB", got / 1_000_000),
        };
    }
    drop(file);
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

/// Pull just `bin/ffmpeg.exe` out of the release zip.
fn extract_ffmpeg(zip_path: &std::path::Path) -> anyhow::Result<()> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    let idx = (0..z.len())
        .find(|&i| z.by_index(i).ok().is_some_and(|f| f.name().to_ascii_lowercase().ends_with("/bin/ffmpeg.exe")))
        .ok_or_else(|| anyhow::anyhow!("ffmpeg.exe not found in the download"))?;
    let mut f = z.by_index(idx)?;
    let tmp = ffmpeg().with_extension("part");
    let mut out = std::fs::File::create(&tmp)?;
    std::io::copy(&mut f, &mut out)?;
    drop(out);
    std::fs::rename(tmp, ffmpeg())?;
    Ok(())
}
