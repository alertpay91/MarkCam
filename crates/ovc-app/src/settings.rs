//! Persisted user settings (`%APPDATA%\MarkCam\settings.json`): camera + format, effect
//! parameters, the virtual-camera toggle and every active effect layer with its placement,
//! so the Studio comes back exactly as it was left.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    /// Camera by name (indices can shift when devices are plugged in), with the index as a
    /// fallback. Both `None` = test pattern.
    pub camera_name: Option<String>,
    pub camera_index: Option<u32>,
    pub res: (u32, u32),
    pub fps: u32,
    pub mirror: bool,
    /// What is on the picture: "camera", "screen", "media" or "test".
    pub source: String,
    /// The last screen area picked (physical pixels).
    pub screen_area: Option<crate::screen::Area>,
    /// The video file used as the picture.
    pub media_path: Option<PathBuf>,

    pub cg_on: bool,
    /// brightness, contrast, saturation, temperature
    pub cg: [f32; 4],
    pub blur_on: bool,
    pub blur_radius: f32,
    pub ck_on: bool,
    pub ck_rgb: [f32; 3],
    pub ck_similarity: f32,
    pub ck_smoothness: f32,

    /// Color filter (`Filter::id`) and its intensity 0..1.
    pub filter: String,
    pub filter_amount: f32,
    /// Vignette strength (0 = off).
    pub vignette: f32,
    /// Camera boost for weak webcams.
    pub camera_boost: bool,
    /// Distortion (`WarpKind::id`) and its parameters.
    pub warp: String,
    pub warp_center: [f32; 2],
    pub warp_radius: f32,
    pub warp_strength: f32,
    pub warp_follow_face: bool,
    /// Active animated overlays (`AnimKind::id`) and their intensity.
    pub anims: Vec<String>,
    pub anim_amount: f32,

    /// Audio: feed MarkCam Microphone (voice + video sound), which real microphone,
    /// volumes, and whether to hear the video on the headphones.
    pub audio_route: bool,
    pub audio_mic: Option<String>,
    pub mic_gain: f32,
    pub media_gain: f32,
    pub hear_media: bool,
    /// Studio voice on the microphone: 0 off, 1 light, 2 best.
    pub voice_mode: u8,
    /// Its strength, 1..=5.
    pub voice_strength: u8,
    /// The last link played.
    pub last_link: String,
    /// Link quality (max lines).
    pub link_quality: u32,

    pub vcam_on: bool,
    pub layers: Vec<SavedLayer>,

    /// UI state: right-panel tab and the effect category last browsed.
    pub tab: String,
    pub category: Option<String>,

    /// Preview frame rate while MarkCam is behind another window (30, 15, 5 or 0 = off).
    /// A new key: the old `background_preview_fps` defaulted to 15, which people took for a
    /// bug, so everyone starts again from 30.
    pub preview_fps_behind: u32,
    /// Close the webcam while MarkCam is minimized and no app uses the virtual camera.
    pub idle_camera: bool,
    /// Where "Download" saves videos (`None` = Videos\MarkCam).
    pub download_dir: Option<PathBuf>,

    /// Virtual background (`vbg::Choice::id`) and the blur strength 0..1.
    pub background: String,
    pub background_blur: f32,
    /// Edge quality: 0 auto, 1 best, 2 light.
    pub background_quality: u8,
    /// Color key instead of AI, its color, similarity / softness (0..1) and AI help.
    pub key_mode: bool,
    pub key_colors: Vec<[u8; 3]>,
    pub key_similarity: f32,
    pub key_smoothness: f32,
    pub key_ai: bool,
    /// Also list an installed ManyCam's effects and backgrounds.
    pub use_manycam: bool,
    /// Saved name banners and the one last used.
    pub banners: Vec<crate::banner::BannerDef>,
    pub banner_sel: usize,
}

