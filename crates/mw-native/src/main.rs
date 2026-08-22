use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use mw_checkpoint::native_runtime::{
    load_runtime_checkpoint, write_runtime_checkpoint_state_v2, write_runtime_checkpoint_state_v3,
    write_runtime_checkpoint_state_v4, write_runtime_checkpoint_state_v5,
    write_runtime_checkpoint_state_v6,
};
use mw_core::{
    CombatConfig, CombatUnit, DecodedScenario, FrameSnapshot, GridSpec, NativeRuntime,
    NativeWarBootstrapConfig, ProductionConfig, RuntimeCheckpoint, RuntimeConfig, RuntimeDiplomacy,
    RuntimeState, RuntimeUnitPolicy, ScenarioProduction, Simulation, SimulationConfig,
    SimulationUnit, StrategicSimulation, TerritoryCity, TerritoryConfig, TerritoryControl,
    TerritoryMaps, TerritoryRenderUpdate, TerritoryTilePixels, UnitKind, bootstrap_native_war,
    decode_mwsc_gzip_file, derive_scenario_production,
};
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

mod headless;
mod options;
mod runtime_worker;
mod unit_renderer;

use options::{AppOptions, help_text, parse_app_options};
use runtime_worker::{RuntimeWorker, RuntimeWorkerStatus};
use unit_renderer::{UnitRenderer, geographic_to_world};

const ROW_ALIGNMENT: usize = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
const DEMO_CAMERA_ZOOM_MULTIPLIER: f32 = 70.0;

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
    ownership_texture: wgpu::Texture,
    unit_renderer: UnitRenderer,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DemoBorder {
    first_owner: u16,
    second_owner: u16,
    first_cell: [usize; 2],
    second_cell: [usize; 2],
    midpoint: [f64; 2],
    toward_second: [f64; 2],
}

struct App {
    scenario_path: PathBuf,
    runtime_checkpoint_path: Option<PathBuf>,
    native_war_sides: Vec<Vec<String>>,
    save_checkpoint_path: Option<PathBuf>,
    checkpoint_baseline: Option<DecodedScenario>,
    demo_units_requested: bool,
    runtime_tick_interval: std::time::Duration,
    runtime_queue_capacity: usize,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    ownership: Vec<u16>,
    land: Vec<u8>,
    palette: Vec<[f32; 4]>,
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
    runtime_worker: Option<RuntimeWorker>,
    runtime_center: Option<[f32; 2]>,
    runtime_zoom: Option<f32>,
    runtime_initial_tick: Option<u64>,
    runtime_terminal: bool,
    latest_snapshot: Option<Arc<FrameSnapshot>>,
    snapshot_dirty: bool,
    territory_updates: VecDeque<Arc<TerritoryRenderUpdate>>,
    fps_epoch: Instant,
    fps: f64,
    fatal_error: Option<String>,
}

impl App {
    fn new(options: AppOptions) -> Self {
        Self {
            scenario_path: options.scenario_path,
            runtime_checkpoint_path: options.runtime_checkpoint_path,
            native_war_sides: options.native_war_sides,
            save_checkpoint_path: options.save_checkpoint_path,
            checkpoint_baseline: None,
            demo_units_requested: options.demo_units,
            runtime_tick_interval: options.runtime_tick_interval,
            runtime_queue_capacity: options.runtime_queue_capacity,
            window: None,
            gpu: None,
            ownership: Vec::new(),
            land: Vec::new(),
            palette: Vec::new(),
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
            smoke_frames: options.smoke_frames,
            runtime_worker: None,
            runtime_center: None,
            runtime_zoom: None,
            runtime_initial_tick: None,
            runtime_terminal: false,
            latest_snapshot: None,
            snapshot_dirty: false,
            territory_updates: VecDeque::new(),
            fps_epoch: Instant::now(),
            fps: 0.0,
            fatal_error: None,
        }
    }

