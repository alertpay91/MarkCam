//! Importer for ManyCam's effect packs, reverse-engineered for interoperability so the
//! effects a user already has can be used in OpenVCam.
//!
//! Container nesting (all plain ZIP):
//! ```text
//! Foo.mcep (zip)
//!   effect_pack_info.json
//!   Some Effect.mcv (zip)
//!     effect.json         { name, author, type, category, subcategory }
//!     properties.json     { position:[x,y], size:[w,h], canvas_aspect, animation_type, repeat, ... }
//!     images/main/<asset> PNG | JPG | GIF   (the actual visual)
//!     images/thumbnail/{small,medium,large}.jpg
//! ```
//! `type` is one of `object` / `fullscreen_object` (a positioned still image), `animation`
//! (an animated GIF), or `face_modifications` / `face` (a mask anchored to 68 facial
//! landmarks — see [`FaceMask`]). Positions/sizes are fractions of the ManyCam canvas.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use serde::Deserialize;

/// A decoded animation frame (one still for non-animated effects).
pub struct Frame {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// How long to show this frame before the next, in milliseconds (stills: 0).
    pub delay_ms: u32,
}

/// The broad category that decides how an effect is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A still image placed at `position`/`size` (sticker or full-screen border).
    Object,
    /// An animated GIF placed at `position`/`size`.
    Animation,
    /// A mask anchored to facial landmarks (needs face tracking to apply).
    FaceMask,
    /// Recognised container but an unsupported/unknown `type`.
    Other,
    /// Not a plain ZIP: ManyCam ships some (store/premium) effects encrypted so they only
    /// work inside ManyCam. They are listed so they can be counted, never decoded.
    Locked,
}

/// 68(+)-point normalized mask anchors for face effects (indices follow the dlib/iBUG
/// 68-landmark model). Present only for [`Kind::FaceMask`].
#[derive(Debug, Clone, Default)]
pub struct FaceMask {
    /// `(x, y)` in [0,1] relative to the mask image, one per landmark pivot.
    pub pivots: Vec<[f32; 2]>,
}

/// A fully decoded effect ready to composite.
pub struct Effect {
    pub name: String,
    pub author: String,
    pub kind: Kind,
    /// Raw `type` string from `effect.json` (for diagnostics).
    pub raw_type: String,
    /// Top-left position as a fraction of the canvas.
    pub position: [f32; 2],
    /// Size as a fraction of the canvas.
    pub size: [f32; 2],
    pub canvas_aspect: f32,
    pub repeat: bool,
    pub frames: Vec<Frame>,
    pub face: Option<FaceMask>,
}

/// A lightweight reference to one effect (name/type only — no image decode), cheap enough to
/// list a whole pack/folder for a browser UI. It points either at an entry inside a `.mcep`
/// pack, or at a standalone `.mcv` file on disk (`file` set).
#[derive(Debug, Clone)]
pub struct EffectRef {
    pub pack_path: PathBuf,
    pub pack_name: String,
    /// The `.mcv` entry name inside the pack (the key to [`load_effect`]).
    pub mcv_name: String,
    pub display_name: String,
    pub kind: Kind,
    /// If set, this is a standalone `.mcv` file (loaded via [`load_mcv_file`]).
    pub file: Option<PathBuf>,
}

#[derive(Deserialize, Default)]
struct EffectJson {
    name: Option<String>,
    author: Option<String>,
    #[serde(rename = "type")]
    type_: Option<String>,
}

#[derive(Deserialize, Default)]
struct PropsJson {
    position: Option<[f32; 2]>,
    size: Option<[f32; 2]>,
    canvas_aspect: Option<f32>,
    repeat: Option<bool>,
}

fn kind_from_type(t: &str) -> Kind {
    match t {
        "object" | "fullscreen_object" => Kind::Object,
        "animation" => Kind::Animation,
        "face_modifications" | "face" => Kind::FaceMask,
        _ => Kind::Other,
    }
}

/// The default ManyCam effect-pack directory for the current user, if ManyCam is installed.
pub fn default_pack_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let p = Path::new(&appdata).join("ManyCam").join("Effects").join("Effect Packs");
    p.is_dir().then_some(p)
}

