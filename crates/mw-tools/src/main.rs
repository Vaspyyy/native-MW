use std::{env, fs, hint::black_box, path::PathBuf, process, time::Instant};

use anyhow::{Context, Result, bail};
use mw_core::{
    DecodedScenario, DirectionFieldInput, GridSpec, HostilityMatrix, NeighborOptions, PairOptions,
    TACTICAL_GRID_SCHEMA_VERSION, TacticalGrid, TacticalGridCounters, TacticalUnit,
    build_direction_field, decode_mwsc_gzip_file, tactical_cell_coords,
};
use serde::Deserialize;
use serde_json::{Value, json};

mod ai_orders;
mod native_tick;
mod strategic_cycle;
mod territory_control;
mod unit_kernel;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_owned());
    match command.as_str() {
        "inspect" => inspect(args.collect()),
        "field-bench" => field_bench(args.collect()),
        "tactical-fixture" => tactical_fixture(args.collect()),
        "tactical-bench" => tactical_bench(args.collect()),
        "unit-fixture" => unit_kernel::run_fixture_command(args.collect()),
        "unit-bench" => unit_kernel::run_bench_command(args.collect()),
        "native-tick-fixture" => native_tick::run_fixture_command(args.collect()),
        "native-tick-bench" => native_tick::run_bench_command(args.collect()),
        "ai-orders-fixture" => ai_orders::run_fixture_command(args.collect()),
        "ai-orders-bench" => ai_orders::run_bench_command(args.collect()),
        "strategic-cycle-fixture" => strategic_cycle::run_fixture_command(args.collect()),
        "strategic-cycle-bench" => strategic_cycle::run_bench_command(args.collect()),
        "territory-control-fixture" => territory_control::run_fixture_command(args.collect()),
        "territory-control-bench" => territory_control::run_bench_command(args.collect()),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => bail!("unknown command {command:?}; run `mw-tools help`"),
    }
}

