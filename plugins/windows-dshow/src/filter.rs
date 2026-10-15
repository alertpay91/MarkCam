//! The DirectShow source filter ("OpenVCam") and its single capture output pin.
//!
//! Written against the raw interfaces (no DirectShow base classes). The filter is a live
//! push source: once paused/running, a worker thread pulls the newest frame from the ring
//! at 30 fps, converts it to the negotiated format and delivers it downstream through the
//! peer's `IMemInputPin`, like the base classes' `CSourceStream` does.
//!
//! Lock discipline: `Shared::st` is never held across a call into another filter (the
//! peer may call back into us), and `Stop` joins the worker only after releasing it.

use std::ffi::c_void;
use std::mem::{size_of, ManuallyDrop};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Result, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{E_NOTIMPL, E_POINTER, E_UNEXPECTED, S_FALSE, S_OK};
use windows::Win32::Media::DirectShow::*;
use windows::Win32::Media::IReferenceClock;
use windows::Win32::Media::KernelStreaming::{IKsPropertySet, IKsPropertySet_Impl};
use windows::Win32::Media::MediaFoundation::{AMPROPSETID_Pin, AM_MEDIA_TYPE, CLSID_MemoryAllocator, PIN_CATEGORY_CAPTURE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, CoUninitialize, IPersist_Impl, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED,
};

use crate::convert;
use crate::formats::{self, Format, FRAME_100NS};
use crate::source::RingSource;
use crate::CLSID_OPENVCAM;

const PIN_NAME: &str = "Capture";
const E_PROP_SET_UNSUPPORTED: HRESULT = HRESULT(0x80070492_u32 as i32);
const E_PROP_ID_UNSUPPORTED: HRESULT = HRESULT(0x80070490_u32 as i32);
const KSPROPERTY_SUPPORT_GET: u32 = 1;

/// Mutable filter state shared by the filter, the pin and the streaming thread.
struct State {
    state: FILTER_STATE,
    clock: Option<IReferenceClock>,
    /// Stream time base given to `Run`.
    start: i64,
    /// Non-owning (DirectShow rule: a filter must not AddRef its graph).
    graph: *mut c_void,
    graph_name: [u16; 128],
    peer: Option<IPin>,
    mem: Option<IMemInputPin>,
    alloc: Option<IMemAllocator>,
    /// The connected format.
    fmt: Option<Format>,
    /// Set via `IAMStreamConfig::SetFormat`; tried first on the next connection.
    preferred: Option<Format>,
    worker: Option<Worker>,
}

// SAFETY: DirectShow filters and allocators are free-threaded ("Both") COM objects, and all
// access to these fields is serialized by the Mutex.
unsafe impl Send for State {}

pub struct Shared {
    st: Mutex<State>,
    /// Non-owning pointer to our own `IBaseFilter`, for `IPin::QueryPinInfo`. Cleared when
    /// the filter is destroyed.
    filter: AtomicPtr<c_void>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned lock only means another thread panicked (impossible with panic=abort);
        // keep going with the data rather than aborting the host app.
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }
}

struct Worker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

/// COM pointers handed to the streaming thread.
struct Pipe {
    mem: IMemInputPin,
    alloc: IMemAllocator,
}
// SAFETY: see `State`.
unsafe impl Send for Pipe {}

fn wide_name(s: &str) -> [u16; 128] {
    let mut out = [0u16; 128];
    for (o, c) in out.iter_mut().take(127).zip(s.encode_utf16()) {
        *o = c;
    }
    out
}

