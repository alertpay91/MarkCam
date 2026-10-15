//! Timing of the two face-tracking stages (detector vs. landmarks) on one image.
//! Run: `cargo run --release -p ovc-ml --example face_bench -- <models_dir> <face image>`

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: face_bench <models_dir> <image>");
    let path = args.next().expect("usage: face_bench <models_dir> <image>");
    let dir = std::path::Path::new(&dir);
    if let Some(rt) = std::env::var_os("ORT_DIR") {
        ovc_ml::seg::init_runtime(std::path::Path::new(&rt))?;
    }
    let t = ovc_ml::face::FaceTracker::load(
        &dir.join("version-RFB-320.onnx"),
        &dir.join(if std::env::var_os("MESH").is_some() { "facemesh_v2.onnx" } else { "2d106det.onnx" }),
        if std::env::var_os("MESH").is_some() { ovc_ml::face::Lmk::FaceMesh } else { ovc_ml::face::Lmk::Insight106 },
    )?;
    let img = image::open(&path)?.to_rgba8();
    let (w, h) = img.dimensions();
    let data = img.into_raw();
    let n = 30;

    let start = std::time::Instant::now();
    let mut lms = None;
    for _ in 0..n {
        lms = t.landmarks68(&data, w, h)?;
    }
    let full = start.elapsed() / n;
    let Some(lms) = lms else {
        println!("no face");
        return Ok(());
    };

    let (mut tracked, mut track) = t.track68(&data, w, h, None, true)?.expect("face");
    let start = std::time::Instant::now();
    for _ in 0..n {
        (tracked, track) = t.track68(&data, w, h, Some(track), false)?.expect("lost the face");
    }
    let track = start.elapsed() / n;
    let drift = lms.iter().zip(&tracked).map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs())).fold(0f32, f32::max);
    println!("detect+landmarks {full:?}/frame, landmarks only (tracking) {track:?}/frame, max drift after {n} tracked frames {drift:.4}");

    // Moving face: paste the image into a bigger canvas, sliding 6 px/frame, and compare the
    // tracked points with a fresh detection every frame (error in canvas pixels).
    let (cw, ch) = (w * 3, h * 2);
    let mut state = None;
    let mut worst = 0f32;
    for f in 0..60u32 {
        let (ox, oy) = (f * 6 % (cw - w), (f * 3) % (ch - h));
        let mut canvas = vec![90u8; (cw * ch * 4) as usize];
        for y in 0..h {
            let src = &data[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
            let at = (((y + oy) * cw + ox) * 4) as usize;
            canvas[at..at + src.len()].copy_from_slice(src);
        }
        let fresh = t.landmarks68(&canvas, cw, ch)?.expect("face");
        let (pts, tr) = t.track68(&canvas, cw, ch, state, state.is_none())?.expect("lost");
        state = Some(tr);
        let err = fresh.iter().zip(&pts).map(|(a, b)| ((a[0] - b[0]) * cw as f32).abs().max(((a[1] - b[1]) * ch as f32).abs())).fold(0f32, f32::max);
        worst = worst.max(err);
    }
    println!("moving face (6 px/frame): worst difference tracked vs. detected {worst:.1} px (face ~{} px)", h / 2);
    Ok(())
}
