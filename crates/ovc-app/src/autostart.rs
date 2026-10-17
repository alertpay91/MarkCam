//! "Start with Windows" (a per-user Run entry that starts MarkCam minimized; on Linux an
//! XDG autostart entry) and the single-instance guard: two MarkCams would fight over the
//! webcam and the virtual camera, so a second launch brings the running one to the front
//! instead.

use std::sync::atomic::{AtomicBool, Ordering};

/// Command-line flag the Run entry passes.
pub const MINIMIZED_FLAG: &str = "--minimized";

/// The setting's label.
pub const LABEL: &str = if cfg!(windows) { "Start MarkCam with Windows (minimized)" } else { "Start MarkCam when I log in (minimized)" };

/// Set when a second launch asked this one to come to the front (Linux; on Windows the
/// second launch raises the window itself).
static SHOW: AtomicBool = AtomicBool::new(false);

/// Whether a second launch asked to show this window (clears the request).
pub fn take_show_request() -> bool {
    SHOW.swap(false, Ordering::SeqCst)
}

/// Lets the request listener wake the UI (call every frame; only the first call counts).
pub fn watch_show_requests(ctx: &eframe::egui::Context) {
    #[cfg(not(windows))]
    {
        let _ = imp::CTX.set(ctx.clone());
    }
    #[cfg(windows)]
    let _ = ctx;
}

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "MarkCam";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg(args: &[&str]) -> Option<std::process::Output> {
        Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output().ok()
    }

    /// Whether the Run entry exists and points at this exe.
    pub fn enabled() -> bool {
        let Some(out) = reg(&["query", RUN_KEY, "/v", VALUE]) else { return false };
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
        out.status.success() && !exe.is_empty() && String::from_utf8_lossy(&out.stdout).to_lowercase().contains(&exe)
    }

    pub fn set(on: bool) -> Result<(), String> {
        let out = if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let cmd = format!("\"{}\" {}", exe.display(), super::MINIMIZED_FLAG);
            reg(&["add", RUN_KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &cmd, "/f"])
        } else {
            reg(&["delete", RUN_KEY, "/v", VALUE, "/f"])
        };
        match out {
            Some(o) if o.status.success() => Ok(()),
            Some(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            None => Err("reg.exe could not be started".into()),
        }
    }

    /// `true` if this is the only MarkCam. Otherwise the running one is brought to the
    /// front and `false` is returned (the caller exits). The mutex lives until exit.
    pub fn first_instance() -> bool {
        use windows::core::w;
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE};
        let created = unsafe { CreateMutexW(None, false, w!("Local\\MarkCam.SingleInstance")) };
        let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        match created {
            // The handle is never closed: the mutex is held until the process exits.
            Ok(_) if !already => true,
            Ok(_) => {
                if let Ok(hwnd) = unsafe { FindWindowW(None, w!("MarkCam")) } {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                        let _ = SetForegroundWindow(hwnd);
                    }
                }
                false
            }
            Err(_) => true, // can't tell: don't block starting
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io::Read;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::OnceLock;

    pub static CTX: OnceLock<eframe::egui::Context> = OnceLock::new();

    /// `~/.config/autostart/markcam.desktop` (the XDG autostart folder every Linux desktop reads).
    fn entry() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("autostart").join("markcam.desktop"))
    }

    pub fn enabled() -> bool {
        entry().is_some_and(|p| std::fs::read_to_string(p).is_ok_and(|t| !t.contains("Hidden=true")))
    }

    pub fn set(on: bool) -> Result<(), String> {
        let p = entry().ok_or("no home folder")?;
        if !on {
            return match std::fs::remove_file(&p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            };
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        // The installed launcher (/usr/bin/markcam) survives updates; else this exe.
        let cmd = if std::path::Path::new("/usr/bin/markcam").exists() { PathBuf::from("/usr/bin/markcam") } else { exe };
        let text = format!(
            "[Desktop Entry]\nType=Application\nName=MarkCam\nComment=Virtual camera with live effects\nExec=\"{}\" {}\nIcon=markcam\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            cmd.display(),
            super::MINIMIZED_FLAG
        );
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&p, text).map_err(|e| e.to_string())
    }

    /// A per-user socket: the first MarkCam listens on it, a second one knocks and exits.
    fn socket() -> PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let user = std::env::var("USER").unwrap_or_default();
        dir.join(format!("markcam-{user}.sock"))
    }

    pub fn first_instance() -> bool {
        let path = socket();
        if UnixStream::connect(&path).is_ok() {
            return false; // the running one comes to the front
        }
        // Nobody answered: a stale socket from a crash.
        let _ = std::fs::remove_file(&path);
        let Ok(listener) = UnixListener::bind(&path) else { return true };
        std::thread::Builder::new()
            .name("markcam-instance".into())
            .spawn(move || {
                for s in listener.incoming().flatten() {
                    let mut s = s;
                    let _ = s.read(&mut [0u8; 8]);
                    super::SHOW.store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(ctx) = CTX.get() {
                        ctx.request_repaint();
                    }
                }
            })
            .ok();
        true
    }
}

pub use imp::{enabled, first_instance, set};
