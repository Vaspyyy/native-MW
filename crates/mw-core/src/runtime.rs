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
        AiOrderConfig, AiOrderError, AiPlanningCounters, AiUnitInput, AiWorldInput,
        FrontAssignmentRecord, FrontObjective, ResolvedCombatModifiers, ResolvedMovementModifiers,
        resolve_ai_orders,
    },
    combat::formation_strength,
    direction::HostilityMatrix,
    economy::PAY_CYCLE_TICKS,
    front::{
        FrontLayoutConfig, FrontLayoutError, FrontLayoutInput, FrontLayoutPrior, FrontLayoutUnit,
        derive_front_layout,
    },
    occupation::{OccupationState, required_garrison},
    production::{
        ProductionConfig, ProductionError, ScenarioProduction, StrategicDerivationCounters,
        StrategicDerivationInput, TerritoryCommitMarker, derive_strategic_cycle_input,
    },
    scenario::GridSpec,
    simulation::{
        FrameSnapshot, ResolvedCombatOrder, ResolvedUnitOrder, Simulation, SimulationError,
        TickCounters, TickInput,
    },
    strategic::{
        ConflictResolutionPlan, PreparedStrategicCycle, StrategicCounters, StrategicError,
        StrategicSimulation, StrategicSnapshot, SurrenderAllocationInput, SurrenderUnitPosition,
        plan_surrender_allocation,
    },
    surrender::evaluate_global_conflict,
    tactical::SideKey,
    territory::{
        CensusStepResult, InfluenceApplyResult, InfluenceSource, TerritoryCommittedState,
        TerritoryControl, TerritoryError, TerritoryRenderUpdate, TerritorySnapshot,
    },
    world::WorldGridView,
};

