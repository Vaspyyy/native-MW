//! Non-blocking ArcGIS World Imagery acquisition and bounded GPU residency.

use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

pub const TILE_SIZE: u32 = 256;
pub const MAX_RESIDENT_TILES: usize = 64;
pub const TILE_URL: &str =
    "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}";

const MAX_TILE_BYTES: u64 = 4 * 1024 * 1024;
const EMPTY_TILE_Z: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageryViewport {
    pub width: u32,
    pub height: u32,
    /// Native Mercator world coordinates. One wrapped world is `2 x 2`.
    pub center_world: [f64; 2],
    pub pixels_per_world: f64,
}

#[derive(Debug)]
pub struct DecodedTile {
    pub key: TileKey,
    pub rgba: Vec<u8>,
}

enum Request {
    Tiles(Vec<TileKey>),
    Stop,
}

enum TileResult {
    Ready(DecodedTile),
    Failed(TileKey),
}

pub fn visible_tiles(view: ImageryViewport) -> Vec<TileKey> {
    if view.width == 0
        || view.height == 0
        || !view.pixels_per_world.is_finite()
        || view.pixels_per_world <= 0.0
        || !view.center_world.into_iter().all(f64::is_finite)
    {
        return Vec::new();
    }

    let zoom = (view.pixels_per_world / 128.0)
        .log2()
        .round()
        .clamp(0.0, 19.0) as u8;
    let tiles_per_axis = 1_u32 << zoom;
    let half_width_world = f64::from(view.width) / view.pixels_per_world * 0.5;
    let half_height_world = f64::from(view.height) / view.pixels_per_world * 0.5;
    let tile_scale = f64::from(tiles_per_axis) * 0.5;
    let min_x = ((view.center_world[0] - half_width_world) * tile_scale).floor() as i64;
    let max_x = ((view.center_world[0] + half_width_world) * tile_scale).ceil() as i64;
    let min_y = (((view.center_world[1] - half_height_world) * tile_scale).floor() as i64)
        .clamp(0, i64::from(tiles_per_axis));
    let max_y = (((view.center_world[1] + half_height_world) * tile_scale).ceil() as i64)
        .clamp(0, i64::from(tiles_per_axis));

    let mut result = Vec::new();
    for y in min_y..max_y {
        for x in min_x..max_x {
            result.push(TileKey {
                z: zoom,
                x: x.rem_euclid(i64::from(tiles_per_axis)) as u32,
                y: y as u32,
            });
        }
    }
    result.sort_unstable_by_key(|tile| (tile.y, tile.x));
    result.dedup();
    result
}

pub struct ImageryWorker {
    requests: SyncSender<Request>,
    results: Receiver<TileResult>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    requested: HashSet<TileKey>,
    loaded: HashSet<TileKey>,
    failed: HashSet<TileKey>,
}

impl ImageryWorker {
    pub fn start(queue_bound: usize) -> Self {
        let (requests, request_rx) = mpsc::sync_channel(queue_bound.max(1));
        let (result_tx, results) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join = thread::Builder::new()
            .name("mw-imagery".to_owned())
            .spawn(move || {
                let agent = ureq::AgentBuilder::new()
                    .timeout_connect(Duration::from_secs(3))
                    .timeout_read(Duration::from_secs(5))
                    .timeout_write(Duration::from_secs(3))
                    .build();
                while !worker_stop.load(Ordering::Acquire) {
                    let Ok(request) = request_rx.recv_timeout(Duration::from_millis(100)) else {
                        continue;
                    };
                    let Request::Tiles(keys) = request else {
                        break;
                    };
                    for key in keys {
                        if worker_stop.load(Ordering::Acquire) {
                            return;
                        }
                        let result = fetch_tile(&agent, key)
                            .map_or(TileResult::Failed(key), TileResult::Ready);
                        if result_tx.send(result).is_err() {
                            return;
                        }
                    }
                }
            })
            .expect("failed to spawn imagery worker");

        Self {
            requests,
            results,
            stop,
            join: Some(join),
            requested: HashSet::new(),
            loaded: HashSet::new(),
            failed: HashSet::new(),
        }
    }

