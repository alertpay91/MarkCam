# OpenVCam shared-memory protocol (`OVC1`)

A single named shared mapping transports frames from the engine (producer) to the native
camera plugin (consumer). Unlike ManyCam's undocumented `manycam_video_source_*` mapping
guarded by a global event-as-mutex, this is **versioned and lock-free** (a seqlock ring).

## Naming

```
<prefix>.<session_id>.video      # the ring mapping
<prefix>.<session_id>.ctrl       # optional control/registration region
```

`<prefix>` = `openvcam` (overridable). `session_id` is a lowercase hex u64 chosen by the
producer and handed to the plugin via the OS registration (registry value / plist / config).
On Windows the names live under `Local\` (per-session) unless `--global` is set.

## Layout

All integers little-endian. One `RingHeader`, then `slot_count` slots.

```
RingHeader  (64 bytes, cache-line aligned)
  0x00  u32   magic            = 0x3143564F  ("OVC1")
  0x04  u16   version          = 1
  0x06  u16   flags            // bit0: producer-alive
  0x08  u32   fourcc           // 'NV12','I420','YUY2','RGBA','BGRA'
  0x0C  u32   width
  0x10  u32   height
  0x14  u32   stride           // bytes per row of plane 0
  0x18  u32   slot_size        // bytes reserved per slot (>= frame size)
  0x1C  u32   slot_count       // number of ring slots (power of two recommended)
  0x20  u64   write_seq        // ATOMIC. total frames published. slot = (write_seq-1) % slot_count
  0x28  u32   producer_pid
  0x2C  u32   fps_num
  0x30  u32   fps_den
  0x34  [12]  reserved (zero)

Slot[i]  at  0x40 + i * (32 + slot_size)
  SlotHeader (32 bytes)
    +0x00 u64  seq             // ATOMIC. odd = write in progress, even/stable = readable
    +0x08 i64  pts_ns          // presentation timestamp, nanoseconds
    +0x10 u32  bytes           // valid payload length for this frame
    +0x14 [12] reserved
  data [slot_size]             // pixel payload
```

## Producer (engine, `ovc-ipc::Producer`)

For each frame, using slot `s = write_seq % slot_count`:

1. `slot.seq.fetch_add(1, Release)`  → now **odd** (write in progress).
2. write `data`, `bytes`, `pts_ns`.
3. `slot.seq.fetch_add(1, Release)`  → back to **even** (stable).
4. `header.write_seq.store(write_seq + 1, Release)`.

No locks; a writer never blocks a reader.

## Consumer (OS plugin, `ovc-ipc::Consumer`)

1. `w = header.write_seq.load(Acquire)`; if `w == last_seen`, no new frame.
2. `s = (w - 1) % slot_count`.
3. **seqlock read**: `s0 = slot.seq.load(Acquire)`; if odd, retry. Copy `data[..bytes]`.
   `s1 = slot.seq.load(Acquire)`; if `s1 != s0`, the slot was overwritten mid-read → retry
   (or skip to the newest frame).
4. deliver, set `last_seen = w`.

Consumers that fall behind simply observe a higher `write_seq` and skip stale frames — the
producer never waits. A dead producer is detected via `flags` bit0 + a liveness timestamp in
the ctrl region.

## Format negotiation

`fourcc`/`width`/`height` are set by the producer after the plugin advertises supported media
types (I420/NV12/YUY2 for webcam compatibility, matching what ManyCam exposed). Changing
format re-initializes the mapping (new `session_id`) rather than mutating a live ring.