pub const NATIVE_RUNTIME_SCHEMA_VERSION: &str = "native-runtime-v2";
pub const DEFAULT_FRONT_REFRESH_TICKS: u64 = 30;
pub const DEFAULT_CENSUS_BUDGET: usize = 16_384;
pub const DEFAULT_CENSUS_FLUSH_CHUNK: usize = 65_536;
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
            influence: Some(influence),
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
    pub processed_source_cells: usize,
    pub touched_influence_cells: usize,
    pub changed_controller_cells: usize,
    pub changed_credit_cells: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeStepCounters {
    pub front_refreshed: bool,
    pub front_segments: usize,
    pub front_objectives: usize,
    pub ai: AiPlanningCounters,
    pub simulation: TickCounters,
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
    pub strategic_snapshot: Option<Arc<StrategicSnapshot>>,
    pub counters: RuntimeStepCounters,
    pub pending_render_updates: usize,
    pub casualty_totals: Arc<BTreeMap<u16, f64>>,
    /// Exact victim -> attacker personnel-loss attribution used by deterministic surrender
    /// allocation and persisted by mid-war checkpoints.
    pub casualties_by_victim: Arc<BTreeMap<u16, BTreeMap<u16, f64>>>,
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
    #[error("AI planning: {0}")]
    Ai(#[from] AiOrderError),
    #[error("front layout: {0}")]
    Front(#[from] FrontLayoutError),
    #[error("unit simulation: {0}")]
    Simulation(#[from] SimulationError),
    #[error("territory: {0}")]
    Territory(#[from] TerritoryError),
    #[error("strategic simulation: {0}")]
    Strategic(#[from] StrategicError),
    #[error("production input derivation: {0}")]
    Production(#[from] ProductionError),
}

fn influence_counters(sources: usize, result: &InfluenceApplyResult) -> RuntimeInfluenceCounters {
    RuntimeInfluenceCounters {
        sources,
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

fn ai_units(
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    prior_objective_by_unit: &BTreeMap<u64, u64>,
    tick: u64,
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
                ally_weight: unit.ally_weight,
                at_sea: unit.combat.at_sea,
                transport: unit.combat.transport,
                base_speed: policy.ai.base_speed,
                movement: policy.ai.movement,
                combat: policy.ai.combat,
                prior_front_objective_id: prior_objective_by_unit.get(&unit.combat.id).copied(),
                is_reserve: policy.ai.is_reserve,
                reinforcement_eligible: policy.ai.reinforcement_eligible,
                encircled: policy.ai.encircled,
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
        order.combat = ResolvedCombatOrder {
            dealt_multiplier: policy.ai.combat.dealt_multiplier,
            taken_multiplier: policy.ai.combat.taken_multiplier,
            defense_bonus: policy.ai.combat.defense_bonus,
            long_war_defense: policy.ai.combat.long_war_defense,
            mountain: policy.ai.combat.mountain,
            urban: policy.ai.combat.urban,
        };
        orders.push(order);
    }
    orders.sort_unstable_by_key(|order| order.unit_id);
    Ok(orders)
}

fn front_layout_units(
    simulation: &Simulation,
    policies: &BTreeMap<u64, RuntimeUnitPolicy>,
    previous: &BTreeMap<u64, FrontLayoutPrior>,
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
                garrison_excluded: policy.ai.garrison_excluded,
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
    /// Explicit objectives are accepted as a checkpoint fallback and replaced by the scheduled
    /// front-layout adapter once its first refresh succeeds.
    pub objectives: Vec<FrontObjective>,
    pub prior_objective_by_unit: BTreeMap<u64, u64>,
    pub casualties: BTreeMap<u16, f64>,
    pub casualties_by_victim: BTreeMap<u16, BTreeMap<u16, f64>>,
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
    unit_sovereign_by_id: Vec<(u64, u16)>,
    objectives: Vec<FrontObjective>,
    front_prior_by_unit: BTreeMap<u64, FrontLayoutPrior>,
    last_front_refresh_tick: Option<u64>,
    prior_objective_by_unit: BTreeMap<u64, u64>,
    casualties: BTreeMap<u16, f64>,
    casualties_by_victim: BTreeMap<u16, BTreeMap<u16, f64>>,
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

        let unit_policies = collect_unit_policies(
            &checkpoint.simulation,
            checkpoint.unit_policies,
            &checkpoint.territory,
        )?;
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
        validate_casualties(&checkpoint.casualties, &checkpoint.scenario)?;
        validate_casualties_by_victim(&checkpoint.casualties_by_victim, &checkpoint.scenario)?;

        // A checkpoint may have an in-progress bounded census. Construction finishes it before
        // exposing the first cross-kernel publication.
        let territory_snapshot = checkpoint.territory.flush_census(config.census_flush_chunk);
        let frame_snapshot = Arc::new(
            checkpoint
                .simulation
                .initial_snapshot(checkpoint.tick, checkpoint.frame),
        );
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
            strategic_snapshot,
            counters: RuntimeStepCounters::default(),
            pending_render_updates: render_updates.len(),
            casualty_totals: Arc::new(checkpoint.casualties.clone()),
            casualties_by_victim: Arc::new(checkpoint.casualties_by_victim.clone()),
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
            unit_sovereign_by_id,
            objectives: checkpoint.objectives,
            front_prior_by_unit: BTreeMap::new(),
            last_front_refresh_tick: None,
            prior_objective_by_unit: checkpoint.prior_objective_by_unit,
            casualties: checkpoint.casualties,
            casualties_by_victim: checkpoint.casualties_by_victim,
            state: initial_state,
            latest,
            render_updates,
        })
    }

    pub fn latest_snapshot(&self) -> Arc<RuntimeSnapshot> {
        self.latest.clone()
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
                config.maps.dominant_side[transfer.cell] = i16::try_from(recipient_side)
                    .map_err(|_| RuntimeError::InvalidCheckpoint("side index exceeds i16"))?;
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
            staged_territory = Some(TerritoryControl::restore(config, committed)?);

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
        // The first step always lays out fronts, including restored checkpoints at an arbitrary
        // tick. Subsequent refreshes use the browser-like 30-tick phase (configurable).
        let refresh_fronts = self.last_front_refresh_tick.is_none()
            || next_tick.is_multiple_of(self.config.front_refresh_ticks);
        let refreshed_layout = if refresh_fronts {
            let units = front_layout_units(
                &self.simulation,
                &self.unit_policies,
                &self.front_prior_by_unit,
                next_tick,
            )?;
            Some(derive_front_layout(
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
            )?)
        } else {
            None
        };
        let refreshed_objective_prior = refreshed_layout
            .as_ref()
            .map(|layout| front_objectives_by_unit(&layout.next_prior));
        let planning_prior = refreshed_objective_prior
            .as_ref()
            .unwrap_or(&self.prior_objective_by_unit);
        let planning_objectives = refreshed_layout
            .as_ref()
            .map_or(self.objectives.as_slice(), |layout| {
                layout.objectives.as_slice()
            });
        let ai_units = ai_units(
            &self.simulation,
            &self.unit_policies,
            planning_prior,
            next_tick,
        )?;
        let mut planning = resolve_ai_orders(
            self.config.ai,
            &ai_units,
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
        )?;
        planning.orders.extend(garrison_hold_orders(
            &self.simulation,
            &self.unit_policies,
            next_tick,
        )?);
        planning.orders.sort_unstable_by_key(|order| order.unit_id);

        // Simulation is the only fallible mutating kernel before territory. Keep an O(units)
        // rollback image so movement/combat errors cannot strand a half-tick.
        let simulation_backup = self.simulation.units.clone();
        let simulation_config = self.simulation.config();
        let mut inactive_unit_ids = self
            .simulation
            .units
            .iter()
            .filter_map(|unit| {
                self.unit_policies
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
            world: WorldGridView::new(
                self.territory.grid_resolution(),
                self.territory.width(),
                self.territory.height(),
                self.territory.land(),
            )
            .map_err(SimulationError::from)?,
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
                self.simulation = Simulation::new(simulation_config, simulation_backup)
                    .expect("validated simulation rollback must reconstruct");
                return Err(RuntimeError::Simulation(error));
            }
        };
        let next_casualties = next_casualties(
            &self.casualties,
            &self.unit_sovereign_by_id,
            &frame_snapshot,
        );
        let next_casualties_by_victim = next_casualties_by_victim(
            &self.casualties_by_victim,
            &self.unit_sovereign_by_id,
            &frame_snapshot,
        );
        let sources = match influence_sources(&frame_snapshot, &self.unit_policies, next_tick) {
            Ok(sources) => sources,
            Err(error) => {
                self.simulation = Simulation::new(simulation_config, simulation_backup)
                    .expect("validated simulation rollback must reconstruct");
                return Err(error);
            }
        };
        let influence = match self.territory.apply_influence_sources(&sources) {
            Ok(influence) => influence,
            Err(error) => {
                // Territory validates the complete source batch before mutation.
                self.simulation = Simulation::new(simulation_config, simulation_backup)
                    .expect("validated simulation rollback must reconstruct");
                return Err(RuntimeError::Territory(error));
            }
        };

        let strategic_due = next_tick.is_multiple_of(PAY_CYCLE_TICKS);
        let (mut territory_snapshot, mut census) = advance_census(
            &mut self.territory,
            self.config.census_budget,
            self.config.census_flush_chunk,
            strategic_due,
        );
        let mut strategic_snapshot = self.strategic.latest_snapshot();
        let mut strategic_counters = None;
        let mut derivation_counters = None;
        let mut next_state = RuntimeState::Running;
        let mut strategic_fronts_invalidated = false;
        if strategic_due {
            let derived = match derive_production_input(
                next_tick,
                &self.scenario,
                &self.territory,
                &territory_snapshot,
                &frame_snapshot,
                &self.diplomacy,
                &self.strategic,
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
            let mut prepared = match self.strategic.prepare_cycle(&strategic_input) {
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
            let (published, counters) = match self.strategic.commit_cycle(prepared) {
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
            next_state = effects.state;
            strategic_snapshot = Some(published);
            strategic_counters = Some(counters);
        }

        let mut next_prior = assignments_by_unit(&planning.assignments);
        let surviving_ids = frame_snapshot
            .units
            .iter()
            .map(|unit| unit.id)
            .collect::<BTreeSet<_>>();
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
        next_prior.retain(|unit_id, _| surviving_ids.contains(unit_id));
        self.prior_objective_by_unit = next_prior;
        self.casualties = next_casualties;
        self.casualties_by_victim = next_casualties_by_victim;
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
        let counters = RuntimeStepCounters {
            front_refreshed: refresh_fronts,
            front_segments: segments,
            front_objectives: self.objectives.len(),
            ai: planning.counters,
            simulation: simulation_counters,
            influence: influence_counters(sources.len(), &influence),
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
            strategic_snapshot,
            counters,
            pending_render_updates: self.render_updates.len(),
            casualty_totals: Arc::new(self.casualties.clone()),
            casualties_by_victim: Arc::new(self.casualties_by_victim.clone()),
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
    let units = ai_units(simulation, policies, prior, tick)?;
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
        combat::{CombatConfig, CombatEvent, CombatLayer, CombatUnit, UnitKind},
        economy::{EconomySeed, create_economy_state},
        production::{PRODUCTION_SCHEMA_VERSION, ProductionCountry, ScenarioProductionCounters},
        simulation::{SimulationConfig, SimulationUnit},
        territory::{TerritoryConfig, TerritoryMaps},
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
                objectives,
                prior_objective_by_unit: BTreeMap::new(),
                casualties: BTreeMap::new(),
                casualties_by_victim: BTreeMap::new(),
            },
        )
        .unwrap()
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
                    influence: Some(policy),
                },
                &runtime.territory,
            ),
            Err(RuntimeError::InvalidUnitPolicy(1))
        ));
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
