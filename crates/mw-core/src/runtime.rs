//! Deterministic orchestration across the native tactical, territory, and strategic kernels.
//!
//! This module is the renderer-facing transaction boundary. A successful step publishes one
//! immutable [`RuntimeSnapshot`]; a failed step never replaces the previously published `Arc`.
//! Territory texture deltas are retained in FIFO order until the renderer explicitly drains them.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

use thiserror::Error;

use crate::{
    ai::{
        AiOrderConfig, AiOrderError, AiPlanningCounters, AiPlanningResult, AiUnitInput,
        AiWorldInput, AssignmentReason, FrontAssignmentRecord, FrontObjective,
        ResolvedCombatModifiers, ResolvedMovementModifiers, resolve_ai_orders,
    },
    air::{
        AIR_MISSION_INTERVAL, AirAdvanceOutcome, AirError, AirPowerState, AirPriorityArea,
        AirTargetKind, AirUnitTarget, AirWorldInput, AirfieldController,
    },
    battlefield::{
        BattlefieldDirectionInput, BattlefieldError, BattlefieldLocalTacticsResult,
        BattlefieldLocalUnitInput, BattlefieldMapView, BattlefieldRuntimeState,
        BattlefieldTickInput, BattlefieldUnitInput, BattlefieldUnitResult, BattlefieldUnitState,
        apply_cohesion_and_repulsion, resolve_battlefield_tick, resolve_local_tactics,
    },
    combat::{
        CombatUnit, PERSONNEL_PER_FORMATION, UNIT_HEALTH, UnitKind, formation_strength,
        wrapped_longitude_delta,
    },
    command::{
        CommandHomeTarget, CommandResolveError, CommandUnitState, CommandWorld,
        ResolvedCommandPolicy, browser_discipline, resolve_command_batch, resolve_command_policy,
    },
    diffusion::DiffusionQueueResult,
    direction::HostilityMatrix,
    dynamics::{MOMENTUM_SAMPLE_INTERVAL, SideDynamics, WarPhase, WarPosture},
    economy::{CommandBand, EconomyState, PAY_CYCLE_TICKS, RECRUITMENT_COST},
    front::{
        FrontLayoutConfig, FrontLayoutError, FrontLayoutInput, FrontLayoutPrior, FrontLayoutUnit,
        derive_front_layout,
    },
    gameplay_rng::{GameplayRng, GameplayRngState},
    naval_planning::{
        NavalPlanningCounters, NavalPlanningError, NavalPlanningInput, NavalPlanningState,
        NavalRouteWorkspace, NavalTopology,
    },
    occupation::{OccupationState, required_garrison},
    operational_execution::{
        DefenderThreat, DefenderThreatKind, DefenderThreatPhase, ExecutionUnitInput,
        NavalOperationKind, NavalOperationPhase, OperationalExecutionCounters,
        OperationalExecutionError, OperationalExecutionOutcome, OperationalExecutionState,
        Point as ExecutionPoint,
    },
    operations::{
        CountryOperationalInput, OperationalError, OperationalPoint, OperationalRuntimeState,
        OperationalSnapshot, OperationalUnitInput, TacticalContactObservation, TaskForcePhase,
    },
    production::{
        ProductionConfig, ProductionError, ScenarioProduction, StrategicDerivationCounters,
        StrategicDerivationInput, TerritoryCommitMarker, derive_strategic_cycle_input,
    },
    reinforcement::{
        AirPayCycleOutcome, MAX_ARMOR_CAPACITY, MaterialLogisticsState, MaterialPayCycleOutcome,
        ReinforcementError, ReinforcementState, bootstrap_reinforcement_state,
    },
    scenario::GridSpec,
    simulation::{
        DamageCommand, DamageOutcome, FrameSnapshot, ResolvedCombatOrder, ResolvedUnitOrder,
        Simulation, SimulationConfig, SimulationError, SimulationUnit, TickCounters, TickInput,
    },
    strategic::{
        ConflictResolutionPlan, DesertionCommand, PreparedStrategicCycle, StrategicCounters,
        StrategicError, StrategicSimulation, StrategicSnapshot, SurrenderAllocationInput,
        SurrenderUnitPosition, plan_surrender_allocation,
    },
    strategic_missile::{
        StrategicMissileAdvanceOutcome, StrategicMissileError, StrategicMissileState,
    },
    surrender::evaluate_global_conflict,
    tactical::SideKey,
    territory::{
        CensusStepResult, InfluenceApplyResult, InfluenceSource, InfluenceTransaction,
        TerritoryCommittedState, TerritoryConfig, TerritoryControl, TerritoryError,
        TerritoryRenderUpdate, TerritorySnapshot,
    },
    world::WorldGridView,
};

pub const NATIVE_RUNTIME_SCHEMA_VERSION: &str = "native-runtime-v4";
pub const DEFAULT_FRONT_REFRESH_TICKS: u64 = 30;
pub const DEFAULT_CENSUS_BUDGET: usize = 16_384;
pub const DEFAULT_CENSUS_FLUSH_CHUNK: usize = 65_536;
const AIRCREW_PER_AIRCRAFT: u64 = 1;
const AIRFIELD_CONTROLLER_PHASE: u64 = 47;
const EARLY_RECRUIT_TICKS: u64 = 1_800;
const EARLY_RECRUIT_MULTIPLIER: f64 = 2.3;
const RECRUIT_DEPLOY_TICKS: u64 = 30;
const BASE_RECRUITMENT_CHANCE: f64 = 0.012;
/// Front-layout slots are already capacity-resolved. Preserve that handoff across the whole
/// geographic domain instead of rebuilding a quadratic unit/objective matching problem.
pub const DEFAULT_RUNTIME_FRONT_STICKINESS: f64 = 360.0;

/// Stable per-unit AI inputs that are not owned by the combat kernel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitAiPolicy {
    pub base_speed: f64,
    pub movement: ResolvedMovementModifiers,
    pub combat: ResolvedCombatModifiers,
    pub is_reserve: bool,
    pub reinforcement_eligible: bool,
    pub encircled: bool,
    /// Units deploying until this absolute tick are excluded from front slots.
    pub deploy_until_tick: u64,
    pub garrison_excluded: bool,
}

impl Default for UnitAiPolicy {
    fn default() -> Self {
        Self {
            base_speed: 0.003,
            movement: ResolvedMovementModifiers::default(),
            combat: ResolvedCombatModifiers::default(),
            is_reserve: false,
            reinforcement_eligible: false,
            encircled: false,
            deploy_until_tick: 0,
            garrison_excluded: false,
        }
    }
}

/// Per-unit policy used to materialize authoritative territory influence sources.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitInfluencePolicy {
    pub radius: f64,
    pub delta: f64,
    pub concentration_bonus: f64,
    /// Original browser unit id used by the tick ramp and deterministic radius/delta noise.
    /// `None` keeps fixtures and fully resolved native policies static.
    pub browser_temporal_seed: Option<f64>,
    /// `None` resolves to the unit sovereign.
    pub beneficiary: Option<u16>,
    pub owner_ally_country_ids: BTreeSet<u16>,
    pub protected_owner_ids: BTreeSet<u16>,
    pub rebel_de_jure: Option<u16>,
    pub credit_de_jure: Option<u16>,
    pub credit_de_jure_by_country: BTreeMap<u16, u16>,
    pub refuses_offense: bool,
}

