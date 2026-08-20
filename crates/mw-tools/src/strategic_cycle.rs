//! JSON command adapter for the deterministic StrategicSimulation parity fixture.
use anyhow::{Context, Result, bail};
use mw_core::{
    CommandBand, ConflictResolutionKind, EconomySeed, OccupationState, STRATEGIC_SCHEMA_VERSION,
    StrategicCycleInput, StrategicSimulation, create_economy_state,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{fs, hint::black_box, path::PathBuf, time::Instant};

pub fn run_fixture_command(args: Vec<String>) -> Result<()> {
    let (path, compact) = parse_fixture_args(args)?;
    let fixture = load(&path)?;
    print(&report(&fixture)?, compact)
}
pub fn run_bench_command(args: Vec<String>) -> Result<()> {
    let (path, repeat, warmup, compact) = parse_bench_args(args)?;
    let fixture = load(&path)?;
    let cycles = benchmark_cycles(&fixture);
    for _ in 0..warmup {
        let mut simulation = simulation(&fixture)?;
        black_box(execute_benchmark_cycles(&mut simulation, &cycles)?);
    }
    let mut samples = Vec::new();
    let mut final_run = None;
    for _ in 0..repeat {
        // State construction is deliberately outside the timer. Both the JS
        // oracle and Rust adapter measure only steady cycle execution.
        let mut simulation = simulation(&fixture)?;
        let started = Instant::now();
        let stats = execute_benchmark_cycles(&mut simulation, &cycles)?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        black_box((&simulation, stats));
        final_run = Some((simulation, stats));
    }
    samples.sort_by(f64::total_cmp);
    let (simulation, stats) = final_run.expect("repeat is validated as non-zero");
    let median = samples[(samples.len() - 1) / 2];
    let p95 = samples[((samples.len() as f64 * 0.95).ceil() as usize - 1).min(samples.len() - 1)];
    print(
        &json!({"schemaVersion":STRATEGIC_SCHEMA_VERSION,"mode":"bench","repeat":repeat,"warmup":warmup,"cycles":fixture.cycles.len(),"countries":fixture.economy_seeds.len(),"occupations":fixture.occupation_states.len(),"medianMs":median,"p95Ms":p95,"stats":{"attempted":stats.attempted,"completed":stats.completed,"expectedErrors":stats.expected_errors,"countriesProcessed":stats.countries_processed,"occupationsProcessed":stats.occupations_processed,"capitulations":stats.capitulations,"desertionCommands":stats.desertion_commands,"events":stats.events},"checksum":semantic_checksum(&simulation, stats)}),
        compact,
    )
}

#[derive(Clone)]
struct BenchmarkCycle {
    input: StrategicCycleInput,
    expected_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct BenchmarkStats {
    attempted: u64,
    completed: u64,
    expected_errors: u64,
    countries_processed: u64,
    occupations_processed: u64,
    capitulations: u64,
    desertion_commands: u64,
    events: u64,
}

fn benchmark_cycles(fixture: &Fixture) -> Vec<BenchmarkCycle> {
    fixture
        .cycles
        .iter()
        .map(|source| BenchmarkCycle {
            input: cycle_input(source),
            expected_error: source.expected_error.clone(),
        })
        .collect()
}

fn execute_benchmark_cycles(
    simulation: &mut StrategicSimulation,
    cycles: &[BenchmarkCycle],
) -> Result<BenchmarkStats> {
    let mut stats = BenchmarkStats::default();
    for source in cycles {
        stats.attempted += 1;
        match simulation.run_cycle(&source.input) {
            Ok((snapshot, counters)) => {
                if let Some(expected) = &source.expected_error {
                    bail!("expected strategic cycle error {expected:?}, but the cycle succeeded")
                }
                stats.completed += 1;
                stats.countries_processed += counters.countries_processed as u64;
                stats.occupations_processed += counters.occupations_processed as u64;
                stats.capitulations += counters.capitulations as u64;
                stats.desertion_commands += counters.desertion_commands as u64;
                stats.events += counters.events as u64;
                black_box((snapshot, counters));
            }
            Err(error) if source.expected_error.as_deref() == Some(error.to_string().as_str()) => {
                stats.expected_errors += 1;
                black_box(error);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(stats)
}

const FNV64_OFFSET: u64 = 14_695_981_039_346_656_037;
const FNV64_PRIME: u64 = 1_099_511_628_211;
const CHECKSUM_SCALE: f64 = 1_000_000.0;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn checksum_u64(hash: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV64_PRIME);
    }
}

fn checksum_bool(hash: &mut u64, value: bool) {
    checksum_u64(hash, u64::from(value));
}

fn checksum_float(hash: &mut u64, value: f64) {
    checksum_bool(hash, value.is_sign_negative());
    let magnitude = (value.abs() * CHECKSUM_SCALE + 0.5)
        .floor()
        .min(MAX_SAFE_INTEGER) as u64;
    checksum_u64(hash, magnitude);
}

const fn command_band_code(value: CommandBand) -> u64 {
    match value {
        CommandBand::Paid => 0,
        CommandBand::Strained => 1,
        CommandBand::Unpaid => 2,
        CommandBand::Breakdown => 3,
        CommandBand::Mutiny => 4,
    }
}

/// FNV-1a over a fixed semantic field stream. Floats are rounded to six
/// decimals, avoiding language-specific JSON formatting while still making
/// skipped settlements, occupation work, and cycle outputs visible.
fn semantic_checksum(simulation: &StrategicSimulation, stats: BenchmarkStats) -> String {
    let mut hash = FNV64_OFFSET;
    for value in [
        stats.attempted,
        stats.completed,
        stats.expected_errors,
        stats.countries_processed,
        stats.occupations_processed,
        stats.capitulations,
        stats.desertion_commands,
        stats.events,
        simulation.cycle(),
        simulation.economies().len() as u64,
    ] {
        checksum_u64(&mut hash, value);
    }
    for state in simulation.economies().values() {
        checksum_u64(&mut hash, u64::from(state.country_id));
        for value in [
            state.economic_strength,
            state.base_income,
            state.treasury,
            state.income,
            state.occupation_yield,
            state.payroll_due,
            state.occupation_due,
            state.payroll_coverage,
            state.occupation_coverage,
            state.arrears_cycles,
        ] {
            checksum_float(&mut hash, value);
        }
        checksum_u64(&mut hash, command_band_code(state.command_band));
        checksum_u64(&mut hash, u64::from(state.mutiny_recovery_cycles));
        checksum_u64(&mut hash, u64::from(state.initial_core_cells));
        checksum_float(&mut hash, state.initial_city_population);
        checksum_float(&mut hash, state.core_control_ratio);
        checksum_float(&mut hash, state.city_control_ratio);
        checksum_bool(&mut hash, state.capital_held);
        checksum_u64(&mut hash, command_band_code(state.last_event_band));
        checksum_bool(&mut hash, state.capitulated);
    }
    checksum_u64(&mut hash, simulation.occupations().len() as u64);
    for state in simulation.occupations().values() {
        checksum_u64(&mut hash, u64::from(state.victim_id));
        checksum_u64(&mut hash, u64::from(state.annexer_id));
        checksum_float(&mut hash, state.base_income);
        checksum_u64(&mut hash, u64::from(state.core_cells));
        checksum_float(&mut hash, state.expected_army_units);
        checksum_float(&mut hash, state.resistance);
        checksum_float(&mut hash, state.occupation_coverage);
        checksum_float(&mut hash, state.garrison_coverage);
        checksum_float(&mut hash, state.garrison_assigned);
        checksum_u64(&mut hash, u64::from(state.required_garrison));
        checksum_float(&mut hash, state.held_ratio);
        checksum_bool(&mut hash, state.active_rebellion);
        checksum_u64(&mut hash, state.queued_at_cycle);
        checksum_u64(&mut hash, state.cooldown_until_cycle);
    }
    if let Some(snapshot) = simulation.latest_snapshot() {
        checksum_bool(&mut hash, true);
        checksum_u64(&mut hash, snapshot.cycle);
        checksum_u64(&mut hash, snapshot.tick);
        checksum_u64(&mut hash, snapshot.territory_generation);
        checksum_u64(&mut hash, snapshot.territory_commit_sequence);
        for count in [
            snapshot.countries.len(),
            snapshot.occupations.len(),
            snapshot.occupation_assessments.len(),
            snapshot.desertions.len(),
            snapshot.surrenders.len(),
            snapshot.events.len(),
        ] {
            checksum_u64(&mut hash, count as u64);
        }
        if let Some(resolution) = snapshot.conflict_resolution {
            checksum_bool(&mut hash, true);
            checksum_u64(
                &mut hash,
                match resolution.kind {
                    ConflictResolutionKind::WhitePeace => 0,
                    ConflictResolutionKind::FullCapitulation => 1,
                },
            );
            checksum_u64(
                &mut hash,
                resolution.winner_side.map_or(u64::MAX, u64::from),
            );
        } else {
            checksum_bool(&mut hash, false);
        }
    } else {
        checksum_bool(&mut hash, false);
    }
    format!("{hash:016x}")
}

fn parse_fixture_args(args: Vec<String>) -> Result<(PathBuf, bool)> {
    let mut path = None;
    let mut compact = false;
    for arg in args {
        if arg == "--json" {
            compact = true
        } else if arg.starts_with('-') {
            bail!("unknown strategic-cycle-fixture option {arg:?}")
        } else if path.is_none() {
            path = Some(PathBuf::from(arg))
        } else {
            bail!("strategic-cycle-fixture accepts one fixture path")
        }
    }
    Ok((
        path.context("usage: strategic-cycle-fixture <fixture.json> [--json]")?,
        compact,
    ))
}
fn parse_bench_args(args: Vec<String>) -> Result<(PathBuf, usize, usize, bool)> {
    let mut path = None;
    let (mut repeat, mut warmup, mut compact) = (7, 2, false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repeat" => {
                i += 1;
                repeat = args.get(i).context("--repeat needs a value")?.parse()?
            }
            "--warmup" => {
                i += 1;
                warmup = args.get(i).context("--warmup needs a value")?.parse()?
            }
            "--json" => compact = true,
            arg if arg.starts_with('-') => bail!("unknown strategic-cycle-bench option {arg:?}"),
            arg if path.is_none() => path = Some(PathBuf::from(arg)),
            _ => bail!("strategic-cycle-bench accepts one fixture path"),
        };
        i += 1
    }
    if repeat == 0 {
        bail!("--repeat must be at least 1")
    }
    Ok((
        path.context(
            "usage: strategic-cycle-bench <fixture.json> [--repeat N] [--warmup N] [--json]",
        )?,
        repeat,
        warmup,
        compact,
    ))
}
fn print(value: &Value, compact: bool) -> Result<()> {
    println!(
        "{}",
        if compact {
            serde_json::to_string(value)?
        } else {
            serde_json::to_string_pretty(value)?
        }
    );
    Ok(())
}
fn load(path: &PathBuf) -> Result<Fixture> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let fixture: Fixture = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if fixture.schema_version != STRATEGIC_SCHEMA_VERSION {
        bail!("unsupported schemaVersion {:?}", fixture.schema_version)
    }
    Ok(fixture)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    schema_version: String,
    economy_seeds: Vec<Seed>,
    #[serde(default)]
    occupation_states: Vec<OccupationState>,
    #[serde(default)]
    cycles: Vec<Cycle>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Seed {
    country_id: u16,
    #[serde(default)]
    gdp: f64,
    #[serde(default)]
    population: f64,
    #[serde(default)]
    territory_units: f64,
    #[serde(default)]
    initial_core_cells: u32,
    #[serde(default, alias = "initialCityPopulation")]
    initial_city_pop: f64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cycle {
    tick: u64,
    #[serde(default)]
    force: bool,
    territory_generation: u64,
    territory_commit_sequence: u64,
    territory_fresh: bool,
    countries: Vec<mw_core::CountryCycleInput>,
    #[serde(default)]
    occupations: Vec<mw_core::strategic::OccupationCycleRecord>,
    #[serde(default)]
    active_sides: Vec<u16>,
    #[serde(default)]
    active_hostile_pairs: Vec<(u16, u16)>,
    #[serde(default)]
    expected_error: Option<String>,
}
fn simulation(fixture: &Fixture) -> Result<StrategicSimulation> {
    let economies = fixture
        .economy_seeds
        .iter()
        .map(|s| {
            create_economy_state(EconomySeed {
                country_id: s.country_id,
                gdp: s.gdp,
                population: s.population,
                territory_units: s.territory_units,
                initial_core_cells: s.initial_core_cells,
                initial_city_population: s.initial_city_pop,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StrategicSimulation::new(
        economies,
        fixture.occupation_states.clone(),
    )?)
}
fn economy_json(value: &mw_core::EconomyState) -> Value {
    serde_json::to_value(value).expect("economy serializes")
}
fn snapshot_json(snapshot: &mw_core::StrategicSnapshot) -> Value {
    json!({"schemaVersion":snapshot.schema_version,"cycle":snapshot.cycle,"tick":snapshot.tick,"territoryGeneration":snapshot.territory_generation,"territoryCommitSequence":snapshot.territory_commit_sequence,"countries":snapshot.countries.iter().map(|country|json!({"countryId":country.country_id,"side":country.side,"economy":economy_json(&country.economy),"capitulation":country.capitulation})).collect::<Vec<_>>(),"occupations":snapshot.occupations.as_ref().to_vec(),"occupationAssessments":snapshot.occupation_assessments.as_ref().to_vec(),"desertions":snapshot.desertions.as_ref().to_vec(),"surrenders":snapshot.surrenders.as_ref().to_vec(),"events":snapshot.events.as_ref().to_vec(),"conflictResolution":snapshot.conflict_resolution})
}
fn report(fixture: &Fixture) -> Result<Value> {
    let mut simulation = simulation(fixture)?;
    let mut cycles = Vec::new();
    for (index, source) in fixture.cycles.iter().enumerate() {
        let input = cycle_input(source);
        let before = state_json(&simulation);
        match simulation.run_cycle(&input){Ok((snapshot,counters))=>cycles.push(json!({"cycleIndex":index,"snapshot":snapshot_json(&snapshot),"counters":counters})),Err(error)if source.expected_error.as_deref()==Some(&error.to_string())=>cycles.push(json!({"cycleIndex":index,"error":error.to_string(),"atomic":before==state_json(&simulation)})),Err(error)=>return Err(error.into())}
    }
    Ok(
        json!({"schemaVersion":STRATEGIC_SCHEMA_VERSION,"cycles":cycles,"final":{"cycle":simulation.cycle(),"economies":simulation.economies().values().map(economy_json).collect::<Vec<_>>(),"occupations":simulation.occupations().values().collect::<Vec<_>>(),"latest":simulation.latest_snapshot().as_deref().map(snapshot_json)}}),
    )
}
fn cycle_input(source: &Cycle) -> StrategicCycleInput {
    StrategicCycleInput {
        tick: source.tick,
        force: source.force,
        territory_generation: source.territory_generation,
        territory_commit_sequence: source.territory_commit_sequence,
        territory_fresh: source.territory_fresh,
        countries: source.countries.clone(),
        occupations: source.occupations.clone(),
        active_sides: source.active_sides.clone(),
        active_hostile_pairs: source.active_hostile_pairs.clone(),
    }
}
fn state_json(simulation: &StrategicSimulation) -> Value {
    json!({"cycle":simulation.cycle(),"economies":simulation.economies().values().map(economy_json).collect::<Vec<_>>(),"occupations":simulation.occupations().values().collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_parses_and_report_is_stable() {
        let fixture = load(&PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/strategic-cycle-v1.json"
        )))
        .unwrap();
        let first = report(&fixture).unwrap();
        assert_eq!(first, report(&fixture).unwrap());
        assert_eq!(first["schemaVersion"], STRATEGIC_SCHEMA_VERSION);
    }
}
