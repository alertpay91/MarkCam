//! Lock-free shared-memory ring buffer — the `OVC1` transport between the engine
//! (producer) and the native camera plugin (consumer).
//!
//! See `docs/SHARED_MEMORY_PROTOCOL.md` for the wire layout. The design replaces
//! ManyCam's single global event-used-as-a-mutex with a **seqlock** ring: the producer
//! never blocks, and a lagging consumer simply skips to the newest frame.

pub mod protocol;
mod ring;

pub use protocol::{RingHeader, SlotHeader, MAGIC, VERSION};
pub use protocol::{mapping_size, slot_offset};
pub use ring::{Consumer, Producer, Region, RingConfig, RingError};

/// The well-known ring the app publishes its final (post-effects) RGBA frames into and the
/// OS virtual-camera plugin reads. One per user session; there is one virtual camera.
#[cfg(windows)]
pub const VCAM_RING_NAME: &str = "Local\\OpenVCam.Video.v1";
#[cfg(not(windows))]
pub const VCAM_RING_NAME: &str = "/OpenVCam.Video.v1";

/// Fixed slot capacity of the virtual-camera ring: one 1920x1080 RGBA frame. Fixed so a
/// restarted app can reuse a mapping the plugin still holds open, whatever the resolution.
pub const VCAM_MAX_FRAME_BYTES: u32 = 1920 * 1080 * 4;
/// Slots in the virtual-camera ring.
pub const VCAM_SLOTS: u32 = 3;
