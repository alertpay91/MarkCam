//! RGBA → output pixel formats, plus aspect-preserving scaling. Everything checks buffer
//! sizes up front and does nothing on a mismatch: this code runs inside other people's
//! processes (ClassIn, Zoom…) and the release profile aborts on panic, so it must never
//! index out of bounds.

use crate::formats::Sub;

/// BT.601 limited-range RGB → Y'CbCr (the convention webcams and video apps assume).
#[inline]
fn yuv(r: i32, g: i32, b: i32) -> (u8, u8, u8) {
    let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
    let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
    let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
    (y.clamp(0, 255) as u8, u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
}

/// Convert a `w`x`h` RGBA frame into `dst` in format `sub`. `dst` must be exactly the
/// format's image size.
pub fn convert(src: &[u8], w: u32, h: u32, sub: Sub, dst: &mut [u8]) {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || src.len() < w * h * 4 {
        return;
    }
    match sub {
        Sub::Nv12 => to_nv12(src, w, h, dst),
        Sub::Yuy2 => to_yuy2(src, w, h, dst),
        Sub::Rgb32 => to_bgra_bottom_up(src, w, h, dst),
    }
}

fn to_nv12(src: &[u8], w: usize, h: usize, dst: &mut [u8]) {
    if dst.len() < w * h * 3 / 2 {
        return;
    }
    let (y_plane, uv_plane) = dst.split_at_mut(w * h);
    for (y, (row_a, row_b)) in src
        .chunks_exact(w * 4)
        .step_by(2)
        .zip(src.chunks_exact(w * 4).skip(1).step_by(2))
        .enumerate()
    {
        let ya = &mut y_plane[2 * y * w..(2 * y + 1) * w];
        for (px, o) in row_a.chunks_exact(4).zip(ya.iter_mut()) {
            *o = yuv(px[0] as i32, px[1] as i32, px[2] as i32).0;
        }
        let yb = &mut y_plane[(2 * y + 1) * w..(2 * y + 2) * w];
        for (px, o) in row_b.chunks_exact(4).zip(yb.iter_mut()) {
            *o = yuv(px[0] as i32, px[1] as i32, px[2] as i32).0;
        }
        // Chroma: average each 2x2 block.
        let uv = &mut uv_plane[y * w..(y + 1) * w];
        for ((a, b), o) in row_a.chunks_exact(8).zip(row_b.chunks_exact(8)).zip(uv.chunks_exact_mut(2)) {
            let r = (a[0] as i32 + a[4] as i32 + b[0] as i32 + b[4] as i32) / 4;
            let g = (a[1] as i32 + a[5] as i32 + b[1] as i32 + b[5] as i32) / 4;
            let bl = (a[2] as i32 + a[6] as i32 + b[2] as i32 + b[6] as i32) / 4;
            let (_, u, v) = yuv(r, g, bl);
            o[0] = u;
            o[1] = v;
        }
    }
}

fn to_yuy2(src: &[u8], w: usize, h: usize, dst: &mut [u8]) {
    if dst.len() < w * h * 2 {
        return;
    }
    for (s, d) in src.chunks_exact(8).zip(dst.chunks_exact_mut(4)) {
        let (y0, u0, v0) = yuv(s[0] as i32, s[1] as i32, s[2] as i32);
        let (y1, u1, v1) = yuv(s[4] as i32, s[5] as i32, s[6] as i32);
        d[0] = y0;
        d[1] = ((u0 as u16 + u1 as u16) / 2) as u8;
        d[2] = y1;
        d[3] = ((v0 as u16 + v1 as u16) / 2) as u8;
    }
}

fn to_bgra_bottom_up(src: &[u8], w: usize, h: usize, dst: &mut [u8]) {
    if dst.len() < w * h * 4 {
        return;
    }
    let stride = w * 4;
    for (y, srow) in src.chunks_exact(stride).take(h).enumerate() {
        let drow = &mut dst[(h - 1 - y) * stride..(h - y) * stride];
        for (s, d) in srow.chunks_exact(4).zip(drow.chunks_exact_mut(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = 255;
        }
    }
}

/// Nearest-neighbour scale `src` (`sw`x`sh` RGBA) into a `dw`x`dh` RGBA frame, preserving
/// aspect ratio with black bars. `out` is resized as needed and reused across calls.
pub fn fit_scale(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32, out: &mut Vec<u8>) {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    out.clear();
    out.resize(dw * dh * 4, 0);
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || src.len() < sw * sh * 4 {
        return;
    }
    // Fitted rectangle inside the output.
    let (fw, fh) = if sw * dh > sh * dw { (dw, (sh * dw / sw).max(1)) } else { ((sw * dh / sh).max(1), dh) };
    let (ox, oy) = ((dw - fw) / 2, (dh - fh) / 2);
    let xmap: Vec<usize> = (0..fw).map(|x| (x * sw / fw).min(sw - 1) * 4).collect();
    for y in 0..fh {
        let sy = (y * sh / fh).min(sh - 1);
        let srow = &src[sy * sw * 4..(sy + 1) * sw * 4];
        let drow = &mut out[((oy + y) * dw + ox) * 4..((oy + y) * dw + ox + fw) * 4];
        for (d, &sx) in drow.chunks_exact_mut(4).zip(xmap.iter()) {
            d.copy_from_slice(&srow[sx..sx + 4]);
        }
    }
    // Opaque black bars.
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn red_to_nv12() {
        let src: Vec<u8> = [255u8, 0, 0, 255].repeat(4 * 2);
        let mut dst = vec![0u8; 4 * 2 * 3 / 2];
        convert(&src, 4, 2, Sub::Nv12, &mut dst);
        assert_eq!(dst[0], 82); // Y of pure red, BT.601 limited
        assert_eq!(&dst[8..10], &[90, 240]); // U, V
    }

    #[test]
    fn letterbox_keeps_aspect() {
        let src = vec![200u8; 4 * 4 * 4]; // 4x4 square
        let mut out = Vec::new();
        fit_scale(&src, 4, 4, 8, 4, &mut out); // into 8x4 → 4x4 centred, bars left/right
        assert_eq!(out[0], 0); // bar
        assert_eq!(out[(2 * 4) * 1], 200); // x=2 inside
    }
}
