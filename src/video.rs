//! Frame hand-off between a source thread and the GPU, plus the upscale + sharpen renderer.

use eframe::egui_wgpu::{self, wgpu};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq)]
pub enum Pixels {
    Rgba,
    /// Luma plane then interleaved chroma at half size; converted to RGB on the GPU.
    Nv12 { full_range: bool, bt601: bool },
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
    pub data: Vec<u8>,
    pub captured_at: Instant,
}

/// Latest-frame mailbox: a new frame replaces an unshown one, so we never queue up latency.
#[derive(Default)]
pub struct FrameSlot {
    latest: Mutex<Option<Frame>>,
    pub stats: Mutex<Stats>,
}

#[derive(Default, Clone)]
pub struct Stats {
    pub source_fps: f32,
    pub decode_ms: f32,
    pub frame_age_ms: f32,
    pub dropped: u64,
    pub width: u32,
    pub height: u32,
    pub audio: Option<String>,
    pub controller: Option<String>,
    pub error: Option<String>,
    pub rtt_ms: f32,
    pub loss: f32,
    pub bitrate_mbps: f32,
    /// Some(muted) once the DualSense mic button was used.
    pub mic: Option<bool>,
    /// Some(previous_was_wrong) while the PS5 waits for its login passcode.
    pub pin: Option<bool>,
}

impl FrameSlot {
    pub fn push(&self, f: Frame) {
        if self.latest.lock().unwrap().replace(f).is_some() {
            self.stats.lock().unwrap().dropped += 1;
        }
    }
    fn take(&self) -> Option<Frame> {
        self.latest.lock().unwrap().take()
    }
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Scaler {
    Bilinear,
    Bicubic,
    /// AMD FSR 1 EASU: edge-aware, keeps lines crisper than bicubic when scaling up.
    Fsr,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Look {
    pub scaler: Scaler,
    pub sharpness: f32,
}

struct Gpu {
    target_format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    upscale_pipe: wgpu::RenderPipeline,
    rcas_pipe: wgpu::RenderPipeline,
    nv12_pipe: wgpu::RenderPipeline,
    src: Option<(wgpu::Texture, [u32; 2])>,
    planes: Option<(wgpu::Texture, wgpu::Texture, [u32; 2])>,
    nv12_bg: Option<wgpu::BindGroup>,
    mid: Option<(wgpu::Texture, [u32; 2])>,
    upscale_bg: Option<wgpu::BindGroup>,
    rcas_bg: Option<wgpu::BindGroup>,
}

const MID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub fn install(rs: &egui_wgpu::RenderState) {
    let dev = &rs.device;
    let shader = dev.create_shader_module(wgpu::include_wgsl!("video.wgsl"));
    let tex_entry = |binding, ty| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty,
        count: None,
    };
    let layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("video"),
        entries: &[
            tex_entry(
                0,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            ),
            tex_entry(1, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            tex_entry(
                2,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            ),
            // chroma plane for the NV12 pass; the other passes bind their input here again
            tex_entry(
                3,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            ),
        ],
    });
    let pl = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("video"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipe = |entry: &str, format: wgpu::TextureFormat| {
        dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(entry),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        })
    };
    let gpu = Gpu {
        target_format: rs.target_format,
        upscale_pipe: pipe("fs_upscale", MID_FORMAT),
        rcas_pipe: pipe("fs_rcas", rs.target_format),
        nv12_pipe: pipe("fs_nv12", MID_FORMAT),
        layout,
        sampler: dev.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        }),
        params: dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
        src: None,
        planes: None,
        nv12_bg: None,
        mid: None,
        upscale_bg: None,
        rcas_bg: None,
    };
    rs.renderer.write().callback_resources.insert(gpu);
}

/// Paint callback: draws the latest frame into `size_px` pixels of the screen.
pub struct VideoPaint {
    pub slot: Arc<FrameSlot>,
    pub size_px: [u32; 2],
    pub look: Look,
}

fn texture(dev: &wgpu::Device, size: [u32; 2], format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> wgpu::Texture {
    dev.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: usage | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn bind_group(dev: &wgpu::Device, g: &Gpu, a: &wgpu::Texture, b: &wgpu::Texture) -> wgpu::BindGroup {
    dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &g.layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&a.create_view(&Default::default())) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&g.sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: g.params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&b.create_view(&Default::default())) },
        ],
    })
}