/// The default ManyCam per-category effects directory (individual `.mcv` files, where
/// effects downloaded through ManyCam land), if present.
pub fn default_video_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let p = Path::new(&appdata).join("ManyCam").join("Effects").join("Video");
    p.is_dir().then_some(p)
}

/// Category subdirectories under a Video dir that actually contain `.mcv` effects.
pub fn list_category_dirs(video_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(video_dir)
        .with_context(|| format!("read dir {}", video_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_dir()
                && std::fs::read_dir(p).map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .any(|e| e.path().extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("mcv")))
                }).unwrap_or(false)
        })
        .collect();
    v.sort();
    Ok(v)
}

/// List every `*.mcep` pack in a directory.
pub fn list_packs(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read dir {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("mcep")))
        .collect();
    v.sort();
    Ok(v)
}

fn read_entry_by_suffix<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, suffix: &str) -> Option<Vec<u8>> {
    let idx = (0..zip.len()).find(|&i| {
        zip.by_index(i).ok().is_some_and(|f| f.name().to_ascii_lowercase().ends_with(suffix))
    })?;
    let mut f = zip.by_index(idx).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Names of the `.mcv` entries in a pack.
fn mcv_names<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>) -> Vec<String> {
    (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| n.to_ascii_lowercase().ends_with(".mcv"))
        .collect()
}

/// List the effects in one pack without decoding any imagery.
pub fn list_effects_in_pack(pack_path: &Path) -> anyhow::Result<Vec<EffectRef>> {
    let pack_name = pack_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let file = std::fs::File::open(pack_path).with_context(|| format!("open {}", pack_path.display()))?;
    let mut zip = zip::ZipArchive::new(file).with_context(|| format!("read pack {}", pack_path.display()))?;
    let names = mcv_names(&mut zip);
    let mut out = Vec::with_capacity(names.len());
    for mcv_name in names {
        // Read the inner .mcv and peek only at effect.json.
        let bytes = match read_entry_by_name(&mut zip, &mcv_name) {
            Some(b) => b,
            None => continue,
        };
        let (display_name, kind) = peek_mcv(&bytes, mcv_name.trim_end_matches(".mcv"));
        out.push(EffectRef {
            pack_path: pack_path.to_path_buf(),
            pack_name: pack_name.clone(),
            mcv_name,
            display_name,
            kind,
            file: None,
        });
    }
    out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    Ok(out)
}

/// Read only `effect.json` from `.mcv` bytes for a quick (name, kind) without decoding images.
fn peek_mcv(bytes: &[u8], fallback_name: &str) -> (String, Kind) {
    match zip::ZipArchive::new(Cursor::new(bytes)) {
        Ok(mut inner) => {
            let ej: EffectJson = read_entry_by_suffix(&mut inner, "effect.json")
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let disp = ej.name.unwrap_or_else(|| fallback_name.to_string());
            (disp, kind_from_type(ej.type_.as_deref().unwrap_or("")))
        }
        Err(_) => (fallback_name.to_string(), Kind::Other),
    }
}

/// Like [`peek_mcv`] but straight from a file: only the zip directory and `effect.json` are
/// read, not the (possibly multi-MB) imagery.
fn peek_mcv_file(path: &Path, fallback_name: &str) -> (String, Kind) {
    let mut magic = [0u8; 2];
    let is_zip = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"PK";
    if !is_zip {
        return (fallback_name.to_string(), Kind::Locked);
    }
    let parsed = std::fs::File::open(path).ok().and_then(|f| zip::ZipArchive::new(std::io::BufReader::new(f)).ok());
    match parsed {
        Some(mut z) => {
            let ej: EffectJson =
                read_entry_by_suffix(&mut z, "effect.json").and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            (ej.name.unwrap_or_else(|| fallback_name.to_string()), kind_from_type(ej.type_.as_deref().unwrap_or("")))
        }
        None => (fallback_name.to_string(), Kind::Other),
    }
}

