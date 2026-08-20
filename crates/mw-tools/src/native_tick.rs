use std::{collections::BTreeMap, fs, hint::black_box, path::PathBuf, time::Instant};

use anyhow::{Context, Result, bail};
use mw_core::{
    CombatConfig, CombatEvent, CombatLayer, CombatUnit, HostilityMatrix, MovementFactors,
    NATIVE_TICK_SCHEMA_VERSION, ResolvedCombatOrder, ResolvedUnitOrder, Simulation,
    SimulationConfig, SimulationUnit, TickCounters, TickInput, UnitKind, UnitSnapshot,
    WorldGridView,
};
use serde::{Deserialize, Serialize};

const DEFAULT_UNIT_SPEED: f64 = 0.003;
const DEFAULT_NAVAL_SPEED: f64 = 0.025;

pub fn run_fixture_command(args: Vec<String>) -> Result<()> {
    let (path, compact) = parse_fixture_args(args)?;
    let fixture = load_fixture(&path)?;
    let report = build_report(&fixture)?;
    print_json(&report, compact)
}

pub fn run_bench_command(args: Vec<String>) -> Result<()> {
    let options = parse_bench_args(args)?;
    let fixture = load_fixture(&options.path)?;

    // A fresh simulation is intentional here: this is the same complete,
    // deterministic workload used by the JavaScript reference runner.
    let expected = canonical_report_bytes(&fixture)?;
    if canonical_report_bytes(&fixture)? != expected {
        bail!("native tick workload is not deterministic from fresh state");
    }

    let mut sink = 0_usize;
    for _ in 0..options.warmup {
        let report = black_box(build_report(&fixture)?);
        sink = sink.wrapping_add(serde_json::to_vec(&report)?.len());
    }

    let mut samples = Vec::with_capacity(options.repeat);
    let mut checksum = 0_usize;
    for _ in 0..options.repeat {
        let started = Instant::now();
        let report = build_report(&fixture)?;
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        // Keep report serialization out of the timed region, matching the JS
        // harness while still anchoring every result against optimization.
        checksum = checksum.wrapping_add(black_box(serde_json::to_vec(&report)?).len());
    }
    black_box(sink);

    samples.sort_by(f64::total_cmp);
    let report = BenchReport {
        steps: fixture.steps,
        units: fixture.units.len(),
        repeat: options.repeat,
        median_ms: javascript_percentile(&samples, 0.50),
        p95_ms: javascript_percentile(&samples, 0.95),
        checksum,
    };
    print_json(&report, options.compact)
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
            flag if flag.starts_with('-') => {
                bail!("unknown native-tick-fixture option {flag:?}")
            }
            value if path.is_none() => path = Some(PathBuf::from(value)),
            _ => bail!("native-tick-fixture accepts exactly one fixture path"),
        }
    }
    Ok((
        path.context("usage: native-tick-fixture <fixture.json> [--json]")?,
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
            flag if flag.starts_with('-') => bail!("unknown native-tick-bench option {flag:?}"),
            value if path.is_none() => path = Some(PathBuf::from(value)),
            _ => bail!("native-tick-bench accepts exactly one fixture path"),
        }
        index += 1;
    }
    Ok(BenchOptions {
        path: path.context(
            "usage: native-tick-bench <fixture.json> [--repeat N] [--warmup N] [--json]",
        )?,
        repeat,
        warmup,
        compact,
    })
}

fn load_fixture(path: &PathBuf) -> Result<Fixture> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let fixture: Fixture = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    validate_fixture(&fixture)?;
    Ok(fixture)
}

fn validate_fixture(fixture: &Fixture) -> Result<()> {
    if fixture.schema != NATIVE_TICK_SCHEMA_VERSION {
        bail!(
            "unsupported native tick fixture schema {:?}; expected {:?}",
            fixture.schema,
            NATIVE_TICK_SCHEMA_VERSION
        );
    }
    if fixture.steps == 0 {
        bail!("native tick fixture must contain at least one step");
    }
    if fixture.max_sides == 0 {
        bail!("maxSides must be at least 1");
    }
    fixture.grid.view()?;
    fixture.hostility_matrix()?;
    Ok(())
}

