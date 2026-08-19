//! Decoder for Modern Wars' compact MWSC v2 scenario format.

use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use flate2::read::GzDecoder;
use rayon::prelude::*;
use serde_json::Value;
use thiserror::Error;

const MAGIC: &[u8; 4] = b"MWSC";
const VERSION: u8 = 2;
const MIN_HEADER_BYTES: usize = 20;

/// Dimensions and resolution of a rectangular world grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridSpec {
    pub grid_res: f64,
    pub width: usize,
    pub height: usize,
}

impl GridSpec {
    /// Construct a grid covering the normal 360 by 180 degree game world.
    pub fn world(grid_res: f64) -> Result<Self, ScenarioError> {
        Self::with_world_size(grid_res, 360.0, 180.0)
    }

    fn with_world_size(
        grid_res: f64,
        world_width_deg: f64,
        world_height_deg: f64,
    ) -> Result<Self, ScenarioError> {
        if !grid_res.is_finite() || grid_res <= 0.0 {
            return Err(ScenarioError::InvalidGrid(
                "grid resolution must be finite and positive".into(),
            ));
        }
        if !world_width_deg.is_finite()
            || !world_height_deg.is_finite()
            || world_width_deg <= 0.0
            || world_height_deg <= 0.0
        {
            return Err(ScenarioError::InvalidGrid(
                "world dimensions must be finite and positive".into(),
            ));
        }
        let width = checked_ceil_to_usize(world_width_deg / grid_res)?;
        let height = checked_ceil_to_usize(world_height_deg / grid_res)?;
        checked_cell_count(width, height)?;
        Ok(Self {
            grid_res,
            width,
            height,
        })
    }

    pub fn cell_count(self) -> Result<usize, ScenarioError> {
        checked_cell_count(self.width, self.height)
    }
}

/// Dense simulation maps produced from one sparse MWSC scenario.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedScenario {
    /// Metadata JSON exactly as embedded in the MWSC envelope.
    pub metadata: Value,
    pub source: GridSpec,
    pub target: GridSpec,
    pub entry_count: u32,
    pub world_control: Vec<u16>,
    pub de_jure: Vec<u16>,
    pub land: Vec<u8>,
    pub biome: Vec<u8>,
    pub province: Vec<i32>,
}

#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("not an MWSC scenario binary")]
    InvalidMagic,
    #[error("unsupported MWSC version {0}")]
    UnsupportedVersion(u8),
    #[error("truncated MWSC scenario binary")]
    Truncated,
    #[error("invalid MWSC header: {0}")]
    InvalidHeader(String),
    #[error("invalid or truncated scenario varint")]
    InvalidVarint,
    #[error("MWSC run length cannot be zero")]
    ZeroRunLength,
    #[error("MWSC payload length does not match its entry count")]
    EntryCountMismatch,
    #[error("invalid scenario metadata JSON: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("decoded scenario has an invalid grid: {0}")]
    InvalidGrid(String),
    #[error("scenario grid is too large")]
    GridTooLarge,
    #[error("failed to read scenario: {0}")]
    Io(#[from] io::Error),
}

