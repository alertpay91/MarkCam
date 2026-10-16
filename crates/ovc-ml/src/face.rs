//! Face tracking for mask effects: UltraFace (`version-RFB-320`) finds the face box, then a
//! landmark model regresses points that are reduced to the 68 dlib/iBUG landmarks ManyCam
//! mask pivots use. Two landmark backends are supported:
//!   * [`Lmk::Fan256`]  — 2DFAN4 (256 input, outputs 68 points directly in dlib order; heavy).
//!   * [`Lmk::Insight106`] — InsightFace 2d106det (192 input, 106 points; small + fast). Its
//!     106 points are reduced to the 68 dlib points via [`INSIGHT106_TO_68`].
//! Everything runs through pure-Rust `tract`. Output is 68 points in normalized frame coords.

use anyhow::Context;
use tract_onnx::prelude::*;

type Plan = TypedRunnableModel<TypedModel>;

const DET_W: usize = 320;
const DET_H: usize = 240;

/// Which landmark model is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lmk {
    /// 2DFAN4: 256×256 input, /255, output `[1,68,3]` on a 64-grid (coords ×256/64).
    Fan256,
    /// InsightFace 2d106det: 192×192 input, raw 0..255, output 106 pairs in [-1,1] → ×96.
    /// (Its weights are for non-commercial research only: not shipped in public builds.)
    Insight106,
    /// Google MediaPipe Face Mesh V2 (Apache-2.0): 256×256 NHWC RGB 0..1, output 478 × (x,y,z)
    /// in crop pixels; reduced to 68 via [`FACEMESH_TO_68`].
    FaceMesh,
}

impl Lmk {
    fn input(self) -> usize {
        match self {
            Lmk::Fan256 => 256,
            Lmk::Insight106 => 192,
            Lmk::FaceMesh => 256,
        }
    }
    fn norm_div(self) -> f32 {
        match self {
            Lmk::Fan256 => 255.0,
            Lmk::Insight106 => 1.0,
            Lmk::FaceMesh => 255.0,
        }
    }
}

/// InsightFace 2d106det index → dlib/iBUG 68 index. Auto-derived (nearest-neighbour against
/// verified 2DFAN4 output on a real face) and checked for a clean bijection; see
/// `examples/derive_106_map.rs`.
pub const INSIGHT106_TO_68: [usize; 68] = [
    9, 11, 12, 14, 16, 3, 4, 7, 0, 23, 22, 21, 19, 32, 30, 28, 25,
    43, 48, 49, 51, 50, 103, 104, 105, 105, 101, 72, 73, 74, 86, 78, 79, 80,
    85, 85, 35, 40, 42, 39, 37, 33, 89, 94, 96, 93, 91, 90, 52, 66, 63,
    71, 67, 68, 69, 58, 59, 53, 56, 54, 65, 63, 60, 57, 69, 70, 62, 63,
];

/// Face Mesh V2 index → dlib/iBUG 68 index: the standard correspondence (face oval for the
/// jaw, brows, nose, eye corners and lids, lips), checked against the InsightFace map on a
/// recorded face with `examples/derive_mesh_map.rs`.
pub const FACEMESH_TO_68: [usize; 68] = [
    127, 234, 93, 132, 58, 172, 136, 149, 152, 378, 365, 397, 288, 361, 323, 454, 356, // jaw
    70, 63, 105, 66, 107, 336, 296, 334, 293, 300, // brows
    168, 197, 5, 4, 98, 97, 2, 326, 327, // nose
    33, 160, 158, 133, 153, 144, 362, 385, 387, 263, 373, 380, // eyes
    61, 40, 37, 0, 267, 270, 291, 321, 314, 17, 84, 91, // outer lips
    78, 81, 13, 311, 308, 402, 14, 178, // inner lips
];

/// Loads and runs the detector + a landmark model.
pub struct FaceTracker {
    det: Plan,
    /// The landmark model in tract — only built when ONNX Runtime isn't used for it
    /// (optimizing Face Mesh in tract takes seconds of CPU).
    lmk: Option<Plan>,
    /// The same landmark model in ONNX Runtime (CPU, one thread) when it is loaded — about
    /// 5× faster than tract for Face Mesh; tract stays as the fallback.
    lmk_ort: Option<std::sync::Mutex<ort::session::Session>>,
    kind: Lmk,
    min_score: f32,
}

