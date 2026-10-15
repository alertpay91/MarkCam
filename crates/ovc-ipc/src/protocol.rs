//! On-the-wire layout for the `OVC1` shared ring. `#[repr(C)]` + `bytemuck` so both
//! the Rust producer and the native C/C++/Swift consumers agree byte-for-byte.

use bytemuck::{Pod, Zeroable};

/// Magic = "OVC1" little-endian.
pub const MAGIC: u32 = u32::from_le_bytes(*b"OVC1");
pub const VERSION: u16 = 1;

/// Ring header at offset 0 of the mapping (80 bytes, 8-byte aligned, no padding so it
/// stays `Pod`). The mapping base is page-aligned and `write_seq` lands on an 8-byte
/// boundary (offset 40), so atomic access is well-aligned without over-aligning the type.
///
/// `write_seq` is accessed atomically (see `ring.rs`); it is a plain `u64` here so the
/// struct stays `Pod` for layout purposes. Never read it through this field directly —
/// go through [`crate::Producer`] / [`crate::Consumer`].
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct RingHeader {
    pub magic: u32,
    pub version: u16,
    pub flags: u16,
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub slot_size: u32,
    pub slot_count: u32,
    /// Pads the 36 bytes before `write_seq` up to a multiple of 8 so the u64 lands on an
    /// 8-byte boundary (offset 40) with no implicit padding (required by `Pod`).
    pub _pad0: [u32; 2],
    pub write_seq: u64,
    pub producer_pid: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    /// Written by the consumer (virtual-camera plugin): the size the app asked it for…
    pub consumer_width: u32,
    pub consumer_height: u32,
    /// …and a counter it bumps every frame it serves, so the producer can tell whether
    /// anyone is actually watching (and skip work when not). Accessed atomically.
    pub consumer_beat: u32,
    pub _reserved: [u32; 2],
}

pub mod flags {
    /// Producer is alive and publishing.
    pub const PRODUCER_ALIVE: u16 = 1 << 0;
}

/// 32-byte per-slot header. `seq` is the seqlock counter: odd = write in progress.
///
/// Plain `#[repr(C)]` (align 8) — slots start at `RingHeader::SIZE` (80, only 16-aligned),
/// so over-aligning the type to 32 would make those pointers misaligned. Align 8 is all
/// the atomic `seq` (u64, first field) needs, and every slot offset is a multiple of 8.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct SlotHeader {
    pub seq: u64,
    pub pts_ns: i64,
    pub bytes: u32,
    pub _reserved: [u32; 3],
}

impl RingHeader {
    pub const SIZE: usize = std::mem::size_of::<RingHeader>();
}
impl SlotHeader {
    pub const SIZE: usize = std::mem::size_of::<SlotHeader>();
}

/// Total mapping size for `slot_count` slots of `slot_size` bytes each.
pub const fn mapping_size(slot_size: u32, slot_count: u32) -> usize {
    RingHeader::SIZE + (SlotHeader::SIZE + slot_size as usize) * slot_count as usize
}

/// Byte offset of slot `i` (its `SlotHeader`) from the start of the mapping.
pub const fn slot_offset(slot_size: u32, i: u32) -> usize {
    RingHeader::SIZE + (SlotHeader::SIZE + slot_size as usize) * i as usize
}
