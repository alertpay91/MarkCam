//! Piecewise-affine mask warping.
//!
//! A ManyCam face mask is an image plus a set of *pivot* points, each normalized to the
//! mask image and corresponding to a facial landmark index. To wear the mask, we map every
//! pivot onto the matching detected landmark, Delaunay-triangulate the landmark mesh, and
//! affine-warp each mask triangle onto the frame — the standard face-mask/face-swap method.
//!
//! This stage is deterministic and model-free (landmarks come from a detector elsewhere),
//! so it is unit-tested directly without a camera or ONNX model.

/// Warp `mask_rgba` (`mw`×`mh`, RGBA8) onto `frame_rgba` (`fw`×`fh`, RGBA8) so that each
/// mask pivot (normalized 0..1 over the mask) lands on the corresponding destination point
/// (in frame pixels). Composited with the mask's own alpha × `opacity`.
///
/// `pivots_norm` and `dst_pts` are parallel arrays (same index = same landmark). At least 3
/// points are required.
#[allow(clippy::too_many_arguments)]
pub fn warp_mask_onto(
    frame_rgba: &mut [u8],
    fw: u32,
    fh: u32,
    mask_rgba: &[u8],
    mw: u32,
    mh: u32,
    pivots_norm: &[[f32; 2]],
    dst_pts: &[[f32; 2]],
    opacity: f32,
) {
    let n = pivots_norm.len().min(dst_pts.len());
    if n < 3 || fw == 0 || fh == 0 || mw == 0 || mh == 0 {
        return;
    }
    // Pivots in mask pixel space.
    let mut src: Vec<[f32; 2]> = pivots_norm[..n].iter().map(|p| [p[0] * mw as f32, p[1] * mh as f32]).collect();
    let mut dst: Vec<[f32; 2]> = dst_pts[..n].to_vec();

    // The landmarks only span brows → chin, so a mesh over them alone cuts off everything
    // beyond the face (cat ears, hats, hair). Like ManyCam's "extra rect": fit the best
    // similarity transform pivots → landmarks and carry the mask image's corners and edge
    // midpoints along with it, so the whole mask image is meshed.
    if let Some(t) = similarity(&src, &dst) {
        let (w, h) = (mw as f32, mh as f32);
        for p in [[0.0, 0.0], [w / 2.0, 0.0], [w, 0.0], [w, h / 2.0], [w, h], [w / 2.0, h], [0.0, h], [0.0, h / 2.0]] {
            src.push(p);
            dst.push(t(p));
        }
    }

    let pts: Vec<delaunator::Point> = dst.iter().map(|p| delaunator::Point { x: p[0] as f64, y: p[1] as f64 }).collect();
    let tri = delaunator::triangulate(&pts);
    if tri.triangles.is_empty() {
        return;
    }
    let op = opacity.clamp(0.0, 1.0);

    // Rasterize the warped mask into a coverage layer first (each pixel written, not blended)
    // so pixels on shared triangle edges aren't composited twice. Then composite once. The
    // layer only covers the mesh's bounding box, not the whole frame.
    let bx0 = dst.iter().map(|p| p[0]).fold(f32::MAX, f32::min).floor().clamp(0.0, (fw - 1) as f32) as u32;
    let by0 = dst.iter().map(|p| p[1]).fold(f32::MAX, f32::min).floor().clamp(0.0, (fh - 1) as f32) as u32;
    let bx1 = dst.iter().map(|p| p[0]).fold(f32::MIN, f32::max).ceil().clamp(0.0, (fw - 1) as f32) as u32;
    let by1 = dst.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil().clamp(0.0, (fh - 1) as f32) as u32;
    if bx1 < bx0 || by1 < by0 {
        return;
    }
    let (lw, lh) = (bx1 - bx0 + 1, by1 - by0 + 1);
    let shift = |p: [f32; 2]| [p[0] - bx0 as f32, p[1] - by0 as f32];
    let mut layer = vec![0u8; (lw * lh * 4) as usize];
    let mut min_x = lw;
    let mut min_y = lh;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for t in tri.triangles.chunks_exact(3) {
        let (a, b, c) = (t[0], t[1], t[2]);
        rasterize_triangle(
            &mut layer,
            lw,
            lh,
            mask_rgba,
            mw,
            mh,
            shift(dst[a]),
            shift(dst[b]),
            shift(dst[c]),
            src[a],
            src[b],
            src[c],
            (&mut min_x, &mut min_y, &mut max_x, &mut max_y),
        );
    }
    if max_x < min_x || max_y < min_y {
        return;
    }
    // Integer "over" blend: alpha·opacity in 0..=255, x/255 via (x + 128 + (x+128)>>8) >> 8.
    let op = (op * 256.0).round() as u32;
    let div255 = |x: u32| (x + 128 + ((x + 128) >> 8)) >> 8;
    for ly in min_y..=max_y {
        let lrow = &layer[((ly * lw + min_x) * 4) as usize..((ly * lw + max_x + 1) * 4) as usize];
        let at = (((ly + by0) * fw + min_x + bx0) * 4) as usize;
        let frow = &mut frame_rgba[at..at + lrow.len()];
        for (src, dst) in lrow.chunks_exact(4).zip(frow.chunks_exact_mut(4)) {
            let a = (src[3] as u32 * op) >> 8;
            if a == 0 {
                continue;
            }
            for c in 0..3 {
                dst[c] = div255(dst[c] as u32 * (255 - a) + src[c] as u32 * a) as u8;
            }
            dst[3] = (a + div255(dst[3] as u32 * (255 - a))) as u8;
        }
    }
}

