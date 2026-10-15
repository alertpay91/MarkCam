# Architecture

## Goals (and how we beat ManyCam)

| Area | ManyCam (observed) | OpenVCam |
|------|--------------------|----------|
| Effects compute | CPU-heavy (OpenCV/dlib on CPU), per-frame copies | GPU render graph (`wgpu`), zero-copy textures |
| Face/ML | dlib 19.15 HOG + ERT landmarks, models baked into the DLL | ONNX models, downloaded via manifest, hot-swappable |
| VCam transport | Undocumented named mapping `manycam_video_source_*`, single global **event used as a mutex**, 4-slot ring, bare `u32` counter | Documented `OVC1` protocol, **lock-free seqlock** ring, N slots, per-slot seq+timestamp |
| Portability | Windows-only DirectShow | One `VirtualCamera` trait: DShow+MF / CMIO / v4l2loopback |
| Architecture | Monolithic C++ | Rust workspace, trait-driven, each concern is its own crate |

## The pipeline

A pipeline is a **DAG of `Node`s**. Edges carry `FrameRef`s. Three node kinds:

- **Source** — produces frames (camera, screen, image, media file).
- **Transform** — consumes and produces (effects, ML overlays, compositor).
- **Sink** — consumes frames (virtual camera, preview, recorder).

Frames are reference-counted and **GPU-resident by default** (`Frame::Gpu(TextureHandle)`).
A frame is only pulled to the CPU (`Frame::Cpu`) when a sink needs system memory — i.e. at
the `ovc-vcam` boundary, where we also do the GPU→NV12 color conversion in a shader and read
back the already-packed bytes. This keeps per-frame `memcpy`s off the hot path.

Scheduling is pull-based: the sink's cadence (target fps) drives `Pipeline::tick(pts)`,
which walks the DAG in topological order. The GPU work for one frame is recorded into a
single command encoder (`ovc-gpu::RenderGraph`) and submitted once.

## Effects

`ovc-effects::Effect` is: a WGSL shader + a typed parameter block (`bytemuck`-able uniform) +
an optional set of input texture bindings (e.g. a segmentation mask from `ovc-ml`). Effects
are registered by name so the UI and project files can reference them declaratively.

Built-ins mirror the ManyCam feature set: color grade / LUT, chroma key, background
blur/replace (fed by the segmentation mask), face landmarks overlay & masks, lower-thirds /
text, picture-in-picture compositor, flip/rotate/crop/zoom.

## ML

`ovc-ml` exposes small traits — `FaceDetector`, `LandmarkEstimator`, `Segmenter` — with ONNX
Runtime backends (`ort`). Models are described in `models/manifest.toml` (url + sha256 +
input layout) and fetched on first use, never committed. Inference runs on a worker thread
and publishes results (boxes, landmarks, mask texture) that effects sample on the next frame;
ML never blocks the render cadence.

## Virtual camera

`ovc-vcam::VirtualCamera` trait:

```rust
fn negotiate(&mut self, formats: &[VideoFormat]) -> Result<VideoFormat>;
fn start(&mut self, fmt: VideoFormat) -> Result<()>;
fn submit(&mut self, frame: &CpuFrame) -> Result<()>; // pushes into the ring buffer
fn stop(&mut self);
```

The Rust side is always the **producer**: it writes frames into the `ovc-ipc` ring buffer.
The native OS plugin (loaded into the consuming app — Zoom, OBS, a browser) is the
**consumer**: it maps the same ring and serves frames to the app. See
`docs/SHARED_MEMORY_PROTOCOL.md`.

## Threads

- **Capture thread** — owns the capture device, uploads to GPU.
- **Render thread** — owns the `wgpu` device, runs the graph, does readback.
- **ML thread(s)** — ONNX inference, lock-free result handoff.
- **VCam thread** — drains readback frames into the ring at the negotiated cadence.

Cross-thread frame handoff uses bounded SPSC queues; back-pressure drops the oldest frame
rather than stalling capture.