    fn initialize(&mut self, window: Arc<Window>) -> Result<()> {
        let load_started = Instant::now();
        let checkpoint_path = self.runtime_checkpoint_path.clone();
        let (decoded, mut pending_runtime, demo_border, runtime_label, checkpoint_baseline) =
            if let Some(checkpoint_path) = checkpoint_path.as_ref() {
                let loaded = load_runtime_checkpoint(&self.scenario_path, checkpoint_path)
                    .with_context(|| {
                        format!(
                            "failed to load native runtime checkpoint {}",
                            checkpoint_path.display()
                        )
                    })?;
                validate_production_checkpoint(
                    loaded.checkpoint_boundary,
                    loaded.resumable,
                    loaded.exact_geography_supplied,
                )?;
                let label = format!(
                    "checkpoint {} ({} units)",
                    checkpoint_path.display(),
                    loaded.unit_count
                );
                let baseline = self
                    .save_checkpoint_path
                    .is_some()
                    .then_some(loaded.baseline);
                (
                    loaded.decoded,
                    Some(loaded.runtime),
                    None,
                    Some(label),
                    baseline,
                )
            } else {
                let target = GridSpec::world(0.15).context("invalid 0.15 degree target grid")?;
                let decoded = decode_mwsc_gzip_file(&self.scenario_path, Some(target))
                    .with_context(|| {
                        format!("failed to decode {}", self.scenario_path.display())
                    })?;
                let baseline = self.save_checkpoint_path.is_some().then(|| decoded.clone());
                if !self.native_war_sides.is_empty() {
                    let sides = resolve_native_war_sides(&decoded, &self.native_war_sides)?;
                    let runtime = bootstrap_native_war(
                        &decoded,
                        &NativeWarBootstrapConfig {
                            sides,
                            hostility: None,
                            production: ProductionConfig::default(),
                            war_grace_end: 600,
                        },
                    )
                    .context("failed to bootstrap native war")?;
                    let unit_count = runtime.latest_snapshot().frame_snapshot.units.len();
                    (
                        decoded,
                        Some(runtime),
                        None,
                        Some(format!("native new war ({unit_count} units)")),
                        baseline,
                    )
                } else if self.demo_units_requested {
                    let border = find_demo_border(
                        &decoded.world_control,
                        &decoded.land,
                        decoded.target.width,
                        decoded.target.height,
                        decoded.target.grid_res,
                    )
                    .context("--demo-units requires an adjacent-country land border")?;
                    let production =
                        derive_scenario_production(&decoded, &ProductionConfig::default())
                            .context(
                                "failed to derive scenario production data for native runtime",
                            )?;
                    let runtime = create_demo_runtime(border, &decoded, production)?;
                    (
                        decoded,
                        Some(runtime),
                        Some(border),
                        Some("scenario-derived demo".to_owned()),
                        baseline,
                    )
                } else {
                    (decoded, None, None, None, None)
                }
            };
        self.checkpoint_baseline = checkpoint_baseline;
        log::info!(
            "loaded {} entries into {}x{} in {:.1} ms",
            decoded.entry_count,
            decoded.target.width,
            decoded.target.height,
            load_started.elapsed().as_secs_f64() * 1_000.0
        );
        self.grid_width =
            u32::try_from(decoded.target.width).context("scenario width exceeds GPU limits")?;
        self.grid_height =
            u32::try_from(decoded.target.height).context("scenario height exceeds GPU limits")?;
        self.grid_res = decoded.target.grid_res as f32;
        anyhow::ensure!(
            decoded.world_control.len() == self.grid_width as usize * self.grid_height as usize,
            "ownership grid has {} cells, expected {}x{}",
            decoded.world_control.len(),
            self.grid_width,
            self.grid_height
        );
        anyhow::ensure!(
            decoded.land.len() == decoded.world_control.len(),
            "land grid has {} cells, expected {}",
            decoded.land.len(),
            decoded.world_control.len()
        );

        self.ownership = decoded.world_control;
        self.land = decoded.land;
        self.metadata = decoded.metadata;
        self.palette = build_palette(&self.metadata, &self.ownership);
        let size = window.inner_size();
        self.zoom = reset_zoom(size);
        if let Some(runtime) = pending_runtime.as_mut() {
            let published = runtime.latest_snapshot();
            // Apply the tick-zero full replacement before creating/presenting the GPU texture.
            while let Some(update) = runtime.pop_render_update() {
                apply_territory_update_to_grid(
                    &mut self.ownership,
                    self.grid_width as usize,
                    self.grid_height as usize,
                    &update,
                )?;
            }
            let (center, zoom) = if let Some(border) = demo_border {
                (
                    geographic_to_world(border.midpoint[0], border.midpoint[1]),
                    demo_zoom(size),
                )
            } else {
                camera_for_runtime(&published.frame_snapshot, size)
            };
            self.runtime_center = Some(center);
            self.runtime_zoom = Some(zoom);
            self.center = center;
            self.zoom = zoom;
            self.runtime_initial_tick = Some(published.tick);
            self.latest_snapshot = Some(published.frame_snapshot.clone());
            self.snapshot_dirty = true;
            log::info!(
                "initialized {} at tick {} with {} rendered units",
                runtime_label.as_deref().unwrap_or("native runtime"),
                published.tick,
                published.frame_snapshot.units.len()
            );
        }

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

        let view_uniform = self.view_uniform(size, self.palette.len() as u32);
        let view_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("map view uniform"),
            contents: bytemuck::bytes_of(&view_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let palette_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("country palette"),
            contents: bytemuck::cast_slice(&self.palette),
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
        let mut unit_renderer = UnitRenderer::new(&device, &view_buffer, format);
        if let Some(snapshot) = &self.latest_snapshot {
            unit_renderer.upload(&device, &queue, snapshot, &self.palette);
        }
        log::info!(
            "unit overlay initialized with {} instances",
            unit_renderer.instance_count()
        );

        self.gpu = Some(GpuState {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            view_buffer,
            ownership_texture,
            unit_renderer,
        });
        self.window = Some(window);
        if let Some(runtime) = pending_runtime {
            let step_limit = self.smoke_frames.map(|_| 1);
            let worker = if let Some(limit) = step_limit {
                RuntimeWorker::spawn_with_limit(
                    runtime,
                    self.runtime_tick_interval,
                    self.runtime_queue_capacity,
                    Some(limit),
                )?
            } else {
                RuntimeWorker::spawn(
                    runtime,
                    self.runtime_tick_interval,
                    self.runtime_queue_capacity,
                )?
            };
            log::debug!(
                "runtime worker owns immutable snapshot at tick {}",
                worker.latest_snapshot().tick
            );
            self.runtime_worker = Some(worker);
        }
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

    fn reset_camera(&mut self, size: PhysicalSize<u32>) {
        if let Some(center) = self.runtime_center {
            self.center = center;
            self.zoom = self.runtime_zoom.unwrap_or_else(|| reset_zoom(size));
        } else {
            self.center = [1.0, 0.5];
            self.zoom = reset_zoom(size);
        }
    }

    fn stop_runtime_worker(&mut self) {
        let Some(mut worker) = self.runtime_worker.take() else {
            return;
        };
        let join_panicked = worker.stop_and_join().is_err();
        let mut worker_failure = None;
        while let Some(status) = worker.poll_status() {
            match status {
                RuntimeWorkerStatus::Failed(error) => {
                    worker_failure = Some(format!("native runtime worker failed: {error}"));
                }
                RuntimeWorkerStatus::Panicked(error) => {
                    worker_failure = Some(format!("native runtime worker panicked: {error}"));
                }
                RuntimeWorkerStatus::Stopped
                | RuntimeWorkerStatus::Terminal(_)
                | RuntimeWorkerStatus::Completed { .. } => {}
            }
        }
        if self.fatal_error.is_none() {
            self.fatal_error = worker_failure.or_else(|| {
                join_panicked.then(|| "native runtime worker panicked during shutdown".to_owned())
            });
        }
    }

    fn save_runtime_checkpoint(&mut self) -> Result<()> {
        let Some(output) = self.save_checkpoint_path.as_ref() else {
            return Ok(());
        };
        let baseline = self
            .checkpoint_baseline
            .as_ref()
            .context("native runtime save is missing its immutable scenario baseline")?;
        let worker = self
            .runtime_worker
            .as_ref()
            .context("native runtime save requested without an active runtime worker")?;
        if matches!(
            worker.latest_snapshot().state,
            RuntimeState::ConflictResolved { .. }
        ) {
            log::warn!(
                "checkpoint save skipped: conflictResolved is terminal and cannot be resumed as a mid-war checkpoint"
            );
            return Ok(());
        }
        let state = worker.checkpoint_state().map_err(|error| {
            anyhow::anyhow!("failed to capture native runtime checkpoint state: {error}")
        })?;
        let writer = if state.operational_execution.is_some() && state.air_power.is_some() {
            write_runtime_checkpoint_state_v6
        } else if state.operations.is_some() {
            write_runtime_checkpoint_state_v5
        } else if state.side_dynamics.is_some() {
            write_runtime_checkpoint_state_v4
        } else if state.influence_runtime.is_some() {
            write_runtime_checkpoint_state_v3
        } else {
            write_runtime_checkpoint_state_v2
        };
        let report = writer(&self.scenario_path, baseline, &state, output, 1)?;
        log::info!(
            "saved {} bytes of {} to {} at tick {}",
            report.bytes,
            report.schema,
            report.path,
            state.tick
        );
        Ok(())
    }

    fn drain_runtime_worker(&mut self) -> Result<()> {
        let Some(worker) = self.runtime_worker.as_ref() else {
            return Ok(());
        };
        let mut drained = worker.drain_render_state();
        let mut statuses = Vec::new();
        while let Some(status) = worker.poll_status() {
            statuses.push(status);
        }
        if !statuses.is_empty() {
            // Every successful terminal/completion status is sent after its final
            // atomic publication. A second drain closes the cross-channel race
            // without weakening the publication's delta/snapshot coherence.
            let final_drain = worker.drain_render_state();
            drained
                .territory_updates
                .extend(final_drain.territory_updates);
            if final_drain.snapshot.is_some() {
                drained.snapshot = final_drain.snapshot;
            }
        }

        for update in drained.territory_updates {
            apply_territory_update_to_grid(
                &mut self.ownership,
                self.grid_width as usize,
                self.grid_height as usize,
                &update,
            )?;
            self.territory_updates.push_back(update);
        }
        if let Some(snapshot) = drained.snapshot {
            log::trace!(
                "runtime tick {}: {} units, {} contacts, {} direct engagements, {} moves, {} influence cells, territory commit {}",
                snapshot.tick,
                snapshot.frame_snapshot.units.len(),
                snapshot.counters.simulation.accepted_contacts,
                snapshot.counters.simulation.direct_events,
                snapshot.counters.simulation.moved_units,
                snapshot.counters.influence.touched_influence_cells,
                snapshot.counters.census.committed,
            );
            self.latest_snapshot = Some(snapshot.frame_snapshot.clone());
            self.snapshot_dirty = true;
        }
        for status in statuses {
            match status {
                RuntimeWorkerStatus::Stopped => {
                    self.runtime_terminal = true;
                    log::info!("native runtime worker stopped");
                }
                RuntimeWorkerStatus::Terminal(state) => {
                    self.runtime_terminal = true;
                    log::warn!("native runtime reached terminal state: {state:?}");
                }
                RuntimeWorkerStatus::Completed { steps } => {
                    self.runtime_terminal = true;
                    log::info!("native runtime completed its {steps}-step limit");
                }
                RuntimeWorkerStatus::Failed(error) => {
                    anyhow::bail!("native runtime worker failed: {error}");
                }
                RuntimeWorkerStatus::Panicked(error) => {
                    anyhow::bail!("native runtime worker panicked: {error}");
                }
            }
        }
        Ok(())
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

    fn render(&mut self) -> Result<()> {
        self.drain_runtime_worker()?;
        let Some(window) = &self.window else {
            return Ok(());
        };
        let size = window.inner_size();
        let palette_len = self.palette.len() as u32;
        let uniform = self.view_uniform(size, palette_len);
        let Some(gpu) = &mut self.gpu else {
            return Ok(());
        };
        gpu.queue
            .write_buffer(&gpu.view_buffer, 0, bytemuck::bytes_of(&uniform));
        while let Some(update) = self.territory_updates.pop_front() {
            upload_territory_update(&gpu.queue, &gpu.ownership_texture, &update)
                .inspect_err(|_| self.territory_updates.push_front(Arc::clone(&update)))?;
        }
        if self.snapshot_dirty
            && let Some(snapshot) = &self.latest_snapshot
        {
            gpu.unit_renderer
                .upload(&gpu.device, &gpu.queue, snapshot, &self.palette);
            self.snapshot_dirty = false;
        }
        let rendered_unit_count = gpu.unit_renderer.instance_count();
        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                log::debug!("suboptimal surface frame");
                frame
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                anyhow::bail!("render surface lost");
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                anyhow::bail!("surface validation error");
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
            gpu.unit_renderer.draw(&mut pass);
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
            let snapshot_status = self
                .latest_snapshot
                .as_ref()
                .map_or_else(String::new, |snapshot| {
                    format!(" | tick {} | {rendered_unit_count} units", snapshot.tick)
                });
            window.set_title(&format!(
                "Modern Wars Native | {:.0} FPS | {}x{} @ {:.2}° | zoom {:.1}{}",
                self.fps,
                self.grid_width,
                self.grid_height,
                self.grid_res,
                self.zoom,
                snapshot_status
            ));
        }
        window.request_redraw();
        Ok(())
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
                let message = format!("window creation failed: {error}");
                log::error!("{message}");
                self.fatal_error = Some(message);
                event_loop.exit();
                return;
            }
        };
        if let Err(error) = self.initialize(window) {
            let message = format!("initialization failed: {error:#}");
            log::error!("{message}");
            self.fatal_error = Some(message);
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
                        self.reset_camera(window.inner_size());
                        window.request_redraw();
                    }
                    PhysicalKey::Code(KeyCode::KeyS) if self.save_checkpoint_path.is_some() => {
                        if let Err(error) = self.save_runtime_checkpoint() {
                            let message = format!("checkpoint save failed: {error:#}");
                            log::error!("{message}");
                            self.fatal_error = Some(message);
                            event_loop.exit();
                        }
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
                if let Err(error) = self.render() {
                    let message = format!("rendering failed: {error:#}");
                    log::error!("{message}");
                    self.fatal_error = Some(message);
                    event_loop.exit();
                    return;
                }
                if let Some(target) = self.smoke_frames {
                    let runtime_ready = self.runtime_initial_tick.is_none()
                        || self.runtime_terminal
                        || self.latest_snapshot.as_ref().is_some_and(|snapshot| {
                            Some(snapshot.tick) > self.runtime_initial_tick
                        });
                    if self.presented_frames >= target && runtime_ready {
                        event_loop.exit();
                    }
                }
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if self.fatal_error.is_none()
            && let Err(error) = self.save_runtime_checkpoint()
        {
            let message = format!("checkpoint save failed during shutdown: {error:#}");
            log::error!("{message}");
            self.fatal_error = Some(message);
        }
        self.stop_runtime_worker();
    }
}

fn reset_zoom(size: PhysicalSize<u32>) -> f32 {
    (size.width as f32 * 0.5).min(size.height as f32).max(1.0)
}

fn demo_zoom(size: PhysicalSize<u32>) -> f32 {
    (reset_zoom(size) * DEMO_CAMERA_ZOOM_MULTIPLIER).clamp(1.0, 100_000.0)
}

fn camera_for_runtime(snapshot: &FrameSnapshot, size: PhysicalSize<u32>) -> ([f32; 2], f32) {
    if snapshot.units.is_empty() {
        return ([1.0, 0.5], reset_zoom(size));
    }
    // The current map shader does not repeat at the antimeridian, so use the
    // literal projected bounds. A circular fit could hide units on the other
    // edge until world-wrap rendering itself is implemented.
    let (min_x, max_x, min_y, max_y) = snapshot
        .units
        .iter()
        .map(|unit| geographic_to_world(unit.lat, unit.lng))
        .fold(
            (
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ),
            |(min_x, max_x, min_y, max_y), world| {
                let x = f64::from(world[0]);
                let y = f64::from(world[1]);
                (min_x.min(x), max_x.max(x), min_y.min(y), max_y.max(y))
            },
        );
    let horizontal_span = (max_x - min_x).max(0.02);
    let vertical_span = (max_y - min_y).max(0.02);
    let padding = 1.35_f64;
    let horizontal_zoom = size.width.max(1) as f64 / (horizontal_span * padding);
    let vertical_zoom = size.height.max(1) as f64 / (vertical_span * padding);
    let zoom = horizontal_zoom.min(vertical_zoom).clamp(1.0, 100_000.0) as f32;
    (
        [
            ((min_x + max_x) * 0.5) as f32,
            ((min_y + max_y) * 0.5) as f32,
        ],
        zoom,
    )
}

fn find_demo_border(
    ownership: &[u16],
    land: &[u8],
    width: usize,
    height: usize,
    grid_res: f64,
) -> Option<DemoBorder> {
    let cell_count = width.checked_mul(height)?;
    if width == 0
        || height == 0
        || ownership.len() != cell_count
        || land.len() != cell_count
        || !grid_res.is_finite()
        || grid_res <= 0.0
    {
        return None;
    }

    let mut best: Option<(f64, usize, DemoBorder)> = None;
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let owner = ownership[index];
            if owner == 0 || land[index] == 0 {
                continue;
            }
            if x + 1 < width {
                consider_demo_border(
                    &mut best,
                    ownership,
                    land,
                    width,
                    grid_res,
                    [x, y],
                    [x + 1, y],
                    index * 2,
                );
            }
            if y + 1 < height {
                consider_demo_border(
                    &mut best,
                    ownership,
                    land,
                    width,
                    grid_res,
                    [x, y],
                    [x, y + 1],
                    index * 2 + 1,
                );
            }
        }
    }
    best.map(|(_, _, border)| border)
}

