//! GPU backend: a `wgpu` device plus an [`EffectChain`] that runs fullscreen effect
//! passes on the GPU and optionally packs the result to NV12 in a compute shader.
//!
//! Each frame: upload RGBA8 → tex0, ping-pong through N render passes (each samples the
//! previous texture and writes the next). Then, depending on [`OutputMode`], either copy
//! the final RGBA texture back, or run a compute pass that writes planar NV12 straight
//! into a storage buffer and read that back (1.5 bytes/px instead of 4, and no CPU
//! per-pixel color conversion at all).

use std::borrow::Cow;

use anyhow::anyhow;

/// What `process` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Tightly packed RGBA8 (`w*h*4`). Caller converts to the wire format on the CPU.
    Rgba,
    /// Planar NV12 (`w*h*3/2`), packed on the GPU. Requires `w % 4 == 0`, `h % 2 == 0`.
    Nv12,
}

/// Owns the GPU device/queue and a human-readable adapter description.
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_info: String,
}

impl GpuContext {
    pub fn new() -> anyhow::Result<Self> {
        pollster::block_on(Self::new_async())
    }

    async fn new_async() -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::empty(),
            ..Default::default()
        });
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
        {
            Some(a) => a,
            None => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::default(),
                    force_fallback_adapter: true,
                    compatible_surface: None,
                })
                .await
                .ok_or_else(|| anyhow!("no GPU adapter (not even a software fallback)"))?,
        };
        let info = adapter.get_info();
        let adapter_info = format!("{} [{:?}, {:?}]", info.name, info.backend, info.device_type);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("ovc-gpu"),
                    required_features: wgpu::Features::empty(),
                    required_limits: adapter.limits(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await?;
        Ok(Self { device, queue, adapter_info })
    }
}

const TEX_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

struct Pass {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    dst: usize,
}

/// NV12 compute-pack resources (present only in [`OutputMode::Nv12`]).
struct Nv12 {
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    storage: wgpu::Buffer,
    readback: wgpu::Buffer,
    workgroups: u32,
}

pub struct EffectChain {
    width: u32,
    height: u32,
    output: OutputMode,
    tex: [wgpu::Texture; 2],
    passes: Vec<Pass>,
    final_idx: usize,
    // RGBA readback path:
    readback: wgpu::Buffer,
    padded_bpr: u32,
    nv12: Option<Nv12>,
    out: Vec<u8>,
}

