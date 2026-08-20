//! Command-line adapter for the production native runtime.
//!
//! The checkpoint is intentionally policy-complete: scenarios supply geography,
//! cities, and economy baselines, while the checkpoint explicitly supplies the
//! active coalitions, directed hostility, live units, and per-unit policies.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    hint::black_box,
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use mw_core::{
    ARMOR_PAYROLL_PER_100, CombatConfig, CombatEvent, CombatLayer, CombatUnit, CommandBand,
    DecodedScenario, EconomyState, GridSpec, NATIVE_RUNTIME_SCHEMA_VERSION, NativeRuntime,
    OccupationState, PAYROLL_PER_UNIT, ProductionConfig, ResolvedCombatModifiers,
    ResolvedMovementModifiers, RuntimeCheckpoint, RuntimeConfig, RuntimeDiplomacy, RuntimeSnapshot,
    RuntimeState, RuntimeUnitPolicy, STARTING_RESERVE_CYCLES, ScenarioProduction, Simulation,
    SimulationConfig, SimulationUnit, StrategicSimulation, TARGET_STARTING_PAYROLL_SHARE,
    TerritoryCity, TerritoryConfig, TerritoryControl, TerritoryMaps, UnitAiPolicy,
    UnitInfluencePolicy, UnitKind, decode_mwsc_gzip, derive_scenario_production,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const NATIVE_RUNTIME_CHECKPOINT_SCHEMA: &str = "native-runtime-checkpoint-v1";

const DEFAULT_GRID_RESOLUTION: f64 = 0.15;
const DEFAULT_BENCH_REPEAT: usize = 7;
const DEFAULT_BENCH_WARMUP: usize = 2;
const TERRITORY_TILE_SIZE: usize = 32;
const FNV64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;
const PERSONNEL_PER_FORMATION: f64 = 1_000.0;

pub fn run_production_inspect_command(args: Vec<String>) -> Result<()> {
    let options = parse_production_inspect_args(args)?;
    let raw = read_file(&options.scenario_path)?;
    let sha256 = sha256_hex(&raw);
    let target = GridSpec::world(options.grid_res)?;
    let decoded = decode_mwsc_gzip(&raw, Some(target))?;
    let production = derive_scenario_production(&decoded, &ProductionConfig::default())?;
    let scenario_name = scenario_name(&decoded);
    let selected = options
        .country
        .as_deref()
        .map(|selector| selected_country_report(selector, &production))
        .transpose()?;

    let report = ProductionInspectReport {
        schema: mw_core::PRODUCTION_SCHEMA_VERSION,
        scenario: ScenarioIdentityReport {
            sha256,
            name: scenario_name,
            source_grid: GridReport::from(decoded.source),
            target_grid: GridReport::from(decoded.target),
            entry_count: decoded.entry_count,
        },
        counters: ProductionCountersReport::from(&production),
        selected_country: selected,
    };
    print_json(&report, options.compact)
}

pub fn run_fixture_command(args: Vec<String>) -> Result<()> {
    let options = parse_fixture_args(args)?;
    let prepared = prepare_runtime(&options.scenario_path, &options.checkpoint_path)?;
    let requested_steps = options.ticks.unwrap_or(prepared.checkpoint.steps);
    let execution = execute_fixture(&prepared, requested_steps)?;
    let body = RuntimeFixtureReportBody {
        schema: NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
        runtime_schema: NATIVE_RUNTIME_SCHEMA_VERSION,
        checkpoint_boundary: prepared.boundary_report(),
        scenario: prepared.identity_report(),
        checkpoint: prepared.checkpoint_report(),
        production: prepared.production_report(),
        requested_steps,
        completed_steps: execution.steps.len(),
        render_updates_drained: execution.render_updates_drained,
        initial: execution.initial,
        steps: execution.steps,
    };
    let checksum = checksum_serializable(&body)?;
    print_json(&RuntimeFixtureReport { body, checksum }, options.compact)
}

pub fn run_bench_command(args: Vec<String>) -> Result<()> {
    let options = parse_bench_args(args)?;
    let prepared = prepare_runtime(&options.scenario_path, &options.checkpoint_path)?;
    let ticks = options.ticks.unwrap_or(prepared.checkpoint.steps);

    // Establish determinism outside the timed samples. Scenario parsing,
    // decompression, production derivation, and runtime construction are all
    // deliberately excluded from the reported kernel time.
    let left = execute_benchmark_fresh(&prepared, ticks)?;
    let right = execute_benchmark_fresh(&prepared, ticks)?;
    if left.completed_steps != right.completed_steps
        || left.gated != right.gated
        || left.checksum != right.checksum
    {
        bail!("fresh native runtime executions are not deterministic");
    }
    if left.completed_steps != ticks {
        bail!(
            "native runtime reached its strategic-effects gate after {} of {ticks} requested ticks",
            left.completed_steps
        );
    }

    for _ in 0..options.warmup {
        black_box(execute_benchmark_fresh(&prepared, ticks)?);
    }

    let mut fresh_samples = Vec::with_capacity(options.repeat);
    let mut fresh_checksum = None;
    let mut fresh_render_updates = 0_usize;
    for _ in 0..options.repeat {
        let sample = execute_benchmark_fresh(&prepared, ticks)?;
        if sample.completed_steps != ticks || sample.gated {
            bail!("fresh runtime benchmark was interrupted by strategic effects");
        }
        if fresh_checksum
            .as_ref()
            .is_some_and(|expected| expected != &sample.checksum)
        {
            bail!("fresh runtime benchmark produced divergent checksums");
        }
        fresh_checksum.get_or_insert_with(|| sample.checksum.clone());
        fresh_render_updates = fresh_render_updates.saturating_add(sample.render_updates_drained);
        fresh_samples.push(sample.elapsed_ms);
    }
    let fresh = benchmark_mode_report(
        &fresh_samples,
        options.repeat,
        ticks,
        fresh_render_updates,
        fresh_checksum.context("benchmark produced no fresh checksum")?,
    );

    // This second mode measures a truly persistent runtime rather than a fresh
    // checkpoint per sample. It is published only while the strategic-effects
    // gate remains clear; callers must never silently acknowledge commands.
    let persistent = execute_persistent_benchmark(&prepared, &options, ticks)?;
    let body = RuntimeBenchReportBody {
        schema: NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
        runtime_schema: NATIVE_RUNTIME_SCHEMA_VERSION,
        mode: "bench",
        checkpoint_boundary: prepared.boundary_report(),
        geography: prepared.geography_report(),
        scenario: prepared.identity_report(),
        units: prepared.checkpoint.units.len(),
        sides: prepared.checkpoint.sides.len(),
        countries: prepared.country_to_side.len(),
        repeat: options.repeat,
        warmup: options.warmup,
        ticks_per_sample: ticks,
        fresh,
        persistent,
    };
    let checksum = benchmark_semantic_checksum(&body)?;
    print_json(&RuntimeBenchReport { body, checksum }, options.compact)
}

fn benchmark_semantic_checksum(body: &RuntimeBenchReportBody) -> Result<String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SemanticBenchmark<'a> {
        schema: &'a str,
        runtime_schema: &'a str,
        checkpoint_boundary: CheckpointBoundary,
        scenario_sha256: &'a str,
        units: usize,
        sides: usize,
        countries: usize,
        repeat: usize,
        ticks_per_sample: usize,
        fresh_state_checksum: &'a str,
        persistent_available: bool,
        persistent_gated: bool,
        persistent_completed_samples: usize,
        persistent_completed_ticks: usize,
        persistent_state_checksum: &'a str,
    }

    checksum_serializable(&SemanticBenchmark {
        schema: body.schema,
        runtime_schema: body.runtime_schema,
        checkpoint_boundary: body.checkpoint_boundary.kind,
        scenario_sha256: &body.scenario.sha256,
        units: body.units,
        sides: body.sides,
        countries: body.countries,
        repeat: body.repeat,
        ticks_per_sample: body.ticks_per_sample,
        fresh_state_checksum: &body.fresh.checksum,
        persistent_available: body.persistent.available,
        persistent_gated: body.persistent.gated,
        persistent_completed_samples: body.persistent.completed_samples,
        persistent_completed_ticks: body.persistent.completed_ticks,
        persistent_state_checksum: &body.persistent.final_checksum,
    })
}

#[derive(Debug)]
struct ProductionInspectOptions {
    scenario_path: PathBuf,
    grid_res: f64,
    country: Option<String>,
    compact: bool,
}

fn parse_production_inspect_args(args: Vec<String>) -> Result<ProductionInspectOptions> {
    let mut scenario_path = None;
    let mut grid_res = DEFAULT_GRID_RESOLUTION;
    let mut country = None;
    let mut compact = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--grid-res" => {
                index += 1;
                grid_res = args
                    .get(index)
                    .context("--grid-res needs a value")?
                    .parse()
                    .context("invalid --grid-res")?;
            }
            "--country" => {
                index += 1;
                country = Some(
                    args.get(index)
                        .context("--country needs an id or name")?
                        .clone(),
                );
            }
            "--json" => compact = true,
            flag if flag.starts_with('-') => bail!("unknown production-inspect option {flag:?}"),
            value if scenario_path.is_none() => scenario_path = Some(PathBuf::from(value)),
            _ => bail!("production-inspect accepts exactly one scenario path"),
        }
        index += 1;
    }
    if !grid_res.is_finite() || grid_res <= 0.0 {
        bail!("--grid-res must be finite and positive");
    }
    Ok(ProductionInspectOptions {
        scenario_path: scenario_path.context(
            "usage: production-inspect <scenario.mwsc.gz> [--grid-res N] [--country ID|NAME] [--json]",
        )?,
        grid_res,
        country,
        compact,
    })
}

