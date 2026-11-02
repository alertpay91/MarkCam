//! MarkCam's effect files: plain images and animations (PNG, APNG, JPG, GIF, WebP), with an
//! optional `<name>.face.json` beside a picture that makes it a face sticker. Decodes them
//! into frames, lists a folder of them and makes small thumbnails for the browser.

use std::io::Cursor;
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
    /// A still image placed on the picture (sticker or full-screen border).
    Object,
    /// An animation placed on the picture.
    Animation,
    /// A face sticker: follows the face (needs face tracking to apply).
    FaceMask,
}

/// A fully decoded effect ready to composite.
pub struct Effect {
    pub name: String,
    pub kind: Kind,
    /// A large ~16:9 picture: drawn over the whole frame (a border).
    pub fullscreen: bool,
    /// Face stickers: center offset and size in face units (see [`load_image_effect`]).
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub repeat: bool,
    pub frames: Vec<Frame>,
}

/// A lightweight reference to one effect file (name/type only — no image decode), cheap
/// enough to list a whole folder for a browser UI.
#[derive(Debug, Clone)]
pub struct EffectRef {
    /// The category it is listed under (its folder).
    pub category: String,
    pub display_name: String,
    pub kind: Kind,
    pub file: PathBuf,
}

/// Load the effect an [`EffectRef`] points at.
pub fn load_effect_ref(r: &EffectRef) -> anyhow::Result<Effect> {
    load_image_effect(&r.file)
}

/// The effect's first frame, shrunk to thumbnail size, for a browser grid.
pub fn load_thumbnail(r: &EffectRef) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::open(&r.file).ok()?.thumbnail(200, 150).to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
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
        // A face sticker: center offset + size in face units.
        Some(f) => (Kind::FaceMask, f.center, f.size),
        None => (if frames.len() > 1 { Kind::Animation } else { Kind::Object }, [0.0, 0.0], [1.0, 1.0]),
    };
    Ok(Effect {
        name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        kind,
        fullscreen: fullscreen && face.is_none(),
        position,
        size,
        repeat: true,
        frames,
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
                category: category.to_string(),
                display_name: p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                kind: if face_placement_file(&p).is_file() {
                    Kind::FaceMask
                } else if gif {
                    Kind::Animation
                } else {
                    Kind::Object
                },
                file: p,
            }
        })
        .collect();
    v.sort_by(|a, b| natural_cmp(&a.display_name, &b.display_name));
    v
}

/// Case-insensitive order where numbers count as numbers ("Number 2" before "Number 10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let num = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut n = 0u64;
                    while let Some(d) = it.peek().and_then(|c| c.to_digit(10)) {
                        n = n.saturating_mul(10).saturating_add(d as u64);
                        it.next();
                    }
                    n
                };
                let (na, nb) = (num(&mut a), num(&mut b));
                if na != nb {
                    return na.cmp(&nb);
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                a.next();
                b.next();
            }
        }
    }
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

#[cfg(test)]
mod natural_order {
    #[test]
    fn numbers_sort_as_numbers() {
        let mut v = vec!["Number 10", "number 2", "Letter B", "Number 1", "letter a"];
        v.sort_by(|a, b| super::natural_cmp(a, b));
        assert_eq!(v, ["letter a", "Letter B", "Number 1", "number 2", "Number 10"]);
    }
}
