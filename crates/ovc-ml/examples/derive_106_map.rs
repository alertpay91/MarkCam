//! Derive + verify the InsightFace-106 → dlib-68 index map by nearest-matching 2d106det
//! points to the (already verified) 2DFAN4 68 points on a real face.
//! Run: `cargo run -p ovc-ml --example derive_106_map -- <models_dir> <face.jpg>`

use ovc_ml::face::{FaceTracker, Lmk};

fn main() -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("models_dir"));
    let img_path = std::env::args().nth(2).expect("face image");
    let det = dir.join("version-RFB-320.onnx");

    let fan = FaceTracker::load(&det, &dir.join("2dfan4.onnx"), Lmk::Fan256)?;
    let ins = FaceTracker::load(&det, &dir.join("2d106det.onnx"), Lmk::Insight106)?;

    let img = image::open(&img_path)?.to_rgba8();
    let (w, h) = img.dimensions();
    let data = img.into_raw();

    let p68 = fan.landmarks68(&data, w, h)?.expect("no face (fan)");
    let p106 = ins.raw_landmarks(&data, w, h)?.expect("no face (insight)");
    println!("fan68={} insight106={}", p68.len(), p106.len());

    let mut map = [0usize; 68];
    let mut max_res = 0f32;
    let mut sum_res = 0f32;
    for (k, g) in p68.iter().enumerate() {
        let (mut bi, mut bd) = (0usize, f32::MAX);
        for (i, p) in p106.iter().enumerate() {
            let d = (p[0] - g[0]).powi(2) + (p[1] - g[1]).powi(2);
            if d < bd {
                bd = d;
                bi = i;
            }
        }
        map[k] = bi;
        let r = bd.sqrt();
        max_res = max_res.max(r);
        sum_res += r;
    }
    let distinct = {
        let mut m = map.to_vec();
        m.sort_unstable();
        m.dedup();
        m.len()
    };
    println!("residual(normalized): avg={:.4} max={:.4}", sum_res / 68.0, max_res);
    println!("distinct targets: {distinct}/68 (68 = perfect bijection)");
    println!("pub const INSIGHT106_TO_68: [usize; 68] = [");
    for row in map.chunks(17) {
        println!("    {},", row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "));
    }
    println!("];");
    Ok(())
}
