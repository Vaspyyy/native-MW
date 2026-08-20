//! Deterministic frontline layout and unit-slot derivation.
//!
//! This module preserves the observable ordering of the browser simulation
//! worker: frontier discovery is cell-major, local-neighbor scans use the
//! worker's bounded 5x5 offset order, component walks explicitly backtrack,
//! and all distance ties retain their original insertion order.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{ai::FrontObjective, direction::HostilityMatrix, tactical::SideKey};

pub const FRONT_LAYOUT_SCHEMA_VERSION: &str = "front-layout-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrontLayoutConfig {
    pub max_grid_cells: usize,
    pub max_sides: usize,
    pub max_units: usize,
    pub max_frontier_cells: usize,
    pub max_segments: usize,
    /// Upper bound for the unit/front distance matrix built by slot assignment.
    pub max_assignment_edges: usize,
}

impl Default for FrontLayoutConfig {
    fn default() -> Self {
        Self {
            max_grid_cells: 5_000_000,
            max_sides: 4_096,
            max_units: 100_000,
            max_frontier_cells: 5_000_000,
            max_segments: 100_000,
            max_assignment_edges: 5_000_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrontLayoutUnit {
    pub id: u64,
    pub side_index: SideKey,
    pub lat: f64,
    pub lng: f64,
    pub garrison_excluded: bool,
    pub deploy_ticks: u32,
    pub previous_pair_key: Option<String>,
    pub previous_segment_idx: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub struct FrontLayoutInput<'a> {
    pub grid_width: usize,
    pub grid_height: usize,
    pub grid_res: f64,
    pub land_mask: &'a [u8],
    pub dominant_side_map: &'a [i16],
    pub hostility: HostilityMatrix<'a>,
    pub units: &'a [FrontLayoutUnit],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrontPoint {
    pub lat: f64,
    pub lng: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrontSegment {
    pub stable_key: String,
    pub id: u64,
    pub pair: [SideKey; 2],
    pub points: Vec<FrontPoint>,
}

/// Worker-compatible slot output plus native IDs used by the AI kernel.
#[derive(Clone, Debug, PartialEq)]
pub struct FrontSlotAssignment {
    pub unit_id: u64,
    pub pair_key: Option<String>,
    pub segment_id: Option<u64>,
    pub segment_idx: Option<usize>,
    pub target_lat: Option<f64>,
    pub target_lng: Option<f64>,
    pub objective_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontLayoutPrior {
    pub unit_id: u64,
    pub pair_key: String,
    pub segment_idx: usize,
    pub objective_id: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrontLayoutCounters {
    pub grid_cells: usize,
    /// Sum of distinct cells in each pair-specific frontier set.
    pub frontier_cells: usize,
    pub segments: usize,
    pub input_units: usize,
    pub eligible_units: usize,
    pub assigned_units: usize,
    pub objectives: usize,
    pub sticky_assignments: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrontLayout {
    pub schema_version: &'static str,
    pub segments: Vec<FrontSegment>,
    pub assignments: Vec<FrontSlotAssignment>,
    pub objectives: Vec<FrontObjective>,
    pub next_prior: Vec<FrontLayoutPrior>,
    pub counters: FrontLayoutCounters,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FrontLayoutError {
    #[error("invalid frontline layout configuration")]
    InvalidConfig,
    #[error("grid dimensions must both be non-zero")]
    EmptyGrid,
    #[error("grid dimensions overflow addressable memory")]
    GridSizeOverflow,
    #[error("frontline layout exceeds configured bounds")]
    LayoutLimitExceeded,
    #[error("grid resolution must be finite and positive")]
    InvalidGridResolution,
    #[error("land mask length {actual} does not match grid size {expected}")]
    LandMaskLength { expected: usize, actual: usize },
    #[error("dominant-side map length {actual} does not match grid size {expected}")]
    DominantSideMapLength { expected: usize, actual: usize },
    #[error("hostility matrix is invalid")]
    InvalidHostility,
    #[error("front layout unit {0} contains invalid data")]
    InvalidUnit(u64),
    #[error("front layout unit id {0} is duplicated")]
    DuplicateUnit(u64),
}

#[derive(Clone, Debug)]
struct FrontierSet {
    pair: [SideKey; 2],
    cells: Vec<usize>,
    membership: BTreeSet<usize>,
}

#[derive(Clone, Copy, Debug)]
struct DistanceToFront {
    dist_sq: f64,
    nearest_index: usize,
}

#[derive(Clone, Debug)]
struct RankedFront {
    segment_index: usize,
    distance: DistanceToFront,
}

#[derive(Clone, Debug)]
struct Leftover {
    unit_index: usize,
    ranked: Vec<RankedFront>,
}

/// Derive browser-compatible polylines and slots, then materialize every valid
/// slot as a capacity-one directed objective for `resolve_ai_orders`.
pub fn derive_front_layout(
    input: FrontLayoutInput<'_>,
    config: &FrontLayoutConfig,
) -> Result<FrontLayout, FrontLayoutError> {
    let total = validate_input(input, config)?;
    let frontier_sets = collect_frontier_sets(input, config)?;
    let segments = build_polylines(input, &frontier_sets, config)?;
    let (mut assignments, sticky_assignments, eligible_units) =
        assign_frontline_slots(input, &segments, config)?;

    let segments_by_key = segments
        .iter()
        .enumerate()
        .map(|(index, segment)| (segment.stable_key.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let units_by_id = input
        .units
        .iter()
        .map(|unit| (unit.id, unit))
        .collect::<BTreeMap<_, _>>();
    let mut objectives = Vec::with_capacity(assignments.len());
    let mut next_prior = Vec::with_capacity(assignments.len());
    let mut used_objective_ids = BTreeSet::new();

    for assignment in &mut assignments {
        let (Some(pair_key), Some(segment_idx), Some(lat), Some(lng)) = (
            assignment.pair_key.as_deref(),
            assignment.segment_idx,
            assignment.target_lat,
            assignment.target_lng,
        ) else {
            continue;
        };
        let segment_index = segments_by_key[pair_key];
        let segment = &segments[segment_index];
        assignment.segment_id = Some(segment.id);
        let unit = units_by_id[&assignment.unit_id];
        let opponent = if segment.pair[0] == unit.side_index {
            segment.pair[1]
        } else if segment.pair[1] == unit.side_index {
            segment.pair[0]
        } else {
            continue;
        };
        // The worker assumes normal symmetric wars when exposing a canonical
        // pair. Keep the native objective explicitly directed for asymmetric
        // relation matrices.
        if !is_hostile(
            input.hostility,
            usize::from(unit.side_index),
            usize::from(opponent),
        ) {
            continue;
        }
        let logical_key = format!("{pair_key}|{}|{opponent}|{}", unit.side_index, unit.id);
        let objective_id =
            unique_stable_id("front-objective", &logical_key, &mut used_objective_ids);
        assignment.objective_id = Some(objective_id);
        // Construction is guaranteed by the validated grid coordinates and
        // distinct pair. Building directly keeps this adapter infallible after
        // its own validation boundary.
        objectives.push(FrontObjective {
            id: objective_id,
            side_pair: [unit.side_index, opponent],
            segment_id: segment.id,
            lat,
            lng,
            capacity: 1,
            priority: 0,
        });
        next_prior.push(FrontLayoutPrior {
            unit_id: unit.id,
            pair_key: pair_key.to_owned(),
            segment_idx,
            objective_id,
        });
    }

    let frontier_cells = frontier_sets.iter().map(|set| set.cells.len()).sum();
    let assigned_units = assignments
        .iter()
        .filter(|assignment| assignment.pair_key.is_some())
        .count();
    Ok(FrontLayout {
        schema_version: FRONT_LAYOUT_SCHEMA_VERSION,
        counters: FrontLayoutCounters {
            grid_cells: total,
            frontier_cells,
            segments: segments.len(),
            input_units: input.units.len(),
            eligible_units,
            assigned_units,
            objectives: objectives.len(),
            sticky_assignments,
        },
        segments,
        assignments,
        objectives,
        next_prior,
    })
}

fn validate_input(
    input: FrontLayoutInput<'_>,
    config: &FrontLayoutConfig,
) -> Result<usize, FrontLayoutError> {
    if config.max_grid_cells == 0
        || config.max_sides == 0
        || config.max_units == 0
        || config.max_frontier_cells == 0
        || config.max_segments == 0
        || config.max_assignment_edges == 0
    {
        return Err(FrontLayoutError::InvalidConfig);
    }
    if input.grid_width == 0 || input.grid_height == 0 {
        return Err(FrontLayoutError::EmptyGrid);
    }
    let total = input
        .grid_width
        .checked_mul(input.grid_height)
        .ok_or(FrontLayoutError::GridSizeOverflow)?;
    if total > config.max_grid_cells || input.units.len() > config.max_units {
        return Err(FrontLayoutError::LayoutLimitExceeded);
    }
    if !input.grid_res.is_finite() || input.grid_res <= 0.0 {
        return Err(FrontLayoutError::InvalidGridResolution);
    }
    if input.land_mask.len() != total {
        return Err(FrontLayoutError::LandMaskLength {
            expected: total,
            actual: input.land_mask.len(),
        });
    }
    if input.dominant_side_map.len() != total {
        return Err(FrontLayoutError::DominantSideMapLength {
            expected: total,
            actual: input.dominant_side_map.len(),
        });
    }
    if input.hostility.max_sides == 0
        || input.hostility.max_sides > config.max_sides
        || input.hostility.max_sides > i16::MAX as usize + 1
    {
        return Err(FrontLayoutError::InvalidHostility);
    }
    if let Some(relations) = input.hostility.relations {
        let expected = input
            .hostility
            .max_sides
            .checked_mul(input.hostility.max_sides)
            .ok_or(FrontLayoutError::InvalidHostility)?;
        if relations.len() != expected || relations.iter().any(|value| *value > 1) {
            return Err(FrontLayoutError::InvalidHostility);
        }
    }
    if input.land_mask.iter().any(|value| *value > 2)
        || input
            .dominant_side_map
            .iter()
            .any(|side| *side < -1 || (*side >= 0 && *side as usize >= input.hostility.max_sides))
    {
        return Err(FrontLayoutError::InvalidHostility);
    }
    let mut ids = BTreeSet::new();
    for unit in input.units {
        if usize::from(unit.side_index) >= input.hostility.max_sides
            || !unit.lat.is_finite()
            || !unit.lng.is_finite()
            || !(-90.0..=90.0).contains(&unit.lat)
            || !(-180.0..=180.0).contains(&unit.lng)
        {
            return Err(FrontLayoutError::InvalidUnit(unit.id));
        }
        if !ids.insert(unit.id) {
            return Err(FrontLayoutError::DuplicateUnit(unit.id));
        }
    }
    Ok(total)
}

fn collect_frontier_sets(
    input: FrontLayoutInput<'_>,
    config: &FrontLayoutConfig,
) -> Result<Vec<FrontierSet>, FrontLayoutError> {
    let total = input.grid_width * input.grid_height;
    let mut pair_indices = BTreeMap::<[SideKey; 2], usize>::new();
    let mut sets = Vec::<FrontierSet>::new();
    let mut frontier_cells = 0_usize;
    for index in 0..total {
        if input.land_mask[index] != 2 {
            continue;
        }
        let side = input.dominant_side_map[index];
        if side < 0 {
            continue;
        }
        let x = index % input.grid_width;
        let mut neighbors = [usize::MAX; 4];
        let mut neighbor_count = 0;
        if x + 1 < input.grid_width {
            neighbors[neighbor_count] = index + 1;
            neighbor_count += 1;
        }
        if x > 0 {
            neighbors[neighbor_count] = index - 1;
            neighbor_count += 1;
        }
        if index + input.grid_width < total {
            neighbors[neighbor_count] = index + input.grid_width;
            neighbor_count += 1;
        }
        if index >= input.grid_width {
            neighbors[neighbor_count] = index - input.grid_width;
            neighbor_count += 1;
        }
        for &neighbor in &neighbors[..neighbor_count] {
            if input.land_mask[neighbor] != 2 {
                continue;
            }
            let other_side = input.dominant_side_map[neighbor];
            if !is_hostile(input.hostility, side as usize, side_key(other_side)) {
                continue;
            }
            let side = side as SideKey;
            let other_side = other_side as SideKey;
            let pair = [side.min(other_side), side.max(other_side)];
            let set_index = match pair_indices.get(&pair).copied() {
                Some(index) => index,
                None => {
                    if sets.len() >= config.max_segments {
                        return Err(FrontLayoutError::LayoutLimitExceeded);
                    }
                    let set_index = sets.len();
                    pair_indices.insert(pair, set_index);
                    sets.push(FrontierSet {
                        pair,
                        cells: Vec::new(),
                        membership: BTreeSet::new(),
                    });
                    set_index
                }
            };
            let set = &mut sets[set_index];
            for cell in [index, neighbor] {
                if set.membership.insert(cell) {
                    set.cells.push(cell);
                    frontier_cells += 1;
                    if frontier_cells > config.max_frontier_cells {
                        return Err(FrontLayoutError::LayoutLimitExceeded);
                    }
                }
            }
            // Browser contract: only the first hostile neighbor contributes.
            break;
        }
    }
    Ok(sets)
}

fn build_polylines(
    input: FrontLayoutInput<'_>,
    frontier_sets: &[FrontierSet],
    config: &FrontLayoutConfig,
) -> Result<Vec<FrontSegment>, FrontLayoutError> {
    let offsets = build_neighbor_offsets(input.grid_width);
    let mut segments = Vec::new();
    let mut used_segment_ids = BTreeSet::new();
    for frontier_set in frontier_sets {
        let mut undiscovered = frontier_set.membership.clone();
        let mut collision_counts = BTreeMap::<String, usize>::new();
        let mut seed_cursor = 0;
        while !undiscovered.is_empty() {
            while !undiscovered.contains(&frontier_set.cells[seed_cursor]) {
                seed_cursor += 1;
            }
            let seed = frontier_set.cells[seed_cursor];
            let mut component = vec![seed];
            let mut component_membership = BTreeSet::from([seed]);
            let mut queue = vec![seed];
            undiscovered.remove(&seed);
            let mut queue_index = 0;
            while queue_index < queue.len() {
                let cell = queue[queue_index];
                queue_index += 1;
                for neighbor in local_neighbors(
                    cell,
                    &frontier_set.membership,
                    input.grid_width,
                    input.grid_height,
                    &offsets,
                ) {
                    if !undiscovered.remove(&neighbor) {
                        continue;
                    }
                    component_membership.insert(neighbor);
                    component.push(neighbor);
                    queue.push(neighbor);
                }
            }

            let mut start = seed;
            for &cell in &component {
                if local_neighbors(
                    cell,
                    &component_membership,
                    input.grid_width,
                    input.grid_height,
                    &offsets,
                )
                .len()
                    <= 1
                {
                    start = cell;
                    break;
                }
            }

            let mut points = Vec::new();
            let mut visited = BTreeSet::from([start]);
            let mut stack = vec![start];
            push_point(&mut points, start, input.grid_width, input.grid_res);
            while let Some(&current) = stack.last() {
                let mut neighbors = local_neighbors(
                    current,
                    &component_membership,
                    input.grid_width,
                    input.grid_height,
                    &offsets,
                )
                .into_iter()
                .filter(|neighbor| !visited.contains(neighbor))
                .collect::<Vec<_>>();
                if neighbors.is_empty() {
                    stack.pop();
                    if let Some(&backtrack) = stack.last() {
                        push_point(&mut points, backtrack, input.grid_width, input.grid_res);
                    }
                    continue;
                }
                let current_x = current % input.grid_width;
                let current_y = current / input.grid_width;
                // `sort_by` is stable, matching modern JavaScript's stable sort
                // when squared local distances tie.
                neighbors.sort_by(|left, right| {
                    local_distance_squared(*left, current_x, current_y, input.grid_width).cmp(
                        &local_distance_squared(*right, current_x, current_y, input.grid_width),
                    )
                });
                let next = neighbors[0];
                visited.insert(next);
                stack.push(next);
                push_point(&mut points, next, input.grid_width, input.grid_res);
            }

            let (x_sum, y_sum) = component.iter().fold((0_f64, 0_f64), |sum, cell| {
                (
                    sum.0 + (*cell % input.grid_width) as f64,
                    sum.1 + (*cell / input.grid_width) as f64,
                )
            });
            let count = component.len() as f64;
            let lat_band = (((y_sum / count) * input.grid_res - 90.0) / 10.0).floor() as i64;
            let lng_band = (((x_sum / count) * input.grid_res - 180.0) / 10.0).floor() as i64;
            let base_key = format!(
                "{}_{}_{}_{}",
                frontier_set.pair[0], frontier_set.pair[1], lat_band, lng_band
            );
            let collision_index = collision_counts.entry(base_key.clone()).or_insert(0);
            let stable_key = if *collision_index == 0 {
                base_key
            } else {
                format!("{base_key}_{collision_index}")
            };
            *collision_index += 1;
            let id = unique_stable_id("front-segment", &stable_key, &mut used_segment_ids);
            segments.push(FrontSegment {
                stable_key,
                id,
                pair: frontier_set.pair,
                points,
            });
            if segments.len() > config.max_segments {
                return Err(FrontLayoutError::LayoutLimitExceeded);
            }
        }
    }
    Ok(segments)
}

fn assign_frontline_slots(
    input: FrontLayoutInput<'_>,
    segments: &[FrontSegment],
    config: &FrontLayoutConfig,
) -> Result<(Vec<FrontSlotAssignment>, usize, usize), FrontLayoutError> {
    let mut assignments = input
        .units
        .iter()
        .map(|unit| FrontSlotAssignment {
            unit_id: unit.id,
            pair_key: None,
            segment_id: None,
            segment_idx: None,
            target_lat: None,
            target_lng: None,
            objective_id: None,
        })
        .collect::<Vec<_>>();
    let assignment_by_id = input
        .units
        .iter()
        .enumerate()
        .map(|(index, unit)| (unit.id, index))
        .collect::<BTreeMap<_, _>>();
    let side_count = input.hostility.max_sides;
    let mut side_fronts = vec![Vec::<usize>::new(); side_count];
    let mut side_units = vec![Vec::<usize>::new(); side_count];
    let front_samples = segments.iter().map(sample_front).collect::<Vec<_>>();
    for (segment_index, segment) in segments.iter().enumerate() {
        if is_hostile(
            input.hostility,
            usize::from(segment.pair[0]),
            usize::from(segment.pair[1]),
        ) && let Some(fronts) = side_fronts.get_mut(usize::from(segment.pair[0]))
        {
            fronts.push(segment_index);
        }
        if is_hostile(
            input.hostility,
            usize::from(segment.pair[1]),
            usize::from(segment.pair[0]),
        ) && let Some(fronts) = side_fronts.get_mut(usize::from(segment.pair[1]))
        {
            fronts.push(segment_index);
        }
    }
    let mut eligible_units = 0;
    for (unit_index, unit) in input.units.iter().enumerate() {
        if unit.garrison_excluded || unit.deploy_ticks > 0 {
            continue;
        }
        let side_index = usize::from(unit.side_index);
        if !side_fronts[side_index].is_empty() {
            side_units[side_index].push(unit_index);
            eligible_units += 1;
        }
    }

    let assignment_edges = side_units
        .iter()
        .enumerate()
        .try_fold(0_usize, |sum, (side, units)| {
            units
                .len()
                .checked_mul(side_fronts[side].len())
                .and_then(|edges| sum.checked_add(edges))
        })
        .ok_or(FrontLayoutError::LayoutLimitExceeded)?;
    if assignment_edges > config.max_assignment_edges {
        return Err(FrontLayoutError::LayoutLimitExceeded);
    }

    // Dense only across actual unit/front pairs. Entries for unrelated sides
    // remain absent, avoiding a global units x segments allocation.
    let mut distance_cache = BTreeMap::<(usize, usize), DistanceToFront>::new();
    let mut sticky_assignments = 0;
    for side_index in 0..side_count {
        let fronts = &side_fronts[side_index];
        let candidates = &side_units[side_index];
        if fronts.is_empty() || candidates.is_empty() {
            continue;
        }
        let total_length = fronts
            .iter()
            .map(|&segment_index| segments[segment_index].points.len())
            .sum::<usize>();
        if total_length == 0 {
            continue;
        }
        let mut sorted_fronts = fronts.clone();
        sorted_fronts.sort_by(|left, right| {
            segments[*right]
                .points
                .len()
                .cmp(&segments[*left].points.len())
        });
        let mut desired = BTreeMap::<usize, usize>::new();
        let mut desired_sum = 0;
        if candidates.len() <= fronts.len() {
            for &front in fronts {
                desired.insert(front, 0);
            }
            for &front in sorted_fronts.iter().take(candidates.len()) {
                desired.insert(front, 1);
            }
            desired_sum = candidates.len();
        } else {
            for &front in fronts {
                let count =
                    ((candidates.len() * segments[front].points.len()) / total_length).max(1);
                desired.insert(front, count);
                desired_sum += count;
            }
        }
        for index in 0..candidates.len().saturating_sub(desired_sum) {
            let front = sorted_fronts[index % sorted_fronts.len()];
            *desired.get_mut(&front).expect("front quota exists") += 1;
        }

        // Vec order is the Map insertion order from `fronts`.
        let mut assigned = fronts
            .iter()
            .map(|&front| (front, Vec::<usize>::new()))
            .collect::<Vec<_>>();
        let assigned_position = fronts
            .iter()
            .enumerate()
            .map(|(position, &front)| (front, position))
            .collect::<BTreeMap<_, _>>();
        let mut leftovers = Vec::<Leftover>::new();
        for &unit_index in candidates {
            let mut ranked = fronts
                .iter()
                .map(|&segment_index| RankedFront {
                    segment_index,
                    distance: distance_to_front(
                        unit_index,
                        segment_index,
                        input.units,
                        segments,
                        &front_samples,
                        &mut distance_cache,
                    ),
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| left.distance.dist_sq.total_cmp(&right.distance.dist_sq));
            let previous_rank = input.units[unit_index]
                .previous_pair_key
                .as_deref()
                .and_then(|key| {
                    ranked
                        .iter()
                        .find(|rank| segments[rank.segment_index].stable_key == key)
                });
            let mut placed_sticky = false;
            if let Some(previous_rank) = previous_rank {
                let position = assigned_position[&previous_rank.segment_index];
                if assigned[position].1.len() < desired[&previous_rank.segment_index]
                    && previous_rank.distance.dist_sq <= ranked[0].distance.dist_sq * 1.8 + 4.0
                {
                    assigned[position].1.push(unit_index);
                    sticky_assignments += 1;
                    placed_sticky = true;
                }
            }
            if !placed_sticky {
                leftovers.push(Leftover { unit_index, ranked });
            }
        }

        let mut ranked_pairs = leftovers
            .iter()
            .flat_map(|leftover| {
                leftover.ranked.iter().map(|rank| {
                    (
                        leftover.unit_index,
                        rank.segment_index,
                        rank.distance.dist_sq,
                    )
                })
            })
            .collect::<Vec<_>>();
        ranked_pairs.sort_by(|left, right| left.2.total_cmp(&right.2));
        let mut placed = BTreeSet::new();
        for (unit_index, segment_index, _) in ranked_pairs {
            if placed.contains(&input.units[unit_index].id) {
                continue;
            }
            let position = assigned_position[&segment_index];
            if assigned[position].1.len() >= desired[&segment_index] {
                continue;
            }
            assigned[position].1.push(unit_index);
            placed.insert(input.units[unit_index].id);
        }
        for leftover in leftovers {
            if placed.contains(&input.units[leftover.unit_index].id) {
                continue;
            }
            let segment_index = leftover.ranked[0].segment_index;
            assigned[assigned_position[&segment_index]]
                .1
                .push(leftover.unit_index);
        }

        for (segment_index, mut bucket) in assigned {
            let segment = &segments[segment_index];
            if segment.points.is_empty() || bucket.is_empty() {
                continue;
            }
            bucket.sort_by(|left, right| {
                slot_order_index(
                    *left,
                    segment_index,
                    input.units,
                    segments,
                    &front_samples,
                    &mut distance_cache,
                )
                .cmp(&slot_order_index(
                    *right,
                    segment_index,
                    input.units,
                    segments,
                    &front_samples,
                    &mut distance_cache,
                ))
            });
            let step = segment.points.len() as f64 / bucket.len() as f64;
            for (bucket_index, unit_index) in bucket.into_iter().enumerate() {
                let segment_idx = ((bucket_index as f64 + 0.5) * step).floor() as usize;
                let segment_idx = segment_idx.min(segment.points.len() - 1);
                let point = segment.points[segment_idx];
                let assignment_index = assignment_by_id[&input.units[unit_index].id];
                let assignment = &mut assignments[assignment_index];
                assignment.pair_key = Some(segment.stable_key.clone());
                assignment.segment_id = Some(segment.id);
                assignment.segment_idx = Some(segment_idx);
                assignment.target_lat = Some(point.lat);
                assignment.target_lng = Some(point.lng);
            }
        }
    }
    Ok((assignments, sticky_assignments, eligible_units))
}

fn distance_to_front(
    unit_index: usize,
    segment_index: usize,
    units: &[FrontLayoutUnit],
    segments: &[FrontSegment],
    samples: &[Vec<(FrontPoint, usize)>],
    cache: &mut BTreeMap<(usize, usize), DistanceToFront>,
) -> DistanceToFront {
    if let Some(distance) = cache.get(&(unit_index, segment_index)).copied() {
        return distance;
    }
    let unit = &units[unit_index];
    let mut best = DistanceToFront {
        dist_sq: f64::INFINITY,
        nearest_index: 0,
    };
    for &(point, index) in &samples[segment_index] {
        let dist_sq = geo_distance_squared(unit.lat, unit.lng, point.lat, point.lng);
        if dist_sq < best.dist_sq {
            best = DistanceToFront {
                dist_sq,
                nearest_index: index,
            };
        }
    }
    debug_assert!(!segments[segment_index].points.is_empty());
    cache.insert((unit_index, segment_index), best);
    best
}

fn slot_order_index(
    unit_index: usize,
    segment_index: usize,
    units: &[FrontLayoutUnit],
    segments: &[FrontSegment],
    samples: &[Vec<(FrontPoint, usize)>],
    cache: &mut BTreeMap<(usize, usize), DistanceToFront>,
) -> usize {
    let unit = &units[unit_index];
    if unit.previous_pair_key.as_deref() == Some(segments[segment_index].stable_key.as_str()) {
        unit.previous_segment_idx.unwrap_or(0)
    } else {
        distance_to_front(unit_index, segment_index, units, segments, samples, cache).nearest_index
    }
}

fn build_neighbor_offsets(grid_width: usize) -> Vec<(isize, isize, isize)> {
    let mut offsets = Vec::with_capacity(24);
    for delta_y in -2_isize..=2 {
        for delta_x in -2_isize..=2 {
            if delta_x == 0 && delta_y == 0 {
                continue;
            }
            if delta_x * delta_x + delta_y * delta_y >= 9 {
                continue;
            }
            offsets.push((delta_x, delta_y, delta_y * grid_width as isize + delta_x));
        }
    }
    offsets
}

fn local_neighbors(
    cell: usize,
    cell_set: &BTreeSet<usize>,
    grid_width: usize,
    grid_height: usize,
    offsets: &[(isize, isize, isize)],
) -> Vec<usize> {
    let x = cell % grid_width;
    let y = cell / grid_width;
    let mut neighbors = Vec::new();
    for &(delta_x, delta_y, offset) in offsets {
        let next_x = x as isize + delta_x;
        let next_y = y as isize + delta_y;
        if next_x < 0
            || next_x >= grid_width as isize
            || next_y < 0
            || next_y >= grid_height as isize
        {
            continue;
        }
        let neighbor = (cell as isize + offset) as usize;
        if cell_set.contains(&neighbor) {
            neighbors.push(neighbor);
        }
    }
    neighbors
}

fn push_point(points: &mut Vec<FrontPoint>, cell: usize, grid_width: usize, grid_res: f64) {
    let y = cell / grid_width;
    let x = cell % grid_width;
    points.push(FrontPoint {
        lat: y as f64 * grid_res - 90.0,
        lng: x as f64 * grid_res - 180.0,
    });
}

fn local_distance_squared(cell: usize, x: usize, y: usize, grid_width: usize) -> usize {
    let cell_x = cell % grid_width;
    let cell_y = cell / grid_width;
    cell_x.abs_diff(x).pow(2) + cell_y.abs_diff(y).pow(2)
}

fn sample_front(segment: &FrontSegment) -> Vec<(FrontPoint, usize)> {
    let mut samples = Vec::new();
    let stride = (segment.points.len() / 24).max(1);
    let mut index = 0;
    while index < segment.points.len() {
        samples.push((segment.points[index], index));
        index += stride;
    }
    if !segment.points.is_empty()
        && samples.last().map(|sample| sample.1) != Some(segment.points.len() - 1)
    {
        let index = segment.points.len() - 1;
        samples.push((segment.points[index], index));
    }
    samples
}

fn geo_distance_squared(left_lat: f64, left_lng: f64, right_lat: f64, right_lng: f64) -> f64 {
    let delta_lat = left_lat - right_lat;
    let mut delta_lng = left_lng - right_lng;
    if delta_lng > 180.0 {
        delta_lng -= 360.0;
    } else if delta_lng < -180.0 {
        delta_lng += 360.0;
    }
    delta_lat * delta_lat + delta_lng * delta_lng
}

fn side_key(side: i16) -> usize {
    // Callers guard negative sides before conversion; keeping this helper tiny
    // makes the directed-hostility check read like its worker counterpart.
    side as usize
}

fn is_hostile(hostility: HostilityMatrix<'_>, left: usize, right: usize) -> bool {
    if left == right || left >= hostility.max_sides || right >= hostility.max_sides {
        return false;
    }
    hostility
        .relations
        .is_none_or(|relations| relations[left * hostility.max_sides + right] == 1)
}

fn unique_stable_id(namespace: &str, logical_key: &str, used: &mut BTreeSet<u64>) -> u64 {
    let mut collision_index = 0_usize;
    loop {
        let value = if collision_index == 0 {
            format!("{namespace}|{logical_key}")
        } else {
            format!("{namespace}|{logical_key}|collision:{collision_index}")
        };
        let id = stable_nonzero_hash(&value);
        if used.insert(id) {
            return id;
        }
        collision_index += 1;
    }
}

fn stable_nonzero_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(id: u64, side_index: usize, lat: f64, lng: f64) -> FrontLayoutUnit {
        FrontLayoutUnit {
            id,
            side_index: side_index.try_into().unwrap(),
            lat,
            lng,
            garrison_excluded: false,
            deploy_ticks: 0,
            previous_pair_key: None,
            previous_segment_idx: None,
        }
    }

    fn layout(
        width: usize,
        height: usize,
        land: &[u8],
        sides: &[i16],
        units: &[FrontLayoutUnit],
    ) -> FrontLayout {
        derive_front_layout(
            FrontLayoutInput {
                grid_width: width,
                grid_height: height,
                grid_res: 1.0,
                land_mask: land,
                dominant_side_map: sides,
                hostility: HostilityMatrix::new(None, 2),
                units,
            },
            &FrontLayoutConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn straight_border_builds_stable_segment_and_directed_slots() {
        let result = layout(
            6,
            2,
            &[2; 12],
            &[0, 0, 0, 1, 1, 1, 0, 0, 0, 1, 1, 1],
            &[unit(10, 0, -89.0, -179.0), unit(20, 1, -89.0, -176.0)],
        );
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].stable_key, "0_1_-9_-18");
        assert_eq!(result.assignments.len(), 2);
        assert!(
            result
                .assignments
                .iter()
                .all(|value| value.pair_key.is_some())
        );
        assert_eq!(result.objectives.len(), 2);
        assert_eq!(result.objectives[0].side_pair, [0, 1]);
        assert_eq!(result.objectives[1].side_pair, [1, 0]);
        assert_eq!(result.next_prior.len(), 2);
    }

    #[test]
    fn asymmetric_hostility_does_not_consume_slots_on_a_reverse_only_front() {
        let land = [2; 7];
        let sides = [1, 0, 0, -1, 0, 0, 2];
        let relations = [0, 1, 0, 0, 0, 0, 1, 0, 0];
        let units = [unit(1, 0, -90.0, -175.0), unit(2, 0, -90.0, -174.0)];
        let result = derive_front_layout(
            FrontLayoutInput {
                grid_width: 7,
                grid_height: 1,
                grid_res: 1.0,
                land_mask: &land,
                dominant_side_map: &sides,
                hostility: HostilityMatrix::new(Some(&relations), 3),
                units: &units,
            },
            &FrontLayoutConfig::default(),
        )
        .unwrap();

        assert_eq!(result.segments.len(), 2);
        assert_eq!(result.objectives.len(), 2);
        assert!(
            result
                .objectives
                .iter()
                .all(|objective| objective.side_pair == [0, 1])
        );
        assert!(result.assignments.iter().all(|assignment| {
            assignment.objective_id.is_some()
                && assignment.pair_key.as_deref() == Some(result.segments[0].stable_key.as_str())
        }));
    }

    #[test]
    fn branched_frontline_walk_contains_explicit_backtracking() {
        let result = layout(3, 3, &[2; 9], &[0, 1, 1, 0, 0, 1, 0, 1, 1], &[]);
        let segment = &result.segments[0];
        let unique = segment
            .points
            .iter()
            .map(|point| (point.lat.to_bits(), point.lng.to_bits()))
            .collect::<BTreeSet<_>>();
        assert!(segment.points.len() > unique.len());
        assert_eq!(unique.len(), result.counters.frontier_cells);
    }

    #[test]
    fn prior_pair_wins_within_worker_stickiness_threshold() {
        let sides = [0, 1, -1, -1, 0, 2];
        let land = [2; 6];
        // Equidistant enough that the worker's `nearest * 1.8 + 4` rule keeps
        // the prior front even though the other segment is marginally nearer.
        let mut prior = unit(1, 0, -90.0, -178.0);
        let first = derive_front_layout(
            FrontLayoutInput {
                grid_width: 6,
                grid_height: 1,
                grid_res: 1.0,
                land_mask: &land,
                dominant_side_map: &sides,
                hostility: HostilityMatrix::new(None, 3),
                units: &[prior.clone(), unit(2, 0, -90.0, -180.0)],
            },
            &FrontLayoutConfig::default(),
        )
        .unwrap();
        assert_eq!(first.segments.len(), 2);
        prior.previous_pair_key = Some(first.segments[1].stable_key.clone());
        let second = derive_front_layout(
            FrontLayoutInput {
                grid_width: 6,
                grid_height: 1,
                grid_res: 1.0,
                land_mask: &land,
                dominant_side_map: &sides,
                hostility: HostilityMatrix::new(None, 3),
                units: &[prior, unit(2, 0, -90.0, -180.0)],
            },
            &FrontLayoutConfig::default(),
        )
        .unwrap();
        assert_eq!(
            second.assignments[0].pair_key,
            Some(second.segments[1].stable_key.clone())
        );
        assert_eq!(second.counters.sticky_assignments, 1);
    }

    #[test]
    fn excluded_and_deploying_units_keep_null_worker_assignments() {
        let mut excluded = unit(1, 0, -90.0, -180.0);
        excluded.garrison_excluded = true;
        let mut deploying = unit(2, 1, -90.0, -177.0);
        deploying.deploy_ticks = 1;
        let result = layout(4, 1, &[2; 4], &[0, 0, 1, 1], &[excluded, deploying]);
        assert_eq!(result.counters.eligible_units, 0);
        assert_eq!(result.counters.assigned_units, 0);
        assert!(
            result
                .assignments
                .iter()
                .all(|value| value.pair_key.is_none())
        );
    }

    #[test]
    fn rejects_duplicate_units_and_assignment_bound_overflow() {
        let duplicate = [unit(7, 0, -90.0, -180.0), unit(7, 1, -90.0, -177.0)];
        let input = FrontLayoutInput {
            grid_width: 4,
            grid_height: 1,
            grid_res: 1.0,
            land_mask: &[2; 4],
            dominant_side_map: &[0, 0, 1, 1],
            hostility: HostilityMatrix::new(None, 2),
            units: &duplicate,
        };
        assert_eq!(
            derive_front_layout(input, &FrontLayoutConfig::default()),
            Err(FrontLayoutError::DuplicateUnit(7))
        );

        let units = [unit(1, 0, -90.0, -180.0)];
        let bounded = FrontLayoutInput {
            units: &units,
            ..input
        };
        let config = FrontLayoutConfig {
            max_assignment_edges: 0,
            ..FrontLayoutConfig::default()
        };
        assert_eq!(
            derive_front_layout(bounded, &config),
            Err(FrontLayoutError::InvalidConfig)
        );
    }
}