impl FaceTracker {
    /// Load the UltraFace detector and a landmark model of the given [`Lmk`] kind.
    pub fn load(det_path: &std::path::Path, lmk_path: &std::path::Path, kind: Lmk) -> anyhow::Result<Self> {
        let det = load_plan(det_path, &[1, 3, DET_H, DET_W]).context("load face detector")?;
        let s = kind.input();
        let shape = if kind == Lmk::FaceMesh { [1, s, s, 3] } else { [1, 3, s, s] };
        // ONNX Runtime, if `seg::init_runtime` was called (otherwise this just fails).
        let build = || -> ort::Result<ort::session::Session> {
            ort::session::Session::builder()?
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
                .with_intra_op_spinning(false)?
                .with_inter_op_spinning(false)?
                .with_intra_threads(1)?
                .commit_from_file(lmk_path)
        };
        let lmk_ort = (kind == Lmk::FaceMesh).then(build).and_then(|r| r.ok()).map(std::sync::Mutex::new);
        let lmk = if lmk_ort.is_none() { Some(load_plan(lmk_path, &shape).context("load landmark model")?) } else { None };
        Ok(Self { det, lmk, lmk_ort, kind, min_score: 0.6 })
    }

    /// The 68 dlib-order landmarks in normalized frame coordinates, or `None` if no face.
    pub fn landmarks68(&self, rgb: &[u8], w: u32, h: u32) -> anyhow::Result<Option<Vec<[f32; 2]>>> {
        Ok(self.raw_landmarks(rgb, w, h)?.map(|raw| self.to68(raw)))
    }

    /// All raw landmarks the model emits (68 for FAN, 106 for Insight), normalized frame coords.
    pub fn raw_landmarks(&self, rgb: &[u8], w: u32, h: u32) -> anyhow::Result<Option<Vec<[f32; 2]>>> {
        Ok(self.find(rgb, w, h)?.map(|(raw, _)| raw))
    }

    /// Detector + landmarks: raw points and the [`Track`] to follow them with.
    fn find(&self, rgb: &[u8], w: u32, h: u32) -> anyhow::Result<Option<(Vec<[f32; 2]>, Track)>> {
        let Some(bbox) = self.detect(rgb, w, h)? else {
            return Ok(None);
        };
        let crop = Crop::from_box(bbox, w, h);
        let raw = self.landmarks_in(rgb, w, h, crop)?;
        let (c, ext) = extent(&raw, w, h);
        let track = Track { crop, scale: crop.side / ext, dx: (crop.cx - c[0]) / ext, dy: (crop.cy - c[1]) / ext };
        Ok(Some((raw, track)))
    }

    /// 68 landmarks for a video stream. With the previous frame's [`Track`] only the landmark
    /// model runs (the crop follows the face from the last points); the detector (~2/3 of the
    /// work) runs only to find a face, or every `check`-th frame to confirm it is still there.
    pub fn track68(
        &self,
        rgb: &[u8],
        w: u32,
        h: u32,
        prev: Option<Track>,
        check: bool,
    ) -> anyhow::Result<Option<(Vec<[f32; 2]>, Track)>> {
        let found = match prev {
            Some(t) if !check => {
                let raw = self.landmarks_in(rgb, w, h, t.crop)?;
                let (c, ext) = extent(&raw, w, h);
                // Points spread over a much smaller/larger area than the crop expects mean
                // the model lost the face: fall back to the detector.
                let ratio = t.scale * ext / t.crop.side;
                if (0.7..1.4).contains(&ratio) && ext > 8.0 {
                    let side = ext * t.scale;
                    let crop = Crop { cx: c[0] + t.dx * ext, cy: c[1] + t.dy * ext, side };
                    Some((raw, Track { crop, ..t }))
                } else {
                    self.find(rgb, w, h)?
                }
            }
            _ => self.find(rgb, w, h)?,
        };
        Ok(found.map(|(raw, t)| (self.to68(raw), t)))
    }

    fn to68(&self, raw: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
        match self.kind {
            Lmk::Fan256 => raw,
            Lmk::Insight106 => INSIGHT106_TO_68.iter().map(|&i| raw.get(i).copied().unwrap_or([0.0, 0.0])).collect(),
            Lmk::FaceMesh => FACEMESH_TO_68.iter().map(|&i| raw.get(i).copied().unwrap_or([0.0, 0.0])).collect(),
        }
    }