/// Decode uncompressed MWSC v2 bytes.
///
/// `target` may override both resolution and dimensions. Passing `None`
/// expands at the scenario's source resolution.
pub fn decode_mwsc(
    input: &[u8],
    target: Option<GridSpec>,
) -> Result<DecodedScenario, ScenarioError> {
    let envelope = parse_envelope(input)?;
    let source = source_grid(&envelope.metadata)?;
    let target = validate_target(target.unwrap_or(source))?;
    let target_len = target.cell_count()?;

    let mut world_control = vec![0_u16; target_len];
    let mut de_jure = vec![0_u16; target_len];
    let mut land = vec![0_u8; target_len];
    let mut biome = vec![0_u8; target_len];
    let same_grid = source == target;

    let mut cursor = envelope.payload_start;
    let mut previous_run_start = 0_i64;
    let mut decoded_entries = 0_u64;
    while cursor < envelope.payload_end {
        let delta = i64::from(read_var_i32(input, &mut cursor)?);
        let run_start = previous_run_start
            .checked_add(delta)
            .ok_or(ScenarioError::InvalidVarint)?;
        let run_length = read_var_u32(input, &mut cursor)?;
        let owner = read_var_u32(input, &mut cursor)? as u16;
        let biome_id = read_var_u32(input, &mut cursor)? as u8;
        if run_length == 0 {
            return Err(ScenarioError::ZeroRunLength);
        }
        decoded_entries = decoded_entries
            .checked_add(u64::from(run_length))
            .ok_or(ScenarioError::EntryCountMismatch)?;

        for offset in 0..run_length {
            let index = run_start + i64::from(offset);
            if index < 0 {
                continue;
            }
            map_entry(
                index as usize,
                owner,
                biome_id,
                source,
                target,
                same_grid,
                &mut world_control,
                &mut de_jure,
                &mut land,
                &mut biome,
            );
        }
        previous_run_start = run_start;
    }
    if cursor != envelope.payload_end || decoded_entries != u64::from(envelope.entry_count) {
        return Err(ScenarioError::EntryCountMismatch);
    }

    let mut province = vec![0_i32; target_len];
    province
        .par_iter_mut()
        .zip(world_control.par_iter().copied())
        .enumerate()
        .for_each(|(index, (slot, owner))| {
            if owner == 0 {
                return;
            }
            *slot = province_id(
                index % target.width,
                index / target.width,
                u32::from(owner),
                target.grid_res,
            );
        });

    Ok(DecodedScenario {
        metadata: envelope.metadata,
        source,
        target,
        entry_count: envelope.entry_count,
        world_control,
        de_jure,
        land,
        biome,
        province,
    })
}

/// Decompress gzip bytes and decode the contained MWSC v2 scenario.
pub fn decode_mwsc_gzip(
    input: &[u8],
    target: Option<GridSpec>,
) -> Result<DecodedScenario, ScenarioError> {
    let mut decoder = GzDecoder::new(input);
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes)?;
    decode_mwsc(&bytes, target)
}

/// Read and decode a gzip-compressed MWSC v2 file.
pub fn decode_mwsc_gzip_file(
    path: impl AsRef<Path>,
    target: Option<GridSpec>,
) -> Result<DecodedScenario, ScenarioError> {
    let file = File::open(path)?;
    let mut decoder = GzDecoder::new(file);
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes)?;
    decode_mwsc(&bytes, target)
}

struct Envelope {
    metadata: Value,
    entry_count: u32,
    payload_start: usize,
    payload_end: usize,
}

fn parse_envelope(input: &[u8]) -> Result<Envelope, ScenarioError> {
    if input.len() < MIN_HEADER_BYTES {
        return Err(ScenarioError::Truncated);
    }
    if &input[..4] != MAGIC {
        return Err(ScenarioError::InvalidMagic);
    }
    if input[4] != VERSION {
        return Err(ScenarioError::UnsupportedVersion(input[4]));
    }
    let header_bytes = usize::from(u16::from_le_bytes([input[6], input[7]]));
    if header_bytes < MIN_HEADER_BYTES {
        return Err(ScenarioError::InvalidHeader(
            "header length is smaller than the v2 header".into(),
        ));
    }
    let metadata_bytes = read_u32_le(input, 8)? as usize;
    let entry_count = read_u32_le(input, 12)?;
    let payload_bytes = read_u32_le(input, 16)? as usize;
    let payload_start = header_bytes
        .checked_add(metadata_bytes)
        .ok_or(ScenarioError::Truncated)?;
    let payload_end = payload_start
        .checked_add(payload_bytes)
        .ok_or(ScenarioError::Truncated)?;
    if payload_end > input.len() || header_bytes > input.len() {
        return Err(ScenarioError::Truncated);
    }
    let metadata = serde_json::from_slice(&input[header_bytes..payload_start])?;
    Ok(Envelope {
        metadata,
        entry_count,
        payload_start,
        payload_end,
    })
}

