//! Load a segmentation model at a fixed input size, run it on an image and time it; writes
//! the (first channel / foreground) output as a grayscale PNG next to the image.
//! Run: `cargo run --release -p ovc-ml --example seg_bench -- model.onnx W H image.png out.png [chw|hwc] [norm]`
//! `norm`: "01" = /255 (default), "pm1" = (x/255-0.5)/0.5.

use tract_onnx::prelude::*;

fn main() -> TractResult<()> {
    let a: Vec<String> = std::env::args().collect();
    let (path, w, h, img_path, out_path) = (&a[1], a[2].parse::<usize>()?, a[3].parse::<usize>()?, &a[4], &a[5]);
    let hwc = a.get(6).is_some_and(|s| s == "hwc");
    let pm1 = a.get(7).is_some_and(|s| s == "pm1");
    let shape: Vec<usize> = if hwc { vec![1, h, w, 3] } else { vec![1, 3, h, w] };
    let t0 = std::time::Instant::now();
    // Intermediate shape annotations carry a symbolic batch that won't unify with the
    // fixed input: drop them and let tract infer.
    let onnx = tract_onnx::onnx();
    let mut proto = onnx.proto_model_for_path(path)?;
    if let Some(g) = proto.graph.as_mut() {
        g.value_info.clear();
    }
    let model = onnx
        .model_for_proto_model(&proto)?
        .with_input_fact(0, f32::fact(&shape).into())?
        // Declared outputs carry a symbolic batch that won't unify with the fixed input.
        .with_output_fact(0, Default::default())?
        .into_optimized()?
        .into_runnable()?;
    println!("load+optimize {:?}", t0.elapsed());

    let img = image::open(img_path).expect("image").to_rgb8();
    let img = image::imageops::resize(&img, w as u32, h as u32, image::imageops::FilterType::Triangle);
    let mut input = vec![0f32; 3 * w * h];
    for (i, p) in img.pixels().enumerate() {
        for c in 0..3 {
            let v = p[c] as f32 / 255.0;
            let v = if pm1 { (v - 0.5) / 0.5 } else { v };
            if hwc {
                input[i * 3 + c] = v;
            } else {
                input[c * w * h + i] = v;
            }
        }
    }
    let t = Tensor::from_shape(&shape, &input)?;
    let mut out = None;
    let n = 10;
    let t0 = std::time::Instant::now();
    for _ in 0..n {
        out = Some(model.run(tvec!(t.clone().into()))?);
    }
    println!("run {:?}/frame", t0.elapsed() / n);
    let out = out.unwrap();
    for (i, o) in out.iter().enumerate() {
        println!("output {i}: shape {:?}", o.shape());
    }
    let o = out[0].to_array_view::<f32>()?;
    let s = o.shape().to_vec();
    // Foreground probability per pixel: single channel as is; NCHW multi-class = 1 - class 0
    // (background); NHWC multi-class likewise on the last axis.
    let (oh, ow) = if s.len() == 4 && s[1] <= 8 && s[3] > 8 { (s[2], s[3]) } else { (s[1], s[2]) };
    let mut px = vec![0u8; oh * ow];
    for y in 0..oh {
        for x in 0..ow {
            let v = if s.len() == 4 && s[1] == 1 {
                o[[0, 0, y, x]]
            } else if s.len() == 4 && s[1] <= 8 && s[3] > 8 {
                1.0 - softmax0(&(0..s[1]).map(|c| o[[0, c, y, x]]).collect::<Vec<_>>())
            } else if s.len() == 4 && s[3] == 1 {
                o[[0, y, x, 0]]
            } else {
                1.0 - softmax0(&(0..s[3]).map(|c| o[[0, y, x, c]]).collect::<Vec<_>>())
            };
            px[y * ow + x] = (v.clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
    image::GrayImage::from_raw(ow as u32, oh as u32, px).unwrap().save(out_path).unwrap();
    Ok(())
}

/// Probability of class 0 (inputs may already be probabilities: then they sum to ~1).
fn softmax0(v: &[f32]) -> f32 {
    let sum: f32 = v.iter().sum();
    if (sum - 1.0).abs() < 0.05 && v.iter().all(|x| *x >= 0.0) {
        return v[0];
    }
    let m = v.iter().cloned().fold(f32::MIN, f32::max);
    let e: Vec<f32> = v.iter().map(|x| (x - m).exp()).collect();
    e[0] / e.iter().sum::<f32>()
}
