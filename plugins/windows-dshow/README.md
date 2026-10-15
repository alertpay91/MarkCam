# windows-dshow

Windows virtual camera: a **DirectShow source filter** (`markcam_dshow.dll`, crate
`ovc-dshow`) registered under *Video Capture Sources* as **"MarkCam"**. Written in Rust
against the raw DirectShow interfaces — no base classes, no Visual Studio needed.

- Apps that use DirectShow capture (ClassIn, Zoom, OBS, browsers…) load the DLL and see a
  webcam named "MarkCam" offering 1280x720 / 1920x1080 / 640x480 at 30 fps in NV12,
  YUY2 or RGB32.
- The filter reads MarkCam's final RGBA frames from the shared `OVC1` ring
  (`ovc_ipc::VCAM_RING_NAME`, a pagefile-backed named mapping), letterbox-scales them to
  the negotiated size and converts the pixel format. With the Studio closed it shows a
  dark placeholder frame.
- Bitness must match the host app. ClassIn is 64-bit, so the x86_64 build covers it.
  (A 32-bit build would need the `i686-pc-windows-gnu` target + toolchain.)

## Build / install

```sh
cargo build --release -p ovc-dshow        # -> target/release/markcam_dshow.dll
regsvr32 markcam_dshow.dll               # elevated; writes HKLM
regsvr32 /u markcam_dshow.dll            # uninstall
```

The Studio has an "Install virtual camera (admin)" button that does the same with one UAC
prompt, using the DLL next to the exe.

## Checks

```sh
cargo run --release -p ovc-dshow --example probe       # in-process: connect + 2 s of NV12 frames
cargo run --release -p ovc-dshow --example devenum     # registered DLL via the system device enumerator
cargo run --release -p ovc-dshow --example ring_watch  # fps the Studio is publishing
```

A Media Foundation virtual camera (`MFCreateVirtualCamera`, Windows 11 only) would add
apps that enumerate MF devices only; not needed for ClassIn.
