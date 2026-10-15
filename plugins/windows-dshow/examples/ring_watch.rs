//! Prints, per second, how many frames OpenVCam Studio publishes into the virtual-camera
//! ring and how many the DirectShow filter delivers to the app (ClassIn…), plus the
//! timing jitter of the Studio's frames. Read-only: safe to run during a call.
//!
//!     cargo run -p ovc-dshow --example ring_watch -- [seconds]

#[cfg(windows)]
fn main() {
    use ovc_ipc::{Consumer, Region, VCAM_RING_NAME};
    use std::time::{Duration, Instant};

    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let mut consumer = None;
    let mut buf = Vec::new();
    let end = Instant::now() + Duration::from_secs(secs);
    let mut tick = Instant::now();
    let (mut frames, mut last_beat) = (0, None::<u32>);
    let mut last_frame: Option<Instant> = None;
    let (mut gap_min, mut gap_max) = (f64::MAX, 0f64);
    while Instant::now() < end {
        if consumer.is_none() {
            consumer = Region::shared(VCAM_RING_NAME, 0, false).and_then(Consumer::open).ok();
        }
        if let Some(c) = consumer.as_mut() {
            if c.try_recv(&mut buf).is_some() {
                frames += 1;
                let now = Instant::now();
                if let Some(p) = last_frame {
                    let g = (now - p).as_secs_f64() * 1000.0;
                    gap_min = gap_min.min(g);
                    gap_max = gap_max.max(g);
                }
                last_frame = Some(now);
            }
        }
        if tick.elapsed() >= Duration::from_secs(1) {
            let (cw, ch, beat) = consumer.as_ref().map(|c| c.consumer_report()).unwrap_or_default();
            let delivered = last_beat.map(|b| beat.wrapping_sub(b)).unwrap_or(0);
            last_beat = Some(beat);
            println!(
                "studio {frames:3} fps (gaps {gap_min:4.0}-{gap_max:4.0} ms) | filter -> app {delivered:3} fps at {cw}x{ch}"
            );
            frames = 0;
            gap_min = f64::MAX;
            gap_max = 0.0;
            tick = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(not(windows))]
fn main() {}