impl Default for Settings {
    fn default() -> Self {
        let blur = ovc_effects::builtin::BackgroundBlur::default().params;
        let ck = ovc_effects::builtin::ChromaKey::default().params;
        Self {
            camera_name: None,
            camera_index: None,
            res: (1280, 720),
            fps: 30,
            mirror: false,
            source: "camera".into(),
            screen_area: None,
            media_path: None,
            cg_on: true,
            cg: [0.0, 1.0, 1.0, 0.0],
            blur_on: false,
            blur_radius: blur.radius,
            ck_on: false,
            ck_rgb: ck.key_rgb,
            ck_similarity: ck.similarity,
            ck_smoothness: ck.smoothness,
            filter: "none".into(),
            filter_amount: 1.0,
            vignette: 0.0,
            camera_boost: false,
            warp: "none".into(),
            warp_center: [0.5, 0.45],
            warp_radius: 0.35,
            warp_strength: 0.6,
            warp_follow_face: true,
            anims: Vec::new(),
            anim_amount: 0.6,
            audio_route: true,
            audio_mic: None,
            mic_gain: 1.0,
            media_gain: 0.8,
            hear_media: true,
            voice_mode: 0,
            voice_strength: crate::voice::DEFAULT_STRENGTH,
            last_link: String::new(),
            link_quality: 360,
            vcam_on: true,
            layers: Vec::new(),
            tab: "effects".into(),
            category: None,
            preview_fps_behind: 30,
            idle_camera: true,
            download_dir: None,
            background: "off".into(),
            background_blur: 0.6,
            background_quality: 0,
            key_mode: false,
            key_colors: vec![[0, 177, 64]],
            key_similarity: 0.2,
            key_smoothness: 0.08,
            key_ai: true,
            use_manycam: false,
            banners: vec![crate::banner::BannerDef::default()],
            banner_sel: 0,
        }
    }
}

/// Where an effect layer came from, so it can be re-loaded on the next start.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum LayerSource {
    /// A loose PNG/JPG.
    Image(PathBuf),
    /// A `.mcv` inside a ManyCam `.mcep` pack.
    PackEntry { pack: PathBuf, mcv: String },
    /// A standalone `.mcv` file.
    McvFile(PathBuf),
    /// A video file (MP4…), played in a loop.
    Video(PathBuf),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct SavedLayer {
    pub source: LayerSource,
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub scale: f32,
    pub opacity: f32,
    pub on: bool,
    #[serde(default)]
    pub remove_green: bool,
}

/// The app's data folder: `%APPDATA%\MarkCam` (settings, My Effects, face models,
/// library organization).
pub fn app_dir() -> PathBuf {
    // Override for testing without touching the user's data.
    if let Some(d) = std::env::var_os("OVC_DATA_DIR") {
        return PathBuf::from(d);
    }
    std::env::var_os("APPDATA").map(|d| Path::new(&d).join("MarkCam")).unwrap_or_else(|| PathBuf::from("."))
}

/// The app was called MarkCam before: move its data folder over once, so settings,
/// "My effects" and downloaded models carry over.
pub fn migrate_old_dir() {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let old = Path::new(&appdata).join("OpenVCam");
    let new = app_dir();
    if old.is_dir() && !new.exists() {
        let _ = std::fs::rename(&old, &new);
    }
}

pub fn path() -> PathBuf {
    // Override for testing a second instance without touching the user's settings.
    if let Some(p) = std::env::var_os("OVC_SETTINGS") {
        return PathBuf::from(p);
    }
    app_dir().join("settings.json")
}

/// Saved settings, or defaults if there are none / they can't be read.
pub fn load() -> Settings {
    std::fs::read(path())
        .ok()
        .and_then(|b| {
            // Tolerate a UTF-8 BOM (Notepad and PowerShell add one).
            let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&b);
            serde_json::from_slice(b).ok()
        })
        .unwrap_or_default()
}

/// Write atomically (temp file + rename) so a crash mid-write can't corrupt the settings.
pub fn save(s: &Settings) -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s).unwrap_or_default())?;
    std::fs::rename(tmp, p)
}
