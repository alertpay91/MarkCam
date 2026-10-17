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
/// (Linux: MarkCam's own PulseAudio / PipeWire virtual microphone, no VB-CABLE.)
pub const MIC_NAME: &str = if crate::PUBLIC && cfg!(windows) { "MarkCam Microphone (VB-CABLE)" } else { "MarkCam Microphone" };
/// Name given to the cable's speaker end (MarkCam writes into it; not for apps — it can't be
/// removed, the microphone is its other end).
pub const OUT_NAME: &str = "MarkCam Mic Feed (not a speaker)";
/// Every VB-CABLE endpoint's name ends with this (also after renaming); on Linux both ends
/// of MarkCam's virtual microphone carry "MarkCam".
const CABLE_TAG: &str = if cfg!(windows) { "VB-Audio Virtual Cable" } else { "MarkCam" };

/// The microphone choice that follows the system's.
pub const DEFAULT_MIC: &str = if cfg!(windows) { "Windows default" } else { "System default" };
/// The setup card's note and button.
pub const SETUP_NOTE: &str = if cfg!(windows) {
    "One-time install (admin): MarkCam sets up the free VB-CABLE virtual audio cable and names it MarkCam Microphone."
} else {
    "One click, no driver and no password: MarkCam creates a virtual microphone in PulseAudio / PipeWire (and again by itself after a restart)."
};
pub const SETUP_BUTTON: &str = if cfg!(windows) { "Install MarkCam Microphone" } else { "Create MarkCam Microphone" };

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
    // Linux: the virtual microphone lives until the sound server restarts (a reboot);
    // once set up, MarkCam brings it back by itself.
    #[cfg(not(windows))]
    imp::restore_cable();
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
    /// Studio voice on the microphone: `voice::OFF` / `LIGHT` / `BEST`.
    pub voice: std::sync::atomic::AtomicU8,
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
            voice: std::sync::atomic::AtomicU8::new(crate::voice::OFF),
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
        #[cfg(not(windows))]
        {
            let _ = rename_only;
            *status.lock().unwrap() = match imp::create_cable() {
                Ok(()) => "MarkCam Microphone is ready".into(),
                Err(e) => format!("setup failed: {e}"),
            };
            active.store(false, Ordering::SeqCst);
            return Setup { status, active };
        }
        #[allow(unreachable_code)]
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
    #[cfg_attr(not(windows), allow(unused_mut))]
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

// ── Studio voice and its before / after test ────────────────────────────────────────

/// The 5-second test: record the raw microphone, then play it back without and with
/// Studio voice on the headphones.
pub struct VoiceTest {
    pub recording: Option<Vec<f32>>,
    pub playback: VecDeque<f32>,
    pub status: String,
}

pub fn voice_test() -> &'static Mutex<VoiceTest> {
    static T: OnceLock<Mutex<VoiceTest>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(VoiceTest { recording: None, playback: VecDeque::new(), status: String::new() }))
}

const TEST_SECS: usize = 5;

/// Start recording the test (the audio engine must be running with a microphone).
pub fn start_voice_test() {
    let mut t = voice_test().lock().unwrap();
    t.playback.clear();
    t.recording = Some(Vec::with_capacity(RATE as usize * CH * TEST_SECS));
    t.status = format!("recording {TEST_SECS} s — say a few sentences…");
}

