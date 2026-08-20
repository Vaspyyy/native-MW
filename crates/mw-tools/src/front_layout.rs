use std::{fs, hint::black_box, path::PathBuf, time::Instant};

use anyhow::{Context, Result, bail};
use mw_core::{
    FRONT_LAYOUT_SCHEMA_VERSION, FrontLayout, FrontLayoutConfig, FrontLayoutInput, FrontLayoutUnit,
    HostilityMatrix, derive_front_layout,
};
use serde::Deserialize;
use serde_json::{Value, json};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    schema_version: String,
    grid: GridFixture,
    max_sides: usize,
    hostility_matrix: Vec<u8>,
    units: Vec<UnitFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GridFixture {
    width: usize,
    height: usize,
    grid_res: f64,
    land_mask: Vec<u8>,
    dominant_side_map: Vec<i16>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnitFixture {
    id: u64,
    side_index: u16,
    lat: f64,
    lng: f64,
    #[serde(default)]
    garrison_excluded: bool,
    #[serde(default)]
    deploy_ticks: u32,
    previous_pair_key: Option<String>,
    previous_segment_idx: Option<usize>,
}

impl Fixture {
    fn load(path: &PathBuf) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let fixture: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        if fixture.schema_version != FRONT_LAYOUT_SCHEMA_VERSION {
            bail!(
                "unsupported front fixture schema {:?}; expected {:?}",
                fixture.schema_version,
                FRONT_LAYOUT_SCHEMA_VERSION
            );
        }
        Ok(fixture)
    }

    fn units(&self) -> Vec<FrontLayoutUnit> {
        self.units
            .iter()
            .map(|unit| FrontLayoutUnit {
                id: unit.id,
                side_index: unit.side_index,
                lat: unit.lat,
                lng: unit.lng,
                garrison_excluded: unit.garrison_excluded,
                deploy_ticks: unit.deploy_ticks,
                previous_pair_key: unit.previous_pair_key.clone(),
                previous_segment_idx: unit.previous_segment_idx,
            })
            .collect()
    }

    fn derive(&self, units: &[FrontLayoutUnit]) -> Result<FrontLayout> {
        Ok(derive_front_layout(
            FrontLayoutInput {
                grid_width: self.grid.width,
                grid_height: self.grid.height,
                grid_res: self.grid.grid_res,
                land_mask: &self.grid.land_mask,
                dominant_side_map: &self.grid.dominant_side_map,
                hostility: HostilityMatrix::new(Some(&self.hostility_matrix), self.max_sides),
                units,
            },
            &FrontLayoutConfig::default(),
        )?)
    }
}

pub fn run_fixture_command(args: Vec<String>) -> Result<()> {
    let (path, as_json) = parse_fixture_args("front-layout-fixture", args)?;
    let fixture = Fixture::load(&path)?;
    let units = fixture.units();
    let layout = fixture.derive(&units)?;
    let report = report(&layout);
    if as_json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

pub fn run_bench_command(args: Vec<String>) -> Result<()> {
    let (path, repeat, warmup, as_json) = parse_bench_args(args)?;
    let fixture = Fixture::load(&path)?;
    let units = fixture.units();
    for _ in 0..warmup {
        black_box(fixture.derive(&units)?);
    }
    let mut samples = Vec::with_capacity(repeat);
    let mut last = None;
    for _ in 0..repeat {
        let started = Instant::now();
        let layout = fixture.derive(&units)?;
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        last = Some(layout);
    }
    samples.sort_by(f64::total_cmp);
    let layout = last.context("front benchmark produced no sample")?;
    let output = json!({
        "schema_version": FRONT_LAYOUT_SCHEMA_VERSION,
        "repeat": repeat,
        "warmup": warmup,
        "median_ms": percentile(&samples, 0.5),
        "p95_ms": percentile(&samples, 0.95),
        "segments": layout.segments.len(),
        "assignments": layout.counters.assigned_units,
        "objectives": layout.objectives.len(),
        "checksum": checksum(&layout),
    });
    if as_json {
        println!("{}", serde_json::to_string(&output)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&output)?);
    }
    Ok(())
}