fn read_u32_le(input: &[u8], offset: usize) -> Result<u32, ScenarioError> {
    let bytes = input
        .get(offset..offset + 4)
        .ok_or(ScenarioError::Truncated)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("four-byte slice"),
    ))
}

fn read_var_u32(input: &[u8], cursor: &mut usize) -> Result<u32, ScenarioError> {
    let mut value = 0_u32;
    for shift in [0, 7, 14, 21, 28] {
        let byte = *input.get(*cursor).ok_or(ScenarioError::InvalidVarint)?;
        *cursor += 1;
        if shift == 28 && byte & 0xf0 != 0 {
            return Err(ScenarioError::InvalidVarint);
        }
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(ScenarioError::InvalidVarint)
}

fn read_var_i32(input: &[u8], cursor: &mut usize) -> Result<i32, ScenarioError> {
    let zigzag = read_var_u32(input, cursor)?;
    Ok(if zigzag & 1 == 0 {
        (zigzag >> 1) as i32
    } else {
        -((zigzag >> 1) as i32) - 1
    })
}

#[allow(clippy::too_many_arguments)]
fn map_entry(
    index: usize,
    owner: u16,
    biome_id: u8,
    source: GridSpec,
    target: GridSpec,
    same_grid: bool,
    world_control: &mut [u16],
    de_jure: &mut [u16],
    land: &mut [u8],
    biome: &mut [u8],
) {
    let Ok(source_len) = source.cell_count() else {
        return;
    };
    if index >= source_len {
        return;
    }
    if same_grid {
        world_control[index] = owner;
        de_jure[index] = owner;
        land[index] = 1;
        biome[index] = biome_id;
        return;
    }

    let source_y = index / source.width;
    let source_x = index % source.width;
    let base_lat = source_y as f64 * source.grid_res - 90.0;
    let base_lng = source_x as f64 * source.grid_res - 180.0;
    let x_start = ((base_lng + 180.0) / target.grid_res).floor() as i64;
    let x_end = ((base_lng + source.grid_res + 180.0 - 0.0001) / target.grid_res).floor() as i64;
    let y_start = ((base_lat + 90.0) / target.grid_res).floor() as i64;
    let y_end = ((base_lat + source.grid_res + 90.0 - 0.0001) / target.grid_res).floor() as i64;
    for y in y_start..=y_end {
        if y < 0 || y >= target.height as i64 {
            continue;
        }
        let row = y as usize * target.width;
        for x in x_start..=x_end {
            if x < 0 || x >= target.width as i64 {
                continue;
            }
            let target_index = row + x as usize;
            world_control[target_index] = owner;
            de_jure[target_index] = owner;
            land[target_index] = 1;
        }
    }
}

fn source_grid(metadata: &Value) -> Result<GridSpec, ScenarioError> {
    let grid_res = metadata
        .get("gridRes")
        .and_then(Value::as_f64)
        .ok_or_else(|| ScenarioError::InvalidGrid("metadata has no numeric gridRes".into()))?;
    let world_width = positive_number_or(metadata.get("worldWidthDeg"), 360.0);
    let world_height = positive_number_or(metadata.get("worldHeightDeg"), 180.0);
    GridSpec::with_world_size(grid_res, world_width, world_height)
}

fn positive_number_or(value: Option<&Value>, fallback: f64) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite() && *number > 0.0)
        .unwrap_or(fallback)
}

fn validate_target(target: GridSpec) -> Result<GridSpec, ScenarioError> {
    if !target.grid_res.is_finite() || target.grid_res <= 0.0 {
        return Err(ScenarioError::InvalidGrid(
            "target grid resolution must be finite and positive".into(),
        ));
    }
    if target.width == 0 || target.height == 0 {
        return Err(ScenarioError::InvalidGrid(
            "target grid dimensions must be positive".into(),
        ));
    }
    target.cell_count()?;
    Ok(target)
}

fn checked_ceil_to_usize(value: f64) -> Result<usize, ScenarioError> {
    let value = value.ceil();
    if !value.is_finite() || value <= 0.0 || value > usize::MAX as f64 {
        return Err(ScenarioError::GridTooLarge);
    }
    Ok(value as usize)
}

