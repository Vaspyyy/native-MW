use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use ab_glyph::{Font, FontArc, GlyphId, PxScale, ScaleFont, point};
use bytemuck::{Pod, Zeroable};
use mw_core::{FrameSnapshot, ProductionCity, UnitKind};

const WORLD_WIDTH: f32 = 2.0;
const TILE_SIZE: f32 = 256.0;
const COUNTRY_SAMPLE_BINS: usize = 4;
const COUNTRY_FIT_CHAR_FACTOR: f32 = 0.65;
const COUNTRY_SPACING_FACTOR: f32 = 0.35;
const COUNTRY_DRAW_CHAR_FACTOR: f32 = 0.6;
const MAX_REGION_SAMPLES: usize = 100_000;
const REGION_PADDING_CELLS: usize = 25;
const LABEL_CENTER_CULL_PX: f32 = 400.0;
const ATLAS_SIZE: u32 = 2_048;
const GLYPH_PADDING: u32 = 12;
const FONT_BUCKETS: [u16; 4] = [16, 32, 64, 96];
const SERIF_FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/fonts/LiberationSerif-Bold.ttf"
));
const SANS_FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/fonts/LiberationSans-Bold.ttf"
));
const MONO_FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/fonts/LiberationMono-Bold.ttf"
));
const SIDE_COLORS: [[f32; 4]; 8] = [
    [1.0, 50.0 / 255.0, 50.0 / 255.0, 1.0],
    [50.0 / 255.0, 100.0 / 255.0, 1.0, 1.0],
    [1.0, 200.0 / 255.0, 0.0, 1.0],
    [0.0, 200.0 / 255.0, 100.0 / 255.0, 1.0],
    [180.0 / 255.0, 50.0 / 255.0, 220.0 / 255.0, 1.0],
    [1.0, 130.0 / 255.0, 0.0, 1.0],
    [0.0, 210.0 / 255.0, 210.0 / 255.0, 1.0],
    [200.0 / 255.0, 200.0 / 255.0, 200.0 / 255.0, 1.0],
];

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct LabelVertex {
    world: [f32; 2],
    offset: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
    effect: [f32; 4],
    textured: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TextEffect {
    radius: f32,
    alpha: f32,
    softness: f32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum FontFace {
    Serif,
    Sans,
    Mono,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct GlyphKey {
    face: FontFace,
    character: char,
    bucket: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AtlasGlyph {
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    bounds_min: [f32; 2],
    bounds_max: [f32; 2],
    advance: f32,
    raster_px: f32,
    drawable: bool,
}

struct FontAtlas {
    serif: FontArc,
    sans: FontArc,
    mono: FontArc,
    pixels: Vec<u8>,
    glyphs: HashMap<GlyphKey, AtlasGlyph>,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    dirty: bool,
    overflow_reported: bool,
}

impl FontAtlas {
    fn new() -> Self {
        Self {
            serif: FontArc::try_from_slice(SERIF_FONT_BYTES)
                .expect("embedded Liberation Serif Bold is invalid"),
            sans: FontArc::try_from_slice(SANS_FONT_BYTES)
                .expect("embedded Liberation Sans Bold is invalid"),
            mono: FontArc::try_from_slice(MONO_FONT_BYTES)
                .expect("embedded Liberation Mono Bold is invalid"),
            pixels: vec![0; (ATLAS_SIZE * ATLAS_SIZE) as usize],
            glyphs: HashMap::new(),
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
            dirty: false,
            overflow_reported: false,
        }
    }

    fn font(&self, face: FontFace) -> &FontArc {
        match face {
            FontFace::Serif => &self.serif,
            FontFace::Sans => &self.sans,
            FontFace::Mono => &self.mono,
        }
    }

    fn bucket(font_size: f32) -> u16 {
        let requested = font_size.max(1.0).ceil() as u16;
        FONT_BUCKETS
            .iter()
            .copied()
            .find(|bucket| *bucket >= requested)
            .unwrap_or(*FONT_BUCKETS.last().unwrap())
    }

    fn vertical_metrics(&self, face: FontFace, font_size: f32) -> (f32, f32) {
        let scaled = self.font(face).as_scaled(PxScale::from(font_size));
        (scaled.ascent(), scaled.descent())
    }

    fn glyph(&mut self, face: FontFace, character: char, font_size: f32) -> Option<AtlasGlyph> {
        let bucket = Self::bucket(font_size);
        let key = GlyphKey {
            face,
            character,
            bucket,
        };
        if let Some(glyph) = self.glyphs.get(&key) {
            return Some(*glyph);
        }

        let font = self.font(face).clone();
        let scale = PxScale::from(f32::from(bucket));
        let scaled = font.as_scaled(scale);
        let mut glyph_id = scaled.glyph_id(character);
        if glyph_id == GlyphId(0) && character != '?' {
            glyph_id = scaled.glyph_id('?');
        }
        let advance = scaled.h_advance(glyph_id);
        let Some(outlined) =
            font.outline_glyph(glyph_id.with_scale_and_position(scale, point(0.0, 0.0)))
        else {
            let glyph = AtlasGlyph {
                uv_min: [0.0; 2],
                uv_max: [0.0; 2],
                bounds_min: [0.0; 2],
                bounds_max: [0.0; 2],
                advance,
                raster_px: f32::from(bucket),
                drawable: false,
            };
            self.glyphs.insert(key, glyph);
            return Some(glyph);
        };
        let bounds = outlined.px_bounds();
        let width = bounds.width().max(0.0) as u32;
        let height = bounds.height().max(0.0) as u32;
        let mut coverage = vec![0u8; (width * height) as usize];
        outlined.draw(|x, y, value| {
            coverage[(y * width + x) as usize] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        });

        let cell_width = width + GLYPH_PADDING * 2;
        let cell_height = height + GLYPH_PADDING * 2;
        if cell_width > ATLAS_SIZE || cell_height > ATLAS_SIZE {
            return self.report_overflow();
        }
        if self.cursor_x + cell_width > ATLAS_SIZE {
            self.cursor_x = 0;
            self.cursor_y += self.row_height;
            self.row_height = 0;
        }
        if self.cursor_y + cell_height > ATLAS_SIZE {
            return self.report_overflow();
        }
        let atlas_x = self.cursor_x;
        let atlas_y = self.cursor_y;
        for y in 0..height {
            let source = (y * width) as usize;
            let destination =
                ((atlas_y + GLYPH_PADDING + y) * ATLAS_SIZE + atlas_x + GLYPH_PADDING) as usize;
            self.pixels[destination..destination + width as usize]
                .copy_from_slice(&coverage[source..source + width as usize]);
        }
        self.cursor_x += cell_width;
        self.row_height = self.row_height.max(cell_height);
        self.dirty = true;

        let atlas_size = ATLAS_SIZE as f32;
        let glyph = AtlasGlyph {
            uv_min: [atlas_x as f32 / atlas_size, atlas_y as f32 / atlas_size],
            uv_max: [
                (atlas_x + cell_width) as f32 / atlas_size,
                (atlas_y + cell_height) as f32 / atlas_size,
            ],
            bounds_min: [
                bounds.min.x - GLYPH_PADDING as f32,
                bounds.min.y - GLYPH_PADDING as f32,
            ],
            bounds_max: [
                bounds.max.x + GLYPH_PADDING as f32,
                bounds.max.y + GLYPH_PADDING as f32,
            ],
            advance,
            raster_px: f32::from(bucket),
            drawable: true,
        };
        self.glyphs.insert(key, glyph);
        Some(glyph)
    }

    fn report_overflow(&mut self) -> Option<AtlasGlyph> {
        if !self.overflow_reported {
            log::warn!("map-label glyph atlas is full; unsupported new glyphs will be skipped");
            self.overflow_reported = true;
        }
        None
    }

    fn prime(&mut self, face: FontFace, text: &str, font_size: f32) {
        for character in text.chars() {
            let _ = self.glyph(face, character, font_size);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapLabelView {
    pub viewport: [f32; 2],
    pub center: [f32; 2],
    pub pixels_per_world: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapLabelLayout {
    pub city_markers: usize,
    pub city_names: usize,
    pub side_labels: usize,
    pub country_labels: usize,
}

struct VertexStore {
    buffer: wgpu::Buffer,
    capacity: usize,
    count: u32,
    vertices: Vec<LabelVertex>,
    label: &'static str,
}

impl VertexStore {
    fn new(device: &wgpu::Device, label: &'static str) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: std::mem::size_of::<LabelVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            capacity: 1,
            count: 0,
            vertices: Vec::new(),
            label,
        }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.vertices.len() > self.capacity {
            self.capacity = self.vertices.len().next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: (self.capacity * std::mem::size_of::<LabelVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !self.vertices.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&self.vertices));
        }
        self.count = self.vertices.len() as u32;
    }
}

pub struct MapLabelRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    atlas_texture: wgpu::Texture,
    atlas: FontAtlas,
    cities: VertexStore,
    sides: VertexStore,
    countries: VertexStore,
    layout: MapLabelLayout,
}

impl MapLabelRenderer {
    pub fn new(
        device: &wgpu::Device,
        view_buffer: &wgpu::Buffer,
        format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("map_label.wgsl"));
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("map label glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("map label glyph sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("map label bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("map label bind group"),
            layout: &bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&atlas_sampler),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("map label pipeline layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("map label pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LabelVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32x2,
                        3 => Float32x4,
                        4 => Float32x4,
                        5 => Float32
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
        Self {
            pipeline,
            bind_group,
            atlas_texture,
            atlas: FontAtlas::new(),
            cities: VertexStore::new(device, "city label vertices"),
            sides: VertexStore::new(device, "side label vertices"),
            countries: VertexStore::new(device, "country label vertices"),
            layout: MapLabelLayout::default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upload_static(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: MapLabelView,
        grid_size: [usize; 2],
        effective_ownership: &[u16],
        dominant_sides: &[i16],
        dominant_city_controlled: &[u8],
        cities: &[ProductionCity],
        country_names: &HashMap<u16, String>,
        show_non_capitals: bool,
    ) -> MapLabelLayout {
        self.cities.vertices.clear();
        self.countries.vertices.clear();
        let static_layout = build_static_layout(
            view,
            grid_size,
            effective_ownership,
            dominant_sides,
            dominant_city_controlled,
            cities,
            country_names,
            show_non_capitals,
            &mut self.atlas,
            &mut self.cities.vertices,
            &mut self.countries.vertices,
        );
        let side_font_size = (browser_zoom(view.pixels_per_world) * 4.0).max(12.0);
        self.atlas
            .prime(FontFace::Sans, "0123456789,", side_font_size);
        self.layout.city_markers = static_layout.city_markers;
        self.layout.city_names = static_layout.city_names;
        self.layout.country_labels = static_layout.country_labels;
        self.cities.upload(device, queue);
        self.countries.upload(device, queue);
        self.upload_atlas(queue);
        self.layout.clone()
    }

    pub fn upload_sides(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: MapLabelView,
        snapshot: Option<&FrameSnapshot>,
    ) -> MapLabelLayout {
        self.sides.vertices.clear();
        self.layout.side_labels =
            build_side_labels(view, snapshot, &mut self.atlas, &mut self.sides.vertices);
        self.sides.upload(device, queue);
        self.upload_atlas(queue);
        self.layout.clone()
    }

    pub fn draw_cities<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw_store(pass, &self.cities);
    }
    pub fn draw_side_labels<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw_store(pass, &self.sides);
    }
    pub fn draw_country_labels<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw_store(pass, &self.countries);
    }
    pub fn layout(&self) -> &MapLabelLayout {
        &self.layout
    }

    fn draw_store<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, store: &'a VertexStore) {
        if store.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, store.buffer.slice(..));
        pass.draw(0..store.count, 0..1);
    }

    fn upload_atlas(&mut self, queue: &wgpu::Queue) {
        if !self.atlas.dirty {
            return;
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.atlas.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE),
                rows_per_image: Some(ATLAS_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
        );
        self.atlas.dirty = false;
    }
}

pub fn browser_zoom(pixels_per_world: f32) -> f32 {
    (pixels_per_world.max(1.0) * WORLD_WIDTH / TILE_SIZE).log2()
}

pub fn geographic_to_world(lat: f64, lng: f64) -> [f32; 2] {
    [
        ((lng + 180.0) / 180.0) as f32,
        ((90.0 - lat) / 180.0) as f32,
    ]
}

fn visible(view: MapLabelView, world: [f32; 2], padding: f32) -> bool {
    let half = [
        view.viewport[0] / view.pixels_per_world * 0.5 + padding,
        view.viewport[1] / view.pixels_per_world * 0.5 + padding,
    ];
    (world[0] - view.center[0]).abs() <= half[0] && (world[1] - view.center[1]).abs() <= half[1]
}

#[allow(clippy::too_many_arguments)]
fn build_static_layout(
    view: MapLabelView,
    grid_size: [usize; 2],
    owners: &[u16],
    dominant: &[i16],
    dominant_city_controlled: &[u8],
    cities: &[ProductionCity],
    names: &HashMap<u16, String>,
    show_non_capitals: bool,
    atlas: &mut FontAtlas,
    city_vertices: &mut Vec<LabelVertex>,
    country_vertices: &mut Vec<LabelVertex>,
) -> MapLabelLayout {
    let zoom = browser_zoom(view.pixels_per_world);
    let mut layout = MapLabelLayout::default();
    let min_population = if zoom >= 5.0 {
        100_000.0
    } else if zoom >= 4.0 {
        400_000.0
    } else {
        1_000_000.0
    };
    for city in cities {
        if !show_non_capitals && !city.capital {
            continue;
        }
        let world = geographic_to_world(city.lat, city.lng);
        let in_view = visible(view, world, 0.0);
        let side = dominant.get(city.cell).copied().unwrap_or(-1);
        let active = side >= 0;
        if zoom < 3.0 && !active {
            continue;
        }
        if zoom >= 6.0 && !in_view {
            continue;
        }
        if (3.0..6.0).contains(&zoom) && !((city.population > min_population && in_view) || active)
        {
            continue;
        }
        let base_radius = (zoom - 2.0).max(2.0);
        let radius = if city.capital {
            base_radius * 1.6
        } else {
            base_radius
        };
        let controlled = side >= 0
            && dominant_city_controlled
                .get(city.cell)
                .copied()
                .unwrap_or(0)
                != 0;
        let (color, stroke_alpha) = if controlled {
            (side_color(side as u16), 0.4)
        } else {
            ([1.0; 4], 0.6)
        };
        push_disc(
            city_vertices,
            world,
            radius,
            [color[0], color[1], color[2], 1.0],
        );
        push_annulus(
            city_vertices,
            world,
            (radius - 0.5).max(0.0),
            radius + 0.5,
            [0.0, 0.0, 0.0, stroke_alpha],
        );
        layout.city_markers += 1;
        if zoom >= 6.0 {
            push_left_baseline_text(
                city_vertices,
                atlas,
                FontFace::Mono,
                &city.name,
                world,
                [base_radius + 2.0, 4.0],
                10.0,
                [1.0; 4],
                TextEffect {
                    radius: 4.0,
                    alpha: 0.9,
                    softness: 1.0,
                },
            );
            layout.city_names += 1;
        }
    }

    for region in sampled_regions(view, grid_size, owners) {
        let Some(name) = names.get(&region.owner) else {
            continue;
        };
        if !region.center_is_near_view(view, grid_size) {
            continue;
        }
        let uppercase = name.to_uppercase();
        if uppercase.is_empty() {
            continue;
        }
        let points = region.screen_points(view, grid_size);
        let length = bezier_length(points);
        let bbox_area = region.screen_area(view, grid_size).sqrt();
        let font = country_font_size(zoom, bbox_area, length, uppercase.chars().count().max(1));
        if font < 7.0 {
            continue;
        }
        push_curved_text(
            country_vertices,
            atlas,
            &uppercase,
            region.anchor_world(grid_size),
            points,
            font,
        );
        layout.country_labels += 1;
    }
    layout
}

fn country_font_size(zoom: f32, area_scale: f32, path_length: f32, chars: usize) -> f32 {
    let initial = (zoom * 12.0).min(area_scale / 4.5).max(8.0);
    let ideal = path_length * 0.9
        / (chars.max(1) as f32 * (COUNTRY_FIT_CHAR_FACTOR + COUNTRY_SPACING_FACTOR));
    initial.min(ideal)
}

fn country_character_step(font_size: f32) -> f32 {
    font_size * (COUNTRY_DRAW_CHAR_FACTOR + COUNTRY_SPACING_FACTOR)
}

#[derive(Default)]
struct SideGroup {
    lat_sum: f64,
    lng_sum: f64,
    personnel: u64,
    positions: Vec<(f64, f64)>,
}

fn build_side_labels(
    view: MapLabelView,
    snapshot: Option<&FrameSnapshot>,
    atlas: &mut FontAtlas,
    vertices: &mut Vec<LabelVertex>,
) -> usize {
    let Some(frame) = snapshot else { return 0 };
    let zoom = browser_zoom(view.pixels_per_world);
    let mut groups = BTreeMap::<u16, SideGroup>::new();
    for unit in frame.units.iter() {
        if !visible(view, geographic_to_world(unit.lat, unit.lng), 0.0) {
            continue;
        }
        let group = groups.entry(unit.side).or_default();
        group.lat_sum += unit.lat;
        group.lng_sum += unit.lng;
        group.positions.push((unit.lat, unit.lng));
        if unit.kind != UnitKind::Armor {
            group.personnel = group.personnel.saturating_add(unit.personnel);
        }
    }
    for (side, group) in &groups {
        let count = group.positions.len();
        let lat = group.lat_sum / count as f64;
        let lng = group.lng_sum / count as f64;
        let world = geographic_to_world(lat, lng);
        let text = grouped_u64(group.personnel);
        let font_size = (zoom * 4.0).max(12.0);
        let y = -(zoom * 5.0).max(30.0);
        let angle = side_label_angle(lat, lng, &group.positions);
        let color = side_color(*side);
        push_centered_text(
            vertices,
            atlas,
            FontFace::Sans,
            &text,
            world,
            [0.0, y],
            font_size,
            color,
            TextEffect {
                radius: 2.5,
                alpha: 1.0,
                softness: 0.0,
            },
            angle,
        );
    }
    groups.len()
}

fn side_color(side: u16) -> [f32; 4] {
    SIDE_COLORS[side as usize % SIDE_COLORS.len()]
}

fn grouped_u64(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

fn side_label_angle(center_lat: f64, center_lng: f64, positions: &[(f64, f64)]) -> f32 {
    if positions.len() <= 5 {
        return 0.0;
    }
    let &(lat, lng) = positions
        .iter()
        .max_by(|a, b| {
            let da = (a.0 - center_lat).powi(2) + (a.1 - center_lng).powi(2);
            let db = (b.0 - center_lat).powi(2) + (b.1 - center_lng).powi(2);
            da.total_cmp(&db)
        })
        .unwrap();
    let mut angle = (-(lat - center_lat) as f32).atan2((lng - center_lng) as f32);
    if angle > std::f32::consts::FRAC_PI_2 {
        angle -= std::f32::consts::PI;
    }
    if angle < -std::f32::consts::FRAC_PI_2 {
        angle += std::f32::consts::PI;
    }
    angle * 0.3
}

#[derive(Clone, Debug)]
struct Region {
    owner: u16,
    visible_cells: Vec<[usize; 2]>,
    component_sum: [usize; 2],
    component_count: usize,
    min: [usize; 2],
    max: [usize; 2],
}

impl Region {
    fn anchor_world(&self, grid: [usize; 2]) -> [f32; 2] {
        [
            self.component_sum[0] as f32 / self.component_count as f32 / grid[0] as f32 * 2.0,
            1.0 - self.component_sum[1] as f32 / self.component_count as f32 / grid[1] as f32,
        ]
    }

    fn center_is_near_view(&self, view: MapLabelView, grid: [usize; 2]) -> bool {
        let world = self.anchor_world(grid);
        let offset = [world[0] - view.center[0], world[1] - view.center[1]];
        if offset[0].abs() > view.viewport[0] / view.pixels_per_world
            || offset[1].abs() > view.viewport[1] / view.pixels_per_world
        {
            return false;
        }
        let screen = [
            offset[0] * view.pixels_per_world + view.viewport[0] * 0.5,
            offset[1] * view.pixels_per_world + view.viewport[1] * 0.5,
        ];
        screen[0] >= -LABEL_CENTER_CULL_PX
            && screen[0] <= view.viewport[0] + LABEL_CENTER_CULL_PX
            && screen[1] >= -LABEL_CENTER_CULL_PX
            && screen[1] <= view.viewport[1] + LABEL_CENTER_CULL_PX
    }

    fn screen_points(&self, view: MapLabelView, grid: [usize; 2]) -> [[f32; 2]; 4] {
        let anchor = self.anchor_world(grid);
        let mut sums = [[0.0f32; 3]; COUNTRY_SAMPLE_BINS];
        let width = (self.max[0] - self.min[0] + 1).max(1) as f32;
        for [x, y] in &self.visible_cells {
            let bin = (((*x - self.min[0]) as f32 / width) * 4.0).floor().min(3.0) as usize;
            let world = [
                *x as f32 / grid[0] as f32 * 2.0,
                1.0 - *y as f32 / grid[1] as f32,
            ];
            sums[bin][0] += world[0];
            sums[bin][1] += world[1];
            sums[bin][2] += 1.0;
        }
        let mut points: [Option<[f32; 2]>; COUNTRY_SAMPLE_BINS] = std::array::from_fn(|index| {
            (sums[index][2] > 0.0).then(|| {
                [
                    sums[index][0] / sums[index][2],
                    sums[index][1] / sums[index][2],
                ]
            })
        });
        for index in 0..COUNTRY_SAMPLE_BINS {
            if points[index].is_some() {
                continue;
            }
            let left = (0..index)
                .rev()
                .find_map(|candidate| points[candidate].map(|point| (candidate, point)));
            let right = (index + 1..COUNTRY_SAMPLE_BINS)
                .find_map(|candidate| points[candidate].map(|point| (candidate, point)));
            points[index] = Some(match (left, right) {
                (Some((left_index, left)), Some((right_index, right))) => {
                    let t = (index - left_index) as f32 / (right_index - left_index) as f32;
                    [
                        left[0] + (right[0] - left[0]) * t,
                        left[1] + (right[1] - left[1]) * t,
                    ]
                }
                (Some((_, point)), None) | (None, Some((_, point))) => point,
                (None, None) => anchor,
            });
        }
        std::array::from_fn(|index| {
            let world = points[index].unwrap_or(anchor);
            [
                (world[0] - anchor[0]) * view.pixels_per_world,
                (world[1] - anchor[1]) * view.pixels_per_world,
            ]
        })
    }

    fn screen_area(&self, view: MapLabelView, grid: [usize; 2]) -> f32 {
        let w = (self.max[0] - self.min[0]) as f32 / grid[0] as f32 * 2.0 * view.pixels_per_world;
        let h = (self.max[1] - self.min[1]) as f32 / grid[1] as f32 * view.pixels_per_world;
        w * h
    }
}

fn sampled_regions(view: MapLabelView, grid: [usize; 2], owners: &[u16]) -> Vec<Region> {
    if grid[0] == 0 || grid[1] == 0 || owners.len() < grid[0] * grid[1] {
        return Vec::new();
    }
    let zoom = browser_zoom(view.pixels_per_world);
    let step = if zoom < 4.0 {
        4
    } else if zoom < 6.0 {
        2
    } else {
        1
    };
    let half_world = [
        view.viewport[0] / view.pixels_per_world * 0.5,
        view.viewport[1] / view.pixels_per_world * 0.5,
    ];
    let view_x_min = ((view.center[0] - half_world[0]) * grid[0] as f32 / 2.0)
        .floor()
        .clamp(0.0, grid[0] as f32 - 1.0) as usize;
    let view_x_max = ((view.center[0] + half_world[0]) * grid[0] as f32 / 2.0)
        .ceil()
        .clamp(0.0, grid[0] as f32 - 1.0) as usize;
    // Source rows run south-to-north, while world Y runs north-to-south.
    let view_y_min = ((1.0 - view.center[1] - half_world[1]) * grid[1] as f32)
        .floor()
        .clamp(0.0, grid[1] as f32 - 1.0) as usize;
    let view_y_max = ((1.0 - view.center[1] + half_world[1]) * grid[1] as f32)
        .ceil()
        .clamp(0.0, grid[1] as f32 - 1.0) as usize;
    let search_x_min = view_x_min.saturating_sub(REGION_PADDING_CELLS);
    let search_x_max = (view_x_max + REGION_PADDING_CELLS).min(grid[0] - 1);
    let search_y_min = view_y_min.saturating_sub(REGION_PADDING_CELLS);
    let search_y_max = (view_y_max + REGION_PADDING_CELLS).min(grid[1] - 1);
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    let first_x = view_x_min / step * step;
    let first_y = view_y_min / step * step;
    for y in (first_y..=view_y_max).step_by(step) {
        for x in (first_x..=view_x_max).step_by(step) {
            let start = y * grid[0] + x;
            let owner = owners[start];
            if owner == 0 || seen.contains(&start) {
                continue;
            }
            let mut queue = VecDeque::from([(x, y)]);
            seen.insert(start);
            let mut visible_cells = Vec::new();
            let mut component_sum = [0usize; 2];
            let mut component_count = 0usize;
            while let Some((cx, cy)) = queue.pop_front() {
                if component_count >= MAX_REGION_SAMPLES {
                    break;
                }
                component_sum[0] += cx;
                component_sum[1] += cy;
                component_count += 1;
                if cx >= view_x_min && cx <= view_x_max && cy >= view_y_min && cy <= view_y_max {
                    visible_cells.push([cx, cy]);
                }
                for (nx, ny) in [
                    (cx.wrapping_sub(step), cy),
                    (cx + step, cy),
                    (cx, cy.wrapping_sub(step)),
                    (cx, cy + step),
                ] {
                    if nx < search_x_min
                        || nx > search_x_max
                        || ny < search_y_min
                        || ny > search_y_max
                    {
                        continue;
                    }
                    let index = ny * grid[0] + nx;
                    if !seen.contains(&index) && owners[index] == owner {
                        seen.insert(index);
                        queue.push_back((nx, ny));
                    }
                }
            }
            if visible_cells.is_empty() {
                continue;
            }
            let min = [
                visible_cells.iter().map(|p| p[0]).min().unwrap(),
                visible_cells.iter().map(|p| p[1]).min().unwrap(),
            ];
            let max = [
                visible_cells.iter().map(|p| p[0]).max().unwrap(),
                visible_cells.iter().map(|p| p[1]).max().unwrap(),
            ];
            let region = Region {
                owner,
                visible_cells,
                component_sum,
                component_count,
                min,
                max,
            };
            result.push(region);
        }
    }
    result
}

fn push_disc(out: &mut Vec<LabelVertex>, world: [f32; 2], radius: f32, color: [f32; 4]) {
    const SEGMENTS: usize = 12;
    for i in 0..SEGMENTS {
        let a = i as f32 * std::f32::consts::TAU / SEGMENTS as f32;
        let b = (i + 1) as f32 * std::f32::consts::TAU / SEGMENTS as f32;
        push_triangle(
            out,
            world,
            [0.0, 0.0],
            [a.cos() * radius, a.sin() * radius],
            [b.cos() * radius, b.sin() * radius],
            color,
        );
    }
}

fn push_annulus(
    out: &mut Vec<LabelVertex>,
    world: [f32; 2],
    inner_radius: f32,
    outer_radius: f32,
    color: [f32; 4],
) {
    const SEGMENTS: usize = 12;
    for i in 0..SEGMENTS {
        let a = i as f32 * std::f32::consts::TAU / SEGMENTS as f32;
        let b = (i + 1) as f32 * std::f32::consts::TAU / SEGMENTS as f32;
        let inner_a = [a.cos() * inner_radius, a.sin() * inner_radius];
        let outer_a = [a.cos() * outer_radius, a.sin() * outer_radius];
        let inner_b = [b.cos() * inner_radius, b.sin() * inner_radius];
        let outer_b = [b.cos() * outer_radius, b.sin() * outer_radius];
        push_triangle(out, world, inner_a, outer_a, outer_b, color);
        push_triangle(out, world, inner_a, outer_b, inner_b, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn push_left_baseline_text(
    out: &mut Vec<LabelVertex>,
    atlas: &mut FontAtlas,
    face: FontFace,
    text: &str,
    world: [f32; 2],
    origin: [f32; 2],
    font_size: f32,
    color: [f32; 4],
    effect: TextEffect,
) {
    let mut pen_x = origin[0];
    for character in text.chars() {
        let Some(glyph) = atlas.glyph(face, character, font_size) else {
            continue;
        };
        let scale = font_size / glyph.raster_px;
        push_atlas_glyph(
            out,
            glyph,
            world,
            [pen_x, origin[1]],
            font_size,
            color,
            effect,
            0.0,
            origin,
        );
        pen_x += glyph.advance * scale;
    }
}

#[allow(clippy::too_many_arguments)]
fn push_centered_text(
    out: &mut Vec<LabelVertex>,
    atlas: &mut FontAtlas,
    face: FontFace,
    text: &str,
    world: [f32; 2],
    center: [f32; 2],
    font_size: f32,
    color: [f32; 4],
    effect: TextEffect,
    angle: f32,
) {
    let mut glyphs = Vec::with_capacity(text.chars().count());
    let mut width = 0.0;
    for character in text.chars() {
        let Some(glyph) = atlas.glyph(face, character, font_size) else {
            continue;
        };
        let advance = glyph.advance * font_size / glyph.raster_px;
        width += advance;
        glyphs.push((glyph, advance));
    }
    let (ascent, descent) = atlas.vertical_metrics(face, font_size);
    let baseline_y = center[1] + (ascent + descent) * 0.5;
    let mut pen_x = center[0] - width * 0.5;
    for (glyph, advance) in glyphs {
        push_atlas_glyph(
            out,
            glyph,
            world,
            [pen_x, baseline_y],
            font_size,
            color,
            effect,
            angle,
            center,
        );
        pen_x += advance;
    }
}

fn push_curved_text(
    out: &mut Vec<LabelVertex>,
    atlas: &mut FontAtlas,
    text: &str,
    world: [f32; 2],
    points: [[f32; 2]; 4],
    font: f32,
) {
    let chars: Vec<char> = text.chars().collect();
    let char_count = chars.len().max(1);
    let length = bezier_length(points).max(1.0);
    let total = chars.len() as f32 * country_character_step(font);
    let step = total / length / char_count as f32;
    let (ascent, descent) = atlas.vertical_metrics(FontFace::Serif, font);
    let baseline_shift = (ascent + descent) * 0.5;
    for (index, ch) in chars.into_iter().enumerate() {
        let t = 0.5 - total / length * 0.5 + index as f32 * step + step * 0.5;
        if !(0.0..=1.0).contains(&t) {
            continue;
        }
        let p = bezier(t, points);
        let angle = bezier_tangent(t, points);
        let Some(glyph) = atlas.glyph(FontFace::Serif, ch, font) else {
            continue;
        };
        let advance = glyph.advance * font / glyph.raster_px;
        push_atlas_glyph(
            out,
            glyph,
            world,
            [p[0] - advance * 0.5, p[1] + baseline_shift],
            font,
            [1.0; 4],
            TextEffect {
                radius: (font / 10.0).max(1.0),
                alpha: 0.8,
                softness: 0.0,
            },
            angle,
            p,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_atlas_glyph(
    out: &mut Vec<LabelVertex>,
    glyph: AtlasGlyph,
    world: [f32; 2],
    baseline: [f32; 2],
    font_size: f32,
    color: [f32; 4],
    effect: TextEffect,
    angle: f32,
    pivot: [f32; 2],
) {
    if !glyph.drawable {
        return;
    }
    let scale = font_size / glyph.raster_px;
    let min = [
        baseline[0] + glyph.bounds_min[0] * scale,
        baseline[1] + glyph.bounds_min[1] * scale,
    ];
    let max = [
        baseline[0] + glyph.bounds_max[0] * scale,
        baseline[1] + glyph.bounds_max[1] * scale,
    ];
    let atlas_pixels_per_screen = glyph.raster_px / font_size.max(1.0);
    let radius = effect
        .radius
        .min((GLYPH_PADDING as f32 - 1.0) / atlas_pixels_per_screen);
    push_textured_quad_rotated(
        out,
        world,
        min,
        max,
        glyph.uv_min,
        glyph.uv_max,
        angle,
        pivot,
        color,
        [
            radius,
            effect.alpha,
            effect.softness,
            atlas_pixels_per_screen,
        ],
    );
}

fn bezier(t: f32, p: [[f32; 2]; 4]) -> [f32; 2] {
    let u = 1.0 - t;
    [
        u * u * u * p[0][0]
            + 3.0 * u * u * t * p[1][0]
            + 3.0 * u * t * t * p[2][0]
            + t * t * t * p[3][0],
        u * u * u * p[0][1]
            + 3.0 * u * u * t * p[1][1]
            + 3.0 * u * t * t * p[2][1]
            + t * t * t * p[3][1],
    ]
}
fn bezier_tangent(t: f32, p: [[f32; 2]; 4]) -> f32 {
    let u = 1.0 - t;
    let dx = 3.0 * u * u * (p[1][0] - p[0][0])
        + 6.0 * u * t * (p[2][0] - p[1][0])
        + 3.0 * t * t * (p[3][0] - p[2][0]);
    let dy = 3.0 * u * u * (p[1][1] - p[0][1])
        + 6.0 * u * t * (p[2][1] - p[1][1])
        + 3.0 * t * t * (p[3][1] - p[2][1]);
    dy.atan2(dx)
}
fn bezier_length(p: [[f32; 2]; 4]) -> f32 {
    let mut prev = p[0];
    let mut sum = 0.0;
    for i in 1..=10 {
        let q = bezier(i as f32 / 10.0, p);
        sum += ((q[0] - prev[0]).powi(2) + (q[1] - prev[1]).powi(2)).sqrt();
        prev = q;
    }
    sum
}
#[allow(clippy::too_many_arguments)]
fn push_textured_quad_rotated(
    out: &mut Vec<LabelVertex>,
    world: [f32; 2],
    min: [f32; 2],
    max: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    angle: f32,
    pivot: [f32; 2],
    color: [f32; 4],
    effect: [f32; 4],
) {
    let rotate = |q: [f32; 2]| {
        let q = [q[0] - pivot[0], q[1] - pivot[1]];
        [
            pivot[0] + q[0] * angle.cos() - q[1] * angle.sin(),
            pivot[1] + q[0] * angle.sin() + q[1] * angle.cos(),
        ]
    };
    let a = rotate(min);
    let b = rotate([max[0], min[1]]);
    let c = rotate(max);
    let d = rotate([min[0], max[1]]);
    for (offset, uv) in [
        (a, uv_min),
        (b, [uv_max[0], uv_min[1]]),
        (c, uv_max),
        (a, uv_min),
        (c, uv_max),
        (d, [uv_min[0], uv_max[1]]),
    ] {
        out.push(LabelVertex {
            world,
            offset,
            uv,
            color,
            effect,
            textured: 1.0,
        });
    }
}
fn push_triangle(
    out: &mut Vec<LabelVertex>,
    world: [f32; 2],
    a: [f32; 2],
    b: [f32; 2],
    c: [f32; 2],
    color: [f32; 4],
) {
    for offset in [a, b, c] {
        out.push(LabelVertex {
            world,
            offset,
            uv: [0.0; 2],
            color,
            effect: [0.0; 4],
            textured: 0.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mw_core::UnitSnapshot;
    use std::sync::Arc;

    fn unit(
        id: u64,
        side: u16,
        kind: UnitKind,
        personnel: u64,
        lat: f64,
        lng: f64,
    ) -> UnitSnapshot {
        UnitSnapshot {
            id,
            side,
            sovereign: 1,
            kind,
            lat,
            lng,
            health: 100.0,
            max_health: 100.0,
            health_fraction: 1.0,
            personnel,
            personnel_capacity: personnel,
            equipment: 0,
            max_equipment: 0,
            dir_lat: 0.0,
            dir_lng: 0.0,
            coast_stuck_ticks: 0,
            last_combat_tick: 0,
            victory_boost_ticks: 0,
            landing_penalty_active: false,
            transport: false,
            at_sea: false,
        }
    }

    fn frame(units: Vec<UnitSnapshot>) -> FrameSnapshot {
        FrameSnapshot {
            schema_version: "test",
            tick: 0,
            frame: 0,
            units: Arc::from(units),
            events: Arc::from([]),
            removed_ids: Arc::from([]),
            abandoned_ids: Arc::from([]),
        }
    }
    #[test]
    fn browser_zoom_uses_world_pixel_width() {
        assert!((browser_zoom(128.0) - 0.0).abs() < 1e-6);
        assert!((browser_zoom(1024.0) - 3.0).abs() < 1e-6);
    }
    #[test]
    fn projection_matches_native_map() {
        assert_eq!(geographic_to_world(0.0, 0.0), [1.0, 0.5]);
    }

    #[test]
    fn embedded_faces_rasterize_antialiased_glyphs() {
        let mut atlas = FontAtlas::new();
        for face in [FontFace::Serif, FontFace::Sans, FontFace::Mono] {
            let glyph = atlas.glyph(face, 'A', 24.0).unwrap();
            assert!(glyph.drawable);
            assert!(glyph.advance > 0.0);
        }
        assert!(
            atlas
                .pixels
                .iter()
                .any(|coverage| (1..255).contains(coverage))
        );
    }

    #[test]
    fn atlas_text_is_one_antialiased_quad_per_glyph() {
        let mut atlas = FontAtlas::new();
        let glyph = atlas.glyph(FontFace::Mono, 'M', 10.0).unwrap();
        let mut vertices = Vec::new();
        push_atlas_glyph(
            &mut vertices,
            glyph,
            [1.0, 0.5],
            [0.0, 0.0],
            10.0,
            [1.0; 4],
            TextEffect {
                radius: 4.0,
                alpha: 0.9,
                softness: 1.0,
            },
            0.0,
            [0.0, 0.0],
        );
        assert_eq!(vertices.len(), 6);
        assert!(vertices.iter().all(|vertex| vertex.textured == 1.0));
        assert!(vertices.iter().any(|vertex| vertex.uv[0] > 0.0));
    }

    #[test]
    fn country_fit_and_spacing_match_browser_factors() {
        assert!((country_font_size(4.0, 1_000.0, 200.0, 10) - 18.0).abs() < 1e-6);
        assert!((country_character_step(20.0) - 19.0).abs() < 1e-6);
    }
    #[test]
    fn sampling_splits_disconnected_owner_regions() {
        let owners = [1, 0, 1, 0, 0, 0, 0, 0, 0];
        let regions = sampled_regions(
            MapLabelView {
                viewport: [65_536.0, 32_768.0],
                center: [1.0, 0.5],
                pixels_per_world: 32768.0,
            },
            [3, 3],
            &owners,
        );
        assert_eq!(regions.len(), 2);
    }
    #[test]
    fn city_population_thresholds_are_strict() {
        let view = MapLabelView {
            viewport: [800.0; 2],
            center: [1.0, 0.5],
            pixels_per_world: 2048.0,
        };
        assert_eq!(browser_zoom(view.pixels_per_world), 4.0);
        let city = |population| ProductionCity {
            city_id: 1,
            name: "TEST".into(),
            owner_id: 0,
            cell: 0,
            lat: 0.0,
            lng: 0.0,
            population,
            capital: false,
        };
        let names = HashMap::new();
        let mut atlas = FontAtlas::new();
        let mut cv = Vec::new();
        let mut nv = Vec::new();
        let exact = build_static_layout(
            view,
            [1, 1],
            &[0],
            &[-1],
            &[0],
            &[city(400_000.0)],
            &names,
            true,
            &mut atlas,
            &mut cv,
            &mut nv,
        );
        assert_eq!(exact.city_markers, 0);
        cv.clear();
        let above = build_static_layout(
            view,
            [1, 1],
            &[0],
            &[-1],
            &[0],
            &[city(400_001.0)],
            &names,
            true,
            &mut atlas,
            &mut cv,
            &mut nv,
        );
        assert_eq!(above.city_markers, 1);
    }

    #[test]
    fn active_city_comes_from_dominant_side_not_owner() {
        let view = MapLabelView {
            viewport: [800.0; 2],
            center: [1.0, 0.5],
            pixels_per_world: 1024.0,
        };
        let city = ProductionCity {
            city_id: 1,
            name: "ACTIVE".into(),
            owner_id: 9,
            cell: 0,
            lat: 80.0,
            lng: 170.0,
            population: 0.0,
            capital: false,
        };
        let mut cv = Vec::new();
        let mut nv = Vec::new();
        let mut atlas = FontAtlas::new();
        let inactive = build_static_layout(
            view,
            [1, 1],
            &[9],
            &[-1],
            &[0],
            std::slice::from_ref(&city),
            &HashMap::new(),
            true,
            &mut atlas,
            &mut cv,
            &mut nv,
        );
        assert_eq!(inactive.city_markers, 0);
        cv.clear();
        let active = build_static_layout(
            view,
            [1, 1],
            &[0],
            &[1],
            &[0],
            &[city],
            &HashMap::new(),
            true,
            &mut atlas,
            &mut cv,
            &mut nv,
        );
        assert_eq!(active.city_markers, 1);
    }

    #[test]
    fn city_fill_and_one_pixel_annulus_follow_browser_control_gate() {
        let view = MapLabelView {
            viewport: [800.0; 2],
            center: [1.0, 0.5],
            pixels_per_world: 2048.0,
        };
        let city = ProductionCity {
            city_id: 1,
            name: "GATE".into(),
            owner_id: 9,
            cell: 0,
            lat: 0.0,
            lng: 0.0,
            population: 0.0,
            capital: false,
        };
        let mut atlas = FontAtlas::new();
        let mut city_vertices = Vec::new();
        let mut country_vertices = Vec::new();
        build_static_layout(
            view,
            [1, 1],
            &[0],
            &[1],
            &[0],
            std::slice::from_ref(&city),
            &HashMap::new(),
            true,
            &mut atlas,
            &mut city_vertices,
            &mut country_vertices,
        );

        assert_eq!(city_vertices.len(), 36 + 72);
        assert!(
            city_vertices[..36]
                .iter()
                .all(|vertex| vertex.color == [1.0; 4])
        );
        assert!(
            city_vertices[36..]
                .iter()
                .all(|vertex| vertex.color == [0.0, 0.0, 0.0, 0.6])
        );
        assert!(city_vertices[36..].iter().all(|vertex| {
            let radius = vertex.offset[0].hypot(vertex.offset[1]);
            (radius - 1.5).abs() < 1e-5 || (radius - 2.5).abs() < 1e-5
        }));

        city_vertices.clear();
        build_static_layout(
            view,
            [1, 1],
            &[0],
            &[1],
            &[1],
            &[city],
            &HashMap::new(),
            true,
            &mut atlas,
            &mut city_vertices,
            &mut country_vertices,
        );
        assert!(
            city_vertices[..36]
                .iter()
                .all(|vertex| vertex.color == SIDE_COLORS[1])
        );
        assert!(
            city_vertices[36..]
                .iter()
                .all(|vertex| vertex.color == [0.0, 0.0, 0.0, 0.4])
        );
    }

    #[test]
    fn source_grid_y_is_south_to_north() {
        let south = Region {
            owner: 1,
            visible_cells: vec![[0, 0]],
            component_sum: [0, 0],
            component_count: 1,
            min: [0, 0],
            max: [0, 0],
        };
        let north = Region {
            owner: 1,
            visible_cells: vec![[0, 3]],
            component_sum: [0, 3],
            component_count: 1,
            min: [0, 3],
            max: [0, 3],
        };
        assert!(south.anchor_world([1, 4])[1] > north.anchor_world([1, 4])[1]);
    }

    #[test]
    fn region_area_uses_coordinate_span_without_cell_extent() {
        let region = Region {
            owner: 1,
            visible_cells: vec![[0, 0], [7, 3]],
            component_sum: [7, 3],
            component_count: 2,
            min: [0, 0],
            max: [7, 3],
        };
        let view = MapLabelView {
            viewport: [800.0; 2],
            center: [1.0, 0.5],
            pixels_per_world: 100.0,
        };
        assert!((region.screen_area(view, [8, 4]) - 13_125.0).abs() < 1e-6);
    }

    #[test]
    fn empty_region_bins_interpolate_then_copy_browser_style() {
        let region = Region {
            owner: 1,
            visible_cells: vec![[0, 2], [7, 2]],
            component_sum: [7, 4],
            component_count: 2,
            min: [0, 2],
            max: [7, 2],
        };
        let view = MapLabelView {
            viewport: [800.0; 2],
            center: [1.0, 0.5],
            pixels_per_world: 100.0,
        };
        let points = region.screen_points(view, [8, 4]);
        assert!((points[0][0] + 87.5).abs() < 1e-6);
        assert!((points[1][0] + 29.166_666).abs() < 1e-5);
        assert!((points[2][0] - 29.166_666).abs() < 1e-5);
        assert!((points[3][0] - 87.5).abs() < 1e-6);
    }

    #[test]
    fn region_center_is_culled_four_hundred_pixels_beyond_view() {
        let near = Region {
            owner: 1,
            visible_cells: vec![[4, 2]],
            component_sum: [4, 2],
            component_count: 1,
            min: [4, 2],
            max: [4, 2],
        };
        let far = Region {
            owner: 1,
            visible_cells: vec![[0, 2]],
            component_sum: [0, 2],
            component_count: 1,
            min: [0, 2],
            max: [0, 2],
        };
        let view = MapLabelView {
            viewport: [100.0; 2],
            center: [1.0, 0.5],
            pixels_per_world: 1_000.0,
        };
        assert!(near.center_is_near_view(view, [8, 4]));
        assert!(!far.center_is_near_view(view, [8, 4]));
    }

    #[test]
    fn region_center_must_also_fit_browser_half_padded_bounds() {
        let outside_padded_bounds_but_inside_pixel_margin = Region {
            owner: 1,
            visible_cells: vec![[5, 2]],
            component_sum: [5, 2],
            component_count: 1,
            min: [5, 2],
            max: [5, 2],
        };
        let view = MapLabelView {
            viewport: [100.0; 2],
            center: [1.0, 0.5],
            pixels_per_world: 1_000.0,
        };
        assert!(!outside_padded_bounds_but_inside_pixel_margin.center_is_near_view(view, [8, 4]));
    }

    #[test]
    fn region_sampling_is_viewport_bounded() {
        let grid = [200, 100];
        let owners = vec![1; grid[0] * grid[1]];
        let view = MapLabelView {
            viewport: [100.0; 2],
            center: [0.1, 0.5],
            pixels_per_world: 1000.0,
        };
        let regions = sampled_regions(view, grid, &owners);
        assert_eq!(regions.len(), 1);
        assert!(
            regions[0].max[0] < 50,
            "scan escaped viewport padding: {}",
            regions[0].max[0]
        );
        assert!(regions[0].visible_cells.len() < owners.len());
        assert!(regions[0].component_count > regions[0].visible_cells.len());
    }

    #[test]
    fn grouping_and_rotation_helpers_are_deterministic() {
        assert_eq!(grouped_u64(0), "0");
        assert_eq!(grouped_u64(1_234_567), "1,234,567");
        let horizontal = side_label_angle(0.0, 0.0, &[(0.0, 0.0); 6]);
        assert_eq!(horizontal, 0.0);
        let tangent = bezier_tangent(0.5, [[0.0, 0.0], [0.0, 1.0], [0.0, 2.0], [0.0, 3.0]]);
        assert!((tangent - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        let mut map = BTreeMap::new();
        map.insert(2u16, "late");
        map.insert(0u16, "first");
        assert_eq!(map.keys().copied().collect::<Vec<_>>(), vec![0, 2]);
    }

    #[test]
    fn mixed_unit_side_labels_are_deterministic_and_keep_zero() {
        let armor = unit(1, 1, UnitKind::Armor, 99, 0.0, 0.0);
        let zero = unit(2, 0, UnitKind::Army, 0, 1.0, 1.0);
        let view = MapLabelView {
            viewport: [800.0; 2],
            center: [1.0, 0.5],
            pixels_per_world: 1024.0,
        };
        let mut first = Vec::new();
        let mut second = Vec::new();
        let mut first_atlas = FontAtlas::new();
        let mut second_atlas = FontAtlas::new();
        assert_eq!(
            build_side_labels(
                view,
                Some(&frame(vec![armor, zero])),
                &mut first_atlas,
                &mut first
            ),
            2
        );
        assert_eq!(
            build_side_labels(
                view,
                Some(&frame(vec![zero, armor])),
                &mut second_atlas,
                &mut second
            ),
            2
        );
        assert_eq!(first, second);
        assert!(
            !first.is_empty(),
            "zero personnel must still produce glyph geometry"
        );
    }
}
