//! Renderer-independent Modern Wars simulation and data core.
//!
//! This crate deliberately contains no windowing, GPU, filesystem UI, or web
//! APIs. Native and future web frontends should consume snapshots from here.

pub mod ai;
pub mod battlefield;
pub mod bootstrap;
pub mod combat;
pub mod command;
pub mod diffusion;
pub mod direction;
pub mod dynamics;
pub mod economy;
pub mod front;
pub mod movement;
pub mod occupation;
pub mod operations;
pub mod production;
pub mod runtime;
pub mod scenario;
pub mod simulation;
pub mod strategic;
pub mod surrender;
pub mod tactical;
pub mod territory;
pub mod world;

pub use ai::{
    AI_ORDER_SCHEMA_VERSION, AiOrderConfig, AiOrderError, AiPlanningCounters, AiPlanningResult,
    AiTacticalContactRecord, AiUnitInput, AiWorldInput, AssignmentReason, FrontAssignmentRecord,
    FrontObjective, ResolvedCombatModifiers, ResolvedMovementModifiers, resolve_ai_orders,
};
pub use battlefield::{
    ATTRITION_DAMAGE, BATTLEFIELD_SCHEMA_VERSION, BattlefieldAttritionResult, BattlefieldBuff,
    BattlefieldCellState, BattlefieldCohesionGroup, BattlefieldConfig, BattlefieldDirectionInput,
    BattlefieldDirectionResult, BattlefieldError, BattlefieldInfluenceModifiers,
    BattlefieldLocalTacticsResult, BattlefieldLocalUnitInput, BattlefieldLocalUnitResult,
    BattlefieldMapView, BattlefieldRuntimeState, BattlefieldTickInput, BattlefieldTickResult,
    BattlefieldUnitInput, BattlefieldUnitResult, BattlefieldUnitState, BattlefieldUrbanCenter,
    BattlefieldVector, BattlefieldWarPhase, CountryBattlefieldPrimitives, ENCIRCLEMENT_DAMAGE_MULT,
    active_combat_influence_eligible, apply_cohesion_and_repulsion, armor_influence_multiplier,
    armor_speed_multiplier, cohesion_group, resolve_battlefield_tick, resolve_local_tactics,
};
pub use bootstrap::{NativeWarBootstrapConfig, NativeWarBootstrapError, bootstrap_native_war};