impl EffectChain {
    pub fn new(
        ctx: &GpuContext,
        width: u32,
        height: u32,
        shaders: &[&str],
        uniform_sizes: &[usize],
        output: OutputMode,
    ) -> anyhow::Result<Self> {
        assert_eq!(shaders.len(), uniform_sizes.len());
        assert!(!shaders.is_empty(), "effect chain needs at least one pass");
        if output == OutputMode::Nv12 && (width % 4 != 0 || height % 2 != 0) {
            return Err(anyhow!("NV12 output needs width % 4 == 0 and height % 2 == 0"));
        }
        let dev = &ctx.device;
        // Capture shader/pipeline validation errors as a Result instead of a hard abort.
        dev.push_error_scope(wgpu::ErrorFilter::Validation);

        let make_tex = |label| {
            dev.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TEX_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let tex = [make_tex("ovc-tex0"), make_tex("ovc-tex1")];
        let views = [
            tex[0].create_view(&wgpu::TextureViewDescriptor::default()),
            tex[1].create_view(&wgpu::TextureViewDescriptor::default()),
        ];
        let sampler = dev.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ovc-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let bgl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ovc-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ovc-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let mut passes = Vec::with_capacity(shaders.len());
        for (i, (src, &usz)) in shaders.iter().zip(uniform_sizes).enumerate() {
            let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ovc-fx"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(src)),
            });
            let pipeline = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ovc-fx-pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: TEX_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });
            let uniform = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ovc-uniform"),
                size: usz.max(16) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ovc-bg"),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&views[i % 2]) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
                ],
            });
            passes.push(Pass { pipeline, uniform, bind_group, dst: (i + 1) % 2 });
        }
        let final_idx = passes.len() % 2;

        let padded_bpr = (width * 4).div_ceil(256) * 256;
        let readback = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ovc-readback"),
            size: (padded_bpr * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let (nv12, out_len) = if output == OutputMode::Nv12 {
            let nv12_size = (width * height * 3 / 2) as u64;
            let y_words = width * height / 4;
            let uv_words = width * height / 8;

            let cs = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ovc-nv12"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("../../../shaders/nv12_pack.wgsl"))),
            });
            let cbgl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("ovc-nv12-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
            let cpl = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("ovc-nv12-pl"),
                bind_group_layouts: &[&cbgl],
                push_constant_ranges: &[],
            });
            let pipeline = dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("ovc-nv12-pipeline"),
                layout: Some(&cpl),
                module: &cs,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            let storage = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ovc-nv12-storage"),
                size: nv12_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let nv12_readback = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ovc-nv12-readback"),
                size: nv12_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let dims = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ovc-nv12-dims"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            ctx.queue.write_buffer(&dims, 0, bytemuck::cast_slice(&[width, height, y_words, uv_words]));

            let bind_group = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ovc-nv12-bg"),
                layout: &cbgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&views[final_idx]) },
                    wgpu::BindGroupEntry { binding: 1, resource: storage.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: dims.as_entire_binding() },
                ],
            });
            let workgroups = (y_words + uv_words).div_ceil(64);
            (
                Some(Nv12 { pipeline, bind_group, storage, readback: nv12_readback, workgroups }),
                nv12_size as usize,
            )
        } else {
            (None, (width * height * 4) as usize)
        };

        if let Some(err) = pollster::block_on(dev.pop_error_scope()) {
            return Err(anyhow!("GPU shader/pipeline validation failed: {err}"));
        }

        Ok(Self {
            width,
            height,
            output,
            tex,
            passes,
            final_idx,
            readback,
            padded_bpr,
            nv12,
            out: vec![0u8; out_len],
        })
    }

    /// True when `process` already returns frames in the final wire format (NV12), so the
    /// caller must not run a CPU conversion afterwards.
    pub fn output_is_final(&self) -> bool {
        self.output == OutputMode::Nv12
    }

    pub fn process(&mut self, ctx: &GpuContext, rgba_in: &[u8], uniforms: &[&[u8]]) -> &[u8] {
        let (w, h) = (self.width, self.height);
        for (p, u) in self.passes.iter().zip(uniforms) {
            if !u.is_empty() {
                ctx.queue.write_buffer(&p.uniform, 0, u);
            }
        }
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.tex[0],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba_in,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );

        let views = [
            self.tex[0].create_view(&wgpu::TextureViewDescriptor::default()),
            self.tex[1].create_view(&wgpu::TextureViewDescriptor::default()),
        ];
        let mut enc = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ovc-frame") });
        for p in &self.passes {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ovc-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &views[p.dst],
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_pipeline(&p.pipeline);
            rp.set_bind_group(0, &p.bind_group, &[]);
            rp.draw(0..3, 0..1);
        }

        if let Some(nv) = &self.nv12 {
            // Pack RGBA (final effect texture) → NV12 storage buffer on the GPU.
            {
                let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("ovc-nv12"),
                    timestamp_writes: None,
                });
                cp.set_pipeline(&nv.pipeline);
                cp.set_bind_group(0, &nv.bind_group, &[]);
                cp.dispatch_workgroups(nv.workgroups, 1, 1);
            }
            enc.copy_buffer_to_buffer(&nv.storage, 0, &nv.readback, 0, self.out.len() as u64);
            ctx.queue.submit(std::iter::once(enc.finish()));
            map_and_copy(&ctx.device, &nv.readback, &mut self.out);
        } else {
            // RGBA readback (row-padded) from the final texture.
            enc.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &self.tex[self.final_idx],
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &self.readback,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(self.padded_bpr),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            ctx.queue.submit(std::iter::once(enc.finish()));
            let slice = self.readback.slice(..);
            map_block(&ctx.device, &slice);
            {
                let data = slice.get_mapped_range();
                let row = (w * 4) as usize;
                let pbr = self.padded_bpr as usize;
                for y in 0..h as usize {
                    self.out[y * row..(y + 1) * row].copy_from_slice(&data[y * pbr..y * pbr + row]);
                }
            }
            self.readback.unmap();
        }
        &self.out
    }
}

/// Map a buffer fully (blocking) and copy its bytes into `dst` (same length).
fn map_and_copy(device: &wgpu::Device, buffer: &wgpu::Buffer, dst: &mut [u8]) {
    let slice = buffer.slice(..);
    map_block(device, &slice);
    dst.copy_from_slice(&slice.get_mapped_range());
    buffer.unmap();
}

/// Block until a buffer slice is mapped for reading.
fn map_block(device: &wgpu::Device, slice: &wgpu::BufferSlice<'_>) {
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv().expect("map callback").expect("map ok");
}
