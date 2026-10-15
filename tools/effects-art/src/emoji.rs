//! Effects made from open emoji art:
//! - Noto Animated Emoji (Google, CC BY 4.0): `noto <codepoints>`, animated WebP, 512 px.
//! - Fluent Emoji 3D (Microsoft, MIT): `fluent <Name>`, static PNG, 256 px.
//!
//! A source file `<NN Category>/<Name>.emoji` (one `key: value` per line):
//!   source: noto 1f620            (or: fluent Horse face)
//!   face: 0 0.55 3.6 3.6          (optional: center x y and size w h in face units →
//!                                  a face sticker that follows the head)
//!   size: 360                     (optional: longest side in px, default 360)
//!   flip: true                    (optional: mirror left/right)
//!   step: 2                       (optional: keep every n-th animation frame)
//! renders `<Name>.png` (APNG when animated) and `<Name>.face.json`.
//!
//! In SVG sources, `href="emoji:fluent:Name"` / `href="emoji:noto:1f31f"` is replaced by the
//! image itself (a PNG data URI; the first frame for animations), for composed effects.
//! Downloads are cached in `<assets>/cache` (fetched with curl).

use std::path::Path;

use anyhow::{anyhow, Context, Result};

pub struct Frames {
    pub w: u32,
    pub h: u32,
    /// RGBA (straight alpha) and delay in ms.
    pub frames: Vec<(Vec<u8>, u32)>,
}

fn fetch(url: &str, dst: &Path) -> Result<()> {
    if dst.is_file() && std::fs::metadata(dst)?.len() > 0 {
        return Ok(());
    }
    std::fs::create_dir_all(dst.parent().unwrap())?;
    let tmp = dst.with_extension("part");
    let ok = std::process::Command::new("curl").args(["-sfL", "--retry", "3", "-o"]).arg(&tmp).arg(url).status()?.success();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("download failed: {url}"));
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

/// Download (cached) and decode a source: `noto 1f620` or `fluent Horse face`.
pub fn load(assets: &Path, source: &str) -> Result<Frames> {
    let (kind, id) = source.trim().split_once(' ').ok_or_else(|| anyhow!("bad source {source}"))?;
    let id = id.trim();
    let cache = assets.join("cache");
    match kind {
        "noto" => {
            let file = cache.join("noto").join(format!("{id}.webp"));
            fetch(&format!("https://fonts.gstatic.com/s/e/notoemoji/latest/{id}/512.webp"), &file)?;
            decode_webp(&std::fs::read(&file)?)
        }
        "fluent" => {
            let file = cache.join("fluent").join(format!("{id}.png"));
            if !file.is_file() {
                let lower = id.to_lowercase().replace(' ', "_");
                let base = format!("https://raw.githubusercontent.com/microsoft/fluentui-emoji/main/assets/{}", id.replace(' ', "%20"));
                fetch(&format!("{base}/3D/{lower}_3d.png"), &file).or_else(|_| fetch(&format!("{base}/Default/3D/{lower}_3d_default.png"), &file))?;
            }
            let img = image::load_from_memory(&std::fs::read(&file)?)?.to_rgba8();
            let (w, h) = img.dimensions();
            Ok(Frames { w, h, frames: vec![(img.into_raw(), 100)] })
        }
        k => Err(anyhow!("unknown emoji source {k}")),
    }
}

pub fn decode_webp(bytes: &[u8]) -> Result<Frames> {
    // image-webp directly with a transparent background: image's wrapper leaves frames that
    // should be cleared on the canvas (ghost trails).
    let mut dec = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes))?;
    dec.set_background_color([0, 0, 0, 0])?;
    let (w, h) = dec.dimensions();
    let alpha = dec.has_alpha();
    let mut buf = vec![0u8; (w * h * if alpha { 4 } else { 3 }) as usize];
    let mut frames = Vec::new();
    if dec.is_animated() {
        for _ in 0..dec.num_frames() {
            let ms = dec.read_frame(&mut buf)?;
            frames.push((to_rgba(&buf, alpha), ms.max(10)));
        }
    } else {
        dec.read_image(&mut buf)?;
        frames.push((to_rgba(&buf, alpha), 100));
    }
    if frames.is_empty() {
        return Err(anyhow!("no frames"));
    }
    Ok(Frames { w, h, frames })
}

fn to_rgba(buf: &[u8], alpha: bool) -> Vec<u8> {
    if alpha {
        buf.to_vec()
    } else {
        buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
    }
}

/// Resize with premultiplied alpha (no dark fringes), longest side `long`.
fn resize(rgba: &[u8], w: u32, h: u32, long: u32, flip: bool) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = if w >= h { (long, (long * h / w).max(1)) } else { ((long * w / h).max(1), long) };
    let mut pre = image::Rgba32FImage::new(w, h);
    for (i, p) in pre.pixels_mut().enumerate() {
        let s = &rgba[i * 4..i * 4 + 4];
        let a = s[3] as f32 / 255.0;
        *p = image::Rgba([s[0] as f32 / 255.0 * a, s[1] as f32 / 255.0 * a, s[2] as f32 / 255.0 * a, a]);
    }
    let mut out = if (nw, nh) == (w, h) { pre } else { image::imageops::resize(&pre, nw, nh, image::imageops::FilterType::Lanczos3) };
    if flip {
        image::imageops::flip_horizontal_in_place(&mut out);
    }
    let mut v = Vec::with_capacity((nw * nh * 4) as usize);
    for p in out.pixels() {
        let a = p[3].clamp(0.0, 1.0);
        let un = |c: f32| if a > 0.001 { (c / a).clamp(0.0, 1.0) } else { 0.0 };
        v.extend_from_slice(&[(un(p[0]) * 255.0).round() as u8, (un(p[1]) * 255.0).round() as u8, (un(p[2]) * 255.0).round() as u8, (a * 255.0).round() as u8]);
    }
    (v, nw, nh)
}

