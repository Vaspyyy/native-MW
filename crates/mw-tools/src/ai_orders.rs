use std::{fs, hint::black_box, path::PathBuf, time::Instant};

use anyhow::{Context, Result, bail};
use mw_core::{
    AI_ORDER_SCHEMA_VERSION, AiOrderConfig, AiOrderError, AiPlanningResult, AiUnitInput,
    AiWorldInput, AssignmentReason, FrontObjective, HostilityMatrix, ResolvedCombatModifiers,
    ResolvedMovementModifiers, UnitKind, resolve_ai_orders,
};
use serde::{Deserialize, Serialize};

const JAVASCRIPT_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub fn run_fixture_command(args: Vec<String>) -> Result<()> {
    let (path, compact) = parse_fixture_args(args)?;
    let fixture = load_fixture(&path)?;
    let prepared = prepare_fixture(&fixture)?;
    print_json(&build_report(&prepared)?, compact)
}

pub fn run_bench_command(args: Vec<String>) -> Result<()> {
    let options = parse_bench_args(args)?;
    let fixture = load_fixture(&options.path)?;
    // JSON parsing, strict validation, owned vector construction, and the first
    // deterministic report all happen outside the planning timer.
    let prepared = prepare_fixture(&fixture)?;
    let expected = build_report(&prepared)?;
    if build_report(&prepared)? != expected {
        bail!("AI order workload is not deterministic from immutable input");
    }

    for _ in 0..options.warmup {
        black_box(build_report(&prepared)?);
    }
    let mut samples = Vec::with_capacity(options.repeat);
    let mut last = None;
    for _ in 0..options.repeat {
        let started = Instant::now();
        let report = build_report(&prepared)?;
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        last = Some(black_box(report));
    }
    samples.sort_by(f64::total_cmp);
    let final_report = last.as_ref().context("benchmark produced no report")?;
    let report = BenchReport {
        schema: AI_ORDER_SCHEMA_VERSION,
        mode_name: "bench",
        cases: prepared.len(),
        units: prepared.iter().map(|case| case.units.len()).sum(),
        objectives: prepared.iter().map(|case| case.objectives.len()).sum(),
        repeat: options.repeat,
        warmup: options.warmup,
        planning: TimingReport {
            median_ms: percentile(&samples, 0.50),
            p95_ms: percentile(&samples, 0.95),
        },
        counters: aggregate_counters(final_report),
        checksum: report_checksum(final_report),
    };
    print_json(&report, options.compact)
}