#[allow(clippy::too_many_arguments)]
fn consider_demo_border(
    best: &mut Option<(f64, usize, DemoBorder)>,
    ownership: &[u16],
    land: &[u8],
    width: usize,
    grid_res: f64,
    first_cell: [usize; 2],
    second_cell: [usize; 2],
    ordinal: usize,
) {
    let first_index = first_cell[1] * width + first_cell[0];
    let second_index = second_cell[1] * width + second_cell[0];
    let first_owner = ownership[first_index];
    let second_owner = ownership[second_index];
    if first_owner == 0
        || second_owner == 0
        || first_owner == second_owner
        || land[first_index] == 0
        || land[second_index] == 0
    {
        return;
    }

    let first = cell_geographic_center(first_cell, grid_res);
    let second = cell_geographic_center(second_cell, grid_res);
    let delta = [second[0] - first[0], second[1] - first[1]];
    let magnitude = (delta[0] * delta[0] + delta[1] * delta[1]).sqrt();
    if !magnitude.is_finite() || magnitude <= 0.0 {
        return;
    }
    let midpoint = [(first[0] + second[0]) * 0.5, (first[1] + second[1]) * 0.5];
    // Prefer a continental European border so the opt-in visual smoke test is
    // stable and recognizable, while still deriving both countries and every
    // coordinate from the decoded scenario.
    let score = (midpoint[0] - 50.0).powi(2) + (midpoint[1] - 10.0).powi(2) * 0.4;
    let border = DemoBorder {
        first_owner,
        second_owner,
        first_cell,
        second_cell,
        midpoint,
        toward_second: [delta[0] / magnitude, delta[1] / magnitude],
    };
    if best
        .as_ref()
        .is_none_or(|(best_score, best_ordinal, _)| (score, ordinal) < (*best_score, *best_ordinal))
    {
        *best = Some((score, ordinal, border));
    }
}

