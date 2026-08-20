//! Deterministic territory influence, control attribution, rendering deltas, and census.
//!
//! Callers supply already-scaled sources in authoritative order. This module owns
//! Float32 influence storage, controller hysteresis, dirty tiles, and atomically
//! committed census snapshots; it never exposes mutable map storage.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use thiserror::Error;

pub const TERRITORY_SCHEMA_VERSION: &str = "territory-control-v1";
pub const DEFAULT_TERRITORY_TILE_SIZE: usize = 32;
const CONTROL_HYSTERESIS: f64 = 0.15;
const CREDIT_THRESHOLD: f64 = 0.05;
const COUNTRY_ID_COUNT: usize = u16::MAX as usize + 1;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TerritoryError {
    #[error("grid dimensions, resolution, side count, and tile size must be positive")]
    InvalidGrid,
    #[error("array '{name}' has length {actual}, expected {expected}")]
    Length {
        name: &'static str,
        actual: usize,
        expected: usize,
    },
    #[error("side index {0} is outside the configured topology")]
    InvalidSide(usize),
    #[error("dominant controller {value} at cell {cell} is invalid")]
    InvalidController { cell: usize, value: i16 },
    #[error("non-finite or negative influence for side {side} at cell {cell}")]
    InvalidInfluence { side: usize, cell: usize },
    #[error("non-finite occupation at cell {0}")]
    InvalidOccupation(usize),
    #[error("country-to-side mapping contains invalid country id 0")]
    InvalidCountryMapping,
    #[error("hostility matrix entry {index} must be 0 or 1, received {value}")]
    InvalidHostility { index: usize, value: u8 },
    #[error("city {id} references invalid cell {cell}")]
    InvalidCityCell { id: u64, cell: usize },
    #[error("city {id} has invalid population")]
    InvalidCityPopulation { id: u64 },
    #[error("influence source {id}: {reason}")]
    InvalidSource { id: u64, reason: &'static str },
    #[error("cell or tile index {0} is outside the grid")]
    InvalidCell(usize),
    #[error("invalid committed territory state: {0}")]
    InvalidCommittedState(&'static str),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InfluenceSource {
    pub id: u64,
    pub side: usize,
    pub sovereign: u16,
    /// Zero means `beneficiary || sovereign`.
    pub beneficiary: u16,
    pub lat: f64,
    pub lng: f64,
    pub radius: f64,
    pub delta: f64,
    pub concentration_bonus: f64,
    /// Exact pre-resolved coalition membership for owner-policy gates. This is
    /// intentionally source-local: two units on the same side may carry
    /// different diplomatic/command permissions.
    #[serde(default)]
    pub owner_ally_country_ids: BTreeSet<u16>,
    /// Source-local friendly/protected owners that this source cannot displace
    /// while occupation remains within the browser's protected threshold.
    #[serde(default)]
    pub protected_owner_ids: BTreeSet<u16>,
    pub rebel_de_jure: Option<u16>,
    pub credit_de_jure: Option<u16>,
    /// Exact per-beneficiary rebel credit restrictions. Entries apply only when
    /// that country wins primary credit (including neighbor-credit selection).
    #[serde(default)]
    pub credit_de_jure_by_country: BTreeMap<u16, u16>,
    pub refuses_offense: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerritoryCity {
    pub id: u64,
    pub cell: usize,
    pub owner: u16,
    pub population: f64,
    pub capital: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerritoryMaps {
    pub land: Vec<u8>,
    pub world_control: Vec<u16>,
    pub de_jure: Vec<u16>,
    pub primary_occupier: Vec<u16>,
    pub dominant_side: Vec<i16>,
    pub occupation: Vec<f32>,
    /// One Float32 row per side.
    pub side_influence: Vec<Vec<f32>>,
}

/// Stable census markers paired with the exact live maps returned by
/// [`TerritoryControl::checkpoint_maps`]. Private tile summaries are rebuilt
/// deterministically during restore instead of becoming checkpoint authority.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerritoryCommittedState {
    pub generation: u64,
    pub commit_sequence: u64,
    pub mutation_sequence: u64,
    pub processed_tiles: usize,
    pub processed_items: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerritoryConfig {
    pub width: usize,
    pub height: usize,
    pub grid_resolution: f64,
    pub max_sides: usize,
    pub tile_size: usize,
    pub maps: TerritoryMaps,
    pub country_to_side: BTreeMap<u16, usize>,
    /// Directed row-major matrix: `matrix[from * max_sides + to] == 1`.
    pub hostility_matrix: Vec<u8>,
    pub cities: Vec<TerritoryCity>,
    /// Foreign protected cells remain blocked until `abs(occupation) > 0.1`.
    pub protected_owner_ids: BTreeSet<u16>,
    pub topology_revision: u64,
    pub world_revision: u64,
    pub city_revision: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CountryAggregate {
    pub country_id: u16,
    pub side_index: i16,
    pub owned: u64,
    pub controlled: u64,
    pub credited_territory: u64,
    pub frontline: u64,
    pub de_jure_total: u64,
    pub core_controlled: u64,
    pub core_control_ratio: f64,
    pub de_jure_not_held: u64,
    pub de_jure_control_by_side: BTreeMap<usize, u64>,
    pub de_jure_control_by_country: BTreeMap<u16, u64>,
    pub cities_total: u64,
    pub cities_controlled: u64,
    pub city_population_total: f64,
    pub city_population_controlled: f64,
    pub capitals_total: u64,
    pub capitals_held: u64,
    pub capital_held: bool,
    pub city_control_by_side: BTreeMap<usize, u64>,
    pub city_population_by_side: BTreeMap<usize, f64>,
    pub capital_control_by_side: BTreeMap<usize, u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SideAggregate {
    pub side_index: usize,
    pub country_ids: Vec<u16>,
    pub territory: u64,
    pub owned_territory: u64,
    pub home_territory_controlled: u64,
    pub frontline: u64,
    pub de_jure_cells_controlled: u64,
    pub cities_controlled: u64,
    pub city_population_controlled: f64,
    pub capitals_controlled: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TileBounds {
    pub tile: usize,
    pub min_x: usize,
    pub min_y: usize,
    pub max_x: usize,
    pub max_y: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerritoryTilePixels {
    pub bounds: TileBounds,
    /// Only this tile's clamped rectangle, row-major.
    pub pixels: Vec<u16>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerritoryRenderUpdate {
    pub full_update: bool,
    pub tiles: Vec<TerritoryTilePixels>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerritorySnapshot {
    /// Owned text avoids `&'static str` deserialization lifetime traps.
    pub schema_version: String,
    pub generation: u64,
    pub commit_sequence: u64,
    pub topology_revision: u64,
    pub world_revision: u64,
    pub city_revision: u64,
    pub processed_tiles: usize,
    pub processed_items: usize,
    pub pending_dirty_tiles_at_commit: usize,
    pub land_cells: u64,
    pub positive_occupation_cells: u64,
    pub negative_occupation_cells: u64,
    pub countries: Vec<CountryAggregate>,
    pub sides: Vec<SideAggregate>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InfluenceApplyResult {
    /// Eligible source/cell applications, including repeated cells across sources.
    pub processed_source_cells: usize,
    /// Controller transitions, including multiple transitions of one cell in a batch.
    pub controller_change_count: usize,
    /// Primary-credit transitions, including multiple transitions of one cell in a batch.
    pub credit_change_count: usize,
    pub touched_influence_cells: Vec<usize>,
    pub changed_controller_cells: Vec<usize>,
    pub changed_credit_cells: Vec<usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CensusStepResult {
    pub processed_items: usize,
    pub committed: bool,
    pub generation: Option<u64>,
    pub remaining_items: usize,
    pub pending_dirty_tiles: usize,
    pub has_snapshot: bool,
    pub snapshot: Option<Arc<TerritorySnapshot>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CensusStatus {
    pub has_snapshot: bool,
    pub commit_sequence: u64,
    pub active_generation: Option<u64>,
    pub active_processed_items: usize,
    pub active_total_items: usize,
    pub dirty_tile_indices: Vec<usize>,
    pub mutation_sequence: u64,
    pub topology_revision: u64,
    pub world_revision: u64,
    pub city_revision: u64,
    pub indexed_cities: usize,
}

#[derive(Clone, Debug, Default)]
pub struct CellStateUpdate {
    pub land: Option<u8>,
    pub world_control: Option<u16>,
    pub de_jure: Option<u16>,
    pub primary_occupier: Option<u16>,
    pub dominant_side: Option<i16>,
    pub occupation: Option<f64>,
}

#[derive(Clone, Debug)]
struct CityRef {
    id: u64,
    cell: usize,
    owner: u16,
    population: f64,
    capital: bool,
}

#[derive(Default)]
struct InfluenceApplyCounts {
    processed_source_cells: usize,
    controller_change_count: usize,
    credit_change_count: usize,
}

#[derive(Default)]
struct InfluenceCellChanges {
    touched_mask: Vec<bool>,
    controller_mask: Vec<bool>,
    credit_mask: Vec<bool>,
    touched: Vec<usize>,
    controllers: Vec<usize>,
    credits: Vec<usize>,
}

impl InfluenceCellChanges {
    fn new(total_cells: usize) -> Self {
        Self {
            touched_mask: vec![false; total_cells],
            controller_mask: vec![false; total_cells],
            credit_mask: vec![false; total_cells],
            touched: Vec::new(),
            controllers: Vec::new(),
            credits: Vec::new(),
        }
    }

    fn record_touched(&mut self, cell: usize) {
        record_cell(&mut self.touched_mask, &mut self.touched, cell);
    }

    fn record_controller(&mut self, cell: usize) {
        record_cell(&mut self.controller_mask, &mut self.controllers, cell);
    }

    fn record_credit(&mut self, cell: usize) {
        record_cell(&mut self.credit_mask, &mut self.credits, cell);
    }

    fn finish(&mut self, counts: InfluenceApplyCounts) -> InfluenceApplyResult {
        self.touched.sort_unstable();
        self.controllers.sort_unstable();
        self.credits.sort_unstable();
        let result = InfluenceApplyResult {
            processed_source_cells: counts.processed_source_cells,
            controller_change_count: counts.controller_change_count,
            credit_change_count: counts.credit_change_count,
            touched_influence_cells: self.touched.clone(),
            changed_controller_cells: self.controllers.clone(),
            changed_credit_cells: self.credits.clone(),
        };
        clear_recorded_cells(&mut self.touched_mask, &mut self.touched);
        clear_recorded_cells(&mut self.controller_mask, &mut self.controllers);
        clear_recorded_cells(&mut self.credit_mask, &mut self.credits);
        result
    }
}

#[derive(Clone, Debug, Default)]
struct CountryCounts {
    owned: i64,
    controlled: i64,
    credited_territory: i64,
    frontline: i64,
    de_jure_total: i64,
    core_controlled: i64,
    de_jure_control_by_side: BTreeMap<usize, i64>,
    de_jure_control_by_country: BTreeMap<u16, i64>,
    cities_total: i64,
    cities_controlled: i64,
    city_population_total: f64,
    city_population_controlled: f64,
    capitals_total: i64,
    capitals_held: i64,
    city_control_by_side: BTreeMap<usize, i64>,
    city_population_by_side: BTreeMap<usize, f64>,
    capital_control_by_side: BTreeMap<usize, i64>,
}

#[derive(Clone, Debug, Default)]
struct SideCounts {
    territory: i64,
    owned_territory: i64,
    home_territory_controlled: i64,
    frontline: i64,
    de_jure_cells_controlled: i64,
    cities_controlled: i64,
    city_population_controlled: f64,
    capitals_controlled: i64,
}

#[derive(Clone, Debug, Default)]
struct Aggregate {
    land_cells: i64,
    positive_occupation_cells: i64,
    negative_occupation_cells: i64,
    countries: BTreeMap<u16, CountryCounts>,
    sides: BTreeMap<usize, SideCounts>,
}

#[derive(Clone, Debug)]
struct TileWork {
    tile: usize,
    bounds: TileBounds,
    cell_offset: usize,
    cell_count: usize,
    city_offset: usize,
    cities: Vec<CityRef>,
    summary: Aggregate,
}

#[derive(Clone, Debug)]
struct ActiveGeneration {
    generation: u64,
    tile_indices: Vec<usize>,
    tile_cursor: usize,
    tile_work: Option<TileWork>,
    changed_summaries: BTreeMap<usize, Aggregate>,
    total_items: usize,
    processed_items: usize,
    started_mutation_sequence: u64,
    full_rebuild: bool,
}

pub struct TerritoryControl {
    width: usize,
    height: usize,
    grid_resolution: f64,
    max_sides: usize,
    tile_size: usize,
    maps: TerritoryMaps,
    country_to_side: BTreeMap<u16, usize>,
    country_side_index: Box<[i16]>,
    hostility_matrix: Vec<u8>,
    cities: Vec<CityRef>,
    cities_by_tile: BTreeMap<usize, Vec<CityRef>>,
    city_cell_mask: Vec<bool>,
    influence_cell_changes: InfluenceCellChanges,
    protected_owner_ids: BTreeSet<u16>,
    census_dirty: BTreeSet<usize>,
    render_dirty: BTreeSet<usize>,
    full_render_pending: bool,
    latest_render_update: Option<Arc<TerritoryRenderUpdate>>,
    // Arc keeps sparse commits shallow: cloning the tile table retains prior
    // immutable summaries instead of deep-cloning every nested aggregate map.
    committed_tiles: Vec<Option<Arc<Aggregate>>>,
    committed_aggregate: Aggregate,
    snapshot: Option<Arc<TerritorySnapshot>>,
    active_generation: Option<ActiveGeneration>,
    next_generation: u64,
    commit_sequence: u64,
    mutation_sequence: u64,
    pending_full_rebuild: bool,
    topology_revision: u64,
    world_revision: u64,
    city_revision: u64,
}

impl TerritoryControl {
    pub fn new(config: TerritoryConfig) -> Result<Self, TerritoryError> {
        validate_config(&config)?;
        let total_tiles = tile_count(config.width, config.height, config.tile_size);
        let total_cells = config.maps.land.len();
        let cities = normalize_cities(&config.cities, config.width, config.height)?;
        let (cities_by_tile, city_cell_mask) =
            index_cities(&cities, config.width, config.tile_size, total_cells);
        let country_side_index = index_country_sides(&config.country_to_side);
        let all_tiles = (0..total_tiles).collect::<BTreeSet<_>>();
        Ok(Self {
            width: config.width,
            height: config.height,
            grid_resolution: config.grid_resolution,
            max_sides: config.max_sides,
            tile_size: config.tile_size,
            maps: config.maps,
            country_to_side: config.country_to_side,
            country_side_index,
            hostility_matrix: config.hostility_matrix,
            cities,
            cities_by_tile,
            city_cell_mask,
            influence_cell_changes: InfluenceCellChanges::new(total_cells),
            protected_owner_ids: config.protected_owner_ids,
            census_dirty: all_tiles.clone(),
            render_dirty: all_tiles,
            full_render_pending: true,
            latest_render_update: None,
            committed_tiles: vec![None; total_tiles],
            committed_aggregate: Aggregate::default(),
            snapshot: None,
            active_generation: None,
            next_generation: 1,
            commit_sequence: 0,
            mutation_sequence: 0,
            pending_full_rebuild: true,
            topology_revision: config.topology_revision,
            world_revision: config.world_revision,
            city_revision: config.city_revision,
        })
    }

    /// Restore an atomically committed territory boundary. The map arrays in
    /// `config` remain the sole serialized map authority; census aggregates and
    /// per-tile summaries are rebuilt from them before the supplied markers are
    /// installed. Restore itself does not consume a generation or commit.
    pub fn restore(
        config: TerritoryConfig,
        state: TerritoryCommittedState,
    ) -> Result<Self, TerritoryError> {
        if state.generation == 0 {
            return Err(TerritoryError::InvalidCommittedState(
                "generation must be positive",
            ));
        }
        let next_generation =
            state
                .generation
                .checked_add(1)
                .ok_or(TerritoryError::InvalidCommittedState(
                    "generation cannot advance",
                ))?;
        if state.commit_sequence == 0 {
            return Err(TerritoryError::InvalidCommittedState(
                "commit sequence must be positive",
            ));
        }

        let mut control = Self::new(config)?;
        let mut aggregate = Aggregate::default();
        let mut committed_tiles = Vec::with_capacity(control.total_tiles());
        for tile in 0..control.total_tiles() {
            let mut work = control.create_tile_work(tile);
            let tile_width = work.bounds.max_x - work.bounds.min_x;
            while work.cell_offset < work.cell_count {
                let x = work.bounds.min_x + work.cell_offset % tile_width;
                let y = work.bounds.min_y + work.cell_offset / tile_width;
                control.process_cell(&mut work.summary, y * control.width + x, x, y);
                work.cell_offset += 1;
            }
            while work.city_offset < work.cities.len() {
                control.process_city(&mut work.summary, &work.cities[work.city_offset]);
                work.city_offset += 1;
            }
            apply_summary(&mut aggregate, &work.summary, 1);
            committed_tiles.push(Some(Arc::new(work.summary)));
        }

        control.next_generation = next_generation;
        control.commit_sequence = state.commit_sequence;
        control.mutation_sequence = state.mutation_sequence;
        let snapshot = Arc::new(control.build_snapshot(
            &aggregate,
            state.generation,
            state.processed_tiles,
            state.processed_items,
            0,
        ));
        control.committed_tiles = committed_tiles;
        control.committed_aggregate = aggregate;
        control.snapshot = Some(snapshot);
        control.active_generation = None;
        control.census_dirty.clear();
        control.pending_full_rebuild = false;
        // A renderer connected after restore has no prior ownership texture.
        control.render_dirty.clear();
        control.render_dirty.extend(0..control.total_tiles());
        control.full_render_pending = true;
        control.latest_render_update = None;
        Ok(control)
    }

    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn grid_resolution(&self) -> f64 {
        self.grid_resolution
    }
    pub fn max_sides(&self) -> usize {
        self.max_sides
    }
    pub fn tile_size(&self) -> usize {
        self.tile_size
    }
    pub fn tiles_wide(&self) -> usize {
        self.width.div_ceil(self.tile_size)
    }
    pub fn tiles_high(&self) -> usize {
        self.height.div_ceil(self.tile_size)
    }
    pub fn total_cells(&self) -> usize {
        self.maps.land.len()
    }
    pub fn total_tiles(&self) -> usize {
        self.tiles_wide() * self.tiles_high()
    }
    pub fn land(&self) -> &[u8] {
        &self.maps.land
    }
    pub fn world_control(&self) -> &[u16] {
        &self.maps.world_control
    }
    pub fn de_jure(&self) -> &[u16] {
        &self.maps.de_jure
    }
    pub fn primary_occupier(&self) -> &[u16] {
        &self.maps.primary_occupier
    }
    pub fn dominant_side(&self) -> &[i16] {
        &self.maps.dominant_side
    }
    pub fn occupation(&self) -> &[f32] {
        &self.maps.occupation
    }
    pub fn side_influence(&self, side: usize) -> Option<&[f32]> {
        self.maps.side_influence.get(side).map(Vec::as_slice)
    }
    pub fn all_side_influence(&self) -> &[Vec<f32>] {
        &self.maps.side_influence
    }
    /// Clone the exact live map arrays for a checkpoint. Cloning `f32` values
    /// preserves their stored bit patterns.
    pub fn checkpoint_maps(&self) -> TerritoryMaps {
        self.maps.clone()
    }
    /// Clone the complete validated static topology plus live maps needed to
    /// rebuild this territory owner at a save barrier or strategic consequence.
    pub fn checkpoint_config(&self) -> TerritoryConfig {
        TerritoryConfig {
            width: self.width,
            height: self.height,
            grid_resolution: self.grid_resolution,
            max_sides: self.max_sides,
            tile_size: self.tile_size,
            maps: self.maps.clone(),
            country_to_side: self.country_to_side.clone(),
            hostility_matrix: self.hostility_matrix.clone(),
            cities: self
                .cities
                .iter()
                .map(|city| TerritoryCity {
                    id: city.id,
                    cell: city.cell,
                    owner: city.owner,
                    population: city.population,
                    capital: city.capital,
                })
                .collect(),
            protected_owner_ids: self.protected_owner_ids.clone(),
            topology_revision: self.topology_revision,
            world_revision: self.world_revision,
            city_revision: self.city_revision,
        }
    }
    /// Return stable committed markers only when the live maps and visible
    /// census snapshot form one coherent checkpoint boundary.
    pub fn committed_state(&self) -> Option<TerritoryCommittedState> {
        if self.active_generation.is_some() || !self.census_dirty.is_empty() {
            return None;
        }
        self.snapshot
            .as_ref()
            .map(|snapshot| TerritoryCommittedState {
                generation: snapshot.generation,
                commit_sequence: snapshot.commit_sequence,
                mutation_sequence: self.mutation_sequence,
                processed_tiles: snapshot.processed_tiles,
                processed_items: snapshot.processed_items,
            })
    }
    pub fn country_to_side(&self) -> &BTreeMap<u16, usize> {
        &self.country_to_side
    }
    pub fn hostility_matrix(&self) -> &[u8] {
        &self.hostility_matrix
    }
    pub fn snapshot(&self) -> Option<Arc<TerritorySnapshot>> {
        self.snapshot.clone()
    }
    pub fn latest_render_update(&self) -> Option<Arc<TerritoryRenderUpdate>> {
        self.latest_render_update.clone()
    }
    pub fn dirty_tiles(&self) -> Vec<usize> {
        self.census_dirty.iter().copied().collect()
    }
    pub fn render_dirty_tiles(&self) -> Vec<usize> {
        self.render_dirty.iter().copied().collect()
    }

    pub fn tile_bounds(&self, tile: usize) -> Result<TileBounds, TerritoryError> {
        if tile >= self.total_tiles() {
            return Err(TerritoryError::InvalidCell(tile));
        }
        Ok(tile_bounds(tile, self.width, self.height, self.tile_size))
    }

    /// Replace influence transactionally without inferring a new boundary.
    pub fn set_side_influence(&mut self, value: Vec<Vec<f32>>) -> Result<(), TerritoryError> {
        validate_influence(&value, self.max_sides, self.total_cells())?;
        self.maps.side_influence = value;
        Ok(())
    }

    /// Transactionally write selected Float32 influence cells without implying
    /// any controller or census invalidation. Callers explicitly mark the map
    /// cells whose derived state they also changed.
    pub fn set_side_influence_cells(
        &mut self,
        side: usize,
        cells: &[(usize, f64)],
    ) -> Result<usize, TerritoryError> {
        if side >= self.max_sides {
            return Err(TerritoryError::InvalidSide(side));
        }
        for (cell, value) in cells {
            if *cell >= self.total_cells() {
                return Err(TerritoryError::InvalidCell(*cell));
            }
            if !value.is_finite() || *value < 0.0 || !(*value as f32).is_finite() {
                return Err(TerritoryError::InvalidInfluence { side, cell: *cell });
            }
        }
        for (cell, value) in cells {
            self.maps.side_influence[side][*cell] = *value as f32;
        }
        Ok(cells.len())
    }

    /// Replace all arrays transactionally. The old snapshot remains visible until commit.
    pub fn replace_maps(
        &mut self,
        maps: TerritoryMaps,
        revision: u64,
    ) -> Result<(), TerritoryError> {
        validate_maps(&maps, self.total_cells(), self.max_sides)?;
        self.maps = maps;
        self.world_revision = revision;
        self.invalidate_all_census();
        self.mark_all_render(true);
        Ok(())
    }

    /// Fixture/world-load boundary that replaces maps and clears the visible
    /// census immediately, matching the browser ledger's explicit `reset()`.
    /// Production callers that need the prior Arc during rebuild use
    /// [`Self::replace_maps`] instead.
    pub fn replace_maps_and_reset(
        &mut self,
        maps: TerritoryMaps,
        revision: u64,
    ) -> Result<(), TerritoryError> {
        validate_maps(&maps, self.total_cells(), self.max_sides)?;
        self.maps = maps;
        self.world_revision = revision;
        self.reset_census();
        self.mark_all_render(true);
        Ok(())
    }

    pub fn set_topology(
        &mut self,
        mapping: BTreeMap<u16, usize>,
        hostility: Vec<u8>,
        revision: u64,
    ) -> Result<bool, TerritoryError> {
        validate_topology(&mapping, &hostility, self.max_sides)?;
        if self.country_to_side == mapping
            && self.hostility_matrix == hostility
            && self.topology_revision == revision
        {
            return Ok(false);
        }
        self.country_to_side = mapping;
        self.country_side_index = index_country_sides(&self.country_to_side);
        self.hostility_matrix = hostility;
        self.topology_revision = revision;
        self.invalidate_all_census();
        Ok(true)
    }

    pub fn set_cities(
        &mut self,
        cities: Vec<TerritoryCity>,
        revision: u64,
    ) -> Result<(), TerritoryError> {
        let normalized = normalize_cities(&cities, self.width, self.height)?;
        let (by_tile, cell_mask) =
            index_cities(&normalized, self.width, self.tile_size, self.total_cells());
        self.cities = normalized;
        self.cities_by_tile = by_tile;
        self.city_cell_mask = cell_mask;
        self.city_revision = revision;
        self.invalidate_all_census();
        Ok(())
    }

    pub fn set_protected_owner_ids(&mut self, ids: BTreeSet<u16>) {
        self.protected_owner_ids = ids.into_iter().filter(|id| *id > 0).collect();
    }

    /// Checked external invalidation with the same exact tile accounting used by
    /// the browser ledger. Marked tiles are also published to the renderer so a
    /// preceding untracked map restore cannot leave the GPU ownership texture stale.
    pub fn mark_cells(
        &mut self,
        cells: &[usize],
        include_neighbors: bool,
    ) -> Result<usize, TerritoryError> {
        for cell in cells {
            if *cell >= self.total_cells() {
                return Err(TerritoryError::InvalidCell(*cell));
            }
        }
        let before = self.census_dirty.len();
        let prior_sequence = self.mutation_sequence;
        for cell in cells {
            self.mark_census_region(*cell, include_neighbors);
            self.mark_render_cell(*cell);
        }
        let added = self.census_dirty.len() - before;
        self.mutation_sequence = if added > 0 {
            prior_sequence.wrapping_add(1)
        } else {
            prior_sequence
        };
        Ok(added)
    }

    /// Clear only private/committed census generations while retaining all map
    /// values. Generation and commit counters intentionally continue monotonically.
    pub fn reset_census(&mut self) {
        self.active_generation = None;
        self.committed_tiles.fill(None);
        self.committed_aggregate = Aggregate::default();
        self.snapshot = None;
        self.census_dirty.clear();
        self.census_dirty.extend(0..self.total_tiles());
        self.pending_full_rebuild = true;
        self.mutation_sequence = self.mutation_sequence.wrapping_add(1);
    }

    /// Browser reset operation: clear influence/controllers and restart the
    /// private census, while preserving generation and commit sequence counters.
    pub fn reset(&mut self) {
        for influence in &mut self.maps.side_influence {
            influence.fill(0.0);
        }
        self.maps.dominant_side.fill(-1);
        self.maps.occupation.fill(0.0);
        self.reset_census();
    }

    /// Checked surgical mutation; every supplied field validates before mutation.
    pub fn set_cell_state(
        &mut self,
        cell: usize,
        update: CellStateUpdate,
    ) -> Result<bool, TerritoryError> {
        if cell >= self.total_cells() {
            return Err(TerritoryError::InvalidCell(cell));
        }
        if let Some(side) = update.dominant_side {
            validate_controller(side, self.max_sides, cell)?;
        }
        if update
            .occupation
            .is_some_and(|v| !v.is_finite() || !(v as f32).is_finite())
        {
            return Err(TerritoryError::InvalidOccupation(cell));
        }
        let before = (
            self.maps.land[cell],
            self.maps.world_control[cell],
            self.maps.de_jure[cell],
            self.maps.primary_occupier[cell],
            self.maps.dominant_side[cell],
            self.maps.occupation[cell].to_bits(),
        );
        if let Some(v) = update.land {
            self.maps.land[cell] = v;
        }
        if let Some(v) = update.world_control {
            self.maps.world_control[cell] = v;
        }
        if let Some(v) = update.de_jure {
            self.maps.de_jure[cell] = v;
        }
        if let Some(v) = update.primary_occupier {
            self.maps.primary_occupier[cell] = v;
        }
        if let Some(v) = update.dominant_side {
            self.maps.dominant_side[cell] = v;
        }
        if let Some(v) = update.occupation {
            self.maps.occupation[cell] = v as f32;
        }
        let after = (
            self.maps.land[cell],
            self.maps.world_control[cell],
            self.maps.de_jure[cell],
            self.maps.primary_occupier[cell],
            self.maps.dominant_side[cell],
            self.maps.occupation[cell].to_bits(),
        );
        let changed = before != after;
        if changed {
            self.mark_census_region(cell, true);
            if before.1 != after.1 || before.3 != after.3 {
                self.mark_render_cell(cell);
            }
        }
        Ok(changed)
    }

    /// Transactionally apply checked raw map writes without deriving controller
    /// state or dirtying census tiles. This is for restore/import adapters whose
    /// protocol carries an explicit subsequent `markCells` operation.
    pub fn set_cell_states_untracked(
        &mut self,
        updates: &[(usize, CellStateUpdate)],
    ) -> Result<usize, TerritoryError> {
        for (cell, update) in updates {
            if *cell >= self.total_cells() {
                return Err(TerritoryError::InvalidCell(*cell));
            }
            if let Some(side) = update.dominant_side {
                validate_controller(side, self.max_sides, *cell)?;
            }
            if update
                .occupation
                .is_some_and(|value| !value.is_finite() || !(value as f32).is_finite())
            {
                return Err(TerritoryError::InvalidOccupation(*cell));
            }
        }
        for (cell, update) in updates {
            if let Some(value) = update.land {
                self.maps.land[*cell] = value;
            }
            if let Some(value) = update.world_control {
                self.maps.world_control[*cell] = value;
            }
            if let Some(value) = update.de_jure {
                self.maps.de_jure[*cell] = value;
            }
            if let Some(value) = update.primary_occupier {
                self.maps.primary_occupier[*cell] = value;
            }
            if let Some(value) = update.dominant_side {
                self.maps.dominant_side[*cell] = value;
            }
            if let Some(value) = update.occupation {
                self.maps.occupation[*cell] = value as f32;
            }
        }
        Ok(updates.len())
    }

    /// Validate the entire source batch, then apply it strictly in supplied order.
    pub fn apply_influence_sources(
        &mut self,
        sources: &[InfluenceSource],
    ) -> Result<InfluenceApplyResult, TerritoryError> {
        for source in sources {
            validate_source(source, self.max_sides)?;
        }
        let mut changes = std::mem::take(&mut self.influence_cell_changes);
        let mut counts = InfluenceApplyCounts::default();
        for source in sources {
            self.apply_source(source, &mut changes, &mut counts);
        }
        let result = changes.finish(counts);
        self.influence_cell_changes = changes;
        Ok(result)
    }

    fn apply_source(
        &mut self,
        source: &InfluenceSource,
        changes: &mut InfluenceCellChanges,
        counts: &mut InfluenceApplyCounts,
    ) {
        // Exact browser floor-and-clamp bounds and lower-edge cell coordinates.
        let sy =
            (((source.lat - source.radius + 90.0) / self.grid_resolution).floor() as isize).max(0);
        let ey = (((source.lat + source.radius + 90.0) / self.grid_resolution).floor() as isize)
            .min(self.height as isize - 1);
        let sx =
            (((source.lng - source.radius + 180.0) / self.grid_resolution).floor() as isize).max(0);
        let ex = (((source.lng + source.radius + 180.0) / self.grid_resolution).floor() as isize)
            .min(self.width as isize - 1);
        if sx > ex || sy > ey {
            return;
        }
        let radius_sq = source.radius * source.radius;
        for y in sy..=ey {
            let dlat = source.lat - (y as f64 * self.grid_resolution - 90.0);
            for x in sx..=ex {
                let i = y as usize * self.width + x as usize;
                if self.maps.land[i] != 2 {
                    continue;
                }
                let dlng = source.lng - (x as f64 * self.grid_resolution - 180.0);
                let d_sq = dlat * dlat + dlng * dlng;
                if d_sq >= radius_sq {
                    continue;
                }
                let owner = self.maps.world_control[i];
                let owner_side = mapped_side(&self.country_side_index, owner);
                let owner_ally = source.owner_ally_country_ids.contains(&owner);
                if owner_side
                    .is_some_and(|s| s != source.side && !self.sides_hostile(source.side, s))
                {
                    continue;
                }
                if source.refuses_offense && !owner_ally {
                    continue;
                }
                if !owner_ally
                    && (self.protected_owner_ids.contains(&owner)
                        || source.protected_owner_ids.contains(&owner))
                    && f64::from(self.maps.occupation[i]).abs() <= 0.1
                {
                    continue;
                }
                counts.processed_source_cells += 1;

                let mut cell_delta = source.delta;
                if self.city_cell_mask[i] {
                    cell_delta *= 0.35;
                }
                let weight =
                    (1.0 - d_sq.sqrt() / source.radius).powi(2) * source.concentration_bonus;
                let current = f64::from(self.maps.side_influence[source.side][i]);
                let mut next = (current + cell_delta.abs() * weight).min(1.0);
                if source
                    .rebel_de_jure
                    .is_some_and(|id| self.maps.de_jure[i] != id)
                    && next > current
                {
                    next = current;
                }
                let old_controller = self.maps.dominant_side[i];
                let old_credit = self.maps.primary_occupier[i];
                if !owner_ally
                    && mapped_side(&self.country_side_index, old_credit) != Some(source.side)
                {
                    let neighbor = self.select_neighbor_credit(x as usize, y as usize, source.side);
                    let fallback = if source.beneficiary > 0 {
                        source.beneficiary
                    } else {
                        source.sovereign
                    };
                    let final_credit = if neighbor > 0 { neighbor } else { fallback };
                    let restriction = source
                        .credit_de_jure_by_country
                        .get(&final_credit)
                        .copied()
                        .or(source.credit_de_jure);
                    let allowed = restriction.is_none_or(|id| self.maps.de_jure[i] == id);
                    if allowed && (next > CREDIT_THRESHOLD || old_credit == 0) {
                        self.maps.primary_occupier[i] = final_credit;
                    }
                }
                if write_f32(&mut self.maps.side_influence[source.side][i], next) {
                    changes.record_touched(i);
                }
                for hostile in 0..self.max_sides {
                    if !self.sides_hostile(source.side, hostile) {
                        continue;
                    }
                    let old = f64::from(self.maps.side_influence[hostile][i]);
                    if old > 0.0
                        && write_f32(
                            &mut self.maps.side_influence[hostile][i],
                            (old - cell_delta * 0.5).max(0.0),
                        )
                    {
                        changes.record_touched(i);
                    }
                }
                // Reclaim follows the min-clamped write and intentionally has no re-clamp.
                if owner == source.sovereign {
                    let own = f64::from(self.maps.side_influence[source.side][i]);
                    if write_f32(&mut self.maps.side_influence[source.side][i], own * 1.5) {
                        changes.record_touched(i);
                    }
                }
                self.sync_occupation(i);
                let credit_changed = old_credit != self.maps.primary_occupier[i];
                let controller_changed = old_controller != self.maps.dominant_side[i];
                if credit_changed || controller_changed {
                    self.mark_census_region(i, true);
                }
                if credit_changed {
                    self.mark_render_cell(i);
                }
                if credit_changed {
                    counts.credit_change_count += 1;
                    changes.record_credit(i);
                }
                if controller_changed {
                    counts.controller_change_count += 1;
                    changes.record_controller(i);
                }
            }
        }
    }

    fn select_neighbor_credit(&self, x: usize, y: usize, side: usize) -> u16 {
        const DELTAS: [(isize, isize); 8] = [
            (0, 1),
            (0, -1),
            (1, 0),
            (-1, 0),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ];
        let mut ids = [0_u16; 8];
        let mut counts = [0_u8; 8];
        let mut unique = 0;
        for (dx, dy) in DELTAS {
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx < 0 || nx >= self.width as isize || ny < 0 || ny >= self.height as isize {
                continue;
            }
            let id = self.maps.primary_occupier[ny as usize * self.width + nx as usize];
            if id == 0 || mapped_side(&self.country_side_index, id) != Some(side) {
                continue;
            }
            if let Some(slot) = ids[..unique].iter().position(|candidate| *candidate == id) {
                counts[slot] += 1;
            } else {
                ids[unique] = id;
                counts[unique] = 1;
                unique += 1;
            }
        }
        let (mut best_id, mut best_count) = (0, 0);
        for slot in 0..unique {
            if counts[slot] >= 3 && counts[slot] > best_count {
                best_count = counts[slot];
                best_id = ids[slot];
            }
        }
        best_id
    }

    fn sync_occupation(&mut self, i: usize) {
        let (mut best_side, mut best_value) = (-1_i16, 0.0_f64);
        for side in 0..self.max_sides {
            let value = f64::from(self.maps.side_influence[side][i]);
            if value > best_value {
                best_value = value;
                best_side = side as i16;
            }
        }
        let current = self.maps.dominant_side[i];
        if best_side >= 0 {
            if current == -1 || current == best_side {
                self.maps.dominant_side[i] = best_side;
                write_f32(
                    &mut self.maps.occupation[i],
                    if best_side % 2 == 0 {
                        best_value
                    } else {
                        -best_value
                    },
                );
            } else {
                let current_value = f64::from(self.maps.side_influence[current as usize][i]);
                if best_value > current_value + CONTROL_HYSTERESIS {
                    self.maps.dominant_side[i] = best_side;
                    write_f32(
                        &mut self.maps.occupation[i],
                        if best_side % 2 == 0 {
                            best_value
                        } else {
                            -best_value
                        },
                    );
                }
            }
        } else {
            self.maps.dominant_side[i] = -1;
            write_f32(&mut self.maps.occupation[i], 0.0);
        }
    }

    pub fn drain_render_update(&mut self) -> Option<Arc<TerritoryRenderUpdate>> {
        if self.render_dirty.is_empty() {
            return None;
        }
        let dirty = std::mem::take(&mut self.render_dirty);
        let mut tiles = Vec::with_capacity(dirty.len());
        for tile in dirty {
            let bounds = tile_bounds(tile, self.width, self.height, self.tile_size);
            let mut pixels =
                Vec::with_capacity((bounds.max_x - bounds.min_x) * (bounds.max_y - bounds.min_y));
            for y in bounds.min_y..bounds.max_y {
                for x in bounds.min_x..bounds.max_x {
                    let i = y * self.width + x;
                    let primary = self.maps.primary_occupier[i];
                    pixels.push(if primary > 0 {
                        primary
                    } else {
                        self.maps.world_control[i]
                    });
                }
            }
            tiles.push(TerritoryTilePixels { bounds, pixels });
        }
        let update = Arc::new(TerritoryRenderUpdate {
            full_update: self.full_render_pending,
            tiles,
        });
        self.full_render_pending = false;
        self.latest_render_update = Some(update.clone());
        Some(update)
    }

    pub fn advance_census(&mut self, budget: usize) -> CensusStepResult {
        self.begin_generation();
        let Some(mut generation) = self.active_generation.take() else {
            return CensusStepResult {
                pending_dirty_tiles: self.census_dirty.len(),
                has_snapshot: self.snapshot.is_some(),
                ..Default::default()
            };
        };
        let generation_id = generation.generation;
        if budget == 0 {
            let remaining = generation
                .total_items
                .saturating_sub(generation.processed_items);
            self.active_generation = Some(generation);
            return CensusStepResult {
                generation: Some(generation_id),
                remaining_items: remaining,
                pending_dirty_tiles: self.census_dirty.len(),
                has_snapshot: self.snapshot.is_some(),
                ..Default::default()
            };
        }
        let (mut available, mut processed) = (budget, 0);
        loop {
            while available > 0 && generation.tile_cursor < generation.tile_indices.len() {
                if generation.tile_work.is_none() {
                    generation.tile_work = Some(
                        self.create_tile_work(generation.tile_indices[generation.tile_cursor]),
                    );
                }
                let mut work = generation.tile_work.take().expect("tile work initialized");
                let tile_width = work.bounds.max_x - work.bounds.min_x;
                while available > 0 && work.cell_offset < work.cell_count {
                    let x = work.bounds.min_x + work.cell_offset % tile_width;
                    let y = work.bounds.min_y + work.cell_offset / tile_width;
                    self.process_cell(&mut work.summary, y * self.width + x, x, y);
                    work.cell_offset += 1;
                    generation.processed_items += 1;
                    processed += 1;
                    available -= 1;
                }
                while available > 0 && work.city_offset < work.cities.len() {
                    self.process_city(&mut work.summary, &work.cities[work.city_offset]);
                    work.city_offset += 1;
                    generation.processed_items += 1;
                    processed += 1;
                    available -= 1;
                }
                if work.cell_offset == work.cell_count && work.city_offset == work.cities.len() {
                    generation.changed_summaries.insert(work.tile, work.summary);
                    generation.tile_cursor += 1;
                } else {
                    generation.tile_work = Some(work);
                }
            }
            if generation.tile_cursor < generation.tile_indices.len() {
                break;
            }
            if generation.started_mutation_sequence != self.mutation_sequence {
                self.append_dirty_tail(&mut generation);
                if available > 0 {
                    continue;
                }
                break;
            }
            let snapshot = self.commit_generation(generation);
            return CensusStepResult {
                processed_items: processed,
                committed: true,
                generation: Some(generation_id),
                remaining_items: 0,
                pending_dirty_tiles: self.census_dirty.len(),
                has_snapshot: true,
                snapshot: Some(snapshot),
            };
        }
        let remaining = generation
            .total_items
            .saturating_sub(generation.processed_items);
        self.active_generation = Some(generation);
        CensusStepResult {
            processed_items: processed,
            committed: false,
            generation: Some(generation_id),
            remaining_items: remaining,
            pending_dirty_tiles: self.census_dirty.len(),
            has_snapshot: self.snapshot.is_some(),
            snapshot: None,
        }
    }

    pub fn flush_census(&mut self, chunk_budget: usize) -> Arc<TerritorySnapshot> {
        let budget = chunk_budget.max(1);
        loop {
            if self.active_generation.is_none() && self.census_dirty.is_empty() {
                return self.snapshot.clone().expect("initial census must commit");
            }
            let result = self.advance_census(budget);
            assert!(
                result.processed_items > 0 || result.committed,
                "territory census made no deterministic progress"
            );
        }
    }

    pub fn census_status(&self) -> CensusStatus {
        CensusStatus {
            has_snapshot: self.snapshot.is_some(),
            commit_sequence: self.commit_sequence,
            active_generation: self.active_generation.as_ref().map(|g| g.generation),
            active_processed_items: self
                .active_generation
                .as_ref()
                .map_or(0, |g| g.processed_items),
            active_total_items: self.active_generation.as_ref().map_or(0, |g| g.total_items),
            dirty_tile_indices: self.census_dirty.iter().copied().collect(),
            mutation_sequence: self.mutation_sequence,
            topology_revision: self.topology_revision,
            world_revision: self.world_revision,
            city_revision: self.city_revision,
            indexed_cities: self.cities.len(),
        }
    }

    fn sides_hostile(&self, from: usize, to: usize) -> bool {
        from != to
            && from < self.max_sides
            && to < self.max_sides
            && self.hostility_matrix[from * self.max_sides + to] == 1
    }

    fn mark_census_region(&mut self, cell: usize, include_neighbors: bool) {
        let tx = (cell % self.width) / self.tile_size;
        let ty = (cell / self.width) / self.tile_size;
        let (tw, th) = (self.tiles_wide(), self.tiles_high());
        let mut added = false;
        let min_y = if include_neighbors {
            ty.saturating_sub(1)
        } else {
            ty
        };
        let max_y = if include_neighbors {
            (ty + 1).min(th - 1)
        } else {
            ty
        };
        let min_x = if include_neighbors {
            tx.saturating_sub(1)
        } else {
            tx
        };
        let max_x = if include_neighbors {
            (tx + 1).min(tw - 1)
        } else {
            tx
        };
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let tile = y * tw + x;
                added |= self.census_dirty.insert(tile);
            }
        }
        if added {
            self.mutation_sequence = self.mutation_sequence.wrapping_add(1);
        }
    }

    fn mark_render_cell(&mut self, cell: usize) {
        let tile_x = (cell % self.width) / self.tile_size;
        let tile_y = (cell / self.width) / self.tile_size;
        let tiles_wide = self.tiles_wide();
        self.render_dirty.insert(tile_y * tiles_wide + tile_x);
    }

    fn mark_all_render(&mut self, full: bool) {
        self.render_dirty.extend(0..self.total_tiles());
        self.full_render_pending |= full;
    }

    fn invalidate_all_census(&mut self) {
        self.active_generation = None;
        self.census_dirty.clear();
        self.census_dirty.extend(0..self.total_tiles());
        self.pending_full_rebuild = true;
        self.mutation_sequence = self.mutation_sequence.wrapping_add(1);
    }

    fn begin_generation(&mut self) {
        if self.active_generation.is_some() || self.census_dirty.is_empty() {
            return;
        }
        let tiles = std::mem::take(&mut self.census_dirty)
            .into_iter()
            .collect::<Vec<_>>();
        let total_items = tiles.iter().map(|tile| self.items_in_tile(*tile)).sum();
        self.active_generation = Some(ActiveGeneration {
            generation: self.next_generation,
            tile_indices: tiles,
            tile_cursor: 0,
            tile_work: None,
            changed_summaries: BTreeMap::new(),
            total_items,
            processed_items: 0,
            started_mutation_sequence: self.mutation_sequence,
            full_rebuild: self.pending_full_rebuild,
        });
        self.next_generation = self.next_generation.wrapping_add(1);
    }

    fn append_dirty_tail(&mut self, generation: &mut ActiveGeneration) {
        for tile in std::mem::take(&mut self.census_dirty) {
            generation.total_items += self.items_in_tile(tile);
            generation.tile_indices.push(tile);
        }
        generation.started_mutation_sequence = self.mutation_sequence;
    }

    fn items_in_tile(&self, tile: usize) -> usize {
        let b = tile_bounds(tile, self.width, self.height, self.tile_size);
        (b.max_x - b.min_x) * (b.max_y - b.min_y)
            + self.cities_by_tile.get(&tile).map_or(0, Vec::len)
    }

    fn create_tile_work(&self, tile: usize) -> TileWork {
        let bounds = tile_bounds(tile, self.width, self.height, self.tile_size);
        TileWork {
            tile,
            bounds,
            cell_offset: 0,
            cell_count: (bounds.max_x - bounds.min_x) * (bounds.max_y - bounds.min_y),
            city_offset: 0,
            cities: self.cities_by_tile.get(&tile).cloned().unwrap_or_default(),
            summary: Aggregate::default(),
        }
    }

    fn process_cell(&self, summary: &mut Aggregate, i: usize, x: usize, y: usize) {
        if self.maps.land[i] != 2 {
            return;
        }
        summary.land_cells += 1;
        let owner = self.maps.world_control[i];
        // The native map is always present; an explicit zero therefore remains
        // "no de-jure country" (the JS fallback applies only when the map itself
        // is absent, not when a present entry is zero).
        let core = self.maps.de_jure[i];
        let controller = self.maps.dominant_side[i];
        let owner_side = mapped_side(&self.country_side_index, owner);
        let core_side = mapped_side(&self.country_side_index, core);
        let credited = if self.maps.primary_occupier[i] > 0 {
            self.maps.primary_occupier[i]
        } else {
            owner
        };
        if self.maps.occupation[i] > 0.0 {
            summary.positive_occupation_cells += 1;
        } else if self.maps.occupation[i] < 0.0 {
            summary.negative_occupation_cells += 1;
        }
        let frontline = self.active_frontline(i, x, y, controller);
        if controller >= 0 {
            let side = summary.sides.entry(controller as usize).or_default();
            side.territory += 1;
            if frontline {
                side.frontline += 1;
            }
        }
        if owner > 0 {
            let counts = summary.countries.entry(owner).or_default();
            counts.owned += 1;
            if let Some(side) = owner_side {
                summary.sides.entry(side).or_default().owned_territory += 1;
                if controller == side as i16 {
                    counts.controlled += 1;
                    summary
                        .sides
                        .entry(side)
                        .or_default()
                        .home_territory_controlled += 1;
                    if frontline {
                        counts.frontline += 1;
                    }
                }
            }
        }
        if credited > 0 {
            summary
                .countries
                .entry(credited)
                .or_default()
                .credited_territory += 1;
        }
        if core > 0 {
            let counts = summary.countries.entry(core).or_default();
            counts.de_jure_total += 1;
            if controller >= 0 {
                increment_i64(&mut counts.de_jure_control_by_side, controller as usize, 1);
                summary
                    .sides
                    .entry(controller as usize)
                    .or_default()
                    .de_jure_cells_controlled += 1;
            }
            if credited > 0 {
                increment_i64(&mut counts.de_jure_control_by_country, credited, 1);
            }
            if core_side.is_some_and(|side| controller == side as i16) {
                counts.core_controlled += 1;
            }
        }
    }

    fn process_city(&self, summary: &mut Aggregate, city: &CityRef) {
        if self.maps.land[city.cell] != 2 {
            return;
        }
        let controller = self.maps.dominant_side[city.cell];
        let owner_side = mapped_side(&self.country_side_index, city.owner);
        if city.owner > 0 {
            let owner = summary.countries.entry(city.owner).or_default();
            owner.cities_total += 1;
            owner.city_population_total += city.population;
            if city.capital {
                owner.capitals_total += 1;
            }
            if controller >= 0 {
                increment_i64(&mut owner.city_control_by_side, controller as usize, 1);
                increment_f64(
                    &mut owner.city_population_by_side,
                    controller as usize,
                    city.population,
                );
                if city.capital {
                    increment_i64(&mut owner.capital_control_by_side, controller as usize, 1);
                }
            }
            if owner_side.is_some_and(|side| controller == side as i16) {
                owner.cities_controlled += 1;
                owner.city_population_controlled += city.population;
                if city.capital {
                    owner.capitals_held += 1;
                }
            }
        }
        if controller >= 0 {
            let side = summary.sides.entry(controller as usize).or_default();
            side.cities_controlled += 1;
            side.city_population_controlled += city.population;
            if city.capital {
                side.capitals_controlled += 1;
            }
        }
    }

    fn active_frontline(&self, i: usize, x: usize, y: usize, controller: i16) -> bool {
        if controller < 0 {
            return false;
        }
        let hostile = |neighbor: usize| {
            let other = self.maps.dominant_side[neighbor];
            other >= 0 && self.sides_hostile(controller as usize, other as usize)
        };
        (x > 0 && hostile(i - 1))
            || (x + 1 < self.width && hostile(i + 1))
            || (y > 0 && hostile(i - self.width))
            || (y + 1 < self.height && hostile(i + self.width))
    }

    fn commit_generation(&mut self, generation: ActiveGeneration) -> Arc<TerritorySnapshot> {
        let mut aggregate = if generation.full_rebuild {
            Aggregate::default()
        } else {
            self.committed_aggregate.clone()
        };
        let mut tiles = if generation.full_rebuild {
            vec![None; self.total_tiles()]
        } else {
            self.committed_tiles.clone()
        };
        for (tile, summary) in generation.changed_summaries {
            if let Some(old) = &tiles[tile] {
                apply_summary(&mut aggregate, old, -1);
            }
            apply_summary(&mut aggregate, &summary, 1);
            tiles[tile] = Some(Arc::new(summary));
        }
        self.commit_sequence = self.commit_sequence.wrapping_add(1);
        let snapshot = Arc::new(self.build_snapshot(
            &aggregate,
            generation.generation,
            generation.tile_indices.len(),
            generation.processed_items,
            self.census_dirty.len(),
        ));
        self.committed_aggregate = aggregate;
        self.committed_tiles = tiles;
        self.snapshot = Some(snapshot.clone());
        self.pending_full_rebuild = false;
        snapshot
    }

    fn build_snapshot(
        &self,
        a: &Aggregate,
        generation: u64,
        processed_tiles: usize,
        processed_items: usize,
        pending_dirty_tiles_at_commit: usize,
    ) -> TerritorySnapshot {
        let country_ids = a
            .countries
            .keys()
            .copied()
            .chain(self.country_to_side.keys().copied())
            .filter(|id| *id > 0)
            .collect::<BTreeSet<_>>();
        let countries = country_ids
            .into_iter()
            .map(|id| {
                let c = a.countries.get(&id).cloned().unwrap_or_default();
                let de_jure_total = nonnegative(c.de_jure_total);
                let core_controlled = nonnegative(c.core_controlled);
                let capitals_total = nonnegative(c.capitals_total);
                let capitals_held = nonnegative(c.capitals_held);
                CountryAggregate {
                    country_id: id,
                    side_index: self.country_side_index[id as usize],
                    owned: nonnegative(c.owned),
                    controlled: nonnegative(c.controlled),
                    credited_territory: nonnegative(c.credited_territory),
                    frontline: nonnegative(c.frontline),
                    de_jure_total,
                    core_controlled,
                    core_control_ratio: if de_jure_total > 0 {
                        (core_controlled as f64 / de_jure_total as f64).min(1.0)
                    } else {
                        0.0
                    },
                    de_jure_not_held: de_jure_total.saturating_sub(core_controlled),
                    de_jure_control_by_side: positive_i64_map(c.de_jure_control_by_side),
                    de_jure_control_by_country: positive_i64_map(c.de_jure_control_by_country),
                    cities_total: nonnegative(c.cities_total),
                    cities_controlled: nonnegative(c.cities_controlled),
                    city_population_total: c.city_population_total.max(0.0),
                    city_population_controlled: c.city_population_controlled.max(0.0),
                    capitals_total,
                    capitals_held,
                    capital_held: capitals_total == 0 || capitals_held == capitals_total,
                    city_control_by_side: positive_i64_map(c.city_control_by_side),
                    city_population_by_side: positive_f64_map(c.city_population_by_side),
                    capital_control_by_side: positive_i64_map(c.capital_control_by_side),
                }
            })
            .collect();
        let side_indices = a
            .sides
            .keys()
            .copied()
            .chain(self.country_to_side.values().copied())
            .collect::<BTreeSet<_>>();
        let sides = side_indices
            .into_iter()
            .map(|side_index| {
                let s = a.sides.get(&side_index).cloned().unwrap_or_default();
                SideAggregate {
                    side_index,
                    country_ids: self
                        .country_to_side
                        .iter()
                        .filter_map(|(id, side)| (*side == side_index).then_some(*id))
                        .collect(),
                    territory: nonnegative(s.territory),
                    owned_territory: nonnegative(s.owned_territory),
                    home_territory_controlled: nonnegative(s.home_territory_controlled),
                    frontline: nonnegative(s.frontline),
                    de_jure_cells_controlled: nonnegative(s.de_jure_cells_controlled),
                    cities_controlled: nonnegative(s.cities_controlled),
                    city_population_controlled: s.city_population_controlled.max(0.0),
                    capitals_controlled: nonnegative(s.capitals_controlled),
                }
            })
            .collect();
        TerritorySnapshot {
            schema_version: TERRITORY_SCHEMA_VERSION.to_owned(),
            generation,
            commit_sequence: self.commit_sequence,
            topology_revision: self.topology_revision,
            world_revision: self.world_revision,
            city_revision: self.city_revision,
            processed_tiles,
            processed_items,
            pending_dirty_tiles_at_commit,
            land_cells: nonnegative(a.land_cells),
            positive_occupation_cells: nonnegative(a.positive_occupation_cells),
            negative_occupation_cells: nonnegative(a.negative_occupation_cells),
            countries,
            sides,
        }
    }
}

fn validate_config(c: &TerritoryConfig) -> Result<(), TerritoryError> {
    let total = c
        .width
        .checked_mul(c.height)
        .ok_or(TerritoryError::InvalidGrid)?;
    if c.width == 0
        || c.height == 0
        || !c.grid_resolution.is_finite()
        || c.grid_resolution <= 0.0
        || c.max_sides == 0
        || c.max_sides > i16::MAX as usize
        || c.tile_size == 0
    {
        return Err(TerritoryError::InvalidGrid);
    }
    validate_maps(&c.maps, total, c.max_sides)?;
    validate_topology(&c.country_to_side, &c.hostility_matrix, c.max_sides)?;
    normalize_cities(&c.cities, c.width, c.height)?;
    Ok(())
}

fn validate_maps(m: &TerritoryMaps, total: usize, sides: usize) -> Result<(), TerritoryError> {
    for (name, actual) in [
        ("land", m.land.len()),
        ("world_control", m.world_control.len()),
        ("de_jure", m.de_jure.len()),
        ("primary_occupier", m.primary_occupier.len()),
        ("dominant_side", m.dominant_side.len()),
        ("occupation", m.occupation.len()),
    ] {
        if actual != total {
            return Err(TerritoryError::Length {
                name,
                actual,
                expected: total,
            });
        }
    }
    for (cell, value) in m.dominant_side.iter().copied().enumerate() {
        validate_controller(value, sides, cell)?;
    }
    for (cell, value) in m.occupation.iter().enumerate() {
        if !value.is_finite() {
            return Err(TerritoryError::InvalidOccupation(cell));
        }
    }
    validate_influence(&m.side_influence, sides, total)
}

fn validate_controller(value: i16, sides: usize, cell: usize) -> Result<(), TerritoryError> {
    if value < -1 || value >= sides as i16 {
        Err(TerritoryError::InvalidController { cell, value })
    } else {
        Ok(())
    }
}

fn validate_influence(
    value: &[Vec<f32>],
    sides: usize,
    total: usize,
) -> Result<(), TerritoryError> {
    if value.len() != sides {
        return Err(TerritoryError::Length {
            name: "side_influence",
            actual: value.len(),
            expected: sides,
        });
    }
    for (side, row) in value.iter().enumerate() {
        if row.len() != total {
            return Err(TerritoryError::Length {
                name: "side_influence_row",
                actual: row.len(),
                expected: total,
            });
        }
        for (cell, v) in row.iter().enumerate() {
            if !v.is_finite() || *v < 0.0 {
                return Err(TerritoryError::InvalidInfluence { side, cell });
            }
        }
    }
    Ok(())
}

fn validate_topology(
    mapping: &BTreeMap<u16, usize>,
    hostility: &[u8],
    sides: usize,
) -> Result<(), TerritoryError> {
    if mapping.contains_key(&0) {
        return Err(TerritoryError::InvalidCountryMapping);
    }
    for side in mapping.values() {
        if *side >= sides {
            return Err(TerritoryError::InvalidSide(*side));
        }
    }
    let expected = sides
        .checked_mul(sides)
        .ok_or(TerritoryError::InvalidGrid)?;
    if hostility.len() != expected {
        return Err(TerritoryError::Length {
            name: "hostility_matrix",
            actual: hostility.len(),
            expected,
        });
    }
    for (index, value) in hostility.iter().copied().enumerate() {
        if value > 1 {
            return Err(TerritoryError::InvalidHostility { index, value });
        }
    }
    Ok(())
}

fn validate_source(s: &InfluenceSource, sides: usize) -> Result<(), TerritoryError> {
    if s.side >= sides {
        return Err(TerritoryError::InvalidSide(s.side));
    }
    let fail = |reason| TerritoryError::InvalidSource { id: s.id, reason };
    if s.sovereign == 0 {
        return Err(fail("sovereign must be non-zero"));
    }
    if !s.lat.is_finite() || !s.lng.is_finite() {
        return Err(fail("coordinates must be finite"));
    }
    if !s.radius.is_finite() || s.radius <= 0.0 {
        return Err(fail("radius must be finite and positive"));
    }
    if !s.delta.is_finite() || s.delta < 0.0 {
        return Err(fail("delta must be finite and non-negative"));
    }
    if !s.concentration_bonus.is_finite() || s.concentration_bonus < 0.0 {
        return Err(fail("concentration bonus must be finite and non-negative"));
    }
    if s.rebel_de_jure == Some(0) || s.credit_de_jure == Some(0) {
        return Err(fail("de-jure restrictions must be non-zero"));
    }
    if s.owner_ally_country_ids.contains(&0) {
        return Err(fail("owner ally country ids must be non-zero"));
    }
    if s.protected_owner_ids.contains(&0) {
        return Err(fail("protected owner ids must be non-zero"));
    }
    if s.credit_de_jure_by_country
        .iter()
        .any(|(country, core)| *country == 0 || *core == 0)
    {
        return Err(fail("credit restriction country ids must be non-zero"));
    }
    Ok(())
}

fn normalize_cities(
    cities: &[TerritoryCity],
    width: usize,
    height: usize,
) -> Result<Vec<CityRef>, TerritoryError> {
    let total = width
        .checked_mul(height)
        .ok_or(TerritoryError::InvalidGrid)?;
    let mut out = Vec::with_capacity(cities.len());
    for c in cities {
        if c.cell >= total {
            return Err(TerritoryError::InvalidCityCell {
                id: c.id,
                cell: c.cell,
            });
        }
        if !c.population.is_finite() || c.population < 0.0 {
            return Err(TerritoryError::InvalidCityPopulation { id: c.id });
        }
        out.push(CityRef {
            id: c.id,
            cell: c.cell,
            owner: c.owner,
            population: c.population,
            capital: c.capital,
        });
    }
    out.sort_by_key(|c| (c.cell, c.id));
    Ok(out)
}

fn index_cities(
    cities: &[CityRef],
    width: usize,
    tile_size: usize,
    total_cells: usize,
) -> (BTreeMap<usize, Vec<CityRef>>, Vec<bool>) {
    let tw = width.div_ceil(tile_size);
    let mut by_tile = BTreeMap::<usize, Vec<CityRef>>::new();
    let mut cell_mask = vec![false; total_cells];
    for c in cities {
        let tile = ((c.cell / width) / tile_size) * tw + (c.cell % width) / tile_size;
        by_tile.entry(tile).or_default().push(c.clone());
        cell_mask[c.cell] = true;
    }
    (by_tile, cell_mask)
}

fn index_country_sides(mapping: &BTreeMap<u16, usize>) -> Box<[i16]> {
    let mut index = vec![-1; COUNTRY_ID_COUNT].into_boxed_slice();
    for (&country, &side) in mapping {
        index[country as usize] = side as i16;
    }
    index
}

#[inline]
fn mapped_side(index: &[i16], country: u16) -> Option<usize> {
    let side = index[country as usize];
    (side >= 0).then_some(side as usize)
}

#[inline]
fn record_cell(mask: &mut [bool], cells: &mut Vec<usize>, cell: usize) {
    if !mask[cell] {
        mask[cell] = true;
        cells.push(cell);
    }
}

fn clear_recorded_cells(mask: &mut [bool], cells: &mut Vec<usize>) {
    for &cell in cells.iter() {
        mask[cell] = false;
    }
    cells.clear();
}

fn tile_count(w: usize, h: usize, s: usize) -> usize {
    w.div_ceil(s) * h.div_ceil(s)
}
fn tile_bounds(tile: usize, w: usize, h: usize, s: usize) -> TileBounds {
    let tw = w.div_ceil(s);
    let x = tile % tw * s;
    let y = tile / tw * s;
    TileBounds {
        tile,
        min_x: x,
        min_y: y,
        max_x: (x + s).min(w),
        max_y: (y + s).min(h),
    }
}
fn write_f32(target: &mut f32, value: f64) -> bool {
    let v = value as f32;
    if target.to_bits() == v.to_bits() {
        false
    } else {
        *target = v;
        true
    }
}
fn increment_i64<K: Ord>(m: &mut BTreeMap<K, i64>, k: K, v: i64) {
    *m.entry(k).or_default() += v;
}
fn increment_f64<K: Ord>(m: &mut BTreeMap<K, f64>, k: K, v: f64) {
    *m.entry(k).or_default() += v;
}

fn apply_i64_map<K: Ord + Copy>(
    target: &mut BTreeMap<K, i64>,
    source: &BTreeMap<K, i64>,
    direction: i64,
) {
    for (k, v) in source {
        let next = target.get(k).copied().unwrap_or_default() + v * direction;
        if next == 0 {
            target.remove(k);
        } else {
            target.insert(*k, next);
        }
    }
}
fn apply_f64_map<K: Ord + Copy>(
    target: &mut BTreeMap<K, f64>,
    source: &BTreeMap<K, f64>,
    direction: i64,
) {
    for (k, v) in source {
        let next = target.get(k).copied().unwrap_or_default() + v * direction as f64;
        if next == 0.0 {
            target.remove(k);
        } else {
            target.insert(*k, next);
        }
    }
}

fn apply_summary(t: &mut Aggregate, s: &Aggregate, d: i64) {
    t.land_cells += s.land_cells * d;
    t.positive_occupation_cells += s.positive_occupation_cells * d;
    t.negative_occupation_cells += s.negative_occupation_cells * d;
    for (id, src) in &s.countries {
        let c = t.countries.entry(*id).or_default();
        c.owned += src.owned * d;
        c.controlled += src.controlled * d;
        c.credited_territory += src.credited_territory * d;
        c.frontline += src.frontline * d;
        c.de_jure_total += src.de_jure_total * d;
        c.core_controlled += src.core_controlled * d;
        c.cities_total += src.cities_total * d;
        c.cities_controlled += src.cities_controlled * d;
        c.city_population_total += src.city_population_total * d as f64;
        c.city_population_controlled += src.city_population_controlled * d as f64;
        c.capitals_total += src.capitals_total * d;
        c.capitals_held += src.capitals_held * d;
        apply_i64_map(
            &mut c.de_jure_control_by_side,
            &src.de_jure_control_by_side,
            d,
        );
        apply_i64_map(
            &mut c.de_jure_control_by_country,
            &src.de_jure_control_by_country,
            d,
        );
        apply_i64_map(&mut c.city_control_by_side, &src.city_control_by_side, d);
        apply_f64_map(
            &mut c.city_population_by_side,
            &src.city_population_by_side,
            d,
        );
        apply_i64_map(
            &mut c.capital_control_by_side,
            &src.capital_control_by_side,
            d,
        );
    }
    for (id, src) in &s.sides {
        let c = t.sides.entry(*id).or_default();
        c.territory += src.territory * d;
        c.owned_territory += src.owned_territory * d;
        c.home_territory_controlled += src.home_territory_controlled * d;
        c.frontline += src.frontline * d;
        c.de_jure_cells_controlled += src.de_jure_cells_controlled * d;
        c.cities_controlled += src.cities_controlled * d;
        c.city_population_controlled += src.city_population_controlled * d as f64;
        c.capitals_controlled += src.capitals_controlled * d;
    }
}
fn nonnegative(v: i64) -> u64 {
    v.max(0) as u64
}
fn positive_i64_map<K: Ord>(m: BTreeMap<K, i64>) -> BTreeMap<K, u64> {
    m.into_iter()
        .filter_map(|(k, v)| (v > 0).then_some((k, v as u64)))
        .collect()
}
fn positive_f64_map<K: Ord>(m: BTreeMap<K, f64>) -> BTreeMap<K, f64> {
    m.into_iter().filter(|(_, v)| *v > 0.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maps(w: usize, h: usize, s: usize) -> TerritoryMaps {
        let n = w * h;
        TerritoryMaps {
            land: vec![2; n],
            world_control: vec![1; n],
            de_jure: vec![1; n],
            primary_occupier: vec![0; n],
            dominant_side: vec![-1; n],
            occupation: vec![0.; n],
            side_influence: vec![vec![0.; n]; s],
        }
    }
    fn config(w: usize, h: usize, s: usize, tile: usize) -> TerritoryConfig {
        let mut hostile = vec![0; s * s];
        if s >= 2 {
            hostile[1] = 1;
            hostile[s] = 1;
        }
        TerritoryConfig {
            width: w,
            height: h,
            grid_resolution: 1.,
            max_sides: s,
            tile_size: tile,
            maps: maps(w, h, s),
            country_to_side: [(1, 0), (2, 1), (3, 0), (4, 0)]
                .into_iter()
                .filter(|(_, x)| *x < s)
                .collect(),
            hostility_matrix: hostile,
            cities: vec![],
            protected_owner_ids: BTreeSet::new(),
            topology_revision: 1,
            world_revision: 2,
            city_revision: 3,
        }
    }
    fn source(side: usize, sovereign: u16, lat: f64, lng: f64) -> InfluenceSource {
        InfluenceSource {
            id: 1,
            side,
            sovereign,
            beneficiary: sovereign,
            lat,
            lng,
            radius: 1.,
            delta: 0.2,
            concentration_bonus: 1.,
            owner_ally_country_ids: [sovereign].into_iter().collect(),
            protected_owner_ids: BTreeSet::new(),
            rebel_de_jure: None,
            credit_de_jure: None,
            credit_de_jure_by_country: BTreeMap::new(),
            refuses_offense: false,
        }
    }

    #[test]
    fn validation_and_setters_are_transactional() {
        let mut bad = config(2, 1, 2, 1);
        bad.maps.occupation[1] = f32::NAN;
        assert_eq!(
            TerritoryControl::new(bad).err(),
            Some(TerritoryError::InvalidOccupation(1))
        );
        let mut t = TerritoryControl::new(config(2, 1, 2, 1)).unwrap();
        let before = t.all_side_influence().to_vec();
        let mut invalid = before.clone();
        invalid[1][1] = f32::INFINITY;
        assert!(t.set_side_influence(invalid).is_err());
        assert_eq!(t.all_side_influence(), before);
        let world = t.world_control().to_vec();
        let mut m = maps(2, 1, 2);
        m.dominant_side[1] = 2;
        assert!(t.replace_maps(m, 9).is_err());
        assert_eq!(t.world_control(), world);
        let topology = t.country_to_side().clone();
        assert!(
            t.set_topology([(9, 2)].into_iter().collect(), vec![0; 4], 9)
                .is_err()
        );
        assert_eq!(t.country_to_side(), &topology);
        assert!(
            t.set_cities(
                vec![TerritoryCity {
                    id: 7,
                    cell: 2,
                    owner: 1,
                    population: 1.,
                    capital: false
                }],
                4
            )
            .is_err()
        );
        assert_eq!(t.census_status().indexed_cities, 0);

        let before = t.side_influence(0).unwrap().to_vec();
        assert!(
            t.set_side_influence_cells(0, &[(0, 0.25), (2, 0.5)])
                .is_err()
        );
        assert_eq!(t.side_influence(0).unwrap(), before);
    }

    #[test]
    fn f64_to_f32_overflow_and_negative_sources_are_rejected_transactionally() {
        let mut t = TerritoryControl::new(config(2, 1, 2, 1)).unwrap();

        let influence_before = t.side_influence(0).unwrap().to_vec();
        assert_eq!(
            t.set_side_influence_cells(0, &[(0, 0.25), (1, 1.0e39)]),
            Err(TerritoryError::InvalidInfluence { side: 0, cell: 1 })
        );
        assert_eq!(t.side_influence(0).unwrap(), influence_before);

        let occupation_before = t.occupation().to_vec();
        assert_eq!(
            t.set_cell_state(
                0,
                CellStateUpdate {
                    occupation: Some(1.0e39),
                    ..Default::default()
                },
            ),
            Err(TerritoryError::InvalidOccupation(0))
        );
        assert_eq!(t.occupation(), occupation_before);

        assert_eq!(
            t.set_cell_states_untracked(&[
                (
                    0,
                    CellStateUpdate {
                        occupation: Some(0.25),
                        ..Default::default()
                    },
                ),
                (
                    1,
                    CellStateUpdate {
                        occupation: Some(-1.0e39),
                        ..Default::default()
                    },
                ),
            ]),
            Err(TerritoryError::InvalidOccupation(1))
        );
        assert_eq!(t.occupation(), occupation_before);

        let mut negative = source(0, 1, -90.0, -180.0);
        negative.delta = -0.01;
        assert!(matches!(
            t.apply_influence_sources(&[negative]),
            Err(TerritoryError::InvalidSource {
                reason: "delta must be finite and non-negative",
                ..
            })
        ));
        assert_eq!(t.side_influence(0).unwrap(), influence_before);
    }

    #[test]
    fn source_batch_validates_before_writes() {
        let mut t = TerritoryControl::new(config(2, 1, 2, 1)).unwrap();
        let good = source(0, 1, -90., -180.);
        let mut bad = good.clone();
        bad.id = 2;
        bad.radius = 0.;
        assert!(t.apply_influence_sources(&[good, bad]).is_err());
        assert_eq!(t.side_influence(0).unwrap(), &[0., 0.]);
        assert_eq!(t.primary_occupier(), &[0, 0]);
    }

    #[test]
    fn exact_geometry_city_weight_and_f32_write() {
        let mut c = config(3, 1, 2, 1);
        c.cities.push(TerritoryCity {
            id: 1,
            cell: 0,
            owner: 1,
            population: 10.,
            capital: false,
        });
        let mut t = TerritoryControl::new(c).unwrap();
        let mut s = source(0, 2, -90., -180.);
        s.concentration_bonus = 0.5;
        let result = t.apply_influence_sources(&[s]).unwrap();
        let expected = (0.2_f64 * 0.35 * 0.5) as f32;
        assert_eq!(
            t.side_influence(0).unwrap()[0].to_bits(),
            expected.to_bits()
        );
        assert_eq!(t.side_influence(0).unwrap()[1], 0.);
        assert_eq!(result.touched_influence_cells, vec![0]);
    }

    #[test]
    fn overlapping_sources_preserve_exact_maps_counts_and_sorted_unique_outputs() {
        let mut c = config(3, 1, 2, 1);
        c.maps.world_control.fill(3);
        c.maps.de_jure.fill(3);
        let mut first = source(0, 1, -90.0, -178.0);
        first.radius = 0.5;
        first.delta = 0.4;
        let mut opposing = source(1, 2, -90.0, -178.0);
        opposing.id = 2;
        opposing.radius = 0.5;
        opposing.delta = 1.0;
        opposing.owner_ally_country_ids.clear();
        let mut reclaim = source(0, 1, -90.0, -178.0);
        reclaim.id = 3;
        reclaim.radius = 0.5;
        reclaim.delta = 1.0;
        let mut lower_cell = source(0, 1, -90.0, -180.0);
        lower_cell.id = 4;
        lower_cell.radius = 0.5;
        lower_cell.delta = 0.4;

        let mut t = TerritoryControl::new(c).unwrap();
        let result = t
            .apply_influence_sources(&[first, opposing, reclaim, lower_cell])
            .unwrap();
        assert_eq!(
            result,
            InfluenceApplyResult {
                processed_source_cells: 4,
                controller_change_count: 4,
                credit_change_count: 4,
                touched_influence_cells: vec![0, 2],
                changed_controller_cells: vec![0, 2],
                changed_credit_cells: vec![0, 2],
            }
        );
        assert_eq!(t.side_influence(0).unwrap(), &[0.4, 0.0, 1.0]);
        assert_eq!(t.side_influence(1).unwrap(), &[0.0, 0.0, 0.5]);
        assert_eq!(t.primary_occupier(), &[1, 0, 1]);
        assert_eq!(t.dominant_side(), &[0, -1, 0]);
        assert_eq!(t.occupation(), &[0.4, 0.0, 1.0]);

        let mut next = source(0, 1, -90.0, -179.0);
        next.radius = 0.5;
        let result = t.apply_influence_sources(&[next]).unwrap();
        assert_eq!(result.touched_influence_cells, vec![1]);
        assert_eq!(result.changed_controller_cells, vec![1]);
        assert_eq!(result.changed_credit_cells, vec![1]);
    }

    #[test]
    fn topology_and_city_updates_rebuild_hot_path_indexes() {
        let mut c = config(1, 1, 2, 1);
        c.maps.world_control[0] = 2;
        c.maps.de_jure[0] = 2;
        c.hostility_matrix.fill(0);
        let mut t = TerritoryControl::new(c).unwrap();
        let mut s = source(0, 1, -90.0, -180.0);
        s.radius = 0.5;

        let skipped = t.apply_influence_sources(&[s.clone()]).unwrap();
        assert_eq!(skipped.processed_source_cells, 0);
        assert_eq!(t.side_influence(0).unwrap(), &[0.0]);

        let mapping = [(1, 0), (2, 0)].into_iter().collect();
        assert!(t.set_topology(mapping, vec![0; 4], 2).unwrap());
        let applied = t.apply_influence_sources(&[s.clone()]).unwrap();
        assert_eq!(applied.processed_source_cells, 1);
        assert_eq!(t.side_influence(0).unwrap()[0].to_bits(), 0.2_f32.to_bits());

        t.reset();
        t.set_cities(
            vec![TerritoryCity {
                id: 9,
                cell: 0,
                owner: 2,
                population: 10.0,
                capital: false,
            }],
            4,
        )
        .unwrap();
        t.apply_influence_sources(&[s.clone()]).unwrap();
        assert_eq!(
            t.side_influence(0).unwrap()[0].to_bits(),
            ((0.2_f64 * 0.35) as f32).to_bits()
        );

        t.reset();
        t.set_cities(Vec::new(), 5).unwrap();
        t.apply_influence_sources(&[s]).unwrap();
        assert_eq!(t.side_influence(0).unwrap()[0].to_bits(), 0.2_f32.to_bits());
    }

    #[test]
    fn directed_decay_reclaim_order_and_rebel_guards() {
        let mut c = config(2, 1, 2, 1);
        c.maps.world_control = vec![1, 2];
        c.maps.de_jure = vec![1, 2];
        c.maps.side_influence[0][0] = 0.9;
        c.maps.side_influence[1][0] = 0.8;
        c.hostility_matrix = vec![0, 1, 0, 0];
        let mut t = TerritoryControl::new(c).unwrap();
        let mut s = source(0, 1, -90., -180.);
        s.delta = 0.2;
        t.apply_influence_sources(&[s]).unwrap();
        assert_eq!(t.side_influence(0).unwrap()[0], 1.5);
        assert_eq!(t.side_influence(1).unwrap()[0], (0.8_f64 - 0.1) as f32);
        let before = t.side_influence(0).unwrap()[0];
        t.apply_influence_sources(&[source(1, 2, -90., -180.)])
            .unwrap();
        assert_eq!(t.side_influence(0).unwrap()[0], before);
        let mut rebel = source(0, 1, -90., -179.);
        rebel.rebel_de_jure = Some(1);
        rebel.credit_de_jure = Some(1);
        t.apply_influence_sources(&[rebel]).unwrap();
        assert_eq!(t.side_influence(0).unwrap()[1], 0.);
        assert_eq!(t.primary_occupier()[1], 0);
    }

    #[test]
    fn neutral_refusal_nonhostile_and_protected_owners_skip() {
        let mut nonhostile = config(1, 1, 2, 1);
        nonhostile.maps.world_control[0] = 2;
        nonhostile.hostility_matrix = vec![0, 0, 1, 0];
        let mut t = TerritoryControl::new(nonhostile).unwrap();
        t.apply_influence_sources(&[source(0, 1, -90., -180.)])
            .unwrap();
        assert_eq!(t.side_influence(0).unwrap()[0], 0.);

        let mut neutral = config(1, 1, 2, 1);
        neutral.maps.world_control[0] = 0;
        let mut t = TerritoryControl::new(neutral).unwrap();
        let mut refusing = source(0, 1, -90., -180.);
        refusing.refuses_offense = true;
        t.apply_influence_sources(&[refusing]).unwrap();
        assert_eq!(t.side_influence(0).unwrap()[0], 0.);

        let mut protected = config(1, 1, 2, 1);
        protected.maps.world_control[0] = 2;
        protected.protected_owner_ids.insert(2);
        let mut t = TerritoryControl::new(protected).unwrap();
        t.apply_influence_sources(&[source(0, 1, -90., -180.)])
            .unwrap();
        assert_eq!(t.side_influence(0).unwrap()[0], 0.);
        t.set_cell_state(
            0,
            CellStateUpdate {
                occupation: Some(0.100_001),
                ..Default::default()
            },
        )
        .unwrap();
        t.apply_influence_sources(&[source(0, 1, -90., -180.)])
            .unwrap();
        assert!(t.side_influence(0).unwrap()[0] > 0.);

        // Same-side mapping is not enough: owner permission is source-local.
        let mut explicit = config(1, 1, 2, 1);
        explicit.maps.world_control[0] = 4;
        explicit.maps.primary_occupier[0] = 2;
        let mut t = TerritoryControl::new(explicit).unwrap();
        let mut restricted = source(0, 1, -90., -180.);
        restricted.refuses_offense = true;
        t.apply_influence_sources(&[restricted]).unwrap();
        assert_eq!(t.side_influence(0).unwrap()[0], 0.);
        let mut permitted = source(0, 1, -90., -180.);
        permitted.owner_ally_country_ids.insert(4);
        t.apply_influence_sources(&[permitted]).unwrap();
        assert!(t.side_influence(0).unwrap()[0] > 0.);
    }

    #[test]
    fn neighbor_credit_order_and_strict_threshold() {
        let mut c = config(3, 3, 2, 1);
        c.maps.world_control.fill(2);
        for (i, id) in [(7, 3), (1, 4), (5, 3), (3, 4), (8, 3), (0, 4)] {
            c.maps.primary_occupier[i] = id;
        }
        let mut t = TerritoryControl::new(c).unwrap();
        let mut s = source(0, 1, -89., -179.);
        s.radius = 0.5;
        s.delta = 0.06;
        t.apply_influence_sources(&[s]).unwrap();
        assert_eq!(t.primary_occupier()[4], 3);
        let mut c = config(1, 1, 2, 1);
        c.maps.world_control[0] = 2;
        c.maps.primary_occupier[0] = 2;
        let mut t = TerritoryControl::new(c).unwrap();
        let mut tiny = source(0, 1, -90., -180.);
        tiny.delta = 0.05;
        t.apply_influence_sources(&[tiny.clone()]).unwrap();
        assert_eq!(t.primary_occupier()[0], 2);
        t.set_cell_state(
            0,
            CellStateUpdate {
                primary_occupier: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        tiny.delta = 0.;
        t.apply_influence_sources(&[tiny]).unwrap();
        assert_eq!(t.primary_occupier()[0], 1);
    }

    #[test]
    fn tie_hysteresis_and_signed_occupation_are_strict() {
        let mut c = config(1, 1, 2, 1);
        c.maps.dominant_side[0] = 1;
        c.maps.occupation[0] = -0.5;
        c.maps.side_influence[0][0] = 0.65;
        c.maps.side_influence[1][0] = 0.5;
        let mut t = TerritoryControl::new(c).unwrap();
        t.sync_occupation(0);
        assert_eq!(t.dominant_side()[0], 1);
        assert_eq!(t.occupation()[0], -0.5);
        t.maps.side_influence[0][0] = 0.650_001;
        t.sync_occupation(0);
        assert_eq!(t.dominant_side()[0], 0);
        assert_eq!(t.occupation()[0], 0.650_001_f32);
        t.maps.side_influence[0][0] = 0.5;
        t.maps.side_influence[1][0] = 0.5;
        t.maps.dominant_side[0] = -1;
        t.sync_occupation(0);
        assert_eq!(t.dominant_side()[0], 0);
    }

    #[test]
    fn budget_counts_cells_and_cities_and_rescans_tail() {
        let mut c = config(2, 1, 2, 2);
        c.maps.dominant_side.fill(0);
        c.cities.push(TerritoryCity {
            id: 1,
            cell: 0,
            owner: 1,
            population: 20.,
            capital: true,
        });
        let mut t = TerritoryControl::new(c).unwrap();
        assert_eq!(t.advance_census(2).processed_items, 2);
        assert!(t.snapshot().is_none());
        t.set_cell_state(
            1,
            CellStateUpdate {
                dominant_side: Some(1),
                occupation: Some(-0.7),
                ..Default::default()
            },
        )
        .unwrap();
        let next = t.advance_census(1);
        assert_eq!(next.processed_items, 1);
        assert!(!next.committed);
        assert_eq!(next.remaining_items, 3);
        let end = t.advance_census(3);
        assert!(end.committed);
        assert_eq!(end.snapshot.unwrap().processed_items, 6);
    }

    #[test]
    fn old_arc_stays_visible_and_tile_summary_subtract_add_is_exact() {
        let mut c = config(2, 1, 2, 1);
        c.maps.dominant_side.fill(0);
        let mut t = TerritoryControl::new(c).unwrap();
        let first = t.flush_census(16);
        t.set_cell_state(
            0,
            CellStateUpdate {
                dominant_side: Some(1),
                occupation: Some(-0.5),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!t.advance_census(1).committed);
        assert!(Arc::ptr_eq(&first, &t.snapshot().unwrap()));
        let second = t.flush_census(16);
        assert_eq!(second.negative_occupation_cells, 1);
        assert_eq!(second.land_cells, 2);
        assert_eq!(second.commit_sequence, 2);

        let mut replacement = maps(2, 1, 2);
        replacement.world_control.fill(2);
        replacement.dominant_side.fill(1);
        t.replace_maps(replacement, 8).unwrap();
        assert!(!t.advance_census(1).committed);
        assert!(Arc::ptr_eq(&second, &t.snapshot().unwrap()));
        let third = t.flush_census(16);
        assert_eq!(third.world_revision, 8);
        assert_eq!(third.commit_sequence, 3);
    }

    #[test]
    fn checked_marking_and_reset_match_ledger_contract() {
        let mut t = TerritoryControl::new(config(70, 33, 2, 32)).unwrap();
        t.flush_census(10_000);
        assert_eq!(t.mark_cells(&[31], false).unwrap(), 1);
        assert_eq!(t.dirty_tiles(), vec![0]);
        t.flush_census(10_000);
        assert_eq!(t.mark_cells(&[31], true).unwrap(), 4);
        assert_eq!(t.dirty_tiles(), vec![0, 1, 3, 4]);
        let before = t.census_status();
        let maps_before = t.world_control().to_vec();
        t.reset();
        let after = t.census_status();
        assert!(!after.has_snapshot);
        assert_eq!(after.commit_sequence, before.commit_sequence);
        assert_eq!(t.world_control(), maps_before);
        assert!(
            t.all_side_influence()
                .iter()
                .flatten()
                .all(|value| *value == 0.0)
        );
        assert!(t.dominant_side().iter().all(|side| *side == -1));
    }

    #[test]
    fn aggregate_matches_cell_city_frontline_and_zero_country_rules() {
        let mut c = config(3, 1, 2, 2);
        c.maps.world_control = vec![1, 1, 0];
        c.maps.de_jure = vec![1, 2, 0];
        c.maps.primary_occupier = vec![0, 2, 0];
        c.maps.dominant_side = vec![0, 1, 1];
        c.maps.occupation = vec![0.4, -0.6, -0.2];
        c.cities = vec![
            TerritoryCity {
                id: 1,
                cell: 0,
                owner: 1,
                population: 100.5,
                capital: true,
            },
            TerritoryCity {
                id: 2,
                cell: 1,
                owner: 1,
                population: 25.,
                capital: false,
            },
            TerritoryCity {
                id: 3,
                cell: 2,
                owner: 0,
                population: 5.,
                capital: true,
            },
        ];
        let mut t = TerritoryControl::new(c).unwrap();
        let s = t.flush_census(2);
        assert_eq!(
            (
                s.land_cells,
                s.positive_occupation_cells,
                s.negative_occupation_cells
            ),
            (3, 1, 2)
        );
        assert!(s.countries.iter().all(|c| c.country_id != 0));
        let c1 = s.countries.iter().find(|c| c.country_id == 1).unwrap();
        assert_eq!(
            (c1.owned, c1.controlled, c1.credited_territory, c1.frontline),
            (2, 1, 1, 1)
        );
        assert_eq!(
            (
                c1.cities_total,
                c1.cities_controlled,
                c1.city_population_total,
                c1.capitals_held
            ),
            (2, 1, 125.5, 1)
        );
        assert_eq!(
            c1.city_control_by_side,
            [(0, 1), (1, 1)].into_iter().collect()
        );
        let c2 = s.countries.iter().find(|c| c.country_id == 2).unwrap();
        assert_eq!((c2.de_jure_total, c2.core_controlled), (1, 1));
        assert_eq!(c2.de_jure_control_by_country.get(&2), Some(&1));
        let side1 = s.sides.iter().find(|x| x.side_index == 1).unwrap();
        assert_eq!(
            (
                side1.territory,
                side1.cities_controlled,
                side1.capitals_controlled
            ),
            (2, 2, 1)
        );
    }

    #[test]
    fn render_packs_only_sorted_tile_rectangles_and_full_replacements() {
        let mut c = config(70, 33, 2, 32);
        for (i, v) in c.maps.world_control.iter_mut().enumerate() {
            *v = (i % 4 + 1) as u16;
        }
        let mut t = TerritoryControl::new(c).unwrap();
        let first = t.drain_render_update().unwrap();
        assert!(first.full_update);
        assert_eq!(first.tiles.len(), 6);
        assert_eq!(first.tiles[0].pixels.len(), 1024);
        assert_eq!(first.tiles[2].pixels.len(), 192);
        assert_eq!(first.tiles[5].pixels.len(), 6);
        t.set_cell_state(
            31,
            CellStateUpdate {
                primary_occupier: Some(4),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t.render_dirty_tiles(), vec![0]);
        let u = t.drain_render_update().unwrap();
        assert!(!u.full_update);
        assert_eq!(
            u.tiles.iter().map(|x| x.bounds.tile).collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(u.tiles[0].pixels[31], 4);
        t.replace_maps(maps(70, 33, 2), 8).unwrap();
        assert!(t.drain_render_update().unwrap().full_update);
    }

    #[test]
    fn census_neighbors_and_render_tiles_are_invalidated_independently() {
        let mut c = config(70, 33, 2, 32);
        c.maps.primary_occupier[31] = 1;
        let mut t = TerritoryControl::new(c).unwrap();
        t.flush_census(10_000);
        let _ = t.drain_render_update().unwrap();

        let mut friendly = source(0, 1, -90.0, -149.0);
        friendly.radius = 0.5;
        let result = t.apply_influence_sources(&[friendly]).unwrap();
        assert_eq!(result.controller_change_count, 1);
        assert_eq!(result.credit_change_count, 0);
        assert_eq!(t.dirty_tiles(), vec![0, 1, 3, 4]);
        assert!(t.render_dirty_tiles().is_empty());
        assert!(t.drain_render_update().is_none());

        t.flush_census(10_000);
        let mut hostile = source(1, 2, -90.0, -149.0);
        hostile.radius = 0.5;
        let result = t.apply_influence_sources(&[hostile]).unwrap();
        assert_eq!(result.credit_change_count, 1);
        assert_eq!(t.render_dirty_tiles(), vec![0]);
        assert_eq!(t.dirty_tiles(), vec![0, 1, 3, 4]);
        let update = t.drain_render_update().unwrap();
        assert_eq!(update.tiles.len(), 1);
        assert_eq!(update.tiles[0].bounds.tile, 0);

        t.flush_census(10_000);
        t.mark_cells(&[31], true).unwrap();
        assert_eq!(t.dirty_tiles(), vec![0, 1, 3, 4]);
        assert_eq!(t.render_dirty_tiles(), vec![0]);
    }

    #[test]
    fn marked_untracked_map_writes_publish_render_tiles() {
        let mut t = TerritoryControl::new(config(4, 2, 2, 2)).unwrap();
        let _ = t.drain_render_update().unwrap();
        t.set_cell_states_untracked(&[(
            3,
            CellStateUpdate {
                primary_occupier: Some(9),
                ..Default::default()
            },
        )])
        .unwrap();
        assert!(t.drain_render_update().is_none());
        t.mark_cells(&[3], false).unwrap();
        let update = t.drain_render_update().unwrap();
        assert_eq!(update.tiles.len(), 1);
        assert_eq!(update.tiles[0].bounds.tile, 1);
        assert_eq!(update.tiles[0].pixels[1], 9);
    }

    #[test]
    fn snapshot_schema_round_trips_as_owned_string() {
        let mut t = TerritoryControl::new(config(1, 1, 1, 1)).unwrap();
        let s = t.flush_census(8);
        let json = serde_json::to_string(&*s).unwrap();
        let decoded: TerritorySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.schema_version, TERRITORY_SCHEMA_VERSION);
    }

    #[test]
    fn committed_state_restore_preserves_generation_and_rebuilds_census() {
        let mut c = config(3, 2, 2, 2);
        c.maps.world_control = vec![1, 1, 2, 2, 1, 2];
        c.maps.de_jure = vec![1, 1, 2, 2, 1, 2];
        c.maps.primary_occupier = vec![1, 0, 2, 2, 1, 0];
        c.maps.dominant_side = vec![0, 0, 1, 1, 0, 1];
        c.maps.occupation = vec![0.5, 0.25, -0.75, -0.5, 0.1, -0.2];
        c.maps.side_influence = vec![
            vec![0.5, 0.25, 0.0, 0.0, 0.1, 0.0],
            vec![0.0, 0.0, 0.75, 0.5, 0.0, 0.2],
        ];
        c.cities.push(TerritoryCity {
            id: 8,
            cell: 2,
            owner: 2,
            population: 12.5,
            capital: true,
        });
        let mut original = TerritoryControl::new(c).unwrap();
        let before = original.flush_census(3);
        let maps = original.checkpoint_maps();
        let state = original.committed_state().unwrap();

        let mut restored_config = config(3, 2, 2, 2);
        restored_config.maps = maps;
        restored_config.cities.push(TerritoryCity {
            id: 8,
            cell: 2,
            owner: 2,
            population: 12.5,
            capital: true,
        });
        let restored = TerritoryControl::restore(restored_config, state).unwrap();
        assert_eq!(restored.snapshot().as_deref(), Some(before.as_ref()));
        assert_eq!(restored.committed_state(), Some(state));
        assert!(restored.dirty_tiles().is_empty());
        assert_eq!(restored.census_status().active_generation, None);
    }

    #[test]
    fn restored_census_uses_the_next_generation_for_incremental_commit() {
        let mut original = TerritoryControl::new(config(4, 1, 2, 2)).unwrap();
        original.flush_census(32);
        let state = original.committed_state().unwrap();
        let mut restored_config = config(4, 1, 2, 2);
        restored_config.maps = original.checkpoint_maps();
        let mut restored = TerritoryControl::restore(restored_config, state).unwrap();

        restored
            .set_cell_state(
                3,
                CellStateUpdate {
                    dominant_side: Some(1),
                    occupation: Some(-0.75),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(restored.committed_state().is_none());
        let next = restored.flush_census(32);
        assert_eq!(next.generation, state.generation + 1);
        assert_eq!(next.commit_sequence, state.commit_sequence + 1);
        assert_eq!(next.negative_occupation_cells, 1);
        assert_eq!(
            restored.census_status().mutation_sequence,
            state.mutation_sequence + 1
        );
    }

    #[test]
    fn restore_rejects_invalid_markers_and_maps() {
        let c = config(1, 1, 1, 1);
        let valid = TerritoryCommittedState {
            generation: 1,
            commit_sequence: 1,
            mutation_sequence: 0,
            processed_tiles: 1,
            processed_items: 1,
        };
        assert!(matches!(
            TerritoryControl::restore(
                c.clone(),
                TerritoryCommittedState {
                    generation: 0,
                    ..valid
                }
            ),
            Err(TerritoryError::InvalidCommittedState(_))
        ));
        assert!(matches!(
            TerritoryControl::restore(
                c.clone(),
                TerritoryCommittedState {
                    generation: u64::MAX,
                    ..valid
                }
            ),
            Err(TerritoryError::InvalidCommittedState(_))
        ));
        assert!(matches!(
            TerritoryControl::restore(
                c.clone(),
                TerritoryCommittedState {
                    commit_sequence: 0,
                    ..valid
                }
            ),
            Err(TerritoryError::InvalidCommittedState(_))
        ));
        let mut invalid = c;
        invalid.maps.side_influence[0].push(0.0);
        assert!(matches!(
            TerritoryControl::restore(invalid, valid),
            Err(TerritoryError::Length {
                name: "side_influence_row",
                ..
            })
        ));
    }

    #[test]
    fn restore_requests_full_render_replacement() {
        let mut original = TerritoryControl::new(config(3, 2, 2, 2)).unwrap();
        original.flush_census(32);
        let state = original.committed_state().unwrap();
        let mut restored_config = config(3, 2, 2, 2);
        restored_config.maps = original.checkpoint_maps();
        let mut restored = TerritoryControl::restore(restored_config, state).unwrap();
        let update = restored.drain_render_update().unwrap();
        assert!(update.full_update);
        assert_eq!(update.tiles.len(), restored.total_tiles());
        assert!(restored.drain_render_update().is_none());
    }

    #[test]
    fn checkpoint_maps_round_trip_exact_f32_bits() {
        let mut c = config(2, 1, 2, 1);
        c.maps.occupation = vec![f32::from_bits(0x8000_0000), f32::from_bits(0x3eaa_aaab)];
        c.maps.side_influence = vec![
            vec![f32::from_bits(0x0000_0001), f32::from_bits(0x3f7f_ffff)],
            vec![f32::from_bits(0x8000_0000), f32::from_bits(0x3d00_0001)],
        ];
        let mut original = TerritoryControl::new(c).unwrap();
        original.flush_census(16);
        let state = original.committed_state().unwrap();
        let encoded = serde_json::to_vec(&original.checkpoint_maps()).unwrap();
        let maps: TerritoryMaps = serde_json::from_slice(&encoded).unwrap();
        let occupation_bits = maps
            .occupation
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>();
        let influence_bits = maps
            .side_influence
            .iter()
            .flatten()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>();
        let mut restored_config = config(2, 1, 2, 1);
        restored_config.maps = maps;
        let restored = TerritoryControl::restore(restored_config, state).unwrap();
        assert_eq!(
            restored
                .occupation()
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            occupation_bits
        );
        assert_eq!(
            restored
                .all_side_influence()
                .iter()
                .flatten()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            influence_bits
        );
    }
}