fn checked_cell_count(width: usize, height: usize) -> Result<usize, ScenarioError> {
    width.checked_mul(height).ok_or(ScenarioError::GridTooLarge)
}

fn province_id(x: usize, y: usize, country_id: u32, grid_res: f64) -> i32 {
    if country_id == 0 {
        return 0;
    }
    let lat = y as f64 * grid_res - 90.0;
    let lng = x as f64 * grid_res - 180.0;
    let nx = lng * 0.65;
    let ny = lat * 0.65;
    let country = f64::from(country_id);
    let w1 = (nx * 0.8 + ny * 0.6 + country * 0.1).sin() * 1.2;
    let w2 = (nx * 0.5 - ny * 0.9 + country * 0.2).cos() * 1.1;
    let noise = ((nx + w1) * 2.3).sin() * 0.5
        + ((ny + w2) * 1.9).sin() * 0.5
        + ((nx + ny) * 1.4 + country).sin() * 0.3
        + (nx * 3.1 - ny * 2.7).cos() * 0.2;
    let cell_x = (nx + w1 + noise).floor();
    let cell_y = (ny + w2 + noise).floor();
    let h1 = js_to_u32((cell_x * 73_856_093.0).abs());
    let h2 = js_to_u32((cell_y * 19_349_663.0).abs());
    let h3 = js_to_u32(country * 83_492_791.0);
    (h1 ^ h2 ^ h3) as i32
}