fn cell_geographic_center(cell: [usize; 2], grid_res: f64) -> [f64; 2] {
    [
        -90.0 + (cell[1] as f64 + 0.5) * grid_res,
        -180.0 + (cell[0] as f64 + 0.5) * grid_res,
    ]
}

fn create_demo_runtime(
    border: DemoBorder,
    decoded: &DecodedScenario,
    production: ScenarioProduction,
) -> Result<NativeRuntime> {
    let normal = border.toward_second;
    let tangent = [-normal[1], normal[0]];
    let half_separation = 0.018;
    let lane_offset = 0.03;
    let first_base = [
        border.midpoint[0] - normal[0] * half_separation,
        border.midpoint[1] - normal[1] * half_separation,
    ];
    let second_base = [
        border.midpoint[0] + normal[0] * half_separation,
        border.midpoint[1] + normal[1] * half_separation,
    ];
    let positions = [
        offset_geographic(first_base, tangent, -lane_offset),
        offset_geographic(first_base, tangent, lane_offset),
        offset_geographic(second_base, tangent, -lane_offset),
        offset_geographic(second_base, tangent, lane_offset),
    ];
    let units = vec![
        demo_unit(
            1,
            0,
            u64::from(border.first_owner),
            UnitKind::Army,
            positions[0],
            normal,
        ),
        demo_unit(
            2,
            0,
            u64::from(border.first_owner),
            UnitKind::Armor,
            positions[1],
            normal,
        ),
        demo_unit(
            3,
            1,
            u64::from(border.second_owner),
            UnitKind::Army,
            positions[2],
            [-normal[0], -normal[1]],
        ),
        demo_unit(
            4,
            1,
            u64::from(border.second_owner),
            UnitKind::Armor,
            positions[3],
            [-normal[0], -normal[1]],
        ),
    ];
    let combat = CombatConfig {
        combat_damage: 0.005,
        target_jitter_scale: 0.0,
        ..CombatConfig::default()
    };
    let simulation = Simulation::new(
        SimulationConfig {
            tactical_cell_size: 0.6,
            combat,
        },
        units,
    )?;
    let cell_count = decoded.target.cell_count()?;
    anyhow::ensure!(
        decoded.world_control.len() == cell_count,
        "demo ownership grid has {} cells, expected {cell_count}",
        decoded.world_control.len()
    );
    anyhow::ensure!(
        decoded.land.len() == cell_count,
        "demo land grid has {} cells, expected {cell_count}",
        decoded.land.len()
    );

    let country_to_side = BTreeMap::from([
        (border.first_owner, 0_usize),
        (border.second_owner, 1_usize),
    ]);
    let mut runtime_land = vec![0_u8; cell_count];
    let mut primary_occupier = vec![0_u16; cell_count];
    let mut dominant_side = vec![-1_i16; cell_count];
    let mut side_influence = vec![vec![0.0_f32; cell_count]; 2];
    let mut occupation = vec![0.0_f32; cell_count];
    for cell in 0..cell_count {
        if decoded.land[cell] == 0 {
            continue;
        }
        let owner = decoded.world_control[cell];
        let Some(&side) = country_to_side.get(&owner) else {
            // Scenario land outside the explicit demo theater stays traversable,
            // but receives no controller, primary credit, or influence state.
            runtime_land[cell] = 1;
            continue;
        };
        runtime_land[cell] = 2;
        primary_occupier[cell] = owner;
        dominant_side[cell] = side as i16;
        side_influence[side][cell] = 1.0;
        occupation[cell] = if side == 0 { 1.0 } else { -1.0 };
    }

    let cities = production
        .cities
        .iter()
        .map(|city| TerritoryCity {
            id: city.city_id,
            cell: city.cell,
            owner: city.owner_id,
            population: city.population,
            capital: city.capital,
        })
        .collect();
    let territory = TerritoryControl::new(TerritoryConfig {
        width: decoded.target.width,
        height: decoded.target.height,
        grid_resolution: decoded.target.grid_res,
        max_sides: 2,
        tile_size: 32,
        maps: TerritoryMaps {
            land: runtime_land,
            world_control: decoded.world_control.clone(),
            de_jure: decoded.de_jure.clone(),
            primary_occupier,
            dominant_side,
            occupation,
            side_influence,
        },
        country_to_side: country_to_side.clone(),
        hostility_matrix: vec![0, 1, 1, 0],
        cities,
        protected_owner_ids: country_to_side.keys().copied().collect(),
        topology_revision: 1,
        world_revision: 1,
        city_revision: 1,
    })?;

    let economies = production
        .economy_states
        .iter()
        .filter(|economy| country_to_side.contains_key(&economy.country_id))
        .cloned()
        .collect::<Vec<_>>();
    anyhow::ensure!(
        economies.len() == country_to_side.len(),
        "demo theater countries are missing derived economy state"
    );
    let strategic = StrategicSimulation::new(economies, Vec::new())?;
    let unit_policies = simulation
        .units
        .iter()
        .map(|unit| {
            RuntimeUnitPolicy::standard(
                unit.combat.id,
                u16::try_from(unit.combat.sovereign).expect("demo sovereigns originate as u16"),
            )
        })
        .collect();
    let runtime = NativeRuntime::new(
        RuntimeConfig::default(),
        RuntimeCheckpoint {
            tick: 0,
            frame: 0,
            war_grace_end: 0,
            simulation,
            territory,
            strategic,
            scenario: production,
            diplomacy: RuntimeDiplomacy {
                hostility: vec![0, 1, 1, 0],
                active_sides: vec![0, 1],
            },
            unit_policies,
            battlefield: None,
            // Front objectives are derived by the runtime on its first step.
            objectives: Vec::new(),
            prior_objective_by_unit: BTreeMap::new(),
            front_prior_by_unit: BTreeMap::new(),
            last_front_refresh_tick: None,
            casualties: BTreeMap::new(),
            casualties_by_victim: BTreeMap::new(),
            side_dynamics: None,
            operations: None,
            operational_execution: None,
            air_power: None,
        },
    )?;
    Ok(runtime)
}

