//! "Start with Windows" (a per-user Run entry that starts MarkCam minimized) and the
//! single-instance guard: two MarkCams would fight over the webcam and the virtual camera,
//! so a second launch brings the running one to the front instead.

/// Command-line flag the Run entry passes.
pub const MINIMIZED_FLAG: &str = "--minimized";

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
    pub fn enabled() -> bool {
        false
    }
    pub fn set(_on: bool) -> Result<(), String> {
        Err("not available on this system yet".into())
    }
    pub fn first_instance() -> bool {
        true
    }
}

pub use imp::{enabled, first_instance, set};