fn js_to_u32(value: f64) -> u32 {
    if !value.is_finite() || value == 0.0 {
        return 0;
    }
    value.trunc().rem_euclid(4_294_967_296.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use serde_json::json;
    use std::io::Write;

    fn push_var_u32(output: &mut Vec<u8>, mut value: u32) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            output.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn push_var_i32(output: &mut Vec<u8>, value: i32) {
        let zigzag = ((value as u32) << 1) ^ ((value >> 31) as u32);
        push_var_u32(output, zigzag);
    }

    fn fixture(metadata: Value, entries: &[(i32, u32, u32, u32)]) -> Vec<u8> {
        let metadata = serde_json::to_vec(&metadata).unwrap();
        let mut payload = Vec::new();
        for &(delta, length, owner, biome) in entries {
            push_var_i32(&mut payload, delta);
            push_var_u32(&mut payload, length);
            push_var_u32(&mut payload, owner);
            push_var_u32(&mut payload, biome);
        }
        let entry_count: u32 = entries.iter().map(|entry| entry.1).sum();
        let mut output = vec![0_u8; MIN_HEADER_BYTES];
        output[..4].copy_from_slice(MAGIC);
        output[4] = VERSION;
        output[6..8].copy_from_slice(&(MIN_HEADER_BYTES as u16).to_le_bytes());
        output[8..12].copy_from_slice(&(metadata.len() as u32).to_le_bytes());
        output[12..16].copy_from_slice(&entry_count.to_le_bytes());
        output[16..20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        output.extend(metadata);
        output.extend(payload);
        output
    }

    #[test]
    fn decodes_runs_metadata_and_typed_array_truncation() {
        let metadata = json!({"gridRes": 90.0, "name": "synthetic", "extra": [1, 2]});
        let bytes = fixture(metadata.clone(), &[(0, 2, 65_537, 258), (3, 1, 9, 7)]);
        let decoded = decode_mwsc(&bytes, None).unwrap();
        assert_eq!(decoded.metadata, metadata);
        assert_eq!(
            decoded.source,
            GridSpec {
                grid_res: 90.0,
                width: 4,
                height: 2
            }
        );
        assert_eq!(decoded.entry_count, 3);
        assert_eq!(decoded.world_control, [1, 1, 0, 9, 0, 0, 0, 0]);
        assert_eq!(decoded.de_jure, decoded.world_control);
        assert_eq!(decoded.land, [1, 1, 0, 1, 0, 0, 0, 0]);
        assert_eq!(decoded.biome, [2, 2, 0, 7, 0, 0, 0, 0]);
        assert_ne!(decoded.province[0], 0);
        assert_eq!(decoded.province[2], 0);
    }

    #[test]
    fn remaps_with_last_write_wins_and_drops_biome_like_js() {
        let bytes = fixture(json!({"gridRes": 90.0}), &[(0, 1, 3, 8), (0, 1, 4, 9)]);
        let target = GridSpec {
            grid_res: 45.0,
            width: 8,
            height: 4,
        };
        let decoded = decode_mwsc(&bytes, Some(target)).unwrap();
        assert_eq!(decoded.world_control[0], 4);
        assert_eq!(decoded.world_control[1], 4);
        assert_eq!(decoded.world_control[8], 4);
        assert_eq!(decoded.world_control[9], 4);
        assert!(decoded.biome.iter().all(|&value| value == 0));
        assert_eq!(decoded.land.iter().filter(|&&value| value == 1).count(), 4);
    }

    #[test]
    fn gzip_and_file_helpers_match_plain_decode() {
        let bytes = fixture(json!({"gridRes": 180.0}), &[(0, 1, 7, 2)]);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        let gzip = encoder.finish().unwrap();
        let expected = decode_mwsc(&bytes, None).unwrap();
        assert_eq!(decode_mwsc_gzip(&gzip, None).unwrap(), expected);

        let path = std::env::temp_dir().join(format!(
            "mw-core-scenario-{}-{}.mwsc.gz",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, gzip).unwrap();
        let from_file = decode_mwsc_gzip_file(&path, None).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(from_file, expected);
    }

    #[test]
    fn rejects_malformed_envelopes_and_payloads() {
        assert!(matches!(
            decode_mwsc(b"short", None),
            Err(ScenarioError::Truncated)
        ));
        let mut bytes = fixture(json!({"gridRes": 1.0}), &[]);
        bytes[0] = b'X';
        assert!(matches!(
            decode_mwsc(&bytes, None),
            Err(ScenarioError::InvalidMagic)
        ));
        bytes[0] = b'M';
        bytes[4] = 3;
        assert!(matches!(
            decode_mwsc(&bytes, None),
            Err(ScenarioError::UnsupportedVersion(3))
        ));

        let zero_run = fixture(json!({"gridRes": 1.0}), &[(0, 0, 1, 1)]);
        assert!(matches!(
            decode_mwsc(&zero_run, None),
            Err(ScenarioError::ZeroRunLength)
        ));

        let mut mismatch = fixture(json!({"gridRes": 1.0}), &[(0, 1, 1, 1)]);
        mismatch[12..16].copy_from_slice(&2_u32.to_le_bytes());
        assert!(matches!(
            decode_mwsc(&mismatch, None),
            Err(ScenarioError::EntryCountMismatch)
        ));
    }

    #[test]
    fn rejects_invalid_grid_and_overlong_varint() {
        let missing = fixture(json!({"name": "missing grid"}), &[]);
        assert!(matches!(
            decode_mwsc(&missing, None),
            Err(ScenarioError::InvalidGrid(_))
        ));
        let bytes = fixture(json!({"gridRes": 1.0}), &[]);
        assert!(matches!(
            decode_mwsc(
                &bytes,
                Some(GridSpec {
                    grid_res: 0.0,
                    width: 1,
                    height: 1
                })
            ),
            Err(ScenarioError::InvalidGrid(_))
        ));

        let mut invalid = fixture(json!({"gridRes": 1.0}), &[]);
        invalid[12..16].copy_from_slice(&1_u32.to_le_bytes());
        invalid[16..20].copy_from_slice(&5_u32.to_le_bytes());
        invalid.extend([0x80, 0x80, 0x80, 0x80, 0x10]);
        assert!(matches!(
            decode_mwsc(&invalid, None),
            Err(ScenarioError::InvalidVarint)
        ));
    }

    #[test]
    fn province_id_matches_known_javascript_results() {
        assert_eq!(province_id(0, 0, 7, 0.1), 1_638_578_298);
        assert_eq!(province_id(1234, 567, 42, 0.1), 1_768_188_734);
        assert_eq!(province_id(1439, 719, 65_535, 0.25), 1_074_814_664);
    }
}
