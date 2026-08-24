use std::{collections::HashMap, io::Cursor};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytemuck::{Pod, Zeroable};
use image::imageops::FilterType;
use mw_core::{FrameSnapshot, UnitKind};
use serde_json::Value;

const FLAG_ATLAS_WIDTH: u32 = 2_048;
const FLAG_ATLAS_HEIGHT: u32 = 1_024;
const FLAG_CELL_WIDTH: u32 = 64;
const FLAG_CELL_HEIGHT: u32 = 44;
const FLAG_CONTENT_WIDTH: u32 = 62;
const FLAG_CONTENT_HEIGHT: u32 = 40;
const FLAG_DATA_URL_MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;
const FLAG_DATA_IMAGE_MAX_DIMENSION: u32 = 4_096;
const FLAG_DATA_IMAGE_MAX_ALLOCATION: u64 = 64 * 1024 * 1024;
const FLAG_ATLAS_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/flags/flag-atlas.rgba"
));
const FLAG_ATLAS_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/flags/flag-atlas.json"
));

const UNIT_FLAG_TEXTURED: u32 = 1 << 0;
const UNIT_AT_SEA: u32 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct UnitInstance {
    pub world: [f32; 2],
    pub color: [f32; 4],
    pub flag_uv: [f32; 4],
    pub visual_seed: f32,
    pub kind: u32,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FlagSlot {
    uv: [f32; 4],
}

struct CountryFlagCatalog {
    atlas_pixels: Vec<u8>,
    by_country: HashMap<u64, FlagSlot>,
}

impl CountryFlagCatalog {
    fn from_metadata(metadata: &Value) -> Self {
        let manifest: Value = serde_json::from_str(FLAG_ATLAS_MANIFEST)
            .expect("embedded flag atlas manifest is invalid JSON");
        assert_eq!(
            manifest.get("schema").and_then(Value::as_str),
            Some("mw.flag-atlas"),
            "embedded flag atlas manifest has the wrong schema"
        );
        assert_eq!(
            manifest.get("version").and_then(Value::as_u64),
            Some(1),
            "embedded flag atlas manifest has the wrong version"
        );

        let entries = manifest
            .get("entries")
            .and_then(Value::as_object)
            .expect("embedded flag atlas manifest is missing entries");
        let slots = entries
            .iter()
            .filter_map(|(code, entry)| parse_flag_slot(entry).map(|slot| (code.clone(), slot)))
            .collect::<HashMap<_, _>>();
        let names = entries
            .iter()
            .filter_map(|(code, entry)| {
                entry
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| (normalize_country_name(name), code.clone()))
            })
            .collect::<HashMap<_, _>>();
        let mut atlas_pixels = FLAG_ATLAS_BYTES.to_vec();
        let mut next_cell = manifest
            .pointer("/nextCell/index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(slots.len());
        let mut dynamic_by_url = HashMap::<String, FlagSlot>::new();
        let mut by_country = HashMap::new();

        for country in country_array(metadata).into_iter().flatten() {
            let Some(country_id) = country.get("id").and_then(Value::as_u64) else {
                continue;
            };
            let name = country.get("name").and_then(Value::as_str).unwrap_or("");
            let flag_url = country
                .get("flagUrl")
                .or_else(|| country.get("flag_url"))
                .and_then(Value::as_str)
                .unwrap_or("");

            let mut slot = extract_flag_code(flag_url).and_then(|code| slots.get(&code).copied());
            if slot.is_none() && flag_url.starts_with("data:image/") {
                slot = dynamic_by_url.get(flag_url).copied().or_else(|| {
                    let capacity = (FLAG_ATLAS_WIDTH / FLAG_CELL_WIDTH)
                        * (FLAG_ATLAS_HEIGHT / FLAG_CELL_HEIGHT);
                    if next_cell >= capacity as usize {
                        return None;
                    }
                    let decoded = decode_data_image(flag_url)?;
                    let packed = pack_dynamic_flag(&mut atlas_pixels, &decoded, next_cell)?;
                    next_cell += 1;
                    dynamic_by_url.insert(flag_url.to_owned(), packed);
                    Some(packed)
                });
            }
            if slot.is_none() {
                slot = names
                    .get(&normalize_country_name(name))
                    .and_then(|code| slots.get(code))
                    .copied();
            }
            if let Some(slot) = slot {
                by_country.insert(country_id, slot);
            }
        }

        Self {
            atlas_pixels,
            by_country,
        }
    }
}

pub struct UnitRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    _flag_texture: wgpu::Texture,
    flag_uv_by_country: HashMap<u64, FlagSlot>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instance_count: u32,
    instances: Vec<UnitInstance>,
}

