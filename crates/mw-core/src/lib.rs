//! Renderer-independent Modern Wars simulation and data core.
//!
//! This crate deliberately contains no windowing, GPU, filesystem UI, or web
//! APIs. Native and future web frontends should consume snapshots from here.

pub mod direction;
pub mod scenario;
pub mod tactical;

pub use direction::{
    DirectionField, DirectionFieldError, DirectionFieldInput, HostilityMatrix,
    build_direction_field,
};
pub use scenario::{
    DecodedScenario, GridSpec, ScenarioError, decode_mwsc, decode_mwsc_gzip, decode_mwsc_gzip_file,
};
pub use tactical::{
    DEFAULT_TACTICAL_CELL_SIZE, NeighborOptions, PairOptions, PairStats, PairVisit, SideKey,
    TACTICAL_GRID_SCHEMA_VERSION, TacticalCell, TacticalCellCoords, TacticalGrid,
    TacticalGridCounters, TacticalGridDimensions, TacticalGridError, TacticalUnit,
    parse_tactical_cell_key, tactical_cell_coords, tactical_cell_key, tactical_grid_dimensions,
    wrap_tactical_longitude,
};
