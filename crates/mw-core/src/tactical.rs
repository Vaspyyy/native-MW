//! Deterministic fine tactical spatial grid and same-side neighbor traversal.
//!
//! This module preserves the browser tactical grid's observable numeric and
//! traversal order. The core deliberately narrows sides to `u16`: conversion
//! from JavaScript's dynamic/string-normalized side keys belongs in frontend
//! adapters, while the live game supplies numeric `sideIndex` values.

use std::collections::BTreeMap;

use thiserror::Error;

pub const TACTICAL_GRID_SCHEMA_VERSION: &str = "1";
pub const DEFAULT_TACTICAL_CELL_SIZE: f64 = 0.6;
const JAVASCRIPT_MAX_SAFE_INTEGER: usize = 9_007_199_254_740_991;

pub type UnitIndex = usize;
pub type CellKey = usize;
pub type SideKey = u16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TacticalUnit {
    pub id: u64,
    pub side: Option<SideKey>,
    /// Raw latitude. Rebuild clamps it for placement and aggregation, while
    /// pair distance intentionally reads this raw value for browser parity.
    pub lat: f64,
    /// Raw longitude. Placement wraps it, while pair distance performs the
    /// browser's single +/-360 correction on the raw-coordinate difference.
    pub lng: f64,
    pub strength: f64,
    pub ally_weight: f64,
    pub is_armor: bool,
    pub is_support: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TacticalGridDimensions {
    pub cell_size: f64,
    pub columns: usize,
    pub rows: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TacticalCellCoords {
    pub x: usize,
    pub y: usize,
    pub key: CellKey,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TacticalCell {
    pub key: CellKey,
    pub x: usize,
    pub y: usize,
    pub side_key: SideKey,
    /// Indices into the input slice passed to the latest rebuild, in input order.
    pub units: Vec<UnitIndex>,
    pub count: usize,
    pub total_strength: f64,
    pub total_ally_weight: f64,
    pub weighted_strength: f64,
    pub centroid_lat: f64,
    pub centroid_lng: f64,
    pub armor_count: usize,
    pub support_count: usize,
    pub has_armor: bool,
    pub has_support: bool,
    sum_lat: f64,
    sum_lng_sin: f64,
    sum_lng_cos: f64,
    sum_raw_lat: f64,
    sum_raw_lng_sin: f64,
    sum_raw_lng_cos: f64,
}

impl TacticalCell {
    fn new(key: CellKey, x: usize, y: usize, side_key: SideKey) -> Self {
        Self {
            key,
            x,
            y,
            side_key,
            units: Vec::new(),
            count: 0,
            total_strength: 0.0,
            total_ally_weight: 0.0,
            weighted_strength: 0.0,
            centroid_lat: 0.0,
            centroid_lng: 0.0,
            armor_count: 0,
            support_count: 0,
            has_armor: false,
            has_support: false,
            sum_lat: 0.0,
            sum_lng_sin: 0.0,
            sum_lng_cos: 0.0,
            sum_raw_lat: 0.0,
            sum_raw_lng_sin: 0.0,
            sum_raw_lng_cos: 0.0,
        }
    }

    fn finalize(&mut self) {
        let weighted = self.total_ally_weight > 0.0;
        let divisor = if weighted {
            self.total_ally_weight
        } else {
            self.count.max(1) as f64
        };
        self.centroid_lat = if weighted {
            self.sum_lat / divisor
        } else {
            self.sum_raw_lat / divisor
        };
        let sin = if weighted {
            self.sum_lng_sin
        } else {
            self.sum_raw_lng_sin
        };
        let cos = if weighted {
            self.sum_lng_cos
        } else {
            self.sum_raw_lng_cos
        };
        self.centroid_lng =
            wrap_tactical_longitude(((sin / divisor).atan2(cos / divisor)).to_degrees());
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TacticalGridCounters {
    pub input_units: usize,
    pub inserted_units: usize,
    pub skipped_units: usize,
    pub side_count: usize,
    pub cell_count: usize,
    pub max_bucket_occupancy: usize,
    pub candidate_pairs: usize,
    pub accepted_pairs: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TacticalGrid {
    pub schema_version: &'static str,
    pub cell_size: f64,
    pub columns: usize,
    pub rows: usize,
    pub by_side: BTreeMap<SideKey, BTreeMap<CellKey, TacticalCell>>,
    pub counters: TacticalGridCounters,
    unit_snapshot: Vec<TacticalUnit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeighborOptions {
    pub radius_cells: usize,
}

impl Default for NeighborOptions {
    fn default() -> Self {
        Self { radius_cells: 1 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairOptions {
    pub radius_cells: usize,
    /// `None` means infinity. Finite values are clamped to zero.
    pub radius_sq: Option<f64>,
}

impl Default for PairOptions {
    fn default() -> Self {
        Self {
            radius_cells: 1,
            radius_sq: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PairStats {
    pub candidate_pairs: usize,
    pub accepted_pairs: usize,
    pub max_bucket_occupancy: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairVisit<'a> {
    pub left_index: UnitIndex,
    pub right_index: UnitIndex,
    pub left: &'a TacticalUnit,
    pub right: &'a TacticalUnit,
    pub distance_sq: f64,
    pub left_cell: &'a TacticalCell,
    pub right_cell: &'a TacticalCell,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TacticalGridError {
    #[error("tactical cell size must be greater than 0 and at most 180")]
    InvalidCellSize,
    #[error("tactical grid dimensions overflow addressable memory")]
    GridSizeOverflow,
    #[error("invalid tactical cell coordinates")]
    InvalidCellCoordinates,
}

pub fn wrap_tactical_longitude(lng: f64) -> f64 {
    let value = if lng.is_finite() { lng } else { 0.0 };
    ((((value + 180.0) % 360.0) + 360.0) % 360.0) - 180.0
}

pub fn tactical_grid_dimensions(
    cell_size: f64,
) -> Result<TacticalGridDimensions, TacticalGridError> {
    let size = if cell_size.is_finite() {
        cell_size
    } else {
        DEFAULT_TACTICAL_CELL_SIZE
    };
    if size <= 0.0 || size > 180.0 {
        return Err(TacticalGridError::InvalidCellSize);
    }
    let columns = checked_ceil_to_usize(360.0 / size)?;
    let rows = checked_ceil_to_usize(180.0 / size)?;
    let key_capacity = columns
        .checked_mul(rows)
        .ok_or(TacticalGridError::GridSizeOverflow)?;
    if key_capacity > JAVASCRIPT_MAX_SAFE_INTEGER {
        return Err(TacticalGridError::GridSizeOverflow);
    }
    Ok(TacticalGridDimensions {
        cell_size: size,
        columns,
        rows,
    })
}

fn checked_ceil_to_usize(value: f64) -> Result<usize, TacticalGridError> {
    let value = value.ceil();
    if !value.is_finite() || value < 0.0 || value > usize::MAX as f64 {
        return Err(TacticalGridError::GridSizeOverflow);
    }
    Ok(value as usize)
}

pub fn tactical_cell_key(x: usize, y: usize, columns: usize) -> Result<CellKey, TacticalGridError> {
    if columns == 0 || x >= columns {
        return Err(TacticalGridError::InvalidCellCoordinates);
    }
    let key = y
        .checked_mul(columns)
        .and_then(|row| row.checked_add(x))
        .ok_or(TacticalGridError::GridSizeOverflow)?;
    if key > JAVASCRIPT_MAX_SAFE_INTEGER {
        return Err(TacticalGridError::GridSizeOverflow);
    }
    Ok(key)
}

pub fn parse_tactical_cell_key(key: CellKey, columns: usize) -> Option<TacticalCellCoords> {
    if columns == 0 || key > JAVASCRIPT_MAX_SAFE_INTEGER {
        return None;
    }
    Some(TacticalCellCoords {
        x: key % columns,
        y: key / columns,
        key,
    })
}

pub fn tactical_cell_coords(
    lat: f64,
    lng: f64,
    cell_size: f64,
) -> Result<Option<TacticalCellCoords>, TacticalGridError> {
    if !lat.is_finite() || !lng.is_finite() {
        return Ok(None);
    }
    let dimensions = tactical_grid_dimensions(cell_size)?;
    coords_with_dimensions(lat, lng, dimensions).map(Some)
}

fn coords_with_dimensions(
    lat: f64,
    lng: f64,
    dimensions: TacticalGridDimensions,
) -> Result<TacticalCellCoords, TacticalGridError> {
    let normalized_lat = lat.clamp(-90.0, 90.0);
    let normalized_lng = wrap_tactical_longitude(lng);
    let x = (((normalized_lng + 180.0) / dimensions.cell_size).floor() as usize)
        .min(dimensions.columns - 1);
    let y = (((normalized_lat + 90.0) / dimensions.cell_size).floor() as usize)
        .min(dimensions.rows - 1);
    Ok(TacticalCellCoords {
        x,
        y,
        key: tactical_cell_key(x, y, dimensions.columns)?,
    })
}

impl TacticalGrid {
    pub fn new(cell_size: f64) -> Result<Self, TacticalGridError> {
        let dimensions = tactical_grid_dimensions(cell_size)?;
        Ok(Self {
            schema_version: TACTICAL_GRID_SCHEMA_VERSION,
            cell_size: dimensions.cell_size,
            columns: dimensions.columns,
            rows: dimensions.rows,
            by_side: BTreeMap::new(),
            counters: TacticalGridCounters::default(),
            unit_snapshot: Vec::new(),
        })
    }

    pub fn rebuild(&mut self, units: &[TacticalUnit]) -> Result<(), TacticalGridError> {
        self.by_side.clear();
        self.unit_snapshot.clear();
        self.unit_snapshot.extend_from_slice(units);
        self.counters = TacticalGridCounters {
            input_units: self.unit_snapshot.len(),
            ..TacticalGridCounters::default()
        };
        let dimensions = TacticalGridDimensions {
            cell_size: self.cell_size,
            columns: self.columns,
            rows: self.rows,
        };

        for (unit_index, unit) in self.unit_snapshot.iter().enumerate() {
            let Some(side_key) = unit.side else {
                self.counters.skipped_units += 1;
                continue;
            };
            if !unit.lat.is_finite() || !unit.lng.is_finite() {
                self.counters.skipped_units += 1;
                continue;
            }

            let normalized_lat = unit.lat.clamp(-90.0, 90.0);
            let normalized_lng = wrap_tactical_longitude(unit.lng);
            let coords = coords_with_dimensions(unit.lat, unit.lng, dimensions)?;
            let side_cells = self.by_side.entry(side_key).or_default();
            let is_new_cell = !side_cells.contains_key(&coords.key);
            let cell = side_cells
                .entry(coords.key)
                .or_insert_with(|| TacticalCell::new(coords.key, coords.x, coords.y, side_key));
            if is_new_cell {
                self.counters.cell_count += 1;
            }

            let strength = finite_or(unit.strength, 1.0).max(0.0);
            let ally_weight = finite_or(unit.ally_weight, 1.0).max(0.0);
            let lng_radians = normalized_lng.to_radians();
            // Keep this sequence aligned with the browser's accumulation order.
            cell.units.push(unit_index);
            cell.count += 1;
            cell.total_strength += strength;
            cell.total_ally_weight += ally_weight;
            cell.weighted_strength += strength * ally_weight;
            cell.sum_lat += normalized_lat * ally_weight;
            cell.sum_lng_sin += lng_radians.sin() * ally_weight;
            cell.sum_lng_cos += lng_radians.cos() * ally_weight;
            cell.sum_raw_lat += normalized_lat;
            cell.sum_raw_lng_sin += lng_radians.sin();
            cell.sum_raw_lng_cos += lng_radians.cos();
            if unit.is_armor {
                cell.armor_count += 1;
            }
            if unit.is_support {
                cell.support_count += 1;
            }
            cell.has_armor = cell.armor_count > 0;
            cell.has_support = cell.support_count > 0;
            self.counters.inserted_units += 1;
            self.counters.max_bucket_occupancy = self.counters.max_bucket_occupancy.max(cell.count);
        }

        self.counters.side_count = self.by_side.len();
        for side_cells in self.by_side.values_mut() {
            for cell in side_cells.values_mut() {
                cell.finalize();
            }
        }
        Ok(())
    }

    pub fn side_cells(&self, side: SideKey) -> Option<&BTreeMap<CellKey, TacticalCell>> {
        self.by_side.get(&side)
    }

    /// Immutable unit values captured by the latest rebuild. Cell unit indices
    /// and pair visits always refer to this snapshot.
    pub fn unit_snapshot(&self) -> &[TacticalUnit] {
        &self.unit_snapshot
    }

    pub fn cell(&self, side: SideKey, lat: f64, lng: f64) -> Option<&TacticalCell> {
        let coords = tactical_cell_coords(lat, lng, self.cell_size).ok()??;
        self.by_side.get(&side)?.get(&coords.key)
    }

    pub fn reset_pair_counters(&mut self) {
        self.counters.candidate_pairs = 0;
        self.counters.accepted_pairs = 0;
    }

    /// Visits occupied neighbor cells in ascending row-major key order.
    pub fn for_each_neighbor_cell<F>(
        &self,
        side: SideKey,
        origin: TacticalCellCoords,
        options: NeighborOptions,
        mut visitor: F,
    ) -> usize
    where
        F: FnMut(&TacticalCell),
    {
        let Some(side_cells) = self.by_side.get(&side) else {
            return 0;
        };
        if origin.x >= self.columns || origin.y >= self.rows {
            return 0;
        }
        if options.radius_cells <= 1 {
            let (keys, key_count) = small_neighbor_keys(
                side_cells,
                origin.x,
                origin.y,
                self.columns,
                self.rows,
                options.radius_cells,
                None,
            );
            for &key in &keys[..key_count] {
                visitor(&side_cells[&key]);
            }
            return key_count;
        }
        let mut visited = 0;
        for cell in side_cells.values() {
            if cells_are_neighbors(
                origin.x,
                origin.y,
                cell.x,
                cell.y,
                self.columns,
                options.radius_cells,
            ) {
                visitor(cell);
                visited += 1;
            }
        }
        visited
    }

    /// Streams each unordered same-side neighboring pair at most once.
    ///
    /// The predicate runs after the inclusive squared-radius test. Accepted
    /// visits are emitted immediately in canonical cell and input order, so no
    /// full pair list is allocated.
    pub fn for_each_unordered_neighbor_pair<A, F>(
        &mut self,
        side: SideKey,
        options: PairOptions,
        mut accept_pair: A,
        mut visitor: F,
    ) -> PairStats
    where
        A: FnMut(&PairVisit<'_>) -> bool,
        F: FnMut(&PairVisit<'_>),
    {
        let mut stats = PairStats {
            max_bucket_occupancy: self.counters.max_bucket_occupancy,
            ..PairStats::default()
        };
        let Some(side_cells) = self.by_side.get(&side) else {
            return stats;
        };
        let radius_sq = match options.radius_sq {
            None => f64::INFINITY,
            Some(value) => finite_or(value, 0.0).max(0.0),
        };

        for (&source_key, source_cell) in side_cells {
            let mut visit_target = |target_key: CellKey, target_cell: &TacticalCell| {
                if target_key == source_key {
                    for left_position in 0..source_cell.units.len() {
                        for right_position in left_position + 1..source_cell.units.len() {
                            visit_candidate(
                                &self.unit_snapshot,
                                source_cell.units[left_position],
                                source_cell.units[right_position],
                                source_cell,
                                target_cell,
                                radius_sq,
                                &mut stats,
                                &mut accept_pair,
                                &mut visitor,
                            );
                        }
                    }
                } else {
                    for &left_index in &source_cell.units {
                        for &right_index in &target_cell.units {
                            visit_candidate(
                                &self.unit_snapshot,
                                left_index,
                                right_index,
                                source_cell,
                                target_cell,
                                radius_sq,
                                &mut stats,
                                &mut accept_pair,
                                &mut visitor,
                            );
                        }
                    }
                }
            };
            if options.radius_cells <= 1 {
                let (keys, key_count) = small_neighbor_keys(
                    side_cells,
                    source_cell.x,
                    source_cell.y,
                    self.columns,
                    self.rows,
                    options.radius_cells,
                    Some(source_key),
                );
                for &target_key in &keys[..key_count] {
                    visit_target(target_key, &side_cells[&target_key]);
                }
            } else {
                for (&target_key, target_cell) in side_cells.range(source_key..) {
                    if cells_are_neighbors(
                        source_cell.x,
                        source_cell.y,
                        target_cell.x,
                        target_cell.y,
                        self.columns,
                        options.radius_cells,
                    ) {
                        visit_target(target_key, target_cell);
                    }
                }
            }
        }

        self.counters.candidate_pairs += stats.candidate_pairs;
        self.counters.accepted_pairs += stats.accepted_pairs;
        stats
    }
}

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}

fn cells_are_neighbors(
    origin_x: usize,
    origin_y: usize,
    target_x: usize,
    target_y: usize,
    columns: usize,
    radius: usize,
) -> bool {
    let delta_y = origin_y.abs_diff(target_y);
    if delta_y > radius {
        return false;
    }
    let direct_x = origin_x.abs_diff(target_x);
    let wrapped_x = columns - direct_x;
    direct_x.min(wrapped_x) <= radius
}

fn small_neighbor_keys(
    side_cells: &BTreeMap<CellKey, TacticalCell>,
    origin_x: usize,
    origin_y: usize,
    columns: usize,
    rows: usize,
    radius: usize,
    minimum_key: Option<CellKey>,
) -> ([CellKey; 9], usize) {
    debug_assert!(radius <= 1);
    let mut keys = [0; 9];
    let mut key_count = 0;
    let radius = radius as isize;
    for dy in -radius..=radius {
        let Some(y) = origin_y.checked_add_signed(dy) else {
            continue;
        };
        if y >= rows {
            continue;
        }
        for dx in -radius..=radius {
            let x = match dx {
                -1 if origin_x == 0 => columns - 1,
                -1 => origin_x - 1,
                0 => origin_x,
                1 if origin_x + 1 == columns => 0,
                1 => origin_x + 1,
                _ => unreachable!("small tactical radius is at most one"),
            };
            let key = y * columns + x;
            if minimum_key.is_some_and(|minimum| key < minimum)
                || !side_cells.contains_key(&key)
                || keys[..key_count].contains(&key)
            {
                continue;
            }
            keys[key_count] = key;
            key_count += 1;
        }
    }
    keys[..key_count].sort_unstable();
    (keys, key_count)
}

fn wrapped_distance_sq(left: &TacticalUnit, right: &TacticalUnit) -> f64 {
    let delta_lat = finite_or(left.lat, 0.0) - finite_or(right.lat, 0.0);
    let mut delta_lng = finite_or(left.lng, 0.0) - finite_or(right.lng, 0.0);
    if delta_lng > 180.0 {
        delta_lng -= 360.0;
    } else if delta_lng < -180.0 {
        delta_lng += 360.0;
    }
    delta_lat * delta_lat + delta_lng * delta_lng
}

#[allow(clippy::too_many_arguments)]
fn visit_candidate<A, F>(
    units: &[TacticalUnit],
    left_index: UnitIndex,
    right_index: UnitIndex,
    left_cell: &TacticalCell,
    right_cell: &TacticalCell,
    radius_sq: f64,
    stats: &mut PairStats,
    accept_pair: &mut A,
    visitor: &mut F,
) where
    A: FnMut(&PairVisit<'_>) -> bool,
    F: FnMut(&PairVisit<'_>),
{
    stats.candidate_pairs += 1;
    let left = &units[left_index];
    let right = &units[right_index];
    let distance_sq = wrapped_distance_sq(left, right);
    if distance_sq > radius_sq {
        return;
    }
    let visit = PairVisit {
        left_index,
        right_index,
        left,
        right,
        distance_sq,
        left_cell,
        right_cell,
    };
    if !accept_pair(&visit) {
        return;
    }
    stats.accepted_pairs += 1;
    visitor(&visit);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(id: u64, side: Option<u16>, lat: f64, lng: f64) -> TacticalUnit {
        TacticalUnit {
            id,
            side,
            lat,
            lng,
            strength: 1.0,
            ally_weight: 1.0,
            is_armor: false,
            is_support: false,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
    }

    #[test]
    fn dimensions_wrapping_and_cell_edges_match_browser() {
        let dimensions = tactical_grid_dimensions(0.6).unwrap();
        assert_eq!((dimensions.columns, dimensions.rows), (600, 300));
        assert_eq!(wrap_tactical_longitude(180.0), -180.0);
        assert_eq!(wrap_tactical_longitude(-181.0), 179.0);
        assert_eq!(wrap_tactical_longitude(-180.00000000000003), -180.0);
        assert_eq!(wrap_tactical_longitude(f64::NAN), 0.0);
        assert_eq!(
            tactical_cell_coords(90.0, 180.0, 0.6).unwrap().unwrap(),
            TacticalCellCoords {
                x: 0,
                y: 299,
                key: 299 * 600,
            }
        );
        assert_eq!(
            tactical_cell_coords(-100.0, 540.0, 0.6)
                .unwrap()
                .unwrap()
                .key,
            0
        );
        assert!(tactical_cell_coords(f64::NAN, 0.0, 0.6).unwrap().is_none());
        assert_eq!(parse_tactical_cell_key(1201, 600).unwrap().x, 1);
        assert!(parse_tactical_cell_key(0, 0).is_none());
        assert_eq!(
            tactical_grid_dimensions(1e-6),
            Err(TacticalGridError::GridSizeOverflow)
        );
        assert!(parse_tactical_cell_key(JAVASCRIPT_MAX_SAFE_INTEGER + 1, 600).is_none());
        assert_eq!(
            tactical_cell_coords(0.0, -180.00000000000003, 0.6)
                .unwrap()
                .unwrap()
                .x,
            0
        );
    }

    #[test]
    fn rebuild_aggregates_in_input_order_and_tracks_counters() {
        let mut units = vec![
            unit(10, Some(0), 10.0, 179.8),
            unit(11, Some(0), 20.0, -179.8),
            unit(12, None, 0.0, 0.0),
            unit(13, Some(1), f64::NAN, 0.0),
        ];
        units[0].strength = 3.0;
        units[0].ally_weight = 1.0;
        units[0].is_armor = true;
        units[1].strength = 5.0;
        units[1].ally_weight = 3.0;
        units[1].is_support = true;
        let mut grid = TacticalGrid::new(1.0).unwrap();
        grid.rebuild(&units).unwrap();
        assert_eq!(grid.counters.input_units, 4);
        assert_eq!(grid.counters.inserted_units, 2);
        assert_eq!(grid.counters.skipped_units, 2);
        assert_eq!(grid.counters.side_count, 1);
        // These longitudes occupy opposite edge cells, so inspect totals separately.
        let cells = grid.side_cells(0).unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(grid.cell(0, 10.0, 179.8).unwrap().units, vec![0]);
        assert_eq!(grid.cell(0, 20.0, -179.8).unwrap().units, vec![1]);
        assert_eq!(grid.counters.max_bucket_occupancy, 1);
    }

    #[test]
    fn weighted_and_zero_weight_centroids_match_browser_fallback() {
        let mut weighted = vec![unit(1, Some(2), 10.0, 10.0), unit(2, Some(2), 20.0, 20.0)];
        weighted[0].ally_weight = 1.0;
        weighted[1].ally_weight = 3.0;
        let mut grid = TacticalGrid::new(30.0).unwrap();
        grid.rebuild(&weighted).unwrap();
        let cell = grid.side_cells(2).unwrap().values().next().unwrap();
        assert_close(cell.centroid_lat, 17.5);

        weighted[0].ally_weight = 0.0;
        weighted[1].ally_weight = 0.0;
        grid.rebuild(&weighted).unwrap();
        let cell = grid.side_cells(2).unwrap().values().next().unwrap();
        assert_close(cell.centroid_lat, 15.0);
        assert_close(cell.centroid_lng, 15.0);
    }

    #[test]
    fn neighbor_cells_wrap_dedupe_and_visit_row_major() {
        let units = vec![
            unit(1, Some(4), -0.1, 179.9),
            unit(2, Some(4), -0.1, -179.9),
            unit(3, Some(4), 1.1, -179.9),
        ];
        let mut grid = TacticalGrid::new(1.0).unwrap();
        grid.rebuild(&units).unwrap();
        let origin = tactical_cell_coords(-0.1, 179.9, 1.0).unwrap().unwrap();
        let mut keys = Vec::new();
        let count =
            grid.for_each_neighbor_cell(4, origin, NeighborOptions { radius_cells: 361 }, |cell| {
                keys.push(cell.key)
            });
        assert_eq!(count, 3);
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn unordered_pairs_preserve_cell_and_input_order_with_inclusive_radius() {
        let units = vec![
            unit(10, Some(7), 0.0, 179.9),
            unit(11, Some(7), 0.0, -179.9),
            unit(12, Some(7), 0.0, 179.8),
            unit(13, Some(8), 0.0, 179.85),
        ];
        let mut grid = TacticalGrid::new(1.0).unwrap();
        grid.rebuild(&units).unwrap();
        let mut visits = Vec::new();
        let stats = grid.for_each_unordered_neighbor_pair(
            7,
            PairOptions {
                radius_cells: 1,
                radius_sq: Some(0.21_f64.powi(2)),
            },
            |_| true,
            |visit| visits.push((visit.left.id, visit.right.id)),
        );
        assert_eq!(stats.candidate_pairs, 3);
        assert_eq!(stats.accepted_pairs, 2);
        // Cell zero (-179.9) is canonical left across the antimeridian; the
        // final pair is the same-cell input-order pair in cell 359.
        assert_eq!(visits, vec![(11, 10), (10, 12)]);
        assert_eq!(grid.counters.candidate_pairs, 3);
        assert_eq!(grid.counters.accepted_pairs, 2);
    }

    #[test]
    fn pair_filter_and_reset_counters_work_without_materializing_pairs() {
        let units = vec![
            unit(1, Some(1), 0.0, 0.0),
            unit(2, Some(1), 0.0, 0.3),
            unit(3, Some(1), 0.0, 0.6),
        ];
        let mut grid = TacticalGrid::new(1.0).unwrap();
        grid.rebuild(&units).unwrap();
        let mut accepted_ids = Vec::new();
        let stats = grid.for_each_unordered_neighbor_pair(
            1,
            PairOptions {
                radius_cells: 1,
                radius_sq: Some(0.3_f64.powi(2)),
            },
            |visit| visit.left.id == 1,
            |visit| accepted_ids.push((visit.left.id, visit.right.id)),
        );
        assert_eq!(stats.candidate_pairs, 3);
        assert_eq!(stats.accepted_pairs, 1);
        assert_eq!(accepted_ids, vec![(1, 2)]);
        grid.reset_pair_counters();
        assert_eq!(grid.counters.candidate_pairs, 0);
        assert_eq!(grid.counters.accepted_pairs, 0);
        assert_eq!(grid.counters.inserted_units, 3);
    }

    #[test]
    fn pair_traversal_owns_the_rebuild_snapshot() {
        let mut units = vec![unit(1, Some(1), 0.0, 0.0), unit(2, Some(1), 0.0, 0.3)];
        let mut grid = TacticalGrid::new(1.0).unwrap();
        grid.rebuild(&units).unwrap();
        units.reverse();
        units.clear();

        let mut visit = None;
        grid.for_each_unordered_neighbor_pair(
            1,
            PairOptions::default(),
            |_| true,
            |pair| visit = Some((pair.left.id, pair.right.id, pair.distance_sq)),
        );
        assert_eq!(grid.unit_snapshot().len(), 2);
        let (left, right, distance_sq) = visit.unwrap();
        assert_eq!((left, right), (1, 2));
        assert_close(distance_sq, 0.09);
    }

    #[test]
    fn raw_distance_uses_only_one_longitude_correction() {
        let units = vec![unit(1, Some(1), 0.0, 720.0), unit(2, Some(1), 0.0, 0.0)];
        let mut grid = TacticalGrid::new(180.0).unwrap();
        grid.rebuild(&units).unwrap();
        let mut distance = 0.0;
        grid.for_each_unordered_neighbor_pair(
            1,
            PairOptions::default(),
            |_| true,
            |visit| distance = visit.distance_sq,
        );
        assert_eq!(distance, 360.0_f64.powi(2));
    }
}
