# Running the end-to-end pipeline

The MVP run loop is CPU-only and needs no camera or GPU: a **test-pattern source** →
**CPU effects** → a **sink** (stats / raw file / shared-memory ring). This exercises the
whole `source → effects → convert → sink` path and the real `OVC1` shared-memory transport.

> Not yet wired: physical-camera capture (behind the `camera` feature + `nokhwa`), GPU
> effects (`ovc-gpu`/wgpu), and the native OS camera plugins. Those replace, respectively,
> the test source, the CPU effect path, and the `ipc-recv` consumer below.

## Desktop app & packaging

```sh
cargo run -p ovc-app                          # dev
cargo build --release -p ovc-app              # -> target/release/markcam.exe (self-contained)
```

The release `markcam.exe` embeds an icon + version info, runs with no console window,
and has no MinGW DLL dependencies (gcc runtime is static-linked) — it runs on any Windows
x64 machine. A packaged `dist/MarkCam-win64.zip` (exe + `markcam_dshow.dll`
virtual-camera filter + README + LICENSE; see `plugins/windows-dshow`) is produced
by the steps in the repo. A Linux `.deb` is a later task (the same `ovc-studio` binary builds
on Linux; v4l2 virtual-camera output via v4l2loopback).

## Build prerequisites

### Linux (Mint / Ubuntu) — not yet verified on hardware

The camera path is cross-platform (nokhwa → v4l2, MJPEG decoded by libjpeg-turbo via
`mozjpeg`), so the same code runs on Linux. Expected packages:

```sh
sudo apt install build-essential pkg-config nasm clang libclang-dev \
    libxkbcommon-dev libwayland-dev libgtk-3-dev mesa-vulkan-drivers
cargo build --release -p ovc-app   # -> target/release/markcam
```

- `nasm` matters for performance: without it libjpeg-turbo builds without SIMD and MJPEG
  decode costs several times more CPU.
- `clang`/`libclang-dev`: bindgen for the v4l2 bindings. `libgtk-3-dev`: file dialogs.
- Vulkan: NVIDIA proprietary driver or Mesa (`vulkaninfo` should list the GPU).
- Your user must be in the `video` group to open `/dev/video*`.

### Windows

Verified building + running on Windows 10 with the **GNU** toolchain (no Visual Studio
needed):

```sh
rustup toolchain install stable-x86_64-pc-windows-gnu
```

The transitively-pulled `windows`/`windows-sys` crates invoke `dlltool` to build import
libraries, which needs a full **MinGW-w64** on `PATH` (`gcc`, `as`, `dlltool` together) —
the toolchain's bundled `self-contained\dlltool.exe` alone is not enough:

```powershell
choco install mingw -y                      # installs to C:\ProgramData\mingw64\mingw64\bin
$env:PATH = "C:\ProgramData\mingw64\mingw64\bin;$env:PATH"
```

With MSVC build tools present you can use the default `-msvc` toolchain instead and skip
MinGW entirely.

## Quick checks

```sh
cargo run -p ovc-cli -- list-effects          # color_grade, background_blur, chroma_key
cargo run -p ovc-cli -- list-cameras          # (empty until the camera feature lands)
```

## Run → stats (simplest end-to-end)

```sh
cargo run -p ovc-cli -- run --width 1280 --height 720 --fps 30 \
    --effect color_grade --sink stats --frames 150
```
Prints per-second throughput and a final average fps. Ctrl-C stops a non-`--frames` run.

## Run → raw file (visually verifiable)

```sh
cargo run -p ovc-cli -- run --format nv12 --sink file --out out.nv12 --frames 90
ffplay -f rawvideo -pixel_format nv12 -video_size 1280x720 out.nv12
```

## Run → shared-memory ring → consumer (the real vcam transport)

Terminal A (producer / engine):
```sh
cargo run -p ovc-cli -- run --sink ring --name openvcam.dev.video \
    --format nv12 --effect color_grade
```
Terminal B (consumer — stands in for the native camera plugin):
```sh
cargo run -p ovc-cli -- ipc-recv --name openvcam.dev.video --out recv.nv12
```
B prints the negotiated `WxH`/fourcc and the received fps; `recv.nv12` is playable as above.
This is exactly the handoff the Windows/macOS/Linux plugins will perform in-process.

## GPU effects

Add `--gpu` to run the effect chain on the GPU (wgpu: Vulkan/DX12/Metal, with a software
fallback). Effects become nearly free and stacking more passes costs ~nothing, whereas each
CPU effect linearly eats the frame budget:

```sh
cargo run -p ovc-cli --release -- run --gpu --effect color_grade --sink stats --frames 600 --fps 100000
```

When the output format is NV12, the RGBA→NV12 conversion also runs on the GPU (a compute
shader, `shaders/nv12_pack.wgsl`) and only the compact NV12 buffer is read back — so the CPU
does no per-pixel work at all.

Measured on a GTX 1070 (`color_grade`, release), `avg_fps`:

| Config | CPU | GPU (CPU convert) | GPU (GPU convert) |
|---|----:|----:|----:|
| 720p, 1 effect  | 12.9 | 20.1 | **122.6** |
| 720p, 4 effects |  6.4 | 21.1 | **118.0** |
| 1080p, 1 effect |  5.8 |  9.4 |  **56.1** |

Moving the convert to the GPU removed the per-pixel CPU stage and shrank readback ~2.7×
(1.5 vs 4 bytes/px), lifting 720p from ~20 to ~122 fps (~9.5× over CPU). Effect count stays
free on the GPU (4 passes ≈ 1). GPU-packed NV12 matches the CPU result within ±1/byte
(shader rounding).

All three built-ins now have GPU shaders: `color_grade`, `chroma_key` (keys the key color
to black with a soft edge; real background compositing is a later feature), and
`background_blur` (full-frame 5×5 Gaussian; gating by an ONNX segmentation mask so only the
background blurs is the next sub-step). They chain in any order:

```sh
cargo run -p ovc-cli --release -- run --gpu \
    --effect color_grade --effect background_blur --effect chroma_key \
    --sink stats --frames 300
```

Effects run on the GPU (`--gpu`); without it they fall back to CPU, where only
`color_grade` has a CPU implementation (the others are no-ops with a warning).

## Release build (much higher fps)

```sh
cargo run -p ovc-cli --release -- run --effect color_grade --sink stats --frames 300
```
