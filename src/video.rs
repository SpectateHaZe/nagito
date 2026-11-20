//! Frame hand-off between a source thread and the GPU, plus the upscale + sharpen renderer.

use eframe::egui_wgpu::{self, wgpu};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    /// With frame generation: when the real frame should replace the generated one on screen.
    pub real_at: Mutex<Option<Instant>>,
}

#[derive(Default, Clone)]
pub struct Stats {
    pub source_fps: f32,
    /// Distinct pictures shown per second, generated ones included.
    pub shown_fps: f32,
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
    pub fn has_pending(&self) -> bool {
        self.latest.lock().unwrap().is_some()
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
    /// Draw a motion-compensated frame between each pair of source frames.
    pub frame_gen: bool,
}

/// Per-source-size textures: two frames to interpolate between, plus motion estimation buffers.
struct Chain {
    size: [u32; 2],
    frames: [wgpu::Texture; 2],
    luma: [wgpu::Texture; 2],
    luma_half: [wgpu::Texture; 2],
    mv: wgpu::Texture,
    mv_smooth: wgpu::Texture,
    mv_fine: wgpu::Texture,
    mv_fine_smooth: wgpu::Texture,
    interp: wgpu::Texture,
    luma_bg: [wgpu::BindGroup; 2],
    me_bg: [wgpu::BindGroup; 2],
    filter_bg: wgpu::BindGroup,
    me_fine_bg: [wgpu::BindGroup; 2],
    filter_fine_bg: wgpu::BindGroup,
    interp_bg: [wgpu::BindGroup; 2],
    /// Upscale input: frames[0], frames[1] or interp.
    upscale_bg: [wgpu::BindGroup; 3],
}

struct Gpu {
    target_format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    upscale_pipe: wgpu::RenderPipeline,
    rcas_pipe: wgpu::RenderPipeline,
    nv12_pipe: wgpu::RenderPipeline,
    luma_pipe: wgpu::RenderPipeline,
    luma_half_pipe: wgpu::RenderPipeline,
    me_fine_pipe: wgpu::RenderPipeline,
    me_pipe: wgpu::RenderPipeline,
    filter_pipe: wgpu::RenderPipeline,
    interp_pipe: wgpu::RenderPipeline,
    chain: Option<Chain>,
    planes: Option<(wgpu::Texture, wgpu::Texture, [u32; 2])>,
    nv12_bg: Option<wgpu::BindGroup>,
    mid: Option<(wgpu::Texture, [u32; 2])>,
    rcas_bg: Option<wgpu::BindGroup>,
    cur: usize,
    has_prev: bool,
    /// What is on screen: 0 or 1 = that real frame, 2 = the generated one.
    show: usize,
    shown: (u32, Instant),
}

const MID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const LUMA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const MV_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;
/// Motion search runs on a quarter-size picture in 8x8 blocks.
const LUMA_DIV: u32 = 4;
const BLOCK: u32 = 8;

pub fn install(rs: &egui_wgpu::RenderState) {
    let dev = &rs.device;
    let shader = dev.create_shader_module(wgpu::include_wgsl!("video.wgsl"));
    let tex = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("video"),
        entries: &[
            tex(0),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
            tex(3),
            tex(4),
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
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
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
        luma_pipe: pipe("fs_luma", LUMA_FORMAT),
        luma_half_pipe: pipe("fs_luma_half", LUMA_FORMAT),
        me_fine_pipe: pipe("fs_me_fine", MV_FORMAT),
        me_pipe: pipe("fs_me", MV_FORMAT),
        filter_pipe: pipe("fs_mv_filter", MV_FORMAT),
        interp_pipe: pipe("fs_interp", MID_FORMAT),
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
        chain: None,
        planes: None,
        nv12_bg: None,
        mid: None,
        rcas_bg: None,
        cur: 0,
        has_prev: false,
        show: 0,
        shown: (0, Instant::now()),
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
        size: wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: usage | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// Binds up to three textures at slots 0, 3 and 4; unused slots repeat the first.
fn bind_group(dev: &wgpu::Device, g: &Gpu, t: &[&wgpu::Texture]) -> wgpu::BindGroup {
    let view = |i: usize| t.get(i).unwrap_or(&t[0]).create_view(&Default::default());
    let (a, b, c) = (view(0), view(1), view(2));
    dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &g.layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&a) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&g.sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: g.params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&b) },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&c) },
        ],
    })
}

