//! Person segmentation for the virtual background: MODNet (webcam portrait matting,
//! Apache-2.0) through ONNX Runtime, on the GPU via DirectML when there is one, else on the
//! CPU. MODNet predicts a soft *alpha matte* (not a blocky class mask), so hair edges, the
//! gap between a headset band and the head, ears and fingers come out right.
//!
//! The runtime (`onnxruntime.dll` + `DirectML.dll`) is loaded at run time from the folder
//! given to [`init_runtime`].

use std::path::Path;
use std::sync::OnceLock;

use anyhow::Context;
use ort::session::Session;
use ort::value::Tensor;

static RUNTIME: OnceLock<Result<(), String>> = OnceLock::new();

/// Load ONNX Runtime from `dir\onnxruntime.dll` (once per process).
pub fn init_runtime(dir: &Path) -> anyhow::Result<()> {
    let r = RUNTIME.get_or_init(|| {
        let dll = dir.join(if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" });
        if !dll.is_file() {
            return Err(format!("{} not found", dll.display()));
        }
        ort::init_from(&dll).map_err(|e| e.to_string())?.with_name("markcam").commit();
        Ok(())
    });
    r.clone().map_err(|e| anyhow::anyhow!("ONNX Runtime: {e}"))
}

/// Where the model runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Gpu,
    Cpu,
}

/// Robust Video Matting (RVM, MobileNetV3): a *recurrent* video matting model — it carries
/// what it saw in earlier frames, so the matte follows moving heads, arms and hands without
/// lagging or flickering, and its built-in refiner gives full-resolution edges.
pub struct VideoMatter {
    session: Session,
    /// Recurrent state r1..r4 (shape, data) fed back every frame.
    rec: Vec<(Vec<usize>, Vec<f32>)>,
    /// Input size and the model's internal downsample ratio.
    pub w: usize,
    pub h: usize,
    pub ratio: f32,
    pub backend: Backend,
    src: Vec<f32>,
}

impl VideoMatter {
    pub fn load(model: &Path, w: usize, h: usize, ratio: f32, gpu: bool, threads: usize) -> anyhow::Result<Self> {
        let session = build_session(model, gpu, threads)?;
        let backend = session.1;
        Ok(Self { session: session.0, rec: Self::zero_state(), w, h, ratio, backend, src: vec![0.0; 3 * w * h] })
    }

    fn zero_state() -> Vec<(Vec<usize>, Vec<f32>)> {
        (0..4).map(|_| (vec![1, 1, 1, 1], vec![0.0])).collect()
    }

    /// Forget the past frames (after a cut / source change).
    pub fn reset(&mut self) {
        self.rec = Self::zero_state();
    }

    /// Alpha matte (0..1, `self.w`x`self.h`) for the next frame of the video.
    pub fn matte(&mut self, rgba: &[u8], fw: u32, fh: u32) -> anyhow::Result<Vec<f32>> {
        let (w, h) = (self.w, self.h);
        downscale_chw(rgba, fw as usize, fh as usize, w, h, &mut self.src, |v| v);
        self.run().map(|(a, _)| a)
    }

    /// Like [`matte`](Self::matte) for a frame already at `self.w`x`self.h`, as planar
    /// RGB 0..1 (saves the downscale when the caller has it anyway).
    pub fn matte_planar(&mut self, rgb: &[Vec<f32>; 3]) -> anyhow::Result<Vec<f32>> {
        self.matte_and_color(rgb).map(|(a, _)| a)
    }

    /// The matte plus RVM's *foreground colors* (planar RGB 0..1): the person's colors with
    /// the background's tint taken out — what edge pixels should show instead of a blend
    /// of person and room (that blend is the dark/light fringe around a cut-out).
    pub fn matte_and_color(&mut self, rgb: &[Vec<f32>; 3]) -> anyhow::Result<(Vec<f32>, Vec<f32>)> {
        let plane = self.w * self.h;
        if rgb.iter().any(|c| c.len() != plane) {
            anyhow::bail!("planes don't match the model size");
        }
        self.src.clear();
        for c in rgb {
            self.src.extend_from_slice(c);
        }
        self.run()
    }

