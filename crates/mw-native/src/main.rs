use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use mw_core::{GridSpec, decode_mwsc_gzip_file};
use serde_json::Value;
use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes, WindowId},
};

const DEFAULT_SCENARIO: &str = "../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz";
const ROW_ALIGNMENT: usize = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewUniform {
    viewport: [f32; 2],
    center: [f32; 2],
    pixels_per_world: f32,
    palette_len: u32,
    grid_size: [u32; 2],
}

struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    view_buffer: wgpu::Buffer,
}

struct App {
    scenario_path: PathBuf,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    ownership: Vec<u16>,
    metadata: Value,
    grid_width: u32,
    grid_height: u32,
    grid_res: f32,
    center: [f32; 2],
    zoom: f32,
    cursor: PhysicalPosition<f64>,
    dragging: bool,
    last_drag: PhysicalPosition<f64>,
    frame_count: u64,
    presented_frames: u64,
    smoke_frames: Option<u64>,
    fps_epoch: Instant,
    fps: f64,
}

impl App {
    fn new(scenario_path: PathBuf, smoke_frames: Option<u64>) -> Self {
        Self {
            scenario_path,
            window: None,
            gpu: None,
            ownership: Vec::new(),
            metadata: Value::Null,
            grid_width: 0,
            grid_height: 0,
            grid_res: 0.15,
            center: [1.0, 0.5],
            zoom: 1.0,
            cursor: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            last_drag: PhysicalPosition::new(0.0, 0.0),
            frame_count: 0,
            presented_frames: 0,
            smoke_frames,
            fps_epoch: Instant::now(),
            fps: 0.0,
        }
    }