    pub fn submit_view(&mut self, view: ImageryViewport) {
        let keys = visible_tiles(view)
            .into_iter()
            .filter(|key| {
                !self.loaded.contains(key)
                    && !self.failed.contains(key)
                    && self.requested.insert(*key)
            })
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return;
        }
        match self.requests.try_send(Request::Tiles(keys)) {
            Ok(()) => {}
            Err(
                TrySendError::Full(Request::Tiles(keys))
                | TrySendError::Disconnected(Request::Tiles(keys)),
            ) => {
                for key in keys {
                    self.requested.remove(&key);
                }
            }
            Err(TrySendError::Full(Request::Stop) | TrySendError::Disconnected(Request::Stop)) => {
                unreachable!()
            }
        }
    }

    pub fn poll(&mut self) -> Vec<DecodedTile> {
        let mut ready = Vec::new();
        loop {
            match self.results.try_recv() {
                Ok(TileResult::Ready(tile)) => {
                    self.requested.remove(&tile.key);
                    self.loaded.insert(tile.key);
                    ready.push(tile);
                }
                Ok(TileResult::Failed(key)) => {
                    self.requested.remove(&key);
                    self.failed.insert(key);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        ready
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.requests.try_send(Request::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for ImageryWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn fetch_tile(agent: &ureq::Agent, key: TileKey) -> Option<DecodedTile> {
    let url = TILE_URL
        .replace("{z}", &key.z.to_string())
        .replace("{y}", &key.y.to_string())
        .replace("{x}", &key.x.to_string());
    let response = agent.get(&url).call().ok()?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_TILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_TILE_BYTES {
        return None;
    }
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    if image.width() != TILE_SIZE || image.height() != TILE_SIZE {
        return None;
    }
    Some(DecodedTile {
        key,
        rgba: image.into_raw(),
    })
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GpuTileSlot {
    pub z: u32,
    pub x: u32,
    pub y: u32,
    pub layer: u32,
}

impl GpuTileSlot {
    fn empty(layer: usize) -> Self {
        Self {
            z: EMPTY_TILE_Z,
            x: 0,
            y: 0,
            layer: layer as u32,
        }
    }
}

pub struct ImageryAtlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    slots_buffer: wgpu::Buffer,
    slots: [GpuTileSlot; MAX_RESIDENT_TILES],
    keys: [Option<TileKey>; MAX_RESIDENT_TILES],
    key_to_slot: HashMap<TileKey, usize>,
    next_eviction: usize,
}

impl ImageryAtlas {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ArcGIS imagery tile array"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: MAX_RESIDENT_TILES as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("ArcGIS imagery tile array view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ArcGIS imagery sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let slots = std::array::from_fn(GpuTileSlot::empty);
        let slots_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ArcGIS imagery tile slots"),
            contents: bytemuck::cast_slice(&slots),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        Self {
            texture,
            view,
            sampler,
            slots_buffer,
            slots,
            keys: [None; MAX_RESIDENT_TILES],
            key_to_slot: HashMap::new(),
            next_eviction: 0,
        }
    }

    pub fn texture_view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    pub fn slots_buffer(&self) -> &wgpu::Buffer {
        &self.slots_buffer
    }

    pub fn upload(&mut self, queue: &wgpu::Queue, tiles: Vec<DecodedTile>) -> usize {
        let mut uploaded = 0;
        for tile in tiles {
            if tile.rgba.len() != (TILE_SIZE * TILE_SIZE * 4) as usize
                || self.key_to_slot.contains_key(&tile.key)
            {
                continue;
            }
            let slot = self
                .keys
                .iter()
                .position(Option::is_none)
                .unwrap_or_else(|| {
                    let slot = self.next_eviction;
                    self.next_eviction = (self.next_eviction + 1) % MAX_RESIDENT_TILES;
                    if let Some(old) = self.keys[slot] {
                        self.key_to_slot.remove(&old);
                    }
                    slot
                });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: slot as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &tile.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE_SIZE * 4),
                    rows_per_image: Some(TILE_SIZE),
                },
                wgpu::Extent3d {
                    width: TILE_SIZE,
                    height: TILE_SIZE,
                    depth_or_array_layers: 1,
                },
            );
            self.keys[slot] = Some(tile.key);
            self.key_to_slot.insert(tile.key, slot);
            self.slots[slot] = GpuTileSlot {
                z: u32::from(tile.key.z),
                x: tile.key.x,
                y: tile.key.y,
                layer: slot as u32,
            };
            uploaded += 1;
        }
        if uploaded > 0 {
            queue.write_buffer(&self.slots_buffer, 0, bytemuck::cast_slice(&self.slots));
        }
        uploaded
    }

    pub fn resident_count(&self) -> usize {
        self.key_to_slot.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_mercator_coordinates_use_the_native_two_unit_world() {
        let tiles = visible_tiles(ImageryViewport {
            width: 256,
            height: 256,
            center_world: [1.0, 1.0],
            pixels_per_world: 256.0,
        });
        assert_eq!(
            tiles,
            vec![
                TileKey { z: 1, x: 0, y: 0 },
                TileKey { z: 1, x: 1, y: 0 },
                TileKey { z: 1, x: 0, y: 1 },
                TileKey { z: 1, x: 1, y: 1 },
            ]
        );
    }

    #[test]
    fn longitude_wraps_and_poles_clip() {
        let tiles = visible_tiles(ImageryViewport {
            width: 512,
            height: 64,
            center_world: [0.0, 0.0],
            pixels_per_world: 256.0,
        });
        assert!(tiles.iter().all(|tile| tile.x < 2 && tile.y == 0));
        assert!(tiles.iter().any(|tile| tile.x == 0));
        assert!(tiles.iter().any(|tile| tile.x == 1));
    }

    #[test]
    fn invalid_view_is_empty() {
        assert!(
            visible_tiles(ImageryViewport {
                width: 0,
                height: 1,
                center_world: [0.0, 0.0],
                pixels_per_world: 1.0,
            })
            .is_empty()
        );
        assert!(
            visible_tiles(ImageryViewport {
                width: 1,
                height: 1,
                center_world: [0.0, 0.0],
                pixels_per_world: f64::NAN,
            })
            .is_empty()
        );
    }

    #[test]
    fn empty_gpu_slot_uses_the_shader_sentinel() {
        let slot = GpuTileSlot::empty(7);
        assert_eq!(slot.z, u32::MAX);
        assert_eq!(slot.layer, 7);
    }
}