fn report(layout: &FrontLayout) -> Value {
    let segments = layout
        .segments
        .iter()
        .map(|segment| {
            json!({
                "stable_key": segment.stable_key,
                "id": segment.id.to_string(),
                "pair": segment.pair,
                "points": segment.points.iter().map(|point| json!({
                    "lat": point.lat,
                    "lng": point.lng,
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let assignments = layout
        .assignments
        .iter()
        .map(|assignment| {
            json!({
                "unit_id": assignment.unit_id.to_string(),
                "pair_key": assignment.pair_key,
                "segment_id": assignment.segment_id.map(|value| value.to_string()),
                "segment_idx": assignment.segment_idx,
                "target_lat": assignment.target_lat,
                "target_lng": assignment.target_lng,
                "objective_id": assignment.objective_id.map(|value| value.to_string()),
            })
        })
        .collect::<Vec<_>>();
    let objectives = layout
        .objectives
        .iter()
        .map(|objective| {
            json!({
                "id": objective.id.to_string(),
                "side_pair": objective.side_pair,
                "segment_id": objective.segment_id.to_string(),
                "lat": objective.lat,
                "lng": objective.lng,
                "capacity": objective.capacity,
                "priority": objective.priority,
            })
        })
        .collect::<Vec<_>>();
    let next_prior = layout
        .next_prior
        .iter()
        .map(|prior| {
            json!({
                "unit_id": prior.unit_id.to_string(),
                "pair_key": prior.pair_key,
                "segment_idx": prior.segment_idx,
                "objective_id": prior.objective_id.to_string(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": layout.schema_version,
        "segments": segments,
        "assignments": assignments,
        "objectives": objectives,
        "next_prior": next_prior,
        "counters": {
            "grid_cells": layout.counters.grid_cells,
            "frontier_cells": layout.counters.frontier_cells,
            "segments": layout.counters.segments,
            "input_units": layout.counters.input_units,
            "eligible_units": layout.counters.eligible_units,
            "assigned_units": layout.counters.assigned_units,
            "objectives": layout.counters.objectives,
        },
        "checksum": checksum(layout),
    })
}

fn checksum(layout: &FrontLayout) -> String {
    let mut hash = FNV_OFFSET;
    for segment in &layout.segments {
        hash_bytes(&mut hash, segment.stable_key.as_bytes());
        hash_bytes(&mut hash, &segment.id.to_le_bytes());
        for point in &segment.points {
            hash_bytes(&mut hash, &point.lat.to_bits().to_le_bytes());
            hash_bytes(&mut hash, &point.lng.to_bits().to_le_bytes());
        }
    }
    for assignment in &layout.assignments {
        hash_bytes(&mut hash, &assignment.unit_id.to_le_bytes());
        if let Some(pair) = &assignment.pair_key {
            hash_bytes(&mut hash, pair.as_bytes());
        }
        hash_bytes(
            &mut hash,
            &assignment.segment_idx.unwrap_or(usize::MAX).to_le_bytes(),
        );
        hash_bytes(
            &mut hash,
            &assignment.objective_id.unwrap_or(0).to_le_bytes(),
        );
    }
    format!("{hash:016x}")
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn percentile(samples: &[f64], percentile: f64) -> f64 {
    let index = ((samples.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len() - 1);
    samples[index]
}

fn parse_fixture_args(command: &str, args: Vec<String>) -> Result<(PathBuf, bool)> {
    let mut path = None;
    let mut as_json = false;
    for argument in args {
        if argument == "--json" {
            as_json = true;
        } else if argument.starts_with('-') {
            bail!("unknown {command} option {argument:?}");
        } else if path.replace(PathBuf::from(argument)).is_some() {
            bail!("usage: {command} <fixture.json> [--json]");
        }
    }
    Ok((
        path.context(format!("usage: {command} <fixture.json> [--json]"))?,
        as_json,
    ))
}

fn parse_bench_args(args: Vec<String>) -> Result<(PathBuf, usize, usize, bool)> {
    let mut path = None;
    let mut repeat = 20_usize;
    let mut warmup = 5_usize;
    let mut as_json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => as_json = true,
            "--repeat" | "--warmup" => {
                let option = args[index].as_str();
                index += 1;
                let value = args
                    .get(index)
                    .context(format!("{option} requires a value"))?;
                let value = value.parse::<usize>().context("invalid benchmark count")?;
                if value == 0 {
                    bail!("benchmark counts must be positive");
                }
                if option == "--repeat" {
                    repeat = value;
                } else {
                    warmup = value;
                }
            }
            argument if argument.starts_with('-') => bail!("unknown option {argument:?}"),
            argument => {
                if path.replace(PathBuf::from(argument)).is_some() {
                    bail!("usage: front-layout-bench <fixture.json> [options]");
                }
            }
        }
        index += 1;
    }
    Ok((
        path.context("usage: front-layout-bench <fixture.json> [options]")?,
        repeat,
        warmup,
        as_json,
    ))
}