fn load_fixture(path: &PathBuf) -> Result<Fixture> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_fixture(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

fn parse_fixture(bytes: &[u8]) -> Result<Fixture> {
    let fixture: Fixture = serde_json::from_slice(bytes)?;
    if fixture.schema != AI_ORDER_SCHEMA_VERSION {
        bail!(
            "unsupported AI order fixture schema {:?}; expected {:?}",
            fixture.schema,
            AI_ORDER_SCHEMA_VERSION
        );
    }
    if fixture.cases.is_empty() {
        bail!("AI order fixture must contain at least one case");
    }
    let mut names = std::collections::BTreeSet::new();
    for case in &fixture.cases {
        if case.name.is_empty() || !names.insert(case.name.as_str()) {
            bail!("AI order case names must be unique and nonempty");
        }
        validate_javascript_safe_case(case)?;
    }
    Ok(fixture)
}

fn validate_javascript_safe_case(case: &CaseFixture) -> Result<()> {
    for unit in &case.units {
        if unit.id > JAVASCRIPT_MAX_SAFE_INTEGER || unit.sovereign > JAVASCRIPT_MAX_SAFE_INTEGER {
            bail!(
                "case {:?} contains an id outside JavaScript's safe integer range",
                case.name
            );
        }
        if unit
            .previous_assignment
            .as_ref()
            .is_some_and(|value| value.objective_id > JAVASCRIPT_MAX_SAFE_INTEGER)
        {
            bail!(
                "case {:?} contains an objective id outside JavaScript's safe integer range",
                case.name
            );
        }
    }
    for objective in &case.world.objectives {
        if objective.id > JAVASCRIPT_MAX_SAFE_INTEGER
            || objective.segment_id > JAVASCRIPT_MAX_SAFE_INTEGER
        {
            bail!(
                "case {:?} contains an id outside JavaScript's safe integer range",
                case.name
            );
        }
    }
    Ok(())
}

fn prepare_fixture(fixture: &Fixture) -> Result<Vec<PreparedCase>> {
    fixture.cases.iter().map(PreparedCase::new).collect()
}

fn build_report(cases: &[PreparedCase]) -> Result<FixtureReport> {
    Ok(FixtureReport {
        schema: AI_ORDER_SCHEMA_VERSION,
        cases: cases
            .iter()
            .map(PreparedCase::run)
            .collect::<Result<Vec<_>>>()?,
    })
}

#[derive(Debug)]
struct PreparedCase {
    name: String,
    config: AiOrderConfig,
    units: Vec<AiUnitInput>,
    grid_width: usize,
    grid_height: usize,
    grid_res: f64,
    land_mask: Vec<u8>,
    dominant_side_map: Vec<i16>,
    max_sides: usize,
    hostility_matrix: Vec<u8>,
    frontline_latitude: Option<Vec<f32>>,
    frontline_longitude: Option<Vec<f32>>,
    objectives: Vec<FrontObjective>,
    verify_permutation_invariance: bool,
    expected_error: Option<String>,
}

impl PreparedCase {
    fn new(source: &CaseFixture) -> Result<Self> {
        let units = source
            .units
            .iter()
            .map(UnitFixture::to_core)
            .collect::<Result<Vec<_>>>()?;
        let objectives = source
            .world
            .objectives
            .iter()
            .copied()
            .map(ObjectiveFixture::to_core)
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            name: source.name.clone(),
            config: source.config.to_core(),
            units,
            grid_width: source.world.grid_width,
            grid_height: source.world.grid_height,
            grid_res: source.world.grid_res,
            land_mask: source.world.land_mask.clone(),
            dominant_side_map: source.world.dominant_side_map.clone(),
            max_sides: source.world.max_sides,
            hostility_matrix: source.world.hostility_matrix.clone(),
            frontline_latitude: source.world.frontline_latitude.clone(),
            frontline_longitude: source.world.frontline_longitude.clone(),
            objectives,
            verify_permutation_invariance: source.verify_permutation_invariance,
            expected_error: source.expected_error.clone(),
        })
    }

    fn world(&self) -> AiWorldInput<'_> {
        AiWorldInput {
            grid_width: self.grid_width,
            grid_height: self.grid_height,
            grid_res: self.grid_res,
            land_mask: &self.land_mask,
            dominant_side_map: &self.dominant_side_map,
            hostility: HostilityMatrix::new(Some(&self.hostility_matrix), self.max_sides),
            frontline_latitude: self.frontline_latitude.as_deref(),
            frontline_longitude: self.frontline_longitude.as_deref(),
            objectives: &self.objectives,
        }
    }

    fn run(&self) -> Result<CaseReport> {
        match resolve_ai_orders(self.config, &self.units, self.world()) {
            Ok(result) => {
                if let Some(expected) = &self.expected_error {
                    bail!(
                        "case {:?} expected error {expected:?} but succeeded",
                        self.name
                    );
                }
                let planning = PlanningReport::from(result);
                if self.verify_permutation_invariance {
                    let mut units = self.units.clone();
                    units.reverse();
                    let mut objectives = self.objectives.clone();
                    objectives.reverse();
                    let world = AiWorldInput {
                        objectives: &objectives,
                        ..self.world()
                    };
                    let permuted =
                        PlanningReport::from(resolve_ai_orders(self.config, &units, world)?);
                    if planning != permuted {
                        bail!("case {:?} is sensitive to input permutation", self.name);
                    }
                }
                Ok(CaseReport {
                    name: self.name.clone(),
                    result: Some(planning),
                    error: None,
                })
            }
            Err(error) => {
                let code = error_code(&error);
                if self.expected_error.as_deref() != Some(code) {
                    bail!("case {:?} failed with {code:?}: {error}", self.name);
                }
                Ok(CaseReport {
                    name: self.name.clone(),
                    result: None,
                    error: Some(code.to_owned()),
                })
            }
        }
    }
}

fn error_code(error: &AiOrderError) -> &'static str {
    match error {
        AiOrderError::InvalidConfig => "invalid_config",
        AiOrderError::PlanningLimitExceeded => "planning_limit_exceeded",
        AiOrderError::InvalidWorld => "invalid_world",
        AiOrderError::InvalidHostility => "invalid_hostility",
        AiOrderError::InvalidUnit(_) => "invalid_unit",
        AiOrderError::DuplicateUnit(_) => "duplicate_unit",
        AiOrderError::InvalidObjective(_) => "invalid_objective",
        AiOrderError::DuplicateObjective(_) => "duplicate_objective",
        AiOrderError::Tactical(_) => "tactical_error",
    }
}