#[derive(Debug)]
struct FixtureOptions {
    scenario_path: PathBuf,
    checkpoint_path: PathBuf,
    ticks: Option<usize>,
    compact: bool,
}

fn parse_fixture_args(args: Vec<String>) -> Result<FixtureOptions> {
    let mut paths = Vec::new();
    let mut ticks = None;
    let mut compact = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--ticks" => {
                index += 1;
                ticks = Some(
                    args.get(index)
                        .context("--ticks needs a value")?
                        .parse()
                        .context("invalid --ticks")?,
                );
            }
            "--json" => compact = true,
            flag if flag.starts_with('-') => {
                bail!("unknown native-runtime-fixture option {flag:?}")
            }
            value => paths.push(PathBuf::from(value)),
        }
        index += 1;
    }
    if paths.len() != 2 || ticks == Some(0) {
        bail!(
            "usage: native-runtime-fixture <scenario.mwsc.gz> <checkpoint.json> [--ticks N] [--json]"
        );
    }
    Ok(FixtureOptions {
        scenario_path: paths.remove(0),
        checkpoint_path: paths.remove(0),
        ticks,
        compact,
    })
}

#[derive(Debug)]
struct BenchOptions {
    scenario_path: PathBuf,
    checkpoint_path: PathBuf,
    repeat: usize,
    warmup: usize,
    ticks: Option<usize>,
    compact: bool,
}