/// The effect's small thumbnail (`images/thumbnail/small.jpg`, else medium/large/main
/// still) decoded to RGBA, for a browser grid. Cheap: reads only that entry.
pub fn load_thumbnail(r: &EffectRef) -> Option<(Vec<u8>, u32, u32)> {
    fn pick<R: Read + std::io::Seek>(z: &mut zip::ZipArchive<R>) -> Option<Vec<u8>> {
        ["thumbnail/small.", "thumbnail/medium.", "thumbnail/large."]
            .iter()
            .find_map(|t| {
                let idx = (0..z.len()).find(|&i| z.by_index(i).ok().is_some_and(|f| f.name().to_ascii_lowercase().contains(t)))?;
                let mut f = z.by_index(idx).ok()?;
                let mut b = Vec::new();
                f.read_to_end(&mut b).ok()?;
                Some(b)
            })
    }
    // Plain image / GIF: its (first) frame, shrunk to thumbnail size.
    if let Some(p) = r.file.as_deref().filter(|p| is_image_file(p)) {
        let img = image::open(p).ok()?.thumbnail(200, 150).to_rgba8();
        let (w, h) = img.dimensions();
        return Some((img.into_raw(), w, h));
    }
    let bytes = match &r.file {
        Some(p) => {
            let f = std::fs::File::open(p).ok()?;
            pick(&mut zip::ZipArchive::new(std::io::BufReader::new(f)).ok()?)?
        }
        None => {
            let f = std::fs::File::open(&r.pack_path).ok()?;
            let mut pack = zip::ZipArchive::new(f).ok()?;
            let mcv = read_entry_by_name(&mut pack, &r.mcv_name)?;
            pick(&mut zip::ZipArchive::new(Cursor::new(mcv)).ok()?)?
        }
    };
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

/// List the standalone `.mcv` effects in a directory (e.g. a ManyCam `Video/<Category>` folder).
pub fn list_effects_in_dir(dir: &Path) -> anyhow::Result<Vec<EffectRef>> {
    let pack_name = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("mcv")) != Some(true) {
            continue;
        }
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (display_name, kind) = peek_mcv_file(&path, &stem);
        out.push(EffectRef {
            pack_path: PathBuf::new(),
            pack_name: pack_name.clone(),
            mcv_name: String::new(),
            display_name,
            kind,
            file: Some(path),
        });
    }
    out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    Ok(out)
}

/// Load an effect referenced by an [`EffectRef`], from a pack entry, a standalone `.mcv`,
/// or a plain image / GIF file.
pub fn load_effect_ref(r: &EffectRef) -> anyhow::Result<Effect> {
    match &r.file {
        Some(p) if is_image_file(p) => load_image_effect(p),
        Some(p) => load_mcv_file(p),
        None => load_effect(&r.pack_path, &r.mcv_name),
    }
}

/// File extensions usable as image effects: stills (PNG, JPG, WebP) and animations
/// (GIF, animated WebP, APNG — an animated `.png` / `.apng`).
pub const IMAGE_EXTENSIONS: [&str; 6] = ["png", "apng", "jpg", "jpeg", "gif", "webp"];