fn validate_production_checkpoint(
    checkpoint_boundary: &str,
    resumable: bool,
    exact_geography_supplied: bool,
) -> Result<()> {
    anyhow::ensure!(
        resumable && matches!(checkpoint_boundary, "postStartWar" | "midWar"),
        "mw-native only renders resumable postStartWar or midWar checkpoints; {checkpoint_boundary:?} is not a production continuation boundary"
    );
    // The strict shared loader requires immutable exact geography for postStartWar and both
    // immutable geography plus committed live territory for midWar. This flag is the final
    // handoff assertion that the renderer will never fall back to an approximate scenario map.
    anyhow::ensure!(
        exact_geography_supplied,
        "production runtime checkpoint is missing exact geography or live territory"
    );
    Ok(())
}

fn resolve_native_war_sides(
    decoded: &DecodedScenario,
    requested: &[Vec<String>],
) -> Result<Vec<Vec<u16>>> {
    let production = derive_scenario_production(decoded, &ProductionConfig::default())
        .context("failed to derive countries for native war selection")?;
    let known_ids = production
        .countries
        .iter()
        .map(|country| country.country_id)
        .collect::<BTreeSet<_>>();
    let mut claimed = BTreeSet::new();
    let mut sides = Vec::with_capacity(requested.len());

    for (side_index, selectors) in requested.iter().enumerate() {
        let mut side = Vec::with_capacity(selectors.len());
        for selector in selectors {
            let country_id = if let Ok(id) = selector.parse::<u16>() {
                anyhow::ensure!(
                    id != 0 && known_ids.contains(&id),
                    "side {} references unknown country ID {id}",
                    side_index + 1
                );
                id
            } else {
                let matches = production
                    .countries
                    .iter()
                    .filter(|country| country.name.eq_ignore_ascii_case(selector))
                    .map(|country| country.country_id)
                    .collect::<Vec<_>>();
                anyhow::ensure!(
                    !matches.is_empty(),
                    "side {} references unknown country {selector:?}",
                    side_index + 1
                );
                anyhow::ensure!(
                    matches.len() == 1,
                    "side {} country name {selector:?} is ambiguous; use a numeric ID",
                    side_index + 1
                );
                matches[0]
            };
            anyhow::ensure!(
                claimed.insert(country_id),
                "country {country_id} appears in more than one native war side"
            );
            side.push(country_id);
        }
        side.sort_unstable();
        sides.push(side);
    }
    Ok(sides)
}

