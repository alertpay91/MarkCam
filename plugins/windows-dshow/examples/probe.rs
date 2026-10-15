//! End-to-end check of the filter without registering it or opening a real app:
//! publishes solid-red frames into the OVC1 ring (as OpenVCam Studio does), connects the
//! filter's capture pin to a minimal sink (as ClassIn's capture graph does), runs it for
//! two seconds and verifies the delivered NV12 frames.
//!
//!     cargo run -p ovc-dshow --example probe

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    imp::run()
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod imp {
    use std::mem::ManuallyDrop;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use ovc_ipc::{mapping_size, Producer, Region, RingConfig, VCAM_MAX_FRAME_BYTES, VCAM_RING_NAME, VCAM_SLOTS};
    use windows::core::{implement, Interface, Result, HRESULT, PWSTR};
    use windows::Win32::Foundation::{BOOL, E_NOTIMPL, S_FALSE, S_OK};
    use windows::Win32::Media::DirectShow::*;
    use windows::Win32::Media::KernelStreaming::IKsPropertySet;
    use windows::Win32::Media::MediaFoundation::{AMPROPSETID_Pin, AM_MEDIA_TYPE, PIN_CATEGORY_CAPTURE};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    #[derive(Default)]
    struct Stats {
        frames: AtomicU32,
        first_byte: AtomicU32,
        len: AtomicU32,
        times_ok: AtomicBool,
        /// Luma of pixel 0 of every delivered frame (identifies which ring frame it was).
        lumas: Mutex<Vec<u8>>,
    }

    /// Gray level of ring frame `i` (cycles; steps of 4 stay distinct after RGB→Y).
    fn gray(i: usize) -> u8 {
        (40 + (i * 4) % 160) as u8
    }
    /// BT.601 limited-range luma of a gray pixel (what the filter's NV12 conversion gives).
    fn luma(v: u8) -> u8 {
        (((66 + 129 + 25) * v as i32 + 128) >> 8) as u8 + 16
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
        fn ReceiveConnection(&self, pconnector: Option<&IPin>, pmt: *const AM_MEDIA_TYPE) -> Result<()> {
            let mt = unsafe { &*pmt };
            println!("  sink: offered subtype {:?}, sample size {}", mt.subtype, mt.lSampleSize);
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
            Err(VFW_E_NO_ALLOCATOR.into()) // force the filter to bring its own
        }
        fn NotifyAllocator(&self, _: Option<&IMemAllocator>, _: BOOL) -> Result<()> {
            Ok(())
        }
        fn GetAllocatorRequirements(&self) -> Result<ALLOCATOR_PROPERTIES> {
            Err(E_NOTIMPL.into())
        }
        fn Receive(&self, psample: Option<&IMediaSample>) -> Result<()> {
            let s = psample.unwrap();
            unsafe {
                let p = s.GetPointer()?;
                self.stats.first_byte.store(*p as u32, Ordering::SeqCst);
                self.stats.lumas.lock().unwrap().push(*p);
                self.stats.len.store(s.GetActualDataLength() as u32, Ordering::SeqCst);
                let (mut t0, mut t1) = (0i64, 0i64);
                if s.GetTime(&mut t0, &mut t1).is_ok() && t1 > t0 {
                    self.stats.times_ok.store(true, Ordering::SeqCst);
                }
            }
            self.stats.frames.fetch_add(1, Ordering::SeqCst);
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
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };

        // 1. Producer: publish red 1280x720 frames at ~30 fps, like OpenVCam Studio.
        let stop = Arc::new(AtomicBool::new(false));
        let producer = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let region =
                    Region::shared(VCAM_RING_NAME, mapping_size(VCAM_MAX_FRAME_BYTES, VCAM_SLOTS), true).unwrap();
                let cfg = RingConfig {
                    width: 1280,
                    height: 720,
                    fourcc: u32::from_le_bytes(*b"RGBA"),
                    stride: 1280 * 4,
                    slot_size: VCAM_MAX_FRAME_BYTES,
                    slot_count: VCAM_SLOTS,
                    fps_num: 30,
                    fps_den: 1,
                };
                let mut p = Producer::create(region, cfg).unwrap();
                // Like a webcam: ~30 fps on its own clock, each frame a distinct gray level.
                let mut i = 0;
                while !stop.load(Ordering::SeqCst) {
                    let v = gray(i);
                    let frame = [v, v, v, 255].repeat(1280 * 720);
                    p.publish(&frame, 0).unwrap();
                    i += 1;
                    std::thread::sleep(Duration::from_micros(33_000));
                }
                p.consumer()
            })
        };
        std::thread::sleep(Duration::from_millis(100));

        // 2. The filter, as an app would see it.
        let filter = markcam_dshow::create_filter();
        let pins = unsafe { filter.EnumPins()? };
        let mut pin: [Option<IPin>; 1] = [None];
        unsafe { pins.Next(&mut pin, None).ok()? };
        let pin = pin[0].clone().expect("filter has a pin");

        let cfg: IAMStreamConfig = pin.cast()?;
        let (mut n, mut sz) = (0, 0);
        unsafe { cfg.GetNumberOfCapabilities(&mut n, &mut sz)? };
        println!("formats advertised: {n} (caps struct {sz} bytes)");

        let ks: IKsPropertySet = pin.cast()?;
        let mut cat = windows::core::GUID::zeroed();
        let mut ret = 0u32;
        unsafe {
            ks.Get(
                &AMPROPSETID_Pin,
                AMPROPERTY_PIN_CATEGORY.0 as u32,
                std::ptr::null(),
                0,
                &mut cat as *mut _ as *mut _,
                16,
                &mut ret,
            )?
        };
        println!("pin category is capture: {}", cat == PIN_CATEGORY_CAPTURE);

        // 3. Connect to our sink and stream for 2 s.
        let stats = Arc::new(Stats::default());
        let sink: IPin = Sink { stats: stats.clone(), peer: Mutex::new(None) }.into();
        unsafe { pin.Connect(&sink, None)? };
        let media: IMediaFilter = filter.cast()?;
        unsafe {
            media.Pause()?;
            media.Run(0)?;
        }
        std::thread::sleep(Duration::from_secs(2));
        unsafe { media.Stop()? };

        stop.store(true, Ordering::SeqCst);
        let (cw, ch, beat) = producer.join().unwrap();
        println!("consumer report seen by the Studio side: {cw}x{ch}, beat {beat}");

        let frames = stats.frames.load(Ordering::SeqCst);
        let y = stats.first_byte.load(Ordering::SeqCst);
        let len = stats.len.load(Ordering::SeqCst);
        println!("frames delivered in 2 s: {frames}");
        println!("sample length: {len} (expected {} for 1280x720 NV12)", 1280 * 720 * 3 / 2);
        println!("last luma byte: {y} (>= 50 = frame from the ring, 37 = placeholder)");
        println!("timestamps set: {}", stats.times_ok.load(Ordering::SeqCst));

        // Judder: map each delivered luma back to its ring frame index (mod the cycle) and
        // count repeats (same frame twice) and skips (a frame never delivered).
        let lut: Vec<u8> = (0..40).map(|i| luma(gray(i))).collect();
        let idx: Vec<usize> = stats.lumas.lock().unwrap().iter().filter_map(|l| lut.iter().position(|x| x == l)).collect();
        let (mut dup, mut skip) = (0, 0);
        for w in idx.windows(2).skip(5) {
            match (w[1] + 40 - w[0]) % 40 {
                0 => dup += 1,
                1 => {}
                _ => skip += 1,
            }
        }
        println!("repeated frames: {dup}, skipped frames: {skip} (of {} delivered)", idx.len());
        let ok = (50..=70).contains(&frames) && y >= 50 && len == 1280 * 720 * 3 / 2 && (cw, ch) == (1280, 720) && beat > 40 && dup + skip <= 1;
        println!("{}", if ok { "PROBE OK" } else { "PROBE FAILED" });
        Ok(())
    }
}