/// Least-squares similarity transform (uniform scale + rotation + translation) mapping
/// `src` onto `dst` (2D Umeyama / Procrustes, closed form).
fn similarity(src: &[[f32; 2]], dst: &[[f32; 2]]) -> Option<impl Fn([f32; 2]) -> [f32; 2]> {
    let n = src.len().min(dst.len()) as f32;
    if n < 2.0 {
        return None;
    }
    let mean = |v: &[[f32; 2]]| {
        let (sx, sy) = v.iter().fold((0.0, 0.0), |a, p| (a.0 + p[0], a.1 + p[1]));
        [sx / n, sy / n]
    };
    let (cs, cd) = (mean(src), mean(dst));
    let (mut a, mut b, mut ss) = (0.0f32, 0.0f32, 0.0f32);
    for (s, d) in src.iter().zip(dst) {
        let (sx, sy, dx, dy) = (s[0] - cs[0], s[1] - cs[1], d[0] - cd[0], d[1] - cd[1]);
        a += sx * dx + sy * dy;
        b += sx * dy - sy * dx;
        ss += sx * sx + sy * sy;
    }
    if ss < 1e-6 {
        return None;
    }
    let (a, b) = (a / ss, b / ss);
    Some(move |p: [f32; 2]| {
        let (x, y) = (p[0] - cs[0], p[1] - cs[1]);
        [a * x - b * y + cd[0], b * x + a * y + cd[1]]
    })
}

