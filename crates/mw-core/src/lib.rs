//! Renderer-independent Modern Wars simulation and data core.
//!
//! This crate deliberately contains no windowing, GPU, filesystem UI, or web
//! APIs. Native and future web frontends should consume snapshots from here.

pub mod combat;
pub mod direction;
pub mod movement;
pub mod scenario;
pub mod simulation;
pub mod tactical;
pub mod world;

pub use combat::{
    COMBAT_SCHEMA_VERSION, CombatConfig, CombatContext, CombatError, CombatEvent, CombatLayer,
    CombatUnit, DamageApply, UnitKind, apply_land_unit_damage, combined_arms_damage,
    formation_strength, jittered_target_distance, matchup_multiplier, quality_multiplier,
    resolve_direct_engagement, resolve_proximity_contact, wrapped_distance_squared,
    wrapped_longitude_delta,
};
pub use direction::{
    DirectionField, DirectionFieldError, DirectionFieldInput, HostilityMatrix,
    build_direction_field,
};
pub use movement::{
    MOVEMENT_SCHEMA_VERSION, MovementError, MovementFactors, MovementInput, MovementOutput,
    MovementState, integrate_unit_step,
};
pub use scenario::{
    DecodedScenario, GridSpec, ScenarioError, decode_mwsc, decode_mwsc_gzip, decode_mwsc_gzip_file,
};
pub use simulation::{
    FrameSnapshot, NATIVE_TICK_SCHEMA_VERSION, ResolvedCombatOrder, ResolvedUnitOrder, Simulation,
    SimulationConfig, SimulationError, SimulationUnit, TickCounters, TickInput, UnitSnapshot,
};
pub use tactical::{
    DEFAULT_TACTICAL_CELL_SIZE, NeighborOptions, PairOptions, PairStats, PairVisit, SideKey,
    TACTICAL_GRID_SCHEMA_VERSION, TacticalCell, TacticalCellCoords, TacticalGrid,
    TacticalGridCounters, TacticalGridDimensions, TacticalGridError, TacticalUnit,
    parse_tactical_cell_key, tactical_cell_coords, tactical_cell_key, tactical_grid_dimensions,
    wrap_tactical_longitude,
};
pub use world::{WorldGridError, WorldGridView};