pub fn is_image_file(p: &Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Decode an image file's bytes into frames: every frame (with timing) for animated
/// GIF / WebP / APNG, a single frame otherwise.
fn decode_any(bytes: &[u8], ext: &str) -> anyhow::Result<Vec<Frame>> {
    use image::AnimationDecoder;
    let collect = |frames: image::Frames| -> anyhow::Result<Vec<Frame>> {
        let mut out = Vec::new();
        for fr in frames {
            let fr = fr.context("decode frame")?;
            let (n, d) = fr.delay().numer_denom_ms();
            let delay_ms = if d == 0 { n } else { n / d.max(1) };
            let buf = fr.into_buffer();
            let (w, h) = buf.dimensions();
            out.push(Frame { rgba: buf.into_raw(), w, h, delay_ms: delay_ms.max(20) });
        }
        if out.is_empty() {
            anyhow::bail!("no frames");
        }
        Ok(out)
    };
    match ext {
        "gif" => decode_gif(bytes),
        "webp" => {
            let mut dec = image_webp::WebPDecoder::new(Cursor::new(bytes)).context("open webp")?;
            if dec.is_animated() {
                decode_webp_anim(&mut dec)
            } else {
                Ok(vec![decode_still(bytes)?])
            }
        }
        "png" | "apng" => {
            let dec = image::codecs::png::PngDecoder::new(Cursor::new(bytes)).context("open png")?;
            if dec.is_apng().unwrap_or(false) {
                collect(dec.apng().context("apng")?.into_frames())
            } else {
                Ok(vec![decode_still(bytes)?])
            }
        }
        _ => Ok(vec![decode_still(bytes)?]),
    }
}

/// A plain image or animation as an effect, keeping all frames and their timing.
/// A large ~16:9 image is treated as a full-frame overlay (a border / frame); anything
/// else as a sticker.
/// Placement of a face sticker, from `<image>.face.json` next to the image:
/// `{ "center": [x, y], "size": [w, h] }` in face units (origin between the eyes, +y down
/// the face, 1 unit = 0.84 × the distance between the eyes; eyes at x = ±0.6, nose tip
/// ≈ +0.55, mouth ≈ +1.2, chin ≈ +2.1, top of the head ≈ −1.9).
#[derive(Deserialize)]
struct FacePlacement {
    center: [f32; 2],
    size: [f32; 2],
}

/// The `.face.json` beside an image, if it has one.
pub fn face_placement_file(image: &Path) -> PathBuf {
    image.with_extension("face.json")
}

fn face_placement(image: &Path) -> Option<FacePlacement> {
    serde_json::from_slice(&std::fs::read(face_placement_file(image)).ok()?).ok()
}

pub fn load_image_effect(path: &Path) -> anyhow::Result<Effect> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    let frames = decode_any(&bytes, &ext)?;
    let (w, h) = frames.first().map(|f| (f.w as f32, f.h as f32)).unwrap_or((1.0, 1.0));
    let fullscreen = w >= 1000.0 && ((w / h) - 16.0 / 9.0).abs() < 0.05;
    let face = face_placement(path);
    let (kind, position, size) = match &face {
        // A face sticker: anchored like ManyCam's `face` effects (center offset + size).
        Some(f) => (Kind::FaceMask, f.center, f.size),
        None => (if frames.len() > 1 { Kind::Animation } else { Kind::Object }, [0.0, 0.0], [1.0, 1.0]),
    };
    Ok(Effect {
        name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        author: String::new(),
        kind,
        raw_type: if fullscreen { "fullscreen_object" } else if face.is_some() { "face" } else { "object" }.into(),
        position,
        size,
        canvas_aspect: 0.0,
        repeat: true,
        frames,
        face: None,
    })
}

/// Image / GIF files in a folder as effect references (the user's "My effects").
pub fn list_images_in_dir(dir: &Path, category: &str) -> Vec<EffectRef> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<EffectRef> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && is_image_file(p))
        .map(|p| {
            // GIFs are always tagged animated; WebP/APNG are checked when loaded.
            let gif = p.extension().and_then(|s| s.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "gif" | "apng"));
            EffectRef {
                pack_path: PathBuf::new(),
                pack_name: category.to_string(),
                mcv_name: String::new(),
                display_name: p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                kind: if face_placement_file(&p).is_file() {
                    Kind::FaceMask
                } else if gif {
                    Kind::Animation
                } else {
                    Kind::Object
                },
                file: Some(p),
            }
        })
        .collect();
    v.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    v
}

fn read_entry_by_name<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let mut f = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Fully decode one effect (by its `.mcv` entry name) from a pack: imagery + placement.
pub fn load_effect(pack_path: &Path, mcv_name: &str) -> anyhow::Result<Effect> {
    let file = std::fs::File::open(pack_path).with_context(|| format!("open {}", pack_path.display()))?;
    let mut pack = zip::ZipArchive::new(file)?;
    let mcv_bytes = read_entry_by_name(&mut pack, mcv_name).ok_or_else(|| anyhow!("{mcv_name} not in pack"))?;
    load_mcv_bytes(&mcv_bytes, mcv_name)
}

/// Decode a standalone `.mcv` file from disk.
pub fn load_mcv_file(path: &Path) -> anyhow::Result<Effect> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    load_mcv_bytes(&bytes, &name)
}

