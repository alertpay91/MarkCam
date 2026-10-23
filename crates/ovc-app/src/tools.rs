//! Helper programs fetched on demand (the first time a link is played):
//! **yt-dlp** (turns a YouTube / Twitch / Facebook… page link, live or not, into stream
//! URLs), **ffmpeg** (decodes any of them — and any local video — into raw frames and
//! audio for MarkCam) and **deno** (the JavaScript runtime yt-dlp needs for YouTube's
//! challenges). All free, open source, and kept in `%APPDATA%\MarkCam\tools`.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(windows)]
const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
#[cfg(not(windows))]
const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux";
const FFMPEG_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
#[cfg(windows)]
const DENO_URL: &str = "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip";
#[cfg(not(windows))]
const DENO_URL: &str = "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip";

const YTDLP: &str = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
const FFMPEG: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
const DENO: &str = if cfg!(windows) { "deno.exe" } else { "deno" };

/// Deno's download was tried this session (it is optional: links that need no JavaScript
/// still play without it, so a failed download doesn't block them).
static DENO_TRIED: AtomicBool = AtomicBool::new(false);

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
    let p = dir().join(YTDLP);
    if !p.is_file() {
        if let Some(b) = bundled(YTDLP) {
            let _ = std::fs::create_dir_all(dir());
            let _ = std::fs::copy(b, &p);
        }
    }
    p
}

pub fn ffmpeg() -> PathBuf {
    let p = dir().join(FFMPEG);
    if p.is_file() {
        return p;
    }
    // Linux: the system's ffmpeg when MarkCam's own copy is missing.
    let system = std::path::Path::new("/usr/bin/ffmpeg");
    bundled(FFMPEG).or_else(|| (!cfg!(windows) && system.is_file()).then(|| system.to_path_buf())).unwrap_or(p)
}

/// Whether this ffmpeg knows `-readrate_catchup` (7.1+; Ubuntu 24.04 / Mint 22 ship 6.1).
pub fn ffmpeg_has_catchup() -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS.get_or_init(|| {
        let mut cmd = std::process::Command::new(ffmpeg());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let Ok(out) = cmd.args(["-hide_banner", "-version"]).output() else { return true };
        // "ffmpeg version 6.1.1-3ubuntu5 …", "ffmpeg version n8.1.3-…", "… N-1234-g…" (git: new).
        let text = String::from_utf8_lossy(&out.stdout);
        let ver = text.split_whitespace().nth(2).unwrap_or("").trim_start_matches('n');
        let mut parts = ver.split(|c: char| !c.is_ascii_digit());
        match (parts.next().and_then(|s| s.parse::<u32>().ok()), parts.next().and_then(|s| s.parse::<u32>().ok())) {
            (Some(major), minor) => (major, minor.unwrap_or(0)) >= (7, 1),
            _ => true,
        }
    })
}

/// YouTube now requires running its JavaScript for many videos ("This video is not
/// available" otherwise); yt-dlp does that with an outside runtime.
pub fn deno() -> PathBuf {
    dir().join(DENO)
}

/// yt-dlp with the JavaScript runtime to use: MarkCam's own deno, else whatever deno or
/// node the computer has.
pub fn ytdlp_command() -> std::process::Command {
    let mut cmd = std::process::Command::new(ytdlp());
    let d = deno();
    if d.is_file() {
        cmd.arg("--js-runtimes").arg(format!("deno:{}", d.display()));
    } else {
        cmd.args(["--js-runtimes", "deno", "--js-runtimes", "node"]);
    }
    cmd
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
    ytdlp().is_file() && ffmpeg().is_file() && (deno().is_file() || DENO_TRIED.load(Ordering::SeqCst))
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
                    if !ffmpeg().is_file() && !cfg!(windows) {
                        anyhow::bail!("ffmpeg is missing: install it with  sudo apt install ffmpeg");
                    }
                    if !ffmpeg().is_file() {
                        let zip = dir().join("ffmpeg.zip");
                        download(FFMPEG_URL, &zip, "ffmpeg", &status)?;
                        *status.lock().unwrap() = "unpacking ffmpeg…".into();
                        extract(&zip, "/bin/ffmpeg.exe", &ffmpeg())?;
                        let _ = std::fs::remove_file(&zip);
                    }
                    if !deno().is_file() {
                        DENO_TRIED.store(true, Ordering::SeqCst);
                        let zip = dir().join("deno.zip");
                        let got = download(DENO_URL, &zip, "deno (for YouTube)", &status).and_then(|_| {
                            *status.lock().unwrap() = "unpacking deno…".into();
                            extract(&zip, DENO, &deno())
                        });
                        let _ = std::fs::remove_file(&zip);
                        if let Err(e) = got {
                            tracing::warn!("deno: {e} (some YouTube videos may not play)");
                        }
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

/// Pull the one file whose path ends with `suffix` (`/bin/ffmpeg.exe`, `deno.exe`) out of
/// a release zip into `dst`.
fn extract(zip_path: &std::path::Path, suffix: &str, dst: &std::path::Path) -> anyhow::Result<()> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    let idx = (0..z.len())
        .find(|&i| z.by_index(i).ok().is_some_and(|f| f.name().to_ascii_lowercase().ends_with(suffix)))
        .ok_or_else(|| anyhow::anyhow!("{suffix} not found in the download"))?;
    let mut f = z.by_index(idx)?;
    let tmp = dst.with_extension("part");
    let mut out = std::fs::File::create(&tmp)?;
    std::io::copy(&mut f, &mut out)?;
    drop(out);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(tmp, dst)?;
    Ok(())
}