fn percentile(samples: &[f64], percentile: f64) -> f64 {
    let index = ((samples.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len() - 1);
    samples[index]
}

const FNV64_OFFSET: u64 = 14_695_981_039_346_656_037;
const FNV64_PRIME: u64 = 1_099_511_628_211;
const CHECKSUM_SCALE: f64 = 1_000_000_000.0;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn checksum_byte(hash: &mut u64, byte: u8) {
    *hash ^= u64::from(byte);
    *hash = hash.wrapping_mul(FNV64_PRIME);
}

fn checksum_u64(hash: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        checksum_byte(hash, byte);
    }
}

fn checksum_bool(hash: &mut u64, value: bool) {
    checksum_u64(hash, u64::from(value));
}

fn checksum_optional_u64(hash: &mut u64, value: Option<u64>) {
    checksum_bool(hash, value.is_some());
    if let Some(value) = value {
        checksum_u64(hash, value);
    }
}

fn checksum_float(hash: &mut u64, value: f64) {
    checksum_bool(hash, value.is_sign_negative());
    let magnitude = (value.abs() * CHECKSUM_SCALE + 0.5)
        .floor()
        .min(MAX_SAFE_INTEGER) as u64;
    checksum_u64(hash, magnitude);
}

fn checksum_text(hash: &mut u64, value: &str) {
    checksum_u64(hash, value.len() as u64);
    for byte in value.bytes() {
        checksum_byte(hash, byte);
    }
}

fn reason_code(value: ReasonReport) -> u64 {
    match value {
        ReasonReport::Contact => 0,
        ReasonReport::Retreat => 1,
        ReasonReport::Front => 2,
        ReasonReport::Reinforce => 3,
        ReasonReport::Field => 4,
        ReasonReport::Hold => 5,
    }
}

fn aggregate_counters(report: &FixtureReport) -> CounterReport {
    let mut aggregate = CounterReport::default();
    for counters in report
        .cases
        .iter()
        .filter_map(|case| case.result.as_ref().map(|result| result.counters))
    {
        aggregate.input_units += counters.input_units;
        aggregate.contact_orders += counters.contact_orders;
        aggregate.retreat_orders += counters.retreat_orders;
        aggregate.sticky_assignments += counters.sticky_assignments;
        aggregate.front_assignments += counters.front_assignments;
        aggregate.reinforcement_assignments += counters.reinforcement_assignments;
        aggregate.field_orders += counters.field_orders;
        aggregate.hold_orders += counters.hold_orders;
    }
    aggregate
}

/// FNV-1a over every semantic order, assignment, and counter field. Floating
/// values are quantized to 1e-9 so insignificant cross-runtime libm ULPs do not
/// hide real planner-output drift behind JSON formatting differences.
fn report_checksum(report: &FixtureReport) -> String {
    let mut hash = FNV64_OFFSET;
    checksum_u64(&mut hash, report.cases.len() as u64);
    for case in &report.cases {
        checksum_text(&mut hash, &case.name);
        checksum_bool(&mut hash, case.error.is_some());
        if let Some(error) = &case.error {
            checksum_text(&mut hash, error);
        }
        checksum_bool(&mut hash, case.result.is_some());
        if let Some(result) = &case.result {
            checksum_u64(&mut hash, result.orders.len() as u64);
            for order in &result.orders {
                checksum_u64(&mut hash, order.unit_id);
                checksum_optional_u64(&mut hash, order.preferred_target_id);
                checksum_bool(&mut hash, order.movement_enabled);
                checksum_float(&mut hash, order.dir_lat);
                checksum_float(&mut hash, order.dir_lng);
                for value in [
                    order.factors.base_speed,
                    order.factors.speed_mult,
                    order.factors.plan_speed_mult,
                    order.factors.neutral_penalty,
                    order.factors.retreat_boost,
                    order.factors.push_readiness,
                    order.combat.dealt_multiplier,
                    order.combat.taken_multiplier,
                    order.combat.defense_bonus,
                    order.combat.long_war_defense,
                ] {
                    checksum_float(&mut hash, value);
                }
                checksum_bool(&mut hash, order.combat.mountain);
                checksum_bool(&mut hash, order.combat.urban);
            }
            checksum_u64(&mut hash, result.assignments.len() as u64);
            for assignment in &result.assignments {
                checksum_u64(&mut hash, assignment.unit_id);
                checksum_optional_u64(&mut hash, assignment.objective_id);
                checksum_u64(&mut hash, reason_code(assignment.reason));
            }
            for value in [
                result.counters.input_units,
                result.counters.contact_orders,
                result.counters.retreat_orders,
                result.counters.sticky_assignments,
                result.counters.front_assignments,
                result.counters.reinforcement_assignments,
                result.counters.field_orders,
                result.counters.hold_orders,
            ] {
                checksum_u64(&mut hash, value as u64);
            }
        }
    }
    format!("{hash:016x}")
}