fn load_mcv_bytes(bytes: &[u8], mcv_name: &str) -> anyhow::Result<Effect> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("open .mcv zip")?;

    let ej: EffectJson = read_entry_by_suffix(&mut zip, "effect.json")
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let props_raw = read_entry_by_suffix(&mut zip, "properties.json").unwrap_or_default();
    let props: PropsJson = serde_json::from_slice(&props_raw).unwrap_or_default();

    let raw_type = ej.type_.clone().unwrap_or_default();
    let kind = kind_from_type(&raw_type);

    // Pull the main asset (the first file under images/main/).
    let main_idx = (0..zip.len()).find(|&i| {
        zip.by_index(i)
            .ok()
            .is_some_and(|f| f.is_file() && f.name().to_ascii_lowercase().contains("images/main/"))
    });
    let (asset_name, asset_bytes) = match main_idx {
        Some(i) => {
            let mut f = zip.by_index(i)?;
            let name = f.name().to_string();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            (name, buf)
        }
        None => return Err(anyhow!("effect '{mcv_name}' has no images/main asset")),
    };

    let is_gif = asset_name.to_ascii_lowercase().ends_with(".gif");
    let frames = if is_gif { decode_gif(&asset_bytes)? } else { vec![decode_still(&asset_bytes)?] };

    // Parse face pivots for mask effects (optional / best-effort).
    let face = if kind == Kind::FaceMask {
        Some(parse_face_mask(&props_raw))
    } else {
        None
    };

    Ok(Effect {
        name: ej.name.unwrap_or_else(|| mcv_name.trim_end_matches(".mcv").to_string()),
        author: ej.author.unwrap_or_default(),
        kind,
        raw_type,
        position: props.position.unwrap_or([0.0, 0.0]),
        size: props.size.unwrap_or([1.0, 1.0]),
        canvas_aspect: props.canvas_aspect.unwrap_or(0.0),
        repeat: props.repeat.unwrap_or(true),
        frames,
        face,
    })
}

fn decode_still(bytes: &[u8]) -> anyhow::Result<Frame> {
    let img = image::load_from_memory(bytes).context("decode image")?.to_rgba8();
    let (w, h) = img.dimensions();
    Ok(Frame { rgba: img.into_raw(), w, h, delay_ms: 0 })
}

/// Every frame of an animated WebP, composited properly (cleared where a frame says so).
fn decode_webp_anim<R: std::io::BufRead + std::io::Seek>(dec: &mut image_webp::WebPDecoder<R>) -> anyhow::Result<Vec<Frame>> {
    dec.set_background_color([0, 0, 0, 0]).context("webp background")?;
    let (w, h) = dec.dimensions();
    let has_alpha = dec.has_alpha();
    let n = dec.num_frames();
    let mut out = Vec::with_capacity(n as usize);
    let mut buf = vec![0u8; (w * h * if has_alpha { 4 } else { 3 }) as usize];
    for _ in 0..n {
        let ms = dec.read_frame(&mut buf).context("webp frame")?;
        let rgba = if has_alpha { buf.clone() } else { buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect() };
        out.push(Frame { rgba, w, h, delay_ms: ms.max(20) });
    }
    if out.is_empty() {
        anyhow::bail!("no frames");
    }
    Ok(out)
}

fn decode_gif(bytes: &[u8]) -> anyhow::Result<Vec<Frame>> {
    use image::AnimationDecoder;
    let dec = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).context("open gif")?;
    let mut out = Vec::new();
    for fr in dec.into_frames() {
        let fr = fr.context("decode gif frame")?;
        let (n, d) = fr.delay().numer_denom_ms();
        let delay_ms = if d == 0 { n } else { n / d.max(1) };
        let buf = fr.into_buffer();
        let (w, h) = buf.dimensions();
        out.push(Frame { rgba: buf.into_raw(), w, h, delay_ms: delay_ms.max(20) });
    }
    if out.is_empty() {
        return Err(anyhow!("gif had no frames"));
    }
    Ok(out)
}

fn parse_face_mask(props_raw: &[u8]) -> FaceMask {
    let v: serde_json::Value = serde_json::from_slice(props_raw).unwrap_or(serde_json::Value::Null);
    let pivots = v
        .get("face_mask_data")
        .and_then(|d| d.get("mask_data"))
        .and_then(|d| d.get("mask_pivots"))
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let x = p.get("x")?.as_f64()? as f32;
                    let y = p.get("y")?.as_f64()? as f32;
                    Some([x, y])
                })
                .collect()
        })
        .unwrap_or_default();
    FaceMask { pivots }
}