fn canonical_report_bytes(fixture: &Fixture) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&build_report(fixture)?)?)
}

fn build_report(fixture: &Fixture) -> Result<FixtureReport> {
    let units = fixture
        .units
        .iter()
        .map(|unit| unit.to_core(fixture.tick))
        .collect();
    let mut simulation = Simulation::new(
        SimulationConfig {
            tactical_cell_size: fixture.config.tactical_cell_size,
            combat: CombatConfig::default(),
        },
        units,
    )?;
    let at_sea_by_id = fixture
        .units
        .iter()
        .map(|unit| (unit.id, unit.is_at_sea))
        .collect::<BTreeMap<_, _>>();
    let orders = fixture
        .orders
        .iter()
        .map(|order| order.to_core(at_sea_by_id.get(&order.unit_id).copied().unwrap_or(false)))
        .collect::<Vec<_>>();
    let relations = fixture.hostility_matrix()?;
    let world = fixture.grid.view()?;
    let hostility = HostilityMatrix::new(relations.as_deref(), fixture.max_sides);
    let mut steps = Vec::with_capacity(fixture.steps);

    for offset in 0..fixture.steps {
        let offset = offset as u64;
        let (snapshot, counters) = simulation.step(TickInput {
            tick: fixture.tick.saturating_add(offset),
            frame: fixture.frame.saturating_add(offset),
            war_grace_end: fixture.war_grace_end,
            world,
            hostility,
            orders: &orders,
            inactive_unit_ids: &[],
        })?;
        steps.push(StepReport::from_snapshot(&snapshot, counters));
    }

    let final_units = steps
        .last()
        .map(|step| step.units.clone())
        .unwrap_or_default();
    Ok(FixtureReport {
        schema: NATIVE_TICK_SCHEMA_VERSION,
        steps,
        final_units,
    })
}

fn javascript_percentile(samples: &[f64], percentile: f64) -> f64 {
    let index = ((samples.len() as f64 * percentile).floor() as usize).min(samples.len() - 1);
    samples[index]
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    schema: String,
    config: FixtureConfig,
    grid: GridFixture,
    max_sides: usize,
    #[serde(default)]
    hostility_relations: BTreeMap<String, u8>,
    tick: u64,
    frame: u64,
    #[serde(default)]
    war_grace_end: u64,
    units: Vec<UnitFixture>,
    #[serde(default)]
    orders: Vec<OrderFixture>,
    steps: usize,
}