fn make_chain(dev: &wgpu::Device, g: &Gpu, size: [u32; 2]) -> Chain {
    let rt = wgpu::TextureUsages::RENDER_ATTACHMENT;
    let frames = [0, 1].map(|_| texture(dev, size, MID_FORMAT, rt | wgpu::TextureUsages::COPY_DST));
    let lsize = [(size[0] / LUMA_DIV).max(1), (size[1] / LUMA_DIV).max(1)];
    let luma = [0, 1].map(|_| texture(dev, lsize, LUMA_FORMAT, rt));
    let bsize = [lsize[0].div_ceil(BLOCK), lsize[1].div_ceil(BLOCK)];
    let mv = texture(dev, bsize, MV_FORMAT, rt);
    let mv_smooth = texture(dev, bsize, MV_FORMAT, rt);
    // Second stage: half-size picture, 8x8 blocks = 16x16 real pixels, seeded by the coarse vectors.
    let hsize = [(size[0] / 2).max(1), (size[1] / 2).max(1)];
    let luma_half = [0, 1].map(|_| texture(dev, hsize, LUMA_FORMAT, rt));
    let fsize = [hsize[0].div_ceil(BLOCK), hsize[1].div_ceil(BLOCK)];
    let mv_fine = texture(dev, fsize, MV_FORMAT, rt);
    let mv_fine_smooth = texture(dev, fsize, MV_FORMAT, rt);
    let interp = texture(dev, size, MID_FORMAT, rt);
    Chain {
        luma_bg: [0, 1].map(|i| bind_group(dev, g, &[&frames[i]])),
        me_bg: [0, 1].map(|cur| bind_group(dev, g, &[&luma[1 - cur], &luma[cur]])),
        filter_bg: bind_group(dev, g, &[&mv]),
        me_fine_bg: [0, 1].map(|cur| bind_group(dev, g, &[&luma_half[1 - cur], &luma_half[cur], &mv_smooth])),
        filter_fine_bg: bind_group(dev, g, &[&mv_fine]),
        interp_bg: [0, 1].map(|cur| bind_group(dev, g, &[&frames[1 - cur], &frames[cur], &mv_fine_smooth])),
        upscale_bg: [bind_group(dev, g, &[&frames[0]]), bind_group(dev, g, &[&frames[1]]), bind_group(dev, g, &[&interp])],
        size,
        frames,
        luma,
        luma_half,
        mv,
        mv_smooth,
        mv_fine,
        mv_fine_smooth,
        interp,
    }
}

