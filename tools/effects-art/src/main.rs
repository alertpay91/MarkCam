//! Render MarkCam's own effects and backgrounds from SVG sources.
//!
//!   cargo run --release --manifest-path tools/effects-art/Cargo.toml -- <assets dir> <out dir> [ffmpeg.exe]
//!
//! Sources (`<assets>/effects/<NN Category>/<Name>.svg`, `<assets>/backgrounds/<Name>.svg`):
//! - `data-face="true"` on the root: a face sticker. Its viewBox is in *face units* (origin
//!   between the eyes, +y down, eyes at x = ±0.6, mouth ≈ +1.2, chin ≈ +2.1, top of the head
//!   ≈ −1.9); the placement file `<Name>.face.json` is written from it.
//! - `data-frames="N" data-ms="M"`: animated — rendered N times with t = i/N, writing an
//!   APNG (effects) or an MP4 via ffmpeg (backgrounds). Inside the SVG text:
//!   `{{lin a b}}` = a→b over the loop, `{{osc a b}}` = a↔b (smooth, once per loop),
//!   `{{osc2 a b}}` = twice per loop, `{{ph p a b}}` = like osc but shifted by p (0..1),
//!   `{{loop p a b}}` = a→b repeating, shifted by p.
//! Output: `<out>/effects/<NN Category>/<Name>.png (+ .face.json)`, `<out>/backgrounds/…`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

mod emoji;
mod strands;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // Debug: effects-art dump <animated.webp> <out dir> — every decoded frame as PNG.
    if args.get(1).map(String::as_str) == Some("dump") {
        let fr = emoji::decode_webp(&std::fs::read(&args[2])?)?;
        std::fs::create_dir_all(&args[3])?;
        for (i, (rgba, _)) in fr.frames.into_iter().enumerate() {
            image::RgbaImage::from_raw(fr.w, fr.h, rgba).unwrap().save(PathBuf::from(&args[3]).join(format!("f{i:03}.png")))?;
        }
        return Ok(());
    }
    let assets = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("usage: effects-art <assets> <out> [ffmpeg]"))?);
    let out = PathBuf::from(args.get(2).ok_or_else(|| anyhow!("missing out dir"))?);
    let ffmpeg = args.get(3).map(PathBuf::from);
    let mut opt = resvg::usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();

    let mut n = 0;
    for cat in sorted_dirs(&assets.join("effects"))? {
        let dst = out.join("effects").join(cat.file_name().unwrap());
        std::fs::create_dir_all(&dst)?;
        for svg in sorted_files(&cat, "svg")? {
            // ART_ONLY=part,part: render only effects whose file name contains one of them.
            if let Ok(only) = std::env::var("ART_ONLY") {
                let name = svg.file_stem().unwrap().to_string_lossy().to_lowercase();
                if !only.split(',').any(|p| name.contains(&p.trim().to_lowercase())) {
                    continue;
                }
            }
            render_effect(&assets, &svg, &dst, &opt).with_context(|| format!("{}", svg.display()))?;
            n += 1;
        }
        for src in sorted_files(&cat, "emoji")? {
            if let Ok(only) = std::env::var("ART_ONLY") {
                let name = src.file_stem().unwrap().to_string_lossy().to_lowercase();
                if !only.split(",").any(|p| name.contains(&p.trim().to_lowercase())) {
                    continue;
                }
            }
            // A missing download is reported, not fatal (the other effects still render).
            match emoji::render(&assets, &src, &dst, ffmpeg.as_deref()) {
                Ok(()) => n += 1,
                Err(e) => eprintln!("SKIPPED {}: {e:#}", src.display()),
            }
        }
    }
    let bg_dir = assets.join("backgrounds");
    if bg_dir.is_dir() && std::env::var("ART_ONLY").is_err() {
        let dst = out.join("backgrounds");
        std::fs::create_dir_all(&dst)?;
        for svg in sorted_files(&bg_dir, "svg")? {
            render_background(&svg, &dst, &opt, ffmpeg.as_deref()).with_context(|| format!("{}", svg.display()))?;
            n += 1;
        }
    }
    println!("rendered {n} items into {}", out.display());
    Ok(())
}

fn sorted_dirs(d: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d)?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort();
    Ok(v)
}

fn sorted_files(d: &Path, ext: &str) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
        .collect();
    v.sort();
    Ok(v)
}

/// A root attribute's value (`name="value"`), from the raw SVG text.
fn root_attr(svg: &str, name: &str) -> Option<String> {
    let start = svg.find("<svg")?;
    let end = start + svg[start..].find('>')?;
    let tag = &svg[start..end];
    let key = format!("{name}=\"");
    let i = tag.find(&key)? + key.len();
    let j = i + tag[i..].find('"')?;
    Some(tag[i..j].to_string())
}

