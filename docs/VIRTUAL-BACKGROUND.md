# MarkCam virtual background — how it works, how to test it, what to improve

Handoff for anyone (person or AI model) continuing work on the virtual background. Written
2026-10-15 after tuning on a real webcam clip. Platform: Windows 10/11, Rust (MinGW
toolchain `stable-x86_64-pc-windows-gnu`, no MSVC), egui UI.

## Pipeline (per camera frame, 1280x720 @ 30 fps)

1. **Capture thread** (`crates/ovc-app/src/main.rs`, `worker` → `Fx::run`) gets frame `seq`.
2. **Segmentation thread** (`crates/ovc-app/src/vbg.rs`, `SegWorker` / `worker`) takes the
   newest frame from `Capture::latest`:
   - `small_rgb`: frame → 640x360 planar RGB.
   - Model (ONNX Runtime 1.24, loaded at run time from `onnxruntime.dll` next to the exe,
     DirectML GPU provider, CPU fallback; `crates/ovc-ml/src/seg.rs`):
     - **RVM ResNet50** (`rvm_resnet50_fp32.onnx`, "Best") or **RVM MobileNetV3**
       (`rvm_mobilenetv3_fp32.onnx`, "Light"), input 640x360, `downsample_ratio` 0.5,
       recurrent state r1..r4 fed back every frame. Outputs `pha` (alpha) and `fgr`
       (foreground color with the background tint removed).
     - Auto: Best unless its average time on frames 10..40 is over 28 ms → Light.
     - MODNet (`modnet_webcam.onnx`) only if no RVM file is present (then a color guided
       filter refines its edges: `fast_guided_filter_rgb`).
   - `keep_main_person`: drops separate blobs (posters, empty chair) on a half-size grid.
   - `shape`: alpha contrast (smoothstep 0.12..0.88).
   - Motion gate: a 64x36 luma thumbnail; when nothing moved, only every 3rd frame is
     segmented.
   - Result: `Matte { w, h, data (alpha u8), seq, fgr (RGB u8) }` pushed to a ring of the
     last 8 (`Mattes`).
3. **Sync** (`Fx::sync_with_matte`): each camera frame waits (≤4 frames) until a matte for
   its own `seq` exists, so the outline never trails a moving head/hand.
4. **Composite** (`Compositor::composite`): picks the matte nearest to the frame's `seq`,
   upsamples alpha once per matte to frame size, copies runs of alpha 0/255, blends only
   the outline — using `fgr` for the person's color there (no dark/light fringe).
   Backgrounds: built-in gradients (`builtin`), pictures/videos (cover fit), or blur (8x
   shrunk, box blurred, enlarged, refreshed every 3rd frame).

## Measured (desktop: Ryzen 5 2600X + GTX 1070)

| | model (GPU, wall) | model CPU | post (CPU) | whole app CPU |
|---|---|---|---|---|
| RVM ResNet50 640x360 | ~20 ms | ~9 ms | ~5 ms | ~630 ms/s (moving clip) |
| RVM MobileNetV3 640x360 | ~16 ms | ~8.6 ms | ~5 ms | ~320 ms/s still, ~660 moving |
| no background | — | — | — | ~100 ms/s |

The laptop (i5-7200U, Intel HD 620) is not measured yet.

## Reproduce / evaluate

```powershell
# 1. Build
cargo build --release -p ovc-app
# 2. Record a 30 s clip from the webcam (MarkCam and ClassIn closed)
powershell -ExecutionPolicy Bypass -File tools\vbg-eval\record.ps1
# 3. Run the clip through MarkCam, save every 15th composited frame
powershell -ExecutionPolicy Bypass -File tools\vbg-eval\eval.ps1 -Name base
#    variants:  -Extra "OVC_VBG_RVM=C:\path\rvm_mobilenetv3_fp32.onnx"
#               -Extra "OVC_VBG_WIDTH=1280;OVC_VBG_RATIO=0.25"
#               -Extra "OVC_VBG_SHAPE=0.3,0.7"
# 4. Contact sheet for review (zoom with -Crop x,y,w,h)
powershell -ExecutionPolicy Bypass -File tools\vbg-eval\sheet.ps1 -Name base -Frames 330,345,420,510,660,810
# Per-stage timings: run with $env:OVC_SEG_PROFILE=1 and read stderr.
# Unit tests: cargo test --release -p ovc-app vbg
```

Models live in `%APPDATA%\MarkCam\models` (dev) or `<install dir>\models`; ONNX Runtime +
DirectML in `%APPDATA%\MarkCam\runtime` (dev) or next to the installed exe. Downloads:
RVM — github.com/PeterL1n/RobustVideoMatting releases v1.0.0 (GPL-3.0); MODNet —
github.com/yakhyo/modnet releases "weights" (Apache-2.0); ONNX Runtime — NuGet
Microsoft.ML.OnnxRuntime.DirectML 1.24.4; DirectML — NuGet Microsoft.AI.DirectML 1.15.4.

## What was tried and why it is (not) there

- tract (pure-Rust ONNX): MODNet 900 ms/frame → replaced by ONNX Runtime + DirectML.
- MediaPipe selfie segmentation: blotchy, false positives on posters → rejected.
- MODNet (frame by frame): edges lagged and shimmered → RVM (recurrent) instead.
- Showing frames with the *latest* matte → ghost of the real room beside a moving head →
  per-frame matte + frame sync (`sync_with_matte`).
- "Learn my room" (remembered empty-room plate, cleared pixels matching it): helped the
  headset-band gap on a low-res test, but with a white T-shirt in front of a white wall it
  punched black holes; restricted to the outline it no longer made a visible difference →
  removed.
- Local wall-color matching near the outline: same white-shirt problem → removed.
- Full-resolution RVM (1280x720, ratio 0.25): no visible gain on the clip, 4x the CPU
  post-processing → not used.
- Spending the spare time of "Best" (GTX 1070: 22 of 33 ms used): ResNet50 at 960x540
  (ratio 0.375, 29 ms) — not better, a fast-moving hand got *more* transparent; an extra
  color guided filter on RVM's alpha (`OVC_VBG_GUIDED=1`) — no visible difference. The
  remaining errors come from motion blur in the camera image, not from matte resolution.

## Known issues / ideas

- Fast hand waves: the camera itself motion-blurs the hand, so those pixels are a mix of
  hand and room; the matte goes semi-transparent there. Better light (shorter exposure)
  helps most. Ideas: temporal hand-region hardening, or a model trained with motion blur.
- Occasional small false foreground patches next to the body (a poster label for a frame).
  Idea: temporal consistency check on new small regions before accepting them.
- Low light: the C920 drops to 15 fps and the image gets noisy → worse edges.
- CPU post-processing could move to the GPU (wgpu is already in the project) — alpha
  upsample + composite are ideal shaders.
- Laptop (Intel HD 620) timing of RVM Light is unknown.
- Licensing: RVM is GPL-3.0; shipping it means the release must be GPL-3.0 (or download
  the model separately). MODNet is Apache-2.0. The bundled ManyCam backgrounds are for the
  owner's personal use only and must not be in a public release.