fn print_help() {
    println!(
        "mw-tools\n\n  inspect <scenario.mwsc.gz> [--grid-res N] [--repeat N] [--json]\n  \
         field-bench <scenario.mwsc.gz> [--grid-res N] [--repeat N] [--side-a NAME] [--side-b NAME] [--json]\n  \
         tactical-fixture <fixture.json> [--json]\n  \
         tactical-bench <fixture.json> [--repeat N] [--warmup N] [--json]\n  \
         unit-fixture <fixture.json> [--json]\n  \
         unit-bench <fixture.json> [--repeat N] [--warmup N] [--json]\n  \
         native-tick-fixture <fixture.json> [--json]\n  \
         native-tick-bench <fixture.json> [--repeat N] [--warmup N] [--json]\n\n\
         ai-orders-fixture <fixture.json> [--json]\n  \
         ai-orders-bench <fixture.json> [--repeat N] [--warmup N] [--json]\n\n\
         strategic-cycle-fixture <fixture.json> [--json]\n  \
         strategic-cycle-bench <fixture.json> [--repeat N] [--warmup N] [--json]\n\n\
         territory-control-fixture <fixture.json> [--json]\n  \
         territory-control-bench <fixture.json> [--repeat N] [--warmup N] [--json]\n\n\
         Inspects web-compatible scenarios and runs deterministic parity fixtures and\n\
         performance benchmarks for migrated native systems."
    );
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TacticalFixture {
    schema_version: String,
    cell_size: f64,
    units: Vec<FixtureUnit>,
    neighbor_queries: Vec<FixtureNeighborQuery>,
    pair_queries: Vec<FixturePairQuery>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureUnit {
    id: u64,
    side: Option<u16>,
    lat: f64,
    lng: f64,
    strength: f64,
    ally_weight: f64,
    armor: bool,
    support: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureNeighborQuery {
    side: u16,
    lat: f64,
    lng: f64,
    radius_cells: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixturePairQuery {
    side: u16,
    radius_cells: usize,
    radius_sq: Option<f64>,
    reject_id_sum_modulo: Option<u64>,
}

fn parse_tactical_fixture(path: &PathBuf) -> Result<TacticalFixture> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let fixture: TacticalFixture = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if fixture.schema_version != TACTICAL_GRID_SCHEMA_VERSION {
        bail!(
            "unsupported tactical fixture schema {:?}; expected {:?}",
            fixture.schema_version,
            TACTICAL_GRID_SCHEMA_VERSION
        );
    }
    if fixture
        .pair_queries
        .iter()
        .any(|query| query.reject_id_sum_modulo == Some(0))
    {
        bail!("rejectIdSumModulo must be greater than zero");
    }
    Ok(fixture)
}

fn fixture_units(fixture: &TacticalFixture) -> Vec<TacticalUnit> {
    fixture
        .units
        .iter()
        .map(|unit| TacticalUnit {
            id: unit.id,
            side: unit.side,
            lat: unit.lat,
            lng: unit.lng,
            strength: unit.strength,
            ally_weight: unit.ally_weight,
            is_armor: unit.armor,
            is_support: unit.support,
        })
        .collect()
}

fn counters_json(counters: TacticalGridCounters) -> Value {
    json!({
        "input_units": counters.input_units,
        "inserted_units": counters.inserted_units,
        "skipped_units": counters.skipped_units,
        "side_count": counters.side_count,
        "cell_count": counters.cell_count,
        "max_bucket_occupancy": counters.max_bucket_occupancy,
        "candidate_pairs": counters.candidate_pairs,
        "accepted_pairs": counters.accepted_pairs,
    })
}

fn tactical_fixture(args: Vec<String>) -> Result<()> {
    let (path, as_json) = parse_fixture_command_args("tactical-fixture", args)?;
    let fixture = parse_tactical_fixture(&path)?;
    let units = fixture_units(&fixture);
    let mut grid = TacticalGrid::new(fixture.cell_size)?;
    grid.rebuild(&units)?;
    let initial_counters = grid.counters;

    let sides = grid
        .by_side
        .iter()
        .map(|(&side, cells)| {
            let cells = cells
                .values()
                .map(|cell| {
                    json!({
                        "key": cell.key,
                        "x": cell.x,
                        "y": cell.y,
                        "side": cell.side_key,
                        "count": cell.count,
                        "total_strength": cell.total_strength,
                        "total_ally_weight": cell.total_ally_weight,
                        "weighted_strength": cell.weighted_strength,
                        "centroid_lat": cell.centroid_lat,
                        "centroid_lng": cell.centroid_lng,
                        "armor_count": cell.armor_count,
                        "support_count": cell.support_count,
                        "has_armor": cell.has_armor,
                        "has_support": cell.has_support,
                        "unit_ids": cell.units.iter().map(|&index| units[index].id).collect::<Vec<_>>(),
                    })
                })
                .collect::<Vec<_>>();
            json!({ "side": side, "cells": cells })
        })
        .collect::<Vec<_>>();

    let neighbors = fixture
        .neighbor_queries
        .iter()
        .map(|query| {
            let mut keys = Vec::new();
            if let Some(origin) = tactical_cell_coords(query.lat, query.lng, grid.cell_size)? {
                grid.for_each_neighbor_cell(
                    query.side,
                    origin,
                    NeighborOptions {
                        radius_cells: query.radius_cells,
                    },
                    |cell| keys.push(cell.key),
                );
            }
            Ok(json!({ "side": query.side, "keys": keys }))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut pair_queries = Vec::with_capacity(fixture.pair_queries.len());
    for query in &fixture.pair_queries {
        let modulo = query.reject_id_sum_modulo;
        let mut visits = Vec::new();
        let stats = grid.for_each_unordered_neighbor_pair(
            query.side,
            PairOptions {
                radius_cells: query.radius_cells,
                radius_sq: query.radius_sq,
            },
            |visit| {
                modulo.is_none_or(|value| {
                    (u128::from(visit.left.id) + u128::from(visit.right.id)) % u128::from(value)
                        != 0
                })
            },
            |visit| {
                visits.push(json!({
                    "left_id": visit.left.id,
                    "right_id": visit.right.id,
                    "distance_sq": visit.distance_sq,
                    "left_key": visit.left_cell.key,
                    "right_key": visit.right_cell.key,
                }));
            },
        );
        pair_queries.push(json!({
            "side": query.side,
            "candidate_pairs": stats.candidate_pairs,
            "accepted_pairs": stats.accepted_pairs,
            "visits": visits,
        }));
    }

    let report = json!({
        "schema_version": fixture.schema_version,
        "dimensions": {
            "cell_size": grid.cell_size,
            "columns": grid.columns,
            "rows": grid.rows,
        },
        "initial_counters": counters_json(initial_counters),
        "sides": sides,
        "neighbors": neighbors,
        "pair_queries": pair_queries,
        "cumulative_pair_counters": {
            "candidate_pairs": grid.counters.candidate_pairs,
            "accepted_pairs": grid.counters.accepted_pairs,
        },
    });
    print_json_report(&report, as_json)
}

fn parse_fixture_command_args(command: &str, args: Vec<String>) -> Result<(PathBuf, bool)> {
    let mut path = None;
    let mut as_json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => as_json = true,
            option if option.starts_with('-') => bail!("unknown option {option:?}"),
            value if path.is_none() => path = Some(PathBuf::from(value)),
            value => bail!("unexpected argument {value:?}"),
        }
    }
    Ok((
        path.with_context(|| format!("{command} needs a fixture.json path"))?,
        as_json,
    ))
}

fn tactical_bench(args: Vec<String>) -> Result<()> {
    let mut path = None;
    let mut repeat = 5_usize;
    let mut warmup = 1_usize;
    let mut as_json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--repeat" => {
                index += 1;
                repeat = args.get(index).context("--repeat needs a value")?.parse()?;
                if repeat == 0 {
                    bail!("--repeat must be at least 1");
                }
            }
            "--warmup" => {
                index += 1;
                warmup = args.get(index).context("--warmup needs a value")?.parse()?;
            }
            "--json" => as_json = true,
            option if option.starts_with('-') => bail!("unknown option {option:?}"),
            value if path.is_none() => path = Some(PathBuf::from(value)),
            value => bail!("unexpected argument {value:?}"),
        }
        index += 1;
    }
    let path = path.context("tactical-bench needs a fixture.json path")?;
    let fixture = parse_tactical_fixture(&path)?;
    let units = fixture_units(&fixture);
    let mut grid = TacticalGrid::new(fixture.cell_size)?;

    for _ in 0..warmup {
        grid.rebuild(&units)?;
        black_box(run_pair_queries(
            &mut grid,
            &fixture.pair_queries,
            DigestMode::Fast,
        ));
    }

    let mut rebuild_ms = Vec::with_capacity(repeat);
    let mut pairs_ms = Vec::with_capacity(repeat);
    for _ in 0..repeat {
        let started = Instant::now();
        grid.rebuild(&units)?;
        rebuild_ms.push(started.elapsed().as_secs_f64() * 1_000.0);

        let started = Instant::now();
        let result = run_pair_queries(&mut grid, &fixture.pair_queries, DigestMode::Fast);
        pairs_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        black_box(result.visit_digest);
    }

    // Hash one untimed traversal so output verification cannot distort the
    // timing comparison (BigInt hashing is especially expensive in Node).
    grid.rebuild(&units)?;
    let verification = run_pair_queries(&mut grid, &fixture.pair_queries, DigestMode::Stable);
    let counters = grid.counters;
    let report = json!({
        "input": path.display().to_string(),
        "repeat": repeat,
        "warmup": warmup,
        "dimensions": {
            "cell_size": grid.cell_size,
            "columns": grid.columns,
            "rows": grid.rows,
        },
        "counters": counters_json(counters),
        "rebuild_ms": sample_summary(&mut rebuild_ms),
        "pairs_ms": sample_summary(&mut pairs_ms),
        "candidate_pairs": verification.candidate_pairs,
        "accepted_pairs": verification.accepted_pairs,
        "visit_hash": finish_hash(verification.visit_digest),
    });
    print_json_report(&report, as_json)
}