fn apply_territory_update_to_grid(
    ownership: &mut [u16],
    width: usize,
    height: usize,
    update: &TerritoryRenderUpdate,
) -> Result<()> {
    let expected_cells = width
        .checked_mul(height)
        .context("territory render dimensions overflow")?;
    anyhow::ensure!(
        ownership.len() == expected_cells,
        "ownership grid has {} cells, expected {expected_cells}",
        ownership.len()
    );
    for tile in &update.tiles {
        let bounds = tile.bounds;
        anyhow::ensure!(
            bounds.min_x < bounds.max_x
                && bounds.min_y < bounds.max_y
                && bounds.max_x <= width
                && bounds.max_y <= height,
            "territory tile {} has invalid bounds",
            bounds.tile
        );
        let tile_width = bounds.max_x - bounds.min_x;
        let tile_height = bounds.max_y - bounds.min_y;
        let expected_pixels = tile_width
            .checked_mul(tile_height)
            .context("territory tile dimensions overflow")?;
        anyhow::ensure!(
            tile.pixels.len() == expected_pixels,
            "territory tile {} has {} pixels, expected {expected_pixels}",
            bounds.tile,
            tile.pixels.len()
        );
        for row in 0..tile_height {
            let source_start = row * tile_width;
            let target_start = (bounds.min_y + row) * width + bounds.min_x;
            ownership[target_start..target_start + tile_width]
                .copy_from_slice(&tile.pixels[source_start..source_start + tile_width]);
        }
    }
    Ok(())
}

