use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use mw_checkpoint::native_runtime::{
    load_runtime_checkpoint, write_runtime_checkpoint_state_v2, write_runtime_checkpoint_state_v3,
    write_runtime_checkpoint_state_v4, write_runtime_checkpoint_state_v5,
    write_runtime_checkpoint_state_v6, write_runtime_checkpoint_state_v9,
    write_runtime_checkpoint_state_v10, write_runtime_checkpoint_state_v11,
    write_runtime_checkpoint_state_v12,
};
use mw_core::{
    CombatConfig, CombatUnit, DecodedScenario, FrameSnapshot, GridSpec, NativeRuntime,
    NativeWarBootstrapConfig, ProductionCity, ProductionConfig, RuntimeCheckpoint, RuntimeConfig,
    RuntimeDiplomacy, RuntimeSnapshot, RuntimeState, RuntimeUnitPolicy, ScenarioProduction,
    Simulation, SimulationConfig, SimulationUnit, StrategicSimulation, TerritoryCity,
    TerritoryConfig, TerritoryControl, TerritoryMaps, TerritoryRenderUpdate, TerritoryTilePixels,
    UnitKind, bootstrap_native_war, decode_mwsc_gzip_file, derive_scenario_production,
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
mod map_label;
mod map_material;
mod observer;
mod observer_hud;
mod options;
mod projection;
mod runtime_worker;
mod unit_renderer;
mod world_overlay;

use map_label::{MapLabelRenderer, MapLabelView};
use map_material::MapMaterial;
use observer::ObserverHudModel;
use observer_hud::{
    HudHit, ObserverHudRenderer, ObserverHudUpload, PlaybackAction, PlaybackPresentation,
    hud_hit_test, hud_layout,
};
use options::{AppOptions, help_text, parse_app_options};
use projection::{
    browser_zoom, geographic_to_world, pixels_per_world_for_zoom, world_to_geographic,
    world_to_grid,
};
use runtime_worker::{RuntimeWorker, RuntimeWorkerControlEvent, RuntimeWorkerStatus};
use unit_renderer::UnitRenderer;
use world_overlay::WorldOverlayRenderer;

const ROW_ALIGNMENT: usize = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
const PLAYBACK_SPEEDS: [u8; 3] = [1, 2, 3];
const DEMO_CAMERA_ZOOM_MULTIPLIER: f32 = 70.0;
const SMOKE_TIMEOUT: Duration = Duration::from_secs(10);
const BROWSER_DEFAULT_CENTER: [f64; 2] = [20.0, 0.0];
const BROWSER_DEFAULT_ZOOM: f32 = 3.0;
const BROWSER_MIN_ZOOM: f32 = 2.0;
const BROWSER_MAX_ZOOM: f32 = 12.0;
const BROWSER_WHEEL_DEBOUNCE: Duration = Duration::from_millis(24);
const BROWSER_WHEEL_PX_PER_ZOOM: f32 = 90.0;
const BROWSER_LINE_SCROLL_PX: f32 = 20.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewUniform {
    viewport: [f32; 2],
    center: [f32; 2],
    pixels_per_world: f32,
    palette_len: u32,
    grid_size: [u32; 2],
    frontlines_active: u32,
    output_is_srgb: u32,
    _padding: [u32; 2],
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
    dominant_texture: wgpu::Texture,
    unit_renderer: UnitRenderer,
    world_overlay: WorldOverlayRenderer,
    map_labels: MapLabelRenderer,
    observer_hud: ObserverHudRenderer,
}

fn texture_binding(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_binding(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
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
    dominant_sides: Vec<i16>,
    dominant_city_controlled: Vec<u8>,
    land: Vec<u8>,
    map_material: Option<MapMaterial>,
    palette: Vec<[f32; 4]>,
    occupation_palette: Vec<[f32; 4]>,
    metadata: Value,
    map_cities: Arc<[ProductionCity]>,
    country_names: HashMap<u16, String>,
    grid_width: u32,
    grid_height: u32,
    grid_res: f32,
    center: [f32; 2],
    zoom: f32,
    cursor: PhysicalPosition<f64>,
    dragging: bool,
    drag_origin: Option<PhysicalPosition<f64>>,
    last_drag: PhysicalPosition<f64>,
    wheel_delta: f32,
    wheel_started_at: Option<Instant>,
    wheel_cursor: PhysicalPosition<f64>,
    frame_count: u64,
    presented_frames: u64,
    smoke_frames: Option<u64>,
    smoke_started_at: Option<Instant>,
    runtime_worker: Option<RuntimeWorker>,
    runtime_center: Option<[f32; 2]>,
    runtime_zoom: Option<f32>,
    runtime_initial_tick: Option<u64>,
    runtime_terminal: bool,
    latest_snapshot: Option<Arc<RuntimeSnapshot>>,
    snapshot_dirty: bool,
    world_overlay_dirty: bool,
    map_labels_static_dirty: bool,
    map_labels_sides_dirty: bool,
    observer_hud_visible: bool,
    observer_hud_dirty: bool,
    selected_country_id: Option<u16>,
    playback_paused: bool,
    playback_speed_index: usize,
    playback_hovered: Option<PlaybackAction>,
    playback_pressed: Option<PlaybackAction>,
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
            dominant_sides: Vec::new(),
            dominant_city_controlled: Vec::new(),
            land: Vec::new(),
            map_material: None,
            palette: Vec::new(),
            occupation_palette: Vec::new(),
            metadata: Value::Null,
            map_cities: Arc::from([]),
            country_names: HashMap::new(),
            grid_width: 0,
            grid_height: 0,
            grid_res: 0.15,
            center: geographic_to_world(BROWSER_DEFAULT_CENTER[0], BROWSER_DEFAULT_CENTER[1]),
            zoom: pixels_per_world_for_zoom(BROWSER_DEFAULT_ZOOM),
            cursor: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            drag_origin: None,
            last_drag: PhysicalPosition::new(0.0, 0.0),
            wheel_delta: 0.0,
            wheel_started_at: None,
            wheel_cursor: PhysicalPosition::new(0.0, 0.0),
            frame_count: 0,
            presented_frames: 0,
            smoke_frames: options.smoke_frames,
            smoke_started_at: options.smoke_frames.map(|_| Instant::now()),
            runtime_worker: None,
            runtime_center: None,
            runtime_zoom: None,
            runtime_initial_tick: None,
            runtime_terminal: false,
            latest_snapshot: None,
            snapshot_dirty: false,
            world_overlay_dirty: false,
            map_labels_static_dirty: false,
            map_labels_sides_dirty: false,
            observer_hud_visible: true,
            observer_hud_dirty: true,
            selected_country_id: None,
            playback_paused: false,
            playback_speed_index: 0,
            playback_hovered: None,
            playback_pressed: None,
            territory_updates: VecDeque::new(),
            fps_epoch: Instant::now(),
            fps: 0.0,
            fatal_error: None,
        }
    }

    fn initialize(&mut self, window: Arc<Window>) -> Result<()> {
        let load_started = Instant::now();
        let checkpoint_path = self.runtime_checkpoint_path.clone();
        let (
            decoded,
            mut pending_runtime,
            demo_border,
            runtime_label,
            checkpoint_baseline,
            mut map_material,
        ) = if let Some(checkpoint_path) = checkpoint_path.as_ref() {
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
            let map_material = MapMaterial::from_scenario(&loaded.baseline);
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
                map_material,
            )
        } else {
            let target = GridSpec::world(0.15).context("invalid 0.15 degree target grid")?;
            let decoded = decode_mwsc_gzip_file(&self.scenario_path, Some(target))
                .with_context(|| format!("failed to decode {}", self.scenario_path.display()))?;
            let baseline = self.save_checkpoint_path.is_some().then(|| decoded.clone());
            let map_material = MapMaterial::from_scenario(&decoded);
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
                    map_material,
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
                let production = derive_scenario_production(&decoded, &ProductionConfig::default())
                    .context("failed to derive scenario production data for native runtime")?;
                let runtime = create_demo_runtime(border, &decoded, production)?;
                (
                    decoded,
                    Some(runtime),
                    Some(border),
                    Some("scenario-derived demo".to_owned()),
                    baseline,
                    map_material,
                )
            } else {
                (decoded, None, None, None, None, map_material)
            }
        };
        if let Some(runtime) = pending_runtime.as_ref() {
            map_material.set_sovereign_sides(runtime.country_to_side());
        }
        self.checkpoint_baseline = checkpoint_baseline;
        self.map_material = Some(map_material);
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

        let map_production = if let Some(runtime) = pending_runtime.as_ref() {
            runtime.scenario().clone()
        } else {
            derive_scenario_production(&decoded, &ProductionConfig::default())
                .context("failed to derive scenario cities and country labels")?
        };
        self.map_cities = Arc::clone(&map_production.cities);
        self.country_names = map_production
            .countries
            .iter()
            .map(|country| (country.country_id, country.name.clone()))
            .collect();

        self.dominant_sides = vec![-1; decoded.world_control.len()];
        self.dominant_city_controlled = vec![0; decoded.world_control.len()];
        self.ownership = decoded.world_control;
        self.land = decoded.land;
        self.metadata = decoded.metadata;
        (self.palette, self.occupation_palette) = build_palettes(&self.metadata, &self.ownership);
        let size = window.inner_size();
        self.zoom = reset_zoom(size);
        if let Some(runtime) = pending_runtime.as_mut() {
            let published = runtime.latest_snapshot();
            // Apply the tick-zero full replacement before creating/presenting the GPU texture.
            while let Some(update) = runtime.pop_render_update() {
                apply_territory_update_to_grid(
                    &mut self.ownership,
                    &mut self.dominant_sides,
                    &mut self.dominant_city_controlled,
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
            if self.smoke_frames.is_some() && self.selected_country_id.is_none() {
                self.selected_country_id = published
                    .territory_snapshot
                    .countries
                    .first()
                    .map(|country| country.country_id);
            }
            self.latest_snapshot = Some(Arc::clone(&published));
            self.snapshot_dirty = true;
            self.world_overlay_dirty = true;
            self.observer_hud_dirty = true;
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

        let dominant_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dominant side R16Sint"),
            size: ownership_texture.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Sint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let dominant_view = dominant_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut dominant_padded = vec![0_u8; padded_bpr * self.grid_height as usize];
        for (row, source) in self
            .dominant_sides
            .chunks_exact(self.grid_width as usize)
            .enumerate()
        {
            let bytes = bytemuck::cast_slice(source);
            dominant_padded[row * padded_bpr..row * padded_bpr + unpadded_bpr]
                .copy_from_slice(bytes);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &dominant_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &dominant_padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr as u32),
                rows_per_image: Some(self.grid_height),
            },
            dominant_texture.size(),
        );

        let material = self
            .map_material
            .as_ref()
            .context("native map material was not initialized")?;
        let geographic_size = ownership_texture.size();
        let sovereign_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("immutable sovereign ownership R16Uint"),
            size: geographic_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        upload_full_texture(
            &queue,
            &sovereign_texture,
            &material.sovereign,
            self.grid_width,
            self.grid_height,
        )?;
        let geographic_land_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("immutable geographic land R8Uint"),
            size: geographic_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        upload_full_texture(
            &queue,
            &geographic_land_texture,
            &material.land,
            self.grid_width,
            self.grid_height,
        )?;
        let biome_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("immutable biome R8Uint"),
            size: geographic_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        upload_full_texture(
            &queue,
            &biome_texture,
            &material.biome,
            self.grid_width,
            self.grid_height,
        )?;
        let sovereign_view = sovereign_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let geographic_land_view =
            geographic_land_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let biome_view = biome_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut sovereign_sides = vec![-1_i32; self.palette.len()];
        let copy_len = sovereign_sides.len().min(material.sovereign_sides.len());
        sovereign_sides[..copy_len].copy_from_slice(&material.sovereign_sides[..copy_len]);
        let sovereign_side_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("immutable sovereign sides"),
            contents: bytemuck::cast_slice(&sovereign_sides),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let mut country_y_bounds = vec![[0_u32, 0_u32]; self.palette.len()];
        let bounds_len = country_y_bounds.len().min(material.country_y_bounds.len());
        country_y_bounds[..bounds_len].copy_from_slice(&material.country_y_bounds[..bounds_len]);
        let country_bounds_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("immutable sovereign vertical bounds"),
            contents: bytemuck::cast_slice(&country_y_bounds),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let view_uniform = self.view_uniform(size, self.palette.len() as u32, format.is_srgb());
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
        let occupation_palette_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("raw country occupation palette"),
                contents: bytemuck::cast_slice(&self.occupation_palette),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Sint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                texture_binding(4, wgpu::TextureSampleType::Uint),
                texture_binding(5, wgpu::TextureSampleType::Uint),
                texture_binding(6, wgpu::TextureSampleType::Uint),
                storage_binding(7),
                storage_binding(8),
                storage_binding(9),
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
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&dominant_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&sovereign_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&geographic_land_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&biome_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: sovereign_side_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: country_bounds_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: occupation_palette_buffer.as_entire_binding(),
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
        let mut unit_renderer =
            UnitRenderer::new(&device, &queue, &view_buffer, format, &self.metadata);
        if let Some(snapshot) = &self.latest_snapshot {
            unit_renderer.upload(&device, &queue, &snapshot.frame_snapshot);
        }
        let mut world_overlay = WorldOverlayRenderer::new(&device, &view_buffer, format);
        if let Some(snapshot) = &self.latest_snapshot {
            world_overlay.upload(&device, &queue, snapshot, self.selected_runtime_side());
        }
        self.world_overlay_dirty = false;
        let mut map_labels = MapLabelRenderer::new(&device, &view_buffer, format);
        let map_label_view = self.map_label_view(size);
        map_labels.upload_static(
            &device,
            &queue,
            map_label_view,
            [self.grid_width as usize, self.grid_height as usize],
            &self.ownership,
            &self.dominant_sides,
            &self.dominant_city_controlled,
            &self.map_cities,
            &self.country_names,
            true,
        );
        map_labels.upload_sides(
            &device,
            &queue,
            map_label_view,
            self.latest_snapshot
                .as_ref()
                .map(|snapshot| snapshot.frame_snapshot.as_ref()),
        );
        map_labels.upload_unit_adornments(
            &device,
            &queue,
            map_label_view,
            self.latest_snapshot
                .as_ref()
                .map(|snapshot| snapshot.frame_snapshot.as_ref()),
        );
        self.map_labels_static_dirty = false;
        self.map_labels_sides_dirty = false;
        let mut observer_hud = ObserverHudRenderer::new(&device, format);
        let observer_model = self.observer_hud_model();
        observer_hud.upload(
            &device,
            &queue,
            size,
            ObserverHudUpload {
                lines: &observer_model.lines,
                accent: self.observer_hud_accent(),
                playback: self.playback_presentation(),
                show_observer: self.observer_hud_visible,
            },
        );
        self.observer_hud_dirty = false;
        log::info!(
            "world overlays initialized with {} units, {} national flags, {} unit adornments, {} markers, {} route segments, {} cities, {} side labels, and {} country labels",
            unit_renderer.instance_count(),
            unit_renderer.flag_count(),
            map_labels.layout().unit_adornments,
            world_overlay.marker_count(),
            world_overlay.segment_count(),
            map_labels.layout().city_markers,
            map_labels.layout().side_labels,
            map_labels.layout().country_labels,
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
            dominant_texture,
            unit_renderer,
            world_overlay,
            map_labels,
            observer_hud,
        });
        // Geographic material is now immutable GPU state; retaining a second
        // CPU copy would only increase the viewer's baseline memory use.
        self.map_material = None;
        self.occupation_palette.clear();
        self.occupation_palette.shrink_to_fit();
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
            self.observer_hud_dirty = true;
        }
        Ok(())
    }

    fn view_uniform(
        &self,
        size: PhysicalSize<u32>,
        palette_len: u32,
        output_is_srgb: bool,
    ) -> ViewUniform {
        let frontlines_active = self.latest_snapshot.as_ref().is_some_and(|snapshot| {
            matches!(
                snapshot.state,
                RuntimeState::Running | RuntimeState::AwaitingStrategicEffects { .. }
            )
        });
        ViewUniform {
            viewport: [size.width.max(1) as f32, size.height.max(1) as f32],
            center: self.center,
            pixels_per_world: self.zoom,
            palette_len,
            grid_size: [self.grid_width, self.grid_height],
            frontlines_active: u32::from(frontlines_active),
            output_is_srgb: u32::from(output_is_srgb),
            _padding: [0; 2],
        }
    }

    fn map_label_view(&self, size: PhysicalSize<u32>) -> MapLabelView {
        MapLabelView {
            viewport: [size.width.max(1) as f32, size.height.max(1) as f32],
            center: self.center,
            pixels_per_world: self.zoom,
        }
    }

    fn invalidate_map_labels(&mut self) {
        self.map_labels_static_dirty = true;
        self.map_labels_sides_dirty = true;
    }

    fn reset_camera(&mut self, size: PhysicalSize<u32>) {
        if let Some(center) = self.runtime_center {
            self.center = center;
            self.zoom = self.runtime_zoom.unwrap_or_else(|| reset_zoom(size));
        } else {
            self.center = geographic_to_world(BROWSER_DEFAULT_CENTER[0], BROWSER_DEFAULT_CENTER[1]);
            self.zoom = reset_zoom(size);
        }
        self.center = world_copy_jump(self.center);
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
        let writer = if state.strategic_missiles.is_some()
            && state.material_logistics.is_some()
            && state.reinforcement.is_some()
            && state.naval_planning.is_some()
        {
            write_runtime_checkpoint_state_v12
        } else if state.material_logistics.is_some()
            && state.reinforcement.is_some()
            && state.naval_planning.is_some()
        {
            write_runtime_checkpoint_state_v11
        } else if state.reinforcement.is_some() && state.naval_planning.is_some() {
            write_runtime_checkpoint_state_v10
        } else if state.naval_planning.is_some() {
            write_runtime_checkpoint_state_v9
        } else if state.operational_execution.is_some() && state.air_power.is_some() {
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
        let mut control_events = Vec::new();
        while let Some(event) = worker.poll_control_event() {
            control_events.push(event);
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
                &mut self.dominant_sides,
                &mut self.dominant_city_controlled,
                self.grid_width as usize,
                self.grid_height as usize,
                &update,
            )?;
            self.map_labels_static_dirty = true;
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
            self.latest_snapshot = Some(snapshot);
            self.snapshot_dirty = true;
            self.world_overlay_dirty = true;
            self.map_labels_sides_dirty = true;
            self.observer_hud_dirty = true;
        }
        for event in control_events {
            match event {
                RuntimeWorkerControlEvent::Paused { tick, frame } => {
                    log::debug!("runtime paused at tick {tick}, frame {frame}");
                }
                RuntimeWorkerControlEvent::Resumed { tick, frame } => {
                    log::debug!("runtime resumed at tick {tick}, frame {frame}");
                }
                RuntimeWorkerControlEvent::TickIntervalChanged {
                    interval,
                    tick,
                    frame,
                } => {
                    log::debug!(
                        "runtime tick interval changed to {interval:?} at tick {tick}, frame {frame}"
                    );
                }
                RuntimeWorkerControlEvent::Unavailable { tick, frame } => {
                    self.runtime_terminal = true;
                    self.playback_hovered = None;
                    self.playback_pressed = None;
                    self.observer_hud_dirty = true;
                    log::debug!(
                        "runtime rejected playback control after terminal tick {tick}, frame {frame}"
                    );
                }
            }
        }
        for status in statuses {
            match status {
                RuntimeWorkerStatus::Stopped => {
                    self.runtime_terminal = true;
                    self.playback_hovered = None;
                    self.playback_pressed = None;
                    self.observer_hud_dirty = true;
                    log::info!("native runtime worker stopped");
                }
                RuntimeWorkerStatus::Terminal(state) => {
                    self.runtime_terminal = true;
                    self.playback_hovered = None;
                    self.playback_pressed = None;
                    self.observer_hud_dirty = true;
                    log::warn!("native runtime reached terminal state: {state:?}");
                }
                RuntimeWorkerStatus::Completed { steps } => {
                    self.runtime_terminal = true;
                    self.playback_hovered = None;
                    self.playback_pressed = None;
                    self.observer_hud_dirty = true;
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

    fn zoom_at(&mut self, cursor: PhysicalPosition<f64>, delta_zoom: f32) {
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
        let before = self.world_at(cursor, size);
        let zoom = (browser_zoom(self.zoom) + delta_zoom).clamp(BROWSER_MIN_ZOOM, BROWSER_MAX_ZOOM);
        self.zoom = pixels_per_world_for_zoom(zoom);
        let after = self.world_at(cursor, size);
        self.center[0] += (before[0] - after[0]) as f32;
        self.center[1] += (before[1] - after[1]) as f32;
        self.center = world_copy_jump(self.center);
    }

    fn queue_wheel_zoom(&mut self, delta: f32) {
        self.wheel_delta += delta;
        self.wheel_cursor = self.cursor;
        self.wheel_started_at.get_or_insert_with(Instant::now);
    }

    fn flush_wheel_zoom(&mut self) -> bool {
        let Some(started_at) = self.wheel_started_at else {
            return false;
        };
        if started_at.elapsed() < BROWSER_WHEEL_DEBOUNCE {
            return false;
        }
        let delta = std::mem::take(&mut self.wheel_delta);
        self.wheel_started_at = None;
        let delta_zoom = leaflet_wheel_zoom_delta(delta);
        if delta_zoom == 0.0 {
            return false;
        }
        self.zoom_at(self.wheel_cursor, delta_zoom);
        self.invalidate_map_labels();
        true
    }

    fn country_at_cursor(&self) -> Option<(u16, f64, f64)> {
        let window = self.window.as_ref()?;
        let world = self.world_at(self.cursor, window.inner_size());
        let (x, y) = world_to_cell(world, self.grid_width, self.grid_height)?;
        let id = *self.ownership.get(y * self.grid_width as usize + x)?;
        let [lat, lng] = world_to_geographic(world);
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

    fn observer_hud_model(&self) -> ObserverHudModel {
        let selected = self.selected_country_id;
        let country_name = selected.map_or("", |country_id| self.country_name(country_id));
        self.latest_snapshot.as_ref().map_or_else(
            || {
                ObserverHudModel::without_runtime(
                    selected.map(|country_id| (country_id, country_name)),
                )
            },
            |snapshot| {
                ObserverHudModel::from_runtime(
                    snapshot,
                    selected,
                    country_name,
                    self.save_checkpoint_path.is_some(),
                )
            },
        )
    }

    fn observer_hud_accent(&self) -> [f32; 4] {
        self.selected_country_id
            .and_then(|country_id| self.palette.get(country_id as usize).copied())
            .unwrap_or([0.15, 0.72, 0.95, 1.0])
    }

    fn selected_runtime_side(&self) -> Option<usize> {
        let country_id = self.selected_country_id?;
        let snapshot = self.latest_snapshot.as_ref()?;
        snapshot
            .territory_snapshot
            .countries
            .iter()
            .find(|country| country.country_id == country_id)
            .and_then(|country| usize::try_from(country.side_index).ok())
            .or_else(|| {
                snapshot
                    .frame_snapshot
                    .units
                    .iter()
                    .find(|unit| unit.sovereign == u64::from(country_id))
                    .map(|unit| usize::from(unit.side))
            })
    }

    fn playback_active(&self) -> bool {
        self.latest_snapshot.is_some() && !self.runtime_terminal
    }

    fn playback_speed(&self) -> u8 {
        PLAYBACK_SPEEDS[self.playback_speed_index.min(PLAYBACK_SPEEDS.len() - 1)]
    }

    fn playback_presentation(&self) -> Option<PlaybackPresentation> {
        self.playback_active().then(|| PlaybackPresentation {
            paused: self.playback_paused,
            speed: self.playback_speed(),
            unit_count: self
                .latest_snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.frame_snapshot.units.len()),
            hovered: self.playback_hovered,
            pressed: self.playback_pressed,
        })
    }

    fn current_hud_hit(&self, point: PhysicalPosition<f64>, size: PhysicalSize<u32>) -> HudHit {
        let line_count = self.observer_hud_model().lines.len();
        let layout = hud_layout(
            size,
            line_count,
            self.playback_active(),
            self.observer_hud_visible,
        );
        hud_hit_test(point, layout)
    }

    fn apply_playback_action(&mut self, action: PlaybackAction) -> Result<()> {
        if !self.playback_active() {
            return Ok(());
        }
        match action {
            PlaybackAction::TogglePause => {
                let paused = !self.playback_paused;
                let worker = self
                    .runtime_worker
                    .as_ref()
                    .context("playback control requested without an active runtime worker")?;
                if paused {
                    worker.request_pause()?;
                } else {
                    worker.request_resume()?;
                }
                self.playback_paused = paused;
            }
            PlaybackAction::SpeedDown | PlaybackAction::CycleSpeed | PlaybackAction::SpeedUp => {
                let speed_index = playback_speed_index(self.playback_speed_index, action);
                let speed = PLAYBACK_SPEEDS[speed_index];
                let interval = self.runtime_tick_interval.div_f64(f64::from(speed));
                let worker = self
                    .runtime_worker
                    .as_ref()
                    .context("playback control requested without an active runtime worker")?;
                worker.request_tick_interval(interval)?;
                self.playback_speed_index = speed_index;
            }
        }
        self.observer_hud_dirty = true;
        Ok(())
    }

    fn smoke_runtime_ready(&self) -> bool {
        self.runtime_initial_tick.is_none()
            || self.runtime_terminal
            || self
                .latest_snapshot
                .as_ref()
                .is_some_and(|snapshot| Some(snapshot.tick) > self.runtime_initial_tick)
    }

    fn smoke_timed_out(&self) -> bool {
        self.smoke_started_at
            .is_some_and(|started| started.elapsed() >= SMOKE_TIMEOUT)
    }

    fn render(&mut self) -> Result<()> {
        self.drain_runtime_worker()?;
        let Some(window) = &self.window else {
            return Ok(());
        };
        let size = window.inner_size();
        let palette_len = self.palette.len() as u32;
        let output_is_srgb = self
            .gpu
            .as_ref()
            .is_some_and(|gpu| gpu.config.format.is_srgb());
        let uniform = self.view_uniform(size, palette_len, output_is_srgb);
        let map_label_view = self.map_label_view(size);
        let map_labels_static_dirty = self.map_labels_static_dirty;
        let map_labels_sides_dirty = self.map_labels_sides_dirty;
        let observer_hud_visible = self.observer_hud_visible;
        let world_overlay_upload = self.world_overlay_dirty.then(|| {
            self.latest_snapshot
                .as_ref()
                .map(|snapshot| (Arc::clone(snapshot), self.selected_runtime_side()))
        });
        let observer_upload = self.observer_hud_dirty.then(|| {
            (
                self.observer_hud_model(),
                self.observer_hud_accent(),
                self.playback_presentation(),
            )
        });
        let Some(gpu) = &mut self.gpu else {
            return Ok(());
        };
        gpu.queue
            .write_buffer(&gpu.view_buffer, 0, bytemuck::bytes_of(&uniform));
        while let Some(update) = self.territory_updates.pop_front() {
            upload_territory_update(
                &gpu.queue,
                &gpu.ownership_texture,
                &gpu.dominant_texture,
                &update,
            )
            .inspect_err(|_| self.territory_updates.push_front(Arc::clone(&update)))?;
        }
        if self.snapshot_dirty
            && let Some(snapshot) = &self.latest_snapshot
        {
            gpu.unit_renderer
                .upload(&gpu.device, &gpu.queue, &snapshot.frame_snapshot);
            self.snapshot_dirty = false;
        }
        if let Some(Some((snapshot, selected_side))) = world_overlay_upload {
            gpu.world_overlay
                .upload(&gpu.device, &gpu.queue, &snapshot, selected_side);
        }
        self.world_overlay_dirty = false;
        if map_labels_static_dirty {
            gpu.map_labels.upload_static(
                &gpu.device,
                &gpu.queue,
                map_label_view,
                [self.grid_width as usize, self.grid_height as usize],
                &self.ownership,
                &self.dominant_sides,
                &self.dominant_city_controlled,
                &self.map_cities,
                &self.country_names,
                true,
            );
            self.map_labels_static_dirty = false;
        }
        if map_labels_sides_dirty {
            gpu.map_labels.upload_sides(
                &gpu.device,
                &gpu.queue,
                map_label_view,
                self.latest_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.frame_snapshot.as_ref()),
            );
            gpu.map_labels.upload_unit_adornments(
                &gpu.device,
                &gpu.queue,
                map_label_view,
                self.latest_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.frame_snapshot.as_ref()),
            );
            self.map_labels_sides_dirty = false;
        }
        if let Some((model, accent, playback)) = observer_upload {
            gpu.observer_hud.upload(
                &gpu.device,
                &gpu.queue,
                size,
                ObserverHudUpload {
                    lines: &model.lines,
                    accent,
                    playback,
                    show_observer: observer_hud_visible,
                },
            );
            self.observer_hud_dirty = false;
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
            gpu.world_overlay.draw_missiles(&mut pass);
            gpu.world_overlay.draw_air(&mut pass);
            gpu.map_labels.draw_cities(&mut pass);
            gpu.unit_renderer.draw(&mut pass);
            gpu.map_labels.draw_unit_adornments(&mut pass);
            gpu.world_overlay.draw_battles(&mut pass);
            gpu.map_labels.draw_side_labels(&mut pass);
            gpu.map_labels.draw_country_labels(&mut pass);
            gpu.world_overlay.draw_operations(&mut pass);
            gpu.observer_hud.draw(&mut pass);
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
                browser_zoom(self.zoom),
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
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) => event_loop.exit(),
                    PhysicalKey::Code(KeyCode::KeyR) => {
                        self.reset_camera(window.inner_size());
                        self.invalidate_map_labels();
                        window.request_redraw();
                    }
                    PhysicalKey::Code(KeyCode::KeyH) => {
                        self.observer_hud_visible = !self.observer_hud_visible;
                        self.observer_hud_dirty = true;
                        self.dragging = false;
                        self.drag_origin = None;
                        self.playback_pressed = None;
                        window.request_redraw();
                    }
                    PhysicalKey::Code(KeyCode::Space) => {
                        if let Err(error) = self.apply_playback_action(PlaybackAction::TogglePause)
                        {
                            let message = format!("playback control failed: {error:#}");
                            log::error!("{message}");
                            self.fatal_error = Some(message);
                            event_loop.exit();
                        }
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
                self.observer_hud_dirty = true;
                self.invalidate_map_labels();
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.observer_hud_dirty = true;
                self.invalidate_map_labels();
                window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                let hovered = match self.current_hud_hit(position, window.inner_size()) {
                    HudHit::Playback(action) => Some(action),
                    HudHit::Panel | HudHit::Outside => None,
                };
                if hovered != self.playback_hovered {
                    self.playback_hovered = hovered;
                    self.observer_hud_dirty = true;
                    window.request_redraw();
                }
                if self.dragging {
                    self.center[0] -= (position.x - self.last_drag.x) as f32 / self.zoom;
                    self.center[1] -= (position.y - self.last_drag.y) as f32 / self.zoom;
                    self.center = world_copy_jump(self.center);
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
                    match self.current_hud_hit(self.cursor, window.inner_size()) {
                        HudHit::Playback(action) => {
                            self.playback_pressed = Some(action);
                            self.observer_hud_dirty = true;
                            self.dragging = false;
                            self.drag_origin = None;
                            window.request_redraw();
                        }
                        HudHit::Panel => {
                            self.playback_pressed = None;
                            self.dragging = false;
                            self.drag_origin = None;
                        }
                        HudHit::Outside => {
                            self.playback_pressed = None;
                            self.dragging = true;
                            self.drag_origin = Some(self.cursor);
                            self.last_drag = self.cursor;
                        }
                    }
                } else {
                    let was_dragging = self.dragging;
                    self.dragging = false;
                    let released_action =
                        match self.current_hud_hit(self.cursor, window.inner_size()) {
                            HudHit::Playback(action) => Some(action),
                            HudHit::Panel | HudHit::Outside => None,
                        };
                    let pressed_action = self.playback_pressed.take();
                    if let Some(action) = pressed_action {
                        self.observer_hud_dirty = true;
                        if Some(action) == released_action
                            && let Err(error) = self.apply_playback_action(action)
                        {
                            let message = format!("playback control failed: {error:#}");
                            log::error!("{message}");
                            self.fatal_error = Some(message);
                            event_loop.exit();
                        }
                        window.request_redraw();
                        return;
                    }
                    if was_dragging {
                        self.invalidate_map_labels();
                    }
                    let clicked = self.drag_origin.take().is_some_and(|origin| {
                        let dx = self.cursor.x - origin.x;
                        let dy = self.cursor.y - origin.y;
                        dx * dx + dy * dy <= 16.0
                    });
                    if clicked && let Some((id, lat, lng)) = self.country_at_cursor() {
                        self.selected_country_id = (id != 0).then_some(id);
                        self.observer_hud_dirty = true;
                        self.world_overlay_dirty = true;
                        println!(
                            "country={id} name={:?} lat={lat:.4} lng={lng:.4}",
                            self.country_name(id)
                        );
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.current_hud_hit(self.cursor, window.inner_size()) != HudHit::Outside {
                    return;
                }
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * BROWSER_LINE_SCROLL_PX,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                self.queue_wheel_zoom(amount);
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.render() {
                    let message = format!("rendering failed: {error:#}");
                    log::error!("{message}");
                    self.fatal_error = Some(message);
                    event_loop.exit();
                    return;
                }
                if let Some(target) = self.smoke_frames
                    && self.presented_frames >= target
                    && self.smoke_runtime_ready()
                {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.flush_wheel_zoom()
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
        if let Some(target) = self.smoke_frames {
            // An occluded Wayland surface may coalesce compositor-driven redraw
            // callbacks after the first present. Exercise the requested GPU
            // frames directly at the event-loop boundary so `--smoke` remains
            // a bounded renderer test even when its window cannot be focused.
            let result = if self.presented_frames < target {
                self.render()
            } else {
                self.drain_runtime_worker()
            };
            if let Err(error) = result {
                let message = format!("smoke rendering failed: {error:#}");
                log::error!("{message}");
                self.fatal_error = Some(message);
                event_loop.exit();
            } else if self.presented_frames >= target && self.smoke_runtime_ready() {
                event_loop.exit();
            } else if self.smoke_timed_out() {
                let message = format!(
                    "smoke rendering timed out after {:.1}s with {}/{} frames presented",
                    SMOKE_TIMEOUT.as_secs_f64(),
                    self.presented_frames,
                    target
                );
                log::error!("{message}");
                self.fatal_error = Some(message);
                event_loop.exit();
            }
        } else if let Some(window) = &self.window {
            window.request_redraw();
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

fn playback_speed_index(current: usize, action: PlaybackAction) -> usize {
    let current = current.min(PLAYBACK_SPEEDS.len() - 1);
    match action {
        PlaybackAction::TogglePause => current,
        PlaybackAction::SpeedDown => current.saturating_sub(1),
        PlaybackAction::CycleSpeed => (current + 1) % PLAYBACK_SPEEDS.len(),
        PlaybackAction::SpeedUp => (current + 1).min(PLAYBACK_SPEEDS.len() - 1),
    }
}

fn reset_zoom(_size: PhysicalSize<u32>) -> f32 {
    pixels_per_world_for_zoom(BROWSER_DEFAULT_ZOOM)
}

fn demo_zoom(size: PhysicalSize<u32>) -> f32 {
    (reset_zoom(size) * DEMO_CAMERA_ZOOM_MULTIPLIER).clamp(
        pixels_per_world_for_zoom(BROWSER_MIN_ZOOM),
        pixels_per_world_for_zoom(BROWSER_MAX_ZOOM),
    )
}

fn world_copy_jump(center: [f32; 2]) -> [f32; 2] {
    [center[0].rem_euclid(2.0), center[1]]
}

fn leaflet_wheel_zoom_delta(delta: f32) -> f32 {
    if delta == 0.0 || !delta.is_finite() {
        return 0.0;
    }
    let scaled = delta.abs() / (BROWSER_WHEEL_PX_PER_ZOOM * 4.0);
    let sigmoid = 4.0 * (2.0 / (1.0 + (-scaled).exp())).ln() / std::f32::consts::LN_2;
    sigmoid.copysign(delta)
}

fn camera_for_runtime(snapshot: &FrameSnapshot, size: PhysicalSize<u32>) -> ([f32; 2], f32) {
    if snapshot.units.is_empty() {
        let center = geographic_to_world(BROWSER_DEFAULT_CENTER[0], BROWSER_DEFAULT_CENTER[1]);
        let zoom = reset_zoom(size);
        return (world_copy_jump(center), zoom);
    }
    // Match Leaflet fitBounds by fitting the literal longitude interval. The
    // renderer repeats that fitted map horizontally when the camera crosses a
    // world edge.
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
    // Browser startWar() calls fitBounds(bounds.pad(0.2)), expanding each
    // dimension by forty percent before Leaflet selects a zoom.
    let padding = 1.4_f64;
    let horizontal_zoom = size.width.max(1) as f64 / (horizontal_span * padding);
    let vertical_zoom = size.height.max(1) as f64 / (vertical_span * padding);
    let zoom = horizontal_zoom.min(vertical_zoom).clamp(
        f64::from(pixels_per_world_for_zoom(BROWSER_MIN_ZOOM)),
        f64::from(pixels_per_world_for_zoom(BROWSER_MAX_ZOOM)),
    ) as f32;
    let center = [
        ((min_x + max_x) * 0.5) as f32,
        ((min_y + max_y) * 0.5) as f32,
    ];
    (world_copy_jump(center), zoom)
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
            gameplay_rng: mw_core::GameplayRngState {
                state: mw_core::DEFAULT_GAMEPLAY_RNG_SEED,
            },
            personnel_reserves: BTreeMap::from([(0, 0.0), (1, 0.0)]),
            side_dynamics: None,
            operations: None,
            operational_execution: None,
            air_power: None,
            naval_planning: None,
            reinforcement: None,
            material_logistics: None,
            strategic_missiles: None,
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
    dominant_sides: &mut [i16],
    dominant_city_controlled: &mut [u8],
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
    anyhow::ensure!(
        dominant_sides.len() == expected_cells,
        "dominant-side grid has {} cells, expected {expected_cells}",
        dominant_sides.len()
    );
    anyhow::ensure!(
        dominant_city_controlled.len() == expected_cells,
        "dominant-city-controlled grid has {} cells, expected {expected_cells}",
        dominant_city_controlled.len()
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
        anyhow::ensure!(
            tile.dominant_sides.len() == expected_pixels,
            "territory tile {} has {} dominant sides, expected {expected_pixels}",
            bounds.tile,
            tile.dominant_sides.len()
        );
        anyhow::ensure!(
            tile.dominant_city_controlled.len() == expected_pixels,
            "territory tile {} has {} dominant city-control flags, expected {expected_pixels}",
            bounds.tile,
            tile.dominant_city_controlled.len()
        );
        for row in 0..tile_height {
            let source_start = row * tile_width;
            let target_start = (bounds.min_y + row) * width + bounds.min_x;
            ownership[target_start..target_start + tile_width]
                .copy_from_slice(&tile.pixels[source_start..source_start + tile_width]);
            dominant_sides[target_start..target_start + tile_width]
                .copy_from_slice(&tile.dominant_sides[source_start..source_start + tile_width]);
            dominant_city_controlled[target_start..target_start + tile_width].copy_from_slice(
                &tile.dominant_city_controlled[source_start..source_start + tile_width],
            );
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

fn pack_full_texture<T: Pod>(pixels: &[T], width: u32, height: u32) -> Result<(Vec<u8>, u32)> {
    let expected = width as usize * height as usize;
    anyhow::ensure!(
        pixels.len() == expected,
        "texture has {} pixels, expected {expected}",
        pixels.len()
    );
    let unpadded_bpr = width as usize * std::mem::size_of::<T>();
    let padded_bpr = unpadded_bpr.div_ceil(ROW_ALIGNMENT) * ROW_ALIGNMENT;
    let source = bytemuck::cast_slice(pixels);
    let mut packed = vec![0_u8; padded_bpr * height as usize];
    for row in 0..height as usize {
        let source_start = row * unpadded_bpr;
        let target_start = row * padded_bpr;
        packed[target_start..target_start + unpadded_bpr]
            .copy_from_slice(&source[source_start..source_start + unpadded_bpr]);
    }
    Ok((packed, padded_bpr as u32))
}

fn upload_full_texture<T: Pod>(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    pixels: &[T],
    width: u32,
    height: u32,
) -> Result<()> {
    anyhow::ensure!(texture.size().width == width && texture.size().height == height);
    let (packed, padded_bpr) = pack_full_texture(pixels, width, height)?;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &packed,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded_bpr),
            rows_per_image: Some(height),
        },
        texture.size(),
    );
    Ok(())
}

fn pack_dominant_tile(tile: &TerritoryTilePixels) -> Result<(Vec<u8>, u32, u32)> {
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
        tile.dominant_sides.len() == expected_pixels,
        "territory tile {} has {} dominant sides, expected {expected_pixels}",
        bounds.tile,
        tile.dominant_sides.len()
    );
    let unpadded_bytes_per_row = width
        .checked_mul(std::mem::size_of::<i16>())
        .context("dominant-side tile row size overflow")?;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(ROW_ALIGNMENT) * ROW_ALIGNMENT;
    let mut packed = vec![0_u8; padded_bytes_per_row * height];
    for row in 0..height {
        let source = &tile.dominant_sides[row * width..(row + 1) * width];
        let bytes = bytemuck::cast_slice(source);
        let target_start = row * padded_bytes_per_row;
        packed[target_start..target_start + unpadded_bytes_per_row].copy_from_slice(bytes);
    }
    Ok((
        packed,
        u32::try_from(padded_bytes_per_row).context("dominant-side tile row exceeds GPU limits")?,
        u32::try_from(height).context("dominant-side tile height exceeds GPU limits")?,
    ))
}

fn upload_territory_update(
    queue: &wgpu::Queue,
    ownership_texture: &wgpu::Texture,
    dominant_texture: &wgpu::Texture,
    update: &TerritoryRenderUpdate,
) -> Result<()> {
    let texture_size = ownership_texture.size();
    anyhow::ensure!(
        dominant_texture.size() == texture_size,
        "ownership and dominant-side textures differ in size"
    );
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
                texture: ownership_texture,
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
        let (dominant_packed, dominant_bytes_per_row, dominant_rows_per_image) =
            pack_dominant_tile(tile)?;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: dominant_texture,
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
            &dominant_packed,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(dominant_bytes_per_row),
                rows_per_image: Some(dominant_rows_per_image),
            },
            wgpu::Extent3d {
                width: u32::try_from(bounds.max_x - bounds.min_x)
                    .context("territory tile width exceeds GPU limits")?,
                height: dominant_rows_per_image,
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
    world_to_grid(world, width as usize, height as usize)
}

fn build_palettes(metadata: &Value, ownership: &[u16]) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
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
    let mut overlord_by_id = Vec::new();
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
        if id >= palette.len() {
            palette.resize(id + 1, [0.59, 0.59, 0.59, 1.0]);
        }
        palette[id] = color;
        if let Some(overlord_id) = country
            .get("overlordId")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        {
            if id >= overlord_by_id.len() {
                overlord_by_id.resize(id + 1, None);
            }
            overlord_by_id[id] = Some(overlord_id);
        }
    }
    let unblended_palette = palette.clone();
    for (subject_id, overlord_id) in overlord_by_id.into_iter().enumerate() {
        let Some(overlord_id) = overlord_id else {
            continue;
        };
        if subject_id < palette.len() && overlord_id < palette.len() {
            let subject = unblended_palette[subject_id];
            let overlord = unblended_palette[overlord_id];
            palette[subject_id] = [
                (overlord[0] * 0.75 * 255.0 + subject[0] * 0.25 * 255.0).round() / 255.0,
                (overlord[1] * 0.75 * 255.0 + subject[1] * 0.25 * 255.0).round() / 255.0,
                (overlord[2] * 0.75 * 255.0 + subject[2] * 0.25 * 255.0).round() / 255.0,
                subject[3],
            ];
        }
    }
    (palette, unblended_palette)
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
    if let Some(body) = value
        .strip_prefix("hsla(")
        .or_else(|| value.strip_prefix("hsl("))
        .and_then(|body| body.strip_suffix(')'))
    {
        let parts = body.split(',').map(str::trim).collect::<Vec<_>>();
        if parts.len() < 3 {
            return None;
        }
        let hue = parts[0].strip_suffix("deg").unwrap_or(parts[0]);
        let saturation = parts[1].strip_suffix('%')?;
        let lightness = parts[2].strip_suffix('%')?;
        let hue = hue.parse::<f32>().ok()?.rem_euclid(360.0) / 60.0;
        let saturation = (saturation.parse::<f32>().ok()? / 100.0).clamp(0.0, 1.0);
        let lightness = (lightness.parse::<f32>().ok()? / 100.0).clamp(0.0, 1.0);
        let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
        let secondary = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
        let (red, green, blue) = match hue as u32 {
            0 => (chroma, secondary, 0.0),
            1 => (secondary, chroma, 0.0),
            2 => (0.0, chroma, secondary),
            3 => (0.0, secondary, chroma),
            4 => (secondary, 0.0, chroma),
            _ => (chroma, 0.0, secondary),
        };
        let match_value = lightness - chroma * 0.5;
        let byte_channel = |channel: f32| ((channel + match_value) * 255.0).round() / 255.0;
        return Some([
            byte_channel(red),
            byte_channel(green),
            byte_channel(blue),
            parts
                .get(3)
                .and_then(|alpha| alpha.parse::<f32>().ok())
                .unwrap_or(1.0)
                .clamp(0.0, 1.0)
                .max(0.65),
        ]);
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
    fn reset_camera_matches_browser_default_zoom() {
        assert_eq!(reset_zoom(PhysicalSize::new(1_280, 720)), 1_024.0);
        assert_eq!(reset_zoom(PhysicalSize::new(2_000, 600)), 1_024.0);
        assert_eq!(browser_zoom(reset_zoom(PhysicalSize::new(1, 1))), 3.0);
        assert_eq!(
            geographic_to_world(BROWSER_DEFAULT_CENTER[0], BROWSER_DEFAULT_CENTER[1]),
            geographic_to_world(20.0, 0.0)
        );
    }

    #[test]
    fn camera_world_copy_jump_wraps_longitude_only() {
        assert_eq!(
            world_copy_jump([-0.25, -3.0]),
            [1.75, -3.0],
            "Leaflet worldCopyJump preserves latitude while wrapping longitude"
        );
        assert_eq!(world_copy_jump([2.25, 4.0]), [0.25, 4.0]);
    }

    #[test]
    fn wheel_zoom_uses_leaflet_sigmoid_and_direction() {
        assert_eq!(leaflet_wheel_zoom_delta(0.0), 0.0);
        let positive = leaflet_wheel_zoom_delta(90.0);
        assert!(positive > 0.0 && positive < 4.0);
        assert_eq!(leaflet_wheel_zoom_delta(-90.0), -positive);
        assert!(leaflet_wheel_zoom_delta(9_000.0) <= 4.0);
    }

    #[test]
    fn playback_speed_controls_match_browser_clamping_and_cycle() {
        assert_eq!(
            playback_speed_index(0, PlaybackAction::SpeedDown),
            0,
            "the browser left arrow clamps at 1x"
        );
        assert_eq!(playback_speed_index(0, PlaybackAction::SpeedUp), 1);
        assert_eq!(playback_speed_index(1, PlaybackAction::SpeedUp), 2);
        assert_eq!(playback_speed_index(2, PlaybackAction::SpeedUp), 2);
        assert_eq!(playback_speed_index(0, PlaybackAction::CycleSpeed), 1);
        assert_eq!(playback_speed_index(1, PlaybackAction::CycleSpeed), 2);
        assert_eq!(playback_speed_index(2, PlaybackAction::CycleSpeed), 0);
        assert_eq!(playback_speed_index(2, PlaybackAction::TogglePause), 2);
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
        assert!((center[1] - 1.0).abs() < 1.0e-6);
        assert!(zoom < reset_zoom(PhysicalSize::new(1_280, 720)));
    }

    #[test]
    fn screen_world_coordinates_inverse_project_to_scenario_rows() {
        for expected in [(100, 100), (1_200, 600), (2_300, 1_100)] {
            let world =
                projection::grid_to_world(expected.0 as f64, expected.1 as f64, 2_400.0, 1_200.0);
            assert_eq!(
                world_to_cell([f64::from(world[0]), f64::from(world[1])], 2_400, 1_200),
                Some(expected)
            );
        }
        assert_eq!(world_to_cell([2.0, 1.0], 2_400, 1_200), Some((0, 600)));
        assert_eq!(
            world_to_cell([-0.25, 1.0], 2_400, 1_200),
            Some((2_100, 600))
        );
        assert_eq!(world_to_cell([1.0, 2.01], 2_400, 1_200), None);
        assert_eq!(world_to_cell([0.0, 0.0], 2_400, 1_200), Some((0, 1_167)));
    }

    #[test]
    fn parses_web_country_colors() {
        assert_eq!(parse_color("#ff8000"), Some([1.0, 128.0 / 255.0, 0.0, 1.0]));
        assert_eq!(
            parse_color("rgba(10, 20, 30, 0.5)"),
            Some([10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 0.65])
        );
        assert_eq!(parse_color("not-a-color"), None);
        assert_eq!(
            parse_color("hsla(301, 63%, 53%, 1)"),
            Some([211.0 / 255.0, 60.0 / 255.0, 208.0 / 255.0, 1.0])
        );
        assert_eq!(
            parse_color("hsla(19, 63%, 50%, 1)"),
            Some([208.0 / 255.0, 98.0 / 255.0, 47.0 / 255.0, 1.0])
        );
        assert_eq!(
            parse_color("hsla(145, 85%, 54%, 1)"),
            Some([38.0 / 255.0, 237.0 / 255.0, 121.0 / 255.0, 1.0])
        );
    }

    #[test]
    fn palette_blends_one_level_of_overlord_color_before_material_shading() {
        let metadata = serde_json::json!([
            {"id": 1, "color": "#010203"},
            {"id": 2, "color": "#050607", "overlordId": 1},
            {"id": 3, "color": "#090a0b", "overlordId": 2}
        ]);
        let (palette, occupation_palette) = build_palettes(&metadata, &[0, 1, 2, 3]);
        assert_eq!(palette[1], [1.0 / 255.0, 2.0 / 255.0, 3.0 / 255.0, 1.0]);
        assert_eq!(palette[2], [2.0 / 255.0, 3.0 / 255.0, 4.0 / 255.0, 1.0]);
        assert_eq!(palette[3], [6.0 / 255.0, 7.0 / 255.0, 8.0 / 255.0, 1.0]);
        assert_eq!(
            occupation_palette[2],
            [5.0 / 255.0, 6.0 / 255.0, 7.0 / 255.0, 1.0]
        );
    }

    #[test]
    fn immutable_texture_rows_use_gpu_alignment_without_reordering_pixels() {
        let (u8_rows, u8_stride) = pack_full_texture(&[1_u8, 2, 3, 4, 5, 6], 3, 2).unwrap();
        assert_eq!(u8_stride, ROW_ALIGNMENT as u32);
        assert_eq!(&u8_rows[..3], &[1, 2, 3]);
        assert_eq!(&u8_rows[ROW_ALIGNMENT..ROW_ALIGNMENT + 3], &[4, 5, 6]);

        let (u16_rows, u16_stride) = pack_full_texture(&[0x0102_u16, 0x0304], 1, 2).unwrap();
        assert_eq!(u16_stride, ROW_ALIGNMENT as u32);
        assert_eq!(&u16_rows[..2], &0x0102_u16.to_ne_bytes());
        assert_eq!(
            &u16_rows[ROW_ALIGNMENT..ROW_ALIGNMENT + 2],
            &0x0304_u16.to_ne_bytes()
        );
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
        assert_eq!(snapshot.economy_snapshot.len(), 2);
        assert_eq!(snapshot.economy_snapshot[0].country_id, 7);
        let observer = ObserverHudModel::from_runtime(&snapshot, Some(7), "Seven", false);
        assert!(observer.lines.iter().any(|line| line == "SEVEN  #7"));
        assert!(observer.lines.iter().any(|line| line == "TERRITORY"));
        assert!(observer.lines.iter().any(|line| line == "ECONOMY"));
        assert!(observer.lines.iter().any(|line| line == "FORCES"));
        assert!(observer.lines.iter().any(|line| line == "AIR POWER"));
        assert!(observer.lines.iter().any(|line| line == "OPERATIONS"));
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
            dominant_sides: vec![0, 1, 1, 0],
            dominant_city_controlled: vec![1, 0, 1, 0],
        };
        let update = TerritoryRenderUpdate {
            full_update: false,
            tiles: vec![tile.clone()],
        };
        let mut ownership = vec![0_u16; 12];
        let mut dominant_sides = vec![-1_i16; 12];
        let mut dominant_city_controlled = vec![0_u8; 12];
        apply_territory_update_to_grid(
            &mut ownership,
            &mut dominant_sides,
            &mut dominant_city_controlled,
            4,
            3,
            &update,
        )
        .unwrap();
        assert_eq!(ownership, vec![0, 0, 0, 0, 0, 9, 8, 0, 0, 7, 6, 0]);
        assert_eq!(
            dominant_sides,
            vec![-1, -1, -1, -1, -1, 0, 1, -1, -1, 1, 0, -1]
        );
        assert_eq!(
            dominant_city_controlled,
            vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0]
        );

        let (packed, bytes_per_row, rows) = pack_territory_tile(&tile).unwrap();
        assert_eq!(bytes_per_row as usize, ROW_ALIGNMENT);
        assert_eq!(rows, 2);
        assert_eq!(&packed[..4], bytemuck::cast_slice::<u16, u8>(&[9_u16, 8]));
        assert_eq!(
            &packed[ROW_ALIGNMENT..ROW_ALIGNMENT + 4],
            bytemuck::cast_slice::<u16, u8>(&[7_u16, 6])
        );
        let (packed_dominant, dominant_bytes_per_row, dominant_rows) =
            pack_dominant_tile(&tile).unwrap();
        assert_eq!(dominant_bytes_per_row as usize, ROW_ALIGNMENT);
        assert_eq!(dominant_rows, 2);
        assert_eq!(
            &packed_dominant[..4],
            bytemuck::cast_slice::<i16, u8>(&[0_i16, 1])
        );
    }
}