struct BenchPairResult {
    candidate_pairs: usize,
    accepted_pairs: usize,
    visit_digest: u64,
}

#[derive(Clone, Copy)]
enum DigestMode {
    Fast,
    Stable,
}

fn run_pair_queries(
    grid: &mut TacticalGrid,
    queries: &[FixturePairQuery],
    digest_mode: DigestMode,
) -> BenchPairResult {
    let mut candidate_pairs = 0;
    let mut accepted_pairs = 0;
    let mut visit_digest = match digest_mode {
        DigestMode::Fast => 0,
        DigestMode::Stable => FNV_OFFSET,
    };
    for query in queries {
        let modulo = query.reject_id_sum_modulo;
        let stats = grid.for_each_unordered_neighbor_pair(
            query.side,
            PairOptions {
                radius_cells: query.radius_cells,
                radius_sq: query.radius_sq,
            },
            |visit| {
                modulo.is_none_or(|value| {
                    (u128::from(visit.left.id) + u128::from(visit.right.id)) % u128::from(value)
                        != 0
                })
            },
            |visit| match digest_mode {
                DigestMode::Fast => {
                    visit_digest = visit_digest
                        .wrapping_add(visit.left.id.rotate_left(7))
                        .wrapping_add(visit.right.id.rotate_left(19))
                        .wrapping_add(visit.distance_sq.to_bits())
                        .wrapping_add(visit.left_cell.key as u64)
                        .wrapping_add((visit.right_cell.key as u64).rotate_left(31));
                }
                DigestMode::Stable => {
                    for bytes in [visit.left.id.to_le_bytes(), visit.right.id.to_le_bytes()] {
                        for byte in bytes {
                            visit_digest = hash_byte(visit_digest, byte);
                        }
                    }
                    for byte in (visit.distance_sq as f32).to_bits().to_le_bytes() {
                        visit_digest = hash_byte(visit_digest, byte);
                    }
                    for bytes in [
                        (visit.left_cell.key as u64).to_le_bytes(),
                        (visit.right_cell.key as u64).to_le_bytes(),
                    ] {
                        for byte in bytes {
                            visit_digest = hash_byte(visit_digest, byte);
                        }
                    }
                }
            },
        );
        candidate_pairs += stats.candidate_pairs;
        accepted_pairs += stats.accepted_pairs;
    }
    BenchPairResult {
        candidate_pairs,
        accepted_pairs,
        visit_digest,
    }
}