impl Fixture {
    fn hostility_matrix(&self) -> Result<Option<Vec<u8>>> {
        if self.hostility_relations.is_empty() {
            return Ok(None);
        }
        let length = self
            .max_sides
            .checked_mul(self.max_sides)
            .context("hostility matrix dimensions overflow")?;
        let mut matrix = vec![0_u8; length];
        for left in 0..self.max_sides {
            for right in 0..self.max_sides {
                matrix[left * self.max_sides + right] = u8::from(left != right);
            }
        }
        for (key, &value) in &self.hostility_relations {
            if value > 1 {
                bail!("hostility relation {key:?} must be 0 or 1");
            }
            let (left, right) = key
                .split_once(':')
                .with_context(|| format!("invalid hostility relation key {key:?}"))?;
            let left: usize = left
                .parse()
                .with_context(|| format!("invalid hostility relation key {key:?}"))?;
            let right: usize = right
                .parse()
                .with_context(|| format!("invalid hostility relation key {key:?}"))?;
            if left >= self.max_sides || right >= self.max_sides {
                bail!("hostility relation {key:?} references an out-of-range side");
            }
            matrix[left * self.max_sides + right] = value;
        }
        Ok(Some(matrix))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureConfig {
    tactical_cell_size: f64,
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
struct UnitFixture {
    id: u64,
    side: u64,
    sovereign: u64,
    kind: FixtureUnitKind,
    lat: f64,
    lng: f64,
    health: f64,
    max_health: f64,
    #[serde(default)]
    personnel: u64,
    #[serde(default)]
    personnel_capacity: u64,
    #[serde(default)]
    equipment: u64,
    #[serde(default)]
    max_equipment: u64,
    #[serde(default = "default_quality")]
    quality: f64,
    #[serde(default)]
    is_transport: bool,
    #[serde(default)]
    is_at_sea: bool,
    #[serde(default)]
    armor_supported: bool,
    #[serde(default)]
    armor_landing_penalty_until_tick: u64,
    #[serde(default)]
    last_combat_tick: u64,
    #[serde(default)]
    victory_boost_ticks: u64,
    #[serde(default)]
    dir_lat: f64,
    #[serde(default)]
    dir_lng: f64,
    #[serde(default)]
    coast_stuck_ticks: u32,
    #[serde(default)]
    is_support: bool,
    #[serde(default = "default_one")]
    ally_weight: f64,
}

impl UnitFixture {
    fn to_core(&self, _tick: u64) -> SimulationUnit {
        SimulationUnit {
            combat: CombatUnit {
                id: self.id,
                side: self.side,
                sovereign: self.sovereign,
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
                transport: self.is_transport,
                armor_supported: self.armor_supported,
                landing_penalty_active: false,
                at_sea: self.is_at_sea,
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
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FixtureUnitKind {
    Army,
    Armor,
}

impl From<FixtureUnitKind> for UnitKind {
    fn from(value: FixtureUnitKind) -> Self {
        match value {
            FixtureUnitKind::Army => Self::Army,
            FixtureUnitKind::Armor => Self::Armor,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrderFixture {
    unit_id: u64,
    #[serde(default)]
    preferred_target_id: Option<u64>,
    #[serde(default)]
    movement_enabled: bool,
    #[serde(default)]
    dir_lat: f64,
    #[serde(default)]
    dir_lng: f64,
    #[serde(default)]
    factors: MovementFactorsFixture,
    #[serde(default)]
    combat: CombatOrderFixture,
}

impl OrderFixture {
    fn to_core(&self, is_at_sea: bool) -> ResolvedUnitOrder {
        ResolvedUnitOrder {
            unit_id: self.unit_id,
            preferred_target_id: self.preferred_target_id,
            movement_enabled: self.movement_enabled,
            dir_lat: self.dir_lat,
            dir_lng: self.dir_lng,
            factors: self.factors.to_core(if is_at_sea {
                DEFAULT_NAVAL_SPEED
            } else {
                DEFAULT_UNIT_SPEED
            }),
            combat: self.combat.to_core(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MovementFactorsFixture {
    base_speed: Option<f64>,
    speed_mult: Option<f64>,
    plan_speed_mult: Option<f64>,
    neutral_penalty: Option<f64>,
    retreat_boost: Option<f64>,
    push_readiness: Option<f64>,
}

impl MovementFactorsFixture {
    fn to_core(self, default_speed: f64) -> MovementFactors {
        MovementFactors {
            base_speed: self.base_speed.unwrap_or(default_speed),
            speed_mult: self.speed_mult.unwrap_or(1.0),
            plan_speed_mult: self.plan_speed_mult.unwrap_or(1.0),
            neutral_penalty: self.neutral_penalty.unwrap_or(1.0),
            retreat_boost: self.retreat_boost.unwrap_or(1.0),
            push_readiness: self.push_readiness.unwrap_or(1.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CombatOrderFixture {
    damage_dealt_mult: Option<f64>,
    damage_taken_mult: Option<f64>,
    defense_bonus: Option<f64>,
    long_war_defense: Option<f64>,
    #[serde(default)]
    mountain: bool,
    #[serde(default)]
    urban: bool,
}

impl CombatOrderFixture {
    fn to_core(self) -> ResolvedCombatOrder {
        ResolvedCombatOrder {
            dealt_multiplier: self.damage_dealt_mult.unwrap_or(1.0),
            taken_multiplier: self.damage_taken_mult.unwrap_or(1.0),
            defense_bonus: self.defense_bonus.unwrap_or(1.0),
            long_war_defense: self.long_war_defense.unwrap_or(1.0),
            mountain: self.mountain,
            urban: self.urban,
        }
    }
}

const fn default_quality() -> f64 {
    50.0
}

const fn default_one() -> f64 {
    1.0
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct FixtureReport {
    schema: &'static str,
    steps: Vec<StepReport>,
    final_units: Vec<UnitReport>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct StepReport {
    tick: u64,
    frame: u64,
    events: Vec<EventReport>,
    removed: Vec<u64>,
    counts: CountReport,
    units: Vec<UnitReport>,
}

impl StepReport {
    fn from_snapshot(snapshot: &mw_core::FrameSnapshot, counters: TickCounters) -> Self {
        Self {
            tick: snapshot.tick,
            frame: snapshot.frame,
            events: snapshot.events.iter().map(EventReport::from).collect(),
            removed: snapshot.removed_ids.to_vec(),
            counts: CountReport {
                proximity_contacts: counters.proximity_events,
                direct_contacts: counters.direct_events,
                movement: counters.moved_units,
            },
            units: snapshot.units.iter().map(UnitReport::from).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
struct CountReport {
    proximity_contacts: usize,
    direct_contacts: usize,
    movement: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "layer", rename_all = "lowercase")]
enum EventReport {
    Proximity {
        attacker_id: u64,
        target_id: u64,
        target_damage: f64,
        attacker_damage: f64,
        transport_self_damage: f64,
        target_health: f64,
        self_health: f64,
    },
    Direct {
        attacker_id: u64,
        target_id: u64,
        target_damage: f64,
        attacker_damage: f64,
        target_health: f64,
        self_health: f64,
        target_knockback_blocked: bool,
        self_knockback_blocked: bool,
    },
}

impl From<&CombatEvent> for EventReport {
    fn from(event: &CombatEvent) -> Self {
        match event.layer {
            CombatLayer::Proximity => Self::Proximity {
                attacker_id: event.attacker_id,
                target_id: event.target_id,
                target_damage: event.target_damage,
                attacker_damage: event.attacker_damage,
                transport_self_damage: event.transport_self_damage,
                target_health: event.target_resulting_health,
                self_health: event.attacker_resulting_health,
            },
            CombatLayer::Direct => Self::Direct {
                attacker_id: event.attacker_id,
                target_id: event.target_id,
                target_damage: event.target_damage,
                attacker_damage: event.attacker_damage,
                target_health: event.target_resulting_health,
                self_health: event.attacker_resulting_health,
                target_knockback_blocked: event.target_knockback_blocked,
                self_knockback_blocked: event.attacker_knockback_blocked,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
struct UnitReport {
    id: u64,
    side: u64,
    sovereign: u64,
    kind: &'static str,
    lat: f64,
    lng: f64,
    health: f64,
    personnel: u64,
    equipment: u64,
    dir_lat: f64,
    dir_lng: f64,
    coast_stuck_ticks: u32,
    last_combat_tick: u64,
    victory_boost_ticks: u64,
}

impl From<&UnitSnapshot> for UnitReport {
    fn from(unit: &UnitSnapshot) -> Self {
        Self {
            id: unit.id,
            side: u64::from(unit.side),
            sovereign: unit.sovereign,
            kind: match unit.kind {
                UnitKind::Army => "army",
                UnitKind::Armor => "armor",
            },
            lat: unit.lat,
            lng: unit.lng,
            health: unit.health,
            personnel: unit.personnel,
            equipment: unit.equipment,
            dir_lat: unit.dir_lat,
            dir_lng: unit.dir_lng,
            coast_stuck_ticks: unit.coast_stuck_ticks,
            last_combat_tick: unit.last_combat_tick,
            victory_boost_ticks: unit.victory_boost_ticks,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
struct BenchReport {
    steps: usize,
    units: usize,
    repeat: usize,
    median_ms: f64,
    p95_ms: f64,
    checksum: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_json(extra: &str) -> String {
        format!(
            r#"{{
                "schema":"native-tick-v1",
                "config":{{"tacticalCellSize":0.6}},
                "grid":{{"gridRes":90,"width":4,"height":2,"landMask":[1,1,1,1,1,1,1,1]}},
                "maxSides":1,
                "tick":10,
                "frame":20,
                "units":[{{"id":7,"side":0,"sovereign":3,"kind":"army","lat":0,"lng":0,"health":100,"maxHealth":100,"personnel":1000,"personnelCapacity":1000}}],
                "orders":[{{"unitId":7,"movementEnabled":true,"dirLng":1,"factors":{{"baseSpeed":0.003}}}}],
                "steps":1{extra}
            }}"#
        )
    }

    #[test]
    fn fixture_parser_is_strict_and_applies_defaults() {
        let fixture: Fixture = serde_json::from_str(&fixture_json("")).unwrap();
        validate_fixture(&fixture).unwrap();
        assert_eq!(fixture.war_grace_end, 0);
        assert_eq!(fixture.orders[0].combat.damage_dealt_mult, None);
        assert!(serde_json::from_str::<Fixture>(&fixture_json(",\"surprise\":true")).is_err());
    }

    #[test]
    fn report_is_canonical_and_uses_snapshot_order() {
        let fixture: Fixture = serde_json::from_str(&fixture_json("")).unwrap();
        let report = build_report(&fixture).unwrap();
        assert_eq!(report.schema, NATIVE_TICK_SCHEMA_VERSION);
        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.steps[0].counts.movement, 1);
        assert_eq!(report.final_units[0].id, 7);
        assert!((report.final_units[0].lng - 0.0024).abs() < 1e-15);
    }

    #[test]
    fn event_shape_matches_javascript_reference() {
        let event = CombatEvent {
            schema_version: "1",
            layer: CombatLayer::Proximity,
            attacker_id: 1,
            target_id: 2,
            target_damage: 3.0,
            attacker_damage: 4.0,
            transport_self_damage: 5.0,
            target_personnel_loss: 0,
            attacker_personnel_loss: 0,
            target_equipment_loss: 0,
            attacker_equipment_loss: 0,
            target_resulting_health: 97.0,
            attacker_resulting_health: 91.0,
            target_knockback_blocked: false,
            attacker_knockback_blocked: false,
        };
        let value = serde_json::to_value(EventReport::from(&event)).unwrap();
        assert_eq!(value["layer"], "proximity");
        assert_eq!(value["transport_self_damage"], 5.0);
        assert!(value.get("target_knockback_blocked").is_none());
    }

    #[test]
    fn hostility_overrides_are_directed() {
        let source = fixture_json(",\"hostilityRelations\":{\"0:0\":1}");
        let fixture: Fixture = serde_json::from_str(&source).unwrap();
        assert_eq!(fixture.hostility_matrix().unwrap().unwrap(), vec![1]);
        let invalid = fixture_json(",\"hostilityRelations\":{\"0:1\":1}");
        let fixture: Fixture = serde_json::from_str(&invalid).unwrap();
        assert!(fixture.hostility_matrix().is_err());
    }

    #[test]
    fn command_and_benchmark_arguments_are_validated() {
        assert_eq!(
            parse_fixture_args(vec!["fixture.json".into(), "--json".into()]).unwrap(),
            (PathBuf::from("fixture.json"), true)
        );
        assert!(parse_fixture_args(Vec::new()).is_err());
        assert!(parse_fixture_args(vec!["a".into(), "b".into()]).is_err());

        let options = parse_bench_args(vec![
            "fixture.json".into(),
            "--repeat".into(),
            "4".into(),
            "--warmup".into(),
            "0".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(options.repeat, 4);
        assert_eq!(options.warmup, 0);
        assert!(options.compact);
        assert!(
            parse_bench_args(vec!["fixture.json".into(), "--repeat".into(), "0".into()]).is_err()
        );
    }

    #[test]
    fn benchmark_percentiles_match_javascript_indices() {
        let samples = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(javascript_percentile(&samples, 0.5), 3.0);
        assert_eq!(javascript_percentile(&samples, 0.95), 4.0);
    }
}
