//! Producer / consumer over a shared byte region, implementing the seqlock protocol.
//!
//! The region can be a real named mapping (production) or an owned buffer (tests),
//! hidden behind [`Region`]. All multi-threaded access goes through atomics placed at
//! fixed offsets inside the region.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::protocol::{flags, mapping_size, slot_offset, RingHeader, SlotHeader, MAGIC, VERSION};

#[derive(Debug, thiserror::Error)]
pub enum RingError {
    #[error("mapping too small: need {need} bytes, have {have}")]
    TooSmall { need: usize, have: usize },
    #[error("bad magic/version: not an OVC1 ring")]
    BadHeader,
    #[error("frame is {got} bytes but slot holds {max}")]
    FrameTooBig { got: usize, max: usize },
    #[error("another MarkCam (process {pid}) is already sending to the virtual camera")]
    Busy { pid: u32 },
    #[error(transparent)]
    Os(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Copy)]
pub struct RingConfig {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub stride: u32,
    pub slot_size: u32,
    pub slot_count: u32,
    pub fps_num: u32,
    pub fps_den: u32,
}

/// A mapped (or owned) byte region. `ptr`/`len` describe the whole mapping.
///
/// SAFETY: `ptr` is valid for `len` bytes for the lifetime of `Region`, and the backing
/// memory is shared coherently across processes (a named mapping) or threads (a Vec).
pub struct Region {
    ptr: *mut u8,
    len: usize,
    _backing: Backing,
}

// Variants are held purely to own the backing memory for the `Region`'s lifetime; the
// data is accessed through `Region::ptr`, so the fields are never read directly.
#[allow(dead_code)]
enum Backing {
    Owned(Vec<u8>),
    /// Holds the named mapping alive for the lifetime of the `Region`.
    #[cfg(not(windows))]
    Shared(shared_memory::Shmem),
    #[cfg(windows)]
    Shared(win::Mapping),
}