fn sample_summary(samples: &mut [f64]) -> Value {
    samples.sort_by(f64::total_cmp);
    let p95_index = (0.95 * samples.len() as f64).ceil() as usize - 1;
    json!({
        "median": samples[samples.len() / 2],
        "p95": samples[p95_index],
    })
}

fn print_json_report(report: &Value, compact: bool) -> Result<()> {
    if compact {
        println!("{}", serde_json::to_string(report)?);
    } else {
        println!("{}", serde_json::to_string_pretty(report)?);
    }
    Ok(())
}

fn field_bench(args: Vec<String>) -> Result<()> {
    let mut path = None;
    let mut grid_res = 0.15_f64;
    let mut repeat = 5_usize;
    let mut side_a = "Russia".to_owned();
    let mut side_b = "People's Republic of China".to_owned();
    let mut as_json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--grid-res" => {
                index += 1;
                grid_res = args
                    .get(index)
                    .context("--grid-res needs a value")?
                    .parse::<f64>()
                    .context("invalid --grid-res")?;
            }
            "--repeat" => {
                index += 1;
                repeat = args
                    .get(index)
                    .context("--repeat needs a value")?
                    .parse::<usize>()
                    .context("invalid --repeat")?;
                if repeat == 0 {
                    bail!("--repeat must be at least 1");
                }
            }
            "--side-a" => {
                index += 1;
                side_a = args.get(index).context("--side-a needs a value")?.clone();
            }
            "--side-b" => {
                index += 1;
                side_b = args.get(index).context("--side-b needs a value")?.clone();
            }
            "--json" => as_json = true,
            option if option.starts_with('-') => bail!("unknown option {option:?}"),
            value if path.is_none() => path = Some(PathBuf::from(value)),
            value => bail!("unexpected argument {value:?}"),
        }
        index += 1;
    }

    let path = path.context("field-bench needs a .mwsc.gz path")?;
    let decoded = decode_mwsc_gzip_file(&path, Some(GridSpec::world(grid_res)?))
        .with_context(|| format!("failed to decode {}", path.display()))?;
    let side_a_id = find_country_id(&decoded.metadata, &side_a)
        .with_context(|| format!("country {side_a:?} is not in the scenario"))?;
    let side_b_id = find_country_id(&decoded.metadata, &side_b)
        .with_context(|| format!("country {side_b:?} is not in the scenario"))?;

    let mut land_mask = decoded.land.clone();
    let mut dominant_side_map = vec![-1_i8; decoded.world_control.len()];
    for (index, owner) in decoded.world_control.iter().copied().enumerate() {
        if owner == side_a_id {
            land_mask[index] = 2;
            dominant_side_map[index] = 0;
        } else if owner == side_b_id {
            land_mask[index] = 2;
            dominant_side_map[index] = 1;
        }
    }

    let relations = [0, 1, 1, 0];
    let mut samples_ms = Vec::with_capacity(repeat);
    let mut last = None;
    for _ in 0..repeat {
        let started = Instant::now();
        let field = build_direction_field(DirectionFieldInput {
            land_mask: &land_mask,
            dominant_side_map: &dominant_side_map,
            hostility: HostilityMatrix::new(Some(&relations), 2),
            grid_width: decoded.target.width,
            grid_height: decoded.target.height,
            grid_res: decoded.target.grid_res,
        })?;
        samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        last = Some(field);
    }
    samples_ms.sort_by(f64::total_cmp);
    let field = last.expect("repeat is non-zero");
    let report = json!({
        "side_a": { "id": side_a_id, "name": side_a },
        "side_b": { "id": side_b_id, "name": side_b },
        "target": {
            "grid_res": decoded.target.grid_res,
            "width": decoded.target.width,
            "height": decoded.target.height,
        },
        "hashes": {
            "latitude": hash_f32(&field.latitude),
            "longitude": hash_f32(&field.longitude),
        },
        "directed_cells": field.latitude.iter().zip(&field.longitude)
            .filter(|(lat, lng)| **lat != 0.0 || **lng != 0.0).count(),
        "median_ms": samples_ms[samples_ms.len() / 2],
    });
    if as_json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

