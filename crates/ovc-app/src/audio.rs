//! Audio for lessons: the **MarkCam Microphone**.
//!
//! Windows has no API to create a microphone from an app (ManyCam ships its own signed
//! kernel driver). MarkCam instead installs the free, signed VB-CABLE virtual cable once
//! and renames its microphone end to "MarkCam Microphone". While MarkCam runs it mixes
//! **your real microphone + the sound of the video / link you play** into it — so in
//! ClassIn you pick "MarkCam Microphone" while showing a video and switch back after.
//! The video's sound can also be played on your headphones so you hear it too.
//!
//! Engine: WASAPI shared mode, 48 kHz stereo f32 everywhere (Windows converts from
//! whatever the devices use), ~40 ms queued on the virtual cable.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub const RATE: u32 = 48_000;
pub const CH: usize = 2;

/// Name given to the virtual cable's microphone end (what apps list).
/// Public builds keep VB-CABLE in the name (VB-Audio's donationware terms: users must be
/// able to see it is their cable).
pub const MIC_NAME: &str = if crate::PUBLIC { "MarkCam Microphone (VB-CABLE)" } else { "MarkCam Microphone" };
/// Name given to the cable's speaker end (MarkCam writes into it; not for apps — it can't be
/// removed, the microphone is its other end).
pub const OUT_NAME: &str = "MarkCam Mic Feed (not a speaker)";
/// Every VB-CABLE endpoint's name ends with this (also after renaming).
const CABLE_TAG: &str = "VB-Audio Virtual Cable";

// ── Sound from the playing video / link (pushed by the player) ──────────────────────────

/// Interleaved stereo f32 samples of the playing media, at 48 kHz.
pub fn media_ring() -> &'static Mutex<VecDeque<f32>> {
    static RING: OnceLock<Mutex<VecDeque<f32>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Queue media audio (keeps at most ~4 s — the link player buffers ~2 s ahead).
pub fn push_media(samples: &[f32]) {
    let mut r = media_ring().lock().unwrap();
    r.extend(samples.iter().copied());
    let cap = RATE as usize * CH * 4;
    if r.len() > cap {
        let drop = r.len() - cap;
        r.drain(..drop);
    }
}

pub fn clear_media() {
    media_ring().lock().unwrap().clear();
}

// ── Devices ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

/// Active microphones (capture endpoints), excluding the MarkCam/VB-CABLE one.
pub fn microphones() -> Vec<Device> {
    imp::list(true).into_iter().filter(|d| !d.name.contains(CABLE_TAG)).collect()
}

/// The virtual cable's endpoints `(microphone end, speaker end)`, if installed.
pub fn cable() -> Option<(Device, Device)> {
    let mic = imp::list(true).into_iter().find(|d| d.name.contains(CABLE_TAG))?;
    let out = imp::list(false).into_iter().find(|d| d.name.contains(CABLE_TAG))?;
    Some((mic, out))
}

/// Whether the cable's microphone end already carries the MarkCam name.
pub fn cable_named() -> bool {
    cable().is_some_and(|(m, _)| m.name.starts_with(MIC_NAME))
}

// ── The engine ──────────────────────────────────────────────────────────────────────

/// Mixer settings shared with the UI (lock-free).
pub struct Controls {
    /// Mix the real microphone into MarkCam Microphone.
    pub mic_on: AtomicBool,
    /// Volumes, f32 bits.
    pub mic_gain: AtomicU32,
    pub media_gain: AtomicU32,
    /// Also play the media sound on the default speakers / headphones.
    pub monitor: AtomicBool,
    /// Peak levels of the last block (f32 bits), for meters.
    pub mic_level: AtomicU32,
    pub media_level: AtomicU32,
}

impl Controls {
    pub fn new() -> Arc<Self> {
        Arc::new(Controls {
            mic_on: AtomicBool::new(true),
            mic_gain: AtomicU32::new(1.0f32.to_bits()),
            media_gain: AtomicU32::new(0.8f32.to_bits()),
            monitor: AtomicBool::new(true),
            mic_level: AtomicU32::new(0),
            media_level: AtomicU32::new(0),
        })
    }

    pub fn get(a: &AtomicU32) -> f32 {
        f32::from_bits(a.load(Ordering::Relaxed))
    }