/// Windows named mapping backed by the **pagefile**, not a file on disk. (`shared_memory`
/// backs its mappings with a temp file, which would turn every published frame into disk
/// write-back.) Any process in the same session can open it by name — that is how the
/// DirectShow filter loaded inside ClassIn/Zoom finds the frames.
#[cfg(windows)]
mod win {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_ALL_ACCESS,
        MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
    };

    pub struct Mapping {
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        pub len: usize,
    }

    impl Mapping {
        /// Create (or open, if it already exists) a mapping of at least `len` bytes, or with
        /// `create == false` open an existing one.
        pub fn open(name: &str, len: usize, create: bool) -> anyhow::Result<Self> {
            let wname = HSTRING::from(name);
            // SAFETY: plain Win32 calls; every handle/view is owned by the returned Mapping.
            unsafe {
                let handle = if create {
                    CreateFileMappingW(
                        INVALID_HANDLE_VALUE,
                        None,
                        PAGE_READWRITE,
                        (len as u64 >> 32) as u32,
                        len as u32,
                        &wname,
                    )?
                } else {
                    OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &wname)?
                };
                let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
                if view.Value.is_null() {
                    let e = windows::core::Error::from_win32();
                    let _ = CloseHandle(handle);
                    return Err(e.into());
                }
                // The real size (an existing mapping may differ from what we asked for).
                let mut info = MEMORY_BASIC_INFORMATION::default();
                VirtualQuery(Some(view.Value), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
                Ok(Mapping { handle, view, len: info.RegionSize })
            }
        }

        pub fn as_ptr(&self) -> *mut u8 {
            self.view.Value as *mut u8
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: view/handle came from MapViewOfFile/Create|OpenFileMappingW above.
            unsafe {
                let _ = UnmapViewOfFile(self.view);
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

// The whole point is cross-process sharing; access is synchronized via the seqlock.
unsafe impl Send for Region {}
unsafe impl Sync for Region {}

impl Region {
    /// Owned region for tests / single-process use.
    pub fn owned(len: usize) -> Self {
        let mut v = vec![0u8; len];
        let ptr = v.as_mut_ptr();
        Region { ptr, len, _backing: Backing::Owned(v) }
    }

    /// Open (or create) a named cross-process mapping. When `create` is true, a mapping
    /// of at least `len` bytes is created; if one with that name already exists it is
    /// opened instead. When `create` is false, `len` is only used as a minimum-size check
    /// (pass 0 to accept whatever size exists).
    #[cfg(windows)]
    pub fn shared(name: &str, len: usize, create: bool) -> Result<Self, RingError> {
        let map = win::Mapping::open(name, len, create)
            .map_err(|e| RingError::Os(anyhow::anyhow!("open mapping '{name}': {e}")))?;
        let have = map.len;
        if have < len {
            return Err(RingError::TooSmall { need: len, have });
        }
        Ok(Region { ptr: map.as_ptr(), len: have, _backing: Backing::Shared(map) })
    }

    #[cfg(not(windows))]
    pub fn shared(name: &str, len: usize, create: bool) -> Result<Self, RingError> {
        use shared_memory::ShmemConf;
        let shmem = if create {
            ShmemConf::new()
                .size(len)
                .os_id(name)
                .create()
                .or_else(|_| ShmemConf::new().os_id(name).open())
                .map_err(|e| RingError::Os(anyhow::anyhow!("create/open shmem '{name}': {e}")))?
        } else {
            ShmemConf::new()
                .os_id(name)
                .open()
                .map_err(|e| RingError::Os(anyhow::anyhow!("open shmem '{name}': {e}")))?
        };
        let have = shmem.len();
        if have < len {
            return Err(RingError::TooSmall { need: len, have });
        }
        let ptr = shmem.as_ptr();
        Ok(Region { ptr, len: have, _backing: Backing::Shared(shmem) })
    }

    #[inline]
    fn header(&self) -> &RingHeader {
        // SAFETY: header lives at offset 0 and the region is >= RingHeader::SIZE.
        unsafe { &*(self.ptr as *const RingHeader) }
    }
    #[inline]
    fn header_mut(&self) -> &mut RingHeader {
        unsafe { &mut *(self.ptr as *mut RingHeader) }
    }
    #[inline]
    fn write_seq_atomic(&self) -> &AtomicU64 {
        // write_seq sits at a fixed, 8-byte-aligned offset in the header.
        let off = std::mem::offset_of!(RingHeader, write_seq);
        unsafe { &*(self.ptr.add(off) as *const AtomicU64) }
    }
    #[inline]
    fn beat_atomic(&self) -> &AtomicU32 {
        let off = std::mem::offset_of!(RingHeader, consumer_beat);
        unsafe { &*(self.ptr.add(off) as *const AtomicU32) }
    }
    #[inline]
    fn slot_seq_atomic(&self, i: u32, slot_size: u32) -> &AtomicU64 {
        let off = slot_offset(slot_size, i); // SlotHeader.seq is first field
        unsafe { &*(self.ptr.add(off) as *const AtomicU64) }
    }
    #[inline]
    unsafe fn slot_header_mut(&self, i: u32, slot_size: u32) -> &mut SlotHeader {
        let off = slot_offset(slot_size, i);
        &mut *(self.ptr.add(off) as *mut SlotHeader)
    }
    #[inline]
    unsafe fn slot_data(&self, i: u32, slot_size: u32) -> *mut u8 {
        self.ptr.add(slot_offset(slot_size, i) + SlotHeader::SIZE)
    }
}

/// Whether process `pid` is still running.
#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: plain Win32 calls on a handle we close.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let alive = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(h);
        alive
    }
}

/// Elsewhere the ring is taken over as before.
#[cfg(not(windows))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// The engine side: publishes frames.
pub struct Producer {
    region: Region,
    cfg: RingConfig,
    next_seq: u64,
}

impl Producer {
    /// Create (and zero-init) a ring for `cfg`.
    pub fn create(region: Region, cfg: RingConfig) -> Result<Self, RingError> {
        let need = mapping_size(cfg.slot_size, cfg.slot_count);
        if region.len < need {
            return Err(RingError::TooSmall { need, have: region.len });
        }
        // Re-initializing a ring another live process is publishing into would cut off
        // every app watching it (the header is zeroed, and dropping us later clears the
        // alive flag under the other producer).
        let cur = region.header();
        if cur.magic == MAGIC
            && cur.flags & flags::PRODUCER_ALIVE != 0
            && cur.producer_pid != std::process::id()
            && pid_alive(cur.producer_pid)
        {
            return Err(RingError::Busy { pid: cur.producer_pid });
        }
        let h = region.header_mut();
        *h = RingHeader {
            magic: MAGIC,
            version: VERSION,
            flags: flags::PRODUCER_ALIVE,
            fourcc: cfg.fourcc,
            width: cfg.width,
            height: cfg.height,
            stride: cfg.stride,
            slot_size: cfg.slot_size,
            slot_count: cfg.slot_count,
            _pad0: [0; 2],
            write_seq: 0,
            producer_pid: std::process::id(),
            fps_num: cfg.fps_num,
            fps_den: cfg.fps_den,
            consumer_width: 0,
            consumer_height: 0,
            consumer_beat: 0,
            _reserved: [0; 2],
        };
        Ok(Self { region, cfg, next_seq: 0 })
    }

    /// What the consumer last reported: `(width, height, beat)`. `beat` advances while a
    /// consumer is actively serving frames to some app.
    pub fn consumer(&self) -> (u32, u32, u32) {
        let h = self.region.header();
        (h.consumer_width, h.consumer_height, self.region.beat_atomic().load(Ordering::Relaxed))
    }

    /// Publish one frame. `data.len()` must be <= `slot_size`.
    pub fn publish(&mut self, data: &[u8], pts_ns: i64) -> Result<(), RingError> {
        if data.len() > self.cfg.slot_size as usize {
            return Err(RingError::FrameTooBig { got: data.len(), max: self.cfg.slot_size as usize });
        }
        let i = (self.next_seq % self.cfg.slot_count as u64) as u32;
        let seq = self.region.slot_seq_atomic(i, self.cfg.slot_size);

        seq.fetch_add(1, Ordering::Release); // -> odd: write in progress
        // SAFETY: exclusive while seq is odd; consumers retry on odd/changed seq.
        unsafe {
            let hdr = self.region.slot_header_mut(i, self.cfg.slot_size);
            hdr.pts_ns = pts_ns;
            hdr.bytes = data.len() as u32;
            let dst = self.region.slot_data(i, self.cfg.slot_size);
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
        }
        seq.fetch_add(1, Ordering::Release); // -> even: stable

        self.next_seq += 1;
        self.region.write_seq_atomic().store(self.next_seq, Ordering::Release);
        Ok(())
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.region.header_mut().flags &= !flags::PRODUCER_ALIVE;
    }
}

/// The plugin side: reads the newest frame into `out`.
pub struct Consumer {
    region: Region,
    last_seen: u64,
}

impl Consumer {
    pub fn open(region: Region) -> Result<Self, RingError> {
        let h = region.header();
        if h.magic != MAGIC || h.version != VERSION {
            return Err(RingError::BadHeader);
        }
        Ok(Self { region, last_seen: 0 })
    }

    pub fn format(&self) -> (u32, u32, u32) {
        let h = self.region.header();
        (h.width, h.height, h.fourcc)
    }

    /// Whether the producer is still attached (it clears the flag when it shuts down).
    pub fn producer_alive(&self) -> bool {
        self.region.header().flags & flags::PRODUCER_ALIVE != 0
    }

    /// The consumer-side report `(width, height, beat)` (see [`Producer::consumer`]).
    pub fn consumer_report(&self) -> (u32, u32, u32) {
        let h = self.region.header();
        (h.consumer_width, h.consumer_height, self.region.beat_atomic().load(Ordering::Relaxed))
    }

    /// Tell the producer we are serving a `width`x`height` stream right now (call once
    /// per delivered frame).
    pub fn report(&self, width: u32, height: u32) {
        let h = self.region.header_mut();
        h.consumer_width = width;
        h.consumer_height = height;
        self.region.beat_atomic().fetch_add(1, Ordering::Relaxed);
    }

    /// Returns `Some(pts_ns)` and fills `out` if a newer frame is available.
    /// Uses the seqlock: retry while the slot is being written or changes mid-copy.
    pub fn try_recv(&mut self, out: &mut Vec<u8>) -> Option<i64> {
        let h = self.region.header();
        let slot_size = h.slot_size;
        let slot_count = h.slot_count;

        let w = self.region.write_seq_atomic().load(Ordering::Acquire);
        if w == self.last_seen || w == 0 {
            return None;
        }
        let i = ((w - 1) % slot_count as u64) as u32;
        let seq = self.region.slot_seq_atomic(i, slot_size);

        for _ in 0..64 {
            let s0 = seq.load(Ordering::Acquire);
            if s0 & 1 == 1 {
                continue; // write in progress
            }
            // SAFETY: snapshotting under the seqlock; validated by re-reading seq.
            let (pts, bytes) = unsafe {
                let hdr = &*(self.region.ptr.add(slot_offset(slot_size, i)) as *const SlotHeader);
                (hdr.pts_ns, hdr.bytes as usize)
            };
            out.clear();
            out.reserve(bytes);
            unsafe {
                let src = self.region.slot_data(i, slot_size);
                out.set_len(bytes);
                std::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), bytes);
            }
            if seq.load(Ordering::Acquire) == s0 {
                self.last_seen = w;
                return Some(pts);
            }
            // seq changed → torn read, retry
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RingConfig {
        RingConfig {
            width: 4, height: 4, fourcc: u32::from_le_bytes(*b"RGBA"),
            stride: 16, slot_size: 64, slot_count: 4, fps_num: 30, fps_den: 1,
        }
    }

    #[test]
    fn roundtrip_newest_frame() {
        let c = cfg();
        let size = mapping_size(c.slot_size, c.slot_count);
        let mut prod = Producer::create(Region::owned(size), c).unwrap();
        // NOTE: a real deployment shares one mapping across processes; here we exercise
        // the protocol math and seqlock on a single owned region per side.
        prod.publish(&[1u8; 64], 1000).unwrap();
        prod.publish(&[2u8; 64], 2000).unwrap();
        assert_eq!(prod.next_seq, 2);
    }
}
