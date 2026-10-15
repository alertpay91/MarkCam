//! Time RVM through ONNX Runtime and save the matte.
//! Run: `cargo run --release -p ovc-ml --example seg_rvm -- <ort dll dir> <rvm.onnx> <image> <out.png> W H RATIO [cpu]`

/// This process's CPU time (user + kernel), ms.
fn cpu_ms() -> f64 {
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Default)]
        struct Ft(u32, u32);
        extern "system" {
            fn GetCurrentProcess() -> isize;
            fn GetProcessTimes(h: isize, c: *mut Ft, e: *mut Ft, k: *mut Ft, u: *mut Ft) -> i32;
        }
        let (mut c, mut e, mut k, mut u) = (Ft::default(), Ft::default(), Ft::default(), Ft::default());
        unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        let v = |f: &Ft| ((f.1 as u64) << 32 | f.0 as u64) as f64 / 10_000.0;
        v(&k) + v(&u)
    }
    #[cfg(not(windows))]
    0.0
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let (dir, model, img, out) = (&a[1], &a[2], &a[3], &a[4]);
    let (w, h, ratio): (usize, usize, f32) = (a[5].parse()?, a[6].parse()?, a[7].parse()?);
    let gpu = a.get(8).map_or(true, |s| s != "cpu");
    ovc_ml::seg::init_runtime(std::path::Path::new(dir))?;
    let im = image::open(img)?.to_rgba8();
    let (fw, fh) = im.dimensions();
    let rgba = im.into_raw();
    let t0 = std::time::Instant::now();
    let mut s = ovc_ml::seg::VideoMatter::load(std::path::Path::new(model), w, h, ratio, gpu, 2)?;
    println!("{:?}: load {:?}", s.backend, t0.elapsed());
    // Recurrent: let it settle over a few frames, like a still video.
    let mut m = Vec::new();
    for _ in 0..5 {
        m = s.matte(&rgba, fw, fh)?;
    }
    let n = 20;
    let c0 = cpu_ms();
    let t0 = std::time::Instant::now();
    for _ in 0..n {
        m = s.matte(&rgba, fw, fh)?;
    }
    println!("{:?}: {:?}/frame at {w}x{h} ratio {ratio}, CPU {:.1} ms/frame", s.backend, t0.elapsed() / n, (cpu_ms() - c0) / n as f64);
    let px: Vec<u8> = m.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8).collect();
    image::GrayImage::from_raw(w as u32, h as u32, px).unwrap().save(out)?;
    Ok(())
}
