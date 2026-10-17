//! OS selection for the virtual-camera backend. Each backend creates an `ovc-ipc`
//! ring the matching native plugin consumes.

use crate::VirtualCamera;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;

/// Linux: the "MarkCam" v4l2loopback device node, if it exists.
#[cfg(target_os = "linux")]
pub fn linux_device() -> Option<std::path::PathBuf> {
    linux::find_device()
}
#[cfg(not(target_os = "linux"))]
pub fn linux_device() -> Option<std::path::PathBuf> {
    None
}

pub fn open() -> anyhow::Result<Box<dyn VirtualCamera>> {
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(windows::WindowsVCam::new()?))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::MacosVCam::new()?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::LinuxVCam::new()?))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        anyhow::bail!("no virtual-camera backend for this OS")
    }
}
