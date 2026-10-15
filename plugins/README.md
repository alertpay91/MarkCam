# Native camera plugins

Thin, per-OS **consumers** of the `ovc-ipc` ring buffer (see
`../docs/SHARED_MEMORY_PROTOCOL.md`). They contain no effects logic — they map the shared
ring, read the newest frame with the seqlock, and hand it to the OS camera stack.

| Folder           | Tech                                   | Installs as |
|------------------|----------------------------------------|-------------|
| `windows-dshow`  | C++ DirectShow filter + MF VirtualCamera | `CLSID_VideoInputDeviceCategory` webcam |
| `macos-cmio`     | Swift/C++ CoreMediaIO camera extension | System camera extension |
| `linux-v4l2`     | shell/helper around `v4l2loopback`     | `/dev/videoN` |

Each plugin links a tiny C ABI reader generated from the Rust `ovc-ipc` protocol structs
(`#[repr(C)]`), so Rust and the native side stay byte-compatible.