/// Expand the `{{…}}` animation expressions for time `t` (0..1).
fn expand(svg: &str, t: f32) -> Result<String> {
    use std::f32::consts::TAU;
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let j = rest[i..].find("}}").ok_or_else(|| anyhow!("unclosed {{"))? + i;
        let parts: Vec<&str> = rest[i + 2..j].split_whitespace().collect();
        let num = |k: usize| -> Result<f32> { parts.get(k).ok_or_else(|| anyhow!("missing number"))?.parse::<f32>().map_err(Into::into) };
        let v = match parts.first().copied() {
            Some("lin") => num(1)? + (num(2)? - num(1)?) * t,
            Some("osc") => num(1)? + (num(2)? - num(1)?) * (0.5 - 0.5 * (TAU * t).cos()),
            Some("osc2") => num(1)? + (num(2)? - num(1)?) * (0.5 - 0.5 * (2.0 * TAU * t).cos()),
            Some("ph") => num(2)? + (num(3)? - num(2)?) * (0.5 - 0.5 * (TAU * (t + num(1)?)).cos()),
            Some("loop") => num(2)? + (num(3)? - num(2)?) * (t + num(1)?).fract(),
            other => return Err(anyhow!("unknown expression {other:?}")),
        };
        out.push_str(&format!("{v:.4}"));
        rest = &rest[j + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Rasterize SVG text to RGBA (premultiplied → straight) at `w`x`h`.
fn raster(svg: &str, opt: &resvg::usvg::Options, w: u32, h: u32) -> Result<Vec<u8>> {
    let tree = resvg::usvg::Tree::from_str(svg, opt)?;
    let size = tree.size();
    let mut pix = resvg::tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow!("pixmap"))?;
    let tf = resvg::tiny_skia::Transform::from_scale(w as f32 / size.width(), h as f32 / size.height());
    resvg::render(&tree, tf, &mut pix.as_mut());
    let mut data = pix.take();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in 0..3 {
                px[c] = ((px[c] as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Ok(data)
}

/// Pixel size for a viewBox aspect: the longer side `long` px.
fn pixel_size(svg: &str, long: u32) -> Result<(u32, u32)> {
    let vb = root_attr(svg, "viewBox").ok_or_else(|| anyhow!("no viewBox"))?;
    let v: Vec<f32> = vb.split(|c: char| c == ' ' || c == ',').filter(|s| !s.is_empty()).map(|s| s.parse().unwrap_or(0.0)).collect();
    let (w, h) = (v[2], v[3]);
    Ok(if w >= h { (long, ((long as f32 * h / w).round() as u32).max(1)) } else { (((long as f32 * w / h).round() as u32).max(1), long) })
}

fn render_effect(assets: &Path, svg_path: &Path, dst: &Path, opt: &resvg::usvg::Options) -> Result<()> {
    let svg = emoji::inline_images(assets, &strands::expand(&std::fs::read_to_string(svg_path)?)?)?;
    let name = svg_path.file_stem().unwrap().to_string_lossy().into_owned();
    let fullscreen = root_attr(&svg, "data-fullscreen").is_some();
    let (w, h) = pixel_size(&svg, if fullscreen { 1920 } else { 640 })?;
    let frames: u32 = root_attr(&svg, "data-frames").and_then(|s| s.parse().ok()).unwrap_or(1);
    let ms: u16 = root_attr(&svg, "data-ms").and_then(|s| s.parse().ok()).unwrap_or(50);
    let png_path = dst.join(format!("{name}.png"));
    let file = std::io::BufWriter::new(std::fs::File::create(&png_path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    if frames > 1 {
        enc.set_animated(frames, 0)?;
        enc.set_frame_delay(ms, 1000)?;
    }
    let mut wr = enc.write_header()?;
    for i in 0..frames {
        let text = expand(&svg, i as f32 / frames as f32)?;
        wr.write_image_data(&raster(&text, opt, w, h)?)?;
    }
    wr.finish()?;
    if root_attr(&svg, "data-face").is_some() {
        let vb = root_attr(&svg, "viewBox").unwrap_or_default();
        let v: Vec<f32> = vb.split(|c: char| c == ' ' || c == ',').filter(|s| !s.is_empty()).map(|s| s.parse().unwrap_or(0.0)).collect();
        let json = serde_json::json!({ "center": [v[0] + v[2] / 2.0, v[1] + v[3] / 2.0], "size": [v[2], v[3]] });
        std::fs::write(dst.join(format!("{name}.face.json")), serde_json::to_vec_pretty(&json)?)?;
    }
    Ok(())
}

fn render_background(svg_path: &Path, dst: &Path, opt: &resvg::usvg::Options, ffmpeg: Option<&Path>) -> Result<()> {
    let svg = std::fs::read_to_string(svg_path)?;
    let name = svg_path.file_stem().unwrap().to_string_lossy().into_owned();
    let (w, h) = (1280u32, 720u32);
    let frames: u32 = root_attr(&svg, "data-frames").and_then(|s| s.parse().ok()).unwrap_or(1);
    if frames <= 1 {
        let rgba = raster(&expand(&svg, 0.0)?, opt, w, h)?;
        let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        let file = std::io::BufWriter::new(std::fs::File::create(dst.join(format!("{name}.png")))?);
        let mut enc = png::Encoder::new(file, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_compression(png::Compression::High);
        enc.write_header()?.write_image_data(&rgb)?;
        return Ok(());
    }
    // Animated: raw frames piped into ffmpeg → a looping H.264 MP4.
    let ffmpeg = ffmpeg.ok_or_else(|| anyhow!("animated background {name} needs ffmpeg"))?;
    let ms: u32 = root_attr(&svg, "data-ms").and_then(|s| s.parse().ok()).unwrap_or(40);
    let fps = (1000 / ms.max(1)).max(1);
    let out = dst.join(format!("{name}.mp4"));
    let mut child = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{w}x{h}"), "-r", &fps.to_string(), "-i", "-"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "24", "-preset", "slow", "-movflags", "+faststart"])
        .arg(&out)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().ok_or_else(|| anyhow!("ffmpeg stdin"))?;
        for i in 0..frames {
            stdin.write_all(&raster(&expand(&svg, i as f32 / frames as f32)?, opt, w, h)?)?;
        }
    }
    drop(child.stdin.take());
    if !child.wait()?.success() {
        return Err(anyhow!("ffmpeg failed for {name}"));
    }
    Ok(())
}