fn find_country_id(metadata: &Value, requested: &str) -> Option<u16> {
    let countries = metadata.get("metadata")?.as_array()?;
    countries
        .iter()
        .find(|country| {
            country
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(requested))
        })
        .and_then(|country| country.get("id"))
        .and_then(Value::as_u64)
        .and_then(|id| u16::try_from(id).ok())
}

fn inspect(args: Vec<String>) -> Result<()> {
    let mut path = None;
    let mut grid_res = None;
    let mut repeat = 1_usize;
    let mut as_json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--grid-res" => {
                index += 1;
                let value = args.get(index).context("--grid-res needs a value")?;
                grid_res = Some(value.parse::<f64>().context("invalid --grid-res")?);
            }
            "--repeat" => {
                index += 1;
                let value = args.get(index).context("--repeat needs a value")?;
                repeat = value.parse::<usize>().context("invalid --repeat")?;
                if repeat == 0 {
                    bail!("--repeat must be at least 1");
                }
            }
            "--json" => as_json = true,
            option if option.starts_with('-') => bail!("unknown option {option:?}"),
            value if path.is_none() => path = Some(PathBuf::from(value)),
            value => bail!("unexpected argument {value:?}"),
        }
        index += 1;
    }

    let path = path.context("inspect needs a .mwsc.gz path")?;
    let mut samples_ms = Vec::with_capacity(repeat);
    let mut decoded = None;
    for _ in 0..repeat {
        let started = Instant::now();
        let target = grid_res.map(GridSpec::world).transpose()?;
        let current = decode_mwsc_gzip_file(&path, target)
            .with_context(|| format!("failed to decode {}", path.display()))?;
        samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        decoded = Some(current);
    }
    samples_ms.sort_by(f64::total_cmp);
    let median_ms = samples_ms[samples_ms.len() / 2];
    let report = report(decoded.as_ref().expect("repeat is non-zero"), median_ms);

    if as_json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