pub fn write_png(path: &Path, w: u32, h: u32, frames: &[(Vec<u8>, u32)]) -> Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    if frames.len() > 1 {
        enc.set_animated(frames.len() as u32, 0)?;
    }
    let mut wr = enc.write_header()?;
    for (rgba, ms) in frames {
        if frames.len() > 1 {
            wr.set_frame_delay((*ms).min(65535) as u16, 1000)?;
        }
        wr.write_image_data(rgba)?;
    }
    wr.finish()?;
    Ok(())
}

/// Animated frames → a small looping animated WebP via ffmpeg (libwebp, lossy + alpha).
fn write_webp(ffmpeg: &Path, path: &Path, w: u32, h: u32, frames: &[(Vec<u8>, u32)]) -> Result<()> {
    use std::io::Write;
    let mut d: Vec<u32> = frames.iter().map(|f| f.1).collect();
    d.sort();
    let fps = 1000.0 / d[d.len() / 2].max(10) as f32;
    let mut child = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{w}x{h}"), "-r", &format!("{fps:.3}"), "-i", "-"])
        .args(["-c:v", "libwebp_anim", "-lossless", "0", "-quality", "82", "-loop", "0", "-pix_fmt", "yuva420p"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| anyhow!("ffmpeg stdin"))?;
        for (rgba, _) in frames {
            stdin.write_all(rgba)?;
        }
    }
    drop(child.stdin.take());
    if !child.wait()?.success() {
        return Err(anyhow!("ffmpeg webp failed"));
    }
    Ok(())
}

/// Render one `.emoji` source into `dst`: a PNG, or an animated WebP when animated.
pub fn render(assets: &Path, src: &Path, dst: &Path, ffmpeg: Option<&Path>) -> Result<()> {
    let text = std::fs::read_to_string(src)?;
    let get = |k: &str| text.lines().find_map(|l| l.split_once(':').filter(|(a, _)| a.trim() == k).map(|(_, b)| b.trim().to_string()));
    let source = get("source").ok_or_else(|| anyhow!("no source"))?;
    let long: u32 = get("size").and_then(|s| s.parse().ok()).unwrap_or(320);
    let flip = get("flip").is_some_and(|s| s == "true");
    let fr = load(assets, &source).with_context(|| source.clone())?;
    // Long animations: keep at most ~75 frames (memory while the effect is on).
    let step: usize = get("step").and_then(|s| s.parse().ok()).unwrap_or(fr.frames.len().div_ceil(75)).max(1);
    let mut out = Vec::new();
    let (mut w, mut h) = (fr.w, fr.h);
    let mut acc = 0;
    for (i, (rgba, ms)) in fr.frames.iter().enumerate() {
        acc += ms;
        if i % step != step - 1 && i + 1 != fr.frames.len() {
            continue;
        }
        let (v, nw, nh) = resize(rgba, fr.w, fr.h, long.min(fr.w.max(fr.h)), flip);
        (w, h) = (nw, nh);
        out.push((v, acc));
        acc = 0;
    }
    let name = src.file_stem().unwrap().to_string_lossy().into_owned();
    let _ = std::fs::remove_file(dst.join(format!("{name}.png")));
    let _ = std::fs::remove_file(dst.join(format!("{name}.webp")));
    match ffmpeg {
        Some(ff) if out.len() > 1 => write_webp(ff, &dst.join(format!("{name}.webp")), w, h, &out)?,
        _ => write_png(&dst.join(format!("{name}.png")), w, h, &out)?,
    }
    if let Some(face) = get("face") {
        let v: Vec<f32> = face.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        if v.len() != 4 {
            return Err(anyhow!("face: needs cx cy w h"));
        }
        let json = serde_json::json!({ "center": [v[0], v[1]], "size": [v[2], v[3]] });
        std::fs::write(dst.join(format!("{name}.face.json")), serde_json::to_vec_pretty(&json)?)?;
    }
    Ok(())
}

/// Replace `href="emoji:<kind>:<id>"` in SVG text with PNG data URIs.
pub fn inline_images(assets: &Path, svg: &str) -> Result<String> {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(i) = rest.find("\"emoji:") {
        out.push_str(&rest[..i + 1]);
        let j = rest[i + 1..].find('"').ok_or_else(|| anyhow!("unclosed emoji href"))? + i + 1;
        let spec = &rest[i + 7..j];
        let (kind, id) = spec.split_once(':').ok_or_else(|| anyhow!("bad emoji href {spec}"))?;
        let fr = load(assets, &format!("{kind} {id}"))?;
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, fr.w, fr.h);
            enc.set_color(png::ColorType::Rgba);
            enc.write_header()?.write_image_data(&fr.frames[0].0)?;
        }
        out.push_str("data:image/png;base64,");
        out.push_str(&base64(&png));
        rest = &rest[j..];
    }
    out.push_str(rest);
    Ok(out)
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                s.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}