#[allow(clippy::too_many_arguments)]
fn rasterize_triangle(
    layer: &mut [u8],
    fw: u32,
    fh: u32,
    mask: &[u8],
    mw: u32,
    mh: u32,
    d0: [f32; 2],
    d1: [f32; 2],
    d2: [f32; 2],
    s0: [f32; 2],
    s1: [f32; 2],
    s2: [f32; 2],
    bounds: (&mut u32, &mut u32, &mut u32, &mut u32),
) {
    let minx = d0[0].min(d1[0]).min(d2[0]).floor().max(0.0) as i32;
    let maxx = d0[0].max(d1[0]).max(d2[0]).ceil().min((fw - 1) as f32) as i32;
    let miny = d0[1].min(d1[1]).min(d2[1]).floor().max(0.0) as i32;
    let maxy = d0[1].max(d1[1]).max(d2[1]).ceil().min((fh - 1) as f32) as i32;
    if minx > maxx || miny > maxy {
        return;
    }
    // Barycentric denominator for the destination triangle.
    let denom = (d1[1] - d2[1]) * (d0[0] - d2[0]) + (d2[0] - d1[0]) * (d0[1] - d2[1]);
    if denom.abs() < 1e-6 {
        return;
    }
    let inv = 1.0 / denom;
    let eps = -1e-4;
    // Barycentric weights are linear in x along a row: w = a + b·px. Per row, solve for the
    // x span where all three are ≥ eps (instead of testing every pixel of the bounding box),
    // and step the source position along the span.
    let (b0, b1) = ((d1[1] - d2[1]) * inv, (d2[1] - d0[1]) * inv);
    let b2 = -(b0 + b1);
    let sxb = b0 * s0[0] + b1 * s1[0] + b2 * s2[0];
    let syb = b0 * s0[1] + b1 * s1[1] + b2 * s2[1];
    let (bmin_x, bmin_y, bmax_x, bmax_y) = bounds;
    for y in miny..=maxy {
        let py = y as f32 + 0.5;
        let a0 = ((d1[1] - d2[1]) * -d2[0] + (d2[0] - d1[0]) * (py - d2[1])) * inv;
        let a1 = ((d2[1] - d0[1]) * -d2[0] + (d0[0] - d2[0]) * (py - d2[1])) * inv;
        let a2 = 1.0 - a0 - a1;
        let (mut lo, mut hi) = (minx as f32 + 0.5, maxx as f32 + 0.5);
        for (a, b) in [(a0, b0), (a1, b1), (a2, b2)] {
            if b > 1e-12 {
                lo = lo.max((eps - a) / b);
            } else if b < -1e-12 {
                hi = hi.min((eps - a) / b);
            } else if a < eps {
                hi = f32::MIN;
            }
        }
        if hi < lo {
            continue;
        }
        let x0 = ((lo - 0.5).ceil() as i32).max(minx);
        let x1 = ((hi - 0.5).floor() as i32).min(maxx);
        if x1 < x0 {
            continue;
        }
        let px = x0 as f32 + 0.5;
        let (w0, w1) = (a0 + b0 * px, a1 + b1 * px);
        let w2 = 1.0 - w0 - w1;
        // Corresponding source point in the mask (same barycentric coords).
        let mut sx = w0 * s0[0] + w1 * s1[0] + w2 * s2[0];
        let mut sy = w0 * s0[1] + w1 * s1[1] + w2 * s2[1];
        let row = (y as u32 * fw) as usize;
        for x in x0..=x1 {
            let di = (row + x as usize) * 4;
            // Overwrite (single-write coverage): shared edges land the same value twice.
            layer[di..di + 4].copy_from_slice(&sample_bilinear(mask, mw, mh, sx, sy));
            sx += sxb;
            sy += syb;
        }
        *bmin_x = (*bmin_x).min(x0 as u32);
        *bmax_x = (*bmax_x).max(x1 as u32);
        *bmin_y = (*bmin_y).min(y as u32);
        *bmax_y = (*bmax_y).max(y as u32);
    }
}