fn co_wstr(s: &str) -> Result<PWSTR> {
    let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: allocation sized for `w`, filled before being returned to the caller (who frees it).
    unsafe {
        let p = CoTaskMemAlloc(w.len() * 2) as *mut u16;
        if p.is_null() {
            return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        Ok(PWSTR(p))
    }
}

/// Create a new, unconnected OpenVCam filter.
pub fn create_filter() -> IBaseFilter {
    let sh = Arc::new(Shared {
        st: Mutex::new(State {
            state: State_Stopped,
            clock: None,
            start: 0,
            graph: std::ptr::null_mut(),
            graph_name: [0; 128],
            peer: None,
            mem: None,
            alloc: None,
            fmt: None,
            preferred: None,
            worker: None,
        }),
        filter: AtomicPtr::new(std::ptr::null_mut()),
    });
    let pin: IPin = OutPin { sh: sh.clone() }.into();
    let filter: IBaseFilter = Filter { sh: sh.clone(), pin }.into();
    sh.filter.store(filter.as_raw(), Ordering::SeqCst);
    filter
}

// ---------------------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------------------

/// Start the worker if connected and not already streaming. Called with the lock held;
/// only touches the allocator (which never calls back into us).
fn start_streaming(sh: &Arc<Shared>, st: &mut State) -> Result<()> {
    if st.worker.is_some() {
        return Ok(());
    }
    let (Some(mem), Some(alloc), Some(fmt)) = (st.mem.clone(), st.alloc.clone(), st.fmt) else {
        return Ok(()); // not connected: nothing to stream
    };
    unsafe { alloc.Commit()? };
    let stop = Arc::new(AtomicBool::new(false));
    let pipe = Pipe { mem, alloc };
    let (sh2, stop2) = (sh.clone(), stop.clone());
    let handle = std::thread::Builder::new()
        .name("openvcam-dshow".into())
        .spawn(move || stream(sh2, pipe, fmt, stop2))
        .map_err(|_| windows::core::Error::from(E_UNEXPECTED))?;
    st.worker = Some(Worker { stop, handle });
    Ok(())
}

/// Stop the worker (if any). Takes the lock briefly; joins without holding it.
fn stop_streaming(sh: &Shared) {
    let (worker, alloc) = {
        let mut st = sh.lock();
        (st.worker.take(), st.alloc.clone())
    };
    if let Some(w) = worker {
        w.stop.store(true, Ordering::SeqCst);
        // Decommit wakes a worker blocked in GetBuffer (it then fails and exits).
        if let Some(a) = &alloc {
            let _ = unsafe { a.Decommit() };
        }
        let _ = w.handle.join();
    }
}

fn stream(sh: Arc<Shared>, pipe: Pipe, fmt: Format, stop: Arc<AtomicBool>) {
    let com_ok = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut src = RingSource::new();
    let mut scaled = Vec::new();
    let mut placeholder: Vec<u8> = Vec::new();
    let need = fmt.image_size();
    // Deliver each Studio frame as soon as it lands instead of on our own fixed 30 fps
    // clock: two "30 fps" clocks never match exactly, and sampling one with the other
    // repeats/skips a frame every so often (visible judder) and adds up to a frame of
    // latency. Bounds: never faster than ~the advertised rate, and a slow heartbeat
    // (placeholder / last frame) when the Studio isn't publishing.
    let min_gap = Duration::from_nanos(1_000_000_000 / formats::FPS as u64) * 8 / 10;
    let max_wait = Duration::from_millis(70);
    let mut last_sent = Instant::now() - max_wait;
    let mut n: i64 = 0;

    while !stop.load(Ordering::SeqCst) {
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let since = last_sent.elapsed();
            // Only look for a frame once we may send one, so none gets consumed unsent.
            if since >= min_gap && src.refresh() {
                break;
            }
            if since >= max_wait {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        last_sent = Instant::now();

        let mut sample: Option<IMediaSample> = None;
        if unsafe { pipe.alloc.GetBuffer(&mut sample, None, None, 0) }.is_err() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let Some(sample) = sample else { continue };

        // SAFETY: the allocator guarantees GetSize() bytes at GetPointer().
        let filled = unsafe {
            match sample.GetPointer() {
                Ok(p) if !p.is_null() && sample.GetSize() as usize >= need => {
                    let dst = std::slice::from_raw_parts_mut(p, need);
                    src.report(fmt.w, fmt.h);
                    match src.frame() {
                        Some((data, sw, sh_)) => {
                            if (sw, sh_) == (fmt.w, fmt.h) {
                                convert::convert(data, fmt.w, fmt.h, fmt.sub, dst);
                            } else {
                                convert::fit_scale(data, sw, sh_, fmt.w, fmt.h, &mut scaled);
                                convert::convert(&scaled, fmt.w, fmt.h, fmt.sub, dst);
                            }
                        }
                        None => {
                            if placeholder.len() != need {
                                placeholder = make_placeholder(fmt);
                            }
                            dst.copy_from_slice(&placeholder);
                        }
                    }
                    true
                }
                _ => false,
            }
        };

        if filled {
            // Timestamps: stream time from the graph clock while running (what renderers
            // expect from a live source), else a plain frame counter.
            let (clock, start, running) = {
                let st = sh.lock();
                (st.clock.clone(), st.start, st.state == State_Running)
            };
            let t0 = match (running, clock) {
                (true, Some(c)) => unsafe { c.GetTime() }.map(|now| now - start).unwrap_or(n * FRAME_100NS),
                _ => n * FRAME_100NS,
            };
            let t1 = t0 + FRAME_100NS;
            unsafe {
                let _ = sample.SetActualDataLength(need as i32);
                let _ = sample.SetTime(Some(&t0), Some(&t1));
                let _ = sample.SetSyncPoint(true);
                let _ = sample.SetDiscontinuity(n == 0);
                let _ = pipe.mem.Receive(&sample);
            }
            n += 1;
        }
        drop(sample);
    }
    drop(pipe);
    if com_ok {
        unsafe { CoUninitialize() };
    }
}

/// Shown while OpenVCam Studio isn't running / publishing: a dark frame.
fn make_placeholder(fmt: Format) -> Vec<u8> {
    let rgba = [24u8, 24, 28, 255].repeat(fmt.w as usize * fmt.h as usize);
    let mut out = vec![0u8; fmt.image_size()];
    convert::convert(&rgba, fmt.w, fmt.h, fmt.sub, &mut out);
    out
}

// ---------------------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------------------

#[implement(IBaseFilter, IAMFilterMiscFlags)]
pub struct Filter {
    sh: Arc<Shared>,
    pin: IPin,
}

impl Drop for Filter {
    fn drop(&mut self) {
        self.sh.filter.store(std::ptr::null_mut(), Ordering::SeqCst);
        stop_streaming(&self.sh);
        let mut st = self.sh.lock();
        if let Some(a) = st.alloc.take() {
            let _ = unsafe { a.Decommit() };
        }
        st.mem = None;
        st.peer = None;
        st.clock = None;
    }
}

impl IPersist_Impl for Filter_Impl {
    fn GetClassID(&self) -> Result<GUID> {
        Ok(CLSID_OPENVCAM)
    }
}

impl IMediaFilter_Impl for Filter_Impl {
    fn Stop(&self) -> Result<()> {
        stop_streaming(&self.sh);
        let mut st = self.sh.lock();
        if let Some(a) = &st.alloc {
            let _ = unsafe { a.Decommit() };
        }
        st.state = State_Stopped;
        Ok(())
    }

    fn Pause(&self) -> Result<()> {
        let mut st = self.sh.lock();
        if st.state == State_Stopped {
            start_streaming(&self.sh, &mut st)?;
        }
        st.state = State_Paused;
        Ok(())
    }

    fn Run(&self, tstart: i64) -> Result<()> {
        let mut st = self.sh.lock();
        if st.state == State_Stopped {
            start_streaming(&self.sh, &mut st)?;
        }
        st.start = tstart;
        st.state = State_Running;
        Ok(())
    }

    fn GetState(&self, _timeout_ms: u32) -> Result<FILTER_STATE> {
        Ok(self.sh.lock().state)
    }

    fn SetSyncSource(&self, pclock: Option<&IReferenceClock>) -> Result<()> {
        self.sh.lock().clock = pclock.cloned();
        Ok(())
    }

    fn GetSyncSource(&self) -> Result<IReferenceClock> {
        self.sh.lock().clock.clone().ok_or_else(|| VFW_E_NO_CLOCK.into())
    }
}

impl IBaseFilter_Impl for Filter_Impl {
    fn EnumPins(&self) -> Result<IEnumPins> {
        Ok(PinEnum { pin: self.pin.clone(), pos: AtomicUsize::new(0) }.into())
    }

    fn FindPin(&self, id: &PCWSTR) -> Result<IPin> {
        if id.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: caller passes a NUL-terminated wide string.
        let s = unsafe { id.to_string() }.unwrap_or_default();
        if s == PIN_NAME {
            Ok(self.pin.clone())
        } else {
            Err(VFW_E_NOT_FOUND.into())
        }
    }

    fn QueryFilterInfo(&self, pinfo: *mut FILTER_INFO) -> Result<()> {
        if pinfo.is_null() {
            return Err(E_POINTER.into());
        }
        let st = self.sh.lock();
        // Returned graph pointer is AddRef'd, per the interface contract.
        let graph = unsafe { IFilterGraph::from_raw_borrowed(&st.graph) }.cloned();
        unsafe { pinfo.write(FILTER_INFO { achName: st.graph_name, pGraph: ManuallyDrop::new(graph) }) };
        Ok(())
    }

    fn JoinFilterGraph(&self, pgraph: Option<&IFilterGraph>, pname: &PCWSTR) -> Result<()> {
        let mut st = self.sh.lock();
        st.graph = pgraph.map_or(std::ptr::null_mut(), |g| g.as_raw());
        st.graph_name = if pname.is_null() {
            [0; 128]
        } else {
            wide_name(&unsafe { pname.to_string() }.unwrap_or_default())
        };
        Ok(())
    }

    fn QueryVendorInfo(&self) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }
}

impl IAMFilterMiscFlags_Impl for Filter_Impl {
    fn GetMiscFlags(&self) -> u32 {
        AM_FILTER_MISC_FLAGS_IS_SOURCE.0 as u32
    }
}

// ---------------------------------------------------------------------------------------
// Output pin
// ---------------------------------------------------------------------------------------

#[implement(IPin, IAMStreamConfig, IKsPropertySet)]
pub struct OutPin {
    sh: Arc<Shared>,
}

impl OutPin {
    /// Agree on an allocator with the downstream pin (theirs if acceptable, else a
    /// standard memory allocator) sized for `fmt`.
    fn setup_allocator(&self, peer: &IPin, fmt: Format) -> Result<(IMemInputPin, IMemAllocator)> {
        let mem: IMemInputPin = peer.cast()?;
        let size = fmt.image_size() as i32;
        let mut req = unsafe { mem.GetAllocatorRequirements() }.unwrap_or_default();
        req.cBuffers = req.cBuffers.max(3);
        req.cbBuffer = req.cbBuffer.max(size);
        req.cbAlign = req.cbAlign.max(1);
        let try_alloc = |alloc: IMemAllocator| -> Result<IMemAllocator> {
            let actual = unsafe { alloc.SetProperties(&req)? };
            if actual.cbBuffer < size {
                return Err(E_UNEXPECTED.into());
            }
            unsafe { mem.NotifyAllocator(&alloc, false)? };
            Ok(alloc)
        };
        if let Ok(a) = unsafe { mem.GetAllocator() } {
            if let Ok(a) = try_alloc(a) {
                return Ok((mem, a));
            }
        }
        let a: IMemAllocator = unsafe { CoCreateInstance(&CLSID_MemoryAllocator, None, CLSCTX_INPROC_SERVER)? };
        let a = try_alloc(a)?;
        Ok((mem, a))
    }
}

impl IPin_Impl for OutPin_Impl {
    fn Connect(&self, preceivepin: Option<&IPin>, pmt: *const AM_MEDIA_TYPE) -> Result<()> {
        let peer = preceivepin.ok_or(windows::core::Error::from(E_POINTER))?;
        let preferred = {
            let st = self.sh.lock();
            if st.peer.is_some() {
                return Err(VFW_E_ALREADY_CONNECTED.into());
            }
            if st.state != State_Stopped {
                return Err(VFW_E_NOT_STOPPED.into());
            }
            st.preferred
        };
        // Candidate formats: the caller's (possibly partial) type narrows our list.
        let mut candidates = formats::ordered(preferred);
        if !pmt.is_null() {
            let mt = unsafe { &*pmt };
            candidates = match Format::from_media_type(mt) {
                Some(f) => vec![f],
                None => candidates.into_iter().filter(|f| f.matches_partial(mt)).collect(),
            };
        }
        let me: IPin = unsafe { self.cast()? };
        for f in candidates {
            let mut mt = f.to_media_type();
            let accepted = unsafe { peer.ReceiveConnection(&me, &mt) }.is_ok();
            formats::free_media_type(&mut mt);
            if !accepted {
                continue;
            }
            match self.setup_allocator(peer, f) {
                Ok((mem, alloc)) => {
                    let mut st = self.sh.lock();
                    st.peer = Some(peer.clone());
                    st.mem = Some(mem);
                    st.alloc = Some(alloc);
                    st.fmt = Some(f);
                    return Ok(());
                }
                Err(_) => {
                    let _ = unsafe { peer.Disconnect() };
                }
            }
        }
        Err(VFW_E_NO_ACCEPTABLE_TYPES.into())
    }

    fn ReceiveConnection(&self, _pconnector: Option<&IPin>, _pmt: *const AM_MEDIA_TYPE) -> Result<()> {
        Err(VFW_E_INVALID_DIRECTION.into()) // we are an output pin
    }

    fn Disconnect(&self) -> Result<()> {
        let mut st = self.sh.lock();
        if st.state != State_Stopped {
            return Err(VFW_E_NOT_STOPPED.into());
        }
        if st.peer.is_none() {
            return Err(S_FALSE.into());
        }
        if let Some(a) = st.alloc.take() {
            let _ = unsafe { a.Decommit() };
        }
        st.mem = None;
        st.peer = None;
        st.fmt = None;
        Ok(())
    }

    fn ConnectedTo(&self) -> Result<IPin> {
        self.sh.lock().peer.clone().ok_or_else(|| VFW_E_NOT_CONNECTED.into())
    }

    fn ConnectionMediaType(&self, pmt: *mut AM_MEDIA_TYPE) -> Result<()> {
        if pmt.is_null() {
            return Err(E_POINTER.into());
        }
        let fmt = self.sh.lock().fmt;
        match fmt {
            Some(f) => {
                unsafe { pmt.write(f.to_media_type()) };
                Ok(())
            }
            None => {
                // Zeroed, so a caller's FreeMediaType on it is harmless.
                unsafe { std::ptr::write_bytes(pmt as *mut u8, 0, size_of::<AM_MEDIA_TYPE>()) };
                Err(VFW_E_NOT_CONNECTED.into())
            }
        }
    }

    fn QueryPinInfo(&self, pinfo: *mut PIN_INFO) -> Result<()> {
        if pinfo.is_null() {
            return Err(E_POINTER.into());
        }
        let raw = self.sh.filter.load(Ordering::SeqCst);
        // AddRef'd, per the interface contract.
        let filter = unsafe { IBaseFilter::from_raw_borrowed(&raw) }.cloned();
        unsafe {
            pinfo.write(PIN_INFO { pFilter: ManuallyDrop::new(filter), dir: PINDIR_OUTPUT, achName: wide_name(PIN_NAME) })
        };
        Ok(())
    }

    fn QueryDirection(&self) -> Result<PIN_DIRECTION> {
        Ok(PINDIR_OUTPUT)
    }

    fn QueryId(&self) -> Result<PWSTR> {
        co_wstr(PIN_NAME)
    }

    fn QueryAccept(&self, pmt: *const AM_MEDIA_TYPE) -> HRESULT {
        if pmt.is_null() {
            return E_POINTER;
        }
        match Format::from_media_type(unsafe { &*pmt }) {
            Some(_) => S_OK,
            None => S_FALSE,
        }
    }

    fn EnumMediaTypes(&self) -> Result<IEnumMediaTypes> {
        let st = self.sh.lock();
        let list = formats::ordered(st.fmt.or(st.preferred));
        Ok(MediaTypeEnum { list, pos: AtomicUsize::new(0) }.into())
    }

    fn QueryInternalConnections(&self, _appin: *mut Option<IPin>, _npin: *mut u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EndOfStream(&self) -> Result<()> {
        Err(E_UNEXPECTED.into())
    }

    fn BeginFlush(&self) -> Result<()> {
        Err(E_UNEXPECTED.into())
    }

    fn EndFlush(&self) -> Result<()> {
        Err(E_UNEXPECTED.into())
    }

    fn NewSegment(&self, _tstart: i64, _tstop: i64, _drate: f64) -> Result<()> {
        Ok(())
    }
}

impl IAMStreamConfig_Impl for OutPin_Impl {
    fn SetFormat(&self, pmt: *const AM_MEDIA_TYPE) -> Result<()> {
        if pmt.is_null() {
            self.sh.lock().preferred = None;
            return Ok(());
        }
        let f = Format::from_media_type(unsafe { &*pmt }).ok_or(windows::core::Error::from(VFW_E_TYPE_NOT_ACCEPTED))?;
        let (graph, needs_reconnect) = {
            let mut st = self.sh.lock();
            st.preferred = Some(f);
            match st.fmt {
                Some(cur) if cur != f => {
                    if st.state != State_Stopped {
                        return Err(VFW_E_NOT_STOPPED.into());
                    }
                    (unsafe { IFilterGraph::from_raw_borrowed(&st.graph) }.cloned(), true)
                }
                _ => (None, false),
            }
        };
        // Already connected with another format: ask the graph to reconnect us; our
        // Connect then tries the preferred format first.
        if needs_reconnect {
            if let Some(g) = graph {
                let me: IPin = unsafe { self.cast()? };
                unsafe { g.Reconnect(&me)? };
            }
        }
        Ok(())
    }

    fn GetFormat(&self) -> Result<*mut AM_MEDIA_TYPE> {
        let st = self.sh.lock();
        let f = st.fmt.or(st.preferred).unwrap_or(formats::ordered(None)[0]);
        let p = formats::alloc_media_type(&f);
        if p.is_null() {
            return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
        }
        Ok(p)
    }

    fn GetNumberOfCapabilities(&self, picount: *mut i32, pisize: *mut i32) -> Result<()> {
        if picount.is_null() || pisize.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            *picount = formats::all().len() as i32;
            *pisize = size_of::<VIDEO_STREAM_CONFIG_CAPS>() as i32;
        }
        Ok(())
    }

    fn GetStreamCaps(&self, iindex: i32, ppmt: *mut *mut AM_MEDIA_TYPE, pscc: *mut u8) -> Result<()> {
        if ppmt.is_null() || pscc.is_null() {
            return Err(E_POINTER.into());
        }
        let all = formats::all();
        let Some(f) = usize::try_from(iindex).ok().and_then(|i| all.get(i)) else {
            return Err(S_FALSE.into());
        };
        let p = formats::alloc_media_type(f);
        if p.is_null() {
            return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
        }
        unsafe {
            *ppmt = p;
            (pscc as *mut VIDEO_STREAM_CONFIG_CAPS).write_unaligned(f.caps());
        }
        Ok(())
    }
}

impl IKsPropertySet_Impl for OutPin_Impl {
    fn Set(&self, _: *const GUID, _: u32, _: *const c_void, _: u32, _: *const c_void, _: u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Get(
        &self,
        guidpropset: *const GUID,
        dwpropid: u32,
        _pinstancedata: *const c_void,
        _cbinstancedata: u32,
        ppropdata: *mut c_void,
        cbpropdata: u32,
        pcbreturned: *mut u32,
    ) -> Result<()> {
        if guidpropset.is_null() || unsafe { *guidpropset } != AMPROPSETID_Pin {
            return Err(E_PROP_SET_UNSUPPORTED.into());
        }
        if dwpropid != AMPROPERTY_PIN_CATEGORY.0 as u32 {
            return Err(E_PROP_ID_UNSUPPORTED.into());
        }
        if !pcbreturned.is_null() {
            unsafe { *pcbreturned = size_of::<GUID>() as u32 };
        }
        if ppropdata.is_null() {
            return Ok(()); // size query
        }
        if (cbpropdata as usize) < size_of::<GUID>() {
            return Err(E_UNEXPECTED.into());
        }
        unsafe { (ppropdata as *mut GUID).write_unaligned(PIN_CATEGORY_CAPTURE) };
        Ok(())
    }

    fn QuerySupported(&self, guidpropset: *const GUID, dwpropid: u32) -> Result<u32> {
        if guidpropset.is_null() || unsafe { *guidpropset } != AMPROPSETID_Pin {
            return Err(E_PROP_SET_UNSUPPORTED.into());
        }
        if dwpropid != AMPROPERTY_PIN_CATEGORY.0 as u32 {
            return Err(E_PROP_ID_UNSUPPORTED.into());
        }
        Ok(KSPROPERTY_SUPPORT_GET)
    }
}

// ---------------------------------------------------------------------------------------
// Enumerators
// ---------------------------------------------------------------------------------------

#[implement(IEnumPins)]
struct PinEnum {
    pin: IPin,
    pos: AtomicUsize,
}

impl IEnumPins_Impl for PinEnum_Impl {
    fn Next(&self, cpins: u32, pppins: *mut Option<IPin>, pcfetched: *mut u32) -> HRESULT {
        if pppins.is_null() || (cpins > 1 && pcfetched.is_null()) {
            return E_POINTER;
        }
        let mut fetched = 0u32;
        if cpins > 0 && self.pos.load(Ordering::SeqCst) == 0 {
            unsafe { pppins.write(Some(self.pin.clone())) };
            self.pos.store(1, Ordering::SeqCst);
            fetched = 1;
        }
        if !pcfetched.is_null() {
            unsafe { *pcfetched = fetched };
        }
        if fetched == cpins {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, cpins: u32) -> Result<()> {
        let pos = self.pos.load(Ordering::SeqCst) + cpins as usize;
        self.pos.store(pos.min(1), Ordering::SeqCst);
        if pos <= 1 {
            Ok(())
        } else {
            Err(S_FALSE.into())
        }
    }

    fn Reset(&self) -> Result<()> {
        self.pos.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumPins> {
        Ok(PinEnum { pin: self.pin.clone(), pos: AtomicUsize::new(self.pos.load(Ordering::SeqCst)) }.into())
    }
}

#[implement(IEnumMediaTypes)]
struct MediaTypeEnum {
    list: Vec<Format>,
    pos: AtomicUsize,
}

impl IEnumMediaTypes_Impl for MediaTypeEnum_Impl {
    fn Next(&self, cmediatypes: u32, ppmediatypes: *mut *mut AM_MEDIA_TYPE, pcfetched: *mut u32) -> HRESULT {
        if ppmediatypes.is_null() || (cmediatypes > 1 && pcfetched.is_null()) {
            return E_POINTER;
        }
        let mut pos = self.pos.load(Ordering::SeqCst);
        let mut fetched = 0u32;
        while fetched < cmediatypes {
            let Some(f) = self.list.get(pos) else { break };
            let p = formats::alloc_media_type(f);
            if p.is_null() {
                break;
            }
            unsafe { ppmediatypes.add(fetched as usize).write(p) };
            fetched += 1;
            pos += 1;
        }
        self.pos.store(pos, Ordering::SeqCst);
        if !pcfetched.is_null() {
            unsafe { *pcfetched = fetched };
        }
        if fetched == cmediatypes {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, cmediatypes: u32) -> Result<()> {
        let pos = self.pos.load(Ordering::SeqCst) + cmediatypes as usize;
        self.pos.store(pos.min(self.list.len()), Ordering::SeqCst);
        if pos <= self.list.len() {
            Ok(())
        } else {
            Err(S_FALSE.into())
        }
    }

    fn Reset(&self) -> Result<()> {
        self.pos.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumMediaTypes> {
        Ok(MediaTypeEnum { list: self.list.clone(), pos: AtomicUsize::new(self.pos.load(Ordering::SeqCst)) }.into())
    }
}
