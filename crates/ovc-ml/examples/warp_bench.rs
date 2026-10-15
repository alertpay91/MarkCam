//! Timing + checksum of the mask warp (a 512² mask onto a ~360 px face in a 1280×720 frame).
//! Run: `cargo run --release -p ovc-ml --example warp_bench`

fn main() {
    let (mw, mh) = (512u32, 512u32);
    let mask: Vec<u8> = (0..mw * mh)
        .flat_map(|i| {
            let (x, y) = (i % mw, i / mw);
            let a = if (x / 32 + y / 32) % 3 == 0 { 0 } else { 255 };
            [(x / 2) as u8, (y / 2) as u8, ((x ^ y) & 255) as u8, a]
        })
        .collect();
    // 68 pivots in the mask's middle area, like a face mask's landmarks.
    let pivots: Vec<[f32; 2]> =
        (0..68).map(|i| [0.25 + 0.5 * ((i % 9) as f32 / 8.0) + 0.01 * (i as f32).sin(), 0.35 + 0.5 * ((i / 9) as f32 / 7.0)]).collect();
    // Onto the frame: scaled, rotated a little, with some wobble.
    let (fw, fh) = (1280u32, 720u32);
    let dst: Vec<[f32; 2]> = pivots
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = ((p[0] - 0.5) * 360.0, (p[1] - 0.5) * 360.0);
            let (s, c) = 0.15f32.sin_cos();
            [640.0 + x * c - y * s + (i as f32 * 1.7).sin() * 3.0, 360.0 + x * s + y * c]
        })
        .collect();
    let base: Vec<u8> = (0..fw * fh * 4).map(|i| (i % 251) as u8).collect();
    let n = 200;
    let mut frame = base.clone();
    let start = std::time::Instant::now();
    for _ in 0..n {
        frame.copy_from_slice(&base);
        ovc_ml::warp::warp_mask_onto(&mut frame, fw, fh, &mask, mw, mh, &pivots, &dst, 0.9);
    }
    let per = start.elapsed() / n;
    let sum: u64 = frame.iter().enumerate().map(|(i, &v)| v as u64 * (i as u64 % 7 + 1)).sum();
    let changed = frame.iter().zip(&base).filter(|(a, b)| a != b).count();
    println!("warp {per:?}/frame, checksum {sum}, changed bytes {changed}");
    std::fs::write(std::env::temp_dir().join("warp_bench.raw"), &frame).unwrap();
}