    fn run(&mut self) -> anyhow::Result<(Vec<f32>, Vec<f32>)> {
        let (w, h) = (self.w, self.h);
        let mut run = || -> ort::Result<(Vec<f32>, Vec<f32>, Vec<(Vec<usize>, Vec<f32>)>)> {
            let t = |(s, d): &(Vec<usize>, Vec<f32>)| Tensor::<f32>::from_array((s.clone(), d.clone()));
            let outputs = self.session.run(ort::inputs![
                "src" => Tensor::<f32>::from_array(([1usize, 3, h, w], self.src.clone()))?,
                "r1i" => t(&self.rec[0])?,
                "r2i" => t(&self.rec[1])?,
                "r3i" => t(&self.rec[2])?,
                "r4i" => t(&self.rec[3])?,
                "downsample_ratio" => Tensor::<f32>::from_array(([1usize], vec![self.ratio]))?,
            ])?;
            let (_, pha) = outputs["pha"].try_extract_tensor::<f32>()?;
            let (_, fgr) = outputs["fgr"].try_extract_tensor::<f32>()?;
            let mut rec = Vec::with_capacity(4);
            for name in ["r1o", "r2o", "r3o", "r4o"] {
                let (shape, data) = outputs[name].try_extract_tensor::<f32>()?;
                rec.push((shape.iter().map(|d| *d as usize).collect(), data.to_vec()));
            }
            Ok((pha.to_vec(), fgr.to_vec(), rec))
        };
        let (pha, fgr, rec) = run().map_err(|e| anyhow::anyhow!("{e}"))?;
        self.rec = rec;
        if pha.len() < w * h || fgr.len() < 3 * w * h {
            anyhow::bail!("unexpected matte size {}", pha.len());
        }
        Ok((pha[..w * h].to_vec(), fgr[..3 * w * h].to_vec()))
    }
}

/// A session on the GPU (DirectML) if asked and possible, else on the CPU.
fn build_session(model: &Path, gpu: bool, threads: usize) -> anyhow::Result<(Session, Backend)> {
    let build = |use_gpu: bool| -> ort::Result<Session> {
        let mut b = Session::builder()?.with_intra_op_spinning(false)?.with_inter_op_spinning(false)?;
        if use_gpu {
            b = b
                .with_intra_threads(1)?
                .with_memory_pattern(false)?
                .with_parallel_execution(false)?
                .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])?;
        } else {
            b = b.with_intra_threads(threads.max(1))?;
        }
        b.commit_from_file(model)
    };
    match gpu.then(|| build(true)) {
        Some(Ok(s)) => Ok((s, Backend::Gpu)),
        _ => Ok((build(false).map_err(|e| anyhow::anyhow!("{e}")).context("load segmentation model")?, Backend::Cpu)),
    }
}

/// Box-sampled downscale of an RGBA frame into planar CHW floats (0..1, then `map`).
fn downscale_chw(rgba: &[u8], fw: usize, fh: usize, w: usize, h: usize, out: &mut Vec<f32>, map: impl Fn(f32) -> f32) {
    out.resize(3 * w * h, 0.0);
    let plane = w * h;
    for y in 0..h {
        let (sy0, sy1) = (y * fh / h, ((y + 1) * fh / h).max(y * fh / h + 1).min(fh));
        for x in 0..w {
            let (sx0, sx1) = (x * fw / w, ((x + 1) * fw / w).max(x * fw / w + 1).min(fw));
            let mut acc = [0u32; 3];
            for (yy, xx) in [(sy0, sx0), (sy0, sx1 - 1), (sy1 - 1, sx0), (sy1 - 1, sx1 - 1)] {
                let i = (yy.min(fh - 1) * fw + xx.min(fw - 1)) * 4;
                for c in 0..3 {
                    acc[c] += rgba[i + c] as u32;
                }
            }
            let i = y * w + x;
            for c in 0..3 {
                out[c * plane + i] = map(acc[c] as f32 / (4.0 * 255.0));
            }
        }
    }
}