/// Bilinear RGBA sample (edge-clamped), 8-bit fixed-point weights.
#[inline]
fn sample_bilinear(img: &[u8], w: u32, h: u32, x: f32, y: f32) -> [u8; 4] {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x as u32, y as u32);
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = ((x - x0 as f32) * 256.0) as u32;
    let fy = ((y - y0 as f32) * 256.0) as u32;
    let (r0, r1) = ((y0 * w) as usize, (y1 * w) as usize);
    let (pa, pb) = ((r0 + x0 as usize) * 4, (r0 + x1 as usize) * 4);
    let (pc, pd) = ((r1 + x0 as usize) * 4, (r1 + x1 as usize) * 4);
    let mut out = [0u8; 4];
    for (k, o) in out.iter_mut().enumerate() {
        let top = img[pa + k] as u32 * (256 - fx) + img[pb + k] as u32 * fx;
        let bot = img[pc + k] as u32 * (256 - fx) + img[pd + k] as u32 * fx;
        *o = ((top * (256 - fy) + bot * fy + (1 << 15)) >> 16) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warps_opaque_square_into_region() {
        // Opaque white mask.
        let (mw, mh) = (64u32, 64u32);
        let mask = vec![255u8; (mw * mh * 4) as usize];
        // Pivots: 4 corners + centre (normalized).
        let pivots = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.5, 0.5]];
        // Map the mask onto a 100×100 region centred in a 200×200 frame.
        let dst = [[50.0, 50.0], [150.0, 50.0], [150.0, 150.0], [50.0, 150.0], [100.0, 100.0]];

        let (fw, fh) = (200u32, 200u32);
        let mut frame = vec![0u8; (fw * fh * 4) as usize];
        warp_mask_onto(&mut frame, fw, fh, &mask, mw, mh, &pivots, &dst, 1.0);

        let at = |x: u32, y: u32| {
            let i = ((y * fw + x) * 4) as usize;
            (frame[i], frame[i + 3])
        };
        // Inside the mapped region: opaque white.
        assert_eq!(at(100, 100), (255, 255));
        assert_eq!(at(70, 70), (255, 255));
        // Outside: untouched.
        assert_eq!(at(10, 10), (0, 0));
        assert_eq!(at(190, 190), (0, 0));
    }

    #[test]
    fn parts_beyond_the_pivots_are_drawn() {
        // Opaque mask whose pivots only cover its lower half (like a face mask whose
        // landmarks stop at the brows while the image has ears above).
        let (mw, mh) = (100u32, 100u32);
        let mask = vec![255u8; (mw * mh * 4) as usize];
        let pivots = [[0.3, 0.6], [0.7, 0.6], [0.7, 0.9], [0.3, 0.9], [0.5, 0.75]];
        // Same layout, 2x bigger, placed in a 400x400 frame.
        let dst: Vec<[f32; 2]> = pivots.iter().map(|p| [100.0 + p[0] * 200.0, 100.0 + p[1] * 200.0]).collect();
        let (fw, fh) = (400u32, 400u32);
        let mut frame = vec![0u8; (fw * fh * 4) as usize];
        warp_mask_onto(&mut frame, fw, fh, &mask, mw, mh, &pivots, &dst, 1.0);
        let at = |x: u32, y: u32| frame[((y * fw + x) * 4) as usize];
        // The mask's top region (above every pivot) maps to y ≈ 100..220: must be drawn.
        assert_eq!(at(200, 130), 255, "top of the mask (\"ears\") was cut off");
        assert_eq!(at(200, 250), 255);
        // Outside the whole mask image: untouched.
        assert_eq!(at(20, 20), 0);
    }

    #[test]
    fn half_opacity_blends() {
        let (mw, mh) = (8u32, 8u32);
        let mask = vec![255u8; (mw * mh * 4) as usize]; // opaque white
        let pivots = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let (fw, fh) = (20u32, 20u32);
        let mut frame = vec![0u8; (fw * fh * 4) as usize]; // black
        let dst = [[2.0, 2.0], [18.0, 2.0], [18.0, 18.0], [2.0, 18.0]];
        warp_mask_onto(&mut frame, fw, fh, &mask, mw, mh, &pivots, &dst, 0.5);
        let i = ((10 * fw + 10) * 4) as usize;
        // 0*0.5 + 255*0.5 ≈ 128.
        assert!((120..=136).contains(&frame[i]), "got {}", frame[i]);
    }
}
