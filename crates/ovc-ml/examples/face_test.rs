//! Smoke/correctness test: load the ONNX models and run the tracker on a synthetic gray
//! frame and (optionally) a real face image.
//! Run: `cargo run -p ovc-ml --example face_test -- <models_dir> [face.jpg]`

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).expect("usage: face_test <models_dir> [image]");
    let dir = std::path::Path::new(&dir);
    let t = ovc_ml::face::FaceTracker::load(
        &dir.join("version-RFB-320.onnx"),
        &dir.join("2d106det.onnx"),
        ovc_ml::face::Lmk::Insight106,
    )?;
    println!("models loaded OK");

    // Gray frame: expect no face.
    let (w, h) = (640u32, 480u32);
    let gray = vec![128u8; (w * h * 4) as usize];
    let start = std::time::Instant::now();
    let res = t.landmarks68(&gray, w, h)?;
    println!("gray frame -> {:?} ({:?})", res.map(|v| v.len()), start.elapsed());

    // Real image, if provided.
    if let Some(path) = std::env::args().nth(2) {
        let img = image::open(&path)?.to_rgba8();
        let (iw, ih) = img.dimensions();
        let data = img.into_raw();
        let start = std::time::Instant::now();
        match t.landmarks68(&data, iw, ih)? {
            None => println!("image {path}: no face detected"),
            Some(pts) => {
                let (mut minx, mut miny, mut maxx, mut maxy) = (1f32, 1f32, 0f32, 0f32);
                for p in &pts {
                    minx = minx.min(p[0]);
                    miny = miny.min(p[1]);
                    maxx = maxx.max(p[0]);
                    maxy = maxy.max(p[1]);
                }
                println!(
                    "image {path} ({iw}x{ih}): {} landmarks in {:?}",
                    pts.len(),
                    start.elapsed()
                );
                println!("  bbox(norm): x[{minx:.3}..{maxx:.3}] y[{miny:.3}..{maxy:.3}]");
                // Spot-check iBUG indices: 30=nose tip, 8=chin, 36=right eye outer, 45=left eye outer.
                let g = |i: usize| pts.get(i).map(|p| format!("({:.3},{:.3})", p[0], p[1])).unwrap_or_default();
                println!("  nose30={} chin8={} eyeR36={} eyeL45={}", g(30), g(8), g(36), g(45));
            }
        }
    }
    Ok(())
}
