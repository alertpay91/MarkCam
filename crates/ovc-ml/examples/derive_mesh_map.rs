//! Derive the Face Mesh (478) → dlib-68 index map: on many frames, match each of the 68
//! points (from the verified InsightFace map) to the mesh point that is closest on average.
//! Run: `cargo run --release -p ovc-ml --example derive_mesh_map -- <models_dir> <frames_dir>`

use ovc_ml::face::{FaceTracker, Lmk};

fn main() -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("models_dir"));
    let frames = std::path::PathBuf::from(std::env::args().nth(2).expect("frames dir"));
    let det = dir.join("version-RFB-320.onnx");
    let ins = FaceTracker::load(&det, &dir.join("2d106det.onnx"), Lmk::Insight106)?;
    let mesh = FaceTracker::load(&det, &dir.join("facemesh_v2.onnx"), Lmk::FaceMesh)?;
    let mut cost = vec![vec![0f64; 478]; 68];
    let mut n = 0;
    let mut files: Vec<_> = std::fs::read_dir(&frames)?.flatten().map(|e| e.path()).collect();
    files.sort();
    for f in files {
        let img = image::open(&f)?.to_rgba8();
        let (w, h) = img.dimensions();
        let data = img.into_raw();
        let (Some(p68), Some(pm)) = (ins.landmarks68(&data, w, h)?, mesh.raw_landmarks(&data, w, h)?) else { continue };
        if pm.len() < 468 {
            continue;
        }
        // Normalize by the eye distance so all frames count the same.
        let eye = |r: std::ops::Range<usize>| {
            let k = r.len() as f32;
            let s = p68[r].iter().fold([0.0, 0.0], |a, p| [a[0] + p[0], a[1] + p[1]]);
            [s[0] / k, s[1] / k]
        };
        let (l, r) = (eye(36..42), eye(42..48));
        let ipd = (((l[0] - r[0]) * w as f32).powi(2) + ((l[1] - r[1]) * h as f32).powi(2)).sqrt().max(1.0);
        for (k, g) in p68.iter().enumerate() {
            for (i, p) in pm.iter().enumerate() {
                let d = (((p[0] - g[0]) * w as f32).powi(2) + ((p[1] - g[1]) * h as f32).powi(2)).sqrt() / ipd;
                cost[k][i] += d as f64;
            }
        }
        n += 1;
    }
    println!("frames used: {n}");
    let mut map = [0usize; 68];
    let mut worst = 0f64;
    for k in 0..68 {
        let (bi, bc) = cost[k].iter().enumerate().fold((0, f64::MAX), |b, (i, &c)| if c < b.1 { (i, c) } else { b });
        map[k] = bi;
        worst = worst.max(bc / n as f64);
    }
    println!("worst mean residual (× eye distance): {worst:.3}");
    let groups = [("jaw", 0..17), ("brows", 17..27), ("nose", 27..36), ("eyes", 36..48), ("mouth", 48..68)];
    for (name, r) in groups {
        let m: f64 = r.clone().map(|k| cost[k][ovc_ml::face::FACEMESH_TO_68[k]] / n as f64).sum::<f64>() / r.len() as f64;
        println!("standard map, {name}: mean residual {m:.3} × eye distance");
    }
    println!("pub const FACEMESH_TO_68: [usize; 68] = [");
    for row in map.chunks(17) {
        println!("    {},", row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "));
    }
    println!("];");
    Ok(())
}