/// Microphone samples on their way to the mix: the test recording, then Studio voice.
fn mic_stage(c: &Controls, voice: &mut Option<crate::voice::Voice>, block: &mut [f32]) {
    {
        let mut t = voice_test().lock().unwrap();
        let full = t.recording.as_mut().map(|r| {
            r.extend_from_slice(block);
            r.len() >= RATE as usize * CH * TEST_SECS
        });
        if full == Some(true) {
            let raw = t.recording.take().unwrap_or_default();
            let mode = match c.voice.load(Ordering::Relaxed) {
                crate::voice::OFF => crate::voice::BEST,
                m => m,
            };
            t.status = "preparing the comparison…".into();
            drop(t);
            std::thread::spawn(move || {
                let mut clean = raw.clone();
                let mut v = crate::voice::Voice::new(mode);
                for ch in clean.chunks_mut(960) {
                    v.process(ch);
                }
                let name = if mode == crate::voice::BEST { "Best" } else { "Light" };
                let mut t = voice_test().lock().unwrap();
                t.playback.extend(raw.iter().copied());
                t.playback.extend(std::iter::repeat(0.0).take(RATE as usize * CH * 3 / 4));
                t.playback.extend(clean);
                t.status = format!("playing: first without, then with Studio voice ({name})");
            });
        }
    }
    let mode = c.voice.load(Ordering::Relaxed);
    if voice.as_ref().map(|v| v.mode()) != Some(mode) && !(mode == crate::voice::OFF && voice.is_none()) {
        *voice = (mode != crate::voice::OFF).then(|| crate::voice::Voice::new(mode));
    }
    if let Some(v) = voice {
        v.process(block);
    }
}