fn pack_territory_tile(tile: &TerritoryTilePixels) -> Result<(Vec<u8>, u32, u32)> {
    let bounds = tile.bounds;
    anyhow::ensure!(
        bounds.min_x < bounds.max_x && bounds.min_y < bounds.max_y,
        "territory tile {} has empty bounds",
        bounds.tile
    );
    let width = bounds.max_x - bounds.min_x;
    let height = bounds.max_y - bounds.min_y;
    let expected_pixels = width
        .checked_mul(height)
        .context("territory tile dimensions overflow")?;
    anyhow::ensure!(
        tile.pixels.len() == expected_pixels,
        "territory tile {} has {} pixels, expected {expected_pixels}",
        bounds.tile,
        tile.pixels.len()
    );
    let unpadded_bytes_per_row = width
        .checked_mul(std::mem::size_of::<u16>())
        .context("territory tile row size overflow")?;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(ROW_ALIGNMENT) * ROW_ALIGNMENT;
    let mut packed = vec![0_u8; padded_bytes_per_row * height];
    for row in 0..height {
        let source = &tile.pixels[row * width..(row + 1) * width];
        let bytes = bytemuck::cast_slice(source);
        let target_start = row * padded_bytes_per_row;
        packed[target_start..target_start + unpadded_bytes_per_row].copy_from_slice(bytes);
    }
    Ok((
        packed,
        u32::try_from(padded_bytes_per_row).context("territory tile row exceeds GPU limits")?,
        u32::try_from(height).context("territory tile height exceeds GPU limits")?,
    ))
}

fn upload_territory_update(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    update: &TerritoryRenderUpdate,
) -> Result<()> {
    let texture_size = texture.size();
    for tile in &update.tiles {
        let bounds = tile.bounds;
        anyhow::ensure!(
            bounds.max_x <= texture_size.width as usize
                && bounds.max_y <= texture_size.height as usize,
            "territory tile {} exceeds ownership texture",
            bounds.tile
        );
        let (packed, bytes_per_row, rows_per_image) = pack_territory_tile(tile)?;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: u32::try_from(bounds.min_x)
                        .context("territory tile x exceeds GPU limits")?,
                    y: u32::try_from(bounds.min_y)
                        .context("territory tile y exceeds GPU limits")?,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &packed,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(rows_per_image),
            },
            wgpu::Extent3d {
                width: u32::try_from(bounds.max_x - bounds.min_x)
                    .context("territory tile width exceeds GPU limits")?,
                height: rows_per_image,
                depth_or_array_layers: 1,
            },
        );
    }
    Ok(())
}

fn offset_geographic(origin: [f64; 2], direction: [f64; 2], distance: f64) -> [f64; 2] {
    [
        origin[0] + direction[0] * distance,
        origin[1] + direction[1] * distance,
    ]
}

