//! CPU color conversion from the pipeline's working format (RGBA8) to the webcam output
//! formats. This is the baseline path; `ovc-gpu` will do the same conversion in a shader.
//!
//! BT.601 limited-range. Width and height are assumed even for 4:2:0 subsampling.

use crate::PixelFormat;

#[inline]
fn rgb_to_yuv(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let r = r as f32;
    let g = g as f32;
    let b = b as f32;
    let y = 0.257 * r + 0.504 * g + 0.098 * b + 16.0;
    let u = -0.148 * r - 0.291 * g + 0.439 * b + 128.0;
    let v = 0.439 * r - 0.368 * g - 0.071 * b + 128.0;
    (clamp8(y), clamp8(u), clamp8(v))
}

#[inline]
fn clamp8(x: f32) -> u8 {
    x.round().clamp(0.0, 255.0) as u8
}

#[inline]
fn luma(r: u8, g: u8, b: u8) -> u8 {
    clamp8(0.257 * r as f32 + 0.504 * g as f32 + 0.098 * b as f32 + 16.0)
}

/// Convert a tightly packed RGBA8 buffer (`w*h*4` bytes) into `dst` in `format`.
/// `dst` is resized to the exact frame size. Returns the number of bytes written.
pub fn rgba_to(format: PixelFormat, rgba: &[u8], w: u32, h: u32, dst: &mut Vec<u8>) -> usize {
    let size = format.frame_size(w, h);
    dst.clear();
    dst.resize(size, 0);
    let (wi, hi) = (w as usize, h as usize);
    match format {
        PixelFormat::Rgba8 => {
            dst[..rgba.len().min(size)].copy_from_slice(&rgba[..rgba.len().min(size)]);
        }
        PixelFormat::Bgra8 => {
            for (px, out) in rgba.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                out[0] = px[2];
                out[1] = px[1];
                out[2] = px[0];
                out[3] = px[3];
            }
        }
        PixelFormat::Yuy2 => {
            // Packed Y0 U Y1 V per 2 horizontal pixels.
            for y in 0..hi {
                for x in (0..wi).step_by(2) {
                    let i0 = (y * wi + x) * 4;
                    let i1 = i0 + 4;
                    let (y0, u0, v0) = rgb_to_yuv(rgba[i0], rgba[i0 + 1], rgba[i0 + 2]);
                    let (y1, u1, v1) = rgb_to_yuv(rgba[i1], rgba[i1 + 1], rgba[i1 + 2]);
                    let o = (y * wi + x) * 2;
                    dst[o] = y0;
                    dst[o + 1] = ((u0 as u16 + u1 as u16) / 2) as u8;
                    dst[o + 2] = y1;
                    dst[o + 3] = ((v0 as u16 + v1 as u16) / 2) as u8;
                }
            }
        }
        PixelFormat::Nv12 | PixelFormat::I420 => {
            let y_size = wi * hi;
            let c_w = wi / 2;
            let c_h = hi / 2;
            // Y plane.
            for y in 0..hi {
                for x in 0..wi {
                    let i = (y * wi + x) * 4;
                    dst[y * wi + x] = luma(rgba[i], rgba[i + 1], rgba[i + 2]);
                }
            }
            // Chroma: average each 2x2 block.
            for cy in 0..c_h {
                for cx in 0..c_w {
                    let (mut su, mut sv) = (0u32, 0u32);
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let px = (cy * 2 + dy) * wi + (cx * 2 + dx);
                            let i = px * 4;
                            let (_, u, v) = rgb_to_yuv(rgba[i], rgba[i + 1], rgba[i + 2]);
                            su += u as u32;
                            sv += v as u32;
                        }
                    }
                    let (u, v) = ((su / 4) as u8, (sv / 4) as u8);
                    match format {
                        PixelFormat::Nv12 => {
                            let o = y_size + (cy * c_w + cx) * 2;
                            dst[o] = u;
                            dst[o + 1] = v;
                        }
                        PixelFormat::I420 => {
                            dst[y_size + cy * c_w + cx] = u;
                            dst[y_size + c_w * c_h + cy * c_w + cx] = v;
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
    }
    size
}