/// What the headphones get for `n` samples: the test playback while it lasts, else the
/// video sound (if "hear the video" is on).
fn monitor_chunk(media: &[f32], gain: f32, monitor: bool, n: usize) -> Vec<f32> {
    {
        let mut t = voice_test().lock().unwrap();
        if !t.playback.is_empty() {
            let k = n.min(t.playback.len());
            let v: Vec<f32> = t.playback.drain(..k).collect();
            if t.playback.is_empty() && t.recording.is_none() {
                t.status = "done — listen again with the button".into();
            }
            return v;
        }
    }
    if monitor {
        media.iter().map(|s| s * gain).collect()
    } else {
        Vec::new()
    }
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
            let mut voice: Option<crate::voice::Voice> = None;
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
                        let mut block = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || p.is_null() {
                            vec![0.0; n]
                        } else {
                            std::slice::from_raw_parts(p as *const f32, n).to_vec()
                        };
                        mic_stage(c, &mut voice, &mut block);
                        mic_q.extend(block);
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
                        spk_q.extend(monitor_chunk(&media, media_g, monitor, n));
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
                    spk_q.extend(monitor_chunk(&media, media_g, monitor, n));
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

/// Linux: PulseAudio's API (PipeWire serves it too, as on Linux Mint 22). The virtual
/// microphone is a null sink "MarkCam Mic Feed" (MarkCam plays the mix into it) whose
/// monitor is remapped into a proper microphone, "MarkCam Microphone".
#[cfg(not(windows))]
mod imp {
    use super::*;
    use libpulse_binding::def::BufferAttr;
    use libpulse_binding::sample::{Format, Spec};
    use libpulse_binding::stream::Direction;
    use libpulse_simple_binding::Simple;
    use std::process::Command;

    const FEED: &str = "markcam_feed";
    const MIC: &str = "markcam_mic";

    /// Marker: the user created the virtual microphone (so it comes back after a reboot).
    fn marker() -> std::path::PathBuf {
        crate::settings::app_dir().join("virtual-microphone")
    }

    fn pactl(args: &[&str]) -> Option<String> {
        let out = Command::new("pactl").env("LC_ALL", "C").args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `pactl list sources|sinks`: (name, description, is a monitor).
    fn entries(kind: &str) -> Vec<(String, String, bool)> {
        let Some(text) = pactl(&["list", kind]) else { return Vec::new() };
        let mut v = Vec::new();
        let (mut name, mut desc, mut monitor) = (None::<String>, None::<String>, false);
        let flush = |v: &mut Vec<(String, String, bool)>, name: &mut Option<String>, desc: &mut Option<String>, monitor: &mut bool| {
            if let Some(n) = name.take() {
                let d = desc.take().unwrap_or_else(|| n.clone());
                v.push((n, d, *monitor));
            }
            *desc = None;
            *monitor = false;
        };
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with("Source #") || l.starts_with("Sink #") {
                flush(&mut v, &mut name, &mut desc, &mut monitor);
            } else if let Some(n) = l.strip_prefix("Name: ") {
                name = Some(n.to_string());
            } else if let Some(d) = l.strip_prefix("Description: ") {
                desc = Some(d.to_string());
            } else if let Some(m) = l.strip_prefix("Monitor of Sink: ") {
                monitor = m != "n/a";
            }
        }
        flush(&mut v, &mut name, &mut desc, &mut monitor);
        v
    }

    pub fn list(capture: bool) -> Vec<Device> {
        entries(if capture { "sources" } else { "sinks" })
            .into_iter()
            .filter(|(_, _, monitor)| !monitor)
            .map(|(id, name, _)| Device { id, name })
            .collect()
    }

    fn exists(kind: &str, name: &str) -> bool {
        pactl(&["list", "short", kind]).is_some_and(|t| t.lines().any(|l| l.split('\t').nth(1) == Some(name)))
    }

    /// Create the virtual microphone (keeps the user's default speakers / microphone).
    pub fn create_cable() -> anyhow::Result<()> {
        if pactl(&["info"]).is_none() {
            anyhow::bail!("no sound server (PulseAudio / PipeWire) answers — is pulseaudio-utils installed?");
        }
        let (spk, mic) = (pactl(&["get-default-sink"]), pactl(&["get-default-source"]));
        if !exists("sinks", FEED) {
            // (Quoted twice: PipeWire's parser needs the outer quotes for names with spaces.)
            let props = format!("sink_properties='device.description=\"{OUT_NAME}\"'");
            pactl(&["load-module", "module-null-sink", &format!("sink_name={FEED}"), "rate=48000", "channels=2", &props])
                .ok_or_else(|| anyhow::anyhow!("could not create the feed (pactl load-module module-null-sink)"))?;
        }
        if !exists("sources", MIC) {
            let props = format!("source_properties='device.description=\"{MIC_NAME}\"'");
            pactl(&["load-module", "module-remap-source", &format!("master={FEED}.monitor"), &format!("source_name={MIC}"), &props])
                .ok_or_else(|| anyhow::anyhow!("could not create the microphone (pactl load-module module-remap-source)"))?;
        }
        // A new device can take over as the default: put the user's back.
        if let Some(s) = spk.map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != FEED) {
            let _ = pactl(&["set-default-sink", &s]);
        }
        if let Some(m) = mic.map(|m| m.trim().to_string()).filter(|m| !m.is_empty() && m != MIC) {
            let _ = pactl(&["set-default-source", &m]);
        }
        let _ = std::fs::create_dir_all(crate::settings::app_dir());
        let _ = std::fs::write(marker(), b"");
        Ok(())
    }

    /// After a reboot: bring the virtual microphone back if the user created it before.
    pub fn restore_cable() {
        static DONE: std::sync::Once = std::sync::Once::new();
        DONE.call_once(|| {
            if marker().is_file() && !exists("sources", MIC) {
                let _ = create_cable();
            }
        });
    }

    pub fn rename_cable() -> anyhow::Result<()> {
        Ok(())
    }
    pub fn hide_extra_outputs() -> anyhow::Result<usize> {
        Ok(0)
    }

    fn spec() -> Spec {
        Spec { format: Format::F32le, channels: CH as u8, rate: RATE }
    }

    /// 10 ms blocks; about `ms` queued on an output.
    fn attr(ms: u32) -> BufferAttr {
        let bytes = |ms: u32| RATE * CH as u32 * 4 * ms / 1000;
        BufferAttr { maxlength: u32::MAX, tlength: bytes(ms), prebuf: u32::MAX, minreq: bytes(10), fragsize: bytes(10) }
    }

    fn as_bytes(s: &[f32]) -> &[u8] {
        // SAFETY: f32 has no padding; the slice is reinterpreted, not copied.
        unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
    }

    /// Write unless the output is already >120 ms behind (then this block is dropped, so a
    /// slower clock never piles up delay).
    fn feed(out: &Simple, block: &[f32]) {
        let behind_ms = out.get_latency().map(|l| l.0 / 1000).unwrap_or(0);
        if behind_ms < 120 {
            let _ = out.write(as_bytes(block));
        }
    }

    pub fn run(c: &Controls, mic_id: Option<&str>, to_cable: bool, stop: &AtomicBool, status: &Mutex<String>) -> anyhow::Result<()> {
        let spec = spec();
        let mut problems: Vec<String> = Vec::new();
        let cable = if to_cable {
            Simple::new(None, "MarkCam", Direction::Playback, Some(FEED), "MarkCam Microphone", &spec, None, Some(&attr(40)))
                .map_err(|e| problems.push(format!("MarkCam Microphone: {e}")))
                .ok()
        } else {
            None
        };
        // MarkCam's own devices as the system defaults (e.g. no other sound device) would
        // feed the mix back into itself: then a real device is picked, or none.
        let ours = |s: Option<String>| s.is_some_and(|s| s.trim().starts_with("markcam"));
        let mic = if cable.is_some() {
            // `Some(None)` = the system default.
            let dev: Option<Option<String>> = match mic_id {
                Some(id) => Some(Some(id.to_string())),
                None if ours(pactl(&["get-default-source"])) => super::microphones().into_iter().next().map(|d| Some(d.id)),
                None => Some(None),
            };
            match dev {
                Some(dev) => Simple::new(None, "MarkCam", Direction::Record, dev.as_deref(), "Microphone", &spec, None, Some(&attr(40)))
                    .map_err(|e| problems.push(format!("microphone: {e}")))
                    .ok(),
                None => None,
            }
        } else {
            None
        };
        let spk = if ours(pactl(&["get-default-sink"])) {
            None
        } else {
            Simple::new(None, "MarkCam", Direction::Playback, None, "Video sound", &spec, None, Some(&attr(60)))
                .map_err(|e| problems.push(format!("headphones: {e}")))
                .ok()
        };
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

        let n = (RATE as usize / 100) * CH; // 10 ms
        let mut micv = vec![0f32; n];
        let mut out = vec![0f32; n];
        let mut voice: Option<crate::voice::Voice> = None;
        let mut tick = std::time::Instant::now();
        while !stop.load(Ordering::SeqCst) {
            // The microphone's clock paces the loop; without one, a 10 ms timer does.
            let have_mic = match &mic {
                Some(m) => {
                    // SAFETY: the f32 buffer read as bytes.
                    let bytes = unsafe { std::slice::from_raw_parts_mut(micv.as_mut_ptr() as *mut u8, n * 4) };
                    m.read(bytes).is_ok()
                }
                None => false,
            };
            if have_mic {
                mic_stage(c, &mut voice, &mut micv);
            } else {
                micv.iter_mut().for_each(|s| *s = 0.0);
                tick += Duration::from_millis(10);
                let now = std::time::Instant::now();
                if tick > now {
                    std::thread::sleep(tick - now);
                } else if now - tick > Duration::from_millis(100) {
                    tick = now;
                }
            }
            let (mic_on, mic_g) = (c.mic_on.load(Ordering::Relaxed), Controls::get(&c.mic_gain));
            let media_g = Controls::get(&c.media_gain);
            let monitor = c.monitor.load(Ordering::Relaxed);
            let media: Vec<f32> = {
                let mut r = media_ring().lock().unwrap();
                let k = n.min(r.len());
                r.drain(..k).collect()
            };
            Controls::set(&c.media_level, peak(&media) * media_g);
            Controls::set(&c.mic_level, if mic_on && have_mic { peak(&micv) * mic_g } else { 0.0 });
            for i in 0..n {
                let m = media.get(i).copied().unwrap_or(0.0) * media_g;
                let v = if mic_on { micv[i] * mic_g } else { 0.0 };
                out[i] = (v + m).clamp(-1.0, 1.0);
            }
            if let Some(cb) = &cable {
                feed(cb, &out);
            }
            if let Some(s) = &spk {
                let chunk = monitor_chunk(&media, media_g, monitor, n);
                if !chunk.is_empty() {
                    feed(s, &chunk);
                }
            }
        }
        Ok(())
    }
}