pub use combat::{
    COMBAT_SCHEMA_VERSION, CombatConfig, CombatContext, CombatError, CombatEvent, CombatLayer,
    CombatUnit, DamageApply, UnitKind, apply_land_unit_damage, combined_arms_damage,
    formation_strength, jittered_target_distance, matchup_multiplier, quality_multiplier,
    resolve_direct_engagement, resolve_proximity_contact, wrapped_distance_squared,
    wrapped_longitude_delta,
};
pub use command::{
    CommandHomeTarget, CommandResolveError, CommandUnitState, CommandWorld, ResolvedCommandPolicy,
    browser_discipline, refusal_share, resolve_command_batch, resolve_command_policy,
};
pub use diffusion::{
    DiffusionError, DiffusionQueueResult, FrontierCellResult, FrontierDiffusion,
    InfluenceRuntimeState,
};
pub use direction::{
    DirectionField, DirectionFieldError, DirectionFieldInput, HostilityMatrix,
    build_direction_field,
};
pub use dynamics::{SideDynamics, WarPhase, WarPosture, bootstrap_sides};
pub use economy::{
    CAPITAL_LOSS_INCOME_MULT, CommandBand, ECONOMY_SCHEMA_VERSION, EconomyCycleInput, EconomyError,
    EconomySeed, EconomyState, MUTINY_RECOVERY_CYCLES, OCCUPATION_COST_SHARE,
    OCCUPATION_YIELD_SHARE, PAY_CYCLE_TICKS, PAYROLL_PER_UNIT, RECRUITMENT_COST,
    STARTING_RESERVE_CYCLES, TARGET_STARTING_PAYROLL_SHARE, command_band, command_refusal_share,
    compute_current_income, compute_economic_strength, create_economy_state, desertion_rate,
    settle_economy_cycle,
};
pub use front::{
    FRONT_LAYOUT_SCHEMA_VERSION, FrontLayout, FrontLayoutConfig, FrontLayoutCounters,
    FrontLayoutError, FrontLayoutInput, FrontLayoutPrior, FrontLayoutUnit, FrontPoint,
    FrontSegment, FrontSlotAssignment, derive_front_layout,
};
pub use movement::{
    MOVEMENT_SCHEMA_VERSION, MovementError, MovementFactors, MovementInput, MovementOutput,
    MovementState, integrate_unit_step,
};
pub use occupation::{
    OCCUPATION_SCHEMA_VERSION, OccupationAssessment, OccupationControllerCandidate,
    OccupationCycleInput, OccupationError, OccupationState, RebellionCandidate, assess_occupation,
    garrison_priority, required_garrison, resistance_delta, select_occupation_controller,
    select_rebellion_candidates,
};
pub use operations::*;
pub use production::{
    ARMOR_PAYROLL_PER_100, PRODUCTION_SCHEMA_VERSION, ProductionCity, ProductionConfig,
    ProductionCountry, ProductionError, ScenarioProduction, ScenarioProductionCounters,
    StrategicDerivationCounters, StrategicDerivationInput, StrategicDerivationOutput,
    TerritoryCommitMarker, derive_scenario_production, derive_strategic_cycle_input,
    estimate_territory_army_units,
};
pub use runtime::{
    DEFAULT_CENSUS_BUDGET, DEFAULT_CENSUS_FLUSH_CHUNK, DEFAULT_FRONT_REFRESH_TICKS,
    DEFAULT_RUNTIME_FRONT_STICKINESS, NATIVE_RUNTIME_SCHEMA_VERSION, NativeRuntime,
    NativeRuntimeCheckpointState, RuntimeAttritionCounters, RuntimeCensusCounters,
    RuntimeCheckpoint, RuntimeConfig, RuntimeDiplomacy, RuntimeError, RuntimeInfluenceCounters,
    RuntimeSnapshot, RuntimeState, RuntimeStepCounters, RuntimeUnitPolicy, UnitAiPolicy,
    UnitCommandPolicy, UnitInfluencePolicy,
};
pub use scenario::{
    DecodedScenario, GridSpec, ScenarioError, decode_mwsc, decode_mwsc_gzip, decode_mwsc_gzip_file,
};
pub use simulation::{
    DamageCommand, DamageOutcome, DamageResult, DesertionOutcome, FrameSnapshot,
    NATIVE_TICK_SCHEMA_VERSION, ResolvedCombatOrder, ResolvedUnitOrder, Simulation,
    SimulationConfig, SimulationError, SimulationUnit, TickCounters, TickInput, UnitSnapshot,
};
pub use strategic::{
    ConflictResolutionPlan, CountryCycleInput, CountryStrategicSnapshot, DesertionCommand,
    OccupationCycleRecord, PreparedStrategicCycle, STRATEGIC_SCHEMA_VERSION, StrategicCounters,
    StrategicCycleInput, StrategicError, StrategicEvent, StrategicEventKind, StrategicSimulation,
    StrategicSnapshot, SurrenderAllocationInput, SurrenderAllocationPlan, SurrenderCellTransfer,
    SurrenderCommand, SurrenderUnitPosition, plan_surrender_allocation,
};
pub use surrender::{
    CapitulationDecision, CapitulationInput, CapitulationReason, CasualtyEntry, CasualtyShare,
    ConflictResolution, ConflictResolutionKind, DEFENDED_CONTROL_PERCENT, OwnerTransfer,
    SURRENDER_SCHEMA_VERSION, SurrenderError, UNITLESS_CONTROL_PERCENT, WeightedQuota,
    eligible_casualty_attackers, evaluate_capitulation, evaluate_global_conflict,
    largest_remainder_quotas, majority_owner_transfers, update_rebellion_failure_cycles,
};
pub use tactical::{
    DEFAULT_TACTICAL_CELL_SIZE, NeighborOptions, PairOptions, PairStats, PairVisit, SideKey,
    TACTICAL_GRID_SCHEMA_VERSION, TacticalCell, TacticalCellCoords, TacticalGrid,
    TacticalGridCounters, TacticalGridDimensions, TacticalGridError, TacticalUnit,
    parse_tactical_cell_key, tactical_cell_coords, tactical_cell_key, tactical_grid_dimensions,
    wrap_tactical_longitude,
};
pub use territory::{
    CellStateUpdate, CensusStatus, CensusStepResult, CountryAggregate, DEFAULT_TERRITORY_TILE_SIZE,
    InfluenceApplyResult, InfluenceSource, SideAggregate, TERRITORY_SCHEMA_VERSION, TerritoryCity,
    TerritoryCommittedState, TerritoryConfig, TerritoryControl, TerritoryError, TerritoryMaps,
    TerritoryRenderUpdate, TerritorySnapshot, TerritoryTilePixels, TileBounds,
};
pub use world::{WorldGridError, WorldGridView};