fn demo_unit(
    id: u64,
    side: u64,
    sovereign: u64,
    kind: UnitKind,
    position: [f64; 2],
    direction: [f64; 2],
) -> SimulationUnit {
    let is_armor = kind == UnitKind::Armor;
    SimulationUnit {
        combat: CombatUnit {
            id,
            side,
            sovereign,
            kind,
            lat: position[0],
            lng: position[1],
            health: 100.0,
            max_health: 100.0,
            personnel: if is_armor { 200 } else { 1_000 },
            personnel_capacity: if is_armor { 200 } else { 1_000 },
            equipment: if is_armor { 100 } else { 0 },
            max_equipment: if is_armor { 100 } else { 0 },
            quality: 50.0,
            transport: false,
            armor_supported: false,
            landing_penalty_active: false,
            at_sea: false,
            last_combat_tick: 0,
            victory_boost_ticks: 0,
        },
        dir_lat: direction[0],
        dir_lng: direction[1],
        coast_stuck_ticks: 0,
        armor_landing_penalty_until_tick: 0,
        is_support: false,
        ally_weight: 1.0,
    }
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
    let options = parse_app_options(std::env::args_os().skip(1))?;
    if options.show_help {
        println!("{}", help_text());
        return Ok(());
    }
    if let Some(steps) = options.headless_ticks {
        return headless::run_headless(&options, steps);
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(options);
    let run_result = event_loop.run_app(&mut app);
    app.stop_runtime_worker();
    run_result?;
    if let Some(error) = app.fatal_error {
        anyhow::bail!(error);
    }
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
    fn runtime_camera_keeps_both_antimeridian_edges_visible() {
        let simulation = Simulation::new(
            SimulationConfig::default(),
            vec![
                demo_unit(1, 0, 1, UnitKind::Army, [0.0, -179.0], [0.0, 0.0]),
                demo_unit(2, 1, 2, UnitKind::Army, [0.0, 179.0], [0.0, 0.0]),
            ],
        )
        .unwrap();
        let snapshot = simulation.initial_snapshot(0, 0);
        let (center, zoom) = camera_for_runtime(&snapshot, PhysicalSize::new(1_280, 720));
        assert!((center[0] - 1.0).abs() < 1.0e-6);
        assert!((center[1] - 0.5).abs() < 1.0e-6);
        assert!(zoom < reset_zoom(PhysicalSize::new(1_280, 720)));
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

    #[test]
    fn demo_border_requires_two_adjacent_land_countries() {
        let ownership = [1, 1, 2, 2, 1, 1, 2, 2];
        let land = [1; 8];
        let border = find_demo_border(&ownership, &land, 4, 2, 45.0).unwrap();
        assert_eq!(border.first_owner, 1);
        assert_eq!(border.second_owner, 2);
        assert_eq!(border.first_cell[0] + 1, border.second_cell[0]);

        let ocean_between = [1, 0, 2, 2];
        assert!(find_demo_border(&ocean_between, &[1, 0, 1, 1], 4, 1, 90.0).is_none());
    }

    #[test]
    fn demo_runtime_derives_fronts_and_publishes_country_colored_units() {
        let grid = GridSpec {
            grid_res: 90.0,
            width: 4,
            height: 2,
        };
        let decoded = DecodedScenario {
            metadata: serde_json::json!({
                "metadata": [
                    {"id": 7, "name": "Seven", "gdp": 100, "population": 1_000_000},
                    {"id": 11, "name": "Eleven", "gdp": 80, "population": 800_000},
                    {"id": 13, "name": "Passive", "gdp": 40, "population": 400_000}
                ],
                "cities": [
                    {"id": 70, "name": "Seven City", "ownerId": 7, "lat": -45,
                     "lng": -135, "population": 100_000, "isCapital": true},
                    {"id": 110, "name": "Eleven City", "ownerId": 11, "lat": -45,
                     "lng": -45, "population": 80_000, "isCapital": true}
                ]
            }),
            source: grid,
            target: grid,
            entry_count: 6,
            world_control: vec![7, 11, 13, 0, 7, 11, 13, 0],
            de_jure: vec![7, 11, 13, 0, 7, 11, 13, 0],
            land: vec![1, 1, 1, 0, 1, 1, 1, 0],
            biome: vec![0; 8],
            province: vec![0; 8],
        };
        let border = DemoBorder {
            first_owner: 7,
            second_owner: 11,
            first_cell: [0, 0],
            second_cell: [1, 0],
            midpoint: [-45.0, -90.0],
            toward_second: [0.0, 1.0],
        };
        let production =
            derive_scenario_production(&decoded, &ProductionConfig::default()).unwrap();
        let mut runtime = create_demo_runtime(border, &decoded, production).unwrap();
        let snapshot = runtime.latest_snapshot();
        assert_eq!(snapshot.frame_snapshot.units.len(), 4);
        assert!(
            snapshot
                .frame_snapshot
                .units
                .iter()
                .any(|unit| unit.side == 0 && unit.sovereign == 7)
        );
        assert!(
            snapshot
                .frame_snapshot
                .units
                .iter()
                .any(|unit| unit.side == 1 && unit.sovereign == 11)
        );
        assert_eq!(snapshot.territory_snapshot.land_cells, 4);
        assert_eq!(runtime.pending_render_updates(), 1);

        let next = runtime.step().unwrap();
        assert!(next.counters.front_refreshed);
        assert_eq!(next.counters.front_objectives, 4);
    }

    #[test]
    fn production_checkpoint_policy_accepts_both_resumable_boundaries() {
        validate_production_checkpoint("postStartWar", true, true).unwrap();
        validate_production_checkpoint("midWar", true, true).unwrap();
    }

    #[test]
    fn production_checkpoint_policy_rejects_replays_and_inexact_maps() {
        let replay = validate_production_checkpoint("baselineReplay", false, true)
            .unwrap_err()
            .to_string();
        assert!(replay.contains("not a production continuation boundary"));

        let inexact = validate_production_checkpoint("midWar", true, false)
            .unwrap_err()
            .to_string();
        assert!(inexact.contains("missing exact geography or live territory"));
    }

    #[test]
    fn native_war_side_selection_accepts_names_and_ids_and_rejects_duplicates() {
        let grid = GridSpec {
            grid_res: 180.0,
            width: 2,
            height: 1,
        };
        let decoded = DecodedScenario {
            metadata: serde_json::json!({
                "metadata": [
                    {"id": 7, "name": "Seven", "gdp": 10, "population": 1000},
                    {"id": 11, "name": "Eleven", "gdp": 10, "population": 1000}
                ]
            }),
            source: grid,
            target: grid,
            entry_count: 2,
            world_control: vec![7, 11],
            de_jure: vec![7, 11],
            land: vec![1, 1],
            biome: vec![0; 2],
            province: vec![0; 2],
        };

        assert_eq!(
            resolve_native_war_sides(&decoded, &[vec!["seven".to_owned()], vec!["11".to_owned()]],)
                .unwrap(),
            vec![vec![7], vec![11]]
        );
        assert!(
            resolve_native_war_sides(&decoded, &[vec!["Seven".to_owned()], vec!["7".to_owned()]],)
                .is_err()
        );
        assert!(
            resolve_native_war_sides(
                &decoded,
                &[vec!["Missing".to_owned()], vec!["11".to_owned()]],
            )
            .is_err()
        );
    }

    #[test]
    fn territory_tiles_patch_cpu_grid_and_pack_aligned_gpu_rows() {
        let tile = TerritoryTilePixels {
            bounds: mw_core::TileBounds {
                tile: 3,
                min_x: 1,
                min_y: 1,
                max_x: 3,
                max_y: 3,
            },
            pixels: vec![9, 8, 7, 6],
        };
        let update = TerritoryRenderUpdate {
            full_update: false,
            tiles: vec![tile.clone()],
        };
        let mut ownership = vec![0_u16; 12];
        apply_territory_update_to_grid(&mut ownership, 4, 3, &update).unwrap();
        assert_eq!(ownership, vec![0, 0, 0, 0, 0, 9, 8, 0, 0, 7, 6, 0]);

        let (packed, bytes_per_row, rows) = pack_territory_tile(&tile).unwrap();
        assert_eq!(bytes_per_row as usize, ROW_ALIGNMENT);
        assert_eq!(rows, 2);
        assert_eq!(&packed[..4], bytemuck::cast_slice::<u16, u8>(&[9_u16, 8]));
        assert_eq!(
            &packed[ROW_ALIGNMENT..ROW_ALIGNMENT + 4],
            bytemuck::cast_slice::<u16, u8>(&[7_u16, 6])
        );
    }
}