fn parse_bench_args(args: Vec<String>) -> Result<BenchOptions> {
    let mut paths = Vec::new();
    let mut repeat = DEFAULT_BENCH_REPEAT;
    let mut warmup = DEFAULT_BENCH_WARMUP;
    let mut ticks = None;
    let mut compact = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--repeat" => {
                index += 1;
                repeat = args
                    .get(index)
                    .context("--repeat needs a value")?
                    .parse()
                    .context("invalid --repeat")?;
            }
            "--warmup" => {
                index += 1;
                warmup = args
                    .get(index)
                    .context("--warmup needs a value")?
                    .parse()
                    .context("invalid --warmup")?;
            }
            "--ticks" => {
                index += 1;
                ticks = Some(
                    args.get(index)
                        .context("--ticks needs a value")?
                        .parse()
                        .context("invalid --ticks")?,
                );
            }
            "--json" => compact = true,
            flag if flag.starts_with('-') => bail!("unknown native-runtime-bench option {flag:?}"),
            value => paths.push(PathBuf::from(value)),
        }
        index += 1;
    }
    if paths.len() != 2 || repeat == 0 || ticks == Some(0) {
        bail!(
            "usage: native-runtime-bench <scenario.mwsc.gz> <checkpoint.json> [--repeat N] [--warmup N] [--ticks N] [--json]"
        );
    }
    Ok(BenchOptions {
        scenario_path: paths.remove(0),
        checkpoint_path: paths.remove(0),
        repeat,
        warmup,
        ticks,
        compact,
    })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeCheckpointFixture {
    schema: String,
    checkpoint_boundary: CheckpointBoundary,
    scenario: ScenarioExpectation,
    #[serde(default)]
    geography: Option<GeographyFixture>,
    sides: Vec<SideFixture>,
    active_sides: Vec<u16>,
    hostility_matrix: Vec<u8>,
    tick: u64,
    frame: u64,
    war_grace_end: u64,
    strategic_cycle: u64,
    steps: usize,
    units: Vec<RuntimeUnitFixture>,
    economies: Vec<EconomyFixture>,
    #[serde(default)]
    occupations: Vec<OccupationFixture>,
    #[serde(default)]
    casualties: BTreeMap<u16, f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeographyFixture {
    land_runs: Vec<(u64, u8)>,
    world_control_runs: Vec<(u64, u16)>,
    de_jure_runs: Vec<(u64, u16)>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum CheckpointBoundary {
    PostStartWar,
    BaselineReplay,
}

impl CheckpointBoundary {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PostStartWar => "postStartWar",
            Self::BaselineReplay => "baselineReplay",
        }
    }

    const fn resumable(self) -> bool {
        matches!(self, Self::PostStartWar)
    }

    const fn description(self) -> &'static str {
        match self {
            Self::PostStartWar => {
                "production-resumable checkpoint captured before the first simulation tick"
            }
            Self::BaselineReplay => {
                "synthetic baseline replay for fixtures and benchmarks; not resumable game state"
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScenarioExpectation {
    sha256: String,
    name: String,
    grid_res: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SideFixture {
    side_index: u16,
    country_ids: Vec<u16>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UnitKindFixture {
    Army,
    Armor,
}

impl From<UnitKindFixture> for UnitKind {
    fn from(value: UnitKindFixture) -> Self {
        match value {
            UnitKindFixture::Army => Self::Army,
            UnitKindFixture::Armor => Self::Armor,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeUnitFixture {
    id: u64,
    side: u16,
    country_id: u16,
    kind: UnitKindFixture,
    lat: f64,
    lng: f64,
    health: f64,
    max_health: f64,
    personnel: u64,
    personnel_capacity: u64,
    equipment: u64,
    max_equipment: u64,
    quality: f64,
    transport: bool,
    armor_supported: bool,
    landing_penalty_active: bool,
    at_sea: bool,
    last_combat_tick: u64,
    victory_boost_ticks: u64,
    dir_lat: f64,
    dir_lng: f64,
    coast_stuck_ticks: u32,
    armor_landing_penalty_until_tick: u64,
    is_support: bool,
    ally_weight: f64,
    ai_policy: AiPolicyFixture,
    influence_policy: Option<InfluencePolicyFixture>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AiPolicyFixture {
    base_speed: f64,
    terrain_speed_multiplier: f64,
    speed_multiplier: f64,
    plan_speed_multiplier: f64,
    neutral_penalty: f64,
    push_readiness: f64,
    dealt_multiplier: f64,
    taken_multiplier: f64,
    defense_bonus: f64,
    long_war_defense: f64,
    mountain: bool,
    urban: bool,
    is_reserve: bool,
    reinforcement_eligible: bool,
    encircled: bool,
    deploy_until_tick: u64,
    garrison_excluded: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InfluencePolicyFixture {
    radius: f64,
    delta: f64,
    concentration_bonus: f64,
    beneficiary_country_id: Option<u16>,
    protected_owner_ids: Vec<u16>,
    rebel_de_jure: Option<u16>,
    credit_de_jure: Option<u16>,
    credit_de_jure_by_country: BTreeMap<u16, u16>,
    refuses_offense: bool,
    #[serde(default)]
    temporal_seed: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OccupationFixture {
    victim_id: u16,
    annexer_id: u16,
    base_income: f64,
    core_cells: u32,
    expected_army_units: f64,
    resistance: f64,
    occupation_coverage: f64,
    garrison_coverage: f64,
    garrison_assigned: f64,
    required_garrison: u32,
    held_ratio: f64,
    active_rebellion: bool,
    queued_at_cycle: u64,
    cooldown_until_cycle: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EconomyFixture {
    country_id: u16,
    economic_strength: f64,
    base_income: f64,
    treasury: f64,
    income: f64,
    occupation_yield: f64,
    payroll_due: f64,
    occupation_due: f64,
    payroll_coverage: f64,
    occupation_coverage: f64,
    arrears_cycles: f64,
    command_band: CommandBand,
    mutiny_recovery_cycles: u32,
    initial_core_cells: u32,
    initial_city_population: f64,
    core_control_ratio: f64,
    city_control_ratio: f64,
    capital_held: bool,
    last_event_band: CommandBand,
    capitulated: bool,
}

impl From<&EconomyFixture> for EconomyState {
    fn from(value: &EconomyFixture) -> Self {
        Self {
            country_id: value.country_id,
            economic_strength: value.economic_strength,
            base_income: value.base_income,
            treasury: value.treasury,
            income: value.income,
            occupation_yield: value.occupation_yield,
            payroll_due: value.payroll_due,
            occupation_due: value.occupation_due,
            payroll_coverage: value.payroll_coverage,
            occupation_coverage: value.occupation_coverage,
            arrears_cycles: value.arrears_cycles,
            command_band: value.command_band,
            mutiny_recovery_cycles: value.mutiny_recovery_cycles,
            initial_core_cells: value.initial_core_cells,
            initial_city_population: value.initial_city_population,
            core_control_ratio: value.core_control_ratio,
            city_control_ratio: value.city_control_ratio,
            capital_held: value.capital_held,
            last_event_band: value.last_event_band,
            capitulated: value.capitulated,
        }
    }
}

impl From<&OccupationFixture> for OccupationState {
    fn from(value: &OccupationFixture) -> Self {
        Self {
            victim_id: value.victim_id,
            annexer_id: value.annexer_id,
            base_income: value.base_income,
            core_cells: value.core_cells,
            expected_army_units: value.expected_army_units,
            resistance: value.resistance,
            occupation_coverage: value.occupation_coverage,
            garrison_coverage: value.garrison_coverage,
            garrison_assigned: value.garrison_assigned,
            required_garrison: value.required_garrison,
            held_ratio: value.held_ratio,
            active_rebellion: value.active_rebellion,
            queued_at_cycle: value.queued_at_cycle,
            cooldown_until_cycle: value.cooldown_until_cycle,
        }
    }
}

struct PreparedRuntime {
    raw_sha256: String,
    scenario_name: String,
    decoded: DecodedScenario,
    production: ScenarioProduction,
    checkpoint: RuntimeCheckpointFixture,
    country_to_side: BTreeMap<u16, usize>,
    coalition_by_side: Vec<BTreeSet<u16>>,
}

/// Fully validated production handoff for native rendering and simulation.
///
/// `decoded` contains the checkpoint's authoritative exact geography. The
/// runtime owns its independent mutable territory state, so the renderer can
/// move these decoded maps into its immutable/GPU caches without decoding the
/// scenario a second time.
pub struct LoadedRuntime {
    pub decoded: DecodedScenario,
    pub runtime: NativeRuntime,
    pub checkpoint_boundary: &'static str,
    pub resumable: bool,
    pub exact_geography_supplied: bool,
    pub unit_count: usize,
}

/// Read, strictly validate, and build one browser-to-native runtime handoff.
///
/// This is the same path used by `native-runtime-fixture` and
/// `native-runtime-bench`; the viewer does not have a weaker fallback parser.
pub fn load_runtime_checkpoint(
    scenario_path: &PathBuf,
    checkpoint_path: &PathBuf,
) -> Result<LoadedRuntime> {
    let prepared = prepare_runtime(scenario_path, checkpoint_path)?;
    let runtime = build_runtime(&prepared)?;
    let checkpoint_boundary = prepared.checkpoint.checkpoint_boundary.as_str();
    let resumable = prepared.checkpoint.checkpoint_boundary.resumable();
    let exact_geography_supplied = prepared.checkpoint.geography.is_some();
    let unit_count = prepared.checkpoint.units.len();
    Ok(LoadedRuntime {
        decoded: prepared.decoded,
        runtime,
        checkpoint_boundary,
        resumable,
        exact_geography_supplied,
        unit_count,
    })
}

type RuntimeTopology = (BTreeMap<u16, usize>, Vec<BTreeSet<u16>>);

struct DecodedGeography {
    land: Vec<u8>,
    world_control: Vec<u16>,
    de_jure: Vec<u16>,
}

fn prepare_runtime(scenario_path: &PathBuf, checkpoint_path: &PathBuf) -> Result<PreparedRuntime> {
    let checkpoint_bytes = read_file(checkpoint_path)?;
    let checkpoint: RuntimeCheckpointFixture = serde_json::from_slice(&checkpoint_bytes)
        .with_context(|| format!("failed to parse {}", checkpoint_path.display()))?;
    validate_checkpoint_shape(&checkpoint)?;

    let raw = read_file(scenario_path)?;
    let raw_sha256 = sha256_hex(&raw);
    if raw_sha256 != checkpoint.scenario.sha256.to_ascii_lowercase() {
        bail!(
            "scenario SHA-256 mismatch: checkpoint expects {}, loaded {raw_sha256}",
            checkpoint.scenario.sha256
        );
    }
    let target = GridSpec::world(checkpoint.scenario.grid_res)?;
    let mut decoded = decode_mwsc_gzip(&raw, Some(target))?;
    let scenario_name = scenario_name(&decoded);
    if scenario_name != checkpoint.scenario.name {
        bail!(
            "scenario name mismatch: checkpoint expects {:?}, loaded {:?}",
            checkpoint.scenario.name,
            scenario_name
        );
    }
    if let Some(geography) = checkpoint.geography.as_ref() {
        // Decode all three streams before replacing any scenario map. A bad
        // stream therefore cannot leave a partially applied checkpoint.
        let exact = decode_geography(geography, decoded.target.cell_count()?)?;
        decoded.land = exact.land;
        decoded.world_control = exact.world_control;
        decoded.de_jure = exact.de_jure;
    }
    let production = derive_scenario_production(&decoded, &ProductionConfig::default())?;
    if checkpoint.geography.is_some() {
        validate_exact_geography_owners(&decoded, &production)?;
    }
    let (country_to_side, coalition_by_side) = topology(&checkpoint)?;
    validate_checkpoint_against_scenario(&checkpoint, &production, &country_to_side)?;

    Ok(PreparedRuntime {
        raw_sha256,
        scenario_name,
        decoded,
        production,
        checkpoint,
        country_to_side,
        coalition_by_side,
    })
}

fn decode_geography(geography: &GeographyFixture, cell_count: usize) -> Result<DecodedGeography> {
    let land = decode_runs(
        &geography.land_runs,
        cell_count,
        "geography.landRuns",
        |value| (*value <= 1).then_some(*value),
    )?;
    let world_control = decode_runs(
        &geography.world_control_runs,
        cell_count,
        "geography.worldControlRuns",
        |value| Some(*value),
    )?;
    let de_jure = decode_runs(
        &geography.de_jure_runs,
        cell_count,
        "geography.deJureRuns",
        |value| Some(*value),
    )?;
    Ok(DecodedGeography {
        land,
        world_control,
        de_jure,
    })
}

fn decode_runs<T, U>(
    runs: &[(u64, T)],
    cell_count: usize,
    label: &str,
    validate_value: impl Fn(&T) -> Option<U>,
) -> Result<Vec<U>>
where
    U: Copy,
{
    let mut decoded = Vec::with_capacity(cell_count);
    for (index, (raw_length, raw_value)) in runs.iter().enumerate() {
        if *raw_length == 0 {
            bail!("{label}[{index}] has a zero run length");
        }
        let length = usize::try_from(*raw_length)
            .with_context(|| format!("{label}[{index}] run length exceeds this platform"))?;
        let end = decoded
            .len()
            .checked_add(length)
            .with_context(|| format!("{label}[{index}] run coverage overflow"))?;
        if end > cell_count {
            bail!("{label}[{index}] exceeds the target grid coverage of {cell_count} cells");
        }
        let value = validate_value(raw_value)
            .with_context(|| format!("{label}[{index}] contains an invalid value"))?;
        decoded.resize(end, value);
    }
    if decoded.len() != cell_count {
        bail!(
            "{label} covers {} cells; expected exactly {cell_count}",
            decoded.len()
        );
    }
    Ok(decoded)
}

fn validate_exact_geography_owners(
    decoded: &DecodedScenario,
    production: &ScenarioProduction,
) -> Result<()> {
    let known = production
        .countries
        .iter()
        .map(|country| country.country_id)
        .collect::<BTreeSet<_>>();
    validate_exact_geography_owner_ids(&decoded.world_control, &decoded.de_jure, &known)
}

fn validate_exact_geography_owner_ids(
    world_control: &[u16],
    de_jure: &[u16],
    known: &BTreeSet<u16>,
) -> Result<()> {
    for (label, owners) in [
        ("geography.worldControlRuns", world_control),
        ("geography.deJureRuns", de_jure),
    ] {
        if let Some(owner) = owners
            .iter()
            .copied()
            .find(|owner| *owner != 0 && !known.contains(owner))
        {
            bail!("{label} references country {owner}, which is absent from scenario metadata");
        }
    }
    Ok(())
}

fn validate_checkpoint_shape(checkpoint: &RuntimeCheckpointFixture) -> Result<()> {
    if checkpoint.schema != NATIVE_RUNTIME_CHECKPOINT_SCHEMA {
        bail!(
            "unsupported native runtime checkpoint schema {:?}; expected {:?}",
            checkpoint.schema,
            NATIVE_RUNTIME_CHECKPOINT_SCHEMA
        );
    }
    if checkpoint.steps == 0 {
        bail!("native runtime checkpoint must request at least one step");
    }
    if checkpoint.sides.is_empty() || checkpoint.sides.len() > u16::MAX as usize + 1 {
        bail!("native runtime checkpoint must contain a bounded, non-empty side list");
    }
    if checkpoint.scenario.sha256.len() != 64
        || !checkpoint
            .scenario
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("scenario.sha256 must be a 64-character hexadecimal SHA-256 digest");
    }
    if !checkpoint.scenario.grid_res.is_finite() || checkpoint.scenario.grid_res <= 0.0 {
        bail!("scenario.gridRes must be finite and positive");
    }
    let side_count = checkpoint.sides.len();
    if checkpoint.hostility_matrix.len() != side_count.saturating_mul(side_count) {
        bail!("hostilityMatrix must be a directed sideCount by sideCount matrix");
    }
    for left in 0..side_count {
        for right in 0..side_count {
            let value = checkpoint.hostility_matrix[left * side_count + right];
            if value > 1 || (left == right && value != 0) {
                bail!("hostilityMatrix must be binary with a zero diagonal");
            }
        }
    }
    let mut active = BTreeSet::new();
    for &side in &checkpoint.active_sides {
        if usize::from(side) >= side_count || !active.insert(side) {
            bail!("activeSides must be unique and reference declared sides");
        }
    }
    if checkpoint.active_sides.is_empty() {
        bail!("activeSides must explicitly name at least one active side");
    }
    validate_geography_boundary(
        checkpoint.checkpoint_boundary,
        checkpoint.geography.as_ref(),
    )?;
    Ok(())
}

fn validate_geography_boundary(
    boundary: CheckpointBoundary,
    geography: Option<&GeographyFixture>,
) -> Result<()> {
    if boundary == CheckpointBoundary::PostStartWar && geography.is_none() {
        bail!("postStartWar checkpoint boundary requires exact geography");
    }
    Ok(())
}

fn topology(checkpoint: &RuntimeCheckpointFixture) -> Result<RuntimeTopology> {
    let mut country_to_side = BTreeMap::new();
    let mut coalitions = vec![BTreeSet::new(); checkpoint.sides.len()];
    for (expected, side) in checkpoint.sides.iter().enumerate() {
        if usize::from(side.side_index) != expected || side.country_ids.is_empty() {
            bail!("sides must be contiguous, ordered by sideIndex, and non-empty");
        }
        for &country_id in &side.country_ids {
            if country_id == 0 || country_to_side.insert(country_id, expected).is_some() {
                bail!("country id {country_id} is zero or appears in multiple sides");
            }
            coalitions[expected].insert(country_id);
        }
    }
    Ok((country_to_side, coalitions))
}

fn validate_checkpoint_against_scenario(
    checkpoint: &RuntimeCheckpointFixture,
    production: &ScenarioProduction,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<()> {
    let countries = production
        .countries
        .iter()
        .map(|country| (country.country_id, country))
        .collect::<BTreeMap<_, _>>();
    for &country_id in country_to_side.keys() {
        let country = countries
            .get(&country_id)
            .with_context(|| format!("checkpoint country {country_id} is absent from scenario"))?;
        if country.initial_core_cells == 0 {
            bail!("checkpoint country {country_id} has no active scenario core cells");
        }
    }

    let mut economy_ids = BTreeSet::new();
    for economy in &checkpoint.economies {
        if !economy_ids.insert(economy.country_id)
            || !country_to_side.contains_key(&economy.country_id)
        {
            bail!("economies must contain one unique state per declared country");
        }
        let scenario_country = countries[&economy.country_id];
        if economy.initial_core_cells != scenario_country.initial_core_cells
            || economy.initial_city_population.to_bits()
                != scenario_country.initial_city_population.to_bits()
        {
            bail!(
                "economy {} immutable scenario baselines do not match the loaded scenario",
                economy.country_id
            );
        }
        validate_economy_fixture(economy)?;
    }
    if economy_ids != country_to_side.keys().copied().collect() {
        bail!("economies must exactly cover every declared country");
    }

    let mut unit_ids = BTreeSet::new();
    for unit in &checkpoint.units {
        if unit.id == 0 || !unit_ids.insert(unit.id) {
            bail!("unit ids must be positive and unique");
        }
        let expected_side = country_to_side
            .get(&unit.country_id)
            .with_context(|| format!("unit {} has an undeclared country", unit.id))?;
        if *expected_side != usize::from(unit.side) {
            bail!("unit {} side and sovereign coalition disagree", unit.id);
        }
        if usize::from(unit.side) >= checkpoint.sides.len() {
            bail!("unit {} references an out-of-range side", unit.id);
        }
        validate_unit_fixture(unit)?;
        if let Some(policy) = &unit.influence_policy {
            validate_influence_references(unit.id, policy, country_to_side)?;
        }
    }

    let mut occupation_victims = BTreeSet::new();
    for occupation in &checkpoint.occupations {
        if !occupation_victims.insert(occupation.victim_id)
            || occupation.victim_id == occupation.annexer_id
            || !country_to_side.contains_key(&occupation.victim_id)
            || !country_to_side.contains_key(&occupation.annexer_id)
            || occupation.queued_at_cycle > checkpoint.strategic_cycle
        {
            bail!(
                "occupation records must be unique, stay inside declared coalitions, and not be queued in a future strategic cycle"
            );
        }
    }
    for (&country_id, casualties) in &checkpoint.casualties {
        if !country_to_side.contains_key(&country_id)
            || !casualties.is_finite()
            || *casualties < 0.0
        {
            bail!("casualty ledgers must be finite, non-negative, and use declared countries");
        }
    }
    validate_checkpoint_boundary(checkpoint, production, country_to_side)?;
    Ok(())
}

fn validate_checkpoint_boundary(
    checkpoint: &RuntimeCheckpointFixture,
    production: &ScenarioProduction,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<()> {
    if checkpoint.checkpoint_boundary == CheckpointBoundary::BaselineReplay {
        return Ok(());
    }
    if checkpoint.tick != 0 || checkpoint.frame != 0 || checkpoint.strategic_cycle != 0 {
        bail!("postStartWar checkpoint boundary requires tick=0, frame=0, and strategicCycle=0");
    }
    if !checkpoint.occupations.is_empty() {
        bail!("postStartWar checkpoint boundary cannot contain occupations");
    }
    let declared = country_to_side.keys().copied().collect::<BTreeSet<_>>();
    if checkpoint
        .casualties
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        != declared
        || checkpoint.casualties.values().any(|value| *value != 0.0)
    {
        bail!(
            "postStartWar checkpoint boundary requires an explicit zero casualty total for every declared country"
        );
    }

    let derived = production
        .economy_states
        .iter()
        .map(|state| (state.country_id, state))
        .collect::<BTreeMap<_, _>>();
    for economy in &checkpoint.economies {
        let expected = derived.get(&economy.country_id).with_context(|| {
            format!(
                "country {} has no derived starting economy",
                economy.country_id
            )
        })?;
        let mandatory_payroll = checkpoint
            .units
            .iter()
            .filter(|unit| unit.country_id == economy.country_id && unit.health > 0.0)
            .map(|unit| match unit.kind {
                UnitKindFixture::Armor => unit.equipment as f64 / 100.0 * ARMOR_PAYROLL_PER_100,
                UnitKindFixture::Army => {
                    unit.personnel_capacity as f64 / PERSONNEL_PER_FORMATION * PAYROLL_PER_UNIT
                }
            })
            .sum::<f64>();
        let expected_base_income = expected
            .base_income
            .max(mandatory_payroll / TARGET_STARTING_PAYROLL_SHARE);
        let expected_treasury = expected_base_income * STARTING_RESERVE_CYCLES;
        let starting_numeric_matches =
            approximately_equal(economy.economic_strength, expected.economic_strength)
                && approximately_equal(economy.base_income, expected_base_income)
                && approximately_equal(economy.income, expected_base_income)
                && approximately_equal(economy.treasury, expected_treasury)
                && economy.occupation_yield == 0.0
                && economy.payroll_due == 0.0
                && economy.occupation_due == 0.0
                && economy.payroll_coverage == 1.0
                && economy.occupation_coverage == 1.0
                && economy.arrears_cycles == 0.0
                && economy.core_control_ratio == 1.0
                && economy.city_control_ratio == 1.0;
        let starting_state_matches = economy.command_band == CommandBand::Paid
            && economy.mutiny_recovery_cycles == 0
            && economy.capital_held
            && economy.last_event_band == CommandBand::Paid
            && !economy.capitulated;
        if !starting_numeric_matches || !starting_state_matches {
            bail!(
                "postStartWar checkpoint boundary economy {} is not a pristine deployment-adjusted starting state",
                economy.country_id
            );
        }
    }
    Ok(())
}

fn approximately_equal(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= expected.abs().max(1.0) * 1e-12
}

fn validate_economy_fixture(economy: &EconomyFixture) -> Result<()> {
    let values = [
        economy.economic_strength,
        economy.base_income,
        economy.treasury,
        economy.income,
        economy.occupation_yield,
        economy.payroll_due,
        economy.occupation_due,
        economy.payroll_coverage,
        economy.occupation_coverage,
        economy.arrears_cycles,
        economy.initial_city_population,
        economy.core_control_ratio,
        economy.city_control_ratio,
    ];
    if economy.country_id == 0
        || economy.initial_core_cells == 0
        || values.iter().any(|value| !value.is_finite())
        || economy.economic_strength < 0.0
        || economy.base_income < 0.0
        || economy.treasury < 0.0
        || economy.payroll_due < 0.0
        || economy.occupation_due < 0.0
        || economy.arrears_cycles < 0.0
        || !(0.0..=1.0).contains(&economy.payroll_coverage)
        || !(0.0..=1.0).contains(&economy.occupation_coverage)
        || !(0.0..=1.0).contains(&economy.core_control_ratio)
        || !(0.0..=1.0).contains(&economy.city_control_ratio)
    {
        bail!(
            "economy {} contains invalid live numeric state",
            economy.country_id
        );
    }
    Ok(())
}

fn validate_unit_fixture(unit: &RuntimeUnitFixture) -> Result<()> {
    let ai = unit.ai_policy;
    let numeric = [
        unit.lat,
        unit.lng,
        unit.health,
        unit.max_health,
        unit.quality,
        unit.dir_lat,
        unit.dir_lng,
        unit.ally_weight,
        ai.base_speed,
        ai.terrain_speed_multiplier,
        ai.speed_multiplier,
        ai.plan_speed_multiplier,
        ai.neutral_penalty,
        ai.push_readiness,
        ai.dealt_multiplier,
        ai.taken_multiplier,
        ai.defense_bonus,
        ai.long_war_defense,
    ];
    if numeric.iter().any(|value| !value.is_finite())
        || unit.health < 0.0
        || unit.max_health <= 0.0
        || unit.health > unit.max_health
        || unit.personnel > unit.personnel_capacity
        || unit.equipment > unit.max_equipment
        || ai.base_speed < 0.0
        || ai.terrain_speed_multiplier < 0.0
        || ai.speed_multiplier < 0.0
        || ai.plan_speed_multiplier < 0.0
        || ai.neutral_penalty < 0.0
        || ai.push_readiness < 0.0
        || ai.dealt_multiplier < 0.0
        || ai.taken_multiplier < 0.0
        || ai.defense_bonus < 0.0
        || ai.long_war_defense < 0.0
    {
        bail!("unit {} contains invalid live or AI numeric state", unit.id);
    }
    if let Some(policy) = &unit.influence_policy
        && (![policy.radius, policy.delta, policy.concentration_bonus]
            .into_iter()
            .all(f64::is_finite)
            || policy.temporal_seed.is_some_and(|value| !value.is_finite())
            || policy.radius <= 0.0
            || policy.delta < 0.0
            || policy.concentration_bonus < 0.0)
    {
        bail!("unit {} contains an invalid influence policy", unit.id);
    }
    Ok(())
}

fn validate_influence_references(
    unit_id: u64,
    policy: &InfluencePolicyFixture,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<()> {
    for country in policy
        .beneficiary_country_id
        .iter()
        .chain(policy.protected_owner_ids.iter())
        .chain(policy.rebel_de_jure.iter())
        .chain(policy.credit_de_jure.iter())
        .chain(policy.credit_de_jure_by_country.keys())
        .chain(policy.credit_de_jure_by_country.values())
    {
        if *country == 0 || !country_to_side.contains_key(country) {
            bail!("unit {unit_id} influence policy references undeclared country {country}");
        }
    }
    Ok(())
}

impl RuntimeUnitFixture {
    fn simulation_unit(&self) -> SimulationUnit {
        SimulationUnit {
            combat: CombatUnit {
                id: self.id,
                side: u64::from(self.side),
                sovereign: u64::from(self.country_id),
                kind: self.kind.into(),
                lat: self.lat,
                lng: self.lng,
                health: self.health,
                max_health: self.max_health,
                personnel: self.personnel,
                personnel_capacity: self.personnel_capacity,
                equipment: self.equipment,
                max_equipment: self.max_equipment,
                quality: self.quality,
                transport: self.transport,
                armor_supported: self.armor_supported,
                landing_penalty_active: self.landing_penalty_active,
                at_sea: self.at_sea,
                last_combat_tick: self.last_combat_tick,
                victory_boost_ticks: self.victory_boost_ticks,
            },
            dir_lat: self.dir_lat,
            dir_lng: self.dir_lng,
            coast_stuck_ticks: self.coast_stuck_ticks,
            armor_landing_penalty_until_tick: self.armor_landing_penalty_until_tick,
            is_support: self.is_support,
            ally_weight: self.ally_weight,
        }
    }

    fn runtime_policy(&self, coalition: &BTreeSet<u16>) -> RuntimeUnitPolicy {
        let ai = self.ai_policy;
        RuntimeUnitPolicy {
            unit_id: self.id,
            ai: UnitAiPolicy {
                base_speed: ai.base_speed,
                movement: ResolvedMovementModifiers {
                    terrain_speed_multiplier: ai.terrain_speed_multiplier,
                    speed_multiplier: ai.speed_multiplier,
                    plan_speed_multiplier: ai.plan_speed_multiplier,
                    neutral_penalty: ai.neutral_penalty,
                    push_readiness: ai.push_readiness,
                },
                combat: ResolvedCombatModifiers {
                    dealt_multiplier: ai.dealt_multiplier,
                    taken_multiplier: ai.taken_multiplier,
                    defense_bonus: ai.defense_bonus,
                    long_war_defense: ai.long_war_defense,
                    mountain: ai.mountain,
                    urban: ai.urban,
                },
                is_reserve: ai.is_reserve,
                reinforcement_eligible: ai.reinforcement_eligible,
                encircled: ai.encircled,
                deploy_until_tick: ai.deploy_until_tick,
                garrison_excluded: ai.garrison_excluded,
            },
            influence: self.influence_policy.as_ref().map(|policy| {
                UnitInfluencePolicy {
                    radius: policy.radius,
                    delta: policy.delta,
                    concentration_bonus: policy.concentration_bonus,
                    beneficiary: policy.beneficiary_country_id,
                    // Coalition membership is a topology invariant rather than
                    // a duplicated, unit-local checkpoint assertion.
                    owner_ally_country_ids: coalition.clone(),
                    protected_owner_ids: policy.protected_owner_ids.iter().copied().collect(),
                    rebel_de_jure: policy.rebel_de_jure,
                    credit_de_jure: policy.credit_de_jure,
                    credit_de_jure_by_country: policy.credit_de_jure_by_country.clone(),
                    refuses_offense: policy.refuses_offense,
                    browser_temporal_seed: policy.temporal_seed,
                }
            }),
        }
    }
}

fn build_runtime(prepared: &PreparedRuntime) -> Result<NativeRuntime> {
    let checkpoint = &prepared.checkpoint;
    let units = checkpoint
        .units
        .iter()
        .map(RuntimeUnitFixture::simulation_unit)
        .collect::<Vec<_>>();
    let simulation = Simulation::new(
        SimulationConfig {
            tactical_cell_size: SimulationConfig::default().tactical_cell_size,
            combat: CombatConfig::default(),
        },
        units,
    )?;
    let unit_policies = checkpoint
        .units
        .iter()
        .map(|unit| {
            unit.runtime_policy(
                prepared
                    .coalition_by_side
                    .get(usize::from(unit.side))
                    .expect("checkpoint side validated"),
            )
        })
        .collect::<Vec<_>>();

    let territory = build_territory(prepared)?;
    let economies = checkpoint
        .economies
        .iter()
        .map(EconomyState::from)
        .collect::<Vec<_>>();
    if economies.len() != prepared.country_to_side.len() {
        bail!("scenario did not derive an economy for every checkpoint country");
    }
    let occupations = checkpoint
        .occupations
        .iter()
        .map(OccupationState::from)
        .collect::<Vec<_>>();
    let strategic =
        StrategicSimulation::restore(checkpoint.strategic_cycle, economies, occupations)?;
    let runtime_checkpoint = RuntimeCheckpoint {
        tick: checkpoint.tick,
        frame: checkpoint.frame,
        war_grace_end: checkpoint.war_grace_end,
        simulation,
        territory,
        strategic,
        scenario: prepared.production.clone(),
        diplomacy: RuntimeDiplomacy {
            hostility: checkpoint.hostility_matrix.clone(),
            active_sides: checkpoint.active_sides.clone(),
        },
        unit_policies,
        objectives: Vec::new(),
        prior_objective_by_unit: BTreeMap::new(),
        casualties: checkpoint.casualties.clone(),
    };
    Ok(NativeRuntime::new(
        RuntimeConfig::default(),
        runtime_checkpoint,
    )?)
}

fn build_territory(prepared: &PreparedRuntime) -> Result<TerritoryControl> {
    let decoded = &prepared.decoded;
    let cell_count = decoded.target.cell_count()?;
    let side_count = prepared.checkpoint.sides.len();
    let mut land = vec![0_u8; cell_count];
    let mut primary_occupier = vec![0_u16; cell_count];
    let mut dominant_side = vec![-1_i16; cell_count];
    let mut occupation = vec![0.0_f32; cell_count];
    let mut side_influence = (0..side_count)
        .map(|_| vec![0.0_f32; cell_count])
        .collect::<Vec<_>>();

    for cell in 0..cell_count {
        if decoded.land[cell] == 0 {
            continue;
        }
        let owner = decoded.world_control[cell];
        let Some(&side) = prepared.country_to_side.get(&owner) else {
            // Non-theater land remains traversable, but is excluded from the
            // active census and cannot fabricate a conflict or controller.
            land[cell] = 1;
            continue;
        };
        land[cell] = 2;
        primary_occupier[cell] = owner;
        dominant_side[cell] = i16::try_from(side).context("side exceeds territory width")?;
        side_influence[side][cell] = 1.0;
        occupation[cell] = if side.is_multiple_of(2) { 1.0 } else { -1.0 };
    }

    let cities = prepared
        .production
        .cities
        .iter()
        .map(|city| TerritoryCity {
            id: city.city_id,
            cell: city.cell,
            owner: city.owner_id,
            population: city.population,
            capital: city.capital,
        })
        .collect::<Vec<_>>();
    let protected_owner_ids = prepared.country_to_side.keys().copied().collect();
    Ok(TerritoryControl::new(TerritoryConfig {
        width: decoded.target.width,
        height: decoded.target.height,
        grid_resolution: decoded.target.grid_res,
        max_sides: side_count,
        tile_size: TERRITORY_TILE_SIZE,
        maps: TerritoryMaps {
            land,
            world_control: decoded.world_control.clone(),
            de_jure: decoded.de_jure.clone(),
            primary_occupier,
            dominant_side,
            occupation,
            side_influence,
        },
        country_to_side: prepared.country_to_side.clone(),
        hostility_matrix: prepared.checkpoint.hostility_matrix.clone(),
        cities,
        protected_owner_ids,
        topology_revision: 1,
        world_revision: 1,
        city_revision: 1,
    })?)
}

struct FixtureExecution {
    initial: RuntimeSnapshotReport,
    steps: Vec<RuntimeSnapshotReport>,
    render_updates_drained: usize,
}

fn execute_fixture(prepared: &PreparedRuntime, steps: usize) -> Result<FixtureExecution> {
    let mut runtime = build_runtime(prepared)?;
    let initial_snapshot = runtime.latest_snapshot();
    let initial = snapshot_report(&initial_snapshot, &prepared.country_to_side)?;
    let mut render_updates_drained = drain_render_updates(&mut runtime);
    let mut reports = Vec::with_capacity(steps);
    for _ in 0..steps {
        if !matches!(runtime.state(), RuntimeState::Running) {
            break;
        }
        let snapshot = runtime.step()?;
        reports.push(snapshot_report(&snapshot, &prepared.country_to_side)?);
        render_updates_drained += drain_render_updates(&mut runtime);
    }
    Ok(FixtureExecution {
        initial,
        steps: reports,
        render_updates_drained,
    })
}

fn drain_render_updates(runtime: &mut NativeRuntime) -> usize {
    let mut drained = 0;
    while runtime.pop_render_update().is_some() {
        drained += 1;
    }
    drained
}

struct TimedRuntimeSample {
    elapsed_ms: f64,
    completed_steps: usize,
    render_updates_drained: usize,
    gated: bool,
    checksum: String,
}

fn execute_benchmark_fresh(prepared: &PreparedRuntime, ticks: usize) -> Result<TimedRuntimeSample> {
    let mut runtime = build_runtime(prepared)?;
    let mut render_updates_drained = drain_render_updates(&mut runtime);
    let started = Instant::now();
    let (completed_steps, gated, drained) = execute_timed_steps(&mut runtime, ticks)?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    render_updates_drained += drained;
    let checksum = snapshot_checksum(&runtime.latest_snapshot())?;
    Ok(TimedRuntimeSample {
        elapsed_ms,
        completed_steps,
        render_updates_drained,
        gated,
        checksum,
    })
}

fn execute_timed_steps(runtime: &mut NativeRuntime, ticks: usize) -> Result<(usize, bool, usize)> {
    let mut completed = 0;
    let mut drained = 0;
    while completed < ticks {
        if !matches!(runtime.state(), RuntimeState::Running) {
            break;
        }
        black_box(runtime.step()?);
        completed += 1;
        drained += drain_render_updates(runtime);
    }
    Ok((
        completed,
        !matches!(runtime.state(), RuntimeState::Running),
        drained,
    ))
}

fn execute_persistent_benchmark(
    prepared: &PreparedRuntime,
    options: &BenchOptions,
    ticks: usize,
) -> Result<PersistentModeReport> {
    let mut runtime = build_runtime(prepared)?;
    let mut render_updates_drained = drain_render_updates(&mut runtime);
    let mut samples = Vec::with_capacity(options.repeat);
    let mut completed_samples = 0;
    let mut completed_ticks = 0;
    let mut gated = false;
    for _ in 0..options.repeat {
        let started = Instant::now();
        let (completed, sample_gated, drained) = execute_timed_steps(&mut runtime, ticks)?;
        let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
        render_updates_drained += drained;
        completed_ticks += completed;
        if completed == ticks {
            samples.push(elapsed);
            completed_samples += 1;
        }
        if sample_gated || completed != ticks {
            gated = true;
            break;
        }
    }
    let checksum = snapshot_checksum(&runtime.latest_snapshot())?;
    let metrics = (!samples.is_empty()).then(|| {
        benchmark_mode_report(
            &samples,
            completed_samples,
            ticks,
            render_updates_drained,
            checksum.clone(),
        )
    });
    Ok(PersistentModeReport {
        available: !gated && completed_samples == options.repeat,
        gated,
        completed_samples,
        completed_ticks,
        metrics,
        final_checksum: checksum,
    })
}

fn benchmark_mode_report(
    samples: &[f64],
    repeat: usize,
    ticks: usize,
    render_updates_drained: usize,
    checksum: String,
) -> BenchmarkModeReport {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    BenchmarkModeReport {
        repeat,
        ticks_per_sample: ticks,
        median_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        median_ms_per_tick: percentile(&sorted, 0.50) / ticks as f64,
        render_updates_drained,
        checksum,
    }
}

fn percentile(samples: &[f64], percentile: f64) -> f64 {
    let index = ((samples.len() as f64 * percentile).floor() as usize).min(samples.len() - 1);
    samples[index]
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductionInspectReport {
    schema: &'static str,
    scenario: ScenarioIdentityReport,
    counters: ProductionCountersReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_country: Option<SelectedCountryReport>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GridReport {
    grid_res: f64,
    width: usize,
    height: usize,
    cells: usize,
}

impl From<GridSpec> for GridReport {
    fn from(value: GridSpec) -> Self {
        Self {
            grid_res: value.grid_res,
            width: value.width,
            height: value.height,
            cells: value.width.saturating_mul(value.height),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioIdentityReport {
    sha256: String,
    name: String,
    source_grid: GridReport,
    target_grid: GridReport,
    entry_count: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductionCountersReport {
    countries: usize,
    cities: usize,
    unresolved_city_owners: usize,
    economy_seeds: usize,
    land_cells: usize,
}

impl From<&ScenarioProduction> for ProductionCountersReport {
    fn from(value: &ScenarioProduction) -> Self {
        Self {
            countries: value.counters.countries,
            cities: value.counters.cities,
            unresolved_city_owners: value.counters.unresolved_city_owners,
            economy_seeds: value.counters.economy_seeds,
            land_cells: value.counters.land_cells,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectedCountryReport {
    country_id: u16,
    name: String,
    gdp: f64,
    population: f64,
    is_rebel: bool,
    initial_core_cells: u32,
    initial_owned_land_cells: u32,
    initial_city_population: f64,
    capital_cell: Option<usize>,
    expected_army_units: f64,
    economic_strength: f64,
    base_income: f64,
    starting_treasury: f64,
}

fn selected_country_report(
    selector: &str,
    production: &ScenarioProduction,
) -> Result<SelectedCountryReport> {
    let numeric = selector.parse::<u16>().ok();
    let normalized = selector.trim().to_lowercase();
    let matches = production
        .countries
        .iter()
        .filter(|country| {
            numeric == Some(country.country_id) || country.name.to_lowercase() == normalized
        })
        .collect::<Vec<_>>();
    let [country] = matches.as_slice() else {
        if matches.is_empty() {
            bail!("country selector {selector:?} did not match this scenario");
        }
        bail!("country selector {selector:?} is ambiguous");
    };
    let economy = production
        .economy_states
        .iter()
        .find(|state| state.country_id == country.country_id)
        .context("selected country has no derived economy")?;
    Ok(SelectedCountryReport {
        country_id: country.country_id,
        name: country.name.clone(),
        gdp: country.gdp,
        population: country.population,
        is_rebel: country.is_rebel,
        initial_core_cells: country.initial_core_cells,
        initial_owned_land_cells: country.initial_owned_land_cells,
        initial_city_population: country.initial_city_population,
        capital_cell: country.capital_cell,
        expected_army_units: country.expected_army_units,
        economic_strength: economy.economic_strength,
        base_income: economy.base_income,
        starting_treasury: economy.treasury,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeFixtureReport {
    #[serde(flatten)]
    body: RuntimeFixtureReportBody,
    checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeFixtureReportBody {
    schema: &'static str,
    runtime_schema: &'static str,
    checkpoint_boundary: CheckpointBoundaryReport,
    scenario: ScenarioIdentityReport,
    checkpoint: CheckpointReport,
    production: RuntimeProductionReport,
    requested_steps: usize,
    completed_steps: usize,
    render_updates_drained: usize,
    initial: RuntimeSnapshotReport,
    steps: Vec<RuntimeSnapshotReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeBenchReport {
    #[serde(flatten)]
    body: RuntimeBenchReportBody,
    checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeBenchReportBody {
    schema: &'static str,
    runtime_schema: &'static str,
    mode: &'static str,
    checkpoint_boundary: CheckpointBoundaryReport,
    geography: GeographyReport,
    scenario: ScenarioIdentityReport,
    units: usize,
    sides: usize,
    countries: usize,
    repeat: usize,
    warmup: usize,
    ticks_per_sample: usize,
    fresh: BenchmarkModeReport,
    persistent: PersistentModeReport,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkModeReport {
    repeat: usize,
    ticks_per_sample: usize,
    median_ms: f64,
    p95_ms: f64,
    median_ms_per_tick: f64,
    render_updates_drained: usize,
    checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistentModeReport {
    available: bool,
    gated: bool,
    completed_samples: usize,
    completed_ticks: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<BenchmarkModeReport>,
    final_checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckpointReport {
    checkpoint_boundary: CheckpointBoundary,
    geography: GeographyReport,
    tick: u64,
    frame: u64,
    war_grace_end: u64,
    strategic_cycle: u64,
    sides: Vec<SideFixture>,
    active_sides: Vec<u16>,
    hostility_matrix: Vec<u8>,
    units: usize,
    economies: Vec<EconomyFixture>,
    occupations: usize,
    casualties: BTreeMap<u16, f64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeographyReport {
    exact_geography_supplied: bool,
    land_runs: usize,
    world_control_runs: usize,
    de_jure_runs: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckpointBoundaryReport {
    kind: CheckpointBoundary,
    resumable: bool,
    description: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeProductionReport {
    counters: ProductionCountersReport,
    theater_countries: Vec<SelectedCountryReport>,
}

impl PreparedRuntime {
    fn boundary_report(&self) -> CheckpointBoundaryReport {
        CheckpointBoundaryReport {
            kind: self.checkpoint.checkpoint_boundary,
            resumable: self.checkpoint.checkpoint_boundary.resumable(),
            description: self.checkpoint.checkpoint_boundary.description(),
        }
    }

    fn identity_report(&self) -> ScenarioIdentityReport {
        ScenarioIdentityReport {
            sha256: self.raw_sha256.clone(),
            name: self.scenario_name.clone(),
            source_grid: GridReport::from(self.decoded.source),
            target_grid: GridReport::from(self.decoded.target),
            entry_count: self.decoded.entry_count,
        }
    }

    fn geography_report(&self) -> GeographyReport {
        self.checkpoint.geography.as_ref().map_or(
            GeographyReport {
                exact_geography_supplied: false,
                land_runs: 0,
                world_control_runs: 0,
                de_jure_runs: 0,
            },
            |geography| GeographyReport {
                exact_geography_supplied: true,
                land_runs: geography.land_runs.len(),
                world_control_runs: geography.world_control_runs.len(),
                de_jure_runs: geography.de_jure_runs.len(),
            },
        )
    }

    fn checkpoint_report(&self) -> CheckpointReport {
        CheckpointReport {
            checkpoint_boundary: self.checkpoint.checkpoint_boundary,
            geography: self.geography_report(),
            tick: self.checkpoint.tick,
            frame: self.checkpoint.frame,
            war_grace_end: self.checkpoint.war_grace_end,
            strategic_cycle: self.checkpoint.strategic_cycle,
            sides: self.checkpoint.sides.clone(),
            active_sides: self.checkpoint.active_sides.clone(),
            hostility_matrix: self.checkpoint.hostility_matrix.clone(),
            units: self.checkpoint.units.len(),
            economies: self.checkpoint.economies.clone(),
            occupations: self.checkpoint.occupations.len(),
            casualties: self.checkpoint.casualties.clone(),
        }
    }

    fn production_report(&self) -> RuntimeProductionReport {
        let theater_countries = self
            .country_to_side
            .keys()
            .filter_map(|country_id| {
                selected_country_report(&country_id.to_string(), &self.production).ok()
            })
            .collect();
        RuntimeProductionReport {
            counters: ProductionCountersReport::from(&self.production),
            theater_countries,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeSnapshotReport {
    tick: u64,
    frame: u64,
    state: RuntimeStateReport,
    units: UnitTotalsReport,
    combat_events: usize,
    removed_units: usize,
    abandoned_orders: usize,
    territory: TerritorySnapshotReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    strategic: Option<StrategicSnapshotReport>,
    counters: Value,
    casualty_totals: BTreeMap<u16, f64>,
    pending_render_updates: usize,
    state_checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStateReport {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cycle: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tick: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    desertion_commands: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    surrender_commands: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflict_resolution: Option<bool>,
}

impl From<RuntimeState> for RuntimeStateReport {
    fn from(value: RuntimeState) -> Self {
        match value {
            RuntimeState::Running => Self {
                kind: "running",
                cycle: None,
                tick: None,
                desertion_commands: None,
                surrender_commands: None,
                conflict_resolution: None,
            },
            RuntimeState::AwaitingStrategicEffects {
                cycle,
                tick,
                desertion_commands,
                surrender_commands,
                conflict_resolution,
            } => Self {
                kind: "awaitingStrategicEffects",
                cycle: Some(cycle),
                tick: Some(tick),
                desertion_commands: Some(desertion_commands),
                surrender_commands: Some(surrender_commands),
                conflict_resolution: Some(conflict_resolution),
            },
            RuntimeState::Poisoned => Self {
                kind: "poisoned",
                cycle: None,
                tick: None,
                desertion_commands: None,
                surrender_commands: None,
                conflict_resolution: None,
            },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnitTotalsReport {
    total: usize,
    by_side: Vec<SideUnitTotalsReport>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SideUnitTotalsReport {
    side: u16,
    units: usize,
    armies: usize,
    armor: usize,
    health: f64,
    personnel: u64,
    equipment: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerritorySnapshotReport {
    generation: u64,
    commit_sequence: u64,
    land_cells: u64,
    positive_occupation_cells: u64,
    negative_occupation_cells: u64,
    countries: Vec<CountryTerritoryReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CountryTerritoryReport {
    country_id: u16,
    side_index: i16,
    owned: u64,
    controlled: u64,
    core_controlled: u64,
    core_control_ratio: f64,
    city_population_controlled: f64,
    capital_held: bool,
    frontline: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StrategicSnapshotReport {
    cycle: u64,
    tick: u64,
    countries: usize,
    occupations: usize,
    occupation_assessments: usize,
    desertion_commands: usize,
    surrender_commands: usize,
    events: usize,
    conflict_resolved: bool,
    economies: Vec<LiveEconomyReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveEconomyReport {
    country_id: u16,
    treasury: f64,
    income: f64,
    payroll_due: f64,
    occupation_due: f64,
    payroll_coverage: f64,
    occupation_coverage: f64,
    arrears_cycles: f64,
    command_band: CommandBand,
    capitulated: bool,
}

fn snapshot_report(
    snapshot: &Arc<RuntimeSnapshot>,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<RuntimeSnapshotReport> {
    let mut by_side = BTreeMap::<u16, SideUnitTotalsReport>::new();
    for unit in snapshot.frame_snapshot.units.iter() {
        let side = unit.side;
        let totals = by_side.entry(side).or_insert_with(|| SideUnitTotalsReport {
            side,
            ..Default::default()
        });
        totals.units += 1;
        match unit.kind {
            UnitKind::Army => totals.armies += 1,
            UnitKind::Armor => totals.armor += 1,
        }
        totals.health += unit.health;
        totals.personnel = totals.personnel.saturating_add(unit.personnel);
        totals.equipment = totals.equipment.saturating_add(unit.equipment);
    }
    let countries = snapshot
        .territory_snapshot
        .countries
        .iter()
        .filter(|country| country_to_side.contains_key(&country.country_id))
        .map(|country| CountryTerritoryReport {
            country_id: country.country_id,
            side_index: country.side_index,
            owned: country.owned,
            controlled: country.controlled,
            core_controlled: country.core_controlled,
            core_control_ratio: country.core_control_ratio,
            city_population_controlled: country.city_population_controlled,
            capital_held: country.capital_held,
            frontline: country.frontline,
        })
        .collect();
    let strategic = snapshot
        .strategic_snapshot
        .as_ref()
        .map(|strategic| StrategicSnapshotReport {
            cycle: strategic.cycle,
            tick: strategic.tick,
            countries: strategic.countries.len(),
            occupations: strategic.occupations.len(),
            occupation_assessments: strategic.occupation_assessments.len(),
            desertion_commands: strategic.desertions.len(),
            surrender_commands: strategic.surrenders.len(),
            events: strategic.events.len(),
            conflict_resolved: strategic.conflict_resolution.is_some(),
            economies: strategic
                .countries
                .iter()
                .map(|country| LiveEconomyReport {
                    country_id: country.country_id,
                    treasury: country.economy.treasury,
                    income: country.economy.income,
                    payroll_due: country.economy.payroll_due,
                    occupation_due: country.economy.occupation_due,
                    payroll_coverage: country.economy.payroll_coverage,
                    occupation_coverage: country.economy.occupation_coverage,
                    arrears_cycles: country.economy.arrears_cycles,
                    command_band: country.economy.command_band,
                    capitulated: country.economy.capitulated,
                })
                .collect(),
        });
    Ok(RuntimeSnapshotReport {
        tick: snapshot.tick,
        frame: snapshot.frame,
        state: snapshot.state.into(),
        units: UnitTotalsReport {
            total: snapshot.frame_snapshot.units.len(),
            by_side: by_side.into_values().collect(),
        },
        combat_events: snapshot.frame_snapshot.events.len(),
        removed_units: snapshot.frame_snapshot.removed_ids.len(),
        abandoned_orders: snapshot.frame_snapshot.abandoned_ids.len(),
        territory: TerritorySnapshotReport {
            generation: snapshot.territory_snapshot.generation,
            commit_sequence: snapshot.territory_snapshot.commit_sequence,
            land_cells: snapshot.territory_snapshot.land_cells,
            positive_occupation_cells: snapshot.territory_snapshot.positive_occupation_cells,
            negative_occupation_cells: snapshot.territory_snapshot.negative_occupation_cells,
            countries,
        },
        strategic,
        counters: counters_json(snapshot),
        casualty_totals: snapshot.casualty_totals.as_ref().clone(),
        pending_render_updates: snapshot.pending_render_updates,
        state_checksum: snapshot_checksum(snapshot)?,
    })
}

fn counters_json(snapshot: &RuntimeSnapshot) -> Value {
    let counters = snapshot.counters;
    json!({
        "front": {
            "refreshed": counters.front_refreshed,
            "segments": counters.front_segments,
            "objectives": counters.front_objectives,
        },
        "ai": {
            "inputUnits": counters.ai.input_units,
            "contactOrders": counters.ai.contact_orders,
            "retreatOrders": counters.ai.retreat_orders,
            "stickyAssignments": counters.ai.sticky_assignments,
            "frontAssignments": counters.ai.front_assignments,
            "reinforcementAssignments": counters.ai.reinforcement_assignments,
            "fieldOrders": counters.ai.field_orders,
            "holdOrders": counters.ai.hold_orders,
        },
        "simulation": {
            "inputUnits": counters.simulation.input_units,
            "tacticalCells": counters.simulation.tactical_cells,
            "candidateContacts": counters.simulation.candidate_contacts,
            "acceptedContacts": counters.simulation.accepted_contacts,
            "proximityEvents": counters.simulation.proximity_events,
            "directEvents": counters.simulation.direct_events,
            "movedUnits": counters.simulation.moved_units,
            "heldUnits": counters.simulation.held_units,
            "removedUnits": counters.simulation.removed_units,
            "abandonedOrders": counters.simulation.abandoned_orders,
        },
        "influence": {
            "sources": counters.influence.sources,
            "processedSourceCells": counters.influence.processed_source_cells,
            "touchedInfluenceCells": counters.influence.touched_influence_cells,
            "changedControllerCells": counters.influence.changed_controller_cells,
            "changedCreditCells": counters.influence.changed_credit_cells,
        },
        "census": {
            "processedItems": counters.census.processed_items,
            "committed": counters.census.committed,
            "flushedForStrategicCycle": counters.census.flushed_for_strategic_cycle,
            "territoryGeneration": counters.census.territory_generation,
            "territoryCommitSequence": counters.census.territory_commit_sequence,
        },
        "strategic": counters.strategic.map(|value| json!({
            "countriesProcessed": value.countries_processed,
            "occupationsProcessed": value.occupations_processed,
            "capitulations": value.capitulations,
            "desertionCommands": value.desertion_commands,
            "events": value.events,
        })),
        "strategicDerivation": counters.strategic_derivation.map(|value| json!({
            "countries": value.countries,
            "units": value.units,
            "payrollUnits": value.payroll_units,
            "garrisonUnits": value.garrison_units,
            "occupations": value.occupations,
            "activeSides": value.active_sides,
            "hostilePairs": value.hostile_pairs,
        })),
        "renderUpdatesEnqueued": counters.render_updates_enqueued,
    })
}

fn snapshot_checksum(snapshot: &RuntimeSnapshot) -> Result<String> {
    let mut checksum = Fnv64::new();
    checksum.write_u64(snapshot.tick);
    checksum.write_u64(snapshot.frame);
    match snapshot.state {
        RuntimeState::Running => checksum.write_u64(0),
        RuntimeState::AwaitingStrategicEffects {
            cycle,
            tick,
            desertion_commands,
            surrender_commands,
            conflict_resolution,
        } => {
            checksum.write_u64(1);
            checksum.write_u64(cycle);
            checksum.write_u64(tick);
            checksum.write_usize(desertion_commands);
            checksum.write_usize(surrender_commands);
            checksum.write_bool(conflict_resolution);
        }
        RuntimeState::Poisoned => checksum.write_u64(2),
    }
    checksum.write_usize(snapshot.frame_snapshot.units.len());
    for unit in snapshot.frame_snapshot.units.iter() {
        checksum.write_u64(unit.id);
        checksum.write_u16(unit.side);
        checksum.write_u64(unit.sovereign);
        checksum.write_u64(match unit.kind {
            UnitKind::Army => 0,
            UnitKind::Armor => 1,
        });
        for value in [
            unit.lat,
            unit.lng,
            unit.health,
            unit.max_health,
            f64::from(unit.health_fraction),
            unit.dir_lat,
            unit.dir_lng,
        ] {
            checksum.write_f64(value);
        }
        for value in [
            unit.personnel,
            unit.personnel_capacity,
            unit.equipment,
            unit.max_equipment,
            unit.last_combat_tick,
            unit.victory_boost_ticks,
        ] {
            checksum.write_u64(value);
        }
        checksum.write_u64(u64::from(unit.coast_stuck_ticks));
        for value in [unit.landing_penalty_active, unit.transport, unit.at_sea] {
            checksum.write_bool(value);
        }
    }
    checksum.write_usize(snapshot.frame_snapshot.events.len());
    for event in snapshot.frame_snapshot.events.iter() {
        checksum_combat_event(&mut checksum, event);
    }
    for ids in [
        snapshot.frame_snapshot.removed_ids.as_ref(),
        snapshot.frame_snapshot.abandoned_ids.as_ref(),
    ] {
        checksum.write_usize(ids.len());
        for &id in ids {
            checksum.write_u64(id);
        }
    }
    checksum.write_bytes(&serde_json::to_vec(snapshot.territory_snapshot.as_ref())?);
    if let Some(strategic) = &snapshot.strategic_snapshot {
        checksum.write_bool(true);
        checksum.write_u64(strategic.cycle);
        checksum.write_u64(strategic.tick);
        checksum.write_u64(strategic.territory_generation);
        checksum.write_u64(strategic.territory_commit_sequence);
        checksum.write_bytes(&serde_json::to_vec(strategic.countries.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(strategic.occupations.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(
            strategic.occupation_assessments.as_ref(),
        )?);
        checksum.write_bytes(&serde_json::to_vec(strategic.desertions.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(strategic.surrenders.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(strategic.events.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(&strategic.conflict_resolution)?);
    } else {
        checksum.write_bool(false);
    }
    checksum.write_bytes(&serde_json::to_vec(snapshot.casualty_totals.as_ref())?);
    checksum.write_bytes(&serde_json::to_vec(&counters_json(snapshot))?);
    checksum.write_usize(snapshot.pending_render_updates);
    Ok(checksum.finish())
}

fn checksum_combat_event(checksum: &mut Fnv64, event: &CombatEvent) {
    checksum.write_u64(match event.layer {
        CombatLayer::Proximity => 0,
        CombatLayer::Direct => 1,
    });
    checksum.write_u64(event.attacker_id);
    checksum.write_u64(event.target_id);
    for value in [
        event.target_damage,
        event.attacker_damage,
        event.transport_self_damage,
        event.target_resulting_health,
        event.attacker_resulting_health,
    ] {
        checksum.write_f64(value);
    }
    for value in [
        event.target_personnel_loss,
        event.attacker_personnel_loss,
        event.target_equipment_loss,
        event.attacker_equipment_loss,
    ] {
        checksum.write_u64(value);
    }
    checksum.write_bool(event.target_knockback_blocked);
    checksum.write_bool(event.attacker_knockback_blocked);
}

fn checksum_serializable(value: &impl Serialize) -> Result<String> {
    let mut checksum = Fnv64::new();
    checksum.write_bytes(&serde_json::to_vec(value)?);
    Ok(checksum.finish())
}

struct Fnv64(u64);

impl Fnv64 {
    const fn new() -> Self {
        Self(FNV64_OFFSET)
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(FNV64_PRIME);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_u16(&mut self, value: u16) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    fn write_f64(&mut self, value: f64) {
        self.write_u64(value.to_bits());
    }

    fn write_bool(&mut self, value: bool) {
        self.write_bytes(&[u8::from(value)]);
    }

    fn finish(&self) -> String {
        format!("{:016x}", self.0)
    }
}

fn scenario_name(decoded: &DecodedScenario) -> String {
    decoded
        .metadata
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Unnamed scenario")
        .to_owned()
}

fn read_file(path: &PathBuf) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}

fn print_json(value: &impl Serialize, compact: bool) -> Result<()> {
    if compact {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_length = (input.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((input.len() + 72).next_multiple_of(64));
    padded.extend_from_slice(input);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    let mut words = [0_u32; 64];
    for block in padded.chunks_exact(64) {
        for (index, bytes) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(bytes.try_into().expect("four-byte SHA word"));
        }
        for index in 16..64 {
            let left = words[index - 15];
            let right = words[index - 2];
            let sigma0 = left.rotate_right(7) ^ left.rotate_right(18) ^ (left >> 3);
            let sigma1 = right.rotate_right(17) ^ right.rotate_right(19) ^ (right >> 10);
            words[index] = words[index - 16]
                .wrapping_add(sigma0)
                .wrapping_add(words[index - 7])
                .wrapping_add(sigma1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temporary1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary1);
            d = c;
            c = b;
            b = a;
            a = temporary1.wrapping_add(temporary2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    state.iter().map(|value| format!("{value:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn fnv_checksum_is_stable() {
        let mut checksum = Fnv64::new();
        checksum.write_bytes(b"native-runtime");
        assert_eq!(checksum.finish(), "3627c1abde06b44b");
    }

    #[test]
    fn checkpoint_boundaries_have_explicit_resume_semantics() {
        let production: CheckpointBoundary = serde_json::from_str("\"postStartWar\"").unwrap();
        let replay: CheckpointBoundary = serde_json::from_str("\"baselineReplay\"").unwrap();
        assert!(production.resumable());
        assert!(!replay.resumable());
        assert!(replay.description().contains("not resumable"));
        assert!(serde_json::from_str::<CheckpointBoundary>("\"midWar\"").is_err());
    }

    #[test]
    fn geography_parser_is_strict_and_width_bounded() {
        let geography: GeographyFixture = serde_json::from_str(
            r#"{
                "landRuns": [[2, 1], [2, 0]],
                "worldControlRuns": [[1, 65535], [3, 0]],
                "deJureRuns": [[4, 1]]
            }"#,
        )
        .unwrap();
        assert_eq!(geography.land_runs, [(2, 1), (2, 0)]);
        assert_eq!(geography.world_control_runs, [(1, u16::MAX), (3, 0)]);
        assert_eq!(geography.de_jure_runs, [(4, 1)]);

        assert!(
            serde_json::from_str::<GeographyFixture>(
                r#"{
                    "landRuns": [[1, 0]],
                    "worldControlRuns": [[1, 65536]],
                    "deJureRuns": [[1, 0]]
                }"#,
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<GeographyFixture>(
                r#"{
                    "landRuns": [[1, 0]],
                    "worldControlRuns": [[1, 0]],
                    "deJureRuns": [[1, 0]],
                    "unexpected": true
                }"#,
            )
            .is_err()
        );
    }

    #[test]
    fn geography_decoder_requires_exact_transactional_coverage() {
        let geography = GeographyFixture {
            land_runs: vec![(2, 1), (2, 0)],
            world_control_runs: vec![(1, 1), (3, 2)],
            de_jure_runs: vec![(3, 7), (1, 9)],
        };
        let decoded = decode_geography(&geography, 4).unwrap();
        assert_eq!(decoded.land, [1, 1, 0, 0]);
        assert_eq!(decoded.world_control, [1, 2, 2, 2]);
        assert_eq!(decoded.de_jure, [7, 7, 7, 9]);

        let mut invalid = geography.clone();
        invalid.land_runs = vec![(4, 2)];
        assert!(decode_geography(&invalid, 4).is_err());
        invalid.land_runs = vec![(0, 1), (4, 0)];
        assert!(decode_geography(&invalid, 4).is_err());
        invalid.land_runs = vec![(3, 1)];
        assert!(decode_geography(&invalid, 4).is_err());
        invalid.land_runs = vec![(5, 1)];
        assert!(decode_geography(&invalid, 4).is_err());
        invalid.land_runs = vec![(u64::MAX, 1)];
        assert!(decode_geography(&invalid, 4).is_err());
    }

    #[test]
    fn production_boundary_requires_exact_geography() {
        let geography = GeographyFixture {
            land_runs: vec![(1, 0)],
            world_control_runs: vec![(1, 0)],
            de_jure_runs: vec![(1, 0)],
        };
        assert!(validate_geography_boundary(CheckpointBoundary::PostStartWar, None).is_err());
        assert!(
            validate_geography_boundary(CheckpointBoundary::PostStartWar, Some(&geography)).is_ok()
        );
        assert!(validate_geography_boundary(CheckpointBoundary::BaselineReplay, None).is_ok());
    }

    #[test]
    fn exact_geography_rejects_unknown_metadata_owners_even_at_sea() {
        let known = BTreeSet::from([1, 2]);
        assert!(validate_exact_geography_owner_ids(&[0, 1, 2], &[2, 1, 0], &known).is_ok());
        assert!(validate_exact_geography_owner_ids(&[0, 99], &[0, 1], &known).is_err());
        assert!(validate_exact_geography_owner_ids(&[0, 1], &[99, 0], &known).is_err());
    }
}
