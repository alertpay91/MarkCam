//! What an app (ClassIn…) gets from the running MarkCam: connects the filter's capture pin
//! to a minimal sink, as an app's capture graph does, and reports per second how many
//! frames arrived, how many were new pictures (not repeats), and the worst gap between
//! new pictures. Doesn't publish anything itself.
//!
//!     cargo run --release -p ovc-dshow --example pull_watch -- [seconds]

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    imp::run()
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod imp {
    use std::mem::ManuallyDrop;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use windows::core::{implement, Interface, Result, HRESULT, PWSTR};
    use windows::Win32::Foundation::{BOOL, E_NOTIMPL, S_FALSE, S_OK};
    use windows::Win32::Media::DirectShow::*;
    use windows::Win32::Media::MediaFoundation::AM_MEDIA_TYPE;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    #[derive(Default)]
    struct Stats {
        /// (arrival, hash of a few rows) per delivered sample.
        samples: Mutex<Vec<(Instant, u64)>>,
    }

    #[implement(IPin, IMemInputPin)]
    struct Sink {
        stats: Arc<Stats>,
        peer: Mutex<Option<IPin>>,
    }

    impl IPin_Impl for Sink_Impl {
        fn Connect(&self, _: Option<&IPin>, _: *const AM_MEDIA_TYPE) -> Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn ReceiveConnection(&self, pconnector: Option<&IPin>, _: *const AM_MEDIA_TYPE) -> Result<()> {
            *self.peer.lock().unwrap() = pconnector.cloned();
            Ok(())
        }
        fn Disconnect(&self) -> Result<()> {
            *self.peer.lock().unwrap() = None;
            Ok(())
        }
        fn ConnectedTo(&self) -> Result<IPin> {
            self.peer.lock().unwrap().clone().ok_or_else(|| VFW_E_NOT_CONNECTED.into())
        }
        fn ConnectionMediaType(&self, _: *mut AM_MEDIA_TYPE) -> Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn QueryPinInfo(&self, pinfo: *mut PIN_INFO) -> Result<()> {
            unsafe { pinfo.write(PIN_INFO { pFilter: ManuallyDrop::new(None), dir: PINDIR_INPUT, achName: [0; 128] }) };
            Ok(())
        }
        fn QueryDirection(&self) -> Result<PIN_DIRECTION> {
            Ok(PINDIR_INPUT)
        }
        fn QueryId(&self) -> Result<PWSTR> {
            Err(E_NOTIMPL.into())
        }
        fn QueryAccept(&self, _: *const AM_MEDIA_TYPE) -> HRESULT {
            S_OK
        }
        fn EnumMediaTypes(&self) -> Result<IEnumMediaTypes> {
            Err(E_NOTIMPL.into())
        }
        fn QueryInternalConnections(&self, _: *mut Option<IPin>, _: *mut u32) -> Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn EndOfStream(&self) -> Result<()> {
            Ok(())
        }
        fn BeginFlush(&self) -> Result<()> {
            Ok(())
        }
        fn EndFlush(&self) -> Result<()> {
            Ok(())
        }
        fn NewSegment(&self, _: i64, _: i64, _: f64) -> Result<()> {
            Ok(())
        }
    }

    impl IMemInputPin_Impl for Sink_Impl {
        fn GetAllocator(&self) -> Result<IMemAllocator> {
            Err(VFW_E_NO_ALLOCATOR.into())
        }
        fn NotifyAllocator(&self, _: Option<&IMemAllocator>, _: BOOL) -> Result<()> {
            Ok(())
        }
        fn GetAllocatorRequirements(&self) -> Result<ALLOCATOR_PROPERTIES> {
            Err(E_NOTIMPL.into())
        }
        fn Receive(&self, psample: Option<&IMediaSample>) -> Result<()> {
            let now = Instant::now();
            let s = psample.unwrap();
            let hash = unsafe {
                let p = s.GetPointer()?;
                let n = s.GetActualDataLength() as usize;
                let data = std::slice::from_raw_parts(p, n);
                // FNV over every 97th byte: enough to tell pictures apart, cheap.
                data.iter().step_by(97).fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
            };
            self.stats.samples.lock().unwrap().push((now, hash));
            Ok(())
        }
        fn ReceiveMultiple(&self, _: *const Option<IMediaSample>, _: i32) -> Result<i32> {
            Err(E_NOTIMPL.into())
        }
        fn ReceiveCanBlock(&self) -> Result<()> {
            Err(S_FALSE.into())
        }
    }

    pub fn run() -> Result<()> {
        let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10);
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        let filter = markcam_dshow::create_filter();
        let pins = unsafe { filter.EnumPins()? };
        let mut pin: [Option<IPin>; 1] = [None];
        unsafe { pins.Next(&mut pin, None).ok()? };
        let pin = pin[0].clone().expect("filter has a pin");
        let stats = Arc::new(Stats::default());
        let sink: IPin = Sink { stats: stats.clone(), peer: Mutex::new(None) }.into();
        unsafe { pin.Connect(&sink, None)? };
        let media: IMediaFilter = filter.cast()?;
        unsafe {
            media.Pause()?;
            media.Run(0)?;
        }
        std::thread::sleep(Duration::from_secs(secs));
        unsafe { media.Stop()? };

        let samples = stats.samples.lock().unwrap().clone();
        // Skip the first 2 s (the Studio starts publishing once it sees us).
        let Some(&(t0, _)) = samples.first() else {
            println!("no frames");
            return Ok(());
        };
        let samples: Vec<_> = samples.into_iter().filter(|(t, _)| *t - t0 >= Duration::from_secs(2)).collect();
        let mut new_times = Vec::new();
        let mut prev_hash = None;
        for (t, h) in &samples {
            if prev_hash != Some(*h) {
                new_times.push(*t);
            }
            prev_hash = Some(*h);
        }
        let gaps: Vec<f64> = new_times.windows(2).map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0).collect();
        // Freezes: when (seconds since the start) and how long.
        for (w, g) in new_times.windows(2).zip(&gaps) {
            if *g > 100.0 {
                println!("  freeze at {:.1} s: {:.0} ms without a new picture", (w[0] - t0).as_secs_f64(), g);
            }
        }
        let span = samples.last().map_or(0.0, |(t, _)| (*t - samples[0].0).as_secs_f64()).max(0.001);
        let mut sorted = gaps.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| sorted.get(((sorted.len() as f64 - 1.0) * p) as usize).copied().unwrap_or(0.0);
        println!(
            "{:.1} samples/s, {:.1} new pictures/s; gap between new pictures: median {:.1} ms, 95% {:.1} ms, worst {:.1} ms; gaps over 40 ms: {}",
            samples.len() as f64 / span,
            new_times.len() as f64 / span,
            pct(0.5),
            pct(0.95),
            pct(1.0),
            gaps.iter().filter(|g| **g > 40.0).count()
        );
        Ok(())
    }
}