pub struct PersonSegmenter {
    session: Session,
    input_name: String,
    /// Model input size (multiples of 32).
    pub w: usize,
    pub h: usize,
    pub backend: Backend,
    buf: Vec<f32>,
}

impl PersonSegmenter {
    /// Load MODNet for `w`x`h` input (multiples of 32), on the GPU if `gpu` and DirectML
    /// works, else on the CPU (`threads` worker threads).
    pub fn load(model: &Path, w: usize, h: usize, gpu: bool, threads: usize) -> anyhow::Result<Self> {
        let build = |use_gpu: bool| -> ort::Result<Session> {
            // No busy-waiting worker threads between runs (they burn a core for nothing;
            // this runs ~15×/s on a battery).
            let mut b = Session::builder()?.with_intra_op_spinning(false)?.with_inter_op_spinning(false)?;
            if use_gpu {
                // DirectML wants these off (its own memory planning, one stream).
                b = b
                    .with_intra_threads(1)?
                    .with_memory_pattern(false)?
                    .with_parallel_execution(false)?
                    .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])?;
            } else {
                b = b.with_intra_threads(threads.max(1))?;
            }
            b.commit_from_file(model)
        };
        let build = |use_gpu: bool| build(use_gpu).map_err(|e| anyhow::anyhow!("{e}"));
        let (session, backend) = match gpu.then(|| build(true)) {
            Some(Ok(s)) => (s, Backend::Gpu),
            _ => (build(false).context("load segmentation model")?, Backend::Cpu),
        };
        let input_name = session.inputs().first().map(|i| i.name().to_string()).unwrap_or_else(|| "input".into());
        Ok(Self { session, input_name, w, h, backend, buf: vec![0.0; 3 * w * h] })
    }

    /// Alpha matte (0..1, `self.w`x`self.h`, row-major) of the person in an RGBA frame.
    pub fn matte(&mut self, rgba: &[u8], fw: u32, fh: u32) -> anyhow::Result<Vec<f32>> {
        let (w, h) = (self.w, self.h);
        let (fw, fh) = (fw as usize, fh as usize);
        if rgba.len() < fw * fh * 4 || fw == 0 || fh == 0 {
            anyhow::bail!("bad frame");
        }
        // Box-average downscale into planar CHW, normalized to -1..1 as MODNet was trained.
        let plane = w * h;
        for y in 0..h {
            let (sy0, sy1) = (y * fh / h, ((y + 1) * fh / h).max(y * fh / h + 1).min(fh));
            for x in 0..w {
                let (sx0, sx1) = (x * fw / w, ((x + 1) * fw / w).max(x * fw / w + 1).min(fw));
                let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
                // Sample a 2x2 grid inside the box (cheap, enough for a 4-8x downscale).
                let xs = [sx0, (sx0 + sx1) / 2];
                let ys = [sy0, (sy0 + sy1) / 2];
                for &yy in &ys {
                    let row = yy.min(fh - 1) * fw;
                    for &xx in &xs {
                        let i = (row + xx.min(fw - 1)) * 4;
                        r += rgba[i] as u32;
                        g += rgba[i + 1] as u32;
                        b += rgba[i + 2] as u32;
                    }
                }
                let n = |v: u32| v as f32 / (4.0 * 127.5) - 1.0;
                let i = y * w + x;
                self.buf[i] = n(r);
                self.buf[plane + i] = n(g);
                self.buf[2 * plane + i] = n(b);
            }
        }
        let input = Tensor::<f32>::from_array(([1usize, 3, h, w], self.buf.clone()))?;
        let outputs = self.session.run(ort::inputs![self.input_name.as_str() => input])?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>()?;
        if data.len() < plane {
            anyhow::bail!("unexpected matte size {}", data.len());
        }
        Ok(data[..plane].to_vec())
    }
}
