//! Headless runner: wires `source → effects → sink` and runs a real, paced loop.
//!
//! The MVP path is CPU-only and hardware-free: a test-pattern source, CPU effects, and a
//! sink (stats / raw file / shared-memory ring). The ring sink speaks the `OVC1` protocol,
//! so a native camera plugin — or `ovc ipc-recv` on the same machine — consumes the frames.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand, ValueEnum};
use ovc_capture::{CaptureSource, TestPatternSource};
use ovc_core::{PixelFormat, VideoFormat};
use ovc_effects::{Effect, Registry};
use ovc_ipc::{mapping_size, Consumer, Producer, Region, RingConfig};

#[derive(Parser)]
#[command(name = "ovc", version, about = "OpenVCam headless runner")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List physical cameras.
    ListCameras,
    /// List built-in effect ids.
    ListEffects,
    /// Run source → effects → sink.
    Run(RunArgs),
    /// Consume an OVC1 ring (verification / headless preview counterpart to `run`).
    IpcRecv {
        /// Shared mapping name, matching `run --name`.
        #[arg(long, default_value = "openvcam.dev.video")]
        name: String,
        /// Optional path to dump received frames (raw, concatenated).
        #[arg(long)]
        out: Option<String>,
    },
}

#[derive(Args)]
struct RunArgs {
    /// Frame source.
    #[arg(long, value_enum, default_value = "test")]
    source: Source,
    /// Camera index (only for `--source camera`).
    #[arg(long, default_value_t = 0)]
    camera: u32,
    #[arg(long, default_value_t = 1280)]
    width: u32,
    #[arg(long, default_value_t = 720)]
    height: u32,
    #[arg(long, default_value_t = 30)]
    fps: u32,
    /// Output pixel format.
    #[arg(long, value_enum, default_value = "nv12")]
    format: OutFormat,
    /// Effect ids to chain, in order (repeatable). See `list-effects`.
    #[arg(long = "effect")]
    effects: Vec<String>,
    /// Where frames go.
    #[arg(long, value_enum, default_value = "stats")]
    sink: SinkKind,
    /// Output file for `--sink file`.
    #[arg(long)]
    out: Option<String>,
    /// Shared mapping name for `--sink ring`.
    #[arg(long, default_value = "openvcam.dev.video")]
    name: String,
    /// Ring slot count for `--sink ring`.
    #[arg(long, default_value_t = 4)]
    slots: u32,
    /// Stop after N frames (default: run until Ctrl-C).
    #[arg(long)]
    frames: Option<u64>,
    /// Run effects on the GPU (wgpu) instead of the CPU fallback.
    #[arg(long)]
    gpu: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Source {
    Test,
    Camera,
}

#[derive(Clone, Copy, ValueEnum)]
enum SinkKind {
    Stats,
    File,
    Ring,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutFormat {
    Nv12,
    I420,
    Yuy2,
    Rgba,
    Bgra,
}

impl OutFormat {
    fn to_core(self) -> PixelFormat {
        match self {
            OutFormat::Nv12 => PixelFormat::Nv12,
            OutFormat::I420 => PixelFormat::I420,
            OutFormat::Yuy2 => PixelFormat::Yuy2,
            OutFormat::Rgba => PixelFormat::Rgba8,
            OutFormat::Bgra => PixelFormat::Bgra8,
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    match Cli::parse().cmd {
        Cmd::ListCameras => {
            let cams = ovc_capture::list_cameras()?;
            if cams.is_empty() {
                println!("(no cameras enumerated — camera capture is not implemented yet)");
            }
            for cam in cams {
                println!("{:>2}  {}", cam.index, cam.name);
            }
            Ok(())
        }
        Cmd::ListEffects => {
            let reg = Registry::with_builtins();
            let mut ids: Vec<_> = reg.ids().copied().collect();
            ids.sort();
            for id in ids {
                println!("{id}");
            }
            Ok(())
        }
        Cmd::Run(args) => run(args),
        Cmd::IpcRecv { name, out } => ipc_recv(&name, out.as_deref()),
    }
}

/// Install a Ctrl-C handler returning a shared "keep running" flag.
fn running_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(true));
    let r = flag.clone();
    let _ = ctrlc::set_handler(move || r.store(false, Ordering::SeqCst));
    flag
}

fn run(args: RunArgs) -> anyhow::Result<()> {
    // --- source --- (open first; the pipeline follows the source's actual resolution)
    let requested = VideoFormat {
        format: PixelFormat::Rgba8,
        width: args.width,
        height: args.height,
        fps_num: args.fps,
        fps_den: 1,
    };
    let mut source: Box<dyn CaptureSource> = match args.source {
        Source::Test => Box::new(TestPatternSource::new(args.width, args.height, args.fps, 1)),
        Source::Camera => Box::new(ovc_capture::CameraSource::open(args.camera, requested)?),
    };
    let in_fmt = source.format();
    let (w, h) = (in_fmt.width, in_fmt.height);
    let out_fmt = VideoFormat {
        format: args.format.to_core(),
        width: w,
        height: h,
        fps_num: in_fmt.fps_num,
        fps_den: in_fmt.fps_den,
    };

    // --- effects ---
    let reg = Registry::with_builtins();
    let mut fx: Vec<Box<dyn Effect>> = Vec::new();
    for id in &args.effects {
        let e = reg
            .create(id)
            .ok_or_else(|| anyhow::anyhow!("unknown effect '{id}' (see `ovc list-effects`)"))?;
        if !args.gpu && !e.has_cpu() {
            tracing::warn!("effect '{id}' has no CPU implementation yet — it will be a no-op");
        }
        fx.push(e);
    }

    // --- sink ---
    let mut sink: Box<dyn Sink> = match args.sink {
        SinkKind::Stats => Box::new(StatsSink::new()),
        SinkKind::File => {
            let path = args.out.clone().ok_or_else(|| anyhow::anyhow!("--sink file needs --out"))?;
            Box::new(FileSink::new(&path)?)
        }
        SinkKind::Ring => Box::new(RingSink::new(&args.name, out_fmt, args.slots)?),
    };

    // --- optional GPU effect chain ---
    let mut gpu: Option<(ovc_gpu::GpuContext, ovc_gpu::EffectChain)> = if args.gpu {
        if fx.is_empty() {
            anyhow::bail!("--gpu needs at least one --effect");
        }
        let ctx = ovc_gpu::GpuContext::new()?;
        tracing::info!("GPU adapter: {}", ctx.adapter_info);
        let shaders: Vec<&str> = fx.iter().map(|e| e.wgsl()).collect();
        let sizes: Vec<usize> = fx.iter().map(|e| e.uniform_bytes().len()).collect();
        // Pack to NV12 on the GPU when that's the target; otherwise read back RGBA and
        // let the CPU convert to the requested format.
        let out_mode = if out_fmt.format == PixelFormat::Nv12 {
            ovc_gpu::OutputMode::Nv12
        } else {
            ovc_gpu::OutputMode::Rgba
        };
        let chain = ovc_gpu::EffectChain::new(&ctx, w, h, &shaders, &sizes, out_mode)?;
        Some((ctx, chain))
    } else {
        None
    };

    tracing::info!(
        ?out_fmt, effects = ?args.effects, gpu = args.gpu,
        "pipeline starting ({}x{} @ {} fps, source={:?})", w, h, out_fmt.fps_num, args.source
    );

    let running = running_flag();
    // A camera self-paces (its `frame()` blocks at the device rate); only the synthetic
    // source needs us to throttle to a target fps.
    let pace = matches!(args.source, Source::Test);
    let frame_dur = Duration::from_nanos(1_000_000_000u64 * out_fmt.fps_den as u64 / out_fmt.fps_num.max(1) as u64);
    let mut next = Instant::now();
    let mut out_buf: Vec<u8> = Vec::new();
    let mut n: u64 = 0;

    while running.load(Ordering::SeqCst) {
        let Some(frame) = source.next_frame()? else { break };

        let pts = frame.pts.as_nanos();
        if let Some((ctx, chain)) = gpu.as_mut() {
            let final_out = chain.output_is_final();
            let uniforms: Vec<Vec<u8>> = fx.iter().map(|e| e.uniform_bytes()).collect();
            let refs: Vec<&[u8]> = uniforms.iter().map(|v| v.as_slice()).collect();
            let processed = chain.process(ctx, &frame.data, &refs);
            if final_out {
                // Already NV12 — packed on the GPU, no CPU conversion.
                sink.submit(processed, pts)?;
            } else {
                ovc_core::convert::rgba_to(out_fmt.format, processed, w, h, &mut out_buf);
                sink.submit(&out_buf, pts)?;
            }
        } else {
            // CPU path: effects in place on RGBA, then convert at the sink boundary.
            let mut buf = frame.data.to_vec();
            for e in &fx {
                e.apply_cpu(&mut buf, w, h);
            }
            ovc_core::convert::rgba_to(out_fmt.format, &buf, w, h, &mut out_buf);
            sink.submit(&out_buf, pts)?;
        }

        n += 1;
        if let Some(max) = args.frames {
            if n >= max {
                break;
            }
        }

        // Pace to target fps without accumulating drift (synthetic source only).
        if pace {
            next += frame_dur;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
    }

    sink.finish();
    source.stop();
    tracing::info!("stopped after {n} frames");
    Ok(())
}

// ---- sinks ----

trait Sink {
    fn submit(&mut self, bytes: &[u8], pts_ns: i64) -> anyhow::Result<()>;
    fn finish(&mut self) {}
}

/// Counts frames and prints measured throughput once a second.
struct StatsSink {
    start: Instant,
    last: Instant,
    count: u64,
    last_count: u64,
}

impl StatsSink {
    fn new() -> Self {
        let now = Instant::now();
        Self { start: now, last: now, count: 0, last_count: 0 }
    }
}

impl Sink for StatsSink {
    fn submit(&mut self, bytes: &[u8], _pts_ns: i64) -> anyhow::Result<()> {
        self.count += 1;
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        if dt >= 1.0 {
            let fps = (self.count - self.last_count) as f64 / dt;
            println!("frames={} inst_fps={:.1} bytes/frame={}", self.count, fps, bytes.len());
            self.last = now;
            self.last_count = self.count;
        }
        Ok(())
    }
    fn finish(&mut self) {
        let secs = self.start.elapsed().as_secs_f64().max(1e-9);
        println!("total frames={} avg_fps={:.2}", self.count, self.count as f64 / secs);
    }
}

/// Appends raw frames to a file. Play back with e.g.
/// `ffplay -f rawvideo -pixel_format nv12 -video_size 1280x720 out.nv12`.
struct FileSink {
    file: File,
}

impl FileSink {
    fn new(path: &str) -> anyhow::Result<Self> {
        Ok(Self { file: File::create(path)? })
    }
}

impl Sink for FileSink {
    fn submit(&mut self, bytes: &[u8], _pts_ns: i64) -> anyhow::Result<()> {
        self.file.write_all(bytes)?;
        Ok(())
    }
    fn finish(&mut self) {
        let _ = self.file.flush();
    }
}

/// Publishes frames into the OVC1 shared-memory ring.
struct RingSink {
    producer: Producer,
}

impl RingSink {
    fn new(name: &str, fmt: VideoFormat, slots: u32) -> anyhow::Result<Self> {
        // Round up to 64 bytes so every slot (and its SlotHeader.seq) stays aligned.
        let slot_size = ((fmt.frame_size() + 63) & !63) as u32;
        let len = mapping_size(slot_size, slots);
        let region = Region::shared(name, len, true)?;
        let cfg = RingConfig {
            width: fmt.width,
            height: fmt.height,
            fourcc: fmt.format.fourcc().as_u32(),
            stride: fmt.format.stride(fmt.width),
            slot_size,
            slot_count: slots,
            fps_num: fmt.fps_num,
            fps_den: fmt.fps_den,
        };
        let producer = Producer::create(region, cfg)?;
        tracing::info!("ring '{name}' ready ({} bytes, {} slots)", len, slots);
        Ok(Self { producer })
    }
}

impl Sink for RingSink {
    fn submit(&mut self, bytes: &[u8], pts_ns: i64) -> anyhow::Result<()> {
        self.producer.publish(bytes, pts_ns)?;
        Ok(())
    }
}

// ---- consumer ----

fn ipc_recv(name: &str, out: Option<&str>) -> anyhow::Result<()> {
    let running = running_flag();

    // Wait until the producer has created and initialized the ring.
    tracing::info!("waiting for ring '{name}' ...");
    let mut consumer = loop {
        if !running.load(Ordering::SeqCst) {
            return Ok(());
        }
        match Region::shared(name, 0, false).and_then(Consumer::open) {
            Ok(c) => break c,
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    let (w, h, fourcc) = consumer.format();
    println!(
        "connected: {}x{} fourcc={}",
        w,
        h,
        String::from_utf8_lossy(&fourcc.to_le_bytes())
    );

    let mut file = out.map(File::create).transpose()?;
    let mut buf = Vec::new();
    let mut n: u64 = 0;
    let start = Instant::now();
    let mut last = start;
    let mut last_n = 0u64;

    while running.load(Ordering::SeqCst) {
        match consumer.try_recv(&mut buf) {
            Some(_pts) => {
                n += 1;
                if let Some(f) = &mut file {
                    f.write_all(&buf)?;
                }
                let now = Instant::now();
                let dt = now.duration_since(last).as_secs_f64();
                if dt >= 1.0 {
                    println!("recv frames={} inst_fps={:.1}", n, (n - last_n) as f64 / dt);
                    last = now;
                    last_n = n;
                }
            }
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }

    if let Some(f) = &mut file {
        let _ = f.flush();
    }
    let secs = start.elapsed().as_secs_f64().max(1e-9);
    println!("received {n} frames, avg_fps={:.2}", n as f64 / secs);
    Ok(())
}