    /// UltraFace: highest-confidence face box as normalized corners `[x1,y1,x2,y2]`, or `None`.
    fn detect(&self, rgb: &[u8], w: u32, h: u32) -> anyhow::Result<Option<[f32; 4]>> {
        let input = resize_chw(rgb, w, h, DET_W, DET_H, 127.0, 128.0);
        let t = Tensor::from_shape(&[1, 3, DET_H, DET_W], &input)?;
        let out = self.det.run(tvec!(t.into()))?;
        let scores = out[0].to_array_view::<f32>()?; // [1, N, 2]
        let boxes = out[1].to_array_view::<f32>()?; // [1, N, 4]
        let n = scores.shape()[1];
        let mut best = (self.min_score, None);
        for i in 0..n {
            let s = scores[[0, i, 1]];
            if s > best.0 {
                best = (s, Some([boxes[[0, i, 0]], boxes[[0, i, 1]], boxes[[0, i, 2]], boxes[[0, i, 3]]]));
            }
        }
        Ok(best.1)
    }

    /// Crop a square around the face, run the landmark model, map points to normalized frame coords.
    fn landmarks_in(&self, rgb: &[u8], w: u32, h: u32, crop: Crop) -> anyhow::Result<Vec<[f32; 2]>> {
        let s = self.kind.input();
        let (fw, fh) = (w as f32, h as f32);
        let side = crop.side;
        let left = crop.cx - side * 0.5;
        let top = crop.cy - side * 0.5;

        let div = self.kind.norm_div();
        let mut input = vec![0f32; 3 * s * s];
        for j in 0..s {
            for i in 0..s {
                let sx = left + (i as f32 + 0.5) / s as f32 * side;
                let sy = top + (j as f32 + 0.5) / s as f32 * side;
                let [r, g, b] = sample_rgb_bilinear(rgb, w, h, sx, sy);
                let idx = j * s + i;
                if self.kind == Lmk::FaceMesh {
                    input[idx * 3] = r / div;
                    input[idx * 3 + 1] = g / div;
                    input[idx * 3 + 2] = b / div;
                } else {
                    input[idx] = r / div;
                    input[s * s + idx] = g / div;
                    input[2 * s * s + idx] = b / div;
                }
            }
        }
        // Decode into crop-pixel coords, then map to normalized frame coords.
        let to_frame = |cxp: f32, cyp: f32| [(left + cxp / s as f32 * side) / fw, (top + cyp / s as f32 * side) / fh];
        if let Some(sess) = &self.lmk_ort {
            let mut sess = sess.lock().unwrap();
            let run = |sess: &mut ort::session::Session| -> ort::Result<Vec<f32>> {
                let name = sess.inputs().first().map(|i| i.name().to_string()).unwrap_or_default();
                let out = sess.run(ort::inputs![name => ort::value::Tensor::<f32>::from_array(([1usize, s, s, 3], input.clone()))?])?;
                let (_, d) = out[0].try_extract_tensor::<f32>()?;
                Ok(d.to_vec())
            };
            let flat = run(&mut sess).map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(flat.chunks_exact(3).map(|p| to_frame(p[0], p[1])).collect());
        }
        let t = if self.kind == Lmk::FaceMesh { Tensor::from_shape(&[1, s, s, 3], &input)? } else { Tensor::from_shape(&[1, 3, s, s], &input)? };
        let out = self.lmk.as_ref().context("no landmark model")?.run(tvec!(t.into()))?;
        let pts = match self.kind {
            Lmk::Fan256 => {
                let lm = out[0].to_array_view::<f32>()?; // [1,68,3]
                let count = lm.shape()[1];
                (0..count)
                    .map(|k| to_frame(lm[[0, k, 0]] * s as f32 / 64.0, lm[[0, k, 1]] * s as f32 / 64.0))
                    .collect()
            }
            Lmk::Insight106 => {
                let flat: Vec<f32> = out[0].to_array_view::<f32>()?.iter().copied().collect();
                let half = s as f32 / 2.0;
                flat.chunks_exact(2).map(|p| to_frame((p[0] + 1.0) * half, (p[1] + 1.0) * half)).collect()
            }
            Lmk::FaceMesh => {
                let flat: Vec<f32> = out[0].to_array_view::<f32>()?.iter().copied().collect();
                flat.chunks_exact(3).map(|p| to_frame(p[0], p[1])).collect()
            }
        };
        Ok(pts)
    }
}