fn print_json<T: Serialize>(value: &T, compact: bool) -> Result<()> {
    if compact {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

fn parse_fixture_args(args: Vec<String>) -> Result<(PathBuf, bool)> {
    let mut path = None;
    let mut compact = false;
    for argument in args {
        match argument.as_str() {
            "--json" => compact = true,
            flag if flag.starts_with('-') => bail!("unknown ai-orders-fixture option {flag:?}"),
            value if path.is_none() => path = Some(PathBuf::from(value)),
            _ => bail!("ai-orders-fixture accepts exactly one fixture path"),
        }
    }
    Ok((
        path.context("usage: ai-orders-fixture <fixture.json> [--json]")?,
        compact,
    ))
}

#[derive(Debug)]
struct BenchOptions {
    path: PathBuf,
    repeat: usize,
    warmup: usize,
    compact: bool,
}

fn parse_bench_args(args: Vec<String>) -> Result<BenchOptions> {
    let mut path = None;
    let mut repeat = 20;
    let mut warmup = 5;
    let mut compact = false;
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
            "--json" => compact = true,
            flag if flag.starts_with('-') => bail!("unknown ai-orders-bench option {flag:?}"),
            value if path.is_none() => path = Some(PathBuf::from(value)),
            _ => bail!("ai-orders-bench accepts exactly one fixture path"),
        }
        index += 1;
    }
    Ok(BenchOptions {
        path: path
            .context("usage: ai-orders-bench <fixture.json> [--repeat N] [--warmup N] [--json]")?,
        repeat,
        warmup,
        compact,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    schema: String,
    cases: Vec<CaseFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CaseFixture {
    name: String,
    config: ConfigFixture,
    world: WorldFixture,
    units: Vec<UnitFixture>,
    verify_permutation_invariance: bool,
    expected_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfigFixture {
    contact_scan_radius: f64,
    retreat_min_hostile_power: f64,
    retreat_multiple: f64,
    retreat_boost: f64,
    encircled_retreat_multiplier: f64,
    prior_assignment_stickiness: f64,
    reinforcement_readiness_threshold: f64,
    contact_plan_speed_multiplier: f64,
    front_plan_speed_multiplier: f64,
    reinforcement_plan_speed_multiplier: f64,
    field_plan_speed_multiplier: f64,
    max_units: usize,
    max_objectives: usize,
    max_grid_cells: usize,
    max_assignment_edges: usize,
}

impl ConfigFixture {
    fn to_core(self) -> AiOrderConfig {
        AiOrderConfig {
            contact_scan_radius: self.contact_scan_radius,
            retreat_min_hostile_power: self.retreat_min_hostile_power,
            retreat_multiple: self.retreat_multiple,
            retreat_boost: self.retreat_boost,
            encircled_retreat_multiplier: self.encircled_retreat_multiplier,
            prior_assignment_stickiness: self.prior_assignment_stickiness,
            reinforcement_readiness_threshold: self.reinforcement_readiness_threshold,
            contact_plan_speed_multiplier: self.contact_plan_speed_multiplier,
            front_plan_speed_multiplier: self.front_plan_speed_multiplier,
            reinforcement_plan_speed_multiplier: self.reinforcement_plan_speed_multiplier,
            field_plan_speed_multiplier: self.field_plan_speed_multiplier,
            max_units: self.max_units,
            max_objectives: self.max_objectives,
            max_grid_cells: self.max_grid_cells,
            max_assignment_edges: self.max_assignment_edges,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorldFixture {
    grid_width: usize,
    grid_height: usize,
    grid_res: f64,
    land_mask: Vec<u8>,
    dominant_side_map: Vec<i16>,
    max_sides: usize,
    hostility_matrix: Vec<u8>,
    frontline_latitude: Option<Vec<f32>>,
    frontline_longitude: Option<Vec<f32>>,
    objectives: Vec<ObjectiveFixture>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ObjectiveFixture {
    id: u64,
    side_pair: [u16; 2],
    segment_id: u64,
    lat: f64,
    lng: f64,
    capacity: usize,
    priority: i32,
}

impl ObjectiveFixture {
    fn to_core(self) -> Result<FrontObjective> {
        Ok(FrontObjective::new(
            self.id,
            self.side_pair,
            self.segment_id,
            self.lat,
            self.lng,
            self.capacity,
            self.priority,
        )?)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnitFixture {
    id: u64,
    side: u16,
    sovereign: u64,
    kind: UnitKindFixture,
    lat: f64,
    lng: f64,
    health: f64,
    max_health: f64,
    combat_power: f64,
    ally_weight: f64,
    at_sea: bool,
    transport: bool,
    base_speed: f64,
    movement: MovementFixture,
    combat: CombatFixture,
    previous_assignment: Option<PreviousAssignmentFixture>,
    is_reserve: bool,
    reinforcement_eligible: bool,
    encircled: bool,
}

impl UnitFixture {
    fn to_core(&self) -> Result<AiUnitInput> {
        Ok(AiUnitInput {
            id: self.id,
            side: self.side,
            sovereign: self.sovereign,
            kind: self.kind.into(),
            lat: self.lat,
            lng: self.lng,
            health: self.health,
            max_health: self.max_health,
            combat_power: self.combat_power,
            ally_weight: self.ally_weight,
            at_sea: self.at_sea,
            transport: self.transport,
            base_speed: self.base_speed,
            movement: self.movement.to_core(),
            combat: self.combat.to_core(),
            prior_front_objective_id: self
                .previous_assignment
                .as_ref()
                .map(|assignment| assignment.objective_id),
            is_reserve: self.is_reserve,
            reinforcement_eligible: self.reinforcement_eligible,
            encircled: self.encircled,
        })
    }
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

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviousAssignmentFixture {
    objective_id: u64,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MovementFixture {
    terrain_speed_multiplier: f64,
    speed_multiplier: f64,
    plan_speed_multiplier: f64,
    neutral_penalty: f64,
    push_readiness: f64,
}

impl MovementFixture {
    fn to_core(self) -> ResolvedMovementModifiers {
        ResolvedMovementModifiers {
            terrain_speed_multiplier: self.terrain_speed_multiplier,
            speed_multiplier: self.speed_multiplier,
            plan_speed_multiplier: self.plan_speed_multiplier,
            neutral_penalty: self.neutral_penalty,
            push_readiness: self.push_readiness,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CombatFixture {
    dealt_multiplier: f64,
    taken_multiplier: f64,
    defense_bonus: f64,
    long_war_defense: f64,
    mountain: bool,
    urban: bool,
}

impl CombatFixture {
    fn to_core(self) -> ResolvedCombatModifiers {
        ResolvedCombatModifiers {
            dealt_multiplier: self.dealt_multiplier,
            taken_multiplier: self.taken_multiplier,
            defense_bonus: self.defense_bonus,
            long_war_defense: self.long_war_defense,
            mountain: self.mountain,
            urban: self.urban,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct FixtureReport {
    schema: &'static str,
    cases: Vec<CaseReport>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct CaseReport {
    name: String,
    result: Option<PlanningReport>,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct PlanningReport {
    orders: Vec<OrderReport>,
    assignments: Vec<AssignmentReport>,
    counters: CounterReport,
}

impl From<AiPlanningResult> for PlanningReport {
    fn from(value: AiPlanningResult) -> Self {
        Self {
            orders: value.orders.into_iter().map(OrderReport::from).collect(),
            assignments: value
                .assignments
                .into_iter()
                .map(AssignmentReport::from)
                .collect(),
            counters: CounterReport {
                input_units: value.counters.input_units,
                contact_orders: value.counters.contact_orders,
                retreat_orders: value.counters.retreat_orders,
                sticky_assignments: value.counters.sticky_assignments,
                front_assignments: value.counters.front_assignments,
                reinforcement_assignments: value.counters.reinforcement_assignments,
                field_orders: value.counters.field_orders,
                hold_orders: value.counters.hold_orders,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct OrderReport {
    unit_id: u64,
    preferred_target_id: Option<u64>,
    movement_enabled: bool,
    dir_lat: f64,
    dir_lng: f64,
    factors: FactorReport,
    combat: CombatReport,
}

impl From<mw_core::ResolvedUnitOrder> for OrderReport {
    fn from(value: mw_core::ResolvedUnitOrder) -> Self {
        Self {
            unit_id: value.unit_id,
            preferred_target_id: value.preferred_target_id,
            movement_enabled: value.movement_enabled,
            dir_lat: value.dir_lat,
            dir_lng: value.dir_lng,
            factors: FactorReport {
                base_speed: value.factors.base_speed,
                speed_mult: value.factors.speed_mult,
                plan_speed_mult: value.factors.plan_speed_mult,
                neutral_penalty: value.factors.neutral_penalty,
                retreat_boost: value.factors.retreat_boost,
                push_readiness: value.factors.push_readiness,
            },
            combat: CombatReport {
                dealt_multiplier: value.combat.dealt_multiplier,
                taken_multiplier: value.combat.taken_multiplier,
                defense_bonus: value.combat.defense_bonus,
                long_war_defense: value.combat.long_war_defense,
                mountain: value.combat.mountain,
                urban: value.combat.urban,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct FactorReport {
    base_speed: f64,
    speed_mult: f64,
    plan_speed_mult: f64,
    neutral_penalty: f64,
    retreat_boost: f64,
    push_readiness: f64,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct CombatReport {
    dealt_multiplier: f64,
    taken_multiplier: f64,
    defense_bonus: f64,
    long_war_defense: f64,
    mountain: bool,
    urban: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AssignmentReport {
    unit_id: u64,
    objective_id: Option<u64>,
    reason: ReasonReport,
}

impl From<mw_core::FrontAssignmentRecord> for AssignmentReport {
    fn from(value: mw_core::FrontAssignmentRecord) -> Self {
        Self {
            unit_id: value.unit_id,
            objective_id: value.objective_id,
            reason: value.reason.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReasonReport {
    Contact,
    Retreat,
    Front,
    Reinforce,
    Field,
    Hold,
}

impl From<AssignmentReason> for ReasonReport {
    fn from(value: AssignmentReason) -> Self {
        match value {
            AssignmentReason::Contact => Self::Contact,
            AssignmentReason::Retreat => Self::Retreat,
            AssignmentReason::Front => Self::Front,
            AssignmentReason::Reinforce => Self::Reinforce,
            AssignmentReason::Field => Self::Field,
            AssignmentReason::Hold => Self::Hold,
        }
    }
}

impl std::fmt::Display for ReasonReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Contact => "contact",
            Self::Retreat => "retreat",
            Self::Front => "front",
            Self::Reinforce => "reinforce",
            Self::Field => "field",
            Self::Hold => "hold",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct CounterReport {
    input_units: usize,
    contact_orders: usize,
    retreat_orders: usize,
    sticky_assignments: usize,
    front_assignments: usize,
    reinforcement_assignments: usize,
    field_orders: usize,
    hold_orders: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchReport {
    schema: &'static str,
    #[serde(rename = "mode")]
    mode_name: &'static str,
    cases: usize,
    units: usize,
    objectives: usize,
    repeat: usize,
    warmup: usize,
    planning: TimingReport,
    counters: CounterReport,
    checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TimingReport {
    median_ms: f64,
    p95_ms: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL_FIXTURE: &[u8] = include_bytes!("../../../fixtures/ai-orders-v1.json");

    #[test]
    fn canonical_fixture_parses_runs_and_is_deterministic() {
        let fixture = parse_fixture(CANONICAL_FIXTURE).unwrap();
        let prepared = prepare_fixture(&fixture).unwrap();
        let first = build_report(&prepared).unwrap();
        let second = build_report(&prepared).unwrap();
        assert_eq!(first, second);
        assert!(
            first
                .cases
                .iter()
                .all(|case| case.result.is_some() || case.error.is_some())
        );
    }

    #[test]
    fn strict_parser_rejects_unknown_root_fields() {
        let mut value: serde_json::Value = serde_json::from_slice(CANONICAL_FIXTURE).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(parse_fixture(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn semantic_checksum_is_stable() {
        let fixture = parse_fixture(CANONICAL_FIXTURE).unwrap();
        let prepared = prepare_fixture(&fixture).unwrap();
        let report = build_report(&prepared).unwrap();
        assert_eq!(report_checksum(&report), report_checksum(&report));
    }
}