    pub fn set(a: &AtomicU32, v: f32) {
        a.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// The running mixer: mic + media → MarkCam Microphone, media → speakers.
pub struct Engine {
    stop: Arc<AtomicBool>,
    pub status: Arc<Mutex<String>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Engine {
    /// `mic_id`: the real microphone (None = Windows default); `to_cable`: whether the
    /// virtual cable exists to feed.
    pub fn start(controls: Arc<Controls>, mic_id: Option<String>, to_cable: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new("starting…".to_string()));
        let handle = std::thread::Builder::new()
            .name("markcam-audio".into())
            .spawn({
                let (stop, status) = (stop.clone(), status.clone());
                move || {
                    let r = imp::run(&controls, mic_id.as_deref(), to_cable, &stop, &status);
                    if let Err(e) = r {
                        *status.lock().unwrap() = format!("audio error: {e}");
                    }
                }
            })
            .ok();
        Engine { stop, status, handle }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(windows)]
#[allow(dead_code)]
pub fn probe_all() -> Vec<String> {
    imp::probe_all()
}

#[cfg(windows)]
#[allow(dead_code)]
pub fn listen_peak(ms: u64) -> anyhow::Result<f32> {
    imp::listen_peak(ms)
}

/// Windows default speakers / microphone names (diagnostics).
#[cfg(windows)]
#[allow(dead_code)]
pub fn imp_defaults() -> (String, String) {
    imp::defaults()
}

/// Rename the virtual cable's endpoints to the MarkCam names and disable its spare second
/// "speaker" (needs admin).
pub fn rename_cable() -> anyhow::Result<()> {
    let _ = imp::hide_extra_outputs();
    imp::rename_cable()
}

// ── One-time setup of the MarkCam Microphone ─────────────────────────────────────────

const CABLE_ZIP: &str = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";

/// The driver pack: shipped with the installer (`<install dir>\drivers\vbcable`), else
/// downloaded into the tools folder.
fn cable_dir() -> std::path::PathBuf {
    let bundled = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("drivers").join("vbcable")));
    match bundled {
        Some(b) if b.join("VBCABLE_Setup_x64.exe").is_file() => b,
        _ => crate::tools::dir().join("vbcable"),
    }
}

/// Background job: download VB-CABLE, then run MarkCam elevated (`--setup-audio`) to
/// install it and give it the MarkCam names. One Windows admin prompt (plus Windows'
/// own "install this device software?" confirmation).
pub struct Setup {
    pub status: Arc<Mutex<String>>,
    pub active: Arc<AtomicBool>,
}

impl Setup {
    /// `rename_only`: the cable is already installed, just (re)apply the names.
    pub fn start(rename_only: bool) -> Self {
        let status = Arc::new(Mutex::new("starting…".to_string()));
        let active = Arc::new(AtomicBool::new(true));
        std::thread::spawn({
            let (status, active) = (status.clone(), active.clone());
            move || {
                let r = (|| -> anyhow::Result<()> {
                    let dir = cable_dir();
                    if !rename_only && !dir.join("VBCABLE_Setup_x64.exe").is_file() {
                        *status.lock().unwrap() = "downloading the virtual audio cable…".into();
                        std::fs::create_dir_all(&dir)?;
                        let mut bytes = Vec::new();
                        ureq::get(CABLE_ZIP).call()?.into_reader().read_to_end(&mut bytes)?;
                        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
                        z.extract(&dir)?;
                    }
                    *status.lock().unwrap() = "installing — confirm the Windows prompts…".into();
                    let exe = std::env::current_exe()?;
                    let flag = if rename_only { "--rename-audio" } else { "--setup-audio" };
                    let script = format!(
                        "Start-Process -FilePath '{}' -Verb RunAs -Wait -ArgumentList '{}', '\"{}\"'",
                        exe.display(),
                        flag,
                        dir.display()
                    );
                    let mut cmd = std::process::Command::new("powershell");
                    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        cmd.creation_flags(0x0800_0000);
                    }
                    cmd.status()?;
                    Ok(())
                })();
                *status.lock().unwrap() = match r {
                    Ok(()) if cable_named() => "MarkCam Microphone is ready".into(),
                    Ok(()) if cable().is_some() => "installed — restart Windows if the name doesn't show yet".into(),
                    Ok(()) => "not installed (cancelled?)".into(),
                    Err(e) => format!("setup failed: {e}"),
                };
                active.store(false, Ordering::SeqCst);
            }
        });
        Setup { status, active }
    }
}