/// Square landmark crop in frame pixels.
#[derive(Clone, Copy, Debug)]
struct Crop {
    cx: f32,
    cy: f32,
    side: f32,
}

impl Crop {
    /// The detector box (normalized corners) widened to the square the landmark model expects.
    fn from_box(bbox: [f32; 4], w: u32, h: u32) -> Self {
        let (fw, fh) = (w as f32, h as f32);
        let (x1, y1, x2, y2) = (bbox[0] * fw, bbox[1] * fh, bbox[2] * fw, bbox[3] * fh);
        Crop { cx: (x1 + x2) * 0.5, cy: (y1 + y2) * 0.5, side: (x2 - x1).max(y2 - y1) * 1.6 }
    }
}

/// Where to crop the next frame, relative to this frame's landmarks: the crop the detector
/// chose is remembered as a size/offset in units of the landmark spread, so frames tracked
/// without the detector see the face the same way.
#[derive(Clone, Copy, Debug)]
pub struct Track {
    crop: Crop,
    scale: f32,
    dx: f32,
    dy: f32,
}

impl Track {
    /// The square the landmark model looks at next: center and side, in frame pixels.
    pub fn crop(&self) -> (f32, f32, f32) {
        (self.crop.cx, self.crop.cy, self.crop.side)
    }
}

/// Center and spread (larger of width/height) of normalized points, in frame pixels.
fn extent(pts: &[[f32; 2]], w: u32, h: u32) -> ([f32; 2], f32) {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    let (fw, fh) = (w as f32, h as f32);
    ([(x0 + x1) * 0.5 * fw, (y0 + y1) * 0.5 * fh], ((x1 - x0) * fw).max((y1 - y0) * fh).max(1.0))
}

fn load_plan(path: &std::path::Path, input_shape: &[usize]) -> anyhow::Result<Plan> {
    let model = tract_onnx::onnx()
        .model_for_path(path)
        .with_context(|| format!("read {}", path.display()))?
        .with_input_fact(0, f32::fact(input_shape).into())?
        .into_optimized()?
        .into_runnable()?;
    Ok(model)
}

/// Nearest-neighbour resize into a planar CHW RGB f32 buffer with `(px - mean) / scale`.
fn resize_chw(rgb: &[u8], w: u32, h: u32, dw: usize, dh: usize, mean: f32, scale: f32) -> Vec<f32> {
    let mut out = vec![0f32; 3 * dw * dh];
    let (plane, w, h) = (dw * dh, w as usize, h as usize);
    for y in 0..dh {
        let sy = (y * h / dh).min(h.saturating_sub(1));
        for x in 0..dw {
            let sx = (x * w / dw).min(w.saturating_sub(1));
            let si = (sy * w + sx) * 4;
            let di = y * dw + x;
            out[di] = (rgb[si] as f32 - mean) / scale;
            out[plane + di] = (rgb[si + 1] as f32 - mean) / scale;
            out[2 * plane + di] = (rgb[si + 2] as f32 - mean) / scale;
        }
    }
    out
}

/// Bilinear RGB sample (clamped; out-of-frame reads the edge). Returns `[r,g,b]` 0..255.
fn sample_rgb_bilinear(rgb: &[u8], w: u32, h: u32, x: f32, y: f32) -> [f32; 3] {
    let xf = x.clamp(0.0, (w - 1) as f32);
    let yf = y.clamp(0.0, (h - 1) as f32);
    let x0 = xf.floor() as u32;
    let y0 = yf.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let (fx, fy) = (xf - x0 as f32, yf - y0 as f32);
    let px = |xx: u32, yy: u32, c: usize| rgb[((yy * w + xx) * 4) as usize + c] as f32;
    let mut out = [0f32; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let top = px(x0, y0, c) * (1.0 - fx) + px(x1, y0, c) * fx;
        let bot = px(x0, y1, c) * (1.0 - fx) + px(x1, y1, c) * fx;
        *o = top * (1.0 - fy) + bot * fy;
    }
    out
}