impl egui_wgpu::CallbackTrait for VideoPaint {
    fn prepare(
        &self,
        dev: &wgpu::Device,
        queue: &wgpu::Queue,
        _sd: &egui_wgpu::ScreenDescriptor,
        enc: &mut wgpu::CommandEncoder,
        res: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let g: &mut Gpu = res.get_mut().unwrap();
        let mut rebind = false;

        let mut yuv = None;
        if let Some(f) = self.slot.take() {
            let size = [f.width, f.height];
            if g.src.as_ref().map(|s| s.1) != Some(size) {
                let usage = wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT;
                g.src = Some((texture(dev, size, MID_FORMAT, usage), size));
                rebind = true;
            }
            let extent = |w, h| wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
            let layout = |row| wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None };
            match f.pixels {
                Pixels::Rgba => queue.write_texture(g.src.as_ref().unwrap().0.as_image_copy(), &f.data, layout(4 * f.width), extent(f.width, f.height)),
                Pixels::Nv12 { full_range, bt601 } => {
                    if g.planes.as_ref().map(|p| p.2) != Some(size) {
                        let y = texture(dev, size, wgpu::TextureFormat::R8Unorm, wgpu::TextureUsages::COPY_DST);
                        let uv = texture(dev, [size[0] / 2, size[1] / 2], wgpu::TextureFormat::Rg8Unorm, wgpu::TextureUsages::COPY_DST);
                        g.nv12_bg = Some(bind_group(dev, g, &y, &uv));
                        g.planes = Some((y, uv, size));
                    }
                    let (y, uv, _) = g.planes.as_ref().unwrap();
                    let luma = (f.width * f.height) as usize;
                    queue.write_texture(y.as_image_copy(), &f.data[..luma], layout(f.width), extent(f.width, f.height));
                    queue.write_texture(uv.as_image_copy(), &f.data[luma..], layout(f.width), extent(f.width / 2, f.height / 2));
                    yuv = Some(full_range as u32 | (bt601 as u32) << 1);
                }
            }
            self.slot.stats.lock().unwrap().frame_age_ms = f.captured_at.elapsed().as_secs_f32() * 1000.0;
        }
        let Some((_, src_size)) = g.src.as_ref() else { return vec![] };
        let src_size = *src_size;

        let out = [self.size_px[0].max(1), self.size_px[1].max(1)];
        if g.mid.as_ref().map(|m| m.1) != Some(out) {
            g.mid = Some((texture(dev, out, MID_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT), out));
            rebind = true;
        }
        if rebind {
            let (src, mid) = (&g.src.as_ref().unwrap().0, &g.mid.as_ref().unwrap().0);
            g.upscale_bg = Some(bind_group(dev, g, src, src));
            g.rcas_bg = Some(bind_group(dev, g, mid, mid));
        }

        let mut params = [0u8; 32];
        params[0..4].copy_from_slice(&(src_size[0] as f32).to_le_bytes());
        params[4..8].copy_from_slice(&(src_size[1] as f32).to_le_bytes());
        params[8..12].copy_from_slice(&(self.look.scaler as u32).to_le_bytes());
        params[12..16].copy_from_slice(&(g.target_format.is_srgb() as u32).to_le_bytes());
        params[16..20].copy_from_slice(&self.look.sharpness.to_le_bytes());
        params[20..24].copy_from_slice(&yuv.unwrap_or(0).to_le_bytes());
        queue.write_buffer(&g.params, 0, &params);

        if yuv.is_some() {
            let view = g.src.as_ref().unwrap().0.create_view(&Default::default());
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nv12"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&g.nv12_pipe);
            pass.set_bind_group(0, g.nv12_bg.as_ref(), &[]);
            pass.draw(0..3, 0..1);
        }

        let view = g.mid.as_ref().unwrap().0.create_view(&Default::default());
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("upscale"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&g.upscale_pipe);
        pass.set_bind_group(0, g.upscale_bg.as_ref(), &[]);
        pass.draw(0..3, 0..1);
        vec![]
    }

    fn paint(&self, _info: eframe::egui::PaintCallbackInfo, rp: &mut wgpu::RenderPass<'static>, res: &egui_wgpu::CallbackResources) {
        let g: &Gpu = res.get().unwrap();
        if let Some(bg) = &g.rcas_bg {
            rp.set_pipeline(&g.rcas_pipe);
            rp.set_bind_group(0, bg, &[]);
            rp.draw(0..3, 0..1);
        }
    }
}
