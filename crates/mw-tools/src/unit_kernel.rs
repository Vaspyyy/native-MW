use std::{collections::HashMap, fs, hint::black_box, path::PathBuf, time::Instant};

use anyhow::{Context, Result, bail};
use mw_core::{
    CombatConfig, CombatContext, CombatEvent, CombatLayer, CombatUnit, MovementFactors,
    MovementInput, MovementState, UnitKind, WorldGridView, integrate_unit_step,
    resolve_direct_engagement, resolve_proximity_contact,
};
use serde::{Deserialize, Serialize};

const FIXTURE_SCHEMA: &str = "movement-combat-v1";
const DEFAULT_HEALTH: f64 = 100.0;
const DEFAULT_PERSONNEL: u64 = 1_000;

pub fn run_fixture_command(args: Vec<String>) -> Result<()> {
    let (path, compact) = parse_command_args(args)?;
    let fixture = load_fixture(&path)?;
    let report = build_report(&fixture)?;
    if compact {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

pub fn run_bench_command(args: Vec<String>) -> Result<()> {
    let options = parse_bench_args(args)?;
    let fixture = load_fixture(&options.path)?;
    let expected_movement = movement_checksum(&fixture)?;
    let expected_combat = combat_checksum(&fixture)?;
    if !expected_movement.is_finite() || !expected_combat.is_finite() {
        bail!("benchmark checksum is not finite");
    }
    if movement_checksum(&fixture)? != expected_movement
        || combat_checksum(&fixture)? != expected_combat
    {
        bail!("benchmark workload is not deterministic from fresh state");
    }

    let mut sink = 0.0;
    for _ in 0..options.warmup {
        sink += movement_checksum(&fixture)?;
        sink += combat_checksum(&fixture)?;
    }

    let mut movement_samples = Vec::with_capacity(options.repeat);
    for _ in 0..options.repeat {
        let started = Instant::now();
        sink += movement_checksum(&fixture)?;
        movement_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    let mut combat_samples = Vec::with_capacity(options.repeat);
    for _ in 0..options.repeat {
        let started = Instant::now();
        sink += combat_checksum(&fixture)?;
        combat_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    black_box(sink);
    if !sink.is_finite() {
        bail!("benchmark sink is not finite");
    }

    let report = BenchReport {
        schema_version: fixture.schema.clone(),
        movement_cases: fixture.movement_cases.len(),
        combat_cases: fixture.combat_cases.len(),
        operations: fixture
            .combat_cases
            .iter()
            .map(|test| test.operations.len())
            .sum(),
        repeat: options.repeat,
        warmup: options.warmup,
        movement_ms: timing_summary(&mut movement_samples),
        combat_ms: timing_summary(&mut combat_samples),
        checksum: expected_movement + expected_combat,
    };
    if options.compact {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

fn load_fixture(path: &PathBuf) -> Result<Fixture> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let fixture: Fixture = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if fixture.schema != FIXTURE_SCHEMA {
        bail!(
            "unsupported unit fixture schema {:?}; expected {:?}",
            fixture.schema,
            FIXTURE_SCHEMA
        );
    }
    Ok(fixture)
}

fn parse_command_args(args: Vec<String>) -> Result<(PathBuf, bool)> {
    let mut path = None;
    let mut compact = false;
    for argument in args {
        match argument.as_str() {
            "--json" => compact = true,
            flag if flag.starts_with('-') => bail!("unknown unit-fixture option {flag:?}"),
            _ if path.is_none() => path = Some(PathBuf::from(argument)),
            _ => bail!("unit-fixture accepts exactly one fixture path"),
        }
    }
    let path = path.context("usage: unit-fixture <fixture.json> [--json]")?;
    Ok((path, compact))
}

struct BenchOptions {
    path: PathBuf,
    repeat: usize,
    warmup: usize,
    compact: bool,
}

fn parse_bench_args(args: Vec<String>) -> Result<BenchOptions> {
    let mut path = None;
    let mut repeat = 50;
    let mut warmup = 10;
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
                if warmup == 0 {
                    bail!("--warmup must be at least 1");
                }
            }
            "--json" => compact = true,
            flag if flag.starts_with('-') => bail!("unknown unit-bench option {flag:?}"),
            value if path.is_none() => path = Some(PathBuf::from(value)),
            _ => bail!("unit-bench accepts exactly one fixture path"),
        }
        index += 1;
    }
    Ok(BenchOptions {
        path: path
            .context("usage: unit-bench <fixture.json> [--repeat N] [--warmup N] [--json]")?,
        repeat,
        warmup,
        compact,
    })
}

fn build_report(fixture: &Fixture) -> Result<FixtureReport> {
    let movement_cases = fixture
        .movement_cases
        .iter()
        .map(run_movement_case)
        .collect::<Result<Vec<_>>>()?;
    let combat_cases = fixture
        .combat_cases
        .iter()
        .map(run_combat_case)
        .collect::<Result<Vec<_>>>()?;
    Ok(FixtureReport {
        schema_version: fixture.schema.clone(),
        movement_cases,
        combat_cases,
    })
}

fn movement_output(test: &MovementCase) -> Result<MovementReport> {
    let world = test.grid.view()?;
    let result = integrate_unit_step(
        world,
        MovementInput {
            state: MovementState {
                lat: test.state.lat,
                lng: test.state.lng,
                coast_stuck_ticks: test.state.coast_stuck_ticks.unwrap_or(0),
            },
            dir_lat: test.state.dir_lat,
            dir_lng: test.state.dir_lng,
            factors: MovementFactors {
                base_speed: test.factors.base_speed,
                speed_mult: test.factors.speed_mult,
                plan_speed_mult: test.factors.plan_speed_mult,
                neutral_penalty: test.factors.neutral_penalty,
                retreat_boost: test.factors.retreat_boost,
                push_readiness: test.factors.push_readiness,
            },
            is_transport: test.state.is_transport,
            is_at_sea: test.state.is_at_sea,
        },
    )?;
    Ok(MovementReport {
        lat: result.state.lat,
        lng: result.state.lng,
        dir_lat: result.applied_dir_lat,
        dir_lng: result.applied_dir_lng,
        move_dist: result.move_distance,
        coast_blocked: result.coast_blocked,
        coast_stuck_ticks: result.state.coast_stuck_ticks,
        abandon_target: result.abandon_target,
        coast_deflect_halved: result.coast_deflect_halved,
    })
}

fn run_movement_case(test: &MovementCase) -> Result<MovementCaseReport> {
    Ok(MovementCaseReport {
        name: test.name.clone(),
        output: movement_output(test)?,
    })
}

fn run_combat_case(test: &CombatCase) -> Result<CombatCaseReport> {
    let mut units = test
        .units
        .iter()
        .map(|unit| unit.to_core(test.context.sim_tick))
        .collect::<Vec<_>>();
    let mut presence = test
        .units
        .iter()
        .map(|unit| (unit.id, UnitPresence::from(unit)))
        .collect::<HashMap<_, _>>();
    let indices = units
        .iter()
        .enumerate()
        .map(|(index, unit)| (unit.id, index))
        .collect::<HashMap<_, _>>();
    let world = test.grid.as_ref().map(GridFixture::view).transpose()?;
    let context = CombatContext {
        sim_tick: test.context.sim_tick,
        frame: test.context.sim_frame,
        war_grace_end: test.context.war_grace_end,
        attacker_damage_dealt_multiplier: test.context.damage_dealt_mult,
        attacker_damage_taken_multiplier: test.context.damage_taken_mult,
        defense_bonus: test.context.defense_bonus,
        long_war_defense: test.context.long_war_defense,
        mountain: test.context.mountain,
        urban: test.context.urban,
        world,
    };
    let config = CombatConfig::default();
    let mut events = Vec::with_capacity(test.operations.len());

    for operation in &test.operations {
        let (Some(&attacker_index), Some(&target_index)) = (
            indices.get(&operation.attacker_id),
            indices.get(&operation.target_id),
        ) else {
            events.push(None);
            continue;
        };
        let attacker_health = units[attacker_index].health;
        let target_health = units[target_index].health;
        let event = match operation.layer {
            OperationLayer::Proximity => resolve_proximity_contact(
                &mut units,
                attacker_index,
                target_index,
                &context,
                &config,
            )?,
            OperationLayer::Direct => resolve_direct_engagement(
                &mut units,
                attacker_index,
                target_index,
                &context,
                &config,
            )?,
        };
        if let Some(ref event) = event {
            update_presence_after_damage(
                &mut presence,
                &units,
                attacker_index,
                event.attacker_damage + event.transport_self_damage,
                attacker_health,
            );
            update_presence_after_damage(
                &mut presence,
                &units,
                target_index,
                event.target_damage,
                target_health,
            );
        }
        events.push(event.map(EventReport::from));
    }

    units.sort_by_key(|unit| unit.id);
    let units = units
        .into_iter()
        .map(|unit| {
            let fields = presence.get(&unit.id).copied().unwrap_or_default();
            UnitReport {
                id: unit.id,
                lat: unit.lat,
                lng: unit.lng,
                health: unit.health,
                personnel: fields.personnel,
                strength_multiplier: fields.strength_multiplier,
                equipment: fields.equipment,
                victory_boost_ticks: unit.victory_boost_ticks,
                last_combat_tick: unit.last_combat_tick,
            }
        })
        .collect();
    Ok(CombatCaseReport {
        name: test.name.clone(),
        events,
        units,
    })
}

fn movement_checksum(fixture: &Fixture) -> Result<f64> {
    let mut checksum = 0.0;
    for test in &fixture.movement_cases {
        let output = movement_output(test)?;
        checksum += output.lat * 0.5
            + output.lng * 0.25
            + output.dir_lat * 0.125
            + output.dir_lng * 0.0625
            + output.move_dist * 0.03125
            + if output.coast_blocked { 3.0 } else { 0.0 }
            + f64::from(output.coast_stuck_ticks) * 0.01
            + if output.abandon_target { 5.0 } else { 0.0 }
            + if output.coast_deflect_halved {
                7.0
            } else {
                0.0
            };
    }
    Ok(checksum)
}

fn combat_checksum(fixture: &Fixture) -> Result<f64> {
    let mut checksum = 0.0;
    for test in &fixture.combat_cases {
        let result = run_combat_case(test)?;
        for event in result.events {
            let Some(event) = event else {
                checksum += 0.125;
                continue;
            };
            checksum += event.attacker_id as f64 * 1e-7
                + event.target_id as f64 * 2e-7
                + event.target_damage * 0.5
                + event.self_damage * 0.25
                + event.target_personnel_loss as f64 * 0.01
                + event.self_personnel_loss as f64 * 0.02
                + event.target_health * 0.001
                + event.self_health * 0.002
                + if event.target_knockback_blocked {
                    3.0
                } else {
                    0.0
                }
                + if event.self_knockback_blocked {
                    5.0
                } else {
                    0.0
                };
        }
        for unit in result.units {
            checksum += unit.id as f64 * 1e-8
                + unit.lat * 1e-5
                + unit.lng * 2e-5
                + unit.health * 3e-5
                + unit.personnel.unwrap_or(0.0) * 1e-6
                + unit.equipment.unwrap_or(0) as f64 * 2e-6
                + unit.victory_boost_ticks as f64 * 3e-7
                + unit.last_combat_tick as f64 * 4e-8;
        }
    }
    Ok(checksum)
}

fn timing_summary(samples: &mut [f64]) -> TimingSummary {
    samples.sort_by(f64::total_cmp);
    let percentile = |value: f64| {
        let index = ((samples.len() as f64 * value).ceil() as usize)
            .saturating_sub(1)
            .min(samples.len() - 1);
        samples[index]
    };
    TimingSummary {
        median: percentile(0.5),
        p95: percentile(0.95),
    }
}

fn update_presence_after_damage(
    presence: &mut HashMap<u64, UnitPresence>,
    units: &[CombatUnit],
    index: usize,
    requested_damage: f64,
    health_before: f64,
) {
    if requested_damage <= 0.0 || health_before <= 0.0 {
        return;
    }
    let unit = &units[index];
    let fields = presence.entry(unit.id).or_default();
    match unit.kind {
        UnitKind::Army => {
            fields.personnel = Some(unit.personnel as f64);
            fields.strength_multiplier = Some(unit.personnel as f64 / DEFAULT_PERSONNEL as f64);
        }
        UnitKind::Armor => fields.equipment = Some(unit.equipment),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    schema: String,
    movement_cases: Vec<MovementCase>,
    combat_cases: Vec<CombatCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GridFixture {
    grid_res: f64,
    width: usize,
    height: usize,
    land_mask: Vec<u8>,
}

impl GridFixture {
    fn view(&self) -> Result<WorldGridView<'_>> {
        Ok(WorldGridView::new(
            self.grid_res,
            self.width,
            self.height,
            &self.land_mask,
        )?)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MovementCase {
    name: String,
    grid: GridFixture,
    state: MovementStateFixture,
    factors: MovementFactorsFixture,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MovementStateFixture {
    lat: f64,
    lng: f64,
    dir_lat: f64,
    dir_lng: f64,
    #[serde(default)]
    coast_stuck_ticks: Option<u32>,
    #[serde(default)]
    is_transport: bool,
    #[serde(default)]
    is_at_sea: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MovementFactorsFixture {
    base_speed: f64,
    speed_mult: f64,
    plan_speed_mult: f64,
    neutral_penalty: f64,
    retreat_boost: f64,
    push_readiness: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CombatCase {
    name: String,
    #[serde(default)]
    grid: Option<GridFixture>,
    context: CombatContextFixture,
    units: Vec<UnitFixture>,
    operations: Vec<OperationFixture>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CombatContextFixture {
    sim_tick: u64,
    sim_frame: u64,
    war_grace_end: u64,
    damage_dealt_mult: f64,
    damage_taken_mult: f64,
    defense_bonus: f64,
    long_war_defense: f64,
    mountain: bool,
    urban: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnitFixture {
    id: u64,
    #[serde(default, alias = "sideIndex")]
    side: u64,
    #[serde(default, alias = "sovereignId")]
    sovereign: u64,
    kind: UnitKindFixture,
    lat: f64,
    lng: f64,
    health: f64,
    #[serde(default)]
    max_health: Option<f64>,
    #[serde(default)]
    base_health: Option<f64>,
    #[serde(default)]
    personnel: Option<f64>,
    #[serde(default)]
    nominal_personnel: Option<f64>,
    #[serde(default)]
    personnel_capacity: Option<u64>,
    #[serde(default)]
    strength_multiplier: Option<f64>,
    #[serde(default)]
    equipment: Option<u64>,
    #[serde(default)]
    max_equipment: Option<u64>,
    #[serde(default)]
    quality: Option<f64>,
    #[serde(default)]
    is_transport: bool,
    #[serde(default)]
    armor_supported: bool,
    #[serde(default)]
    armor_landing_penalty_until_tick: Option<u64>,
    #[serde(default)]
    is_at_sea: bool,
    #[serde(default)]
    last_combat_tick: u64,
    #[serde(default)]
    victory_boost_ticks: u64,
}

impl UnitFixture {
    fn to_core(&self, sim_tick: u64) -> CombatUnit {
        let nominal = self.nominal_personnel.unwrap_or(DEFAULT_PERSONNEL as f64);
        let personnel = if let Some(personnel) = self.personnel {
            personnel.round()
        } else if let Some(multiplier) = self.strength_multiplier {
            (nominal * multiplier).round()
        } else {
            let base_health = self
                .base_health
                .or(self.max_health)
                .unwrap_or(DEFAULT_HEALTH);
            (self.health / base_health * nominal).round()
        }
        .max(0.0) as u64;
        CombatUnit {
            id: self.id,
            side: self.side,
            sovereign: self.sovereign,
            kind: self.kind.into(),
            lat: self.lat,
            lng: self.lng,
            health: self.health,
            max_health: self.max_health.unwrap_or(DEFAULT_HEALTH),
            personnel,
            personnel_capacity: self.personnel_capacity.unwrap_or(0),
            equipment: self.equipment.unwrap_or(0),
            max_equipment: self.max_equipment.unwrap_or(0),
            quality: self.quality.unwrap_or(50.0),
            transport: self.is_transport,
            armor_supported: self.armor_supported,
            landing_penalty_active: self
                .armor_landing_penalty_until_tick
                .is_some_and(|until| until > sim_tick),
            at_sea: self.is_at_sea,
            last_combat_tick: self.last_combat_tick,
            victory_boost_ticks: self.victory_boost_ticks,
        }
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
struct OperationFixture {
    layer: OperationLayer,
    attacker_id: u64,
    target_id: u64,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OperationLayer {
    Proximity,
    Direct,
}

#[derive(Clone, Copy, Debug, Default)]
struct UnitPresence {
    personnel: Option<f64>,
    strength_multiplier: Option<f64>,
    equipment: Option<u64>,
}

impl From<&UnitFixture> for UnitPresence {
    fn from(unit: &UnitFixture) -> Self {
        Self {
            personnel: unit.personnel,
            strength_multiplier: unit.strength_multiplier,
            equipment: unit.equipment,
        }
    }
}

#[derive(Debug, Serialize)]
struct FixtureReport {
    schema_version: String,
    movement_cases: Vec<MovementCaseReport>,
    combat_cases: Vec<CombatCaseReport>,
}

#[derive(Debug, Serialize)]
struct BenchReport {
    schema_version: String,
    movement_cases: usize,
    combat_cases: usize,
    operations: usize,
    repeat: usize,
    warmup: usize,
    movement_ms: TimingSummary,
    combat_ms: TimingSummary,
    checksum: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct TimingSummary {
    median: f64,
    p95: f64,
}

#[derive(Debug, Serialize)]
struct MovementCaseReport {
    name: String,
    output: MovementReport,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct MovementReport {
    lat: f64,
    lng: f64,
    dir_lat: f64,
    dir_lng: f64,
    move_dist: f64,
    coast_blocked: bool,
    coast_stuck_ticks: u32,
    abandon_target: bool,
    coast_deflect_halved: bool,
}

#[derive(Debug, Serialize)]
struct CombatCaseReport {
    name: String,
    events: Vec<Option<EventReport>>,
    units: Vec<UnitReport>,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct EventReport {
    layer: &'static str,
    attacker_id: u64,
    target_id: u64,
    target_damage: f64,
    self_damage: f64,
    target_personnel_loss: u64,
    self_personnel_loss: u64,
    target_health: f64,
    self_health: f64,
    target_knockback_blocked: bool,
    self_knockback_blocked: bool,
}

impl From<CombatEvent> for EventReport {
    fn from(event: CombatEvent) -> Self {
        Self {
            layer: match event.layer {
                CombatLayer::Proximity => "proximity",
                CombatLayer::Direct => "direct",
            },
            attacker_id: event.attacker_id,
            target_id: event.target_id,
            target_damage: event.target_damage,
            self_damage: event.attacker_damage + event.transport_self_damage,
            target_personnel_loss: event.target_personnel_loss,
            self_personnel_loss: event.attacker_personnel_loss,
            target_health: event.target_resulting_health,
            self_health: event.attacker_resulting_health,
            target_knockback_blocked: event.target_knockback_blocked,
            self_knockback_blocked: event.attacker_knockback_blocked,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
struct UnitReport {
    id: u64,
    lat: f64,
    lng: f64,
    health: f64,
    personnel: Option<f64>,
    strength_multiplier: Option<f64>,
    equipment: Option<u64>,
    victory_boost_ticks: u64,
    last_combat_tick: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_parser_requires_one_path_and_accepts_json_anywhere() {
        assert_eq!(
            parse_command_args(vec!["fixture.json".into(), "--json".into()]).unwrap(),
            (PathBuf::from("fixture.json"), true)
        );
        assert!(parse_command_args(Vec::new()).is_err());
        assert!(parse_command_args(vec!["a".into(), "b".into()]).is_err());
        assert!(parse_command_args(vec!["a".into(), "--wat".into()]).is_err());
    }

    #[test]
    fn fixture_parser_rejects_unknown_fields() {
        let source = r#"{
            "schema":"movement-combat-v1",
            "movementCases":[],
            "combatCases":[],
            "surprise":true
        }"#;
        assert!(serde_json::from_str::<Fixture>(source).is_err());
    }

    #[test]
    fn bench_parser_validates_counts_and_timing_uses_nearest_rank() {
        let options = parse_bench_args(vec![
            "fixture.json".into(),
            "--repeat".into(),
            "4".into(),
            "--warmup".into(),
            "2".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(options.path, PathBuf::from("fixture.json"));
        assert_eq!(options.repeat, 4);
        assert_eq!(options.warmup, 2);
        assert!(options.compact);
        assert!(
            parse_bench_args(vec!["fixture.json".into(), "--repeat".into(), "0".into()]).is_err()
        );

        let summary = timing_summary(&mut [4.0, 1.0, 3.0, 2.0]);
        assert_eq!(summary.median, 2.0);
        assert_eq!(summary.p95, 4.0);
    }
}