impl Default for UnitInfluencePolicy {
    fn default() -> Self {
        Self {
            radius: 0.45,
            delta: 0.04,
            concentration_bonus: 1.0,
            browser_temporal_seed: None,
            beneficiary: None,
            owner_ally_country_ids: BTreeSet::new(),
            protected_owner_ids: BTreeSet::new(),
            rebel_de_jure: None,
            credit_de_jure: None,
            credit_de_jure_by_country: BTreeMap::new(),
            refuses_offense: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeUnitPolicy {
    pub unit_id: u64,
    pub ai: UnitAiPolicy,
    pub command: UnitCommandPolicy,
    /// `None` means the unit never stamps territory influence.
    pub influence: Option<UnitInfluencePolicy>,
}

impl RuntimeUnitPolicy {
    pub fn standard(unit_id: u64, sovereign: u16) -> Self {
        let mut influence = UnitInfluencePolicy::default();
        influence.owner_ally_country_ids.insert(sovereign);
        Self {
            unit_id,
            ai: UnitAiPolicy::default(),
            command: UnitCommandPolicy::paid(unit_id as f64, sovereign),
            influence: Some(influence),
        }
    }
}

/// Persistent command-band state. Unlike terrain/combat policy, this changes
/// only at a strategic pay-cycle transition and must survive checkpoint reload.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitCommandPolicy {
    pub band: CommandBand,
    pub discipline: f64,
    pub refuses_offense: bool,
    pub return_home: bool,
    pub self_defense_only: bool,
    pub home_target: Option<CommandHomeTarget>,
    pub transition_cycle: u64,
}

impl UnitCommandPolicy {
    pub fn paid(seed: f64, sovereign: u16) -> Self {
        Self {
            band: CommandBand::Paid,
            discipline: browser_discipline(seed, sovereign),
            refuses_offense: false,
            return_home: false,
            self_defense_only: false,
            home_target: None,
            transition_cycle: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeDiplomacy {
    /// Directed row-major side hostility owned by the runtime.
    pub hostility: Vec<u8>,
    /// Explicit scenario/controller policy. The runtime never infers active wars from ownership.
    pub active_sides: Vec<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RuntimeConfig {
    pub ai: AiOrderConfig,
    pub front: FrontLayoutConfig,
    pub production: ProductionConfig,
    pub front_refresh_ticks: u64,
    pub census_budget: usize,
    pub census_flush_chunk: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            ai: AiOrderConfig {
                prior_assignment_stickiness: DEFAULT_RUNTIME_FRONT_STICKINESS,
                ..AiOrderConfig::default()
            },
            front: FrontLayoutConfig::default(),
            production: ProductionConfig::default(),
            front_refresh_ticks: DEFAULT_FRONT_REFRESH_TICKS,
            census_budget: DEFAULT_CENSUS_BUDGET,
            census_flush_chunk: DEFAULT_CENSUS_FLUSH_CHUNK,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeCensusCounters {
    pub processed_items: usize,
    pub committed: bool,
    pub flushed_for_strategic_cycle: bool,
    pub territory_generation: u64,
    pub territory_commit_sequence: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeInfluenceCounters {
    pub sources: usize,
    pub cohort: Option<u8>,
    pub application_budget: usize,
    pub diffusion_budget: usize,
    pub diffusion_processed_items: usize,
    pub diffusion_stale_entries: usize,
    pub processed_source_cells: usize,
    pub touched_influence_cells: usize,
    pub changed_controller_cells: usize,
    pub changed_credit_cells: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeAttritionCounters {
    pub damaged_units: usize,
    pub removed_units: usize,
    pub personnel_loss: u64,
    pub equipment_loss: u64,
    pub supply_collapses: usize,
    pub exiled_units: usize,
    pub recovered_personnel: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeAirCounters {
    pub updated: bool,
    pub missions_selected: u32,
    pub interceptions_completed: u32,
    pub strikes_completed: u32,
    pub wings_destroyed: u32,
    pub airfield_captures: usize,
    pub damaged_land_units: usize,
    pub aircraft_loss: u64,
    pub personnel_loss: u64,
    pub equipment_loss: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeReinforcementCounters {
    pub recruited_units: usize,
    pub recruited_personnel: u64,
    pub aircraft_purchased: u64,
    pub aircraft_reinforced: u64,
    pub air_wings_created: u64,
    pub armor_purchased: u64,
    pub armor_reinforced: u64,
    pub armor_formations_created: u64,
    pub airfields_repaired: u64,
    pub aircraft_evacuated: u64,
    pub aircraft_lost_on_capitulation: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeMissileCounters {
    pub launches: usize,
    pub impacts: usize,
    pub damaged_units: usize,
    pub removed_units: usize,
    pub personnel_loss: u64,
    pub equipment_loss: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeStepCounters {
    pub front_refreshed: bool,
    pub front_segments: usize,
    pub front_objectives: usize,
    pub ai: AiPlanningCounters,
    pub simulation: TickCounters,
    pub air: RuntimeAirCounters,
    pub missiles: RuntimeMissileCounters,
    pub reinforcement: RuntimeReinforcementCounters,
    pub naval_planning: NavalPlanningCounters,
    pub operational_execution: OperationalExecutionCounters,
    pub attrition: RuntimeAttritionCounters,
    pub influence: RuntimeInfluenceCounters,
    pub census: RuntimeCensusCounters,
    pub strategic: Option<StrategicCounters>,
    pub strategic_derivation: Option<StrategicDerivationCounters>,
    pub render_updates_enqueued: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Running,
    AwaitingStrategicEffects {
        cycle: u64,
        tick: u64,
        desertion_commands: usize,
        surrender_commands: usize,
        conflict_resolution: bool,
    },
    /// The strategic layer resolved the war. The final immutable publication remains renderable,
    /// but no later simulation step is valid.
    ConflictResolved {
        cycle: u64,
        tick: u64,
        resolution: ConflictResolutionPlan,
    },
    /// A post-mutation invariant failed. No new snapshot was published and stepping is blocked.
    Poisoned,
}

/// Renderer-safe immutable publication. Old snapshots remain valid across later steps.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeSnapshot {
    pub schema_version: &'static str,
    pub tick: u64,
    pub frame: u64,
    pub state: RuntimeState,
    pub frame_snapshot: Arc<FrameSnapshot>,
    pub territory_snapshot: Arc<TerritorySnapshot>,
    /// Immutable live country economies, ordered by stable country ID for observer renderers.
    pub economy_snapshot: Arc<[EconomyState]>,
    pub strategic_snapshot: Option<Arc<StrategicSnapshot>>,
    /// Immutable observer intel, task-force membership, and operational intent for renderers.
    pub operational_snapshot: Option<Arc<OperationalSnapshot>>,
    /// Immutable naval-transport and defender-reaction continuation state for renderers.
    pub operational_execution_snapshot: Option<Arc<OperationalExecutionState>>,
    /// Immutable airfields and live wing mission state for renderers.
    pub air_power_snapshot: Option<Arc<AirPowerState>>,
    /// Immutable silos, in-flight missiles, trails, and impact explosions for observers.
    pub strategic_missile_snapshot: Option<Arc<StrategicMissileState>>,
    pub counters: RuntimeStepCounters,
    pub pending_render_updates: usize,
    pub casualty_totals: Arc<BTreeMap<u16, f64>>,
    /// Exact victim -> attacker personnel-loss attribution used by deterministic surrender
    /// allocation and persisted by mid-war checkpoints.
    pub casualties_by_victim: Arc<BTreeMap<u16, BTreeMap<u16, f64>>>,
    pub gameplay_rng_state: GameplayRngState,
    /// Side-level recruitable personnel returned by non-casualty formation removals.
    pub personnel_reserves: Arc<BTreeMap<usize, f64>>,
    /// Immutable aircraft reserves, funding, and monotonic formation-ID continuation.
    pub reinforcement_snapshot: Option<Arc<ReinforcementState>>,
    /// Immutable armor reserves, quality, and material-maintenance continuation.
    pub material_logistics_snapshot: Option<Arc<MaterialLogisticsState>>,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime configuration is invalid")]
    InvalidConfig,
    #[error("runtime checkpoint references are inconsistent: {0}")]
    InvalidCheckpoint(&'static str),
    #[error("unit policy for id {0} is missing or duplicated")]
    InvalidUnitPolicy(u64),
    #[error("runtime tick or frame counter overflowed")]
    ClockOverflow,
    #[error("runtime is waiting for strategic effects from cycle {cycle} at tick {tick}")]
    AwaitingStrategicEffects { cycle: u64, tick: u64 },
    #[error("runtime conflict was resolved in cycle {cycle} at tick {tick}")]
    ConflictResolved { cycle: u64, tick: u64 },
    #[error("runtime is poisoned after a post-mutation invariant failure")]
    Poisoned,
    #[error("runtime checkpoints can only be captured from a running state")]
    CheckpointUnavailable,
    #[error("AI planning: {0}")]
    Ai(#[from] AiOrderError),
    #[error("front layout: {0}")]
    Front(#[from] FrontLayoutError),
    #[error("battlefield policy: {0}")]
    Battlefield(#[from] BattlefieldError),
    #[error("command policy: {0}")]
    Command(#[from] CommandResolveError),
    #[error("unit simulation: {0}")]
    Simulation(#[from] SimulationError),
    #[error("territory: {0}")]
    Territory(#[from] TerritoryError),
    #[error("strategic simulation: {0}")]
    Strategic(#[from] StrategicError),
    #[error("production input derivation: {0}")]
    Production(#[from] ProductionError),
    #[error("operational AI: {0}")]
    Operational(#[from] OperationalError),
    #[error("air operations: {0}")]
    Air(#[from] AirError),
    #[error("reinforcement: {0}")]
    Reinforcement(#[from] ReinforcementError),
    #[error("operational execution: {0}")]
    OperationalExecution(#[from] OperationalExecutionError),
    #[error("naval planning: {0}")]
    NavalPlanning(#[from] NavalPlanningError),
    #[error("strategic missiles: {0}")]
    StrategicMissile(#[from] StrategicMissileError),
}

fn influence_counters(sources: usize, result: &InfluenceApplyResult) -> RuntimeInfluenceCounters {
    RuntimeInfluenceCounters {
        sources,
        cohort: None,
        application_budget: sources,
        diffusion_budget: 0,
        diffusion_processed_items: 0,
        diffusion_stale_entries: 0,
        processed_source_cells: result.processed_source_cells,
        touched_influence_cells: result.touched_influence_cells.len(),
        changed_controller_cells: result.changed_controller_cells.len(),
        changed_credit_cells: result.changed_credit_cells.len(),
    }
}

fn census_counters(
    result: &CensusStepResult,
    snapshot: &TerritorySnapshot,
    flushed_for_strategic_cycle: bool,
) -> RuntimeCensusCounters {
    RuntimeCensusCounters {
        processed_items: result.processed_items,
        committed: result.committed,
        flushed_for_strategic_cycle,
        territory_generation: snapshot.generation,
        territory_commit_sequence: snapshot.commit_sequence,
    }
}

fn next_casualties(
    previous: &BTreeMap<u16, f64>,
    sovereign_by_unit: &[(u64, u16)],
    after: &FrameSnapshot,
) -> BTreeMap<u16, f64> {
    let mut casualties = previous.clone();
    for event in after.events.iter() {
        if let Ok(index) = sovereign_by_unit.binary_search_by_key(&event.target_id, |entry| entry.0)
        {
            let country = sovereign_by_unit[index].1;
            *casualties.entry(country).or_default() += event.target_personnel_loss as f64;
        }
        if let Ok(index) =
            sovereign_by_unit.binary_search_by_key(&event.attacker_id, |entry| entry.0)
        {
            let country = sovereign_by_unit[index].1;
            *casualties.entry(country).or_default() += event.attacker_personnel_loss as f64;
        }
    }
    casualties
}

fn next_casualties_by_victim(
    previous: &BTreeMap<u16, BTreeMap<u16, f64>>,
    sovereign_by_unit: &[(u64, u16)],
    after: &FrameSnapshot,
) -> BTreeMap<u16, BTreeMap<u16, f64>> {
    let mut casualties = previous.clone();
    for event in after.events.iter() {
        let attacker = sovereign_by_unit
            .binary_search_by_key(&event.attacker_id, |entry| entry.0)
            .ok()
            .map(|index| sovereign_by_unit[index].1);
        let target = sovereign_by_unit
            .binary_search_by_key(&event.target_id, |entry| entry.0)
            .ok()
            .map(|index| sovereign_by_unit[index].1);
        let Some((attacker, target)) = attacker.zip(target) else {
            continue;
        };
        if attacker == target {
            continue;
        }
        if event.target_personnel_loss > 0 {
            *casualties
                .entry(target)
                .or_default()
                .entry(attacker)
                .or_default() += event.target_personnel_loss as f64;
        }
        if event.attacker_personnel_loss > 0 {
            *casualties
                .entry(attacker)
                .or_default()
                .entry(target)
                .or_default() += event.attacker_personnel_loss as f64;
        }
    }
    casualties
}

fn add_attrition_casualties(
    casualties: &mut BTreeMap<u16, f64>,
    sovereign_by_unit: &[(u64, u16)],
    outcome: Option<&DamageOutcome>,
) {
    let Some(outcome) = outcome else {
        return;
    };
    for result in outcome.results.iter() {
        if result.personnel_loss == 0 {
            continue;
        }
        if let Ok(index) = sovereign_by_unit.binary_search_by_key(&result.unit_id, |entry| entry.0)
        {
            *casualties.entry(sovereign_by_unit[index].1).or_default() +=
                result.personnel_loss as f64;
        }
    }
}

fn attrition_counters(
    outcome: Option<&DamageOutcome>,
    stage: Option<&StagedBattlefieldTick>,
    exiled_units: usize,
    recovered_personnel: u64,
) -> RuntimeAttritionCounters {
    let mut counters = RuntimeAttritionCounters {
        exiled_units,
        recovered_personnel,
        ..RuntimeAttritionCounters::default()
    };
    if let Some(outcome) = outcome {
        counters.damaged_units = outcome.results.len();
        counters.removed_units = outcome.removed_ids.len();
        for result in outcome.results.iter() {
            counters.personnel_loss = counters
                .personnel_loss
                .saturating_add(result.personnel_loss);
            counters.equipment_loss = counters
                .equipment_loss
                .saturating_add(result.equipment_loss);
        }
    }
    counters.supply_collapses = outcome.zip(stage).map_or(0, |(outcome, stage)| {
        outcome
            .results
            .iter()
            .filter(|damage| {
                stage
                    .resolved_unit(damage.unit_id)
                    .is_some_and(|result| result.attrition.supply_collapsed)
            })
            .count()
    });
    counters
}

fn air_unit_targets(simulation: &Simulation) -> Result<Vec<AirUnitTarget>, RuntimeError> {
    simulation
        .units
        .iter()
        .map(|unit| {
            Ok(AirUnitTarget {
                id: unit.combat.id,
                side: usize::try_from(unit.combat.side).map_err(|_| {
                    RuntimeError::InvalidCheckpoint("air target side exceeds platform")
                })?,
                country_id: u16::try_from(unit.combat.sovereign)
                    .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?,
                lat: unit.combat.lat,
                lng: unit.combat.lng,
                kind: match unit.combat.kind {
                    UnitKind::Army => AirTargetKind::Army,
                    UnitKind::Armor => AirTargetKind::Armor,
                },
                strength: match unit.combat.kind {
                    // Browser strike candidates score armor by its live
                    // equipment count. `_localAllyCount` is currently unset
                    // for armies, so its `|| 1` fallback is authoritative.
                    UnitKind::Army => 1.0,
                    UnitKind::Armor => unit.combat.equipment as f64,
                },
                priority_area: false,
            })
        })
        .collect()
}

fn air_priority_areas(operations: Option<&OperationalRuntimeState>) -> Vec<AirPriorityArea> {
    let mut areas = Vec::new();
    if let Some(operations) = operations {
        areas.extend(operations.task_forces.iter().filter_map(|task_force| {
            if !matches!(
                task_force.phase,
                TaskForcePhase::Assembling
                    | TaskForcePhase::Attacking
                    | TaskForcePhase::Consolidating
            ) {
                return None;
            }
            task_force
                .target
                .or(task_force.staging_anchor)
                .map(|target| {
                    AirPriorityArea::with_default_radius(
                        task_force.side_index,
                        target.lat,
                        target.lng,
                    )
                })
        }));
    }
    areas.sort_by(|left, right| {
        left.side
            .cmp(&right.side)
            .then_with(|| left.lat.total_cmp(&right.lat))
            .then_with(|| left.lng.total_cmp(&right.lng))
    });
    areas.dedup_by(|left, right| {
        left.side == right.side
            && left.lat.to_bits() == right.lat.to_bits()
            && left.lng.to_bits() == right.lng.to_bits()
            && left.radius_km.to_bits() == right.radius_km.to_bits()
    });
    areas
}

fn airfield_controllers(
    air_power: &AirPowerState,
    territory: &TerritoryControl,
) -> Result<BTreeMap<u64, AirfieldController>, RuntimeError> {
    let world = WorldGridView::new(
        territory.grid_resolution(),
        territory.width(),
        territory.height(),
        territory.land(),
    )
    .map_err(SimulationError::from)?;
    let mut controllers = BTreeMap::new();
    for field in &air_power.airfields {
        let Some(cell) = world.grid_index(field.lat, field.lng) else {
            continue;
        };
        let Ok(side) = usize::try_from(territory.dominant_side()[cell]) else {
            continue;
        };
        if side >= territory.max_sides() || side == field.side {
            continue;
        }
        let physical_occupier = territory.primary_occupier()[cell];
        let controller_country_id =
            if territory.country_to_side().get(&physical_occupier) == Some(&side) {
                physical_occupier
            } else {
                territory
                    .country_to_side()
                    .iter()
                    .find_map(|(&country, &country_side)| (country_side == side).then_some(country))
                    .unwrap_or(field.controller_country_id)
            };
        controllers.insert(
            field.id,
            AirfieldController {
                side,
                controller_country_id,
            },
        );
    }
    Ok(controllers)
}

fn airfield_controller_update_due(tick: u64) -> bool {
    tick % AIR_MISSION_INTERVAL == AIRFIELD_CONTROLLER_PHASE
}

fn air_country_policy(
    strategic: &StrategicSimulation,
    air_power: &AirPowerState,
) -> (BTreeMap<u16, CommandBand>, BTreeMap<u16, f64>) {
    let bands = strategic
        .economies()
        .iter()
        .map(|(&country, economy)| (country, economy.command_band))
        .collect();
    let coverage = air_power
        .country_coverage
        .iter()
        .map(|entry| {
            let capitulated = strategic
                .economies()
                .get(&entry.country_id)
                .is_some_and(|economy| economy.capitulated);
            (
                entry.country_id,
                if capitulated {
                    0.0
                } else {
                    entry.operations_coverage
                },
            )
        })
        .collect();
    (bands, coverage)
}

fn air_damage_commands(outcome: &AirAdvanceOutcome) -> Vec<DamageCommand> {
    let mut damage_by_unit = BTreeMap::<u64, f64>::new();
    for &(unit_id, _, damage) in &outcome.land_damage {
        *damage_by_unit.entry(unit_id).or_default() += damage;
    }
    damage_by_unit
        .into_iter()
        .filter_map(|(unit_id, damage)| {
            (damage.is_finite() && damage > 0.0).then_some(DamageCommand { unit_id, damage })
        })
        .collect()
}

fn air_counters(
    outcome: Option<&AirAdvanceOutcome>,
    damage: Option<&DamageOutcome>,
) -> RuntimeAirCounters {
    let mut counters = RuntimeAirCounters::default();
    if let Some(outcome) = outcome {
        counters.updated = outcome.updated;
        counters.missions_selected = outcome.missions_selected;
        counters.interceptions_completed = outcome.interceptions_completed;
        counters.strikes_completed = outcome.strikes_completed;
        counters.wings_destroyed = outcome.wings_destroyed;
        counters.airfield_captures = outcome.airfield_captures.len();
        counters.aircraft_loss = outcome
            .wing_losses
            .iter()
            .fold(0_u64, |total, loss| total.saturating_add(u64::from(loss.2)));
        counters.personnel_loss = counters
            .personnel_loss
            .saturating_add(counters.aircraft_loss.saturating_mul(AIRCREW_PER_AIRCRAFT));
    }
    if let Some(damage) = damage {
        counters.damaged_land_units = damage.results.len();
        for result in damage.results.iter() {
            counters.personnel_loss = counters
                .personnel_loss
                .saturating_add(result.personnel_loss);
            counters.equipment_loss = counters
                .equipment_loss
                .saturating_add(result.equipment_loss);
        }
    }
    counters
}

fn missile_counters(
    outcome: Option<&StrategicMissileAdvanceOutcome>,
    damages: &[DamageOutcome],
) -> RuntimeMissileCounters {
    let mut counters = RuntimeMissileCounters::default();
    if let Some(outcome) = outcome {
        counters.launches = outcome.launches;
        counters.impacts = outcome.impacts.len();
    }
    for damage in damages {
        counters.damaged_units += damage.results.len();
        counters.removed_units += damage.removed_ids.len();
        for result in damage.results.iter() {
            counters.personnel_loss = counters
                .personnel_loss
                .saturating_add(result.personnel_loss);
            counters.equipment_loss = counters
                .equipment_loss
                .saturating_add(result.equipment_loss);
        }
    }
    counters
}

fn add_aircrew_casualties(
    casualties: &mut BTreeMap<u16, f64>,
    sovereign_by_wing: &[(u64, u16)],
    outcome: Option<&AirAdvanceOutcome>,
) {
    let Some(outcome) = outcome else {
        return;
    };
    for &(wing_id, _, aircraft_lost) in &outcome.wing_losses {
        let Ok(index) = sovereign_by_wing.binary_search_by_key(&wing_id, |entry| entry.0) else {
            continue;
        };
        *casualties.entry(sovereign_by_wing[index].1).or_default() +=
            f64::from(aircraft_lost) * AIRCREW_PER_AIRCRAFT as f64;
    }
}

fn add_air_casualty_attribution(
    casualties: &mut BTreeMap<u16, BTreeMap<u16, f64>>,
    sovereign_by_unit: &[(u64, u16)],
    sovereign_by_wing: &[(u64, u16)],
    outcome: Option<&AirAdvanceOutcome>,
    damage: Option<&DamageOutcome>,
) {
    if let Some(outcome) = outcome {
        for &(wing_id, attacker, aircraft_lost) in &outcome.wing_losses {
            let Ok(index) = sovereign_by_wing.binary_search_by_key(&wing_id, |entry| entry.0)
            else {
                continue;
            };
            let victim = sovereign_by_wing[index].1;
            if attacker != victim {
                *casualties
                    .entry(victim)
                    .or_default()
                    .entry(attacker)
                    .or_default() += f64::from(aircraft_lost) * AIRCREW_PER_AIRCRAFT as f64;
            }
        }
    }
    let Some((outcome, damage)) = outcome.zip(damage) else {
        return;
    };
    let mut weights = BTreeMap::<u64, BTreeMap<u16, f64>>::new();
    for &(target, attacker, amount) in &outcome.land_damage {
        if amount.is_finite() && amount > 0.0 {
            *weights
                .entry(target)
                .or_default()
                .entry(attacker)
                .or_default() += amount;
        }
    }
    for result in damage
        .results
        .iter()
        .filter(|result| result.personnel_loss > 0)
    {
        let Some(attackers) = weights.get(&result.unit_id) else {
            continue;
        };
        let Ok(index) = sovereign_by_unit.binary_search_by_key(&result.unit_id, |entry| entry.0)
        else {
            continue;
        };
        let victim = sovereign_by_unit[index].1;
        let total = attackers.values().sum::<f64>();
        if total <= 0.0 {
            continue;
        }
        for (&attacker, &weight) in attackers {
            if attacker != victim {
                *casualties
                    .entry(victim)
                    .or_default()
                    .entry(attacker)
                    .or_default() += result.personnel_loss as f64 * weight / total;
            }
        }
    }
}

fn hostile_controlled_land_by_side(
    snapshot: &TerritorySnapshot,
    country_to_side: &BTreeMap<u16, usize>,
    hostility: &[u8],
    side_count: usize,
) -> Vec<f64> {
    let mut controlled = vec![0.0; side_count];
    for country in &snapshot.countries {
        if let Some(&side) = country_to_side.get(&country.country_id)
            && side < side_count
        {
            controlled[side] += country.controlled as f64;
        }
    }
    (0..side_count)
        .map(|side| {
            controlled
                .iter()
                .enumerate()
                .filter(|(other, _)| hostility[side * side_count + *other] == 1)
                .map(|(_, cells)| *cells)
                .sum()
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn stage_side_dynamics(
    current: &Option<BTreeMap<usize, SideDynamics>>,
    next_tick: u64,
    frame: u64,
    territory: &TerritorySnapshot,
    country_to_side: &BTreeMap<u16, usize>,
    active_sides: &[u16],
    hostility: &[u8],
    side_count: usize,
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    operations: Option<&OperationalRuntimeState>,
) -> Result<Option<BTreeMap<usize, SideDynamics>>, RuntimeError> {
    let Some(mut next) = current.clone() else {
        return Ok(None);
    };
    let active = active_sides
        .iter()
        .copied()
        .map(usize::from)
        .collect::<BTreeSet<_>>();
    let mut controlled = vec![0_u64; side_count];
    for country in &territory.countries {
        if let Some(&side) = country_to_side.get(&country.country_id)
            && side < side_count
        {
            controlled[side] = controlled[side].saturating_add(country.controlled);
        }
    }

    let mut strength = vec![0.0; side_count];
    let mut deployed_units = vec![0_usize; side_count];
    for unit in &simulation.units {
        let side = unit.combat.side as usize;
        if side >= side_count {
            return Err(RuntimeError::InvalidCheckpoint(
                "side dynamics encountered a unit outside declared topology",
            ));
        }
        let policy = policies
            .get(&unit.combat.id)
            .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
        if unit_deploying(policy, next_tick) {
            continue;
        }
        deployed_units[side] += 1;
        strength[side] += formation_strength(&unit.combat).max(0.0);
    }

    for (&side, dynamics) in &mut next {
        if !active.contains(&side) {
            // Browser rebuilds posture from a BALANCED array every tick and leaves zero-unit or
            // retired sides at that default. Phase/history remain frozen for those stable sides.
            dynamics.posture = WarPosture::Balanced;
            continue;
        }
        if SideDynamics::sample_due(next_tick) {
            dynamics.sample(frame, controlled[side]);
        }
        let mut hostile_strength = 0.0;
        let mut hostile_units = 0_usize;
        for other in 0..side_count {
            if hostility[side * side_count + other] == 1 {
                hostile_strength += strength[other];
                hostile_units += deployed_units[other];
            }
        }
        if let Some(operations) = operations {
            dynamics.posture_override = operations.posture_override(side);
            dynamics.refresh_posture_from_intel(
                deployed_units[side] > 0,
                strength[side],
                operations.known_hostile_strength(side),
            );
        } else {
            dynamics.refresh_posture(
                deployed_units[side] > 0,
                strength[side],
                hostile_units > 0,
                hostile_strength,
            );
        }
    }
    Ok(Some(next))
}

fn apply_runtime_casualties_to_side_dynamics(
    dynamics: &mut Option<BTreeMap<usize, SideDynamics>>,
    previous: &BTreeMap<u16, f64>,
    next: &BTreeMap<u16, f64>,
    country_to_side: &BTreeMap<u16, usize>,
) {
    let Some(dynamics) = dynamics else {
        return;
    };
    let mut by_side = BTreeMap::<usize, f64>::new();
    for (&country, &total) in next {
        let before = previous.get(&country).copied().unwrap_or(0.0);
        let delta = (total - before).max(0.0);
        if let Some(&side) = country_to_side.get(&country) {
            *by_side.entry(side).or_default() += delta;
        }
    }
    for (side, casualties) in by_side {
        if let Some(state) = dynamics.get_mut(&side) {
            state.apply_casualties(casualties);
        }
    }
}

fn apply_personnel_loss_to_side_dynamics(
    dynamics: &mut Option<BTreeMap<usize, SideDynamics>>,
    losses: &BTreeMap<usize, f64>,
) {
    let Some(dynamics) = dynamics else {
        return;
    };
    for (&side, &loss) in losses {
        if let Some(state) = dynamics.get_mut(&side) {
            state.apply_casualties(loss);
        }
    }
}

fn desertion_personnel_loss_by_side(
    simulation: &Simulation,
    commands: &[DesertionCommand],
) -> BTreeMap<usize, f64> {
    let rate_by_country = commands
        .iter()
        .map(|command| (command.country_id, command.rate))
        .collect::<BTreeMap<_, _>>();
    let crew_per_vehicle = simulation.config().combat.armor_crew_per_vehicle;
    let mut losses = BTreeMap::<usize, f64>::new();
    for unit in &simulation.units {
        let Ok(country) = u16::try_from(unit.combat.sovereign) else {
            continue;
        };
        let Some(&rate) = rate_by_country.get(&country) else {
            continue;
        };
        if rate <= 0.0 {
            continue;
        }
        let next_health = unit.combat.health - (unit.combat.health * rate).max(0.0);
        let personnel_loss = if unit.combat.kind == UnitKind::Armor {
            let before = unit.combat.equipment;
            let capacity = if unit.combat.max_equipment > 0 {
                unit.combat.max_equipment
            } else {
                before
            };
            let after = before.min(
                ((capacity as f64) * (next_health / UNIT_HEALTH))
                    .ceil()
                    .max(0.0) as u64,
            );
            let mut loss = (before - after).saturating_mul(crew_per_vehicle) as f64;
            if next_health <= 1.0 {
                loss += after.saturating_mul(crew_per_vehicle) as f64;
            }
            loss
        } else {
            let before = unit.combat.personnel;
            let proportional_loss = (before as f64 * rate).min(before as f64);
            let after = ((before as f64 - proportional_loss).round().max(0.0)) as u64;
            proportional_loss
                + if next_health <= 1.0 {
                    after as f64
                } else {
                    0.0
                }
        };
        *losses.entry(unit.combat.side as usize).or_default() += personnel_loss;
    }
    losses
}

fn ai_units(
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    prior_objective_by_unit: &BTreeMap<u64, u64>,
    operationally_claimed: &BTreeSet<u64>,
    tick: u64,
    battlefield: Option<&StagedBattlefieldTick>,
    side_dynamics: Option<&BTreeMap<usize, SideDynamics>>,
) -> Result<Vec<AiUnitInput>, RuntimeError> {
    simulation
        .units
        .iter()
        .filter(|unit| {
            policies
                .get(&unit.combat.id)
                .is_some_and(|policy| !unit_deploying(policy, tick) && !policy.ai.garrison_excluded)
        })
        .map(|unit| {
            let policy = policies
                .get(&unit.combat.id)
                .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
            let side = SideKey::try_from(unit.combat.side)
                .map_err(|_| RuntimeError::InvalidCheckpoint("unit side exceeds SideKey"))?;
            let at_sea = battlefield
                .and_then(|stage| stage.resolved_unit(unit.combat.id))
                .map_or(unit.combat.at_sea, |result| result.cell.at_sea);
            let ally_weight = battlefield
                .and_then(|stage| stage.ally_weight_by_id.get(&unit.combat.id))
                .copied()
                .unwrap_or(unit.ally_weight);
            let defensive_posture = side_dynamics
                .and_then(|dynamics| dynamics.get(&usize::from(side)))
                .is_some_and(|dynamics| dynamics.posture == WarPosture::Defensive);
            Ok(AiUnitInput {
                id: unit.combat.id,
                side,
                sovereign: unit.combat.sovereign,
                kind: unit.combat.kind,
                lat: unit.combat.lat,
                lng: unit.combat.lng,
                health: unit.combat.health,
                max_health: unit.combat.max_health,
                combat_power: formation_strength(&unit.combat),
                ally_weight,
                at_sea,
                transport: unit.combat.transport,
                base_speed: policy.ai.base_speed,
                movement: policy.ai.movement,
                combat: policy.ai.combat,
                prior_front_objective_id: (!operationally_claimed.contains(&unit.combat.id))
                    .then(|| prior_objective_by_unit.get(&unit.combat.id).copied())
                    .flatten(),
                operationally_claimed: operationally_claimed.contains(&unit.combat.id),
                is_reserve: policy.ai.is_reserve || policy.command.refuses_offense,
                reinforcement_eligible: policy.ai.reinforcement_eligible,
                encircled: policy.ai.encircled,
                defensive_only: policy.command.refuses_offense || defensive_posture,
            })
        })
        .collect()
}

fn garrison_hold_orders(
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    tick: u64,
) -> Result<Vec<ResolvedUnitOrder>, RuntimeError> {
    let mut orders = Vec::new();
    for unit in &simulation.units {
        let policy = policies
            .get(&unit.combat.id)
            .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
        if !policy.ai.garrison_excluded || unit_deploying(policy, tick) {
            continue;
        }
        let mut order = ResolvedUnitOrder::hold(unit.combat.id);
        order.combat = resolved_combat_order(policy.ai.combat);
        orders.push(order);
    }
    orders.sort_unstable_by_key(|order| order.unit_id);
    Ok(orders)
}

fn command_orders(
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    tick: u64,
) -> Result<(Vec<ResolvedUnitOrder>, Vec<FrontAssignmentRecord>), RuntimeError> {
    let mut orders = Vec::new();
    let mut assignments = Vec::new();
    for unit in &simulation.units {
        let policy = policies
            .get(&unit.combat.id)
            .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
        if !policy.command.return_home
            || policy.ai.garrison_excluded
            || unit_deploying(policy, tick)
        {
            continue;
        }
        let Some(target) = policy.command.home_target else {
            continue;
        };
        let delta_lat = target.lat - unit.combat.lat;
        let delta_lng = wrapped_longitude_delta(unit.combat.lng, target.lng);
        let distance = delta_lat.hypot(delta_lng);
        if distance <= 0.15 && !policy.command.self_defense_only {
            continue;
        }
        let mut order = ResolvedUnitOrder::hold(unit.combat.id);
        order.combat = resolved_combat_order(policy.ai.combat);
        if distance > 0.15 {
            order.movement_enabled = true;
            order.dir_lat = delta_lat / distance;
            order.dir_lng = delta_lng / distance;
            order.factors.base_speed = policy.ai.base_speed;
            order.factors.speed_mult =
                policy.ai.movement.terrain_speed_multiplier * policy.ai.movement.speed_multiplier;
            order.factors.plan_speed_mult = policy.ai.movement.plan_speed_multiplier * 0.8;
            order.factors.neutral_penalty = policy.ai.movement.neutral_penalty;
            order.factors.push_readiness = 1.0;
        }
        orders.push(order);
        assignments.push(FrontAssignmentRecord {
            unit_id: unit.combat.id,
            objective_id: None,
            reason: AssignmentReason::Hold,
        });
    }
    orders.sort_unstable_by_key(|order| order.unit_id);
    assignments.sort_unstable_by_key(|assignment| assignment.unit_id);
    Ok((orders, assignments))
}

fn resolved_combat_order(combat: ResolvedCombatModifiers) -> ResolvedCombatOrder {
    ResolvedCombatOrder {
        dealt_multiplier: combat.dealt_multiplier,
        taken_multiplier: combat.taken_multiplier,
        defense_bonus: combat.defense_bonus,
        long_war_defense: combat.long_war_defense,
        mountain: combat.mountain,
        urban: combat.urban,
        current_cell_mountain: combat.current_cell_mountain,
        current_cell_urban: combat.current_cell_urban,
    }
}

fn apply_command_policy_updates(
    policies: &mut BTreeMap<u64, RuntimeUnitPolicy>,
    staged: &StagedCommandPolicies,
) {
    for &unit_id in &staged.changed_unit_ids {
        let resolved = staged
            .policies
            .get(&unit_id)
            .expect("validated command stage covers each changed unit");
        let Some(policy) = policies.get_mut(&unit_id) else {
            // Strategic consequences may have removed the formation after the
            // command policy was staged.
            continue;
        };
        policy.command = UnitCommandPolicy {
            band: resolved.band,
            discipline: resolved.discipline,
            refuses_offense: resolved.refuses_offense,
            return_home: resolved.return_home,
            self_defense_only: resolved.self_defense_only,
            home_target: resolved.home_target,
            transition_cycle: staged.cycle,
        };
        if let Some(influence) = &mut policy.influence {
            influence.refuses_offense = resolved.refuses_offense;
        }
        // Browser clears operational/front/garrison assignment caches whenever
        // a band's refusal cohort changes. Native assignment history is cleared
        // below; these two unit-local roles must not survive the transition.
        policy.ai.is_reserve = false;
        policy.ai.garrison_excluded = false;
    }
}

fn front_layout_units(
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    previous: &BTreeMap<u64, FrontLayoutPrior>,
    operationally_claimed: &BTreeSet<u64>,
    tick: u64,
) -> Result<Vec<FrontLayoutUnit>, RuntimeError> {
    simulation
        .units
        .iter()
        .map(|unit| {
            let policy = policies
                .get(&unit.combat.id)
                .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
            let side_index = SideKey::try_from(unit.combat.side)
                .map_err(|_| RuntimeError::InvalidCheckpoint("unit side exceeds SideKey"))?;
            let prior = previous.get(&unit.combat.id);
            Ok(FrontLayoutUnit {
                id: unit.combat.id,
                side_index,
                lat: unit.combat.lat,
                lng: unit.combat.lng,
                garrison_excluded: policy.ai.garrison_excluded
                    || policy.command.refuses_offense
                    || operationally_claimed.contains(&unit.combat.id),
                deploy_ticks: u32::from(unit_deploying(policy, tick)),
                previous_pair_key: prior.map(|prior| prior.pair_key.clone()),
                previous_segment_idx: prior.map(|prior| prior.segment_idx),
            })
        })
        .collect()
}

fn front_prior_by_unit(prior: &[FrontLayoutPrior]) -> BTreeMap<u64, FrontLayoutPrior> {
    prior
        .iter()
        .cloned()
        .map(|prior| (prior.unit_id, prior))
        .collect()
}

fn front_objectives_by_unit(prior: &[FrontLayoutPrior]) -> BTreeMap<u64, u64> {
    prior
        .iter()
        .map(|prior| (prior.unit_id, prior.objective_id))
        .collect()
}

fn assignments_by_unit(assignments: &[FrontAssignmentRecord]) -> BTreeMap<u64, u64> {
    assignments
        .iter()
        .filter_map(|assignment| {
            assignment
                .objective_id
                .map(|objective| (assignment.unit_id, objective))
        })
        .collect()
}

fn influence_sources(
    snapshot: &FrameSnapshot,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    tick: u64,
    eligible_unit_ids: Option<&BTreeSet<u64>>,
) -> Result<Vec<InfluenceSource>, RuntimeError> {
    snapshot
        .units
        .iter()
        .filter(|unit| unit.health > 0.0)
        .filter_map(|unit| {
            let policy = match policies.get(&unit.id) {
                Some(policy) => policy,
                None => return Some(Err(RuntimeError::InvalidUnitPolicy(unit.id))),
            };
            if unit_deploying(policy, tick) {
                return None;
            }
            if eligible_unit_ids.is_some_and(|eligible| !eligible.contains(&unit.id)) {
                return None;
            }
            let influence = match &policy.influence {
                Some(influence) => influence,
                None => return None,
            };
            let (radius, delta) = resolve_influence_timing(influence, tick);
            let sovereign = match u16::try_from(unit.sovereign) {
                Ok(sovereign) if sovereign > 0 => sovereign,
                _ => {
                    return Some(Err(RuntimeError::InvalidCheckpoint(
                        "unit sovereign exceeds territory country width",
                    )));
                }
            };
            Some(Ok(InfluenceSource {
                id: unit.id,
                side: usize::from(unit.side),
                sovereign,
                beneficiary: influence.beneficiary.unwrap_or(sovereign),
                lat: unit.lat,
                lng: unit.lng,
                radius,
                delta,
                concentration_bonus: influence.concentration_bonus,
                owner_ally_country_ids: influence.owner_ally_country_ids.clone(),
                protected_owner_ids: influence.protected_owner_ids.clone(),
                rebel_de_jure: influence.rebel_de_jure,
                credit_de_jure: influence.credit_de_jure,
                credit_de_jure_by_country: influence.credit_de_jure_by_country.clone(),
                refuses_offense: influence.refuses_offense,
            }))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InfluenceSchedule {
    cohort: usize,
    application_budget: usize,
    selected_units: usize,
}

fn browser_stable_unit_cohort(seed: f64, stride: usize) -> usize {
    debug_assert!(stride > 0);
    // JavaScript's `>>> 0` first floors the positive product and then applies
    // ToUint32. Keeping the modulo in f64 also matches seeds whose product is
    // larger than Rust's integer widths.
    let scaled = (seed.abs() * 2_147_483_647.0).floor();
    let stable_id = if scaled.is_finite() {
        scaled.rem_euclid(4_294_967_296.0) as u32
    } else {
        0
    };
    stable_id as usize % stride
}

fn browser_influence_budgets(nonempty_side_count: usize) -> (usize, usize) {
    let optimization_factor = (nonempty_side_count as f64 / 2.0).max(1.0);
    let max_applications = (300.0 / optimization_factor).floor().max(50.0) as usize;
    let diffusion_budget = (1_600.0 / optimization_factor).floor().max(400.0) as usize;
    (max_applications, diffusion_budget)
}

fn browser_influence_sources(
    units: &[SimulationUnit],
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    tick: u64,
    frame: u64,
    nonempty_side_count: usize,
) -> Result<(Vec<InfluenceSource>, InfluenceSchedule), RuntimeError> {
    const STRIDE: usize = 3;

    let cohort = tick as usize % STRIDE;
    let (max_applications, _) = browser_influence_budgets(nonempty_side_count);
    let share = max_applications / STRIDE;
    let remainder = max_applications % STRIDE;
    let application_budget = (share + usize::from(cohort < remainder)).max(1);
    let start = if units.is_empty() {
        0
    } else {
        ((u128::from(tick) * 30) % units.len() as u128) as usize
    };
    let mut schedule = InfluenceSchedule {
        cohort,
        application_budget,
        selected_units: 0,
    };
    let mut sources = Vec::with_capacity(application_budget.min(units.len()));

    for offset in 0..units.len() {
        let unit = &units[(start + offset) % units.len()];
        let policy = policies
            .get(&unit.combat.id)
            .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
        let seed = policy
            .influence
            .as_ref()
            .and_then(|influence| influence.browser_temporal_seed)
            .ok_or(RuntimeError::InvalidCheckpoint(
                "influence runtime requires a temporal seed for every live formation",
            ))?;
        if browser_stable_unit_cohort(seed, STRIDE) != cohort
            || unit_deploying(policy, tick)
            || frame.saturating_sub(unit.combat.last_combat_tick) <= 5
            || unit.combat.last_combat_tick > frame
        {
            continue;
        }
        schedule.selected_units += 1;
        if schedule.selected_units > application_budget {
            break;
        }
        let influence = policy
            .influence
            .as_ref()
            .expect("the temporal seed came from an influence policy");
        let (radius, delta) = resolve_influence_timing(influence, tick);
        let sovereign = u16::try_from(unit.combat.sovereign).map_err(|_| {
            RuntimeError::InvalidCheckpoint("unit sovereign exceeds territory country width")
        })?;
        if sovereign == 0 {
            return Err(RuntimeError::InvalidCheckpoint(
                "unit sovereign exceeds territory country width",
            ));
        }
        let side = usize::try_from(unit.combat.side).map_err(|_| {
            RuntimeError::InvalidCheckpoint("unit side exceeds territory side width")
        })?;
        sources.push(InfluenceSource {
            id: unit.combat.id,
            side,
            sovereign,
            beneficiary: influence.beneficiary.unwrap_or(sovereign),
            lat: unit.combat.lat,
            lng: unit.combat.lng,
            radius,
            // One cohort member represents three elapsed logical ticks.
            delta: delta * STRIDE as f64,
            concentration_bonus: influence.concentration_bonus,
            owner_ally_country_ids: influence.owner_ally_country_ids.clone(),
            protected_owner_ids: influence.protected_owner_ids.clone(),
            rebel_de_jure: influence.rebel_de_jure,
            credit_de_jure: influence.credit_de_jure,
            credit_de_jure_by_country: influence.credit_de_jure_by_country.clone(),
            refuses_offense: influence.refuses_offense,
        });
    }

    schedule.selected_units = sources.len();
    Ok((sources, schedule))
}

fn resolve_influence_timing(policy: &UnitInfluencePolicy, tick: u64) -> (f64, f64) {
    let Some(seed) = policy.browser_temporal_seed else {
        return (policy.radius, policy.delta);
    };
    let tick = tick as f64;
    let ramp = (0.05 + tick / 600.0 * 0.95).min(1.0);
    let delta_noise = 0.8 + (seed * 1_000.0 + tick * 0.05).sin() * 0.4;
    let radius_noise = 0.9 + (seed * 500.0 + tick * 0.1).sin() * 0.2;
    (
        policy.radius * radius_noise,
        policy.delta * ramp * delta_noise,
    )
}

fn unit_deploying(policy: &RuntimeUnitPolicy, tick: u64) -> bool {
    policy.ai.deploy_until_tick > 0 && tick <= policy.ai.deploy_until_tick
}

/// Fully owned hand-off from scenario loading into deterministic runtime orchestration.
pub struct RuntimeCheckpoint {
    pub tick: u64,
    pub frame: u64,
    pub war_grace_end: u64,
    pub simulation: Simulation,
    pub territory: TerritoryControl,
    pub strategic: StrategicSimulation,
    pub scenario: ScenarioProduction,
    pub diplomacy: RuntimeDiplomacy,
    pub unit_policies: Vec<RuntimeUnitPolicy>,
    /// Optional raw, live battlefield inputs. When absent, the resolved unit policies above are
    /// treated as frozen for compatibility with earlier checkpoints.
    pub battlefield: Option<BattlefieldRuntimeState>,
    /// Explicit objectives are accepted as a checkpoint fallback and replaced by the scheduled
    /// front-layout adapter once its first refresh succeeds.
    pub objectives: Vec<FrontObjective>,
    pub prior_objective_by_unit: BTreeMap<u64, u64>,
    pub front_prior_by_unit: BTreeMap<u64, FrontLayoutPrior>,
    pub last_front_refresh_tick: Option<u64>,
    pub casualties: BTreeMap<u16, f64>,
    pub casualties_by_victim: BTreeMap<u16, BTreeMap<u16, f64>>,
    pub gameplay_rng: GameplayRngState,
    pub personnel_reserves: BTreeMap<usize, f64>,
    pub side_dynamics: Option<BTreeMap<usize, SideDynamics>>,
    /// Required by checkpoint v5; absent checkpoints retain legacy planning semantics.
    pub operations: Option<OperationalRuntimeState>,
    /// Native-only deterministic plan origination. Browser v1-v6 handoffs omit this state.
    pub naval_planning: Option<NavalPlanningState>,
    /// Required by checkpoint v6; absent checkpoints retain legacy land-only execution.
    pub operational_execution: Option<OperationalExecutionState>,
    /// Required by checkpoint v6 and advanced atomically with operational execution.
    pub air_power: Option<AirPowerState>,
    /// Checkpoint v10 aircraft reserves, funding, and monotonic formation allocators.
    pub reinforcement: Option<ReinforcementState>,
    /// Checkpoint v11 armor reserves and material-maintenance state.
    pub material_logistics: Option<MaterialLogisticsState>,
    /// Checkpoint v12 autonomous strategic missile continuation state.
    pub strategic_missiles: Option<StrategicMissileState>,
}

/// Owned state captured at a committed, serialization-ready runtime boundary.
///
/// Renderer queues are intentionally omitted, while history-dependent planning
/// state is retained so a restored runtime continues on the same tick phase.
#[derive(Clone, Debug)]
pub struct NativeRuntimeCheckpointState {
    pub tick: u64,
    pub frame: u64,
    pub war_grace_end: u64,
    pub runtime_config: RuntimeConfig,
    pub simulation_config: SimulationConfig,
    pub units: Vec<SimulationUnit>,
    pub territory_config: TerritoryConfig,
    pub territory_committed_state: TerritoryCommittedState,
    pub influence_runtime: Option<crate::diffusion::InfluenceRuntimeState>,
    pub strategic_cycle: u64,
    pub economies: Vec<EconomyState>,
    pub occupations: Vec<OccupationState>,
    pub scenario: ScenarioProduction,
    pub diplomacy: RuntimeDiplomacy,
    pub unit_policies: Vec<RuntimeUnitPolicy>,
    pub battlefield: Option<BattlefieldRuntimeState>,
    pub objectives: Vec<FrontObjective>,
    pub prior_objective_by_unit: BTreeMap<u64, u64>,
    pub front_prior_by_unit: BTreeMap<u64, FrontLayoutPrior>,
    pub last_front_refresh_tick: Option<u64>,
    pub casualties: BTreeMap<u16, f64>,
    pub casualties_by_victim: BTreeMap<u16, BTreeMap<u16, f64>>,
    pub gameplay_rng: GameplayRngState,
    pub personnel_reserves: BTreeMap<usize, f64>,
    pub side_dynamics: Option<BTreeMap<usize, SideDynamics>>,
    pub operations: Option<OperationalRuntimeState>,
    pub naval_planning: Option<NavalPlanningState>,
    pub operational_execution: Option<OperationalExecutionState>,
    pub air_power: Option<AirPowerState>,
    pub reinforcement: Option<ReinforcementState>,
    pub material_logistics: Option<MaterialLogisticsState>,
    pub strategic_missiles: Option<StrategicMissileState>,
}

/// Shared native simulation owner. No mutable kernel state is exposed to renderers.
pub struct NativeRuntime {
    config: RuntimeConfig,
    tick: u64,
    frame: u64,
    war_grace_end: u64,
    simulation: Simulation,
    territory: TerritoryControl,
    strategic: StrategicSimulation,
    scenario: ScenarioProduction,
    diplomacy: RuntimeDiplomacy,
    unit_policies: BTreeMap<u64, RuntimeUnitPolicy>,
    battlefield: Option<BattlefieldRuntimeState>,
    battlefield_urban_mask: Option<Vec<u8>>,
    unit_sovereign_by_id: Vec<(u64, u16)>,
    objectives: Vec<FrontObjective>,
    front_prior_by_unit: BTreeMap<u64, FrontLayoutPrior>,
    last_front_refresh_tick: Option<u64>,
    prior_objective_by_unit: BTreeMap<u64, u64>,
    casualties: BTreeMap<u16, f64>,
    casualties_by_victim: BTreeMap<u16, BTreeMap<u16, f64>>,
    gameplay_rng: GameplayRng,
    personnel_reserves: BTreeMap<usize, f64>,
    side_dynamics: Option<BTreeMap<usize, SideDynamics>>,
    operations: Option<OperationalRuntimeState>,
    naval_planning: Option<NavalPlanningState>,
    naval_topology: Option<Arc<NavalTopology>>,
    naval_route_workspace: Option<NavalRouteWorkspace>,
    operational_execution: Option<OperationalExecutionState>,
    air_power: Option<AirPowerState>,
    reinforcement: Option<ReinforcementState>,
    material_logistics: Option<MaterialLogisticsState>,
    strategic_missiles: Option<StrategicMissileState>,
    state: RuntimeState,
    latest: Arc<RuntimeSnapshot>,
    render_updates: VecDeque<Arc<TerritoryRenderUpdate>>,
}

struct StagedStrategicEffects {
    simulation: Option<Simulation>,
    territory: Option<TerritoryControl>,
    removed_ids: Vec<u64>,
    state: RuntimeState,
    fronts_invalidated: bool,
    desertion_personnel_loss_by_side: BTreeMap<usize, f64>,
}

struct StagedBattlefieldTick {
    policies: BTreeMap<u64, RuntimeUnitPolicy>,
    next_unit_state: BTreeMap<u64, BattlefieldUnitState>,
    resolved_by_id: BTreeMap<u64, BattlefieldUnitResult>,
    local: BattlefieldLocalTacticsResult,
    local_by_id: BTreeMap<u64, crate::battlefield::BattlefieldLocalUnitResult>,
    ally_weight_by_id: BTreeMap<u64, f64>,
    influence_eligible: BTreeSet<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PublishedUnitVisuals {
    armor_supported: bool,
    is_alpenjager: bool,
    encircled_ticks: u64,
    mountain_intensity: f32,
}

fn enrich_unit_visuals(
    frame: &mut FrameSnapshot,
    battlefield: Option<&BattlefieldRuntimeState>,
    territory: &TerritoryControl,
    staged: Option<&StagedBattlefieldTick>,
) -> Result<(), RuntimeError> {
    let Some(battlefield) = battlefield else {
        return Ok(());
    };
    let world = WorldGridView::new(
        territory.grid_resolution(),
        territory.width(),
        territory.height(),
        territory.land(),
    )
    .map_err(BattlefieldError::from)?;
    let mut units = frame.units.to_vec();
    for unit in &mut units {
        let memory = staged
            .and_then(|stage| stage.next_unit_state.get(&unit.id))
            .or_else(|| battlefield.units.get(&unit.id))
            .ok_or(RuntimeError::InvalidCheckpoint(
                "battlefield state does not cover a published unit",
            ))?;
        let mountain_intensity = staged
            .and_then(|stage| stage.resolved_unit(unit.id))
            .map(|resolved| resolved.cell.mountain_intensity as f32)
            .unwrap_or_else(|| {
                if battlefield.mountains_enabled {
                    world
                        .grid_index(unit.lat, unit.lng)
                        .map_or(0.0, |cell| battlefield.terrain_intensity[cell])
                } else {
                    0.0
                }
            });
        if !mountain_intensity.is_finite() || !(0.0..=1.0).contains(&mountain_intensity) {
            return Err(RuntimeError::InvalidCheckpoint(
                "published unit mountain intensity is invalid",
            ));
        }
        let visuals = PublishedUnitVisuals {
            armor_supported: staged
                .and_then(|stage| stage.local_unit(unit.id))
                .map_or(unit.armor_supported, |local| local.armor_supported),
            is_alpenjager: memory.is_alpenjager,
            encircled_ticks: staged
                .and_then(|stage| stage.resolved_unit(unit.id))
                .map_or(memory.encircled_ticks, |resolved| resolved.encircled_ticks),
            mountain_intensity,
        };
        unit.armor_supported = visuals.armor_supported;
        unit.is_alpenjager = visuals.is_alpenjager;
        unit.encircled_ticks = visuals.encircled_ticks;
        unit.mountain_intensity = visuals.mountain_intensity;
    }
    debug_assert!(units.windows(2).all(|pair| pair[0].id < pair[1].id));
    frame.units = units.into();
    Ok(())
}

struct StagedInfluenceTick {
    transaction: InfluenceTransaction,
    source_count: usize,
    schedule: Option<InfluenceSchedule>,
    diffusion_budget: usize,
    diffusion: DiffusionQueueResult,
}

struct StagedCommandPolicies {
    cycle: u64,
    policies: BTreeMap<u64, ResolvedCommandPolicy>,
    changed_unit_ids: BTreeSet<u64>,
}

#[derive(Default)]
struct StagedRecruitment {
    units: Vec<SimulationUnit>,
    policies: Vec<RuntimeUnitPolicy>,
    battlefield_units: BTreeMap<u64, BattlefieldUnitState>,
    treasury_costs: Vec<(u16, f64)>,
    counters: RuntimeReinforcementCounters,
}

impl StagedBattlefieldTick {
    fn resolved_unit(&self, unit_id: u64) -> Option<&BattlefieldUnitResult> {
        self.resolved_by_id.get(&unit_id)
    }

    fn local_unit(&self, unit_id: u64) -> Option<&crate::battlefield::BattlefieldLocalUnitResult> {
        self.local_by_id.get(&unit_id)
    }
}

fn air_pay_cycle_counters(outcome: &AirPayCycleOutcome) -> RuntimeReinforcementCounters {
    let mut counters = RuntimeReinforcementCounters::default();
    for country in &outcome.countries {
        counters.aircraft_purchased = counters.aircraft_purchased.saturating_add(u64::from(
            country
                .fighters_purchased
                .saturating_add(country.strike_purchased),
        ));
        counters.aircraft_reinforced = counters.aircraft_reinforced.saturating_add(u64::from(
            country
                .fighters_reinforced
                .saturating_add(country.strike_reinforced),
        ));
        counters.air_wings_created = counters.air_wings_created.saturating_add(
            country
                .wing_creation
                .iter()
                .filter(|creation| creation.created_wing_id.is_some())
                .count() as u64,
        );
    }
    counters
}

fn material_pay_cycle_counters(outcome: &MaterialPayCycleOutcome) -> RuntimeReinforcementCounters {
    let mut counters = RuntimeReinforcementCounters::default();
    for country in &outcome.countries {
        counters.aircraft_purchased = counters.aircraft_purchased.saturating_add(u64::from(
            country
                .air
                .fighters_purchased
                .saturating_add(country.air.strike_purchased),
        ));
        counters.aircraft_reinforced = counters.aircraft_reinforced.saturating_add(u64::from(
            country
                .air
                .fighters_reinforced
                .saturating_add(country.air.strike_reinforced),
        ));
        counters.air_wings_created = counters.air_wings_created.saturating_add(
            country
                .air
                .wing_creation
                .iter()
                .filter(|creation| creation.created_wing_id.is_some())
                .count() as u64,
        );
        counters.armor_purchased = counters
            .armor_purchased
            .saturating_add(country.armor_purchased);
        counters.armor_reinforced = counters
            .armor_reinforced
            .saturating_add(country.armor_reinforced);
        counters.armor_formations_created = counters
            .armor_formations_created
            .saturating_add(u64::from(country.armor_creation.created_unit_id.is_some()));
        counters.airfields_repaired = counters
            .airfields_repaired
            .saturating_add(u64::from(country.airfields_repaired));
        counters.aircraft_evacuated = counters
            .aircraft_evacuated
            .saturating_add(country.evacuated_aircraft);
        counters.aircraft_lost_on_capitulation = counters
            .aircraft_lost_on_capitulation
            .saturating_add(country.lost_aircraft);
    }
    counters
}

fn recruit_cell_center(cell: usize, grid: GridSpec) -> (f64, f64) {
    let x = cell % grid.width;
    let y = cell / grid.width;
    (
        -90.0 + (y as f64 + 0.5) * grid.grid_res,
        -180.0 + (x as f64 + 0.5) * grid.grid_res,
    )
}

fn recruitment_spawn_cell(
    country_id: u16,
    side: usize,
    supply_failed: bool,
    scenario: &ScenarioProduction,
    territory: &TerritoryControl,
    rng: &mut GameplayRng,
) -> Option<(f64, f64, usize)> {
    let world = WorldGridView::new(
        territory.grid_resolution(),
        territory.width(),
        territory.height(),
        territory.land(),
    )
    .ok()?;
    let friendly_cities = scenario
        .cities
        .iter()
        .filter(|city| {
            city.owner_id == country_id
                && city.cell < territory.total_cells()
                && territory.land()[city.cell] > 0
                && territory.dominant_side()[city.cell] == side as i16
        })
        .collect::<Vec<_>>();
    if !friendly_cities.is_empty() {
        let frontline_cities = friendly_cities
            .iter()
            .copied()
            .filter(|city| {
                let neighbors = [
                    city.cell.saturating_add(1),
                    city.cell.saturating_sub(1),
                    city.cell.saturating_add(territory.width()),
                    city.cell.saturating_sub(territory.width()),
                ];
                neighbors.into_iter().any(|cell| {
                    if cell >= territory.total_cells() {
                        return false;
                    }
                    let Ok(other) = usize::try_from(territory.dominant_side()[cell]) else {
                        return false;
                    };
                    other < territory.max_sides()
                        && territory.hostility_matrix()[side * territory.max_sides() + other] == 1
                })
            })
            .collect::<Vec<_>>();
        let candidates = if !supply_failed && !frontline_cities.is_empty() {
            &frontline_cities
        } else {
            &friendly_cities
        };
        let selected =
            ((rng.next_f64() * candidates.len() as f64).floor() as usize).min(candidates.len() - 1);
        let city = candidates[selected];
        let mut lat = city.lat + (rng.next_f64() - 0.5) * scenario.grid.grid_res * 0.8;
        let mut lng = city.lng + (rng.next_f64() - 0.5) * scenario.grid.grid_res * 0.8;
        let valid = world.grid_index(lat, lng).is_some_and(|cell| {
            territory.world_control()[cell] == country_id
                && territory.dominant_side()[cell] == side as i16
        });
        if !valid {
            lat = city.lat;
            lng = city.lng;
        }
        return world.grid_index(lat, lng).map(|cell| (lat, lng, cell));
    }

    let sample_step = territory
        .total_cells()
        .checked_div(500_000)
        .unwrap_or(0)
        .max(1);
    let cells = (0..territory.total_cells())
        .step_by(sample_step)
        .filter(|&cell| {
            territory.land()[cell] == 2
                && territory.world_control()[cell] == country_id
                && territory.dominant_side()[cell] == side as i16
        })
        .collect::<Vec<_>>();
    if cells.is_empty() {
        return None;
    }
    let selected = ((rng.next_f64() * cells.len() as f64).floor() as usize).min(cells.len() - 1);
    let cell = cells[selected];
    let (lat, lng) = recruit_cell_center(cell, scenario.grid);
    Some((lat, lng, cell))
}

#[allow(clippy::too_many_arguments)]
fn stage_recruitment(
    next_tick: u64,
    next_frame: u64,
    simulation: &Simulation,
    scenario: &ScenarioProduction,
    territory: &TerritoryControl,
    territory_snapshot: &TerritorySnapshot,
    strategic: &StrategicSimulation,
    side_dynamics: Option<&BTreeMap<usize, SideDynamics>>,
    reinforcement: &mut ReinforcementState,
    personnel_reserves: &mut BTreeMap<usize, f64>,
    rng: &mut GameplayRng,
    config: ProductionConfig,
    battlefield: Option<&BattlefieldRuntimeState>,
) -> Result<StagedRecruitment, RuntimeError> {
    let mut staged = StagedRecruitment::default();
    let maximum_live_id = simulation
        .units
        .iter()
        .map(|unit| unit.combat.id)
        .max()
        .unwrap_or(0);
    if reinforcement.next_unit_id <= maximum_live_id {
        return Err(RuntimeError::InvalidCheckpoint(
            "reinforcement next unit ID does not exceed live units",
        ));
    }

    let aggregates = territory_snapshot
        .countries
        .iter()
        .map(|country| (country.country_id, country))
        .collect::<BTreeMap<_, _>>();
    let mut country_strength = BTreeMap::<u16, f64>::new();
    let mut side_unit_count = BTreeMap::<usize, usize>::new();
    for unit in &simulation.units {
        let country_id = u16::try_from(unit.combat.sovereign)
            .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?;
        *country_strength.entry(country_id).or_default() += formation_strength(&unit.combat);
        *side_unit_count
            .entry(unit.combat.side as usize)
            .or_default() += 1;
    }
    let allies = territory.country_to_side().iter().fold(
        BTreeMap::<usize, BTreeSet<u16>>::new(),
        |mut by_side, (&country, &side)| {
            by_side.entry(side).or_default().insert(country);
            by_side
        },
    );
    let capital_targets = scenario
        .cities
        .iter()
        .filter(|city| {
            city.capital
                && city.owner_id > 0
                && city.cell < territory.total_cells()
                && territory.country_to_side().contains_key(&city.owner_id)
        })
        .map(|city| {
            (
                city.owner_id,
                CommandHomeTarget {
                    cell: city.cell,
                    lat: city.lat,
                    lng: city.lng,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let command_world = CommandWorld {
        grid_resolution: territory.grid_resolution(),
        width: territory.width(),
        height: territory.height(),
        land: territory.land(),
        world_control: territory.world_control(),
        dominant_side: territory.dominant_side(),
        country_side: territory.country_to_side(),
        capital_targets: &capital_targets,
    };
    let mut available_treasury = strategic
        .economies()
        .iter()
        .map(|(&country, economy)| (country, economy.treasury))
        .collect::<BTreeMap<_, _>>();
    let mut costs = BTreeMap::<u16, f64>::new();
    let mut countries = territory
        .country_to_side()
        .iter()
        .map(|(&country, &side)| (side, country))
        .collect::<Vec<_>>();
    countries.sort_unstable();

    for (side, country_id) in countries {
        let Some(aggregate) = aggregates.get(&country_id) else {
            continue;
        };
        let Some(country) = scenario
            .countries
            .iter()
            .find(|country| country.country_id == country_id)
        else {
            return Err(RuntimeError::InvalidCheckpoint(
                "recruitment country is absent from production",
            ));
        };
        let Some(economy) = strategic.economies().get(&country_id) else {
            return Err(RuntimeError::InvalidCheckpoint(
                "recruitment country has no economy",
            ));
        };
        if economy.capitulated {
            continue;
        }
        let current_units = country_strength.get(&country_id).copied().unwrap_or(0.0);
        let current_land = aggregate.controlled as f64;
        let initial_land = f64::from(country.initial_core_cells.max(1));
        let city_count = scenario
            .cities
            .iter()
            .filter(|city| city.owner_id == country_id)
            .count() as f64;
        let size_factor = (current_land / 2_000.0).max(1.0);
        let density_scale = 1.0 / size_factor.powf(0.35);
        let land_city_multiplier = 1.0 + city_count * 0.12;
        let land_cap = (current_land
            * config.unit_density_factor
            * 1.5
            * density_scale
            * land_city_multiplier)
            .floor()
            .max(8.0);
        let flexible_limit = f64::from(config.max_units_per_side)
            * (1.0 + (current_land / 4_000.0 + city_count * 0.15).min(3.0));
        let supply_failed = !economy.capital_held;
        let mut absolute_cap = land_cap.min(flexible_limit);
        if supply_failed {
            absolute_cap = (absolute_cap * 0.3).floor().max(15.0);
        }
        let reserve = personnel_reserves.get(&side).copied().unwrap_or(0.0);
        let reserve_equivalents = if reserve > 0.0 {
            (reserve / PERSONNEL_PER_FORMATION).ceil()
        } else {
            0.0
        };
        if reserve_equivalents == 0.0 {
            absolute_cap = 0.0;
        } else {
            absolute_cap = absolute_cap.min(current_units + reserve_equivalents);
        }
        if current_units >= absolute_cap {
            continue;
        }

        let control_ratio = current_land / initial_land;
        let city_bonus = 0.5 + city_count * 0.5;
        let scale_factor = (current_land / 2_000.0).powf(0.4).max(0.8);
        let underdog = if control_ratio < 0.4 {
            (0.4 - control_ratio) * 2.0
        } else {
            0.0
        };
        let annexation_multiplier = if control_ratio < 0.6 {
            1.0 + (0.6 - control_ratio) * 4.0
        } else {
            1.0
        };
        let early_multiplier = if next_frame < EARLY_RECRUIT_TICKS {
            1.0 + (EARLY_RECRUIT_MULTIPLIER - 1.0)
                * (1.0 - next_frame as f64 / EARLY_RECRUIT_TICKS as f64)
        } else {
            1.0
        };
        let cap_fill = 1.0 + (1.0 - (current_units / absolute_cap.max(1.0)).min(1.0)) * 3.0;
        let phase = side_dynamics
            .and_then(|states| states.get(&side))
            .map_or(WarPhase::Stalemate, |state| state.phase);
        let phase_multiplier = match phase {
            WarPhase::Collapsing => 5.0,
            WarPhase::Retreating => 3.0,
            WarPhase::Advancing | WarPhase::Stalemate => 1.0,
        };
        let manpower_multiplier = if supply_failed {
            let ratio = side_dynamics
                .and_then(|states| states.get(&side))
                .filter(|state| state.initial_personnel > 0.0)
                .map_or(1.0, |state| {
                    (state.current_personnel / state.initial_personnel * 2.0).clamp(0.0, 1.0)
                });
            0.1 * ratio
        } else {
            1.0
        };
        let recruitment_chance = (BASE_RECRUITMENT_CHANCE
            * scale_factor
            * (control_ratio + city_bonus + underdog)
            * annexation_multiplier
            * early_multiplier
            * cap_fill
            * phase_multiplier
            * manpower_multiplier)
            .min(0.8);
        let attempts = if phase == WarPhase::Collapsing && current_units < absolute_cap * 0.3 {
            5
        } else if phase == WarPhase::Retreating && current_units < absolute_cap * 0.5 {
            3
        } else {
            1
        };

        for _ in 0..attempts {
            if rng.next_f64() >= recruitment_chance {
                continue;
            }
            let recruited_personnel = if supply_failed { 400_u64 } else { 1_000_u64 };
            let treasury = available_treasury.entry(country_id).or_default();
            let reserve = personnel_reserves.entry(side).or_default();
            if economy.arrears_cycles >= 1.0
                || *treasury < RECRUITMENT_COST
                || *reserve < recruited_personnel as f64
                || side_unit_count.get(&side).copied().unwrap_or(0)
                    >= config.max_units_per_side as usize
            {
                continue;
            }
            let Some((lat, lng, cell)) =
                recruitment_spawn_cell(country_id, side, supply_failed, scenario, territory, rng)
            else {
                continue;
            };
            let mountain = battlefield.is_some_and(|state| {
                state.mountains_enabled
                    && state
                        .terrain_intensity
                        .get(cell)
                        .is_some_and(|intensity| *intensity > 0.35)
            });
            let is_alpenjager = mountain && rng.next_f64() < 0.4;
            let browser_unit_seed = rng.next_f64();
            let unit_id = reinforcement.next_unit_id;
            reinforcement.next_unit_id = reinforcement
                .next_unit_id
                .checked_add(1)
                .ok_or(ReinforcementError::Overflow("land unit ID"))?;
            let health = if supply_failed {
                UNIT_HEALTH * 0.4
            } else {
                UNIT_HEALTH
            };
            staged.units.push(SimulationUnit {
                combat: CombatUnit {
                    id: unit_id,
                    side: side as u64,
                    sovereign: u64::from(country_id),
                    kind: UnitKind::Army,
                    lat,
                    lng,
                    health,
                    max_health: UNIT_HEALTH,
                    personnel: recruited_personnel,
                    personnel_capacity: 1_000,
                    equipment: 0,
                    max_equipment: 0,
                    quality: 50.0,
                    transport: false,
                    armor_supported: false,
                    landing_penalty_active: false,
                    at_sea: false,
                    last_combat_tick: 0,
                    victory_boost_ticks: 0,
                },
                dir_lat: 0.0,
                dir_lng: 0.0,
                coast_stuck_ticks: 0,
                armor_landing_penalty_until_tick: 0,
                is_support: false,
                ally_weight: 1.0,
            });
            let discipline = browser_discipline(browser_unit_seed, country_id);
            let resolved = resolve_command_policy(
                CommandUnitState {
                    id: unit_id,
                    sovereign_id: country_id,
                    side,
                    discipline,
                },
                economy.command_band,
                command_world,
            )?;
            let mut policy = RuntimeUnitPolicy::standard(unit_id, country_id);
            policy.ai.deploy_until_tick = next_tick
                .checked_add(RECRUIT_DEPLOY_TICKS)
                .ok_or(RuntimeError::ClockOverflow)?;
            let influence = policy
                .influence
                .as_mut()
                .expect("standard recruitment policy has influence");
            influence.browser_temporal_seed = Some(browser_unit_seed);
            influence.owner_ally_country_ids = allies.get(&side).cloned().unwrap_or_default();
            policy.command = UnitCommandPolicy {
                band: resolved.band,
                discipline: resolved.discipline,
                refuses_offense: resolved.refuses_offense,
                return_home: resolved.return_home,
                self_defense_only: resolved.self_defense_only,
                home_target: resolved.home_target,
                transition_cycle: strategic.cycle(),
            };
            staged.policies.push(policy);
            staged.battlefield_units.insert(
                unit_id,
                BattlefieldUnitState {
                    is_alpenjager,
                    cohesion_seed: browser_unit_seed,
                    ..BattlefieldUnitState::default()
                },
            );
            *treasury -= RECRUITMENT_COST;
            *reserve -= recruited_personnel as f64;
            *costs.entry(country_id).or_default() += RECRUITMENT_COST;
            *side_unit_count.entry(side).or_default() += 1;
            staged.counters.recruited_units += 1;
            staged.counters.recruited_personnel = staged
                .counters
                .recruited_personnel
                .saturating_add(recruited_personnel);
        }
    }
    staged.treasury_costs = costs.into_iter().collect();
    Ok(staged)
}

fn operational_unit_inputs(
    simulation: &Simulation,
    battlefield: Option<&StagedBattlefieldTick>,
) -> Result<Vec<OperationalUnitInput>, RuntimeError> {
    simulation
        .units
        .iter()
        .map(|unit| {
            let country_id = u16::try_from(unit.combat.sovereign)
                .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?;
            let resolved = battlefield.and_then(|stage| stage.resolved_unit(unit.combat.id));
            let memory = battlefield.and_then(|stage| stage.next_unit_state.get(&unit.combat.id));
            Ok(OperationalUnitInput {
                unit_id: unit.combat.id,
                side_index: unit.combat.side as usize,
                country_id,
                position: OperationalPoint {
                    lat: unit.combat.lat,
                    lng: unit.combat.lng,
                },
                power: formation_strength(&unit.combat).max(0.0),
                readiness: if unit.combat.max_health > 0.0 {
                    (unit.combat.health / unit.combat.max_health).clamp(0.0, 1.0)
                } else {
                    0.0
                },
                supply_collapsed_tick: memory.and_then(|state| state.supply_collapsed_tick),
                encircled_ticks: resolved.map_or(0, |result| result.encircled_ticks),
            })
        })
        .collect()
}

fn operational_claimed_unit_ids(
    operations: Option<&OperationalRuntimeState>,
    execution: Option<&OperationalExecutionState>,
) -> BTreeSet<u64> {
    let mut claimed = BTreeSet::new();
    if let Some(operations) = operations {
        claimed.extend(
            operations
                .task_forces
                .iter()
                .flat_map(|task_force| task_force.members.iter())
                .map(|member| member.unit_id),
        );
    }
    if let Some(execution) = execution {
        claimed.extend(
            execution
                .naval_operations
                .iter()
                .flat_map(|operation| operation.members.iter())
                .map(|member| member.unit_id),
        );
        claimed.extend(
            execution
                .defender_reactions
                .iter()
                .flat_map(|reaction| reaction.unit_ids.iter().copied()),
        );
    }
    claimed
}

fn execution_unit_inputs(
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    tick: u64,
    battlefield: Option<&StagedBattlefieldTick>,
    planning: &AiPlanningResult,
    operations: Option<&OperationalRuntimeState>,
) -> Result<Vec<ExecutionUnitInput>, RuntimeError> {
    let engaged = planning
        .assignments
        .iter()
        .filter(|assignment| {
            matches!(
                assignment.reason,
                AssignmentReason::Contact | AssignmentReason::Retreat
            )
        })
        .map(|assignment| assignment.unit_id)
        .collect::<BTreeSet<_>>();
    simulation
        .units
        .iter()
        .map(|unit| {
            let policy = policies
                .get(&unit.combat.id)
                .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
            let side = usize::try_from(unit.combat.side)
                .map_err(|_| RuntimeError::InvalidCheckpoint("unit side exceeds platform"))?;
            let country = u16::try_from(unit.combat.sovereign)
                .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?;
            let at_sea = battlefield
                .and_then(|stage| stage.resolved_unit(unit.combat.id))
                .map_or(unit.combat.at_sea, |resolved| resolved.cell.at_sea);
            Ok(ExecutionUnitInput {
                unit_id: unit.combat.id,
                side,
                country,
                position: ExecutionPoint {
                    lat: unit.combat.lat,
                    lng: unit.combat.lng,
                },
                transport: unit.combat.transport,
                at_sea,
                deploying: unit_deploying(policy, tick),
                engaged: engaged.contains(&unit.combat.id),
                operationally_assigned: policy.ai.garrison_excluded
                    || operations
                        .is_some_and(|operations| operations.contains_unit(unit.combat.id)),
            })
        })
        .collect()
}

fn controlled_side_at_point(
    territory: &TerritoryControl,
    point: ExecutionPoint,
) -> Result<Option<usize>, RuntimeError> {
    let world = WorldGridView::new(
        territory.grid_resolution(),
        territory.width(),
        territory.height(),
        territory.land(),
    )
    .map_err(SimulationError::from)?;
    let Some(cell) = world.grid_index(point.lat, point.lng) else {
        return Ok(None);
    };
    let dominant = territory.dominant_side()[cell];
    if dominant >= 0 {
        return Ok(usize::try_from(dominant).ok());
    }
    let country = if territory.primary_occupier()[cell] != 0 {
        territory.primary_occupier()[cell]
    } else {
        territory.world_control()[cell]
    };
    Ok(territory.country_to_side().get(&country).copied())
}

fn defender_threats(
    execution: &OperationalExecutionState,
    operations: Option<&OperationalRuntimeState>,
    units: &[ExecutionUnitInput],
    territory: &TerritoryControl,
    hostility: &[u8],
    side_count: usize,
) -> Result<Vec<DefenderThreat>, RuntimeError> {
    let mut threats = Vec::new();
    let hostile = |attacker: usize, defender: usize| {
        attacker < side_count
            && defender < side_count
            && hostility[attacker * side_count + defender] == 1
    };
    for operation in &execution.naval_operations {
        if operation.kind != NavalOperationKind::Invasion {
            continue;
        }
        let phase = match operation.phase {
            NavalOperationPhase::Transit => DefenderThreatPhase::Transit,
            NavalOperationPhase::Landing => DefenderThreatPhase::Landing,
            _ => continue,
        };
        let defender_side = if let Some(side) = operation.enemy_side {
            side
        } else if let Some(side) = controlled_side_at_point(territory, operation.target)? {
            side
        } else {
            continue;
        };
        if defender_side == operation.side || !hostile(operation.side, defender_side) {
            continue;
        }
        let enemy_force = match phase {
            DefenderThreatPhase::Landing => units
                .iter()
                .filter(|unit| {
                    unit.side == operation.side
                        && unit.position.distance_squared(operation.target) < 4.0
                })
                .count(),
            DefenderThreatPhase::Transit => operation
                .members
                .iter()
                .filter(|member| units.iter().any(|unit| unit.unit_id == member.unit_id))
                .count(),
            DefenderThreatPhase::Execution => unreachable!(),
        };
        threats.push(DefenderThreat {
            signature: operation.signature.clone(),
            defender_side,
            enemy_side: operation.side,
            kind: DefenderThreatKind::NavalInvasion,
            phase,
            target: operation.target,
            enemy_force,
            active: true,
        });
    }
    if let Some(operations) = operations {
        for task_force in &operations.task_forces {
            if task_force.phase != TaskForcePhase::Attacking {
                continue;
            }
            let Some(target) = task_force.target else {
                continue;
            };
            let target = ExecutionPoint {
                lat: target.lat,
                lng: target.lng,
            };
            let Some(defender_side) = controlled_side_at_point(territory, target)? else {
                continue;
            };
            if defender_side == task_force.side_index
                || !hostile(task_force.side_index, defender_side)
            {
                continue;
            }
            let enemy_force = units
                .iter()
                .filter(|unit| {
                    unit.side == task_force.side_index
                        && unit.position.distance_squared(target) < 9.0
                })
                .count();
            threats.push(DefenderThreat {
                signature: task_force.plan_signature.clone(),
                defender_side,
                enemy_side: task_force.side_index,
                kind: DefenderThreatKind::LandOffensive,
                phase: DefenderThreatPhase::Execution,
                target,
                enemy_force,
                active: true,
            });
        }
    }
    threats.sort_by(|left, right| left.signature.cmp(&right.signature));
    Ok(threats)
}

fn apply_execution_outputs(
    simulation: &mut Simulation,
    planning: &mut AiPlanningResult,
    outcome: &OperationalExecutionOutcome,
) -> Result<BTreeSet<u64>, RuntimeError> {
    let protected = planning
        .assignments
        .iter()
        .filter(|assignment| {
            matches!(
                assignment.reason,
                AssignmentReason::Contact | AssignmentReason::Retreat
            )
        })
        .map(|assignment| assignment.unit_id)
        .collect::<BTreeSet<_>>();
    let mut units = simulation
        .units
        .iter_mut()
        .map(|unit| (unit.combat.id, unit))
        .collect::<BTreeMap<_, _>>();
    for update in &outcome.transport_updates {
        let unit = units
            .get_mut(&update.unit_id)
            .ok_or(RuntimeError::InvalidCheckpoint(
                "execution transport update omitted a live unit",
            ))?;
        unit.combat.transport = update.transport;
    }
    let steering = outcome
        .steering
        .iter()
        .map(|steering| (steering.unit_id, steering))
        .collect::<BTreeMap<_, _>>();
    for order in &mut planning.orders {
        let Some(steering) = steering.get(&order.unit_id) else {
            continue;
        };
        if protected.contains(&order.unit_id) {
            continue;
        }
        order.preferred_target_id = None;
        order.movement_enabled = steering.movement_enabled;
        order.dir_lat = steering.dir_lat;
        order.dir_lng = steering.dir_lng;
        order.factors.plan_speed_mult *= steering.speed_multiplier;
        if !order.factors.plan_speed_mult.is_finite() {
            return Err(RuntimeError::InvalidCheckpoint(
                "execution steering produced an invalid speed multiplier",
            ));
        }
    }
    Ok(steering.keys().copied().collect())
}

fn tactical_contact_observations(
    planning: &AiPlanningResult,
    units: &[OperationalUnitInput],
    simulation: &Simulation,
) -> Vec<TacticalContactObservation> {
    let operational_by_id = units
        .iter()
        .map(|unit| (unit.unit_id, *unit))
        .collect::<BTreeMap<_, _>>();
    let kind_by_id = simulation
        .units
        .iter()
        .map(|unit| {
            (
                unit.combat.id,
                match unit.combat.kind {
                    UnitKind::Army => "army",
                    UnitKind::Armor => "armor",
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut observed = BTreeMap::new();
    for contact in &planning.contacts {
        let Some(target_id) = contact.target_unit_id else {
            continue;
        };
        let Some(observer) = operational_by_id.get(&contact.unit_id) else {
            continue;
        };
        let Some(target) = operational_by_id.get(&target_id) else {
            continue;
        };
        observed.insert(
            (observer.side_index, target_id),
            TacticalContactObservation {
                observer_side: observer.side_index,
                enemy_side: target.side_index,
                target_unit_id: target_id,
                target_country_id: target.country_id,
                target_position: target.position,
                observed_power: target.power,
                kind: kind_by_id
                    .get(&target_id)
                    .copied()
                    .unwrap_or("army")
                    .to_owned(),
            },
        );
    }
    observed.into_values().collect()
}

fn apply_operational_steering(
    planning: &mut AiPlanningResult,
    operations: &OperationalRuntimeState,
    units: &[OperationalUnitInput],
) -> Result<BTreeSet<u64>, RuntimeError> {
    let protected = planning
        .assignments
        .iter()
        .filter(|assignment| {
            matches!(
                assignment.reason,
                AssignmentReason::Contact | AssignmentReason::Retreat
            )
        })
        .map(|assignment| assignment.unit_id)
        .collect::<BTreeSet<_>>();
    let steering = operations
        .steering(units)
        .into_iter()
        .map(|steering| (steering.unit_id, steering))
        .collect::<BTreeMap<_, _>>();
    for order in &mut planning.orders {
        let Some(steering) = steering.get(&order.unit_id) else {
            continue;
        };
        if protected.contains(&order.unit_id) {
            continue;
        }
        order.preferred_target_id = None;
        order.movement_enabled = steering.movement_enabled;
        order.dir_lat = steering.dir_lat;
        order.dir_lng = steering.dir_lng;
        order.factors.plan_speed_mult *= steering.speed_multiplier;
        if !order.factors.plan_speed_mult.is_finite() {
            return Err(RuntimeError::InvalidCheckpoint(
                "operational steering produced an invalid speed multiplier",
            ));
        }
    }
    Ok(steering.keys().copied().collect())
}

fn operational_country_inputs(
    simulation: &Simulation,
    scenario: &ScenarioProduction,
    territory: &TerritorySnapshot,
) -> Result<Vec<CountryOperationalInput>, RuntimeError> {
    let crew_per_vehicle = simulation.config().combat.armor_crew_per_vehicle;
    let mut personnel = BTreeMap::<u16, f64>::new();
    for unit in &simulation.units {
        let country = u16::try_from(unit.combat.sovereign)
            .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?;
        let amount = if unit.combat.kind == UnitKind::Armor {
            unit.combat.equipment.saturating_mul(crew_per_vehicle) as f64
        } else {
            unit.combat.personnel as f64
        };
        *personnel.entry(country).or_default() += amount;
    }
    let aggregate = territory
        .countries
        .iter()
        .map(|country| (country.country_id, country))
        .collect::<BTreeMap<_, _>>();
    Ok(scenario
        .countries
        .iter()
        .map(|country| {
            let current = aggregate.get(&country.country_id).copied();
            CountryOperationalInput {
                country_id: country.country_id,
                initial_land: u64::from(country.initial_core_cells.max(1)),
                controlled: current.map_or(0, |state| state.controlled),
                cities_controlled: current.map_or(0, |state| state.cities_controlled),
                current_personnel: personnel.get(&country.country_id).copied().unwrap_or(0.0),
                // Browser role metadata is not part of the native scenario contract. Treat a
                // country as offense-capable; viable task-force gating still prevents a phantom
                // offensive override.
                offensive_role: true,
            }
        })
        .collect())
}

fn material_armor_profiles(
    simulation: &Simulation,
    scenario: &ScenarioProduction,
    strategic: &StrategicSimulation,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<BTreeMap<u16, (u64, f64)>, RuntimeError> {
    let mut live_capacity = BTreeMap::<u16, u64>::new();
    let mut weighted_quality = BTreeMap::<u16, (f64, u64)>::new();
    for unit in &simulation.units {
        if unit.combat.kind != UnitKind::Armor {
            continue;
        }
        let country_id = u16::try_from(unit.combat.sovereign)
            .map_err(|_| RuntimeError::InvalidCheckpoint("invalid armor sovereign"))?;
        let capacity = unit.combat.max_equipment.max(unit.combat.equipment);
        let live = live_capacity.entry(country_id).or_default();
        *live = live
            .checked_add(capacity)
            .ok_or(RuntimeError::InvalidCheckpoint("armor capacity overflowed"))?;
        let quality = weighted_quality.entry(country_id).or_default();
        quality.0 += unit.combat.quality * capacity as f64;
        quality.1 = quality.1.saturating_add(capacity);
    }
    country_to_side
        .keys()
        .copied()
        .map(|country_id| {
            let country = scenario
                .countries
                .iter()
                .find(|country| country.country_id == country_id)
                .ok_or(RuntimeError::InvalidCheckpoint(
                    "material country is absent from production",
                ))?;
            let economy =
                strategic
                    .economies()
                    .get(&country_id)
                    .ok_or(RuntimeError::InvalidCheckpoint(
                        "material country has no economy",
                    ))?;
            let fallback =
                (country.expected_army_units.max(0.0) * economy.economic_strength.max(0.0)).sqrt()
                    * 8.0;
            let fallback = fallback.round().clamp(0.0, MAX_ARMOR_CAPACITY as f64) as u64;
            let capacity = fallback.max(live_capacity.get(&country_id).copied().unwrap_or(0));
            let quality = weighted_quality
                .get(&country_id)
                .filter(|(_, weight)| *weight > 0)
                .map_or(50.0, |(weighted, weight)| weighted / *weight as f64)
                .clamp(0.0, 100.0);
            Ok((country_id, (capacity, quality)))
        })
        .collect()
}

fn material_home_positions(
    simulation: &Simulation,
    scenario: &ScenarioProduction,
    territory: &TerritoryControl,
) -> BTreeMap<u16, (f64, f64)> {
    territory
        .country_to_side()
        .iter()
        .filter_map(|(&country_id, &side)| {
            let capital = scenario.cities.iter().find(|city| {
                city.owner_id == country_id
                    && city.capital
                    && city.cell < territory.total_cells()
                    && territory.world_control()[city.cell] == country_id
                    && territory.dominant_side()[city.cell] == side as i16
            });
            if let Some(capital) = capital {
                return Some((country_id, (capital.lat, capital.lng)));
            }
            if let Some(unit) = simulation
                .units
                .iter()
                .find(|unit| unit.combat.sovereign == u64::from(country_id) && !unit.combat.at_sea)
            {
                return Some((country_id, (unit.combat.lat, unit.combat.lng)));
            }
            territory
                .world_control()
                .iter()
                .zip(territory.dominant_side())
                .enumerate()
                .find(|&(_, (&controller, &dominant))| {
                    controller == country_id && dominant == side as i16
                })
                .map(|(cell, _)| {
                    let x = cell % territory.width();
                    let y = cell / territory.width();
                    (
                        country_id,
                        (
                            -90.0 + (y as f64 + 0.5) * territory.grid_resolution(),
                            -180.0 + (x as f64 + 0.5) * territory.grid_resolution(),
                        ),
                    )
                })
        })
        .collect()
}

impl NativeRuntime {
    /// Validate the complete checkpoint, establish a coherent census, and publish tick zero (or
    /// the restored checkpoint tick) atomically from the caller's perspective.
    pub fn new(
        config: RuntimeConfig,
        mut checkpoint: RuntimeCheckpoint,
    ) -> Result<Self, RuntimeError> {
        validate_runtime_config(config)?;
        validate_diplomacy(&checkpoint.diplomacy, &checkpoint.territory)?;
        validate_scenario_grid(&checkpoint.scenario, &checkpoint.territory)?;
        if let Some(missiles) = &checkpoint.strategic_missiles {
            missiles.validate(checkpoint.territory.max_sides())?;
        }

        let mut unit_policies = collect_unit_policies(
            &checkpoint.simulation,
            checkpoint.unit_policies,
            &checkpoint.territory,
        )?;
        if checkpoint.territory.has_influence_runtime()
            && checkpoint.simulation.units.iter().any(|unit| {
                unit_policies
                    .get(&unit.combat.id)
                    .and_then(|policy| policy.influence.as_ref())
                    .and_then(|influence| influence.browser_temporal_seed)
                    .is_none()
            })
        {
            return Err(RuntimeError::InvalidCheckpoint(
                "influence runtime requires a temporal seed for every live formation",
            ));
        }
        hydrate_missing_command_home_targets(
            &checkpoint.simulation,
            &mut unit_policies,
            &checkpoint.strategic,
            &checkpoint.scenario,
            &checkpoint.territory,
        )?;
        validate_command_checkpoint(
            &checkpoint.simulation,
            &unit_policies,
            &checkpoint.strategic,
        )?;
        let battlefield_urban_mask = if let Some(battlefield) = &checkpoint.battlefield {
            let world = WorldGridView::new(
                checkpoint.territory.grid_resolution(),
                checkpoint.territory.width(),
                checkpoint.territory.height(),
                checkpoint.territory.land(),
            )
            .map_err(BattlefieldError::from)?;
            let live_unit_ids = checkpoint
                .simulation
                .units
                .iter()
                .map(|unit| unit.combat.id)
                .collect::<Vec<_>>();
            battlefield.validate(
                world,
                checkpoint.territory.max_sides(),
                checkpoint.territory.country_to_side(),
                &live_unit_ids,
            )?;
            if let Some((&unit_id, _)) = battlefield.units.iter().find(|(_, state)| {
                state
                    .supply_collapsed_tick
                    .is_some_and(|collapse_tick| collapse_tick > checkpoint.tick)
            }) {
                return Err(BattlefieldError::InvalidBattlefieldUnitState(unit_id).into());
            }
            Some(battlefield.urban_cell_mask(world)?)
        } else {
            None
        };
        let mut unit_sovereign_by_id = checkpoint
            .simulation
            .units
            .iter()
            .map(|unit| {
                u16::try_from(unit.combat.sovereign)
                    .map(|country| (unit.combat.id, country))
                    .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        unit_sovereign_by_id.sort_unstable_by_key(|entry| entry.0);
        validate_prior_assignments(
            &checkpoint.simulation,
            &checkpoint.objectives,
            &checkpoint.prior_objective_by_unit,
        )?;
        validate_front_planner_state(
            checkpoint.tick,
            &checkpoint.simulation,
            &checkpoint.objectives,
            &checkpoint.front_prior_by_unit,
            checkpoint.last_front_refresh_tick,
        )?;
        validate_casualties(&checkpoint.casualties, &checkpoint.scenario)?;
        validate_casualties_by_victim(&checkpoint.casualties_by_victim, &checkpoint.scenario)?;
        validate_personnel_reserves(
            &checkpoint.personnel_reserves,
            checkpoint.territory.max_sides(),
        )?;
        let controlled_cell_limit = u64::try_from(checkpoint.territory.total_cells())
            .map_err(|_| RuntimeError::InvalidCheckpoint("territory cell count exceeds u64"))?;
        validate_side_dynamics_state(
            checkpoint.side_dynamics.as_ref(),
            checkpoint.territory.max_sides(),
            checkpoint.frame,
            controlled_cell_limit,
        )?;
        if checkpoint.operational_execution.is_some() != checkpoint.air_power.is_some() {
            return Err(RuntimeError::InvalidCheckpoint(
                "operational execution and air power must be restored together",
            ));
        }
        if let Some(air_power) = &checkpoint.air_power {
            air_power.validate()?;
            let coverage_countries = air_power
                .country_coverage
                .iter()
                .map(|coverage| coverage.country_id)
                .collect::<BTreeSet<_>>();
            let topology_countries = checkpoint
                .territory
                .country_to_side()
                .keys()
                .copied()
                .collect::<BTreeSet<_>>();
            let economy_countries = checkpoint
                .strategic
                .economies()
                .keys()
                .copied()
                .collect::<BTreeSet<_>>();
            if coverage_countries != topology_countries
                || coverage_countries != economy_countries
                || air_power.airfields.iter().any(|field| {
                    field.side >= checkpoint.territory.max_sides()
                        || checkpoint
                            .territory
                            .country_to_side()
                            .get(&field.controller_country_id)
                            != Some(&field.side)
                        || !checkpoint
                            .territory
                            .country_to_side()
                            .contains_key(&field.owner_country_id)
                })
                || air_power.wings.iter().any(|wing| {
                    wing.side >= checkpoint.territory.max_sides()
                        || checkpoint
                            .territory
                            .country_to_side()
                            .get(&wing.sovereign_country_id)
                            != Some(&wing.side)
                })
            {
                return Err(RuntimeError::InvalidCheckpoint(
                    "air power disagrees with the stable country and side topology",
                ));
            }
        }
        if checkpoint.reinforcement.is_none()
            && let Some(air_power) = checkpoint.air_power.as_ref()
        {
            let next_unit_id = checkpoint
                .simulation
                .units
                .iter()
                .map(|unit| unit.combat.id)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(RuntimeError::InvalidCheckpoint(
                    "unit ID allocator overflowed",
                ))?;
            let next_air_wing_id = air_power
                .wings
                .iter()
                .map(|wing| wing.id)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(RuntimeError::InvalidCheckpoint(
                    "air-wing ID allocator overflowed",
                ))?;
            checkpoint.reinforcement = Some(bootstrap_reinforcement_state(
                air_power,
                next_unit_id,
                next_air_wing_id,
                checkpoint.territory.country_to_side(),
                checkpoint.territory.max_sides(),
            )?);
        }
        match (&checkpoint.reinforcement, &checkpoint.air_power) {
            (Some(reinforcement), Some(air_power)) => {
                reinforcement.validate(
                    air_power,
                    checkpoint.territory.country_to_side(),
                    checkpoint.territory.max_sides(),
                )?;
                if checkpoint
                    .simulation
                    .units
                    .iter()
                    .any(|unit| unit.combat.id >= reinforcement.next_unit_id)
                {
                    return Err(RuntimeError::InvalidCheckpoint(
                        "next formation ID must exceed every issued live ID",
                    ));
                }
            }
            (Some(_), None) => {
                return Err(RuntimeError::InvalidCheckpoint(
                    "reinforcement state requires air power",
                ));
            }
            (None, _) => {}
        }
        if checkpoint.material_logistics.is_none() && checkpoint.reinforcement.is_some() {
            let profiles = material_armor_profiles(
                &checkpoint.simulation,
                &checkpoint.scenario,
                &checkpoint.strategic,
                checkpoint.territory.country_to_side(),
            )?;
            checkpoint.material_logistics = Some(MaterialLogisticsState::bootstrap(
                &checkpoint.simulation.units,
                &profiles,
                checkpoint.territory.country_to_side(),
                checkpoint.territory.max_sides(),
            )?);
        }
        match (&checkpoint.material_logistics, &checkpoint.reinforcement) {
            (Some(material), Some(_)) => material.validate(
                &checkpoint.simulation.units,
                checkpoint.territory.country_to_side(),
                checkpoint.territory.max_sides(),
            )?,
            (Some(_), None) => {
                return Err(RuntimeError::InvalidCheckpoint(
                    "material logistics requires reinforcement state",
                ));
            }
            (None, _) => {}
        }
        if let Some(operations) = &checkpoint.operations {
            let live_units = checkpoint
                .simulation
                .units
                .iter()
                .map(|unit| (unit.combat.id, unit.combat.side as usize))
                .collect::<BTreeMap<_, _>>();
            let countries = checkpoint
                .scenario
                .countries
                .iter()
                .map(|country| country.country_id)
                .collect::<BTreeSet<_>>();
            operations.validate(
                checkpoint.territory.max_sides(),
                &live_units,
                &countries,
                checkpoint.tick,
            )?;
            for side in &operations.sides {
                let declared = (0..checkpoint.territory.max_sides())
                    .filter(|enemy| {
                        checkpoint.diplomacy.hostility
                            [side.side_index * checkpoint.territory.max_sides() + *enemy]
                            == 1
                    })
                    .collect::<Vec<_>>();
                if side.hostile_side_indices != declared {
                    return Err(RuntimeError::InvalidCheckpoint(
                        "operational intel hostility disagrees with runtime diplomacy",
                    ));
                }
            }
        }
        if let Some(execution) = &checkpoint.operational_execution {
            let units = checkpoint
                .simulation
                .units
                .iter()
                .map(|unit| {
                    let policy = unit_policies
                        .get(&unit.combat.id)
                        .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
                    Ok(ExecutionUnitInput {
                        unit_id: unit.combat.id,
                        side: usize::try_from(unit.combat.side).map_err(|_| {
                            RuntimeError::InvalidCheckpoint("unit side exceeds platform")
                        })?,
                        country: u16::try_from(unit.combat.sovereign).map_err(|_| {
                            RuntimeError::InvalidCheckpoint("invalid unit sovereign")
                        })?,
                        position: ExecutionPoint {
                            lat: unit.combat.lat,
                            lng: unit.combat.lng,
                        },
                        transport: unit.combat.transport,
                        at_sea: unit.combat.at_sea,
                        deploying: unit_deploying(policy, checkpoint.tick),
                        engaged: false,
                        operationally_assigned: policy.ai.garrison_excluded
                            || checkpoint
                                .operations
                                .as_ref()
                                .is_some_and(|operations| operations.contains_unit(unit.combat.id)),
                    })
                })
                .collect::<Result<Vec<_>, RuntimeError>>()?;
            execution.validate_units(checkpoint.territory.max_sides(), &units, checkpoint.tick)?;
        }
        if checkpoint.naval_planning.is_some()
            && (checkpoint.operations.is_none()
                || checkpoint.operational_execution.is_none()
                || checkpoint.air_power.is_none())
        {
            return Err(RuntimeError::InvalidCheckpoint(
                "naval planning requires operational AI, execution, and air state",
            ));
        }
        let naval_topology = if let Some(planning) = &checkpoint.naval_planning {
            planning.validate_with_execution(
                checkpoint.territory.max_sides(),
                checkpoint
                    .operational_execution
                    .as_ref()
                    .expect("naval planning dependency was checked above"),
            )?;
            let world = WorldGridView::new(
                checkpoint.territory.grid_resolution(),
                checkpoint.territory.width(),
                checkpoint.territory.height(),
                checkpoint.territory.land(),
            )
            .map_err(SimulationError::from)?;
            Some(Arc::new(NavalTopology::derive(world)?))
        } else {
            None
        };
        let naval_route_workspace = checkpoint
            .naval_planning
            .as_ref()
            .map(|_| NavalRouteWorkspace::default());

        // A checkpoint may have an in-progress bounded census. Construction finishes it before
        // exposing the first cross-kernel publication.
        let territory_snapshot = checkpoint.territory.flush_census(config.census_flush_chunk);
        let mut initial_frame = checkpoint
            .simulation
            .initial_snapshot(checkpoint.tick, checkpoint.frame);
        enrich_unit_visuals(
            &mut initial_frame,
            checkpoint.battlefield.as_ref(),
            &checkpoint.territory,
            None,
        )?;
        let frame_snapshot = Arc::new(initial_frame);
        validate_production_boundary(
            checkpoint.tick,
            &checkpoint.scenario,
            &checkpoint.territory,
            &territory_snapshot,
            &frame_snapshot,
            &checkpoint.diplomacy,
            &checkpoint.strategic,
            &checkpoint.casualties,
            &config.production,
        )?;
        validate_ai_boundary(
            config.ai,
            checkpoint.tick,
            &checkpoint.simulation,
            &unit_policies,
            &checkpoint.prior_objective_by_unit,
            &checkpoint.objectives,
            &checkpoint.territory,
            &checkpoint.diplomacy,
        )?;

        let mut render_updates = VecDeque::new();
        if let Some(update) = checkpoint.territory.drain_render_update() {
            render_updates.push_back(update);
        }
        let strategic_snapshot = checkpoint.strategic.latest_snapshot();
        if strategic_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.tick > checkpoint.tick)
        {
            return Err(RuntimeError::InvalidCheckpoint(
                "strategic snapshot is newer than the runtime clock",
            ));
        }
        let initial_state = strategic_snapshot
            .as_ref()
            .map_or(RuntimeState::Running, |snapshot| {
                if let Some(resolution) = snapshot.conflict_resolution {
                    RuntimeState::ConflictResolved {
                        cycle: snapshot.cycle,
                        tick: snapshot.tick,
                        resolution: resolution.into(),
                    }
                } else if snapshot.desertions.is_empty() && snapshot.surrenders.is_empty() {
                    RuntimeState::Running
                } else {
                    RuntimeState::AwaitingStrategicEffects {
                        cycle: snapshot.cycle,
                        tick: snapshot.tick,
                        desertion_commands: snapshot.desertions.len(),
                        surrender_commands: snapshot.surrenders.len(),
                        conflict_resolution: snapshot.conflict_resolution.is_some(),
                    }
                }
            });
        let latest = Arc::new(RuntimeSnapshot {
            schema_version: NATIVE_RUNTIME_SCHEMA_VERSION,
            tick: checkpoint.tick,
            frame: checkpoint.frame,
            state: initial_state,
            frame_snapshot,
            territory_snapshot,
            economy_snapshot: checkpoint
                .strategic
                .economies()
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .into(),
            strategic_snapshot,
            operational_snapshot: checkpoint
                .operations
                .as_ref()
                .map(|operations| Arc::new(operations.snapshot(checkpoint.tick))),
            operational_execution_snapshot: checkpoint
                .operational_execution
                .as_ref()
                .map(|execution| Arc::new(execution.clone())),
            air_power_snapshot: checkpoint
                .air_power
                .as_ref()
                .map(|air_power| Arc::new(air_power.clone())),
            strategic_missile_snapshot: checkpoint
                .strategic_missiles
                .as_ref()
                .map(|missiles| Arc::new(missiles.clone())),
            counters: RuntimeStepCounters::default(),
            pending_render_updates: render_updates.len(),
            casualty_totals: Arc::new(checkpoint.casualties.clone()),
            casualties_by_victim: Arc::new(checkpoint.casualties_by_victim.clone()),
            gameplay_rng_state: checkpoint.gameplay_rng,
            personnel_reserves: Arc::new(checkpoint.personnel_reserves.clone()),
            reinforcement_snapshot: checkpoint
                .reinforcement
                .as_ref()
                .map(|reinforcement| Arc::new(reinforcement.clone())),
            material_logistics_snapshot: checkpoint
                .material_logistics
                .as_ref()
                .map(|material| Arc::new(material.clone())),
        });

        Ok(Self {
            config,
            tick: checkpoint.tick,
            frame: checkpoint.frame,
            war_grace_end: checkpoint.war_grace_end,
            simulation: checkpoint.simulation,
            territory: checkpoint.territory,
            strategic: checkpoint.strategic,
            scenario: checkpoint.scenario,
            diplomacy: checkpoint.diplomacy,
            unit_policies,
            battlefield: checkpoint.battlefield,
            battlefield_urban_mask,
            unit_sovereign_by_id,
            objectives: checkpoint.objectives,
            front_prior_by_unit: checkpoint.front_prior_by_unit,
            last_front_refresh_tick: checkpoint.last_front_refresh_tick,
            prior_objective_by_unit: checkpoint.prior_objective_by_unit,
            casualties: checkpoint.casualties,
            casualties_by_victim: checkpoint.casualties_by_victim,
            gameplay_rng: GameplayRng::restore(checkpoint.gameplay_rng),
            personnel_reserves: checkpoint.personnel_reserves,
            side_dynamics: checkpoint.side_dynamics,
            operations: checkpoint.operations,
            naval_planning: checkpoint.naval_planning,
            naval_topology,
            naval_route_workspace,
            operational_execution: checkpoint.operational_execution,
            air_power: checkpoint.air_power,
            reinforcement: checkpoint.reinforcement,
            material_logistics: checkpoint.material_logistics,
            strategic_missiles: checkpoint.strategic_missiles,
            state: initial_state,
            latest,
            render_updates,
        })
    }

    pub fn latest_snapshot(&self) -> Arc<RuntimeSnapshot> {
        self.latest.clone()
    }

    /// Immutable scenario features used by renderer initialization. Live
    /// simulation state remains available only through atomic publications.
    pub fn scenario(&self) -> &ScenarioProduction {
        &self.scenario
    }

    pub const fn state(&self) -> RuntimeState {
        self.state
    }

    pub const fn tick(&self) -> u64 {
        self.tick
    }

    pub const fn frame(&self) -> u64 {
        self.frame
    }

    pub fn pending_render_updates(&self) -> usize {
        self.render_updates.len()
    }

    /// Pop exactly one renderer update. Later deltas cannot overtake it.
    pub fn pop_render_update(&mut self) -> Option<Arc<TerritoryRenderUpdate>> {
        self.render_updates.pop_front()
    }

    pub fn pending_strategic_snapshot(&self) -> Option<Arc<StrategicSnapshot>> {
        matches!(self.state, RuntimeState::AwaitingStrategicEffects { .. })
            .then(|| self.latest.strategic_snapshot.clone())
            .flatten()
    }

    /// Flush the census and capture every authoritative subsystem at one
    /// quiescent save barrier. The runtime remains usable after capture.
    pub fn checkpoint_state(&mut self) -> Result<NativeRuntimeCheckpointState, RuntimeError> {
        if !matches!(self.state, RuntimeState::Running) {
            return Err(RuntimeError::CheckpointUnavailable);
        }

        let previous_commit = self.latest.territory_snapshot.commit_sequence;
        let territory_snapshot = self.territory.flush_census(self.config.census_flush_chunk);
        if let Some(update) = self.territory.drain_render_update() {
            self.render_updates.push_back(update);
        }
        let committed = self
            .territory
            .committed_state()
            .ok_or(RuntimeError::InvalidCheckpoint(
                "save barrier did not produce a committed territory state",
            ))?;

        let mut latest = (*self.latest).clone();
        latest.territory_snapshot = territory_snapshot.clone();
        latest.pending_render_updates = self.render_updates.len();
        latest.counters.census.committed |= territory_snapshot.commit_sequence != previous_commit;
        latest.counters.census.territory_generation = territory_snapshot.generation;
        latest.counters.census.territory_commit_sequence = territory_snapshot.commit_sequence;
        self.latest = Arc::new(latest);

        Ok(NativeRuntimeCheckpointState {
            tick: self.tick,
            frame: self.frame,
            war_grace_end: self.war_grace_end,
            runtime_config: self.config,
            simulation_config: self.simulation.config(),
            units: self.simulation.units.clone(),
            territory_config: self.territory.checkpoint_config(),
            territory_committed_state: committed,
            influence_runtime: self.territory.influence_runtime_state(),
            strategic_cycle: self.strategic.cycle(),
            economies: self.strategic.economies().values().cloned().collect(),
            occupations: self.strategic.occupations().values().cloned().collect(),
            scenario: self.scenario.clone(),
            diplomacy: self.diplomacy.clone(),
            unit_policies: self.unit_policies.values().cloned().collect(),
            battlefield: self.battlefield.clone(),
            objectives: self.objectives.clone(),
            prior_objective_by_unit: self.prior_objective_by_unit.clone(),
            front_prior_by_unit: self.front_prior_by_unit.clone(),
            last_front_refresh_tick: self.last_front_refresh_tick,
            casualties: self.casualties.clone(),
            casualties_by_victim: self.casualties_by_victim.clone(),
            gameplay_rng: self.gameplay_rng.state(),
            personnel_reserves: self.personnel_reserves.clone(),
            side_dynamics: self.side_dynamics.clone(),
            operations: self.operations.clone(),
            naval_planning: self.naval_planning.clone(),
            operational_execution: self.operational_execution.clone(),
            air_power: self.air_power.clone(),
            reinforcement: self.reinforcement.clone(),
            material_logistics: self.material_logistics.clone(),
            strategic_missiles: self.strategic_missiles.clone(),
        })
    }

    fn stage_battlefield_tick(
        &self,
        next_tick: u64,
        next_frame: u64,
        territory: &TerritoryControl,
        hostile_controlled_land: &[f64],
        side_dynamics: Option<&BTreeMap<usize, SideDynamics>>,
    ) -> Result<Option<StagedBattlefieldTick>, RuntimeError> {
        let Some(state) = &self.battlefield else {
            return Ok(None);
        };
        let world = WorldGridView::new(
            territory.grid_resolution(),
            territory.width(),
            territory.height(),
            territory.land(),
        )
        .map_err(BattlefieldError::from)?;
        let task_force_keys = self
            .operations
            .as_ref()
            .map(OperationalRuntimeState::task_force_key_by_unit)
            .unwrap_or_default();
        let local_inputs = self
            .simulation
            .units
            .iter()
            .map(|unit| {
                let sovereign = u16::try_from(unit.combat.sovereign)
                    .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?;
                let side = SideKey::try_from(unit.combat.side)
                    .map_err(|_| RuntimeError::InvalidCheckpoint("unit side exceeds SideKey"))?;
                let policy = self
                    .unit_policies
                    .get(&unit.combat.id)
                    .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
                Ok(BattlefieldLocalUnitInput {
                    id: unit.combat.id,
                    side,
                    sovereign,
                    kind: unit.combat.kind,
                    lat: unit.combat.lat,
                    lng: unit.combat.lng,
                    formation_strength: formation_strength(&unit.combat),
                    refuses_offense: policy
                        .influence
                        .as_ref()
                        .is_some_and(|influence| influence.refuses_offense),
                    previous_dir_lat: unit.dir_lat,
                    previous_dir_lng: unit.dir_lng,
                    task_force_key: task_force_keys.get(&unit.combat.id).copied(),
                })
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        let local = resolve_local_tactics(
            next_tick,
            state,
            &local_inputs,
            HostilityMatrix::new(Some(&self.diplomacy.hostility), territory.max_sides()),
        )?;
        let local_by_id = local
            .units
            .iter()
            .copied()
            .map(|unit| (unit.unit_id, unit))
            .collect::<BTreeMap<_, _>>();

        let mut ally_weight_by_id = BTreeMap::new();
        let battlefield_inputs = self
            .simulation
            .units
            .iter()
            .map(|unit| {
                let sovereign = u16::try_from(unit.combat.sovereign)
                    .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?;
                let side = SideKey::try_from(unit.combat.side)
                    .map_err(|_| RuntimeError::InvalidCheckpoint("unit side exceeds SideKey"))?;
                let memory =
                    state
                        .units
                        .get(&unit.combat.id)
                        .ok_or(RuntimeError::InvalidCheckpoint(
                            "battlefield state does not cover a live unit",
                        ))?;
                let mut country =
                    *state
                        .countries
                        .get(&sovereign)
                        .ok_or(RuntimeError::InvalidCheckpoint(
                            "battlefield country state is missing",
                        ))?;
                if let Some(dynamics) =
                    side_dynamics.and_then(|dynamics| dynamics.get(&usize::from(side)))
                {
                    country.war_phase = match dynamics.phase {
                        WarPhase::Advancing => crate::battlefield::BattlefieldWarPhase::Advancing,
                        WarPhase::Collapsing => crate::battlefield::BattlefieldWarPhase::Collapsing,
                        WarPhase::Stalemate | WarPhase::Retreating => {
                            crate::battlefield::BattlefieldWarPhase::Stable
                        }
                    };
                    if dynamics.posture == WarPosture::Defensive {
                        let manpower_ratio = if dynamics.initial_personnel > 0.0 {
                            (dynamics.current_personnel / dynamics.initial_personnel).max(0.0)
                        } else {
                            0.0
                        };
                        let defensive_scale = if manpower_ratio < 0.25 {
                            0.6
                        } else if manpower_ratio < 0.5 {
                            0.8
                        } else {
                            1.0
                        };
                        country.ai_speed_multiplier =
                            country.ai_speed_multiplier.min(0.96 * defensive_scale);
                    }
                }
                if let Some(capital_cell) = self
                    .scenario
                    .countries
                    .iter()
                    .find(|country| country.country_id == sovereign)
                    .and_then(|country| country.capital_cell)
                {
                    let controller_side = *territory.dominant_side().get(capital_cell).ok_or(
                        RuntimeError::InvalidCheckpoint(
                            "country capital is outside the territory grid",
                        ),
                    )?;
                    country.capital_lost = controller_side >= 0
                        && controller_side as usize != usize::from(side)
                        && self.diplomacy.hostility
                            [usize::from(side) * territory.max_sides() + controller_side as usize]
                            == 1;
                }
                let strength = formation_strength(&unit.combat);
                let ally_multiplier = match country.influence_buff {
                    crate::battlefield::BattlefieldBuff::Super => 200.0,
                    crate::battlefield::BattlefieldBuff::Buff => 50.0,
                    _ => 1.0,
                };
                ally_weight_by_id.insert(unit.combat.id, strength * ally_multiplier);
                Ok(BattlefieldUnitInput {
                    id: unit.combat.id,
                    side,
                    sovereign,
                    kind: unit.combat.kind,
                    transport: unit.combat.transport,
                    lat: unit.combat.lat,
                    lng: unit.combat.lng,
                    formation_strength: strength,
                    counts_for_capitulation: unit.combat.health > 0.0
                        && (unit.combat.kind == UnitKind::Army || unit.combat.equipment > 0),
                    // Influence is resolved before this tick's support prepass in the browser.
                    armor_supported: unit.combat.armor_supported,
                    is_alpenjager: memory.is_alpenjager,
                    victory_boost_ticks: unit.combat.victory_boost_ticks,
                    encircled_ticks: memory.encircled_ticks,
                    last_combat_frame: (unit.combat.last_combat_tick != 0)
                        .then_some(unit.combat.last_combat_tick),
                    last_ally_count: memory.last_ally_count,
                    near_sovereign_city: state.near_sovereign_city(
                        sovereign,
                        unit.combat.lat,
                        unit.combat.lng,
                    ),
                    country,
                })
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        let resolved = resolve_battlefield_tick(
            state.config,
            BattlefieldTickInput {
                tick: next_tick,
                frame: next_frame,
                mountains_enabled: state.mountains_enabled,
                maps: BattlefieldMapView {
                    world,
                    terrain_intensity: Some(&state.terrain_intensity),
                    urban_cells: self.battlefield_urban_mask.as_deref(),
                    world_control: territory.world_control(),
                    de_jure: territory.de_jure(),
                    dominant_side: territory.dominant_side(),
                    occupation: territory.occupation(),
                    side_influence: territory.all_side_influence(),
                    country_to_side: territory.country_to_side(),
                    hostility: HostilityMatrix::new(
                        Some(&self.diplomacy.hostility),
                        territory.max_sides(),
                    ),
                },
                hostile_controlled_land_by_side: hostile_controlled_land,
                units: &battlefield_inputs,
            },
        )?;

        let mut policies = self.unit_policies.clone();
        let mut next_unit_state = state.units.clone();
        let mut influence_eligible = BTreeSet::new();
        let live_by_id = self
            .simulation
            .units
            .iter()
            .map(|unit| (unit.combat.id, unit))
            .collect::<BTreeMap<_, _>>();
        let resolved_by_id = resolved
            .units
            .into_iter()
            .map(|result| {
                let policy = policies
                    .get_mut(&result.unit_id)
                    .ok_or(RuntimeError::InvalidUnitPolicy(result.unit_id))?;
                let live =
                    live_by_id
                        .get(&result.unit_id)
                        .ok_or(RuntimeError::InvalidCheckpoint(
                            "battlefield result references a missing unit",
                        ))?;
                policy.ai.base_speed = result.base_speed;
                policy.ai.movement = result.movement;
                policy.ai.combat = result.combat;
                policy.ai.encircled = result.encircled;
                policy.ai.reinforcement_eligible =
                    live.combat.health / live.combat.max_health.max(1.0) < 0.45;
                if let Some(influence) = &mut policy.influence {
                    influence.radius = result.influence.radius;
                    influence.delta = result.influence.delta;
                    influence.concentration_bonus = result.influence.concentration_bonus;
                    if result.influence.eligible {
                        influence_eligible.insert(result.unit_id);
                    }
                }
                let memory = next_unit_state.get_mut(&result.unit_id).ok_or(
                    RuntimeError::InvalidCheckpoint("battlefield result has no persistent state"),
                )?;
                if !unit_deploying(policy, next_tick) {
                    memory.encircled_ticks = result.encircled_ticks;
                    if result.attrition.supply_collapse_sampled {
                        memory.supply_collapsed_tick =
                            result.attrition.supply_collapsed.then_some(next_tick);
                    }
                }
                let local =
                    local_by_id
                        .get(&result.unit_id)
                        .ok_or(RuntimeError::InvalidCheckpoint(
                            "battlefield tactics omitted a live unit",
                        ))?;
                memory.armor_support_last_tick = local.armor_support_last_tick;
                memory.last_ally_count = local.last_ally_count;
                Ok((result.unit_id, result))
            })
            .collect::<Result<BTreeMap<_, _>, RuntimeError>>()?;

        Ok(Some(StagedBattlefieldTick {
            policies,
            next_unit_state,
            resolved_by_id,
            local,
            local_by_id,
            ally_weight_by_id,
            influence_eligible,
        }))
    }

    fn stage_command_policies(
        &self,
        snapshot: &StrategicSnapshot,
        simulation: &Simulation,
    ) -> Result<StagedCommandPolicies, RuntimeError> {
        let country_bands = snapshot
            .countries
            .iter()
            .map(|country| (country.country_id, country.economy.command_band))
            .collect::<BTreeMap<_, _>>();
        let resolved = resolve_command_policies_for(
            simulation,
            &self.unit_policies,
            &country_bands,
            &self.scenario,
            &self.territory,
        )?;
        let changed_unit_ids = resolved
            .iter()
            .filter_map(|(&unit_id, resolved)| {
                self.unit_policies
                    .get(&unit_id)
                    .filter(|policy| policy.command.band != resolved.band)
                    .map(|_| unit_id)
            })
            .collect();
        Ok(StagedCommandPolicies {
            cycle: snapshot.cycle,
            policies: resolved,
            changed_unit_ids,
        })
    }

    fn stage_strategic_effects(
        &self,
        prepared: &mut PreparedStrategicCycle,
        casualties_by_victim: &BTreeMap<u16, BTreeMap<u16, f64>>,
    ) -> Result<StagedStrategicEffects, RuntimeError> {
        let evaluated = prepared.snapshot();
        if evaluated.surrenders.len() > 1 {
            return Err(RuntimeError::InvalidCheckpoint(
                "strategic cycle produced more than one capitulation",
            ));
        }
        let capitulated = evaluated
            .surrenders
            .iter()
            .map(|command| command.country_id)
            .collect::<Vec<_>>();
        let desertion_personnel_loss_by_side =
            desertion_personnel_loss_by_side(&self.simulation, &evaluated.desertions);
        let mut staged_simulation = if evaluated.desertions.is_empty() && capitulated.is_empty() {
            None
        } else {
            Some(Simulation::new(
                self.simulation.config(),
                self.simulation.units.clone(),
            )?)
        };
        let removed_ids = if let Some(simulation) = &mut staged_simulation {
            simulation
                .apply_strategic_unit_consequences_atomic(&evaluated.desertions, &capitulated)?
                .removed_ids
                .to_vec()
        } else {
            Vec::new()
        };

        let mut staged_territory = None;
        if let Some(command) = evaluated.surrenders.first() {
            let victim_side = usize::from(command.side);
            if self.territory.country_to_side().get(&command.country_id) != Some(&victim_side) {
                return Err(RuntimeError::InvalidCheckpoint(
                    "capitulating country does not belong to its strategic side",
                ));
            }
            let hostile_attacker_ids = self
                .territory
                .country_to_side()
                .iter()
                .filter_map(|(&country_id, &side)| {
                    let hostile = side != victim_side
                        && self.diplomacy.hostility
                            [victim_side * self.territory.max_sides() + side]
                            == 1;
                    let side_is_active = self
                        .diplomacy
                        .active_sides
                        .contains(&u16::try_from(side).ok()?);
                    let active = prepared
                        .economy(country_id)
                        .is_some_and(|economy| !economy.capitulated);
                    (hostile && side_is_active && active).then_some(country_id)
                })
                .collect::<Vec<_>>();
            let simulation = staged_simulation.as_ref().unwrap_or(&self.simulation);
            let unit_positions = simulation
                .units
                .iter()
                .filter_map(|unit| {
                    u16::try_from(unit.combat.sovereign).ok().map(|country_id| {
                        SurrenderUnitPosition {
                            country_id,
                            lat: unit.combat.lat,
                            lng: unit.combat.lng,
                            health: unit.combat.health,
                        }
                    })
                })
                .collect::<Vec<_>>();
            let plan = plan_surrender_allocation(SurrenderAllocationInput {
                victim_country_id: command.country_id,
                hostile_attacker_ids: &hostile_attacker_ids,
                casualties_by_victim,
                width: self.territory.width(),
                height: self.territory.height(),
                grid_resolution: self.territory.grid_resolution(),
                land: self.territory.land(),
                world_control: self.territory.world_control(),
                de_jure: self.territory.de_jure(),
                primary_occupier: self.territory.primary_occupier(),
                units: &unit_positions,
            })?;

            let mut config = self.territory.checkpoint_config();
            let mut controller_transfer_cells = Vec::new();
            for transfer in &plan.transfers {
                if config.maps.world_control.get(transfer.cell) != Some(&transfer.original_owner)
                    || transfer.original_owner != command.country_id
                {
                    return Err(RuntimeError::InvalidCheckpoint(
                        "surrender transfer does not match live territory",
                    ));
                }
                let recipient_side = *config.country_to_side.get(&transfer.new_owner).ok_or(
                    RuntimeError::InvalidCheckpoint("surrender recipient has no declared side"),
                )?;
                config.maps.world_control[transfer.cell] = transfer.new_owner;
                config.maps.primary_occupier[transfer.cell] = transfer.new_owner;
                config.maps.land[transfer.cell] = 2;
                for influence in &mut config.maps.side_influence {
                    influence[transfer.cell] = 0.0;
                }
                config.maps.side_influence[recipient_side][transfer.cell] = 1.0;
                let recipient_controller = i16::try_from(recipient_side)
                    .map_err(|_| RuntimeError::InvalidCheckpoint("side index exceeds i16"))?;
                if config.maps.dominant_side[transfer.cell] != recipient_controller {
                    controller_transfer_cells.push(transfer.cell);
                }
                config.maps.dominant_side[transfer.cell] = recipient_controller;
                config.maps.occupation[transfer.cell] =
                    if recipient_side % 2 == 0 { 1.0 } else { -1.0 };
            }
            config.world_revision = config
                .world_revision
                .checked_add(1)
                .ok_or(RuntimeError::ClockOverflow)?;

            let prior = self
                .territory
                .committed_state()
                .ok_or(RuntimeError::InvalidCheckpoint(
                    "strategic effects require a committed territory boundary",
                ))?;
            let committed = TerritoryCommittedState {
                generation: prior
                    .generation
                    .checked_add(1)
                    .ok_or(RuntimeError::ClockOverflow)?,
                commit_sequence: prior
                    .commit_sequence
                    .checked_add(1)
                    .ok_or(RuntimeError::ClockOverflow)?,
                mutation_sequence: prior
                    .mutation_sequence
                    .checked_add(1)
                    .ok_or(RuntimeError::ClockOverflow)?,
                processed_tiles: config.width.div_ceil(config.tile_size)
                    * config.height.div_ceil(config.tile_size),
                processed_items: config.maps.land.len() + config.cities.len(),
            };
            let mut restored = TerritoryControl::restore(config, committed)?;
            if let Some(influence_runtime) = self.territory.influence_runtime_state() {
                restored.restore_influence_runtime(influence_runtime)?;
                for cell in controller_transfer_cells {
                    restored.queue_influence_runtime_cell(cell, true)?;
                }
            }
            staged_territory = Some(restored);

            let country = self
                .scenario
                .countries
                .iter()
                .find(|country| country.country_id == command.country_id)
                .ok_or(RuntimeError::InvalidCheckpoint(
                    "capitulating country is absent from scenario production",
                ))?;
            let economy =
                prepared
                    .economy(command.country_id)
                    .ok_or(RuntimeError::InvalidCheckpoint(
                        "capitulating country has no prepared economy",
                    ))?;
            let expected_army_units = country.expected_army_units.max(3.0);
            prepared.register_occupation(OccupationState {
                victim_id: command.country_id,
                annexer_id: plan.primary_annexer_id,
                base_income: economy.base_income,
                core_cells: country.initial_core_cells.max(1),
                expected_army_units,
                resistance: 0.0,
                occupation_coverage: 1.0,
                garrison_coverage: 0.0,
                garrison_assigned: 0.0,
                required_garrison: required_garrison(expected_army_units)
                    .map_err(StrategicError::from)?,
                held_ratio: 1.0,
                active_rebellion: false,
                queued_at_cycle: 0,
                cooldown_until_cycle: 0,
            })?;
        }

        if !evaluated.surrenders.is_empty() {
            let allowed = self
                .diplomacy
                .active_sides
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            let post_capitulation_sides = evaluated
                .countries
                .iter()
                .filter(|country| allowed.contains(&country.side) && !country.economy.capitulated)
                .map(|country| country.side)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let hostile_pairs = canonical_hostile_pairs(
                &post_capitulation_sides,
                &self.diplomacy.hostility,
                self.territory.max_sides(),
            );
            if let Some(resolution) =
                evaluate_global_conflict(&post_capitulation_sides, &hostile_pairs)
            {
                prepared.register_conflict_resolution(resolution);
            }
        }

        let finalized = prepared.snapshot();
        let state = finalized
            .conflict_resolution
            .map_or(RuntimeState::Running, |resolution| {
                RuntimeState::ConflictResolved {
                    cycle: finalized.cycle,
                    tick: finalized.tick,
                    resolution: resolution.into(),
                }
            });
        Ok(StagedStrategicEffects {
            simulation: staged_simulation,
            territory: staged_territory,
            removed_ids,
            state,
            fronts_invalidated: !evaluated.surrenders.is_empty(),
            desertion_personnel_loss_by_side,
        })
    }

    /// Advance one owned tick and frame. Publication is the final operation.
    pub fn step(&mut self) -> Result<Arc<RuntimeSnapshot>, RuntimeError> {
        match self.state {
            RuntimeState::Running => {}
            RuntimeState::AwaitingStrategicEffects { cycle, tick, .. } => {
                return Err(RuntimeError::AwaitingStrategicEffects { cycle, tick });
            }
            RuntimeState::ConflictResolved { cycle, tick, .. } => {
                return Err(RuntimeError::ConflictResolved { cycle, tick });
            }
            RuntimeState::Poisoned => return Err(RuntimeError::Poisoned),
        }

        let next_tick = self
            .tick
            .checked_add(1)
            .ok_or(RuntimeError::ClockOverflow)?;
        let next_frame = self
            .frame
            .checked_add(1)
            .ok_or(RuntimeError::ClockOverflow)?;
        let mut staged_operations = self.operations.clone();
        if let Some(operations) = &mut staged_operations {
            operations.pre_tick(next_tick);
        }
        let mut staged_naval_planning = self.naval_planning.clone();
        let mut staged_operational_execution = self.operational_execution.clone();
        let mut staged_air_power = self.air_power.clone();
        let mut staged_reinforcement = self.reinforcement.clone();
        let mut staged_material_logistics = self.material_logistics.clone();
        let mut staged_strategic_missiles = self.strategic_missiles.clone();
        let mut staged_strategic = self.strategic.clone();
        let mut staged_gameplay_rng = self.gameplay_rng;
        let mut staged_personnel_reserves = self.personnel_reserves.clone();
        let air_wing_sovereign_by_id = staged_air_power
            .as_ref()
            .map(|air_power| {
                air_power
                    .wings
                    .iter()
                    .map(|wing| (wing.id, wing.sovereign_country_id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut staged_side_dynamics = stage_side_dynamics(
            &self.side_dynamics,
            next_tick,
            self.frame,
            &self.latest.territory_snapshot,
            self.territory.country_to_side(),
            &self.diplomacy.active_sides,
            &self.diplomacy.hostility,
            self.territory.max_sides(),
            &self.simulation,
            &self.unit_policies,
            staged_operations.as_ref(),
        )?;
        // Browser influence runs before the current tick resolves controller-dependent policy,
        // fronts, AI, movement, and combat. Build influence from the pre-tick unit/map snapshot,
        // apply it through a sparse undo transaction, then resolve the rest of the tick against
        // that staged map. A fallible pre-simulation path rolls the transaction back; dropping it
        // after a successful simulation commits it without cloning the full world.
        let hostile_controlled_land = self.battlefield.as_ref().map_or_else(Vec::new, |_| {
            hostile_controlled_land_by_side(
                &self.latest.territory_snapshot,
                self.territory.country_to_side(),
                &self.diplomacy.hostility,
                self.territory.max_sides(),
            )
        });
        let pre_influence_battlefield = self.stage_battlefield_tick(
            next_tick,
            next_frame,
            &self.territory,
            &hostile_controlled_land,
            staged_side_dynamics.as_ref(),
        )?;
        let mut staged_influence = None;
        // Browser influence caches contain only non-empty coalition rows. The
        // territory topology deliberately retains capitulated countries for
        // attribution, so it cannot define the live diffusion rows or budgets.
        let influence_sides = self
            .diplomacy
            .active_sides
            .iter()
            .copied()
            .map(usize::from)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let dynamic_influence = self.territory.has_influence_runtime();
        let mut staged_battlefield = if let Some(pre_stage) = pre_influence_battlefield {
            let (sources, schedule) = if dynamic_influence {
                let (sources, schedule) = browser_influence_sources(
                    &self.simulation.units,
                    &pre_stage.policies,
                    next_tick,
                    self.frame,
                    influence_sides.len(),
                )?;
                (sources, Some(schedule))
            } else {
                let before = self.simulation.initial_snapshot(self.tick, self.frame);
                (
                    influence_sources(
                        &before,
                        &pre_stage.policies,
                        next_tick,
                        Some(&pre_stage.influence_eligible),
                    )?,
                    None,
                )
            };
            let (transaction, diffusion_budget, diffusion) = if dynamic_influence {
                let (_, diffusion_budget) = browser_influence_budgets(influence_sides.len());
                let (transaction, diffusion) = self.territory.apply_influence_runtime_staged(
                    &sources,
                    &influence_sides,
                    diffusion_budget,
                )?;
                (transaction, diffusion_budget, diffusion)
            } else {
                (
                    self.territory.apply_influence_sources_staged(&sources)?,
                    0,
                    DiffusionQueueResult::default(),
                )
            };
            staged_influence = Some(StagedInfluenceTick {
                transaction,
                source_count: sources.len(),
                schedule,
                diffusion_budget,
                diffusion,
            });
            match self.stage_battlefield_tick(
                next_tick,
                next_frame,
                &self.territory,
                &hostile_controlled_land,
                staged_side_dynamics.as_ref(),
            ) {
                Ok(Some(stage)) => Some(stage),
                Ok(None) => {
                    let staged = staged_influence
                        .take()
                        .expect("live battlefield staging created a transaction");
                    self.territory
                        .rollback_influence_transaction(staged.transaction);
                    return Err(RuntimeError::InvalidCheckpoint(
                        "live battlefield state disappeared during staging",
                    ));
                }
                Err(error) => {
                    let staged = staged_influence
                        .take()
                        .expect("live battlefield staging created a transaction");
                    self.territory
                        .rollback_influence_transaction(staged.transaction);
                    return Err(error);
                }
            }
        } else if dynamic_influence {
            let (sources, schedule) = browser_influence_sources(
                &self.simulation.units,
                &self.unit_policies,
                next_tick,
                self.frame,
                influence_sides.len(),
            )?;
            let (_, diffusion_budget) = browser_influence_budgets(influence_sides.len());
            let (transaction, diffusion) = self.territory.apply_influence_runtime_staged(
                &sources,
                &influence_sides,
                diffusion_budget,
            )?;
            staged_influence = Some(StagedInfluenceTick {
                transaction,
                source_count: sources.len(),
                schedule: Some(schedule),
                diffusion_budget,
                diffusion,
            });
            None
        } else {
            None
        };

        // Attrition mutates units before AI/combat. Retain one O(units) image so
        // every later planning or simulation failure rolls back both subsystems.
        let simulation_backup = self.simulation.units.clone();
        let simulation_config = self.simulation.config();

        macro_rules! rollback_try {
            ($expression:expr) => {
                match $expression {
                    Ok(value) => value,
                    Err(error) => {
                        self.simulation =
                            Simulation::new(simulation_config, simulation_backup.clone())
                                .expect("validated simulation rollback must reconstruct");
                        if let Some(staged) = staged_influence.take() {
                            self.territory
                                .rollback_influence_transaction(staged.transaction);
                        }
                        return Err(error.into());
                    }
                }
            };
        }

        let (air_outcome, air_damage_outcome) = if let Some(air_power) = &mut staged_air_power {
            let targets = rollback_try!(air_unit_targets(&self.simulation));
            let priorities = air_priority_areas(staged_operations.as_ref());
            let controllers = if airfield_controller_update_due(next_tick) {
                rollback_try!(airfield_controllers(air_power, &self.territory))
            } else {
                BTreeMap::new()
            };
            let (command_bands, coverage) = air_country_policy(&staged_strategic, air_power);
            let outcome = rollback_try!(air_power.advance(AirWorldInput {
                tick: next_tick,
                side_count: self.territory.max_sides(),
                hostility: &self.diplomacy.hostility,
                command_bands: &command_bands,
                air_operations_coverage: &coverage,
                targets: &targets,
                priority_areas: &priorities,
                airfield_controllers: &controllers,
            }));
            let commands = air_damage_commands(&outcome);
            let damage = if commands.is_empty() {
                None
            } else {
                Some(rollback_try!(self.simulation.apply_batch_damage(&commands)))
            };
            (Some(outcome), damage)
        } else {
            (None, None)
        };

        // Browser unit processing is reverse stable-array order. Eligible exiled naval
        // formations consume exactly one shared gameplay draw apiece; hits recover the
        // surviving crew/manpower and disappear before sea attrition, AI, or combat.
        let controlled_by_country = self
            .latest
            .territory_snapshot
            .countries
            .iter()
            .map(|country| (country.country_id, country.controlled))
            .collect::<BTreeMap<_, _>>();
        let exile_policies = staged_battlefield
            .as_ref()
            .map_or(&self.unit_policies, |stage| &stage.policies);
        let mut exiled_ids = Vec::new();
        let mut recovered_personnel = 0_u64;
        for unit in self.simulation.units.iter().rev() {
            let Ok(country) = u16::try_from(unit.combat.sovereign) else {
                continue;
            };
            let Some(&controlled) = controlled_by_country.get(&country) else {
                continue;
            };
            let policy = rollback_try!(
                exile_policies
                    .get(&unit.combat.id)
                    .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))
            );
            if unit_deploying(policy, next_tick) {
                continue;
            }
            let at_sea = staged_battlefield
                .as_ref()
                .and_then(|stage| stage.resolved_unit(unit.combat.id))
                .map_or(unit.combat.at_sea, |resolved| resolved.cell.at_sea);
            if !at_sea || controlled != 0 || staged_gameplay_rng.next_f64() >= 0.02 {
                continue;
            }
            let side =
                rollback_try!(usize::try_from(unit.combat.side).map_err(|_| {
                    RuntimeError::InvalidCheckpoint("unit side exceeds platform")
                }));
            let recovered = if unit.combat.kind == UnitKind::Armor {
                unit.combat
                    .equipment
                    .saturating_mul(simulation_config.combat.armor_crew_per_vehicle)
            } else {
                unit.combat.personnel
            };
            let reserve = rollback_try!(staged_personnel_reserves.get_mut(&side).ok_or(
                RuntimeError::InvalidCheckpoint("personnel reserve is missing a unit side"),
            ));
            *reserve += recovered as f64;
            if !reserve.is_finite() {
                rollback_try!(Err::<(), RuntimeError>(RuntimeError::InvalidCheckpoint(
                    "personnel reserve overflowed"
                )));
            }
            recovered_personnel = recovered_personnel.saturating_add(recovered);
            exiled_ids.push(unit.combat.id);
        }
        exiled_ids.sort_unstable();
        let exile_outcome = if exiled_ids.is_empty() {
            None
        } else {
            Some(rollback_try!(
                self.simulation.remove_units_atomic(&exiled_ids)
            ))
        };

        let attrition_commands = if let Some(stage) = &staged_battlefield {
            self.simulation
                .units
                .iter()
                .filter_map(|unit| {
                    let policy = stage.policies.get(&unit.combat.id)?;
                    let result = stage.resolved_unit(unit.combat.id)?;
                    (!unit_deploying(policy, next_tick) && result.attrition.damage > 0.0).then_some(
                        DamageCommand {
                            unit_id: unit.combat.id,
                            damage: result.attrition.damage,
                        },
                    )
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let attrition_outcome = if attrition_commands.is_empty() {
            None
        } else {
            Some(rollback_try!(
                self.simulation.apply_batch_damage(&attrition_commands)
            ))
        };

        let planning_policies = staged_battlefield
            .as_ref()
            .map_or(&self.unit_policies, |stage| &stage.policies);
        let planning_operational_claimed = operational_claimed_unit_ids(
            staged_operations.as_ref(),
            staged_operational_execution.as_ref(),
        );
        // Checkpoints with no completed layout refresh once before planning. Exact native
        // checkpoints retain the prior layout and continue on the configured tick phase.
        let refresh_fronts = self.last_front_refresh_tick.is_none()
            || next_tick.is_multiple_of(self.config.front_refresh_ticks);
        let mut refreshed_layout = if refresh_fronts {
            let units = rollback_try!(front_layout_units(
                &self.simulation,
                planning_policies,
                &self.front_prior_by_unit,
                &planning_operational_claimed,
                next_tick,
            ));
            Some(rollback_try!(derive_front_layout(
                FrontLayoutInput {
                    grid_width: self.territory.width(),
                    grid_height: self.territory.height(),
                    grid_res: self.territory.grid_resolution(),
                    land_mask: self.territory.land(),
                    dominant_side_map: self.territory.dominant_side(),
                    hostility: HostilityMatrix::new(
                        Some(&self.diplomacy.hostility),
                        self.territory.max_sides(),
                    ),
                    units: &units,
                },
                &self.config.front,
            )))
        } else {
            None
        };
        let refreshed_objective_prior = refreshed_layout
            .as_ref()
            .map(|layout| front_objectives_by_unit(&layout.next_prior));
        let mut planning_prior =
            refreshed_objective_prior.unwrap_or_else(|| self.prior_objective_by_unit.clone());
        let planning_objectives = refreshed_layout
            .as_ref()
            .map_or(self.objectives.as_slice(), |layout| {
                layout.objectives.as_slice()
            });
        let planning_units = rollback_try!(ai_units(
            &self.simulation,
            planning_policies,
            &planning_prior,
            &planning_operational_claimed,
            next_tick,
            staged_battlefield.as_ref(),
            staged_side_dynamics.as_ref(),
        ));
        let mut planning = rollback_try!(resolve_ai_orders(
            self.config.ai,
            &planning_units,
            AiWorldInput {
                grid_width: self.territory.width(),
                grid_height: self.territory.height(),
                grid_res: self.territory.grid_resolution(),
                land_mask: self.territory.land(),
                dominant_side_map: self.territory.dominant_side(),
                hostility: HostilityMatrix::new(
                    Some(&self.diplomacy.hostility),
                    self.territory.max_sides(),
                ),
                frontline_latitude: None,
                frontline_longitude: None,
                objectives: planning_objectives,
            },
        ));
        let operational_inputs = if staged_operations.is_some() {
            Some(rollback_try!(operational_unit_inputs(
                &self.simulation,
                staged_battlefield.as_ref(),
            )))
        } else {
            None
        };
        if let (Some(operations), Some(inputs)) =
            (&mut staged_operations, operational_inputs.as_ref())
        {
            let observations = tactical_contact_observations(&planning, inputs, &self.simulation);
            operations.ingest_tactical_contacts(next_tick, &observations);
            let collapsing_sides = staged_side_dynamics
                .as_ref()
                .map(|dynamics| {
                    dynamics
                        .iter()
                        .filter_map(|(&side, dynamics)| {
                            (dynamics.phase == WarPhase::Collapsing).then_some(side)
                        })
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            operations.advance_task_forces(next_tick, inputs, &collapsing_sides);
            operations.evolve_overrides(next_tick, self.territory.country_to_side());
            let live_units = inputs
                .iter()
                .map(|unit| (unit.unit_id, unit.side_index))
                .collect::<BTreeMap<_, _>>();
            let countries = self
                .scenario
                .countries
                .iter()
                .map(|country| country.country_id)
                .collect::<BTreeSet<_>>();
            rollback_try!(operations.validate(
                self.territory.max_sides(),
                &live_units,
                &countries,
                next_tick,
            ));
        }
        let mut naval_planning_counters = NavalPlanningCounters::default();
        let execution_outcome = if let Some(execution) = &mut staged_operational_execution {
            let units = rollback_try!(execution_unit_inputs(
                &self.simulation,
                planning_policies,
                next_tick,
                staged_battlefield.as_ref(),
                &planning,
                staged_operations.as_ref(),
            ));
            if let Some(planner) = &mut staged_naval_planning {
                let topology = rollback_try!(self.naval_topology.as_deref().ok_or(
                    RuntimeError::InvalidCheckpoint("naval planning topology is missing"),
                ));
                let route_workspace = rollback_try!(self.naval_route_workspace.as_mut().ok_or(
                    RuntimeError::InvalidCheckpoint("naval route workspace is missing"),
                ));
                let world = rollback_try!(
                    WorldGridView::new(
                        self.territory.grid_resolution(),
                        self.territory.width(),
                        self.territory.height(),
                        self.territory.land(),
                    )
                    .map_err(SimulationError::from)
                );
                let originated = rollback_try!(planner.advance(
                    NavalPlanningInput {
                        tick: next_tick,
                        units: &units,
                        operations: staged_operations.as_ref(),
                        execution,
                        topology,
                        world,
                        dominant_side_map: self.territory.dominant_side(),
                        hostility: &self.diplomacy.hostility,
                        side_count: self.territory.max_sides(),
                    },
                    route_workspace,
                ));
                naval_planning_counters = originated.counters;
                execution.naval_operations.extend(originated.created);
                execution
                    .naval_operations
                    .sort_by(|left, right| left.id.cmp(&right.id));
            }
            let threats = rollback_try!(defender_threats(
                execution,
                staged_operations.as_ref(),
                &units,
                &self.territory,
                &self.diplomacy.hostility,
                self.territory.max_sides(),
            ));
            let outcome = rollback_try!(execution.advance(next_tick, &units, &threats));
            Some(outcome)
        } else {
            None
        };

        let resolved_operational_claimed = operational_claimed_unit_ids(
            staged_operations.as_ref(),
            staged_operational_execution.as_ref(),
        );
        if resolved_operational_claimed != planning_operational_claimed {
            if refresh_fronts {
                let units = rollback_try!(front_layout_units(
                    &self.simulation,
                    planning_policies,
                    &self.front_prior_by_unit,
                    &resolved_operational_claimed,
                    next_tick,
                ));
                refreshed_layout = Some(rollback_try!(derive_front_layout(
                    FrontLayoutInput {
                        grid_width: self.territory.width(),
                        grid_height: self.territory.height(),
                        grid_res: self.territory.grid_resolution(),
                        land_mask: self.territory.land(),
                        dominant_side_map: self.territory.dominant_side(),
                        hostility: HostilityMatrix::new(
                            Some(&self.diplomacy.hostility),
                            self.territory.max_sides(),
                        ),
                        units: &units,
                    },
                    &self.config.front,
                )));
            }
            planning_prior = refreshed_layout
                .as_ref()
                .map(|layout| front_objectives_by_unit(&layout.next_prior))
                .unwrap_or_else(|| self.prior_objective_by_unit.clone());
            let objectives = refreshed_layout
                .as_ref()
                .map_or(self.objectives.as_slice(), |layout| {
                    layout.objectives.as_slice()
                });
            let units = rollback_try!(ai_units(
                &self.simulation,
                planning_policies,
                &planning_prior,
                &resolved_operational_claimed,
                next_tick,
                staged_battlefield.as_ref(),
                staged_side_dynamics.as_ref(),
            ));
            planning = rollback_try!(resolve_ai_orders(
                self.config.ai,
                &units,
                AiWorldInput {
                    grid_width: self.territory.width(),
                    grid_height: self.territory.height(),
                    grid_res: self.territory.grid_resolution(),
                    land_mask: self.territory.land(),
                    dominant_side_map: self.territory.dominant_side(),
                    hostility: HostilityMatrix::new(
                        Some(&self.diplomacy.hostility),
                        self.territory.max_sides(),
                    ),
                    frontline_latitude: None,
                    frontline_longitude: None,
                    objectives,
                },
            ));
        }

        let mut operational_plan_unit_ids = if let (Some(operations), Some(inputs)) =
            (staged_operations.as_ref(), operational_inputs.as_ref())
        {
            rollback_try!(apply_operational_steering(
                &mut planning,
                operations,
                inputs,
            ))
        } else {
            BTreeSet::new()
        };
        if let Some(outcome) = &execution_outcome {
            operational_plan_unit_ids.extend(rollback_try!(apply_execution_outputs(
                &mut self.simulation,
                &mut planning,
                outcome,
            )));
        }
        planning.orders.extend(rollback_try!(garrison_hold_orders(
            &self.simulation,
            planning_policies,
            next_tick,
        )));
        let (mut command_orders, mut command_assignments) = rollback_try!(command_orders(
            &self.simulation,
            planning_policies,
            next_tick,
        ));
        let engaged_or_retreating = planning
            .assignments
            .iter()
            .filter(|assignment| {
                matches!(
                    assignment.reason,
                    AssignmentReason::Contact | AssignmentReason::Retreat
                )
            })
            .map(|assignment| assignment.unit_id)
            .collect::<BTreeSet<_>>();
        command_orders.retain(|order| !engaged_or_retreating.contains(&order.unit_id));
        command_assignments
            .retain(|assignment| !engaged_or_retreating.contains(&assignment.unit_id));
        let command_override_ids = command_orders
            .iter()
            .map(|order| order.unit_id)
            .collect::<BTreeSet<_>>();
        for assignment in planning
            .assignments
            .iter()
            .filter(|assignment| command_override_ids.contains(&assignment.unit_id))
        {
            match assignment.reason {
                AssignmentReason::Front => {
                    let was_sticky = assignment.objective_id.is_some_and(|objective| {
                        planning_prior.get(&assignment.unit_id) == Some(&objective)
                    });
                    if was_sticky {
                        planning.counters.sticky_assignments =
                            planning.counters.sticky_assignments.saturating_sub(1);
                    } else {
                        planning.counters.front_assignments =
                            planning.counters.front_assignments.saturating_sub(1);
                    }
                }
                AssignmentReason::Reinforce => {
                    planning.counters.reinforcement_assignments = planning
                        .counters
                        .reinforcement_assignments
                        .saturating_sub(1);
                }
                AssignmentReason::Field => {
                    planning.counters.field_orders =
                        planning.counters.field_orders.saturating_sub(1);
                }
                AssignmentReason::Hold => {
                    planning.counters.hold_orders = planning.counters.hold_orders.saturating_sub(1);
                }
                AssignmentReason::Contact | AssignmentReason::Retreat => {}
            }
        }
        planning
            .orders
            .retain(|order| !command_override_ids.contains(&order.unit_id));
        planning
            .assignments
            .retain(|assignment| !command_override_ids.contains(&assignment.unit_id));
        planning.counters.hold_orders += command_orders.len();
        planning.orders.extend(command_orders);
        planning.assignments.extend(command_assignments);
        planning.orders.sort_unstable_by_key(|order| order.unit_id);
        planning
            .assignments
            .sort_unstable_by_key(|assignment| assignment.unit_id);
        if let Some(stage) = &staged_battlefield {
            let reasons = planning
                .assignments
                .iter()
                .map(|assignment| (assignment.unit_id, assignment.reason))
                .collect::<BTreeMap<_, _>>();
            let direction_inputs = planning
                .orders
                .iter()
                .map(|order| {
                    let reason = reasons
                        .get(&order.unit_id)
                        .copied()
                        .unwrap_or(AssignmentReason::Hold);
                    let policy = planning_policies
                        .get(&order.unit_id)
                        .ok_or(RuntimeError::InvalidUnitPolicy(order.unit_id))?;
                    let at_sea = stage
                        .resolved_unit(order.unit_id)
                        .ok_or(RuntimeError::InvalidCheckpoint(
                            "battlefield result omitted an ordered unit",
                        ))?
                        .cell
                        .at_sea;
                    Ok(BattlefieldDirectionInput {
                        unit_id: order.unit_id,
                        dir_lat: order.dir_lat,
                        dir_lng: order.dir_lng,
                        is_plan_unit: operational_plan_unit_ids.contains(&order.unit_id)
                            || matches!(
                                reason,
                                AssignmentReason::Front | AssignmentReason::Reinforce
                            ),
                        at_sea,
                        active_retreat: reason == AssignmentReason::Retreat,
                        occupation_garrison_holding: policy.ai.garrison_excluded
                            || command_override_ids.contains(&order.unit_id),
                    })
                })
                .collect::<Result<Vec<_>, RuntimeError>>();
            let direction_inputs = rollback_try!(direction_inputs);
            let directions = rollback_try!(apply_cohesion_and_repulsion(
                self.battlefield
                    .as_ref()
                    .expect("staged battlefield has runtime state")
                    .config,
                &stage.local,
                &direction_inputs,
            ));
            let by_id = directions
                .into_iter()
                .map(|direction| (direction.unit_id, direction))
                .collect::<BTreeMap<_, _>>();
            for order in &mut planning.orders {
                let direction = rollback_try!(by_id.get(&order.unit_id).ok_or(
                    RuntimeError::InvalidCheckpoint("battlefield direction omitted an order"),
                ));
                order.dir_lat = direction.dir_lat;
                order.dir_lng = direction.dir_lng;
            }
        }

        let simulation_world = rollback_try!(
            WorldGridView::new(
                self.territory.grid_resolution(),
                self.territory.width(),
                self.territory.height(),
                self.territory.land(),
            )
            .map_err(SimulationError::from)
        );

        let staged_simulation_updates = if let Some(stage) = &staged_battlefield {
            let updates = self
                .simulation
                .units
                .iter()
                .map(|unit| {
                    let resolved = stage.resolved_unit(unit.combat.id).ok_or(
                        RuntimeError::InvalidCheckpoint("battlefield result omitted a live unit"),
                    )?;
                    let local =
                        stage
                            .local_unit(unit.combat.id)
                            .ok_or(RuntimeError::InvalidCheckpoint(
                                "battlefield tactics omitted a live unit",
                            ))?;
                    let ally_weight = *stage.ally_weight_by_id.get(&unit.combat.id).ok_or(
                        RuntimeError::InvalidCheckpoint(
                            "battlefield ally weight omitted a live unit",
                        ),
                    )?;
                    Ok((
                        unit.combat.id,
                        (resolved.cell.at_sea, local.armor_supported, ally_weight),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, RuntimeError>>();
            Some(rollback_try!(updates))
        } else {
            None
        };

        if let Some(updates) = staged_simulation_updates {
            for unit in &mut self.simulation.units {
                let (at_sea, armor_supported, ally_weight) = updates
                    .get(&unit.combat.id)
                    .copied()
                    .expect("battlefield updates cover every surviving unit");
                unit.combat.at_sea = at_sea;
                unit.combat.armor_supported = armor_supported;
                let policy = planning_policies
                    .get(&unit.combat.id)
                    .expect("planning policy covers every surviving unit");
                if !unit_deploying(policy, next_tick) {
                    unit.combat.victory_boost_ticks =
                        unit.combat.victory_boost_ticks.saturating_sub(1);
                }
                unit.ally_weight = ally_weight;
            }
        }
        let mut inactive_unit_ids = self
            .simulation
            .units
            .iter()
            .filter_map(|unit| {
                planning_policies
                    .get(&unit.combat.id)
                    .filter(|policy| unit_deploying(policy, next_tick))
                    .map(|_| unit.combat.id)
            })
            .collect::<Vec<_>>();
        inactive_unit_ids.sort_unstable();
        let simulation_result = self.simulation.step(TickInput {
            tick: next_tick,
            frame: next_frame,
            war_grace_end: self.war_grace_end,
            world: simulation_world,
            hostility: HostilityMatrix::new(
                Some(&self.diplomacy.hostility),
                self.territory.max_sides(),
            ),
            orders: &planning.orders,
            inactive_unit_ids: &inactive_unit_ids,
        });
        let (mut frame_snapshot, mut simulation_counters) = match simulation_result {
            Ok(result) => result,
            Err(error) => {
                self.simulation = Simulation::new(simulation_config, simulation_backup.clone())
                    .expect("validated simulation rollback must reconstruct");
                if let Some(staged) = staged_influence.take() {
                    self.territory
                        .rollback_influence_transaction(staged.transaction);
                }
                return Err(RuntimeError::Simulation(error));
            }
        };
        // Browser missiles advance after land movement/combat. Stage their transient state and
        // shared-RNG draws, then apply each impact in reverse missile-array order. Rebuilding only
        // the unit slice keeps the land combat event publication while reflecting impact damage.
        let (missile_outcome, missile_damage_outcomes) =
            if let Some(missiles) = &mut staged_strategic_missiles {
                let outcome = rollback_try!(missiles.advance(
                    &self.diplomacy.active_sides,
                    &self.diplomacy.hostility,
                    self.territory.max_sides(),
                    &self.simulation.units,
                    simulation_config.combat.combat_damage,
                    &mut staged_gameplay_rng,
                ));
                let mut damages = Vec::new();
                for impact in &outcome.impacts {
                    let live_ids = self
                        .simulation
                        .units
                        .iter()
                        .map(|unit| unit.combat.id)
                        .collect::<BTreeSet<_>>();
                    let commands = impact
                        .damage_commands
                        .iter()
                        .copied()
                        .filter(|command| live_ids.contains(&command.unit_id))
                        .collect::<Vec<_>>();
                    if !commands.is_empty() {
                        damages.push(rollback_try!(self.simulation.apply_batch_damage(&commands)));
                    }
                }
                if !damages.is_empty() {
                    frame_snapshot.units = self
                        .simulation
                        .initial_snapshot(next_tick, next_frame)
                        .units;
                }
                (Some(outcome), damages)
            } else {
                (None, Vec::new())
            };
        if air_damage_outcome.is_some()
            || exile_outcome.is_some()
            || attrition_outcome.is_some()
            || !missile_damage_outcomes.is_empty()
        {
            let mut removed_ids = frame_snapshot.removed_ids.to_vec();
            if let Some(outcome) = &air_damage_outcome {
                removed_ids.extend(outcome.removed_ids.iter().copied());
            }
            if let Some(outcome) = &exile_outcome {
                removed_ids.extend(outcome.removed_ids.iter().copied());
            }
            if let Some(outcome) = &attrition_outcome {
                removed_ids.extend(outcome.removed_ids.iter().copied());
            }
            for outcome in &missile_damage_outcomes {
                removed_ids.extend(outcome.removed_ids.iter().copied());
            }
            removed_ids.sort_unstable();
            removed_ids.dedup();
            frame_snapshot.removed_ids = removed_ids.into();
            simulation_counters.input_units = simulation_backup.len();
            simulation_counters.removed_units = frame_snapshot.removed_ids.len();
        }
        let mut next_casualties = next_casualties(
            &self.casualties,
            &self.unit_sovereign_by_id,
            &frame_snapshot,
        );
        add_attrition_casualties(
            &mut next_casualties,
            &self.unit_sovereign_by_id,
            air_damage_outcome.as_ref(),
        );
        add_aircrew_casualties(
            &mut next_casualties,
            &air_wing_sovereign_by_id,
            air_outcome.as_ref(),
        );
        add_attrition_casualties(
            &mut next_casualties,
            &self.unit_sovereign_by_id,
            attrition_outcome.as_ref(),
        );
        for outcome in &missile_damage_outcomes {
            // Browser missile damage has no attacker argument: it increments victim country/side
            // totals but deliberately creates no attacker-attribution entry.
            add_attrition_casualties(
                &mut next_casualties,
                &self.unit_sovereign_by_id,
                Some(outcome),
            );
        }
        apply_runtime_casualties_to_side_dynamics(
            &mut staged_side_dynamics,
            &self.casualties,
            &next_casualties,
            self.territory.country_to_side(),
        );
        let mut next_casualties_by_victim = next_casualties_by_victim(
            &self.casualties_by_victim,
            &self.unit_sovereign_by_id,
            &frame_snapshot,
        );
        add_air_casualty_attribution(
            &mut next_casualties_by_victim,
            &self.unit_sovereign_by_id,
            &air_wing_sovereign_by_id,
            air_outcome.as_ref(),
            air_damage_outcome.as_ref(),
        );
        let (
            influence,
            influence_source_count,
            influence_schedule,
            influence_diffusion_budget,
            influence_diffusion,
        ) = if let Some(staged) = staged_influence.take() {
            (
                staged.transaction.result().clone(),
                staged.source_count,
                staged.schedule,
                staged.diffusion_budget,
                staged.diffusion,
            )
        } else {
            let sources =
                match influence_sources(&frame_snapshot, &self.unit_policies, next_tick, None) {
                    Ok(sources) => sources,
                    Err(error) => {
                        self.simulation = Simulation::new(simulation_config, simulation_backup)
                            .expect("validated simulation rollback must reconstruct");
                        return Err(error);
                    }
                };
            let source_count = sources.len();
            let influence = match self.territory.apply_influence_sources(&sources) {
                Ok(influence) => influence,
                Err(error) => {
                    // Territory validates the complete source batch before mutation.
                    self.simulation = Simulation::new(simulation_config, simulation_backup)
                        .expect("validated simulation rollback must reconstruct");
                    return Err(RuntimeError::Territory(error));
                }
            };
            (
                influence,
                source_count,
                None,
                0,
                DiffusionQueueResult::default(),
            )
        };

        let strategic_due = next_tick.is_multiple_of(PAY_CYCLE_TICKS);
        let (mut territory_snapshot, mut census) = advance_census(
            &mut self.territory,
            self.config.census_budget,
            self.config.census_flush_chunk,
            strategic_due,
        );
        let mut strategic_snapshot = staged_strategic.latest_snapshot();
        let mut strategic_counters = None;
        let mut derivation_counters = None;
        let mut next_state = RuntimeState::Running;
        let mut strategic_fronts_invalidated = false;
        let mut staged_command_policies = None;
        let mut reinforcement_counters = RuntimeReinforcementCounters::default();
        let mut staged_material_units = None;
        let mut material_created_ids = BTreeSet::new();
        if strategic_due {
            let derived = match derive_production_input(
                next_tick,
                &self.scenario,
                &self.territory,
                &territory_snapshot,
                &frame_snapshot,
                &self.diplomacy,
                &staged_strategic,
                &next_casualties,
                &self.config.production,
            ) {
                Ok(derived) => derived,
                Err(error) => {
                    self.state = RuntimeState::Poisoned;
                    return Err(RuntimeError::Production(error));
                }
            };
            let mut strategic_input = derived.input;
            restrict_to_explicit_active_sides(
                &mut strategic_input,
                &self.diplomacy.active_sides,
                &self.diplomacy.hostility,
                self.territory.max_sides(),
            );
            let mut counters = derived.counters;
            counters.active_sides = strategic_input.active_sides.len();
            counters.hostile_pairs = strategic_input.active_hostile_pairs.len();
            derivation_counters = Some(counters);
            let mut prepared = match staged_strategic.prepare_cycle(&strategic_input) {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.state = RuntimeState::Poisoned;
                    return Err(RuntimeError::Strategic(error));
                }
            };
            let effects =
                match self.stage_strategic_effects(&mut prepared, &next_casualties_by_victim) {
                    Ok(effects) => effects,
                    Err(error) => {
                        self.state = RuntimeState::Poisoned;
                        return Err(error);
                    }
                };
            apply_personnel_loss_to_side_dynamics(
                &mut staged_side_dynamics,
                &effects.desertion_personnel_loss_by_side,
            );
            let command_updates = match self.stage_command_policies(
                &prepared.snapshot(),
                effects.simulation.as_ref().unwrap_or(&self.simulation),
            ) {
                Ok(updates) => updates,
                Err(error) => {
                    self.state = RuntimeState::Poisoned;
                    return Err(error);
                }
            };
            let (published, counters) = match staged_strategic.commit_cycle(prepared) {
                Ok(result) => result,
                Err(error) => {
                    self.state = RuntimeState::Poisoned;
                    return Err(RuntimeError::Strategic(error));
                }
            };
            let active_sides = published
                .countries
                .iter()
                .filter(|country| !country.economy.capitulated)
                .map(|country| country.side)
                .collect::<BTreeSet<_>>();
            self.diplomacy
                .active_sides
                .retain(|side| active_sides.contains(side));
            if let Some(simulation) = effects.simulation {
                let consequence_snapshot = simulation.initial_snapshot(next_tick, next_frame);
                let mut removed_ids = frame_snapshot.removed_ids.to_vec();
                removed_ids.extend(effects.removed_ids);
                removed_ids.sort_unstable();
                removed_ids.dedup();
                simulation_counters.removed_units = removed_ids.len();
                frame_snapshot.units = consequence_snapshot.units;
                frame_snapshot.removed_ids = removed_ids.into();
                self.simulation = simulation;
            }
            if let Some(territory) = effects.territory {
                territory_snapshot = territory
                    .snapshot()
                    .expect("restored surrender territory has a committed snapshot");
                census.territory_generation = territory_snapshot.generation;
                census.territory_commit_sequence = territory_snapshot.commit_sequence;
                census.committed = true;
                census.flushed_for_strategic_cycle = true;
                self.territory = territory;
            }
            strategic_fronts_invalidated = effects.fronts_invalidated;
            staged_command_policies = Some(command_updates);
            next_state = effects.state;
            strategic_snapshot = Some(published);
            strategic_counters = Some(counters);

            if let (Some(reinforcement), Some(air_power)) =
                (&mut staged_reinforcement, &mut staged_air_power)
            {
                let treasury_before = staged_strategic
                    .economies()
                    .iter()
                    .map(|(&country, economy)| (country, economy.treasury))
                    .collect::<BTreeMap<_, _>>();
                let mut maintenance_economies = staged_strategic.economies().clone();
                if let Some(material) = &mut staged_material_logistics {
                    let mut logistics_units = self.simulation.units.clone();
                    let home_positions =
                        material_home_positions(&self.simulation, &self.scenario, &self.territory);
                    let outcome = reinforcement.settle_material_pay_cycle(
                        material,
                        &mut logistics_units,
                        air_power,
                        &mut maintenance_economies,
                        &mut staged_personnel_reserves,
                        self.territory.country_to_side(),
                        self.territory.max_sides(),
                        &home_positions,
                        self.config.production.max_units_per_side,
                    )?;
                    material_created_ids.extend(
                        outcome
                            .countries
                            .iter()
                            .filter_map(|country| country.armor_creation.created_unit_id),
                    );
                    reinforcement_counters = material_pay_cycle_counters(&outcome);
                    staged_material_units = Some(logistics_units);
                } else {
                    let outcome = reinforcement.settle_air_pay_cycle(
                        air_power,
                        &mut maintenance_economies,
                        &mut staged_personnel_reserves,
                        self.territory.country_to_side(),
                        self.territory.max_sides(),
                    )?;
                    reinforcement_counters = air_pay_cycle_counters(&outcome);
                }
                let maintenance_costs = maintenance_economies
                    .iter()
                    .filter_map(|(&country, economy)| {
                        let spent = treasury_before[&country] - economy.treasury;
                        (spent > 0.0).then_some((country, spent))
                    })
                    .collect::<Vec<_>>();
                if !maintenance_costs.is_empty() {
                    staged_strategic.spend_treasury_batch(&maintenance_costs)?;
                }
                strategic_snapshot = staged_strategic.latest_snapshot();
            }
        }

        if let Some(operations) = &mut staged_operations {
            let committed_units = operational_unit_inputs(&self.simulation, None)?;
            operations.post_movement(next_tick, &committed_units);
            if SideDynamics::sample_due(next_tick) {
                let countries = operational_country_inputs(
                    &self.simulation,
                    &self.scenario,
                    &territory_snapshot,
                )?;
                operations.update_country_desperation(
                    next_tick,
                    MOMENTUM_SAMPLE_INTERVAL,
                    &countries,
                );
            }
            operations.evolve_overrides(next_tick, self.territory.country_to_side());
            let live_units = committed_units
                .iter()
                .map(|unit| (unit.unit_id, unit.side_index))
                .collect::<BTreeMap<_, _>>();
            let countries = self
                .scenario
                .countries
                .iter()
                .map(|country| country.country_id)
                .collect::<BTreeSet<_>>();
            if let Err(error) = operations.validate(
                self.territory.max_sides(),
                &live_units,
                &countries,
                next_tick,
            ) {
                // All recoverable operational validation happens before simulation commits.
                // A failure here means a post-mutation invariant was violated, matching the
                // runtime's existing strategic poison semantics.
                self.state = RuntimeState::Poisoned;
                return Err(RuntimeError::Operational(error));
            }
        }
        if let Some(execution) = &mut staged_operational_execution {
            let committed_units = match execution_unit_inputs(
                &self.simulation,
                planning_policies,
                next_tick,
                staged_battlefield.as_ref(),
                &planning,
                staged_operations.as_ref(),
            ) {
                Ok(units) => units,
                Err(error) => {
                    self.state = RuntimeState::Poisoned;
                    return Err(error);
                }
            };
            let committed_by_id = committed_units
                .iter()
                .map(|unit| (unit.unit_id, *unit))
                .collect::<BTreeMap<_, _>>();
            execution.retain_live_units(&committed_by_id);
            if let Err(error) =
                execution.validate(self.territory.max_sides(), &committed_by_id, next_tick)
            {
                self.state = RuntimeState::Poisoned;
                return Err(RuntimeError::OperationalExecution(error));
            }
        }

        let mut material_created_units = Vec::new();
        let mut material_policies = Vec::new();
        let mut material_battlefield_units = BTreeMap::new();
        let mut staged_material_simulation = None;
        if let Some(units) = staged_material_units {
            let previous_units = self.simulation.units.clone();
            let material_simulation = Simulation::new(simulation_config, units)?;
            let allies = self.territory.country_to_side().iter().fold(
                BTreeMap::<usize, BTreeSet<u16>>::new(),
                |mut by_side, (&country, &side)| {
                    by_side.entry(side).or_default().insert(country);
                    by_side
                },
            );
            for unit in material_simulation
                .units
                .iter()
                .filter(|unit| material_created_ids.contains(&unit.combat.id))
            {
                let country_id = u16::try_from(unit.combat.sovereign).map_err(|_| {
                    RuntimeError::InvalidCheckpoint("material unit sovereign exceeds u16")
                })?;
                let side = unit.combat.side as usize;
                let mut policy = RuntimeUnitPolicy::standard(unit.combat.id, country_id);
                policy.ai.deploy_until_tick = next_tick
                    .checked_add(RECRUIT_DEPLOY_TICKS)
                    .ok_or(RuntimeError::ClockOverflow)?;
                let influence = policy
                    .influence
                    .as_mut()
                    .expect("standard material policy has influence");
                influence.browser_temporal_seed = Some(unit.combat.id as f64);
                influence.owner_ally_country_ids = allies.get(&side).cloned().unwrap_or_default();
                if let Some((source_id, source)) = previous_units
                    .iter()
                    .find(|source| source.combat.sovereign == unit.combat.sovereign)
                    .and_then(|source| {
                        self.unit_policies
                            .get(&source.combat.id)
                            .map(|policy| (source.combat.id, policy))
                    })
                {
                    policy.command = source.command;
                    if let Some(resolved) = staged_command_policies
                        .as_ref()
                        .and_then(|staged| staged.policies.get(&source_id))
                    {
                        policy.command = UnitCommandPolicy {
                            band: resolved.band,
                            discipline: resolved.discipline,
                            refuses_offense: resolved.refuses_offense,
                            return_home: resolved.return_home,
                            self_defense_only: resolved.self_defense_only,
                            home_target: resolved.home_target,
                            transition_cycle: staged_command_policies
                                .as_ref()
                                .expect("resolved command came from staged command policies")
                                .cycle,
                        };
                    }
                }
                material_battlefield_units.insert(
                    unit.combat.id,
                    BattlefieldUnitState {
                        cohesion_seed: (unit.combat.id % 4) as f64 / 1_000.0,
                        ..BattlefieldUnitState::default()
                    },
                );
                material_policies.push(policy);
                material_created_units.push(unit.clone());
            }
            staged_material_simulation = Some(material_simulation);
        }

        // Browser recruitment runs after the current tactical simulation. New formations are
        // published immediately but remain deployment-inactive and enter planning next tick.
        let mut staged_recruitment = if matches!(next_state, RuntimeState::Running) {
            if let Some(reinforcement) = &mut staged_reinforcement {
                stage_recruitment(
                    next_tick,
                    next_frame,
                    staged_material_simulation
                        .as_ref()
                        .unwrap_or(&self.simulation),
                    &self.scenario,
                    &self.territory,
                    &territory_snapshot,
                    &staged_strategic,
                    staged_side_dynamics.as_ref(),
                    reinforcement,
                    &mut staged_personnel_reserves,
                    &mut staged_gameplay_rng,
                    self.config.production,
                    self.battlefield.as_ref(),
                )?
            } else {
                StagedRecruitment::default()
            }
        } else {
            StagedRecruitment::default()
        };
        staged_recruitment.policies.extend(material_policies);
        staged_recruitment
            .battlefield_units
            .extend(material_battlefield_units);
        if !staged_recruitment.treasury_costs.is_empty() {
            staged_strategic.spend_recruitment_batch(&staged_recruitment.treasury_costs)?;
            strategic_snapshot = staged_strategic.latest_snapshot();
        }
        if let Some(simulation) = staged_material_simulation {
            self.simulation = simulation;
            frame_snapshot.units = self
                .simulation
                .initial_snapshot(next_tick, next_frame)
                .units;
        }
        if !staged_recruitment.units.is_empty() {
            self.simulation
                .insert_units_atomic(staged_recruitment.units.clone())?;
            frame_snapshot.units = self
                .simulation
                .initial_snapshot(next_tick, next_frame)
                .units;
        }
        reinforcement_counters.recruited_units = staged_recruitment.counters.recruited_units;
        reinforcement_counters.recruited_personnel =
            staged_recruitment.counters.recruited_personnel;

        let attrition_counters = attrition_counters(
            attrition_outcome.as_ref(),
            staged_battlefield.as_ref(),
            exiled_ids.len(),
            recovered_personnel,
        );
        let final_operational_claimed = operational_claimed_unit_ids(
            staged_operations.as_ref(),
            staged_operational_execution.as_ref(),
        );
        let mut next_prior = assignments_by_unit(&planning.assignments);
        if let Some(stage) = &mut staged_battlefield {
            for policy in &staged_recruitment.policies {
                stage.policies.insert(policy.unit_id, policy.clone());
            }
            stage.next_unit_state.extend(
                staged_recruitment
                    .battlefield_units
                    .iter()
                    .map(|(&unit_id, &state)| (unit_id, state)),
            );
        } else {
            for policy in &staged_recruitment.policies {
                self.unit_policies.insert(policy.unit_id, policy.clone());
            }
        }
        self.unit_sovereign_by_id.extend(
            staged_recruitment
                .units
                .iter()
                .map(|unit| (unit.combat.id, unit.combat.sovereign as u16)),
        );
        self.unit_sovereign_by_id.extend(
            material_created_units
                .iter()
                .map(|unit| (unit.combat.id, unit.combat.sovereign as u16)),
        );
        self.unit_sovereign_by_id.sort_unstable();
        enrich_unit_visuals(
            &mut frame_snapshot,
            self.battlefield.as_ref(),
            &self.territory,
            staged_battlefield.as_ref(),
        )?;
        let surviving_ids = frame_snapshot
            .units
            .iter()
            .map(|unit| unit.id)
            .collect::<BTreeSet<_>>();
        if let Some(stage) = staged_battlefield {
            self.unit_policies = stage.policies;
            let battlefield = self
                .battlefield
                .as_mut()
                .expect("staged battlefield has runtime state");
            battlefield.units = stage.next_unit_state;
            battlefield
                .units
                .retain(|unit_id, _| surviving_ids.contains(unit_id));
        }
        let command_changed_ids = staged_command_policies
            .as_ref()
            .map(|stage| stage.changed_unit_ids.clone())
            .unwrap_or_default();
        if let Some(staged) = &staged_command_policies {
            apply_command_policy_updates(&mut self.unit_policies, staged);
        }
        self.unit_policies
            .retain(|unit_id, _| surviving_ids.contains(unit_id));
        self.unit_sovereign_by_id
            .retain(|(unit_id, _)| surviving_ids.contains(unit_id));
        if strategic_fronts_invalidated {
            self.objectives.clear();
            self.front_prior_by_unit.clear();
            next_prior.clear();
            self.last_front_refresh_tick = None;
        } else if let Some(layout) = &refreshed_layout {
            self.objectives.clone_from(&layout.objectives);
            self.front_prior_by_unit = front_prior_by_unit(&layout.next_prior);
            self.last_front_refresh_tick = Some(next_tick);
        }
        self.front_prior_by_unit
            .retain(|unit_id, _| surviving_ids.contains(unit_id));
        self.front_prior_by_unit
            .retain(|unit_id, _| !command_changed_ids.contains(unit_id));
        self.front_prior_by_unit
            .retain(|unit_id, _| !final_operational_claimed.contains(unit_id));
        next_prior.retain(|unit_id, _| surviving_ids.contains(unit_id));
        next_prior.retain(|unit_id, _| !command_changed_ids.contains(unit_id));
        next_prior.retain(|unit_id, _| !final_operational_claimed.contains(unit_id));
        self.prior_objective_by_unit = next_prior;
        self.casualties = next_casualties;
        self.casualties_by_victim = next_casualties_by_victim;
        self.side_dynamics = staged_side_dynamics;
        self.operations = staged_operations;
        self.naval_planning = staged_naval_planning;
        self.operational_execution = staged_operational_execution;
        self.air_power = staged_air_power;
        self.reinforcement = staged_reinforcement;
        self.material_logistics = staged_material_logistics;
        self.strategic_missiles = staged_strategic_missiles;
        self.strategic = staged_strategic;
        self.gameplay_rng = staged_gameplay_rng;
        self.personnel_reserves = staged_personnel_reserves;
        self.tick = next_tick;
        self.frame = next_frame;
        self.state = next_state;

        let queued_before = self.render_updates.len();
        if let Some(update) = self.territory.drain_render_update() {
            self.render_updates.push_back(update);
        }
        let segments = if strategic_fronts_invalidated {
            0
        } else {
            refreshed_layout.as_ref().map_or_else(
                || {
                    self.objectives
                        .iter()
                        .map(|objective| objective.segment_id)
                        .collect::<BTreeSet<_>>()
                        .len()
                },
                |layout| layout.counters.segments,
            )
        };
        let mut influence_counters = influence_counters(influence_source_count, &influence);
        if let Some(schedule) = influence_schedule {
            influence_counters.cohort = Some(schedule.cohort as u8);
            influence_counters.application_budget = schedule.application_budget;
        }
        influence_counters.diffusion_budget = influence_diffusion_budget;
        influence_counters.diffusion_processed_items = influence_diffusion.processed_items;
        influence_counters.diffusion_stale_entries = influence_diffusion.stale_entries;
        let counters = RuntimeStepCounters {
            front_refreshed: refresh_fronts,
            front_segments: segments,
            front_objectives: self.objectives.len(),
            ai: planning.counters,
            simulation: simulation_counters,
            air: air_counters(air_outcome.as_ref(), air_damage_outcome.as_ref()),
            missiles: missile_counters(missile_outcome.as_ref(), &missile_damage_outcomes),
            reinforcement: reinforcement_counters,
            naval_planning: naval_planning_counters,
            operational_execution: execution_outcome
                .as_ref()
                .map_or_else(OperationalExecutionCounters::default, |outcome| {
                    outcome.counters
                }),
            attrition: attrition_counters,
            influence: influence_counters,
            census,
            strategic: strategic_counters,
            strategic_derivation: derivation_counters,
            render_updates_enqueued: self.render_updates.len() - queued_before,
        };
        let published = Arc::new(RuntimeSnapshot {
            schema_version: NATIVE_RUNTIME_SCHEMA_VERSION,
            tick: self.tick,
            frame: self.frame,
            state: self.state,
            frame_snapshot: Arc::new(frame_snapshot),
            territory_snapshot,
            economy_snapshot: self
                .strategic
                .economies()
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .into(),
            strategic_snapshot,
            operational_snapshot: self
                .operations
                .as_ref()
                .map(|operations| Arc::new(operations.snapshot(self.tick))),
            operational_execution_snapshot: self
                .operational_execution
                .as_ref()
                .map(|execution| Arc::new(execution.clone())),
            air_power_snapshot: self
                .air_power
                .as_ref()
                .map(|air_power| Arc::new(air_power.clone())),
            strategic_missile_snapshot: self
                .strategic_missiles
                .as_ref()
                .map(|missiles| Arc::new(missiles.clone())),
            counters,
            pending_render_updates: self.render_updates.len(),
            casualty_totals: Arc::new(self.casualties.clone()),
            casualties_by_victim: Arc::new(self.casualties_by_victim.clone()),
            gameplay_rng_state: self.gameplay_rng.state(),
            personnel_reserves: Arc::new(self.personnel_reserves.clone()),
            reinforcement_snapshot: self
                .reinforcement
                .as_ref()
                .map(|reinforcement| Arc::new(reinforcement.clone())),
            material_logistics_snapshot: self
                .material_logistics
                .as_ref()
                .map(|material| Arc::new(material.clone())),
        });
        self.latest = published.clone();
        Ok(published)
    }
}

fn validate_runtime_config(config: RuntimeConfig) -> Result<(), RuntimeError> {
    if config.front_refresh_ticks == 0
        || config.census_budget == 0
        || config.census_flush_chunk == 0
        || config.front.max_grid_cells == 0
        || config.front.max_sides == 0
        || config.front.max_units == 0
        || config.front.max_frontier_cells == 0
        || config.front.max_segments == 0
        || config.front.max_assignment_edges == 0
    {
        return Err(RuntimeError::InvalidConfig);
    }
    Ok(())
}

fn validate_scenario_grid(
    scenario: &ScenarioProduction,
    territory: &TerritoryControl,
) -> Result<(), RuntimeError> {
    let expected = GridSpec {
        grid_res: territory.grid_resolution(),
        width: territory.width(),
        height: territory.height(),
    };
    if scenario.grid != expected || territory.de_jure().len() != territory.total_cells() {
        return Err(RuntimeError::InvalidCheckpoint(
            "scenario production grid does not match territory",
        ));
    }
    Ok(())
}

fn validate_diplomacy(
    diplomacy: &RuntimeDiplomacy,
    territory: &TerritoryControl,
) -> Result<(), RuntimeError> {
    let side_count = territory.max_sides();
    if diplomacy.hostility.len() != side_count.saturating_mul(side_count)
        || diplomacy.hostility != territory.hostility_matrix()
    {
        return Err(RuntimeError::InvalidCheckpoint(
            "runtime and territory hostility matrices differ",
        ));
    }
    for left in 0..side_count {
        for right in 0..side_count {
            let value = diplomacy.hostility[left * side_count + right];
            if value > 1 || (left == right && value != 0) {
                return Err(RuntimeError::InvalidCheckpoint(
                    "hostility must be binary and have a zero diagonal",
                ));
            }
        }
    }
    let mut unique = BTreeSet::new();
    for &side in &diplomacy.active_sides {
        if usize::from(side) >= side_count || !unique.insert(side) {
            return Err(RuntimeError::InvalidCheckpoint(
                "active sides must be unique and in range",
            ));
        }
    }
    Ok(())
}

fn collect_unit_policies(
    simulation: &Simulation,
    policies: Vec<RuntimeUnitPolicy>,
    territory: &TerritoryControl,
) -> Result<BTreeMap<u64, RuntimeUnitPolicy>, RuntimeError> {
    let mut by_id = BTreeMap::new();
    for policy in policies {
        let id = policy.unit_id;
        if by_id.insert(id, policy).is_some() {
            return Err(RuntimeError::InvalidUnitPolicy(id));
        }
    }
    if by_id.len() != simulation.units.len() {
        return Err(RuntimeError::InvalidCheckpoint(
            "unit policy count differs from live unit count",
        ));
    }
    for unit in &simulation.units {
        let Some(policy) = by_id.get(&unit.combat.id) else {
            return Err(RuntimeError::InvalidUnitPolicy(unit.combat.id));
        };
        let country = u16::try_from(unit.combat.sovereign)
            .ok()
            .filter(|country| *country > 0)
            .ok_or(RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?;
        if territory.country_to_side().get(&country).copied() != Some(unit.combat.side as usize) {
            return Err(RuntimeError::InvalidCheckpoint(
                "unit sovereign and side mapping disagree",
            ));
        }
        validate_unit_policy(policy, territory)?;
    }
    Ok(by_id)
}

fn hydrate_missing_command_home_targets(
    simulation: &Simulation,
    policies: &mut BTreeMap<u64, RuntimeUnitPolicy>,
    strategic: &StrategicSimulation,
    scenario: &ScenarioProduction,
    territory: &TerritoryControl,
) -> Result<(), RuntimeError> {
    if !policies
        .values()
        .any(|policy| policy.command.return_home && policy.command.home_target.is_none())
    {
        return Ok(());
    }
    let country_bands = strategic
        .economies()
        .iter()
        .map(|(&country, economy)| (country, economy.command_band))
        .collect::<BTreeMap<_, _>>();
    let resolved =
        resolve_command_policies_for(simulation, policies, &country_bands, scenario, territory)?;
    for (&unit_id, policy) in policies.iter_mut() {
        if policy.command.return_home && policy.command.home_target.is_none() {
            policy.command.home_target = resolved
                .get(&unit_id)
                .and_then(|resolved| resolved.home_target);
        }
    }
    Ok(())
}

fn resolve_command_policies_for(
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    country_bands: &BTreeMap<u16, CommandBand>,
    scenario: &ScenarioProduction,
    territory: &TerritoryControl,
) -> Result<BTreeMap<u64, ResolvedCommandPolicy>, RuntimeError> {
    let capital_targets = scenario
        .cities
        .iter()
        .filter(|city| {
            city.capital
                && city.owner_id > 0
                && city.cell < territory.total_cells()
                && city.lat.is_finite()
                && city.lng.is_finite()
                && territory.country_to_side().contains_key(&city.owner_id)
        })
        .map(|city| {
            (
                city.owner_id,
                CommandHomeTarget {
                    cell: city.cell,
                    lat: city.lat,
                    lng: city.lng,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let units = simulation
        .units
        .iter()
        .map(|unit| {
            let policy = policies
                .get(&unit.combat.id)
                .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
            Ok(CommandUnitState {
                id: unit.combat.id,
                sovereign_id: u16::try_from(unit.combat.sovereign)
                    .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?,
                side: usize::try_from(unit.combat.side).map_err(|_| {
                    RuntimeError::InvalidCheckpoint("unit side exceeds platform width")
                })?,
                discipline: policy.command.discipline,
            })
        })
        .collect::<Result<Vec<_>, RuntimeError>>()?;
    resolve_command_batch(
        &units,
        country_bands,
        CommandWorld {
            grid_resolution: territory.grid_resolution(),
            width: territory.width(),
            height: territory.height(),
            land: territory.land(),
            world_control: territory.world_control(),
            dominant_side: territory.dominant_side(),
            country_side: territory.country_to_side(),
            capital_targets: &capital_targets,
        },
    )
    .map_err(RuntimeError::from)
}

fn validate_command_checkpoint(
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    strategic: &StrategicSimulation,
) -> Result<(), RuntimeError> {
    for unit in &simulation.units {
        let country = u16::try_from(unit.combat.sovereign)
            .map_err(|_| RuntimeError::InvalidCheckpoint("invalid unit sovereign"))?;
        let policy = policies
            .get(&unit.combat.id)
            .ok_or(RuntimeError::InvalidUnitPolicy(unit.combat.id))?;
        let economy =
            strategic
                .economies()
                .get(&country)
                .ok_or(RuntimeError::InvalidCheckpoint(
                    "unit country has no strategic economy",
                ))?;
        if policy.command.band != economy.command_band
            || policy.command.transition_cycle > strategic.cycle()
        {
            return Err(RuntimeError::InvalidUnitPolicy(unit.combat.id));
        }
    }
    Ok(())
}

fn validate_unit_policy(
    policy: &RuntimeUnitPolicy,
    territory: &TerritoryControl,
) -> Result<(), RuntimeError> {
    let ai = policy.ai;
    if ![
        ai.base_speed,
        ai.movement.terrain_speed_multiplier,
        ai.movement.speed_multiplier,
        ai.movement.plan_speed_multiplier,
        ai.movement.neutral_penalty,
        ai.movement.push_readiness,
        ai.combat.dealt_multiplier,
        ai.combat.taken_multiplier,
        ai.combat.defense_bonus,
        ai.combat.long_war_defense,
    ]
    .into_iter()
    .all(f64::is_finite)
        || ai.base_speed < 0.0
        || ai.movement.terrain_speed_multiplier < 0.0
        || ai.movement.speed_multiplier < 0.0
        || ai.movement.plan_speed_multiplier < 0.0
        || ai.movement.neutral_penalty < 0.0
        || ai.movement.push_readiness < 0.0
        || ai.combat.dealt_multiplier < 0.0
        || ai.combat.taken_multiplier < 0.0
        || ai.combat.defense_bonus < 0.0
        || ai.combat.long_war_defense < 0.0
    {
        return Err(RuntimeError::InvalidUnitPolicy(policy.unit_id));
    }
    let command = policy.command;
    let expected_refusal = command.discipline < crate::economy::command_refusal_share(command.band);
    let expected_return = matches!(command.band, CommandBand::Breakdown | CommandBand::Mutiny);
    if !command.discipline.is_finite()
        || !(0.0..1.0).contains(&command.discipline)
        || command.refuses_offense != expected_refusal
        || command.return_home != expected_return
        || command.self_defense_only != (command.band == CommandBand::Mutiny)
        || (command.home_target.is_some() && !command.return_home)
        || command.home_target.is_some_and(|target| {
            target.cell >= territory.width() * territory.height()
                || !target.lat.is_finite()
                || !target.lng.is_finite()
        })
    {
        return Err(RuntimeError::InvalidUnitPolicy(policy.unit_id));
    }
    let Some(influence) = &policy.influence else {
        return Ok(());
    };
    if ![
        influence.radius,
        influence.delta,
        influence.concentration_bonus,
    ]
    .into_iter()
    .all(f64::is_finite)
        || influence.radius <= 0.0
        || influence.delta < 0.0
        || influence.concentration_bonus < 0.0
        || influence
            .browser_temporal_seed
            .is_some_and(|seed| !seed.is_finite())
    {
        return Err(RuntimeError::InvalidUnitPolicy(policy.unit_id));
    }
    if influence.refuses_offense != command.refuses_offense {
        return Err(RuntimeError::InvalidUnitPolicy(policy.unit_id));
    }
    for country in influence
        .beneficiary
        .iter()
        .chain(influence.owner_ally_country_ids.iter())
        .chain(influence.protected_owner_ids.iter())
        .chain(influence.rebel_de_jure.iter())
        .chain(influence.credit_de_jure.iter())
        .chain(influence.credit_de_jure_by_country.keys())
        .chain(influence.credit_de_jure_by_country.values())
    {
        if *country == 0 || !territory.country_to_side().contains_key(country) {
            return Err(RuntimeError::InvalidUnitPolicy(policy.unit_id));
        }
    }
    Ok(())
}

fn validate_prior_assignments(
    simulation: &Simulation,
    objectives: &[FrontObjective],
    assignments: &BTreeMap<u64, u64>,
) -> Result<(), RuntimeError> {
    let units = simulation
        .units
        .iter()
        .map(|unit| unit.combat.id)
        .collect::<BTreeSet<_>>();
    let objective_ids = objectives
        .iter()
        .map(|objective| objective.id)
        .collect::<BTreeSet<_>>();
    if objective_ids.len() != objectives.len()
        || assignments
            .iter()
            .any(|(unit, objective)| !units.contains(unit) || !objective_ids.contains(objective))
    {
        return Err(RuntimeError::InvalidCheckpoint(
            "prior front assignments reference missing units or objectives",
        ));
    }
    Ok(())
}

fn validate_front_planner_state(
    tick: u64,
    simulation: &Simulation,
    objectives: &[FrontObjective],
    front_prior_by_unit: &BTreeMap<u64, FrontLayoutPrior>,
    last_front_refresh_tick: Option<u64>,
) -> Result<(), RuntimeError> {
    if last_front_refresh_tick.is_some_and(|refresh_tick| refresh_tick > tick) {
        return Err(RuntimeError::InvalidCheckpoint(
            "last front refresh is newer than the runtime clock",
        ));
    }
    if last_front_refresh_tick.is_none() && !front_prior_by_unit.is_empty() {
        return Err(RuntimeError::InvalidCheckpoint(
            "front layout prior has no completed refresh",
        ));
    }

    let units = simulation
        .units
        .iter()
        .map(|unit| unit.combat.id)
        .collect::<BTreeSet<_>>();
    let objective_ids = objectives
        .iter()
        .map(|objective| objective.id)
        .collect::<BTreeSet<_>>();
    if front_prior_by_unit.iter().any(|(unit_id, prior)| {
        *unit_id != prior.unit_id
            || prior.pair_key.is_empty()
            || !units.contains(unit_id)
            || !objective_ids.contains(&prior.objective_id)
    }) {
        return Err(RuntimeError::InvalidCheckpoint(
            "front layout prior references missing units or objectives",
        ));
    }
    Ok(())
}

fn validate_side_dynamics_state(
    dynamics: Option<&BTreeMap<usize, SideDynamics>>,
    side_count: usize,
    checkpoint_frame: u64,
    controlled_cell_limit: u64,
) -> Result<(), RuntimeError> {
    let Some(dynamics) = dynamics else {
        return Ok(());
    };
    if dynamics.len() != side_count
        || dynamics.iter().enumerate().any(|(expected, (&key, side))| {
            key != expected
                || side.side_index != key
                || !side.validate(checkpoint_frame, controlled_cell_limit)
        })
    {
        return Err(RuntimeError::InvalidCheckpoint(
            "side dynamics must exactly cover the stable side topology",
        ));
    }
    Ok(())
}

fn validate_personnel_reserves(
    reserves: &BTreeMap<usize, f64>,
    side_count: usize,
) -> Result<(), RuntimeError> {
    if reserves.len() != side_count
        || reserves
            .iter()
            .enumerate()
            .any(|(expected, (&side, value))| {
                side != expected || !value.is_finite() || *value < 0.0
            })
    {
        return Err(RuntimeError::InvalidCheckpoint(
            "personnel reserves must exactly cover the stable side topology",
        ));
    }
    Ok(())
}

fn validate_casualties(
    casualties: &BTreeMap<u16, f64>,
    scenario: &ScenarioProduction,
) -> Result<(), RuntimeError> {
    let countries = scenario
        .countries
        .iter()
        .map(|country| country.country_id)
        .collect::<BTreeSet<_>>();
    if casualties
        .iter()
        .any(|(country, value)| !countries.contains(country) || !value.is_finite() || *value < 0.0)
    {
        return Err(RuntimeError::InvalidCheckpoint("invalid casualty ledger"));
    }
    Ok(())
}

fn validate_casualties_by_victim(
    casualties: &BTreeMap<u16, BTreeMap<u16, f64>>,
    scenario: &ScenarioProduction,
) -> Result<(), RuntimeError> {
    let countries = scenario
        .countries
        .iter()
        .map(|country| country.country_id)
        .collect::<BTreeSet<_>>();
    if casualties.iter().any(|(victim, attackers)| {
        !countries.contains(victim)
            || attackers.iter().any(|(attacker, value)| {
                attacker == victim
                    || !countries.contains(attacker)
                    || !value.is_finite()
                    || *value < 0.0
            })
    }) {
        return Err(RuntimeError::InvalidCheckpoint(
            "invalid victim-to-attacker casualty ledger",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_production_boundary(
    tick: u64,
    scenario: &ScenarioProduction,
    territory: &TerritoryControl,
    territory_snapshot: &TerritorySnapshot,
    frame: &FrameSnapshot,
    diplomacy: &RuntimeDiplomacy,
    strategic: &StrategicSimulation,
    casualties: &BTreeMap<u16, f64>,
    config: &ProductionConfig,
) -> Result<(), ProductionError> {
    derive_production_input(
        tick,
        scenario,
        territory,
        territory_snapshot,
        frame,
        diplomacy,
        strategic,
        casualties,
        config,
    )
    .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
fn derive_production_input(
    tick: u64,
    scenario: &ScenarioProduction,
    territory: &TerritoryControl,
    territory_snapshot: &TerritorySnapshot,
    frame: &FrameSnapshot,
    diplomacy: &RuntimeDiplomacy,
    strategic: &StrategicSimulation,
    casualties: &BTreeMap<u16, f64>,
    config: &ProductionConfig,
) -> Result<crate::production::StrategicDerivationOutput, ProductionError> {
    derive_strategic_cycle_input(
        StrategicDerivationInput {
            tick,
            force: false,
            scenario,
            grid: scenario.grid,
            de_jure: territory.de_jure(),
            territory: territory_snapshot,
            expected_territory: TerritoryCommitMarker::from(territory_snapshot),
            territory_fresh: true,
            frame,
            country_to_side: territory.country_to_side(),
            side_count: territory.max_sides(),
            hostility_matrix: &diplomacy.hostility,
            economies: strategic.economies(),
            occupations: strategic.occupations(),
            casualties,
        },
        config,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_ai_boundary(
    config: AiOrderConfig,
    tick: u64,
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    prior: &BTreeMap<u64, u64>,
    objectives: &[FrontObjective],
    territory: &TerritoryControl,
    diplomacy: &RuntimeDiplomacy,
) -> Result<(), RuntimeError> {
    let units = ai_units(
        simulation,
        policies,
        prior,
        &BTreeSet::new(),
        tick,
        None,
        None,
    )?;
    resolve_ai_orders(
        config,
        &units,
        AiWorldInput {
            grid_width: territory.width(),
            grid_height: territory.height(),
            grid_res: territory.grid_resolution(),
            land_mask: territory.land(),
            dominant_side_map: territory.dominant_side(),
            hostility: HostilityMatrix::new(Some(&diplomacy.hostility), territory.max_sides()),
            frontline_latitude: None,
            frontline_longitude: None,
            objectives,
        },
    )?;
    Ok(())
}

fn advance_census(
    territory: &mut TerritoryControl,
    budget: usize,
    flush_chunk: usize,
    flush: bool,
) -> (Arc<TerritorySnapshot>, RuntimeCensusCounters) {
    if !flush {
        let result = territory.advance_census(budget);
        let snapshot = territory
            .snapshot()
            .expect("runtime construction establishes the initial census");
        return (snapshot.clone(), census_counters(&result, &snapshot, false));
    }

    let mut processed_items = 0;
    let mut committed = false;
    loop {
        let status = territory.census_status();
        if status.active_generation.is_none() && status.dirty_tile_indices.is_empty() {
            let snapshot = territory
                .snapshot()
                .expect("runtime construction establishes the initial census");
            return (
                snapshot.clone(),
                RuntimeCensusCounters {
                    processed_items,
                    committed,
                    flushed_for_strategic_cycle: true,
                    territory_generation: snapshot.generation,
                    territory_commit_sequence: snapshot.commit_sequence,
                },
            );
        }
        let result = territory.advance_census(flush_chunk);
        processed_items += result.processed_items;
        committed |= result.committed;
    }
}

fn restrict_to_explicit_active_sides(
    input: &mut crate::strategic::StrategicCycleInput,
    explicit: &[u16],
    hostility: &[u8],
    side_count: usize,
) {
    let allowed = explicit.iter().copied().collect::<BTreeSet<_>>();
    input.active_sides.retain(|side| allowed.contains(side));
    input
        .active_hostile_pairs
        .retain(|(left, right)| allowed.contains(left) && allowed.contains(right));
    input.capitulation_active_sides = Some(
        input
            .active_sides
            .iter()
            .copied()
            .filter(|&left| {
                input.active_sides.iter().copied().any(|right| {
                    left != right
                        && hostility[usize::from(left) * side_count + usize::from(right)] == 1
                })
            })
            .collect(),
    );
    for country in &mut input.countries {
        country.active &= allowed.contains(&country.side);
    }
}

fn canonical_hostile_pairs(
    active_sides: &[u16],
    hostility: &[u8],
    side_count: usize,
) -> Vec<(u16, u16)> {
    let active = active_sides.iter().copied().collect::<BTreeSet<_>>();
    let active = active.into_iter().collect::<Vec<_>>();
    let mut pairs = Vec::new();
    for (position, &left) in active.iter().enumerate() {
        for &right in &active[position + 1..] {
            let left_index = usize::from(left);
            let right_index = usize::from(right);
            if hostility[left_index * side_count + right_index] == 1
                || hostility[right_index * side_count + left_index] == 1
            {
                pairs.push((left, right));
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        air::{AirCountryCoverage, AirRole, AirWing, AirWingState, Airfield},
        battlefield::{BattlefieldConfig, CountryBattlefieldPrimitives},
        combat::{CombatConfig, CombatEvent, CombatLayer, CombatUnit, UnitKind},
        economy::{CommandBand, EconomySeed, create_economy_state},
        operational_execution::{NavalMember, NavalOperation},
        operations::{
            CountryDesperationMode, CountryDesperationState, OperationalTaskForce, TaskForceMember,
            TaskForcePhase, TaskForcePosture, TaskForceRole,
        },
        production::{PRODUCTION_SCHEMA_VERSION, ProductionCountry, ScenarioProductionCounters},
        simulation::{SimulationConfig, SimulationUnit},
        territory::{CellStateUpdate, TerritoryConfig, TerritoryMaps},
    };

    fn country(country_id: u16) -> ProductionCountry {
        ProductionCountry {
            country_id,
            name: format!("Country {country_id}"),
            gdp: 100.0,
            population: 1_000_000.0,
            is_rebel: false,
            initial_core_cells: 4,
            initial_owned_land_cells: 4,
            initial_city_population: 0.0,
            capital_cell: None,
            expected_army_units: 3.0,
        }
    }

    fn unit(id: u64, side: u64, sovereign: u64, lng: f64) -> SimulationUnit {
        SimulationUnit {
            combat: CombatUnit {
                id,
                side,
                sovereign,
                kind: UnitKind::Army,
                lat: -90.0,
                lng,
                health: 100.0,
                max_health: 100.0,
                personnel: 1_000,
                personnel_capacity: 1_000,
                equipment: 0,
                max_equipment: 0,
                quality: 50.0,
                transport: false,
                armor_supported: false,
                landing_penalty_active: false,
                at_sea: false,
                last_combat_tick: 0,
                victory_boost_ticks: 0,
            },
            dir_lat: 0.0,
            dir_lng: 0.0,
            coast_stuck_ticks: 0,
            armor_landing_penalty_until_tick: 0,
            is_support: false,
            ally_weight: 1.0,
        }
    }

    #[test]
    fn casualty_lookup_survives_same_tick_unit_removal() {
        let event = CombatEvent {
            schema_version: crate::combat::COMBAT_SCHEMA_VERSION,
            layer: CombatLayer::Direct,
            attacker_id: 1,
            target_id: 2,
            target_damage: 100.0,
            attacker_damage: 1.0,
            transport_self_damage: 0.0,
            target_personnel_loss: 7,
            attacker_personnel_loss: 3,
            target_equipment_loss: 0,
            attacker_equipment_loss: 0,
            target_resulting_health: 0.0,
            attacker_resulting_health: 99.0,
            target_knockback_blocked: false,
            attacker_knockback_blocked: false,
        };
        let snapshot = FrameSnapshot {
            schema_version: crate::simulation::NATIVE_TICK_SCHEMA_VERSION,
            tick: 1,
            frame: 1,
            units: Arc::from([]),
            events: Arc::from([event]),
            removed_ids: Arc::from([2]),
            abandoned_ids: Arc::from([]),
        };

        let casualties = next_casualties(&BTreeMap::new(), &[(1, 10), (2, 20)], &snapshot);

        assert_eq!(casualties, BTreeMap::from([(10, 3.0), (20, 7.0)]));
    }

    #[test]
    fn side_personnel_tracks_desertion_but_not_capitulation_removal() {
        let mut armor = unit(2, 1, 2, 90.0);
        armor.combat.kind = UnitKind::Armor;
        armor.combat.personnel = 0;
        armor.combat.personnel_capacity = 0;
        armor.combat.equipment = 10;
        armor.combat.max_equipment = 10;
        let simulation =
            Simulation::new(SimulationConfig::default(), vec![unit(1, 0, 1, 0.0), armor]).unwrap();
        let losses = desertion_personnel_loss_by_side(
            &simulation,
            &[
                DesertionCommand {
                    country_id: 1,
                    rate: 0.1,
                },
                DesertionCommand {
                    country_id: 2,
                    rate: 0.1,
                },
            ],
        );

        assert_eq!(losses, BTreeMap::from([(0, 100.0), (1, 2.0)]));
        // Capitulation IDs are intentionally absent from this calculation: deleting the
        // remaining formations retires the side but does not spend its surviving manpower pool.
    }

    #[test]
    fn attrition_land_totals_use_committed_country_control_and_directed_hostility() {
        let runtime = fixture(0, false);
        let totals = hostile_controlled_land_by_side(
            &runtime.latest.territory_snapshot,
            runtime.territory.country_to_side(),
            &[0, 1, 0, 0],
            2,
        );
        assert_eq!(totals, vec![4.0, 0.0]);
    }

    fn exile_fixture(two_armies: bool, armor: bool) -> NativeRuntime {
        let mut source = fixture(0, false);
        let mut state = source.checkpoint_state().unwrap();
        state.gameplay_rng = GameplayRngState {
            state: if two_armies { 0 } else { 0x6d2b_79f5 },
        };
        state.territory_config.maps.primary_occupier.fill(2);
        state.territory_config.maps.dominant_side.fill(1);
        state.territory_config.maps.occupation.fill(-1.0);
        state.territory_config.maps.side_influence[0].fill(0.0);
        state.territory_config.maps.side_influence[1].fill(1.0);
        state.units[0].combat.at_sea = true;
        if armor {
            state.units[0].combat.kind = UnitKind::Armor;
            state.units[0].combat.personnel = 0;
            state.units[0].combat.personnel_capacity = 0;
            state.units[0].combat.equipment = 17;
            state.units[0].combat.max_equipment = 20;
        }
        if two_armies {
            let mut second = state.units[0].clone();
            second.combat.id = 3;
            second.combat.lng = -90.0;
            state.units.push(second);
            state.unit_policies.push(RuntimeUnitPolicy::standard(3, 1));
        }
        let checkpoint = runtime_checkpoint(state);
        NativeRuntime::new(RuntimeConfig::default(), checkpoint).unwrap()
    }

    #[test]
    fn naval_exile_uses_reverse_draw_order_recovers_reserve_and_resumes_exactly() {
        let mut uninterrupted = exile_fixture(true, false);
        let snapshot = uninterrupted.step().unwrap();

        assert_eq!(snapshot.counters.attrition.exiled_units, 1);
        assert_eq!(snapshot.counters.attrition.recovered_personnel, 1_000);
        assert_eq!(&*snapshot.frame_snapshot.removed_ids, &[1]);
        assert_eq!(
            snapshot
                .frame_snapshot
                .units
                .iter()
                .map(|unit| unit.id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(snapshot.personnel_reserves.get(&0), Some(&1_000.0));
        assert!(snapshot.casualty_totals.is_empty());
        assert!(snapshot.casualties_by_victim.is_empty());

        let split = uninterrupted.checkpoint_state().unwrap();
        assert_eq!(split.gameplay_rng.state, 0x6d2b_79f5_u32.wrapping_mul(2));
        let mut restored =
            NativeRuntime::new(split.runtime_config, runtime_checkpoint(split.clone())).unwrap();
        let expected = uninterrupted.step().unwrap();
        let actual = restored.step().unwrap();
        assert_eq!(actual.frame_snapshot, expected.frame_snapshot);
        assert_eq!(actual.personnel_reserves, expected.personnel_reserves);
        assert_eq!(
            restored.checkpoint_state().unwrap().gameplay_rng,
            uninterrupted.checkpoint_state().unwrap().gameplay_rng
        );
    }

    #[test]
    fn armor_naval_exile_returns_live_crew_without_casualties() {
        let mut runtime = exile_fixture(false, true);
        let snapshot = runtime.step().unwrap();

        assert_eq!(snapshot.counters.attrition.exiled_units, 1);
        assert_eq!(snapshot.counters.attrition.recovered_personnel, 34);
        assert_eq!(snapshot.personnel_reserves.get(&0), Some(&34.0));
        assert_eq!(&*snapshot.frame_snapshot.removed_ids, &[1]);
        assert!(snapshot.casualty_totals.is_empty());
    }

    fn fixture(tick: u64, collapsed_second_side: bool) -> NativeRuntime {
        let grid = GridSpec {
            grid_res: 90.0,
            width: 4,
            height: 2,
        };
        let countries = vec![country(1), country(2)];
        let economy_states = countries
            .iter()
            .map(|country| {
                create_economy_state(EconomySeed {
                    country_id: country.country_id,
                    gdp: country.gdp,
                    population: country.population,
                    territory_units: country.expected_army_units,
                    initial_core_cells: country.initial_core_cells,
                    initial_city_population: country.initial_city_population,
                })
                .unwrap()
            })
            .collect::<Vec<_>>();
        let scenario = ScenarioProduction {
            schema_version: PRODUCTION_SCHEMA_VERSION,
            grid,
            countries: countries.into(),
            cities: Arc::from([]),
            economy_seeds: Arc::from([]),
            economy_states: economy_states.clone().into(),
            counters: ScenarioProductionCounters {
                countries: 2,
                land_cells: 8,
                economy_seeds: 2,
                ..ScenarioProductionCounters::default()
            },
        };
        let world_control = vec![1, 1, 2, 2, 1, 1, 2, 2];
        let dominant_side = if collapsed_second_side {
            vec![0; 8]
        } else {
            vec![0, 0, 1, 1, 0, 0, 1, 1]
        };
        let primary_occupier = if collapsed_second_side {
            vec![1; 8]
        } else {
            world_control.clone()
        };
        let mut side_influence = vec![vec![0.0; 8]; 2];
        let mut occupation = vec![0.0; 8];
        for cell in 0..8 {
            let side = usize::from(dominant_side[cell] == 1);
            side_influence[side][cell] = 1.0;
            occupation[cell] = if side == 0 { 1.0 } else { -1.0 };
        }
        let territory = TerritoryControl::new(TerritoryConfig {
            width: grid.width,
            height: grid.height,
            grid_resolution: grid.grid_res,
            max_sides: 2,
            tile_size: 2,
            maps: TerritoryMaps {
                land: vec![2; 8],
                world_control: world_control.clone(),
                de_jure: world_control,
                primary_occupier,
                dominant_side,
                occupation,
                side_influence,
            },
            country_to_side: BTreeMap::from([(1, 0), (2, 1)]),
            hostility_matrix: vec![0, 1, 1, 0],
            cities: Vec::new(),
            protected_owner_ids: BTreeSet::new(),
            topology_revision: 1,
            world_revision: 1,
            city_revision: 1,
        })
        .unwrap();
        let mut units = vec![unit(1, 0, 1, 0.0)];
        if !collapsed_second_side {
            units.push(unit(2, 1, 2, 90.0));
        }
        let simulation = Simulation::new(
            SimulationConfig {
                tactical_cell_size: 1.0,
                combat: CombatConfig {
                    combat_damage: 0.0,
                    target_jitter_scale: 0.0,
                    ..CombatConfig::default()
                },
            },
            units,
        )
        .unwrap();
        let strategic = StrategicSimulation::new(economy_states, []).unwrap();
        let objectives = vec![
            FrontObjective::new(1, [0, 1], 1, -90.0, 0.0, 1, 10).unwrap(),
            FrontObjective::new(2, [1, 0], 1, -90.0, -90.0, 1, 10).unwrap(),
        ];
        let mut policies = simulation
            .units
            .iter()
            .map(|unit| RuntimeUnitPolicy::standard(unit.combat.id, unit.combat.sovereign as u16))
            .collect::<Vec<_>>();
        // Ensure the side-zero source changes enemy primary credit on the first step, producing a
        // sparse renderer delta after the constructor's queued full update.
        policies[0].influence.as_mut().unwrap().delta = 0.2;
        NativeRuntime::new(
            RuntimeConfig {
                census_budget: 1,
                census_flush_chunk: 2,
                ..RuntimeConfig::default()
            },
            RuntimeCheckpoint {
                tick,
                frame: tick,
                war_grace_end: u64::MAX,
                simulation,
                territory,
                strategic,
                scenario,
                diplomacy: RuntimeDiplomacy {
                    hostility: vec![0, 1, 1, 0],
                    active_sides: vec![0, 1],
                },
                unit_policies: policies,
                battlefield: None,
                objectives,
                prior_objective_by_unit: BTreeMap::new(),
                front_prior_by_unit: BTreeMap::new(),
                last_front_refresh_tick: None,
                casualties: BTreeMap::new(),
                casualties_by_victim: BTreeMap::new(),
                gameplay_rng: GameplayRngState {
                    state: crate::gameplay_rng::DEFAULT_GAMEPLAY_RNG_SEED,
                },
                personnel_reserves: BTreeMap::from([(0, 0.0), (1, 0.0)]),
                side_dynamics: None,
                operations: None,
                naval_planning: None,
                operational_execution: None,
                air_power: None,
                reinforcement: None,
                material_logistics: None,
                strategic_missiles: None,
            },
        )
        .unwrap()
    }

    fn runtime_checkpoint(state: NativeRuntimeCheckpointState) -> RuntimeCheckpoint {
        RuntimeCheckpoint {
            tick: state.tick,
            frame: state.frame,
            war_grace_end: state.war_grace_end,
            simulation: Simulation::new(state.simulation_config, state.units).unwrap(),
            territory: TerritoryControl::restore(
                state.territory_config,
                state.territory_committed_state,
            )
            .unwrap(),
            strategic: StrategicSimulation::restore(
                state.strategic_cycle,
                state.economies,
                state.occupations,
            )
            .unwrap(),
            scenario: state.scenario,
            diplomacy: state.diplomacy,
            unit_policies: state.unit_policies,
            battlefield: state.battlefield,
            objectives: state.objectives,
            prior_objective_by_unit: state.prior_objective_by_unit,
            front_prior_by_unit: state.front_prior_by_unit,
            last_front_refresh_tick: state.last_front_refresh_tick,
            casualties: state.casualties,
            casualties_by_victim: state.casualties_by_victim,
            gameplay_rng: state.gameplay_rng,
            personnel_reserves: state.personnel_reserves,
            side_dynamics: state.side_dynamics,
            operations: state.operations,
            naval_planning: state.naval_planning,
            operational_execution: state.operational_execution,
            air_power: state.air_power,
            reinforcement: state.reinforcement,
            material_logistics: state.material_logistics,
            strategic_missiles: state.strategic_missiles,
        }
    }

    fn with_test_missile(mut runtime: NativeRuntime, progress: f64) -> NativeRuntime {
        let config = runtime.config;
        let mut captured = runtime.checkpoint_state().unwrap();
        captured.simulation_config.combat.combat_damage = 0.7;
        let target = captured
            .units
            .iter()
            .find(|unit| unit.combat.side == 1)
            .unwrap();
        captured.strategic_missiles = Some(StrategicMissileState {
            schema: crate::strategic_missile::STRATEGIC_MISSILE_SCHEMA_VERSION.to_owned(),
            enabled: true,
            technology_allowed: true,
            bases: Vec::new(),
            missiles: vec![crate::strategic_missile::StrategicMissile {
                id: 0.25,
                start_lat: 0.0,
                start_lng: 0.0,
                target_lat: target.combat.lat,
                target_lng: target.combat.lng,
                current_lat: 0.0,
                current_lng: 0.0,
                next_lat: 0.0,
                next_lng: 0.0,
                progress,
                side_index: 0,
                phase: crate::strategic_missile::MissilePhase::Falling,
                trail: Vec::new(),
                peak_alt: 2.0,
            }],
            explosions: Vec::new(),
        });
        NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap()
    }

    #[test]
    fn missile_impact_publishes_damage_and_effects_without_mutating_old_snapshot() {
        let mut runtime = with_test_missile(fixture(0, false), 0.999);
        let old = runtime.latest_snapshot();
        let target_before = old
            .frame_snapshot
            .units
            .iter()
            .find(|unit| unit.side == 1)
            .unwrap()
            .health;

        let next = runtime.step().unwrap();
        let missiles = next.strategic_missile_snapshot.as_ref().unwrap();
        assert!(missiles.missiles.is_empty());
        assert_eq!(missiles.explosions.len(), 1);
        assert_eq!(missiles.explosions[0].life, 29);
        let target_after = next
            .frame_snapshot
            .units
            .iter()
            .find(|unit| unit.side == 1)
            .unwrap()
            .health;
        assert!(target_after < target_before);
        assert_eq!(next.counters.missiles.impacts, 1);
        assert_eq!(next.counters.missiles.damaged_units, 1);
        assert!(next.counters.missiles.personnel_loss > 0);
        assert!(next.casualty_totals.values().sum::<f64>() > 0.0);
        assert_eq!(next.strategic_snapshot, old.strategic_snapshot);

        let old_missiles = old.strategic_missile_snapshot.as_ref().unwrap();
        assert_eq!(old_missiles.missiles.len(), 1);
        assert!(old_missiles.explosions.is_empty());
        assert_eq!(
            old.frame_snapshot
                .units
                .iter()
                .find(|unit| unit.side == 1)
                .unwrap()
                .health,
            target_before
        );
    }

    #[test]
    fn same_tick_land_combat_events_survive_missile_snapshot_refresh() {
        let mut base = fixture(0, false);
        base.war_grace_end = 0;
        base.simulation.units[0].combat.lng = 0.0;
        base.simulation.units[1].combat.lng = 0.1;
        let base = with_live_battlefield(base, vec![0.0; 8]);
        let mut runtime = with_test_missile(base, 0.999);

        let next = runtime.step().unwrap();

        assert!(next.counters.simulation.accepted_contacts > 0);
        assert!(!next.frame_snapshot.events.is_empty());
        assert_eq!(next.counters.missiles.impacts, 1);
        assert_eq!(next.counters.missiles.damaged_units, 1);
    }

    #[test]
    fn missile_flight_and_impact_continue_exactly_across_checkpoint_split() {
        let mut uninterrupted = with_test_missile(fixture(0, false), 0.98);
        uninterrupted.step().unwrap();
        assert_eq!(
            uninterrupted
                .latest_snapshot()
                .strategic_missile_snapshot
                .as_ref()
                .unwrap()
                .missiles
                .len(),
            1
        );
        let config = uninterrupted.config;
        let split = uninterrupted.checkpoint_state().unwrap();
        let mut resumed = NativeRuntime::new(config, runtime_checkpoint(split)).unwrap();

        let expected = uninterrupted.step().unwrap();
        let actual = resumed.step().unwrap();
        assert_eq!(actual.frame_snapshot, expected.frame_snapshot);
        assert_eq!(
            actual.strategic_missile_snapshot,
            expected.strategic_missile_snapshot
        );
        assert_eq!(actual.counters.missiles, expected.counters.missiles);
        assert_eq!(actual.casualty_totals, expected.casualty_totals);
        assert_eq!(actual.gameplay_rng_state, expected.gameplay_rng_state);
    }

    #[test]
    fn runtime_launch_consumes_browser_rng_sequence_and_resumes_in_flight() {
        let mut base = fixture(0, false);
        let config = base.config;
        let mut captured = base.checkpoint_state().unwrap();
        captured.simulation_config.combat.combat_damage = 0.7;
        captured.gameplay_rng = GameplayRngState { state: 35 };
        captured.strategic_missiles = Some(StrategicMissileState {
            schema: crate::strategic_missile::STRATEGIC_MISSILE_SCHEMA_VERSION.to_owned(),
            enabled: true,
            technology_allowed: true,
            bases: vec![
                crate::strategic_missile::MissileBase {
                    lat: -45.0,
                    lng: -135.0,
                    side_index: 0,
                },
                crate::strategic_missile::MissileBase {
                    lat: 45.0,
                    lng: 45.0,
                    side_index: 1,
                },
            ],
            missiles: Vec::new(),
            explosions: Vec::new(),
        });
        let mut uninterrupted = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();

        let launch = uninterrupted.step().unwrap();
        assert_eq!(launch.counters.missiles.launches, 1);
        assert_eq!(launch.gameplay_rng_state.state, 4_231_026_134);
        let flight = launch.strategic_missile_snapshot.as_ref().unwrap();
        assert_eq!(flight.missiles.len(), 1);
        assert_eq!(flight.missiles[0].side_index, 1);
        assert_eq!(flight.missiles[0].progress, 0.0);
        assert!(flight.missiles[0].trail.is_empty());

        let split = uninterrupted.checkpoint_state().unwrap();
        let mut resumed = NativeRuntime::new(config, runtime_checkpoint(split)).unwrap();
        let expected = uninterrupted.step().unwrap();
        let actual = resumed.step().unwrap();
        assert_eq!(
            actual.strategic_missile_snapshot,
            expected.strategic_missile_snapshot
        );
        assert_eq!(actual.gameplay_rng_state, expected.gameplay_rng_state);
        assert_eq!(actual.frame_snapshot, expected.frame_snapshot);
    }

    #[test]
    fn invalid_staged_missile_tick_rolls_back_simulation_rng_and_publication() {
        let mut runtime = with_test_missile(fixture(0, false), 0.25);
        runtime.strategic_missiles.as_mut().unwrap().missiles[0].progress = f64::NAN;
        let units_before = runtime.simulation.units.clone();
        let rng_before = runtime.gameplay_rng.state();
        let publication_before = runtime.latest_snapshot();

        assert!(matches!(
            runtime.step(),
            Err(RuntimeError::StrategicMissile(
                StrategicMissileError::InvalidState("missile")
            ))
        ));
        assert_eq!(runtime.simulation.units, units_before);
        assert_eq!(runtime.gameplay_rng.state(), rng_before);
        assert!(Arc::ptr_eq(&runtime.latest_snapshot(), &publication_before));
        assert!(
            runtime.strategic_missiles.as_ref().unwrap().missiles[0]
                .progress
                .is_nan()
        );
    }

    fn with_live_battlefield(
        mut runtime: NativeRuntime,
        terrain_intensity: Vec<f32>,
    ) -> NativeRuntime {
        let config = runtime.config;
        let mut captured = runtime.checkpoint_state().unwrap();
        let countries = captured
            .territory_config
            .country_to_side
            .keys()
            .copied()
            .map(|country| (country, CountryBattlefieldPrimitives::default()))
            .collect();
        let units = captured
            .units
            .iter()
            .map(|unit| {
                (
                    unit.combat.id,
                    BattlefieldUnitState {
                        cohesion_seed: (unit.combat.id % 4) as f64 / 1_000.0,
                        ..BattlefieldUnitState::default()
                    },
                )
            })
            .collect();
        captured.battlefield = Some(BattlefieldRuntimeState {
            config: BattlefieldConfig::default(),
            mountains_enabled: true,
            terrain_intensity,
            urban_centers: Vec::new(),
            countries,
            units,
        });
        NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap()
    }

    #[test]
    fn unit_visual_publication_is_enriched_and_immutable_across_continuation() {
        let mut base = fixture(0, false);
        let mut captured = base.checkpoint_state().unwrap();
        captured.units[0].combat.armor_supported = true;
        let countries = captured
            .territory_config
            .country_to_side
            .keys()
            .copied()
            .map(|country| (country, CountryBattlefieldPrimitives::default()))
            .collect();
        let mut units = captured
            .units
            .iter()
            .map(|unit| (unit.combat.id, BattlefieldUnitState::default()))
            .collect::<BTreeMap<_, _>>();
        units.get_mut(&1).unwrap().is_alpenjager = true;
        units.get_mut(&1).unwrap().encircled_ticks = 61;
        captured.battlefield = Some(BattlefieldRuntimeState {
            config: BattlefieldConfig {
                encirclement_radius: 90.0,
                ..BattlefieldConfig::default()
            },
            mountains_enabled: true,
            terrain_intensity: vec![0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
            urban_centers: Vec::new(),
            countries,
            units,
        });

        let mut runtime = NativeRuntime::new(base.config, runtime_checkpoint(captured)).unwrap();
        let initial = runtime.latest_snapshot();
        let unit = initial
            .frame_snapshot
            .units
            .iter()
            .find(|unit| unit.id == 1)
            .unwrap();
        assert!(unit.armor_supported);
        assert!(unit.is_alpenjager);
        assert_eq!(unit.encircled_ticks, 61);
        assert_eq!(unit.mountain_intensity, 0.5);

        let next = runtime.step().unwrap();
        let next_unit = next
            .frame_snapshot
            .units
            .iter()
            .find(|unit| unit.id == 1)
            .unwrap();
        assert!(next_unit.is_alpenjager);
        assert_eq!(next_unit.mountain_intensity, 0.5);
        assert_eq!(initial.frame_snapshot.units[0].encircled_ticks, 61);
        assert_eq!(initial.frame_snapshot.units[0].mountain_intensity, 0.5);

        let state = runtime.checkpoint_state().unwrap();
        let resumed = NativeRuntime::new(runtime.config, runtime_checkpoint(state)).unwrap();
        let resumed_snapshot = resumed.latest_snapshot();
        for resumed_unit in resumed_snapshot.frame_snapshot.units.iter() {
            let continued_unit = next
                .frame_snapshot
                .units
                .iter()
                .find(|unit| unit.id == resumed_unit.id)
                .unwrap();
            assert_eq!(resumed_unit.armor_supported, continued_unit.armor_supported);
            assert_eq!(resumed_unit.is_alpenjager, continued_unit.is_alpenjager);
            assert_eq!(resumed_unit.encircled_ticks, continued_unit.encircled_ticks);
            assert!(resumed_unit.mountain_intensity.is_finite());
            assert!((0.0..=1.0).contains(&resumed_unit.mountain_intensity));
        }
    }

    #[test]
    fn legacy_runtime_unit_visuals_use_explicit_defaults() {
        let runtime = fixture(0, false);
        assert!(
            runtime
                .latest_snapshot()
                .frame_snapshot
                .units
                .iter()
                .all(|unit| {
                    !unit.is_alpenjager
                        && unit.encircled_ticks == 0
                        && unit.mountain_intensity == 0.0
                })
        );
    }

    fn enable_side_dynamics(runtime: &mut NativeRuntime) {
        runtime.side_dynamics = Some(crate::dynamics::bootstrap_sides(
            runtime.territory.max_sides(),
            runtime.simulation.units.iter().map(|unit| {
                (
                    unit.combat.side as usize,
                    if unit.combat.kind == UnitKind::Armor {
                        unit.combat.equipment.saturating_mul(2) as f64
                    } else {
                        unit.combat.personnel as f64
                    },
                )
            }),
        ));
    }

    fn with_operational_state(mut runtime: NativeRuntime) -> NativeRuntime {
        let config = runtime.config;
        let mut captured = runtime.checkpoint_state().unwrap();
        let strengths = (0..captured.territory_config.max_sides)
            .map(|side| {
                captured
                    .units
                    .iter()
                    .filter(|unit| unit.combat.side as usize == side)
                    .map(|unit| formation_strength(&unit.combat))
                    .sum()
            })
            .collect::<Vec<_>>();
        let mut operations = OperationalRuntimeState::bootstrap(
            captured.territory_config.max_sides,
            &captured.diplomacy.hostility,
            &strengths,
        );
        operations.country_desperation = vec![
            CountryDesperationState {
                country_id: 1,
                mode: CountryDesperationMode::LastStand,
                initial_cities: Some(1),
                initial_manpower: Some(1_000.0),
                previous_controlled: Some(4),
                stall_ticks: 0,
            },
            CountryDesperationState {
                country_id: 2,
                mode: CountryDesperationMode::Normal,
                initial_cities: Some(1),
                initial_manpower: Some(1_000.0),
                previous_controlled: Some(4),
                stall_ticks: 0,
            },
        ];
        operations.task_forces.push(OperationalTaskForce {
            id: "force-a".to_owned(),
            signature: "force-a".to_owned(),
            side_index: 0,
            plan_signature: "plan-a".to_owned(),
            plan_type: "FRONTLINE_PUSH".to_owned(),
            theater_id: None,
            target: Some(OperationalPoint {
                lat: -90.0,
                lng: 0.0,
            }),
            staging_anchor: Some(OperationalPoint {
                lat: -90.0,
                lng: 0.0,
            }),
            route: vec![OperationalPoint {
                lat: -90.0,
                lng: 0.0,
            }],
            phase: TaskForcePhase::Attacking,
            posture: TaskForcePosture::Balanced,
            members: vec![TaskForceMember {
                unit_id: 1,
                role: TaskForceRole::Spearhead,
                assigned_tick: captured.tick,
                route_progress: 0.2,
            }],
            reserve_unit_ids: Vec::new(),
            desired_power: 1.0,
            launch_power: 1.0,
            current_power: 1.0,
            peak_power: 1.0,
            readiness: 1.0,
            max_assigned_units: 1,
            created_tick: captured.tick,
            phase_started_tick: captured.tick,
            last_progress_tick: captured.tick,
            last_recovery_tick: captured.tick,
            recovery_power: 0.0,
            progress: 0.2,
            withdrawal_anchor: None,
            completion_reason: None,
            outcome: None,
            severe_surprise: false,
            parent_task_force_id: None,
            supply_invalidated_tick: None,
            intent_revision: 0,
        });
        operations.ingest_tactical_contacts(
            captured.tick,
            &[TacticalContactObservation {
                observer_side: 0,
                enemy_side: 1,
                target_unit_id: 2,
                target_country_id: 2,
                target_position: OperationalPoint {
                    lat: -90.0,
                    lng: 90.0,
                },
                observed_power: strengths[1],
                kind: "army".to_owned(),
            }],
        );
        captured.operations = Some(operations);
        NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap()
    }

    fn with_air_and_execution(mut runtime: NativeRuntime) -> NativeRuntime {
        let config = runtime.config;
        let mut captured = runtime.checkpoint_state().unwrap();
        captured
            .units
            .iter_mut()
            .find(|unit| unit.combat.id == 2)
            .unwrap()
            .combat
            .lng = -170.0;
        captured.operational_execution = Some(OperationalExecutionState::default());
        let mut air_power = AirPowerState::new(
            vec![Airfield {
                id: 1,
                side: 0,
                owner_country_id: 1,
                controller_country_id: 1,
                lat: -90.0,
                lng: -170.0,
                capacity: 4,
                health: 100.0,
                disabled: false,
                capture_repair_cycles: 0,
                capital: true,
            }],
            vec![AirWing {
                id: 1,
                side: 0,
                sovereign_country_id: 1,
                airfield_id: 1,
                return_airfield_id: None,
                role: AirRole::Strike,
                quality: 50.0,
                max_count: 100,
                count: 100,
                lat: -90.0,
                lng: -170.0,
                state: AirWingState::Grounded,
                target_kind: None,
                target_id: None,
                rearm_ticks: 0,
                cooldown_ticks: 0,
                endurance_ticks: 0,
                next_mission_tick: None,
                force_mission: true,
            }],
        )
        .unwrap();
        air_power.country_coverage.push(AirCountryCoverage {
            country_id: 2,
            operations_coverage: 1.0,
        });
        air_power.validate().unwrap();
        captured.air_power = Some(air_power);
        NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap()
    }

    #[test]
    fn recruitment_consumes_reserve_rng_and_treasury_in_browser_order() {
        let runtime = fixture(0, false);
        let mut air_power = AirPowerState::empty();
        air_power.country_coverage = vec![
            AirCountryCoverage {
                country_id: 1,
                operations_coverage: 1.0,
            },
            AirCountryCoverage {
                country_id: 2,
                operations_coverage: 1.0,
            },
        ];
        let mut reinforcement =
            ReinforcementState::bootstrap(&air_power, 3, 1, runtime.territory.country_to_side(), 2)
                .unwrap();
        let mut reserves = BTreeMap::from([(0, 1_000.0), (1, 0.0)]);
        let mut rng = GameplayRng::new(7);

        let staged = stage_recruitment(
            1,
            1,
            &runtime.simulation,
            &runtime.scenario,
            &runtime.territory,
            &runtime.latest.territory_snapshot,
            &runtime.strategic,
            None,
            &mut reinforcement,
            &mut reserves,
            &mut rng,
            runtime.config.production,
            None,
        )
        .unwrap();

        assert_eq!(staged.counters.recruited_units, 1);
        assert_eq!(staged.counters.recruited_personnel, 1_000);
        assert_eq!(staged.treasury_costs, vec![(1, RECRUITMENT_COST)]);
        assert_eq!(staged.units[0].combat.id, 3);
        assert_eq!(staged.units[0].combat.sovereign, 1);
        assert_eq!(staged.policies[0].ai.deploy_until_tick, 31);
        assert!(
            staged.policies[0]
                .influence
                .as_ref()
                .unwrap()
                .browser_temporal_seed
                .is_some()
        );
        assert_eq!(reinforcement.next_unit_id, 4);
        assert_eq!(reserves[&0], 0.0);
        // Chance, fallback-cell selection, and browser identity seed. No mountain draw exists.
        assert_eq!(
            rng.state().state,
            7_u32.wrapping_add(0x6d2b_79f5_u32.wrapping_mul(3))
        );
    }

    #[test]
    fn pay_cycle_reinforcement_snapshot_and_next_tick_resume_exactly() {
        let mut uninterrupted = with_air_and_execution(fixture(599, false));
        let pay_snapshot = uninterrupted.step().unwrap();
        let old_reinforcement = pay_snapshot.reinforcement_snapshot.clone().unwrap();
        assert_eq!(old_reinforcement.countries[0].air_operations_due, 1.5);
        assert!(old_reinforcement.countries[0].operations_coverage > 0.0);

        let split = uninterrupted.checkpoint_state().unwrap();
        assert_eq!(split.reinforcement.as_ref(), Some(&*old_reinforcement));
        let mut restored =
            NativeRuntime::new(split.runtime_config, runtime_checkpoint(split)).unwrap();
        let expected = uninterrupted.step().unwrap();
        let actual = restored.step().unwrap();

        assert_eq!(actual.frame_snapshot, expected.frame_snapshot);
        assert_eq!(actual.air_power_snapshot, expected.air_power_snapshot);
        assert_eq!(
            actual.reinforcement_snapshot,
            expected.reinforcement_snapshot
        );
        assert_eq!(actual.personnel_reserves, expected.personnel_reserves);
        assert_eq!(actual.gameplay_rng_state, expected.gameplay_rng_state);
        assert_eq!(
            actual.material_logistics_snapshot,
            expected.material_logistics_snapshot
        );
        let expected_state = uninterrupted.checkpoint_state().unwrap();
        let actual_state = restored.checkpoint_state().unwrap();
        assert_eq!(actual_state.strategic_cycle, expected_state.strategic_cycle);
        assert_eq!(actual_state.economies, expected_state.economies);
        assert_eq!(actual_state.occupations, expected_state.occupations);
        assert_eq!(
            actual_state.material_logistics,
            expected_state.material_logistics
        );
        assert_eq!(old_reinforcement.countries[0].air_operations_due, 1.5);
    }

    #[test]
    fn recruited_units_publish_immutable_renderer_snapshots_and_checkpoint_cursor() {
        let mut runtime = with_air_and_execution(fixture(0, false));
        runtime.personnel_reserves.insert(0, 1_000.0);
        runtime.gameplay_rng = GameplayRng::new(7);
        let before = runtime.latest_snapshot();

        let recruited = runtime.step().unwrap();
        assert_eq!(recruited.counters.reinforcement.recruited_units, 1);
        assert_eq!(recruited.counters.reinforcement.recruited_personnel, 1_000);
        assert_eq!(before.frame_snapshot.units.len(), 2);
        assert_eq!(recruited.frame_snapshot.units.len(), 3);
        assert_eq!(recruited.frame_snapshot.units[2].id, 3);
        assert_eq!(recruited.personnel_reserves[&0], 0.0);
        assert_eq!(
            recruited
                .reinforcement_snapshot
                .as_ref()
                .unwrap()
                .next_unit_id,
            4
        );

        let checkpoint = runtime.checkpoint_state().unwrap();
        assert_eq!(checkpoint.units.last().unwrap().combat.id, 3);
        assert_eq!(checkpoint.reinforcement.unwrap().next_unit_id, 4);
        assert_eq!(before.frame_snapshot.units.len(), 2);
    }

    #[test]
    fn failed_preplanning_tick_preserves_reinforcement_transaction_and_publication() {
        let mut runtime = with_air_and_execution(fixture(0, false));
        runtime.personnel_reserves.insert(0, 1_000.0);
        runtime.gameplay_rng = GameplayRng::new(7);
        let published = runtime.latest_snapshot();
        let strategic_cycle = runtime.strategic.cycle();
        let economies = runtime.strategic.economies().clone();
        let occupations = runtime.strategic.occupations().clone();
        let air_power = runtime.air_power.clone();
        let reinforcement = runtime.reinforcement.clone();
        let reserves = runtime.personnel_reserves.clone();
        let rng = runtime.gameplay_rng.state();
        runtime.config.front.max_grid_cells = 0;

        assert!(matches!(runtime.step(), Err(RuntimeError::Front(_))));
        assert_eq!(runtime.strategic.cycle(), strategic_cycle);
        assert_eq!(runtime.strategic.economies(), &economies);
        assert_eq!(runtime.strategic.occupations(), &occupations);
        assert_eq!(runtime.air_power, air_power);
        assert_eq!(runtime.reinforcement, reinforcement);
        assert_eq!(runtime.personnel_reserves, reserves);
        assert_eq!(runtime.gameplay_rng.state(), rng);
        assert!(Arc::ptr_eq(&runtime.latest_snapshot(), &published));
    }

    #[test]
    fn air_target_strength_matches_browser_candidate_fields() {
        let army = unit(1, 0, 1, 0.0);
        let mut armor = unit(2, 1, 2, 1.0);
        armor.combat.kind = UnitKind::Armor;
        armor.combat.personnel = 0;
        armor.combat.personnel_capacity = 0;
        armor.combat.equipment = 87;
        armor.combat.max_equipment = 100;
        let simulation = Simulation::new(SimulationConfig::default(), vec![army, armor]).unwrap();

        let targets = air_unit_targets(&simulation).unwrap();
        assert_eq!(targets[0].kind, AirTargetKind::Army);
        assert_eq!(targets[0].strength, 1.0);
        assert_eq!(targets[1].kind, AirTargetKind::Armor);
        assert_eq!(targets[1].strength, 87.0);
    }

    #[test]
    fn air_priority_areas_match_browser_active_task_force_phases() {
        assert!(air_priority_areas(None).is_empty());
        let runtime = with_operational_state(fixture(0, false));
        let mut operations = runtime.operations.unwrap();
        assert_eq!(air_priority_areas(Some(&operations)).len(), 1);

        operations.task_forces[0].phase = TaskForcePhase::Culminated;
        assert!(air_priority_areas(Some(&operations)).is_empty());
        let (side_index, staging) = {
            let task_force = &mut operations.task_forces[0];
            task_force.phase = TaskForcePhase::Assembling;
            task_force.target = None;
            (task_force.side_index, task_force.staging_anchor.unwrap())
        };
        assert_eq!(
            air_priority_areas(Some(&operations)),
            vec![AirPriorityArea::with_default_radius(
                side_index,
                staging.lat,
                staging.lng,
            )]
        );
    }

    #[test]
    fn airfield_controller_updates_use_browser_phase_cadence() {
        assert!(!airfield_controller_update_due(46));
        assert!(airfield_controller_update_due(47));
        assert!(!airfield_controller_update_due(48));
        assert!(airfield_controller_update_due(167));
    }

    #[test]
    fn side_dynamics_stage_feeds_same_tick_battlefield_and_ai_policy() {
        let mut runtime = with_live_battlefield(fixture(36, false), vec![0.0; 8]);
        enable_side_dynamics(&mut runtime);
        let dynamics = runtime.side_dynamics.as_mut().unwrap();
        let side_zero = dynamics.get_mut(&0).unwrap();
        side_zero.sample(0, 4);
        side_zero.sample(1, 4);
        side_zero.current_personnel = side_zero.initial_personnel * 0.09;

        runtime.step().unwrap();

        let dynamics = runtime.side_dynamics.as_ref().unwrap();
        let side = &dynamics[&0];
        assert_eq!(side.momentum_samples.len(), 3);
        assert_eq!(side.momentum_samples.back().unwrap().frame, 36);
        assert_eq!(side.momentum_samples.back().unwrap().controlled, 4);
        assert_eq!(side.phase, WarPhase::Collapsing);
        assert_eq!(
            runtime.unit_policies[&1].ai.combat.dealt_multiplier,
            0.25 * 0.7
        );

        let mut defensive = dynamics.clone();
        defensive.get_mut(&0).unwrap().posture = WarPosture::Defensive;
        let inputs = ai_units(
            &runtime.simulation,
            &runtime.unit_policies,
            &runtime.prior_objective_by_unit,
            &BTreeSet::new(),
            runtime.tick + 1,
            None,
            Some(&defensive),
        )
        .unwrap();
        let side_zero = inputs
            .iter()
            .find(|unit| usize::from(unit.side) == 0)
            .unwrap();
        assert!(side_zero.defensive_only);
        assert!(!runtime.unit_policies[&side_zero.id].command.refuses_offense);
    }

    #[test]
    fn failed_tick_does_not_advance_side_dynamics() {
        let mut runtime = fixture(36, false);
        enable_side_dynamics(&mut runtime);
        runtime
            .side_dynamics
            .as_mut()
            .unwrap()
            .get_mut(&0)
            .unwrap()
            .sample(0, 4);
        runtime
            .side_dynamics
            .as_mut()
            .unwrap()
            .get_mut(&0)
            .unwrap()
            .sample(1, 4);
        let before = runtime.side_dynamics.clone();
        runtime
            .unit_policies
            .get_mut(&1)
            .unwrap()
            .influence
            .as_mut()
            .unwrap()
            .radius = f64::NAN;

        assert!(runtime.step().is_err());
        assert_eq!(runtime.side_dynamics, before);
        assert_eq!(runtime.tick(), 36);
        assert_eq!(runtime.frame(), 36);
    }

    #[test]
    fn inactive_side_resets_posture_without_losing_history_or_override() {
        let mut runtime = fixture(36, false);
        enable_side_dynamics(&mut runtime);
        let retired = runtime.side_dynamics.as_mut().unwrap().get_mut(&1).unwrap();
        retired.sample(1, 2);
        retired.posture = WarPosture::Defensive;
        retired.posture_override = Some(WarPosture::Offensive);
        runtime.diplomacy.active_sides = vec![0];

        let staged = stage_side_dynamics(
            &runtime.side_dynamics,
            37,
            runtime.frame,
            &runtime.latest.territory_snapshot,
            runtime.territory.country_to_side(),
            &runtime.diplomacy.active_sides,
            &runtime.diplomacy.hostility,
            runtime.territory.max_sides(),
            &runtime.simulation,
            &runtime.unit_policies,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(staged[&1].posture, WarPosture::Balanced);
        assert_eq!(staged[&1].momentum_samples.len(), 1);
        assert_eq!(staged[&1].posture_override, Some(WarPosture::Offensive));
    }

    #[test]
    fn side_dynamics_continue_exactly_across_checkpoint_split() {
        let mut uninterrupted = with_live_battlefield(fixture(36, false), vec![0.0; 8]);
        enable_side_dynamics(&mut uninterrupted);
        for controlled in [4, 4] {
            uninterrupted
                .side_dynamics
                .as_mut()
                .unwrap()
                .get_mut(&0)
                .unwrap()
                .sample(uninterrupted.frame, controlled);
        }
        uninterrupted.step().unwrap();
        let config = uninterrupted.config;
        let captured = uninterrupted.checkpoint_state().unwrap();
        let mut resumed = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();

        let expected = uninterrupted.step().unwrap();
        let actual = resumed.step().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(resumed.side_dynamics, uninterrupted.side_dynamics);
    }

    #[test]
    fn operational_state_and_renderer_snapshot_continue_exactly_across_split() {
        let mut uninterrupted =
            with_operational_state(with_live_battlefield(fixture(0, false), vec![0.0; 8]));
        let initial = uninterrupted.latest_snapshot();
        let initial_operations = initial.operational_snapshot.clone().unwrap();
        uninterrupted.simulation.units[0].combat.lat = -89.0;
        uninterrupted
            .unit_policies
            .get_mut(&1)
            .unwrap()
            .influence
            .as_mut()
            .unwrap()
            .delta = 0.0;
        uninterrupted
            .battlefield
            .as_mut()
            .unwrap()
            .config
            .encirclement_radius = 90.0;

        uninterrupted.step().unwrap();
        let config = uninterrupted.config;
        let captured = uninterrupted.checkpoint_state().unwrap();
        assert_eq!(
            captured
                .battlefield
                .as_ref()
                .unwrap()
                .units
                .get(&1)
                .unwrap()
                .supply_collapsed_tick,
            Some(1)
        );
        let mut resumed = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();

        let expected = uninterrupted.step().unwrap();
        let actual = resumed.step().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(resumed.operations, uninterrupted.operations);
        assert_eq!(resumed.battlefield, uninterrupted.battlefield);
        assert_eq!(actual.operational_snapshot, expected.operational_snapshot);
        assert!(!Arc::ptr_eq(
            actual.operational_snapshot.as_ref().unwrap(),
            &initial_operations
        ));
        assert_eq!(initial_operations.tick, 0);
        assert_eq!(
            initial_operations.task_forces[0].phase,
            TaskForcePhase::Attacking
        );
    }

    #[test]
    fn air_execution_damage_and_state_continue_exactly_across_checkpoint_split() {
        let mut uninterrupted = with_air_and_execution(fixture(5, false));
        let config = uninterrupted.config;
        let captured = uninterrupted.checkpoint_state().unwrap();
        let mut resumed = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();

        let expected = uninterrupted.step().unwrap();
        let actual = resumed.step().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(resumed.air_power, uninterrupted.air_power);
        assert_eq!(
            resumed.operational_execution,
            uninterrupted.operational_execution
        );
        assert_eq!(actual.counters.air.strikes_completed, 1);
        assert_eq!(actual.counters.air.damaged_land_units, 1);
        assert!(actual.counters.air.personnel_loss > 0);
        assert_eq!(actual.casualty_totals.get(&2).copied(), Some(100.0));
        assert_eq!(
            actual
                .casualties_by_victim
                .get(&2)
                .and_then(|row| row.get(&1)),
            Some(&100.0)
        );
        assert!(actual.air_power_snapshot.is_some());
        assert!(actual.operational_execution_snapshot.is_some());
    }

    #[test]
    fn persisted_air_coverage_controls_policy_across_checkpoint_split() {
        let mut uninterrupted = with_air_and_execution(fixture(5, false));
        uninterrupted.air_power.as_mut().unwrap().country_coverage[0].operations_coverage = 0.5;
        let config = uninterrupted.config;
        let captured = uninterrupted.checkpoint_state().unwrap();
        assert_eq!(
            captured.air_power.as_ref().unwrap().country_coverage[0].operations_coverage,
            0.5
        );
        let mut resumed = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();

        let expected = uninterrupted.step().unwrap();
        let actual = resumed.step().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.counters.air.strikes_completed, 0);
        assert_eq!(actual.counters.air.damaged_land_units, 0);
        assert_eq!(
            resumed.air_power.as_ref().unwrap().country_coverage[0].operations_coverage,
            0.5
        );
    }

    #[test]
    fn capitulation_overrides_persisted_air_coverage() {
        let mut runtime = with_air_and_execution(fixture(5, false));
        let mut air_power = runtime.air_power.clone().unwrap();
        air_power.country_coverage[0].operations_coverage = 0.5;
        let (_, funded) = air_country_policy(&runtime.strategic, &air_power);
        assert_eq!(funded.get(&1), Some(&0.5));

        let mut captured = runtime.checkpoint_state().unwrap();
        captured
            .economies
            .iter_mut()
            .find(|economy| economy.country_id == 1)
            .unwrap()
            .capitulated = true;
        let strategic = StrategicSimulation::restore(
            captured.strategic_cycle,
            captured.economies,
            captured.occupations,
        )
        .unwrap();
        let (_, grounded) = air_country_policy(&strategic, &air_power);
        assert_eq!(grounded.get(&1), Some(&0.0));
    }

    #[test]
    fn air_coverage_country_must_exist_in_stable_topology() {
        let mut runtime = fixture(0, false);
        let config = runtime.config;
        let mut captured = runtime.checkpoint_state().unwrap();
        captured.operational_execution = Some(OperationalExecutionState::default());
        let mut air_power = AirPowerState::empty();
        air_power.country_coverage.push(AirCountryCoverage {
            country_id: 99,
            operations_coverage: 0.5,
        });
        captured.air_power = Some(air_power);

        let result = NativeRuntime::new(config, runtime_checkpoint(captured));
        assert!(matches!(
            result,
            Err(RuntimeError::InvalidCheckpoint(
                "air power disagrees with the stable country and side topology"
            ))
        ));
    }

    #[test]
    fn air_coverage_must_include_no_wing_topology_countries() {
        let mut runtime = fixture(0, false);
        let config = runtime.config;
        let mut captured = runtime.checkpoint_state().unwrap();
        captured.operational_execution = Some(OperationalExecutionState::default());
        let mut air_power = AirPowerState::empty();
        air_power.country_coverage.push(AirCountryCoverage {
            country_id: 1,
            operations_coverage: 1.0,
        });
        captured.air_power = Some(air_power);

        let result = NativeRuntime::new(config, runtime_checkpoint(captured));
        assert!(matches!(
            result,
            Err(RuntimeError::InvalidCheckpoint(
                "air power disagrees with the stable country and side topology"
            ))
        ));
    }

    #[test]
    fn aircraft_losses_charge_aircrew_to_totals_attribution_and_side_dynamics() {
        let mut runtime = fixture(5, false);
        enable_side_dynamics(&mut runtime);
        let config = runtime.config;
        let mut captured = runtime.checkpoint_state().unwrap();
        captured.operational_execution = Some(OperationalExecutionState::default());
        captured.air_power = Some(
            AirPowerState::new(
                vec![
                    Airfield {
                        id: 10,
                        side: 0,
                        owner_country_id: 1,
                        controller_country_id: 1,
                        lat: 0.0,
                        lng: 0.0,
                        capacity: 3,
                        health: 100.0,
                        disabled: false,
                        capture_repair_cycles: 0,
                        capital: true,
                    },
                    Airfield {
                        id: 20,
                        side: 1,
                        owner_country_id: 2,
                        controller_country_id: 2,
                        lat: 0.0,
                        lng: 0.05,
                        capacity: 3,
                        health: 100.0,
                        disabled: false,
                        capture_repair_cycles: 0,
                        capital: true,
                    },
                ],
                vec![
                    AirWing {
                        id: 1,
                        side: 0,
                        sovereign_country_id: 1,
                        airfield_id: 10,
                        return_airfield_id: None,
                        role: AirRole::Fighter,
                        quality: 50.0,
                        max_count: 24,
                        count: 24,
                        lat: 0.0,
                        lng: 0.0,
                        state: AirWingState::Patrol,
                        target_kind: None,
                        target_id: None,
                        rearm_ticks: 0,
                        cooldown_ticks: 0,
                        endurance_ticks: 0,
                        next_mission_tick: None,
                        force_mission: true,
                    },
                    AirWing {
                        id: 2,
                        side: 1,
                        sovereign_country_id: 2,
                        airfield_id: 20,
                        return_airfield_id: None,
                        role: AirRole::Fighter,
                        quality: 50.0,
                        max_count: 24,
                        count: 24,
                        lat: 0.0,
                        lng: 0.05,
                        state: AirWingState::Patrol,
                        target_kind: None,
                        target_id: None,
                        rearm_ticks: 0,
                        cooldown_ticks: 0,
                        endurance_ticks: 0,
                        next_mission_tick: Some(120),
                        force_mission: false,
                    },
                ],
            )
            .unwrap(),
        );
        runtime = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();
        let before_side_zero = runtime.side_dynamics.as_ref().unwrap()[&0].current_personnel;
        let before_side_one = runtime.side_dynamics.as_ref().unwrap()[&1].current_personnel;

        let snapshot = runtime.step().unwrap();
        assert_eq!(snapshot.counters.air.interceptions_completed, 1);
        assert_eq!(snapshot.counters.air.aircraft_loss, 3);
        assert_eq!(snapshot.counters.air.personnel_loss, 3);
        assert_eq!(snapshot.counters.air.damaged_land_units, 0);
        assert_eq!(snapshot.casualty_totals.get(&1), Some(&1.0));
        assert_eq!(snapshot.casualty_totals.get(&2), Some(&2.0));
        assert_eq!(
            snapshot
                .casualties_by_victim
                .get(&1)
                .and_then(|row| row.get(&2)),
            Some(&1.0)
        );
        assert_eq!(
            snapshot
                .casualties_by_victim
                .get(&2)
                .and_then(|row| row.get(&1)),
            Some(&2.0)
        );
        assert_eq!(
            runtime.side_dynamics.as_ref().unwrap()[&0].current_personnel,
            before_side_zero - 1.0
        );
        assert_eq!(
            runtime.side_dynamics.as_ref().unwrap()[&1].current_personnel,
            before_side_one - 2.0
        );
    }

    #[test]
    fn landing_threat_counts_every_live_attacker_at_the_beachhead() {
        let runtime = fixture(0, false);
        let target = ExecutionPoint {
            lat: -90.0,
            lng: 0.0,
        };
        let mut execution = OperationalExecutionState::default();
        execution.naval_operations.push(NavalOperation {
            id: "invasion-a".to_owned(),
            signature: "invasion-a".to_owned(),
            kind: NavalOperationKind::Invasion,
            phase: NavalOperationPhase::Landing,
            side: 0,
            country: 1,
            enemy_side: Some(1),
            max_assigned_units: 1,
            members: vec![NavalMember {
                unit_id: 1,
                role: "INVASION".to_owned(),
                assigned_tick: 0,
            }],
            staging: target,
            target,
            route: Vec::new(),
            route_index: 0,
            progress: 0.75,
            started_tick: 0,
            phase_started_tick: 0,
            last_progress_tick: 0,
            completion_reason: None,
        });
        let input = |unit_id, side, lng, at_sea| ExecutionUnitInput {
            unit_id,
            side,
            country: (side + 1) as u16,
            position: ExecutionPoint { lat: -90.0, lng },
            transport: false,
            at_sea,
            deploying: false,
            engaged: false,
            operationally_assigned: false,
        };
        let units = [
            input(1, 0, 0.0, false),
            input(3, 0, 0.5, false),
            input(4, 0, 1.5, false),
            input(5, 0, 0.0, true),
            input(6, 1, 0.0, false),
        ];

        let threats = defender_threats(
            &execution,
            None,
            &units,
            &runtime.territory,
            &runtime.diplomacy.hostility,
            runtime.territory.max_sides(),
        )
        .unwrap();

        assert_eq!(threats.len(), 1);
        assert_eq!(threats[0].phase, DefenderThreatPhase::Landing);
        assert_eq!(threats[0].enemy_force, 4);
    }

    #[test]
    fn fast_transport_execution_owns_transport_and_movement_before_publication() {
        let mut runtime = fixture(0, false);
        let config = runtime.config;
        let mut captured = runtime.checkpoint_state().unwrap();
        let mut execution = OperationalExecutionState::default();
        execution.naval_operations.push(NavalOperation {
            id: "transport-a".to_owned(),
            signature: "transport-a".to_owned(),
            kind: NavalOperationKind::FastTransport,
            phase: NavalOperationPhase::Transit,
            side: 0,
            country: 1,
            enemy_side: None,
            max_assigned_units: 1,
            members: vec![NavalMember {
                unit_id: 1,
                role: "TRANSPORT".to_owned(),
                assigned_tick: 0,
            }],
            staging: ExecutionPoint {
                lat: -90.0,
                lng: 0.0,
            },
            target: ExecutionPoint {
                lat: -89.0,
                lng: 10.0,
            },
            route: Vec::new(),
            route_index: 0,
            progress: 0.0,
            started_tick: 0,
            phase_started_tick: 0,
            last_progress_tick: 0,
            completion_reason: None,
        });
        captured.operational_execution = Some(execution);
        let mut air_power = AirPowerState::empty();
        air_power.country_coverage = vec![
            AirCountryCoverage {
                country_id: 1,
                operations_coverage: 1.0,
            },
            AirCountryCoverage {
                country_id: 2,
                operations_coverage: 1.0,
            },
        ];
        captured.air_power = Some(air_power);
        runtime = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();
        let before = runtime.latest_snapshot();

        let after = runtime.step().unwrap();
        let unit = after
            .frame_snapshot
            .units
            .iter()
            .find(|unit| unit.id == 1)
            .unwrap();
        assert!(unit.transport);
        assert!(unit.lat > -90.0);
        assert_eq!(after.counters.operational_execution.transport_updates, 1);
        assert_eq!(after.counters.operational_execution.steering_orders, 1);
        assert!(!before.frame_snapshot.units[0].transport);
    }

    #[test]
    fn new_defender_recruits_are_replanned_out_of_front_slots_in_the_same_tick() {
        let mut runtime = fixture(0, false);
        let config = runtime.config;
        let mut captured = runtime.checkpoint_state().unwrap();
        for id in 3..=21 {
            captured
                .units
                .push(unit(id, 1, 2, 4.0 + (id - 3) as f64 * 0.05));
            captured
                .unit_policies
                .push(RuntimeUnitPolicy::standard(id, 2));
        }
        let mut execution = OperationalExecutionState::default();
        execution.naval_operations.push(NavalOperation {
            id: "invasion-a".to_owned(),
            signature: "invasion-a".to_owned(),
            kind: NavalOperationKind::Invasion,
            phase: NavalOperationPhase::Transit,
            side: 0,
            country: 1,
            enemy_side: Some(1),
            max_assigned_units: 1,
            members: vec![NavalMember {
                unit_id: 1,
                role: "INVASION".to_owned(),
                assigned_tick: 0,
            }],
            staging: ExecutionPoint {
                lat: -90.0,
                lng: 0.0,
            },
            target: ExecutionPoint {
                lat: -90.0,
                lng: 0.0,
            },
            route: Vec::new(),
            route_index: 0,
            progress: 0.45,
            started_tick: 0,
            phase_started_tick: 0,
            last_progress_tick: 0,
            completion_reason: None,
        });
        captured.operational_execution = Some(execution);
        let mut air_power = AirPowerState::empty();
        air_power.country_coverage = vec![
            AirCountryCoverage {
                country_id: 1,
                operations_coverage: 1.0,
            },
            AirCountryCoverage {
                country_id: 2,
                operations_coverage: 1.0,
            },
        ];
        captured.air_power = Some(air_power);
        let mut runtime = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();

        let snapshot = runtime.step().unwrap();
        let reaction = &snapshot
            .operational_execution_snapshot
            .as_ref()
            .unwrap()
            .defender_reactions[0];
        assert_eq!(reaction.unit_ids, vec![3, 4, 5]);
        assert_eq!(
            snapshot
                .counters
                .operational_execution
                .defender_units_recruited,
            3
        );
        assert_eq!(
            snapshot.counters.ai.sticky_assignments
                + snapshot.counters.ai.front_assignments
                + snapshot.counters.ai.reinforcement_assignments,
            17
        );
        for unit_id in &reaction.unit_ids {
            assert!(!runtime.prior_objective_by_unit.contains_key(unit_id));
            assert!(!runtime.front_prior_by_unit.contains_key(unit_id));
        }
    }

    #[test]
    fn failed_staged_tick_does_not_advance_operational_state_or_publication() {
        let mut runtime =
            with_operational_state(with_live_battlefield(fixture(0, false), vec![0.0; 8]));
        let published = runtime.latest_snapshot();
        let operations = runtime.operations.clone();
        let queued = runtime.pending_render_updates();
        runtime.config.front.max_grid_cells = 0;

        assert!(matches!(runtime.step(), Err(RuntimeError::Front(_))));
        assert_eq!(runtime.operations, operations);
        assert!(Arc::ptr_eq(&runtime.latest_snapshot(), &published));
        assert_eq!(runtime.tick(), 0);
        assert_eq!(runtime.frame(), 0);
        assert_eq!(runtime.pending_render_updates(), queued);
    }

    #[test]
    fn failed_tick_rolls_back_naval_exile_rng_reserve_and_unit_removal() {
        let mut runtime = exile_fixture(true, false);
        let published = runtime.latest_snapshot();
        let units = runtime.simulation.units.clone();
        let rng = runtime.gameplay_rng.state();
        let reserves = runtime.personnel_reserves.clone();
        let casualties = runtime.casualties.clone();
        runtime.config.front.max_grid_cells = 0;

        assert!(matches!(runtime.step(), Err(RuntimeError::Front(_))));
        assert_eq!(runtime.simulation.units, units);
        assert_eq!(runtime.gameplay_rng.state(), rng);
        assert_eq!(runtime.personnel_reserves, reserves);
        assert_eq!(runtime.casualties, casualties);
        assert!(Arc::ptr_eq(&runtime.latest_snapshot(), &published));
        assert_eq!(runtime.tick(), 0);
        assert_eq!(runtime.frame(), 0);
    }

    #[test]
    fn checkpoint_restores_exact_planner_history_and_refresh_phase() {
        let mut original = fixture(0, false);
        assert!(original.step().unwrap().counters.front_refreshed);
        let config = original.config;
        let captured = original.checkpoint_state().unwrap();
        assert_eq!(captured.last_front_refresh_tick, Some(1));
        assert!(!captured.objectives.is_empty());
        assert!(!captured.front_prior_by_unit.is_empty());

        let mut restored = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();
        assert_eq!(restored.objectives, original.objectives);
        assert_eq!(restored.front_prior_by_unit, original.front_prior_by_unit);
        assert_eq!(restored.last_front_refresh_tick, Some(1));
        assert_eq!(
            restored.prior_objective_by_unit,
            original.prior_objective_by_unit
        );

        let uninterrupted = original.step().unwrap();
        let continued = restored.step().unwrap();
        assert!(!uninterrupted.counters.front_refreshed);
        assert_eq!(continued.frame_snapshot, uninterrupted.frame_snapshot);
        assert_eq!(
            continued.territory_snapshot,
            uninterrupted.territory_snapshot
        );
        assert_eq!(
            continued.strategic_snapshot,
            uninterrupted.strategic_snapshot
        );
        assert_eq!(continued.casualty_totals, uninterrupted.casualty_totals);
        assert_eq!(
            continued.casualties_by_victim,
            uninterrupted.casualties_by_victim
        );
    }

    #[test]
    fn live_battlefield_recomputes_terrain_without_compounding() {
        let mut terrain = vec![0.0; 8];
        // Unit one begins at (-90, 0), which is target cell two.
        terrain[2] = 0.5;
        let mut runtime = with_live_battlefield(fixture(0, false), terrain);
        // The tiny 90-degree fixture would round the browser's 0.7-degree sampling radius to
        // zero and sample the current cell eight times. Keep this test focused on terrain.
        runtime
            .battlefield
            .as_mut()
            .unwrap()
            .config
            .encirclement_radius = 90.0;

        runtime.step().unwrap();
        let mountain = &runtime.unit_policies[&1];
        assert_eq!(mountain.ai.movement.terrain_speed_multiplier, 0.675);
        assert_eq!(mountain.ai.combat.dealt_multiplier, 0.8);
        assert_eq!(mountain.ai.combat.taken_multiplier, 0.8);
        assert_eq!(mountain.influence.as_ref().unwrap().radius, 0.27);
        assert_eq!(mountain.influence.as_ref().unwrap().delta, 0.135);

        // Moving to a flat cell must rebuild from raw primitives rather than multiply the
        // preceding tick's already-resolved policy.
        runtime
            .simulation
            .units
            .iter_mut()
            .find(|unit| unit.combat.id == 1)
            .unwrap()
            .combat
            .lng = -90.0;
        let pre_step_strength = formation_strength(
            &runtime
                .simulation
                .units
                .iter()
                .find(|unit| unit.combat.id == 1)
                .unwrap()
                .combat,
        );
        runtime.step().unwrap();
        let flat = &runtime.unit_policies[&1];
        assert_eq!(flat.ai.movement.terrain_speed_multiplier, 1.0);
        assert_eq!(flat.ai.combat.dealt_multiplier, 1.0);
        assert_eq!(flat.ai.combat.taken_multiplier, 1.0);
        assert_eq!(flat.influence.as_ref().unwrap().radius, 0.4);
        assert_eq!(
            flat.influence.as_ref().unwrap().delta,
            0.18 * pre_step_strength
        );
    }

    #[test]
    fn live_influence_uses_precombat_state_then_excludes_the_next_tick() {
        let mut base = fixture(0, false);
        base.war_grace_end = 0;
        base.simulation.units[0].combat.lng = 0.0;
        base.simulation.units[1].combat.lng = 0.1;
        let mut runtime = with_live_battlefield(base, vec![0.0; 8]);

        let first = runtime.step().unwrap();
        assert!(first.counters.simulation.accepted_contacts > 0);
        assert!(!first.frame_snapshot.events.is_empty());
        assert_eq!(first.counters.influence.sources, 2);

        let second = runtime.step().unwrap();
        assert_eq!(second.counters.influence.sources, 0);
    }

    #[test]
    fn live_policy_and_ai_see_same_tick_influence_changes() {
        let mut runtime = with_live_battlefield(fixture(0, false), vec![0.0; 8]);
        // Unit one occupies cell two. Seed a deliberately stale hostile controller with side
        // zero already dominant by influence, so this tick's source synchronizes the cell.
        runtime
            .territory
            .set_side_influence_cells(0, &[(2, 0.9)])
            .unwrap();
        runtime
            .territory
            .set_side_influence_cells(1, &[(2, 0.0)])
            .unwrap();

        runtime.step().unwrap();

        assert_eq!(runtime.territory.dominant_side()[2], 0);
        assert_eq!(
            runtime.unit_policies[&1].ai.movement.speed_multiplier,
            1.8 * BattlefieldConfig::default().native_speed_scale
        );
    }

    #[test]
    fn capital_loss_uses_hostile_dominance_instead_of_raw_owner() {
        let mut runtime = with_live_battlefield(fixture(0, false), vec![0.0; 8]);
        let mut countries = runtime.scenario.countries.to_vec();
        countries
            .iter_mut()
            .find(|country| country.country_id == 1)
            .unwrap()
            .capital_cell = Some(0);
        runtime.scenario.countries = countries.into();
        runtime
            .territory
            .set_cell_state(
                0,
                CellStateUpdate {
                    dominant_side: Some(1),
                    ..CellStateUpdate::default()
                },
            )
            .unwrap();
        runtime
            .battlefield
            .as_mut()
            .unwrap()
            .config
            .encirclement_radius = 90.0;

        let staged = runtime
            .stage_battlefield_tick(1, 1, &runtime.territory, &[4.0, 4.0], None)
            .unwrap()
            .unwrap();

        assert_eq!(runtime.territory.world_control()[0], 1);
        assert_eq!(staged.policies[&1].ai.combat.dealt_multiplier, 0.8);
        assert_eq!(staged.policies[&1].ai.combat.taken_multiplier, 1.15);
        assert_eq!(
            staged.policies[&1].ai.movement.terrain_speed_multiplier,
            0.9
        );
    }

    #[test]
    fn live_battlefield_memory_and_policy_resume_exactly() {
        let mut terrain = vec![0.0; 8];
        terrain[2] = 0.5;
        let mut uninterrupted = with_live_battlefield(fixture(0, false), terrain);
        uninterrupted.step().unwrap();
        let config = uninterrupted.config;
        let captured = uninterrupted.checkpoint_state().unwrap();
        let mut resumed = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();

        let expected = uninterrupted.step().unwrap();
        let actual = resumed.step().unwrap();
        assert_eq!(actual, expected);
        let expected_state = uninterrupted.checkpoint_state().unwrap();
        let actual_state = resumed.checkpoint_state().unwrap();
        assert_eq!(actual_state.unit_policies, expected_state.unit_policies);
        assert_eq!(actual_state.battlefield, expected_state.battlefield);
    }

    #[test]
    fn checkpoint_rejects_inconsistent_planner_history() {
        let mut source = fixture(0, false);
        source.step().unwrap();
        let config = source.config;
        let captured = source.checkpoint_state().unwrap();
        let prior = captured
            .front_prior_by_unit
            .values()
            .next()
            .unwrap()
            .clone();

        let mut future_refresh = captured.clone();
        future_refresh.last_front_refresh_tick = Some(future_refresh.tick + 1);
        assert!(matches!(
            NativeRuntime::new(config, runtime_checkpoint(future_refresh)),
            Err(RuntimeError::InvalidCheckpoint(
                "last front refresh is newer than the runtime clock"
            ))
        ));

        let mut unknown_unit = captured.clone();
        unknown_unit.front_prior_by_unit.insert(
            u64::MAX,
            FrontLayoutPrior {
                unit_id: u64::MAX,
                ..prior.clone()
            },
        );
        assert!(matches!(
            NativeRuntime::new(config, runtime_checkpoint(unknown_unit)),
            Err(RuntimeError::InvalidCheckpoint(
                "front layout prior references missing units or objectives"
            ))
        ));

        let mut missing_refresh = captured;
        missing_refresh.last_front_refresh_tick = None;
        assert!(matches!(
            NativeRuntime::new(config, runtime_checkpoint(missing_refresh)),
            Err(RuntimeError::InvalidCheckpoint(
                "front layout prior has no completed refresh"
            ))
        ));
    }

    #[test]
    fn orchestrates_ai_simulation_influence_and_immutable_publication() {
        let mut runtime = fixture(0, false);
        let old = runtime.latest_snapshot();
        let old_units = old.frame_snapshot.units.clone();

        let next = runtime.step().unwrap();

        assert_eq!(next.tick, 1);
        assert_eq!(next.frame, 1);
        assert_eq!(next.counters.ai.input_units, 2);
        assert!(next.counters.front_refreshed);
        assert!(next.counters.front_objectives > 0);
        assert_eq!(next.counters.simulation.input_units, 2);
        assert_eq!(next.counters.influence.sources, 2);
        assert_eq!(old.tick, 0);
        assert_eq!(old.frame_snapshot.units, old_units);
        assert!(!Arc::ptr_eq(&old, &next));
    }

    #[test]
    fn due_cycle_flushes_one_coherent_territory_snapshot() {
        let mut runtime = fixture(PAY_CYCLE_TICKS - 1, false);
        let next = runtime.step().unwrap();
        let strategic = next.strategic_snapshot.as_ref().unwrap();

        assert!(next.counters.census.flushed_for_strategic_cycle);
        assert_eq!(strategic.tick, PAY_CYCLE_TICKS);
        assert_eq!(
            strategic.territory_generation,
            next.territory_snapshot.generation
        );
        assert_eq!(
            strategic.territory_commit_sequence,
            next.territory_snapshot.commit_sequence
        );
        assert!(next.counters.strategic.is_some());
        assert!(next.counters.strategic_derivation.is_some());
    }

    #[test]
    fn renderer_deltas_are_fifo_and_never_overwritten() {
        let mut runtime = fixture(0, false);
        assert_eq!(runtime.pending_render_updates(), 1);
        runtime.step().unwrap();
        assert_eq!(runtime.pending_render_updates(), 2);

        let full = runtime.pop_render_update().unwrap();
        let sparse = runtime.pop_render_update().unwrap();
        assert!(full.full_update);
        assert!(!sparse.full_update);
        assert!(runtime.pop_render_update().is_none());
    }

    #[test]
    fn pre_mutation_failure_does_not_publish_or_advance_clock() {
        let mut runtime = fixture(0, false);
        let published = runtime.latest_snapshot();
        let queued = runtime.pending_render_updates();
        runtime.diplomacy.hostility.pop();

        assert!(matches!(runtime.step(), Err(RuntimeError::Front(_))));
        assert!(Arc::ptr_eq(&runtime.latest_snapshot(), &published));
        assert_eq!(runtime.tick(), 0);
        assert_eq!(runtime.frame(), 0);
        assert_eq!(runtime.pending_render_updates(), queued);
        assert_eq!(runtime.state(), RuntimeState::Running);
    }

    #[test]
    fn post_influence_planning_failure_rolls_back_sparse_territory_transaction() {
        let mut runtime = with_live_battlefield(fixture(0, false), vec![0.0; 8]);
        let published = runtime.latest_snapshot();
        let maps = runtime.territory.checkpoint_maps();
        let census = runtime.territory.census_status();
        let queued = runtime.pending_render_updates();
        runtime.config.front.max_grid_cells = 0;

        assert!(matches!(runtime.step(), Err(RuntimeError::Front(_))));
        assert_eq!(runtime.territory.checkpoint_maps(), maps);
        assert_eq!(runtime.territory.census_status(), census);
        assert!(Arc::ptr_eq(&runtime.latest_snapshot(), &published));
        assert_eq!(runtime.tick(), 0);
        assert_eq!(runtime.frame(), 0);
        assert_eq!(runtime.pending_render_updates(), queued);
        assert_eq!(runtime.state(), RuntimeState::Running);
    }

    #[test]
    fn invalid_influence_rolls_back_the_already_stepped_simulation() {
        let mut runtime = fixture(0, false);
        let published = runtime.latest_snapshot();
        runtime
            .unit_policies
            .get_mut(&1)
            .unwrap()
            .influence
            .as_mut()
            .unwrap()
            .radius = f64::NAN;

        assert!(matches!(
            runtime.step(),
            Err(RuntimeError::Territory(
                TerritoryError::InvalidSource { .. }
            ))
        ));
        assert!(Arc::ptr_eq(&runtime.latest_snapshot(), &published));
        assert_eq!(runtime.tick(), 0);
        assert_eq!(
            runtime.simulation.initial_snapshot(0, 0).units,
            published.frame_snapshot.units
        );
        assert_eq!(runtime.state(), RuntimeState::Running);
    }

    #[test]
    fn surrender_and_last_side_resolution_are_published_atomically() {
        let mut runtime = fixture(PAY_CYCLE_TICKS - 1, true);
        let published = runtime.step().unwrap();
        assert!(matches!(
            published.state,
            RuntimeState::ConflictResolved { .. }
        ));
        assert_eq!(
            published
                .strategic_snapshot
                .as_ref()
                .unwrap()
                .surrenders
                .len(),
            1
        );
        assert!(
            published
                .strategic_snapshot
                .as_ref()
                .unwrap()
                .conflict_resolution
                .is_some()
        );
        assert_eq!(
            published
                .strategic_snapshot
                .as_ref()
                .unwrap()
                .events
                .last()
                .map(|event| event.kind),
            Some(crate::strategic::StrategicEventKind::TreatyResolved)
        );
        assert_eq!(runtime.strategic.occupations().len(), 1);
        assert!(
            runtime
                .territory
                .world_control()
                .iter()
                .all(|owner| *owner == 1)
        );
        assert!(
            runtime
                .territory
                .primary_occupier()
                .iter()
                .all(|owner| *owner == 1)
        );
        assert!(published.counters.census.committed);
        assert_eq!(
            published.territory_snapshot,
            runtime.territory.snapshot().unwrap()
        );

        assert!(matches!(
            runtime.step(),
            Err(RuntimeError::ConflictResolved { .. })
        ));
    }

    #[test]
    fn reverse_only_hostility_cannot_poison_surrender_application() {
        let mut runtime = fixture(PAY_CYCLE_TICKS - 1, true);
        let directed = vec![0, 1, 0, 0];
        runtime
            .territory
            .set_topology(
                runtime.territory.country_to_side().clone(),
                directed.clone(),
                2,
            )
            .unwrap();
        runtime.diplomacy.hostility = directed;

        let published = runtime.step().unwrap();
        assert_eq!(published.state, RuntimeState::Running);
        assert!(
            published
                .strategic_snapshot
                .as_ref()
                .unwrap()
                .surrenders
                .is_empty()
        );
        assert!(!runtime.strategic.economies()[&2].capitulated);
    }

    #[test]
    fn explicit_single_active_side_cannot_surrender_to_an_inactive_side() {
        let mut runtime = fixture(PAY_CYCLE_TICKS - 1, true);
        let directed = vec![0, 0, 1, 0];
        runtime
            .territory
            .set_topology(
                runtime.territory.country_to_side().clone(),
                directed.clone(),
                2,
            )
            .unwrap();
        runtime.diplomacy.hostility = directed;
        runtime.diplomacy.active_sides = vec![1];

        let published = runtime.step().unwrap();
        assert!(matches!(
            published.state,
            RuntimeState::ConflictResolved { .. }
        ));
        assert!(
            published
                .strategic_snapshot
                .as_ref()
                .unwrap()
                .surrenders
                .is_empty()
        );
        assert!(!runtime.strategic.economies()[&2].capitulated);
    }

    #[test]
    fn front_layout_refreshes_first_then_on_configured_tick_phase() {
        let mut runtime = fixture(27, false);
        assert!(runtime.step().unwrap().counters.front_refreshed);
        assert!(!runtime.step().unwrap().counters.front_refreshed);
        assert!(runtime.step().unwrap().counters.front_refreshed);
    }

    #[test]
    fn deploying_units_are_absent_from_planning_combat_movement_and_influence() {
        let mut runtime = fixture(0, false);
        runtime.config.front_refresh_ticks = 1;
        for policy in runtime.unit_policies.values_mut() {
            policy.ai.deploy_until_tick = 1;
        }

        let deploying = runtime.step().unwrap();
        assert_eq!(deploying.counters.ai.input_units, 0);
        assert_eq!(deploying.counters.front_objectives, 0);
        assert_eq!(deploying.counters.simulation.held_units, 2);
        assert_eq!(deploying.counters.simulation.accepted_contacts, 0);
        assert_eq!(deploying.counters.simulation.moved_units, 0);
        assert_eq!(deploying.counters.influence.sources, 0);

        let active = runtime.step().unwrap();
        assert_eq!(active.counters.ai.input_units, 2);
        assert!(active.counters.front_objectives > 0);
        assert_eq!(active.counters.influence.sources, 2);
    }

    #[test]
    fn deploying_live_units_preserve_momentum_and_encirclement_history() {
        let mut runtime = with_live_battlefield(fixture(0, false), vec![0.0; 8]);
        runtime
            .unit_policies
            .get_mut(&1)
            .unwrap()
            .ai
            .deploy_until_tick = 1;
        runtime.simulation.units[0].combat.victory_boost_ticks = 5;
        runtime
            .battlefield
            .as_mut()
            .unwrap()
            .units
            .get_mut(&1)
            .unwrap()
            .encircled_ticks = 77;

        runtime.step().unwrap();

        assert_eq!(runtime.simulation.units[0].combat.victory_boost_ticks, 5);
        assert_eq!(
            runtime
                .battlefield
                .as_ref()
                .unwrap()
                .units
                .get(&1)
                .unwrap()
                .encircled_ticks,
            77
        );
    }

    #[test]
    fn supply_collapse_marker_commits_and_survives_exact_runtime_restore() {
        let mut runtime = with_live_battlefield(fixture(0, false), vec![0.0; 8]);
        runtime.simulation.units[0].combat.lat = -89.0;
        runtime
            .unit_policies
            .get_mut(&1)
            .unwrap()
            .influence
            .as_mut()
            .unwrap()
            .delta = 0.0;
        runtime
            .battlefield
            .as_mut()
            .unwrap()
            .config
            .encirclement_radius = 90.0;

        runtime.step().unwrap();

        assert_eq!(
            runtime
                .battlefield
                .as_ref()
                .unwrap()
                .units
                .get(&1)
                .unwrap()
                .supply_collapsed_tick,
            Some(1)
        );
        let config = runtime.config;
        let captured = runtime.checkpoint_state().unwrap();
        let mut restored = NativeRuntime::new(config, runtime_checkpoint(captured)).unwrap();
        restored.simulation.units[0].combat.victory_boost_ticks = 3;

        restored.step().unwrap();

        assert_eq!(
            restored
                .battlefield
                .as_ref()
                .unwrap()
                .units
                .get(&1)
                .unwrap()
                .supply_collapsed_tick,
            Some(1),
            "ticks outside the supply scan preserve the browser marker"
        );
    }

    #[test]
    fn garrison_excluded_units_hold_without_consuming_front_capacity() {
        let mut runtime = fixture(0, false);
        runtime.config.front_refresh_ticks = 1;
        runtime
            .unit_policies
            .get_mut(&1)
            .unwrap()
            .ai
            .garrison_excluded = true;
        let before = runtime.latest_snapshot();
        let before_garrison = before
            .frame_snapshot
            .units
            .iter()
            .find(|unit| unit.id == 1)
            .unwrap();

        let next = runtime.step().unwrap();
        let after_garrison = next
            .frame_snapshot
            .units
            .iter()
            .find(|unit| unit.id == 1)
            .unwrap();
        assert_eq!(next.counters.ai.input_units, 1);
        assert_eq!(
            (after_garrison.lat, after_garrison.lng),
            (before_garrison.lat, before_garrison.lng)
        );
        assert!(next.counters.simulation.held_units >= 1);
        assert_eq!(next.counters.influence.sources, 2);
    }

    #[test]
    fn garrison_hold_preserves_resolved_combat_modifiers() {
        let runtime = fixture(0, false);
        let mut policies = runtime.unit_policies.clone();
        let policy = policies.get_mut(&1).unwrap();
        policy.ai.garrison_excluded = true;
        policy.ai.combat.dealt_multiplier = 7.0;
        policy.ai.combat.taken_multiplier = 0.4;
        policy.ai.combat.mountain = true;

        let orders = garrison_hold_orders(&runtime.simulation, &policies, 1).unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].unit_id, 1);
        assert!(!orders[0].movement_enabled);
        assert_eq!(orders[0].combat.dealt_multiplier, 7.0);
        assert_eq!(orders[0].combat.taken_multiplier, 0.4);
        assert!(orders[0].combat.mountain);

        policies.get_mut(&1).unwrap().ai.combat.dealt_multiplier = -1.0;
        assert!(matches!(
            validate_unit_policy(policies.get(&1).unwrap(), &runtime.territory),
            Err(RuntimeError::InvalidUnitPolicy(1))
        ));
    }

    #[test]
    fn at_sea_units_stamp_their_pre_resolved_reduced_influence() {
        let mut runtime = fixture(0, false);
        runtime.simulation.units[0].combat.at_sea = true;
        runtime
            .unit_policies
            .get_mut(&1)
            .unwrap()
            .influence
            .as_mut()
            .unwrap()
            .delta *= 0.4;

        let next = runtime.step().unwrap();
        assert_eq!(next.counters.influence.sources, 2);
    }

    #[test]
    fn browser_influence_timing_recomputes_ramp_and_noise_each_tick() {
        let mut policy = UnitInfluencePolicy {
            radius: 10.0,
            delta: 2.0,
            browser_temporal_seed: Some(0.0),
            ..UnitInfluencePolicy::default()
        };
        let (radius_zero, delta_zero) = resolve_influence_timing(&policy, 0);
        assert_eq!(radius_zero, 9.0);
        assert!((delta_zero - 0.08).abs() < 1e-15);

        let (radius_late, delta_late) = resolve_influence_timing(&policy, 600);
        assert!((radius_late - 10.0 * (0.9 + 60.0_f64.sin() * 0.2)).abs() < 1e-12);
        assert!((delta_late - 2.0 * (0.8 + 30.0_f64.sin() * 0.4)).abs() < 1e-12);

        policy.browser_temporal_seed = Some(f64::NAN);
        let runtime = fixture(0, false);
        assert!(matches!(
            validate_unit_policy(
                &RuntimeUnitPolicy {
                    unit_id: 1,
                    ai: UnitAiPolicy::default(),
                    command: UnitCommandPolicy::paid(1.0, 1),
                    influence: Some(policy),
                },
                &runtime.territory,
            ),
            Err(RuntimeError::InvalidUnitPolicy(1))
        ));
    }

    #[test]
    fn browser_influence_cohorts_and_side_scaled_budgets_match_javascript() {
        assert_eq!(browser_stable_unit_cohort(0.1, 3), 0);
        assert_eq!(browser_stable_unit_cohort(0.2, 3), 1);
        assert_eq!(browser_stable_unit_cohort(0.3, 3), 2);
        assert_eq!(browser_stable_unit_cohort(0.5, 3), 0);
        assert_eq!(browser_stable_unit_cohort(1.0, 3), 1);
        assert_eq!(browser_stable_unit_cohort(2.000_000_001, 3), 0);
        assert_eq!(browser_stable_unit_cohort(f64::INFINITY, 3), 0);

        assert_eq!(browser_influence_budgets(0), (300, 1_600));
        assert_eq!(browser_influence_budgets(2), (300, 1_600));
        assert_eq!(browser_influence_budgets(8), (75, 400));
        assert_eq!(browser_influence_budgets(24), (50, 400));
    }

    #[test]
    fn strategic_band_transition_updates_live_unit_policy_and_clears_front_memory() {
        let mut runtime = fixture(PAY_CYCLE_TICKS - 1, false);
        let mut economies = runtime
            .strategic
            .economies()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let economy = economies
            .iter_mut()
            .find(|economy| economy.country_id == 1)
            .unwrap();
        economy.base_income = 0.0;
        economy.income = 0.0;
        economy.treasury = 0.0;
        economy.arrears_cycles = 2.0;
        economy.command_band = CommandBand::Unpaid;
        economy.last_event_band = CommandBand::Unpaid;
        runtime.strategic = StrategicSimulation::restore(0, economies, []).unwrap();

        let policy = runtime.unit_policies.get_mut(&1).unwrap();
        policy.command = UnitCommandPolicy {
            band: CommandBand::Unpaid,
            discipline: 0.1,
            refuses_offense: true,
            return_home: false,
            self_defense_only: false,
            home_target: None,
            transition_cycle: 0,
        };
        policy.influence.as_mut().unwrap().refuses_offense = true;
        runtime.prior_objective_by_unit.insert(1, 1);

        let published = runtime.step().unwrap();
        let policy = runtime.unit_policies.get(&1).unwrap();
        assert_eq!(policy.command.band, CommandBand::Breakdown);
        assert!(policy.command.refuses_offense);
        assert!(policy.command.return_home);
        assert!(!policy.command.self_defense_only);
        assert_eq!(policy.command.transition_cycle, 1);
        assert_eq!(policy.command.home_target.unwrap().cell, 0);
        assert!(policy.influence.as_ref().unwrap().refuses_offense);
        assert!(!runtime.prior_objective_by_unit.contains_key(&1));
        assert_eq!(
            published.strategic_snapshot.as_ref().unwrap().countries[0]
                .economy
                .command_band,
            CommandBand::Breakdown
        );

        let before_lng = published
            .frame_snapshot
            .units
            .iter()
            .find(|unit| unit.id == 1)
            .unwrap()
            .lng;
        let next = runtime.step().unwrap();
        let after_lng = next
            .frame_snapshot
            .units
            .iter()
            .find(|unit| unit.id == 1)
            .unwrap()
            .lng;
        assert!(after_lng < before_lng);
        assert!(next.counters.ai.hold_orders >= 1);
    }

    #[test]
    fn restore_hydrates_missing_legacy_return_home_target() {
        let mut source = fixture(0, false);
        let mut state = source.checkpoint_state().unwrap();
        let economy = state
            .economies
            .iter_mut()
            .find(|economy| economy.country_id == 1)
            .unwrap();
        economy.arrears_cycles = 3.0;
        economy.command_band = CommandBand::Breakdown;
        economy.last_event_band = CommandBand::Breakdown;
        let policy = state
            .unit_policies
            .iter_mut()
            .find(|policy| policy.unit_id == 1)
            .unwrap();
        policy.command = UnitCommandPolicy {
            band: CommandBand::Breakdown,
            discipline: 0.1,
            refuses_offense: true,
            return_home: true,
            self_defense_only: false,
            home_target: None,
            transition_cycle: 0,
        };
        policy.influence.as_mut().unwrap().refuses_offense = true;

        let mut resumed =
            NativeRuntime::new(state.runtime_config, runtime_checkpoint(state)).unwrap();
        let target = resumed
            .unit_policies
            .get(&1)
            .unwrap()
            .command
            .home_target
            .unwrap();
        assert_eq!(target.cell, 0);
        let before_lng = resumed.simulation.units[0].combat.lng;
        resumed.step().unwrap();
        assert!(resumed.simulation.units[0].combat.lng < before_lng);
    }

    #[test]
    fn conflict_resolution_is_a_clean_terminal_publication() {
        let mut runtime = fixture(PAY_CYCLE_TICKS - 1, false);
        runtime.diplomacy.active_sides = vec![0];

        let terminal = runtime.step().unwrap();
        let RuntimeState::ConflictResolved { resolution, .. } = terminal.state else {
            panic!("expected resolved conflict");
        };
        assert!(resolution.stop_simulation);
        assert!(matches!(
            runtime.step(),
            Err(RuntimeError::ConflictResolved { .. })
        ));
    }
}