fn report(decoded: &DecodedScenario, median_ms: f64) -> Value {
    json!({
        "name": decoded.metadata.get("name").and_then(Value::as_str).unwrap_or("Unnamed scenario"),
        "entry_count": decoded.entry_count,
        "source": {
            "grid_res": decoded.source.grid_res,
            "width": decoded.source.width,
            "height": decoded.source.height,
        },
        "target": {
            "grid_res": decoded.target.grid_res,
            "width": decoded.target.width,
            "height": decoded.target.height,
        },
        "hashes": {
            "world_control": hash_u16(&decoded.world_control),
            "de_jure": hash_u16(&decoded.de_jure),
            "land": hash_u8(&decoded.land),
            "biome": hash_u8(&decoded.biome),
            "province": hash_i32(&decoded.province),
        },
        "decode_ms": median_ms,
    })
}

fn finish_hash(hash: u64) -> String {
    format!("{hash:016x}")
}

fn hash_byte(mut hash: u64, byte: u8) -> u64 {
    hash ^= u64::from(byte);
    hash.wrapping_mul(FNV_PRIME)
}

fn hash_u8(values: &[u8]) -> String {
    finish_hash(
        values
            .iter()
            .fold(FNV_OFFSET, |hash, value| hash_byte(hash, *value)),
    )
}

fn hash_u16(values: &[u16]) -> String {
    let mut hash = FNV_OFFSET;
    for value in values {
        for byte in value.to_le_bytes() {
            hash = hash_byte(hash, byte);
        }
    }
    finish_hash(hash)
}

fn hash_i32(values: &[i32]) -> String {
    let mut hash = FNV_OFFSET;
    for value in values {
        for byte in value.to_le_bytes() {
            hash = hash_byte(hash, byte);
        }
    }
    finish_hash(hash)
}

fn hash_f32(values: &[f32]) -> String {
    let mut hash = FNV_OFFSET;
    for value in values {
        for byte in value.to_bits().to_le_bytes() {
            hash = hash_byte(hash, byte);
        }
    }
    finish_hash(hash)
}

#[cfg(test)]
mod tests {
    #[test]
    fn fixture_json_preserves_javascript_shortest_float_bits() {
        let negative: f64 = serde_json::from_str("-113.93476713448763").unwrap();
        let positive: f64 = serde_json::from_str("936.0848039388657").unwrap();
        assert_eq!(negative.to_bits(), 0xc05c_7bd3_3988_0000);
        assert_eq!(positive.to_bits(), 0x408d_40ad_adb0_0000);
    }
}