impl UnitRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view_buffer: &wgpu::Buffer,
        surface_format: wgpu::TextureFormat,
        metadata: &Value,
    ) -> Self {
        let catalog = CountryFlagCatalog::from_metadata(metadata);
        let flag_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("unit flag atlas"),
            size: wgpu::Extent3d {
                width: FLAG_ATLAS_WIDTH,
                height: FLAG_ATLAS_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &flag_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &catalog.atlas_pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(FLAG_ATLAS_WIDTH * 4),
                rows_per_image: Some(FLAG_ATLAS_HEIGHT),
            },
            wgpu::Extent3d {
                width: FLAG_ATLAS_WIDTH,
                height: FLAG_ATLAS_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let flag_view = flag_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let flag_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("unit flag sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("unit.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("unit bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
            label: Some("unit bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&flag_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&flag_sampler),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("unit pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("unit pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UnitInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        1 => Float32x2,
                        2 => Float32x4,
                        3 => Float32x4,
                        4 => Float32,
                        5 => Uint32,
                        6 => Uint32
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
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
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unit instances"),
            size: std::mem::size_of::<UnitInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bind_group,
            _flag_texture: flag_texture,
            flag_uv_by_country: catalog.by_country,
            instance_buffer,
            instance_capacity: 1,
            instance_count: 0,
            instances: Vec::new(),
        }
    }

    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, snapshot: &FrameSnapshot) {
        self.instances.clear();
        self.instances.extend(snapshot.units.iter().map(|unit| {
            let flag = self.flag_uv_by_country.get(&unit.sovereign).copied();
            UnitInstance {
                world: geographic_to_world(unit.lat, unit.lng),
                color: side_color(unit.side),
                flag_uv: flag.map_or([0.0; 4], |slot| slot.uv),
                visual_seed: unit_visual_seed(unit.id),
                kind: u32::from(matches!(unit.kind, UnitKind::Armor)),
                flags: (u32::from(flag.is_some()) * UNIT_FLAG_TEXTURED)
                    | (u32::from(unit.at_sea) * UNIT_AT_SEA),
            }
        }));
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unit instances"),
                size: (self.instance_capacity * std::mem::size_of::<UnitInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !self.instances.is_empty() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        }
        self.instance_count = self.instances.len() as u32;
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.instance_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, 0..self.instance_count);
    }

    pub fn instance_count(&self) -> u32 {
        self.instance_count
    }

    pub fn flag_count(&self) -> usize {
        self.flag_uv_by_country.len()
    }
}

fn country_array(metadata: &Value) -> Option<&[Value]> {
    metadata
        .get("metadata")
        .and_then(Value::as_array)
        .or_else(|| metadata.get("countries").and_then(Value::as_array))
        .or_else(|| metadata.as_array())
        .map(Vec::as_slice)
}

fn parse_flag_slot(value: &Value) -> Option<FlagSlot> {
    Some(FlagSlot {
        uv: [
            value.get("u0")?.as_f64()? as f32,
            value.get("v0")?.as_f64()? as f32,
            value.get("u1")?.as_f64()? as f32,
            value.get("v1")?.as_f64()? as f32,
        ],
    })
}

fn extract_flag_code(url: &str) -> Option<String> {
    if url.starts_with("data:") {
        return None;
    }
    let clean = url.split(['?', '#']).next()?;
    let file = clean.rsplit('/').next()?;
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    (!stem.is_empty()).then(|| stem.to_ascii_lowercase())
}

fn normalize_country_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn decode_data_image(url: &str) -> Option<image::RgbaImage> {
    let (header, encoded) = url.split_once(',')?;
    if !header.starts_with("data:image/")
        || !header.contains(";base64")
        || encoded.len() > FLAG_DATA_URL_MAX_ENCODED_BYTES
    {
        return None;
    }
    let bytes = STANDARD.decode(encoded.trim()).ok()?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(FLAG_DATA_IMAGE_MAX_DIMENSION);
    limits.max_image_height = Some(FLAG_DATA_IMAGE_MAX_DIMENSION);
    limits.max_alloc = Some(FLAG_DATA_IMAGE_MAX_ALLOCATION);
    reader.limits(limits);
    reader.decode().ok().map(|image| image.to_rgba8())
}

fn pack_dynamic_flag(atlas: &mut [u8], source: &image::RgbaImage, cell: usize) -> Option<FlagSlot> {
    let columns = FLAG_ATLAS_WIDTH / FLAG_CELL_WIDTH;
    let rows = FLAG_ATLAS_HEIGHT / FLAG_CELL_HEIGHT;
    if cell >= (columns * rows) as usize {
        return None;
    }
    let cell = cell as u32;
    let content_x = cell % columns * FLAG_CELL_WIDTH + 1;
    let content_y = cell / columns * FLAG_CELL_HEIGHT + 2;
    let resized = image::imageops::resize(
        source,
        FLAG_CONTENT_WIDTH,
        FLAG_CONTENT_HEIGHT,
        FilterType::Triangle,
    );
    let source_bytes = resized.as_raw();
    let row_bytes = (FLAG_CONTENT_WIDTH * 4) as usize;
    for row in 0..FLAG_CONTENT_HEIGHT as usize {
        let source_start = row * row_bytes;
        let target_start =
            ((content_y as usize + row) * FLAG_ATLAS_WIDTH as usize + content_x as usize) * 4;
        atlas[target_start..target_start + row_bytes]
            .copy_from_slice(&source_bytes[source_start..source_start + row_bytes]);
    }
    Some(FlagSlot {
        uv: [
            content_x as f32 / FLAG_ATLAS_WIDTH as f32,
            content_y as f32 / FLAG_ATLAS_HEIGHT as f32,
            (content_x + FLAG_CONTENT_WIDTH) as f32 / FLAG_ATLAS_WIDTH as f32,
            (content_y + FLAG_CONTENT_HEIGHT) as f32 / FLAG_ATLAS_HEIGHT as f32,
        ],
    })
}

pub(crate) fn unit_visual_seed(id: u64) -> f32 {
    let hash = fnv1a64(&id.to_le_bytes());
    ((hash >> 40) as u32) as f32 / 16_777_216.0
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn side_color(side: u16) -> [f32; 4] {
    const COLORS: [[f32; 4]; 8] = [
        [1.0, 50.0 / 255.0, 50.0 / 255.0, 1.0],
        [50.0 / 255.0, 100.0 / 255.0, 1.0, 1.0],
        [1.0, 200.0 / 255.0, 0.0, 1.0],
        [0.0, 200.0 / 255.0, 100.0 / 255.0, 1.0],
        [180.0 / 255.0, 50.0 / 255.0, 220.0 / 255.0, 1.0],
        [1.0, 130.0 / 255.0, 0.0, 1.0],
        [0.0, 210.0 / 255.0, 210.0 / 255.0, 1.0],
        [200.0 / 255.0, 200.0 / 255.0, 200.0 / 255.0, 1.0],
    ];
    COLORS[side as usize % COLORS.len()]
}

pub use crate::projection::geographic_to_world;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn geographic_mapping_matches_map_projection() {
        assert_eq!(geographic_to_world(90.0, -180.0), [0.0, 0.0]);
        assert_eq!(geographic_to_world(0.0, 0.0), [1.0, 1.0]);
        assert_eq!(geographic_to_world(-90.0, 180.0), [2.0, 2.0]);
    }

    #[test]
    fn embedded_atlas_and_manifest_have_the_declared_shape() {
        assert_eq!(
            FLAG_ATLAS_BYTES.len(),
            (FLAG_ATLAS_WIDTH * FLAG_ATLAS_HEIGHT * 4) as usize
        );
        let manifest: Value = serde_json::from_str(FLAG_ATLAS_MANIFEST).unwrap();
        assert!(manifest["entries"].as_object().unwrap().len() >= 200);
        assert_eq!(manifest["dimensions"]["width"], FLAG_ATLAS_WIDTH);
        assert_eq!(manifest["dimensions"]["height"], FLAG_ATLAS_HEIGHT);
    }

    #[test]
    fn flagcdn_country_code_and_normalized_name_resolve_offline() {
        let catalog = CountryFlagCatalog::from_metadata(&json!({
            "metadata": [
                {"id": 42, "name": "Germany", "flagUrl": "https://flagcdn.com/w320/de.png"},
                {"id": 56, "name": "  France  ", "flagUrl": ""}
            ]
        }));
        assert!(catalog.by_country.contains_key(&42));
        assert!(catalog.by_country.contains_key(&56));
        assert_ne!(catalog.by_country[&42], catalog.by_country[&56]);
    }

    #[test]
    fn embedded_data_flag_is_decoded_and_packed_in_the_next_cell() {
        let catalog = CountryFlagCatalog::from_metadata(&json!({
            "metadata": [{
                "id": 9,
                "name": "Customland",
                "flagUrl": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
            }]
        }));
        let slot = catalog.by_country[&9];
        assert!(slot.uv[0] > 0.45);
        assert!(slot.uv[2] > slot.uv[0]);
    }

    #[test]
    fn oversized_embedded_flag_is_rejected_before_decode() {
        let mut url = String::from("data:image/png;base64,");
        url.push_str(&"A".repeat(FLAG_DATA_URL_MAX_ENCODED_BYTES + 1));
        assert!(decode_data_image(&url).is_none());
    }

    #[test]
    fn visual_seed_is_stable_bounded_and_id_sensitive() {
        let seed = unit_visual_seed(1);
        assert_eq!(seed, unit_visual_seed(1));
        assert!((0.0..1.0).contains(&seed));
        assert_ne!(seed, unit_visual_seed(2));
    }

    #[test]
    fn side_palette_matches_browser_and_wraps() {
        assert_eq!(side_color(0), [1.0, 50.0 / 255.0, 50.0 / 255.0, 1.0]);
        assert_eq!(side_color(0), side_color(8));
    }
}