/// Installing the virtual cable can make it Windows' default speakers / microphone (then
/// nothing is heard). Put real devices back: a headset / speakers / headphones if there
/// is one, else the first real device. Returns what was fixed.
pub fn fix_defaults() -> Vec<String> {
    let mut fixed = Vec::new();
    #[cfg(windows)]
    {
        let (spk, mic) = imp::defaults();
        let pick = |devs: Vec<Device>| {
            let real: Vec<Device> = devs.into_iter().filter(|d| !d.name.contains(CABLE_TAG)).collect();
            let pref = ["headset", "headphone", "earphone", "speakers", "slušalice", "zvučnici"];
            real.iter().find(|d| pref.iter().any(|p| d.name.to_lowercase().contains(p))).or(real.first()).cloned()
        };
        if spk.contains(CABLE_TAG) {
            if let Some(d) = pick(imp::list(false)) {
                if imp::set_default(&d.id).is_ok() {
                    fixed.push(format!("speakers → {}", d.name));
                }
            }
        }
        if mic.contains(CABLE_TAG) {
            if let Some(d) = pick(imp::list(true)) {
                if imp::set_default(&d.id).is_ok() {
                    fixed.push(format!("microphone → {}", d.name));
                }
            }
        }
    }
    fixed
}

/// The elevated half (`MarkCam.exe --setup-audio <dir>` / `--rename-audio`): install the
/// driver if asked, wait for its endpoints, rename them. Runs without a window.
pub fn elevated_setup(dir: &std::path::Path, install: bool) {
    // Remember the user's devices: the driver install may hijack the defaults.
    #[cfg(windows)]
    let before = imp::default_ids();
    // (Already there — e.g. reinstalling MarkCam — only the names are set again.)
    if install && cable().is_none() {
        let setup = dir.join("VBCABLE_Setup_x64.exe");
        // -i install, -h hidden (the installer's documented silent switches).
        let _ = std::process::Command::new(&setup).args(["-i", "-h"]).current_dir(dir).status();
    }
    // The endpoints take a moment to appear after a fresh install.
    for _ in 0..40 {
        if cable().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = rename_cable();
    #[cfg(windows)]
    {
        if let Some(id) = &before.0 {
            let _ = imp::set_default(id);
        }
        if let Some(id) = &before.1 {
            let _ = imp::set_default(id);
        }
    }
    fix_defaults();
}

/// Peak of interleaved samples.
fn peak(s: &[f32]) -> f32 {
    s.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

#[cfg(windows)]
#[allow(non_snake_case)] // IPolicyConfig keeps its COM method names
mod imp {
    use super::*;
    use windows::core::{HSTRING, PROPVARIANT};
    use windows::Win32::Devices::FunctionDiscovery::{PKEY_Device_DeviceDesc, PKEY_Device_FriendlyName};
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::System::Com::StructuredStorage::{PropVariantChangeType, PropVariantToStringAlloc, PVCHF_DEFAULT};

    struct Com(bool);
    impl Com {
        fn init() -> Self {
            Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
    }

    fn name_of(d: &IMMDevice) -> String {
        unsafe {
            let Ok(store) = d.OpenPropertyStore(STGM_READ) else { return String::new() };
            let Ok(v) = store.GetValue(&PKEY_Device_FriendlyName) else { return String::new() };
            match PropVariantToStringAlloc(&v) {
                Ok(p) => {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                }
                Err(_) => String::new(),
            }
        }
    }

    pub fn list(capture: bool) -> Vec<Device> {
        let _com = Com::init();
        let mut out = Vec::new();
        unsafe {
            let Ok(en) = enumerator() else { return out };
            let flow = if capture { eCapture } else { eRender };
            let Ok(col) = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else { return out };
            for i in 0..col.GetCount().unwrap_or(0) {
                let Ok(d) = col.Item(i) else { continue };
                let id = d.GetId().ok().map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                });
                if let Some(id) = id {
                    out.push(Device { id, name: name_of(&d) });
                }
            }
        }
        out
    }

    /// Windows' (undocumented but long-stable) interface behind the Sound control panel's
    /// "Set as default device".
    #[windows::core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    unsafe trait IPolicyConfig: windows::core::IUnknown {
        fn GetMixFormat(&self, id: windows::core::PCWSTR, f: *mut *mut core::ffi::c_void) -> windows::core::HRESULT;
        fn GetDeviceFormat(&self, id: windows::core::PCWSTR, d: i32, f: *mut *mut core::ffi::c_void) -> windows::core::HRESULT;
        fn ResetDeviceFormat(&self, id: windows::core::PCWSTR) -> windows::core::HRESULT;
        fn SetDeviceFormat(&self, id: windows::core::PCWSTR, a: *mut core::ffi::c_void, b: *mut core::ffi::c_void) -> windows::core::HRESULT;
        fn GetProcessingPeriod(&self, id: windows::core::PCWSTR, d: i32, a: *mut i64, b: *mut i64) -> windows::core::HRESULT;
        fn SetProcessingPeriod(&self, id: windows::core::PCWSTR, a: *mut i64) -> windows::core::HRESULT;
        fn GetShareMode(&self, id: windows::core::PCWSTR, m: *mut core::ffi::c_void) -> windows::core::HRESULT;
        fn SetShareMode(&self, id: windows::core::PCWSTR, m: *mut core::ffi::c_void) -> windows::core::HRESULT;
        fn GetPropertyValue(&self, id: windows::core::PCWSTR, k: *const core::ffi::c_void, v: *mut core::ffi::c_void) -> windows::core::HRESULT;
        fn SetPropertyValue(&self, id: windows::core::PCWSTR, k: *const core::ffi::c_void, v: *mut core::ffi::c_void) -> windows::core::HRESULT;
        fn SetDefaultEndpoint(&self, id: windows::core::PCWSTR, role: i32) -> windows::core::HRESULT;
        fn SetEndpointVisibility(&self, id: windows::core::PCWSTR, visible: i32) -> windows::core::HRESULT;
    }
    const CLSID_POLICY_CONFIG: windows::core::GUID = windows::core::GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

    /// Make `id` the default device for all roles (console, multimedia, communications).
    pub fn set_default(id: &str) -> anyhow::Result<()> {
        let _com = Com::init();
        unsafe {
            let pc: IPolicyConfig = CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)?;
            let w = HSTRING::from(id);
            for role in 0..3 {
                pc.SetDefaultEndpoint(windows::core::PCWSTR(w.as_ptr()), role).ok()?;
            }
        }
        Ok(())
    }

    /// IDs of the current defaults `(speakers, microphone)`.
    pub fn default_ids() -> (Option<String>, Option<String>) {
        let _com = Com::init();
        unsafe {
            let Ok(en) = enumerator() else { return (None, None) };
            let id = |d: windows::core::Result<IMMDevice>| {
                d.ok().and_then(|d| d.GetId().ok()).map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                })
            };
            (id(en.GetDefaultAudioEndpoint(eRender, eConsole)), id(en.GetDefaultAudioEndpoint(eCapture, eConsole)))
        }
    }

    /// Names of the Windows default (speakers, microphone) — diagnostics.
    pub fn defaults() -> (String, String) {
        let _com = Com::init();
        unsafe {
            let Ok(en) = enumerator() else { return Default::default() };
            let spk = en.GetDefaultAudioEndpoint(eRender, eConsole).map(|d| name_of(&d)).unwrap_or_default();
            let mic = en.GetDefaultAudioEndpoint(eCapture, eCommunications).map(|d| name_of(&d)).unwrap_or_default();
            (spk, mic)
        }
    }

    pub fn rename_cable() -> anyhow::Result<()> {
        let _com = Com::init();
        unsafe {
            let en = enumerator()?;
            for (flow, new) in [(eCapture, MIC_NAME), (eRender, OUT_NAME)] {
                let col = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
                for i in 0..col.GetCount()? {
                    let d = col.Item(i)?;
                    if !name_of(&d).contains(CABLE_TAG) {
                        continue;
                    }
                    // The Sound control panel's "rename" edits the device description.
                    let store = d.OpenPropertyStore(STGM_READWRITE)?;
                    let bstr = PROPVARIANT::from(new);
                    let mut v = PROPVARIANT::default();
                    PropVariantChangeType(&mut v, &bstr, PVCHF_DEFAULT, VT_LPWSTR)?;
                    store.SetValue(&PKEY_Device_DeviceDesc, &v)?;
                    store.Commit()?;
                }
            }
        }
        Ok(())
    }

    /// VB-CABLE adds TWO playback endpoints feeding the same cable ("CABLE Input" and the
    /// 16-channel "CABLE In 16ch"); apps list both as speakers. Keep the plain one (fewest
    /// channels — MarkCam writes into it) and disable the other. Needs admin. Returns how
    /// many were disabled.
    pub fn hide_extra_outputs() -> anyhow::Result<usize> {
        let _com = Com::init();
        unsafe {
            let en = enumerator()?;
            let col = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            let mut outs: Vec<(u16, String)> = Vec::new();
            for i in 0..col.GetCount()? {
                let d = col.Item(i)?;
                if !name_of(&d).contains(CABLE_TAG) {
                    continue;
                }
                let channels = d
                    .Activate::<IAudioClient>(CLSCTX_ALL, None)
                    .and_then(|c| c.GetMixFormat())
                    .map(|f| {
                        let n = (*f).nChannels;
                        CoTaskMemFree(Some(f as *const _));
                        n
                    })
                    .unwrap_or(u16::MAX);
                let p = d.GetId()?;
                let id = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                outs.push((channels, id));
            }
            if outs.len() < 2 {
                return Ok(0);
            }
            outs.sort();
            let pc: IPolicyConfig = CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)?;
            let mut n = 0;
            for (_, id) in &outs[1..] {
                let w = HSTRING::from(id.as_str());
                if pc.SetEndpointVisibility(windows::core::PCWSTR(w.as_ptr()), 0).is_ok() {
                    n += 1;
                }
            }
            Ok(n)
        }
    }

    /// Listen to the MarkCam Microphone (as ClassIn would) for `ms`; the peak level heard.
    #[allow(dead_code)]
    pub fn listen_peak(ms: u64) -> anyhow::Result<f32> {
        let _com = Com::init();
        unsafe {
            let en = enumerator()?;
            let col = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
            let d = (0..col.GetCount()?)
                .filter_map(|i| col.Item(i).ok())
                .find(|d| name_of(d).contains(CABLE_TAG))
                .ok_or_else(|| anyhow::anyhow!("no MarkCam Microphone"))?;
            let cl = open(&d)?;
            let cc: IAudioCaptureClient = cl.GetService()?;
            cl.Start()?;
            let t0 = std::time::Instant::now();
            let mut peak_v = 0f32;
            while t0.elapsed().as_millis() < ms as u128 {
                while cc.GetNextPacketSize()? > 0 {
                    let (mut p, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                    cc.GetBuffer(&mut p, &mut frames, &mut flags, None, None)?;
                    if !p.is_null() && flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 == 0 {
                        peak_v = peak_v.max(peak(std::slice::from_raw_parts(p as *const f32, frames as usize * CH)));
                    }
                    cc.ReleaseBuffer(frames)?;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = cl.Stop();
            Ok(peak_v)
        }
    }

    /// Try opening every active endpoint the way the engine does; report each result.
    #[allow(dead_code)]
    pub fn probe_all() -> Vec<String> {
        let _com = Com::init();
        let mut out = Vec::new();
        unsafe {
            let Ok(en) = enumerator() else { return out };
            for (flow, kind) in [(eRender, "out"), (eCapture, "in ")] {
                let Ok(col) = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else { continue };
                for i in 0..col.GetCount().unwrap_or(0) {
                    let Ok(d) = col.Item(i) else { continue };
                    let r = open(&d).map(|_| "ok".to_string()).unwrap_or_else(|e| format!("{:#010x} {}", e.code().0, e.message()));
                    out.push(format!("{kind} {:60} {r}", name_of(&d)));
                }
            }
        }
        out
    }

    /// 48 kHz stereo f32.
    fn format() -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
            nChannels: CH as u16,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * (CH as u32) * 4,
            nBlockAlign: (CH * 4) as u16,
            wBitsPerSample: 32,
            cbSize: 0,
        }
    }

    /// Open an endpoint in shared mode at 48 kHz stereo f32 (auto-converted).
    unsafe fn open(d: &IMMDevice) -> windows::core::Result<IAudioClient> {
        let c: IAudioClient = d.Activate(CLSCTX_ALL, None)?;
        let wf = format();
        c.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            2_000_000, // 200 ms buffer
            0,
            &wf,
            None,
        )?;
        Ok(c)
    }

    /// Fill a render client up to `target` queued frames from `take(n) -> samples`.
    unsafe fn feed(client: &IAudioClient, render: &IAudioRenderClient, target: u32, mut take: impl FnMut(usize) -> Vec<f32>) {
        let (Ok(size), Ok(pad)) = (client.GetBufferSize(), client.GetCurrentPadding()) else { return };
        let want = target.min(size).saturating_sub(pad);
        if want == 0 {
            return;
        }
        let Ok(p) = render.GetBuffer(want) else { return };
        let samples = take(want as usize);
        let dst = std::slice::from_raw_parts_mut(p as *mut f32, want as usize * CH);
        for (d, s) in dst.iter_mut().zip(samples.iter().chain(std::iter::repeat(&0.0))) {
            *d = *s;
        }
        let _ = render.ReleaseBuffer(want, 0);
    }

    pub fn run(c: &Controls, mic_id: Option<&str>, to_cable: bool, stop: &AtomicBool, status: &Mutex<String>) -> anyhow::Result<()> {
        let _com = Com::init();
        unsafe {
            let en = enumerator()?;
            let mut problems: Vec<String> = Vec::new();
            // Open an output and start it; `None` (with the reason noted) if it won't.
            let start_render = |d: &IMMDevice, problems: &mut Vec<String>| -> Option<(IAudioClient, IAudioRenderClient)> {
                let r = (|| -> windows::core::Result<_> {
                    let cl = open(d)?;
                    let rc: IAudioRenderClient = cl.GetService()?;
                    cl.Start()?;
                    Ok((cl, rc))
                })();
                r.map_err(|e| problems.push(format!("{}: {:#010x}", name_of(d), e.code().0))).ok()
            };
            // The cable's speaker end: where the mix goes. VB-CABLE can expose more than one
            // (e.g. a 16-channel variant that's busy): use the first that opens.
            let cable = if to_cable {
                let col = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
                let mut found = None;
                let mut tried = Vec::new();
                for d in (0..col.GetCount()?).filter_map(|i| col.Item(i).ok()).filter(|d| name_of(d).contains(CABLE_TAG)) {
                    if let Some(c) = start_render(&d, &mut tried) {
                        found = Some(c);
                        break;
                    }
                }
                if found.is_none() {
                    problems.extend(tried);
                }
                found
            } else {
                None
            };
            // The real microphone (only needed when feeding the cable).
            let mic = if cable.is_some() {
                let dev = match mic_id {
                    Some(id) => en.GetDevice(&HSTRING::from(id)).ok(),
                    None => en.GetDefaultAudioEndpoint(eCapture, eCommunications).ok(),
                }
                .filter(|d| !name_of(d).contains(CABLE_TAG));
                dev.and_then(|d| {
                    let r = (|| -> windows::core::Result<_> {
                        let cl = open(&d)?;
                        let cc: IAudioCaptureClient = cl.GetService()?;
                        cl.Start()?;
                        Ok((cl, cc))
                    })();
                    r.map_err(|e| problems.push(format!("microphone {}: {:#010x}", name_of(&d), e.code().0))).ok()
                })
            } else {
                None
            };
            // Speakers / headphones, for hearing the video.
            let spk = en
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .ok()
                .filter(|d| !name_of(d).contains(CABLE_TAG))
                .and_then(|d| start_render(&d, &mut problems));
            let mut line = match (&cable, &mic, to_cable) {
                (Some(_), Some(_), _) => "running: your voice + video sound → MarkCam Microphone".to_string(),
                (Some(_), None, _) => "running: video sound → MarkCam Microphone (microphone not available)".to_string(),
                (None, _, true) => "MarkCam Microphone could not be opened".to_string(),
                (None, _, false) => "running: video sound on your headphones".to_string(),
            };
            if spk.is_none() {
                line.push_str(" · headphones not available");
            }
            if !problems.is_empty() {
                line.push_str(&format!(" · problems: {}", problems.join("; ")));
            }
            *status.lock().unwrap() = line;

            let mut mic_q: VecDeque<f32> = VecDeque::new();
            let mut spk_q: VecDeque<f32> = VecDeque::new();
            let target = RATE / 25; // ~40 ms queued on each output
            while !stop.load(Ordering::SeqCst) {
                // 1. Drain the microphone.
                if let Some((_, cc)) = &mic {
                    while let Ok(n) = cc.GetNextPacketSize() {
                        if n == 0 {
                            break;
                        }
                        let (mut p, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                        if cc.GetBuffer(&mut p, &mut frames, &mut flags, None, None).is_err() {
                            break;
                        }
                        let n = frames as usize * CH;
                        if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || p.is_null() {
                            mic_q.extend(std::iter::repeat(0.0).take(n));
                        } else {
                            mic_q.extend(std::slice::from_raw_parts(p as *const f32, n));
                        }
                        let _ = cc.ReleaseBuffer(frames);
                    }
                    // Keep latency low: never more than ~80 ms of mic backlog.
                    let max = (RATE as usize / 12) * CH;
                    if mic_q.len() > max {
                        let d = mic_q.len() - max;
                        mic_q.drain(..d);
                    }
                }
                let (mic_on, mic_g) = (c.mic_on.load(Ordering::Relaxed), Controls::get(&c.mic_gain));
                let media_g = Controls::get(&c.media_gain);
                let monitor = c.monitor.load(Ordering::Relaxed);

                // 2. Mix into the cable.
                if let Some((cl, rc)) = &cable {
                    feed(cl, rc, target, |frames| {
                        let n = frames * CH;
                        let media: Vec<f32> = {
                            let mut r = media_ring().lock().unwrap();
                            let k = n.min(r.len());
                            r.drain(..k).collect()
                        };
                        let micv: Vec<f32> = {
                            let k = n.min(mic_q.len());
                            mic_q.drain(..k).collect()
                        };
                        Controls::set(&c.media_level, peak(&media) * media_g);
                        Controls::set(&c.mic_level, if mic_on { peak(&micv) * mic_g } else { 0.0 });
                        if monitor {
                            spk_q.extend(media.iter().map(|s| s * media_g));
                        }
                        (0..n)
                            .map(|i| {
                                let m = if mic_on { micv.get(i).copied().unwrap_or(0.0) * mic_g } else { 0.0 };
                                (m + media.get(i).copied().unwrap_or(0.0) * media_g).clamp(-1.0, 1.0)
                            })
                            .collect()
                    });
                } else {
                    // No cable: media goes to the speakers only.
                    let n = (RATE as usize / 100) * CH;
                    let media: Vec<f32> = {
                        let mut r = media_ring().lock().unwrap();
                        let k = n.min(r.len());
                        r.drain(..k).collect()
                    };
                    Controls::set(&c.media_level, peak(&media) * media_g);
                    if monitor {
                        spk_q.extend(media.iter().map(|s| s * media_g));
                    }
                }

                // 3. Speakers.
                if let Some((cl, rc)) = &spk {
                    let max = (RATE as usize / 5) * CH;
                    if spk_q.len() > max {
                        let d = spk_q.len() - max;
                        spk_q.drain(..d);
                    }
                    feed(cl, rc, target, |frames| {
                        let k = (frames * CH).min(spk_q.len());
                        spk_q.drain(..k).collect()
                    });
                }
                // 10 ms: well inside the 40 ms queued on each output, and half the wake-ups
                // (the CPU can sleep deeper in between — battery).
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some((cl, _)) = &cable {
                let _ = cl.Stop();
            }
            if let Some((cl, _)) = &mic {
                let _ = cl.Stop();
            }
            if let Some((cl, _)) = &spk {
                let _ = cl.Stop();
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn list(_: bool) -> Vec<Device> {
        Vec::new()
    }
    pub fn rename_cable() -> anyhow::Result<()> {
        anyhow::bail!("Windows only")
    }
    pub fn hide_extra_outputs() -> anyhow::Result<usize> {
        Ok(0)
    }
    pub fn run(_: &Controls, _: Option<&str>, _: bool, _: &AtomicBool, _: &Mutex<String>) -> anyhow::Result<()> {
        anyhow::bail!("audio is Windows-only for now")
    }
}