    fn initialize(&mut self, window: Arc<Window>) -> Result<()> {
        let decode_started = Instant::now();
        let target = GridSpec::world(0.15).context("invalid 0.15 degree target grid")?;
        let decoded = decode_mwsc_gzip_file(&self.scenario_path, Some(target))
            .with_context(|| format!("failed to decode {}", self.scenario_path.display()))?;
        log::info!(
            "decoded {} entries into {}x{} in {:.1} ms",
            decoded.entry_count,
            decoded.target.width,
            decoded.target.height,
            decode_started.elapsed().as_secs_f64() * 1_000.0
        );
        self.grid_width =
            u32::try_from(decoded.target.width).context("scenario width exceeds GPU limits")?;
        self.grid_height =
            u32::try_from(decoded.target.height).context("scenario height exceeds GPU limits")?;
        self.grid_res = decoded.target.grid_res as f32;
        self.ownership = decoded.world_control;
        self.metadata = decoded.metadata;

        anyhow::ensure!(
            self.ownership.len() == self.grid_width as usize * self.grid_height as usize,
            "ownership grid has {} cells, expected {}x{}",
            self.ownership.len(),
            self.grid_width,
            self.grid_height
        );

        let palette = build_palette(&self.metadata, &self.ownership);
        let size = window.inner_size();
        self.zoom = reset_zoom(size);

        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone())?;
        let adapter = futures_lite::future::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            },
        ))?;
        let (device, queue) =
            futures_lite::future::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("mw-native device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            }))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::AutoVsync) {
            wgpu::PresentMode::AutoVsync
        } else {
            caps.present_modes[0]
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let ownership_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ownership R16Uint"),
            size: wgpu::Extent3d {
                width: self.grid_width,
                height: self.grid_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let ownership_view = ownership_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let unpadded_bpr = self.grid_width as usize * 2;
        let padded_bpr = unpadded_bpr.div_ceil(ROW_ALIGNMENT) * ROW_ALIGNMENT;
        let mut padded = vec![0_u8; padded_bpr * self.grid_height as usize];
        for (row, source) in self
            .ownership
            .chunks_exact(self.grid_width as usize)
            .enumerate()
        {
            let bytes = bytemuck::cast_slice(source);
            padded[row * padded_bpr..row * padded_bpr + unpadded_bpr].copy_from_slice(bytes);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &ownership_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr as u32),
                rows_per_image: Some(self.grid_height),
            },
            ownership_texture.size(),
        );

        let view_uniform = self.view_uniform(size, palette.len() as u32);
        let view_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("map view uniform"),
            contents: bytemuck::bytes_of(&view_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let palette_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("country palette"),
            contents: bytemuck::cast_slice(&palette),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("map.wgsl"));
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("map bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("map bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&ownership_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: view_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: palette_buffer.as_entire_binding(),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("map pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("map pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        self.gpu = Some(GpuState {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            view_buffer,
        });
        self.window = Some(window);
        Ok(())
    }

    fn view_uniform(&self, size: PhysicalSize<u32>, palette_len: u32) -> ViewUniform {
        ViewUniform {
            viewport: [size.width.max(1) as f32, size.height.max(1) as f32],
            center: self.center,
            pixels_per_world: self.zoom,
            palette_len,
            grid_size: [self.grid_width, self.grid_height],
        }
    }

    fn world_at(&self, point: PhysicalPosition<f64>, size: PhysicalSize<u32>) -> [f64; 2] {
        [
            self.center[0] as f64 + (point.x - size.width as f64 * 0.5) / self.zoom as f64,
            self.center[1] as f64 + (point.y - size.height as f64 * 0.5) / self.zoom as f64,
        ]
    }

    fn zoom_at_cursor(&mut self, delta: f32) {
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
        let before = self.world_at(self.cursor, size);
        let minimum_zoom = reset_zoom(size) * 0.75;
        self.zoom = (self.zoom * (delta * 0.16).exp()).clamp(minimum_zoom, 100_000.0);
        let after = self.world_at(self.cursor, size);
        self.center[0] += (before[0] - after[0]) as f32;
        self.center[1] += (before[1] - after[1]) as f32;
    }

    fn country_at_cursor(&self) -> Option<(u16, f64, f64)> {
        let window = self.window.as_ref()?;
        let world = self.world_at(self.cursor, window.inner_size());
        if !(0.0..2.0).contains(&world[0]) || !(0.0..1.0).contains(&world[1]) {
            return None;
        }
        let (x, y) = world_to_cell(world, self.grid_width, self.grid_height)?;
        let id = *self.ownership.get(y * self.grid_width as usize + x)?;
        let lng = world[0] * 180.0 - 180.0;
        let lat = 90.0 - world[1] * 180.0;
        Some((id, lat, lng))
    }

    fn country_name(&self, id: u16) -> &str {
        self.metadata
            .get("metadata")
            .and_then(Value::as_array)
            .and_then(|countries| {
                countries.iter().find(|country| {
                    country.get("id").and_then(Value::as_u64) == Some(u64::from(id))
                })
            })
            .and_then(|country| country.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(if id == 0 { "Ocean" } else { "Unknown" })
    }

    fn render(&mut self) {
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
        let palette_len = self.ownership.iter().copied().max().unwrap_or(0) as u32 + 1;
        let uniform = self.view_uniform(size, palette_len);
        let Some(gpu) = &mut self.gpu else { return };
        gpu.queue
            .write_buffer(&gpu.view_buffer, 0, bytemuck::bytes_of(&uniform));
        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                log::debug!("suboptimal surface frame");
                frame
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                log::error!("render surface lost");
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Validation => {
                log::error!("surface validation error");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("map frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("map pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.008,
                            g: 0.012,
                            b: 0.02,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, &gpu.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit(Some(encoder.finish()));
        window.pre_present_notify();
        frame.present();

        self.frame_count += 1;
        self.presented_frames += 1;
        let elapsed = self.fps_epoch.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.frame_count as f64 / elapsed.as_secs_f64();
            self.frame_count = 0;
            self.fps_epoch = Instant::now();
            window.set_title(&format!(
                "Modern Wars Native | {:.0} FPS | {}x{} @ {:.2}° | zoom {:.1}",
                self.fps, self.grid_width, self.grid_height, self.grid_res, self.zoom
            ));
        }
        window.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = WindowAttributes::default()
            .with_title("Modern Wars Native — loading")
            .with_inner_size(PhysicalSize::new(1280, 720));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                log::error!("window creation failed: {error}");
                event_loop.exit();
                return;
            }
        };
        if let Err(error) = self.initialize(window) {
            log::error!("initialization failed: {error:#}");
            event_loop.exit();
        } else if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) => event_loop.exit(),
                    PhysicalKey::Code(KeyCode::KeyR) => {
                        self.center = [1.0, 0.5];
                        self.zoom = reset_zoom(window.inner_size());
                        window.request_redraw();
                    }
                    _ => {}
                }
            }
            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.config.width = size.width;
                    gpu.config.height = size.height;
                    gpu.surface.configure(&gpu.device, &gpu.config);
                }
                window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                if self.dragging {
                    self.center[0] -= (position.x - self.last_drag.x) as f32 / self.zoom;
                    self.center[1] -= (position.y - self.last_drag.y) as f32 / self.zoom;
                    window.request_redraw();
                }
                self.last_drag = position;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if state == ElementState::Pressed {
                    self.dragging = true;
                    self.last_drag = self.cursor;
                } else {
                    self.dragging = false;
                    if let Some((id, lat, lng)) = self.country_at_cursor() {
                        println!(
                            "country={id} name={:?} lat={lat:.4} lng={lng:.4}",
                            self.country_name(id)
                        );
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32 / 80.0).clamp(-4.0, 4.0),
                };
                self.zoom_at_cursor(amount);
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                self.render();
                if self
                    .smoke_frames
                    .is_some_and(|target| self.presented_frames >= target)
                {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

fn reset_zoom(size: PhysicalSize<u32>) -> f32 {
    (size.width as f32 * 0.5).min(size.height as f32).max(1.0)
}

fn world_to_cell(world: [f64; 2], width: u32, height: u32) -> Option<(usize, usize)> {
    if width == 0
        || height == 0
        || !(0.0..2.0).contains(&world[0])
        || !(0.0..1.0).contains(&world[1])
    {
        return None;
    }
    let x = (world[0] * 0.5 * f64::from(width)).floor() as usize;
    let y = ((1.0 - world[1]) * f64::from(height))
        .floor()
        .min(f64::from(height.saturating_sub(1))) as usize;
    Some((x, y))
}

fn build_palette(metadata: &Value, ownership: &[u16]) -> Vec<[f32; 4]> {
    let max_id = ownership.iter().copied().max().unwrap_or(0) as usize;
    let mut palette = (0..=max_id)
        .map(|id| {
            if id == 0 {
                [0.035, 0.055, 0.08, 1.0]
            } else {
                let h = (id as u32).wrapping_mul(0x9e37_79b9);
                [
                    0.18 + (h & 255) as f32 / 510.0,
                    0.18 + ((h >> 8) & 255) as f32 / 510.0,
                    0.18 + ((h >> 16) & 255) as f32 / 510.0,
                    1.0,
                ]
            }
        })
        .collect::<Vec<_>>();
    let countries = metadata
        .get("metadata")
        .and_then(Value::as_array)
        .or_else(|| metadata.as_array());
    for country in countries.into_iter().flatten() {
        let Some(id) = country
            .get("id")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
        else {
            continue;
        };
        let Some(color) = country
            .get("color")
            .and_then(Value::as_str)
            .and_then(parse_color)
        else {
            continue;
        };
        if let Some(slot) = palette.get_mut(id) {
            *slot = color;
        }
    }
    palette
}

fn parse_color(value: &str) -> Option<[f32; 4]> {
    if let Some(hex) = value.strip_prefix('#') {
        let raw = u32::from_str_radix(hex, 16).ok()?;
        return match hex.len() {
            6 => Some([
                ((raw >> 16) & 255) as f32 / 255.0,
                ((raw >> 8) & 255) as f32 / 255.0,
                (raw & 255) as f32 / 255.0,
                1.0,
            ]),
            8 => Some([
                ((raw >> 24) & 255) as f32 / 255.0,
                ((raw >> 16) & 255) as f32 / 255.0,
                ((raw >> 8) & 255) as f32 / 255.0,
                (raw & 255) as f32 / 255.0,
            ]),
            _ => None,
        };
    }
    let body = value
        .strip_prefix("rgba(")
        .or_else(|| value.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let values = body
        .split(',')
        .map(|v| v.trim().parse::<f32>().ok())
        .collect::<Option<Vec<_>>>()?;
    if values.len() < 3 {
        return None;
    }
    Some([
        values[0] / 255.0,
        values[1] / 255.0,
        values[2] / 255.0,
        values.get(3).copied().unwrap_or(1.0).max(0.65),
    ])
}

fn main() -> Result<()> {
    env_logger::init();
    let mut scenario = None;
    let mut smoke_frames = None;
    for argument in std::env::args_os().skip(1) {
        if argument == "--smoke" {
            smoke_frames = Some(3);
        } else if scenario.is_none() {
            scenario = Some(PathBuf::from(argument));
        } else {
            anyhow::bail!("unexpected argument {:?}", argument);
        }
    }
    let scenario = scenario.unwrap_or_else(|| PathBuf::from(DEFAULT_SCENARIO));
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut App::new(scenario, smoke_frames))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_fit_preserves_two_to_one_world_aspect() {
        assert_eq!(reset_zoom(PhysicalSize::new(1_280, 720)), 640.0);
        assert_eq!(reset_zoom(PhysicalSize::new(2_000, 600)), 600.0);
    }

    #[test]
    fn screen_world_coordinates_flip_scenario_rows() {
        assert_eq!(world_to_cell([0.0, 0.0], 2_400, 1_200), Some((0, 1_199)));
        assert_eq!(world_to_cell([1.0, 0.5], 2_400, 1_200), Some((1_200, 600)));
        assert_eq!(
            world_to_cell([1.999_999, 0.999_999], 2_400, 1_200),
            Some((2_399, 0))
        );
        assert_eq!(world_to_cell([2.0, 0.5], 2_400, 1_200), None);
    }

    #[test]
    fn parses_web_country_colors() {
        assert_eq!(parse_color("#ff8000"), Some([1.0, 128.0 / 255.0, 0.0, 1.0]));
        assert_eq!(
            parse_color("rgba(10, 20, 30, 0.5)"),
            Some([10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 0.65])
        );
        assert_eq!(parse_color("not-a-color"), None);
    }
}