fn pass(enc: &mut wgpu::CommandEncoder, target: &wgpu::Texture, pipe: &wgpu::RenderPipeline, bg: &wgpu::BindGroup) {
    let view = target.create_view(&Default::default());
    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipe);
    pass.set_bind_group(0, bg, &[]);
    pass.draw(0..3, 0..1);
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
        let now = Instant::now();
        let frame = self.slot.take();
        let source_fps = self.slot.stats.lock().unwrap().source_fps;
        let half = Duration::from_secs_f32(0.5 / source_fps.clamp(20.0, 240.0));

        let yuv = match frame.as_ref().map(|f| f.pixels) {
            Some(Pixels::Nv12 { full_range, bt601 }) => Some(full_range as u32 | (bt601 as u32) << 1),
            _ => None,
        };
        let src_size = frame.as_ref().map(|f| [f.width, f.height]).or(g.chain.as_ref().map(|c| c.size));
        let Some(src_size) = src_size else { return vec![] };
        let mut params = [0u8; 32];
        params[0..4].copy_from_slice(&(src_size[0] as f32).to_le_bytes());
        params[4..8].copy_from_slice(&(src_size[1] as f32).to_le_bytes());
        params[8..12].copy_from_slice(&(self.look.scaler as u32).to_le_bytes());
        params[12..16].copy_from_slice(&(g.target_format.is_srgb() as u32).to_le_bytes());
        params[16..20].copy_from_slice(&self.look.sharpness.to_le_bytes());
        params[20..24].copy_from_slice(&yuv.unwrap_or(0).to_le_bytes());
        queue.write_buffer(&g.params, 0, &params);

        let mut changed = frame.is_some();
        if let Some(f) = frame {
            if g.chain.as_ref().map(|c| c.size) != Some(src_size) {
                g.chain = Some(make_chain(dev, g, src_size));
                g.has_prev = false;
            }
            if self.look.frame_gen {
                g.cur ^= 1;
            }
            let cur = g.cur;
            let extent = |w, h| wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
            let layout = |row| wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None };
            match f.pixels {
                Pixels::Rgba => {
                    let target = &g.chain.as_ref().unwrap().frames[cur];
                    queue.write_texture(target.as_image_copy(), &f.data, layout(4 * f.width), extent(f.width, f.height));
                }
                Pixels::Nv12 { .. } => {
                    if g.planes.as_ref().map(|p| p.2) != Some(src_size) {
                        let y = texture(dev, src_size, wgpu::TextureFormat::R8Unorm, wgpu::TextureUsages::COPY_DST);
                        let uv = texture(dev, [src_size[0] / 2, src_size[1] / 2], wgpu::TextureFormat::Rg8Unorm, wgpu::TextureUsages::COPY_DST);
                        g.nv12_bg = Some(bind_group(dev, g, &[&y, &uv]));
                        g.planes = Some((y, uv, src_size));
                    }
                    let (y, uv, _) = g.planes.as_ref().unwrap();
                    let luma = (f.width * f.height) as usize;
                    queue.write_texture(y.as_image_copy(), &f.data[..luma], layout(f.width), extent(f.width, f.height));
                    queue.write_texture(uv.as_image_copy(), &f.data[luma..], layout(f.width), extent(f.width / 2, f.height / 2));
                    pass(enc, &g.chain.as_ref().unwrap().frames[cur], &g.nv12_pipe, g.nv12_bg.as_ref().unwrap());
                }
            }
            g.show = cur;
            let c = g.chain.as_ref().unwrap();
            if self.look.frame_gen {
                pass(enc, &c.luma[cur], &g.luma_pipe, &c.luma_bg[cur]);
                pass(enc, &c.luma_half[cur], &g.luma_half_pipe, &c.luma_bg[cur]);
                if g.has_prev {
                    pass(enc, &c.mv, &g.me_pipe, &c.me_bg[cur]);
                    pass(enc, &c.mv_smooth, &g.filter_pipe, &c.filter_bg);
                    pass(enc, &c.mv_fine, &g.me_fine_pipe, &c.me_fine_bg[cur]);
                    pass(enc, &c.mv_fine_smooth, &g.filter_pipe, &c.filter_fine_bg);
                    pass(enc, &c.interp, &g.interp_pipe, &c.interp_bg[cur]);
                    g.show = 2;
                    *self.slot.real_at.lock().unwrap() = Some(now + half);
                }
            }
            g.has_prev = self.look.frame_gen;
            self.slot.stats.lock().unwrap().frame_age_ms = f.captured_at.elapsed().as_secs_f32() * 1000.0;
        } else if g.show == 2 && self.slot.real_at.lock().unwrap().is_some_and(|t| now + Duration::from_millis(2) >= t) {
            g.show = g.cur; // the real frame's turn
            *self.slot.real_at.lock().unwrap() = None;
            changed = true;
        }
        g.shown.0 += changed as u32;
        if g.shown.1.elapsed() >= Duration::from_secs(1) {
            self.slot.stats.lock().unwrap().shown_fps = g.shown.0 as f32 / g.shown.1.elapsed().as_secs_f32();
            g.shown = (0, Instant::now());
        }

        let out = [self.size_px[0].max(1), self.size_px[1].max(1)];
        if g.mid.as_ref().map(|m| m.1) != Some(out) {
            let mid = texture(dev, out, MID_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
            g.rcas_bg = Some(bind_group(dev, g, &[&mid]));
            g.mid = Some((mid, out));
        }
        let c = g.chain.as_ref().unwrap();
        pass(enc, &g.mid.as_ref().unwrap().0, &g.upscale_pipe, &c.upscale_bg[g.show]);
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
