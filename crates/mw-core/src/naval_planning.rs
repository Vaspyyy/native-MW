//! Deterministic native origination for naval execution plans.
//!
//! This module owns bounded coastal analysis, sea-route generation, proposal
//! cadence, and initial unit membership. It does not advance operations or
//! mutate units; [`crate::operational_execution`] remains the sole owner of
//! phase continuation, transport flags, and movement steering.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    operational_execution::{
        ExecutionUnitInput, NavalMember, NavalOperation, NavalOperationKind, NavalOperationPhase,
        OperationalExecutionState, Point,
    },
    operations::{OperationalRuntimeState, TaskForcePhase},
    world::WorldGridView,
};

pub const NAVAL_PLANNING_SCHEMA_VERSION: &str = "native-naval-planning-v1";

const INITIAL_REASSESS_TICK: u64 = 150;
const REASSESS_INTERVAL: u64 = 300;
const REASSESS_STAGGER_SIDES: usize = 75;
const MAX_COASTAL_SCAN_ITEMS: usize = 24_000;
const COAST_SAMPLE_LIMIT: usize = 96;
const MAX_INVASION_PATH_CHECKS: usize = 12;
const MAX_SEA_VISITED: usize = 120_000;
const NEAREST_WATER_RADIUS: isize = 6;
const INVASION_MIN_DISTANCE_SQUARED: f64 = 4.0;
const INVASION_MAX_DISTANCE_SQUARED: f64 = 400.0;
const INVASION_STAGING_FORCE_DISTANCE_SQUARED: f64 = 9.0;
const SUPPLY_ASSIGN_DISTANCE_SQUARED: f64 = 64.0;
const STRANDED_DISTANCE_SQUARED: f64 = 9.0;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavalPlanningSideState {
    pub side: usize,
    pub next_reassess_tick: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavalPlanningState {
    pub schema: String,
    pub side_states: Vec<NavalPlanningSideState>,
    pub next_operation_sequence: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavalPlanningCounters {
    pub reassessed_sides: u64,
    pub coastal_candidates: u64,
    pub eligible_units: u64,
    pub coastal_force_units: u64,
    pub supported_staging_cells: u64,
    pub invasion_targets_considered: u64,
    pub distance_eligible_targets: u64,
    pub nearby_front_rejections: u64,
    pub assignment_eligible_targets: u64,
    pub route_checks: u64,
    pub route_visited_cells: u64,
    pub invasions_created: u64,
    pub supply_operations_created: u64,
    pub fast_transports_created: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavalPlanningOutcome {
    pub created: Vec<NavalOperation>,
    pub counters: NavalPlanningCounters,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavalTopology {
    grid_res: f64,
    width: usize,
    height: usize,
    coastal_land_cells: Vec<usize>,
}

#[derive(Clone, Copy)]
pub struct NavalPlanningInput<'a> {
    /// Logical simulation tick used for reassessment cadence.
    pub tick: u64,
    /// Browser-frame time used by operational execution lifecycle timers.
    pub execution_tick: u64,
    pub units: &'a [ExecutionUnitInput],
    pub operations: Option<&'a OperationalRuntimeState>,
    pub execution: &'a OperationalExecutionState,
    pub topology: &'a NavalTopology,
    pub world: WorldGridView<'a>,
    pub dominant_side_map: &'a [i16],
    pub hostility: &'a [u8],
    pub side_count: usize,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum NavalPlanningError {
    #[error("naval planning state is invalid: {0}")]
    InvalidState(&'static str),
    #[error("naval planning input is invalid: {0}")]
    InvalidInput(&'static str),
    #[error("naval planning sequence overflowed")]
    SequenceOverflow,
    #[error("naval planning clock overflowed")]
    ClockOverflow,
}

#[derive(Clone, Copy, Debug)]
struct CoastCell {
    index: usize,
    point: Point,
    side: usize,
}

#[derive(Clone, Debug)]
struct SeaPath {
    cells: Option<Vec<usize>>,
    visited: usize,
}

/// Reusable route-search storage. It is derived scratch state rather than part
/// of the checkpoint contract, so exact continuation does not serialize it.
#[derive(Debug, Default)]
pub struct NavalRouteWorkspace {
    seen_generation: Vec<u32>,
    parent: Vec<u32>,
    queue: Vec<usize>,
    generation: u32,
}

impl NavalRouteWorkspace {
    fn prepare(&mut self, cell_count: usize, max_visited: usize) {
        if self.seen_generation.len() != cell_count {
            self.seen_generation.resize(cell_count, 0);
            self.parent.resize(cell_count, u32::MAX);
            self.generation = 0;
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.seen_generation.fill(0);
            self.generation = 1;
        }
        self.queue.clear();
        let desired = max_visited.min(cell_count);
        if self.queue.capacity() < desired {
            self.queue.reserve(desired - self.queue.capacity());
        }
    }
}

impl NavalTopology {
    pub fn derive(world: WorldGridView<'_>) -> Result<Self, NavalPlanningError> {
        world
            .validate()
            .map_err(|_| NavalPlanningError::InvalidInput("world"))?;
        let mut coastal_land_cells = Vec::new();
        for index in 0..world.land_mask.len() {
            if world.land_mask[index] == 0 {
                continue;
            }
            let row = index / world.width;
            let column = index % world.width;
            let mut coastal = false;
            for row_offset in -1_isize..=1 {
                for column_offset in -1_isize..=1 {
                    if row_offset == 0 && column_offset == 0 {
                        continue;
                    }
                    let neighbor_row = row as isize + row_offset;
                    let neighbor_column = column as isize + column_offset;
                    if neighbor_row < 0
                        || neighbor_row >= world.height as isize
                        || neighbor_column < 0
                        || neighbor_column >= world.width as isize
                    {
                        continue;
                    }
                    let neighbor = neighbor_row as usize * world.width + neighbor_column as usize;
                    if world.land_mask[neighbor] == 0 {
                        coastal = true;
                        break;
                    }
                }
                if coastal {
                    break;
                }
            }
            if coastal {
                coastal_land_cells.push(index);
            }
        }
        Ok(Self {
            grid_res: world.grid_res,
            width: world.width,
            height: world.height,
            coastal_land_cells,
        })
    }

    fn matches(&self, world: WorldGridView<'_>) -> bool {
        self.grid_res == world.grid_res && self.width == world.width && self.height == world.height
    }
}

impl NavalPlanningState {
    pub fn bootstrap(side_count: usize) -> Result<Self, NavalPlanningError> {
        if side_count == 0 {
            return Err(NavalPlanningError::InvalidState("side count"));
        }
        let side_states = (0..side_count)
            .map(|side| NavalPlanningSideState {
                side,
                next_reassess_tick: INITIAL_REASSESS_TICK
                    + (side % REASSESS_STAGGER_SIDES) as u64 * 2,
            })
            .collect();
        Ok(Self {
            schema: NAVAL_PLANNING_SCHEMA_VERSION.to_owned(),
            side_states,
            next_operation_sequence: 1,
        })
    }

    pub fn validate(&self, side_count: usize) -> Result<(), NavalPlanningError> {
        if self.schema != NAVAL_PLANNING_SCHEMA_VERSION {
            return Err(NavalPlanningError::InvalidState("schema"));
        }
        if side_count == 0
            || self.side_states.len() != side_count
            || !self
                .side_states
                .iter()
                .enumerate()
                .all(|(side, state)| state.side == side)
        {
            return Err(NavalPlanningError::InvalidState("side states"));
        }
        if self.next_operation_sequence == 0 {
            return Err(NavalPlanningError::InvalidState("next operation sequence"));
        }
        Ok(())
    }

    pub fn validate_with_execution(
        &self,
        side_count: usize,
        execution: &OperationalExecutionState,
    ) -> Result<(), NavalPlanningError> {
        self.validate(side_count)?;
        for operation in &execution.naval_operations {
            let Some((side, sequence)) = native_operation_identity(&operation.id)? else {
                continue;
            };
            if side != operation.side || sequence >= self.next_operation_sequence {
                return Err(NavalPlanningError::InvalidState(
                    "native operation sequence",
                ));
            }
        }
        Ok(())
    }

    /// Reassess at most one staggered side and return new operations without
    /// mutating execution or simulation state. The state update is atomic.
    pub fn advance(
        &mut self,
        input: NavalPlanningInput<'_>,
        route_workspace: &mut NavalRouteWorkspace,
    ) -> Result<NavalPlanningOutcome, NavalPlanningError> {
        validate_input(&input)?;
        self.validate_with_execution(input.side_count, input.execution)?;
        let mut next = self.clone();
        let outcome = next.advance_in_place(input, route_workspace)?;
        next.validate(input.side_count)?;
        *self = next;
        Ok(outcome)
    }

    fn advance_in_place(
        &mut self,
        input: NavalPlanningInput<'_>,
        route_workspace: &mut NavalRouteWorkspace,
    ) -> Result<NavalPlanningOutcome, NavalPlanningError> {
        let Some(side_state_index) = self
            .side_states
            .iter()
            .position(|state| input.tick >= state.next_reassess_tick)
        else {
            return Ok(NavalPlanningOutcome::default());
        };
        let side = self.side_states[side_state_index].side;
        self.side_states[side_state_index].next_reassess_tick = input
            .tick
            .checked_add(REASSESS_INTERVAL)
            .ok_or(NavalPlanningError::ClockOverflow)?;

        let mut outcome = NavalPlanningOutcome::default();
        outcome.counters.reassessed_sides = 1;
        let (friendly_coasts, enemy_coasts) = sample_coasts(&input, side);
        outcome.counters.coastal_candidates = (friendly_coasts.len() + enemy_coasts.len()) as u64;
        let mut claimed = claimed_units(input.execution, input.operations);

        if !has_operation(input.execution, side, NavalOperationKind::Invasion)
            && let Some(operation) = self.propose_invasion(
                &input,
                side,
                (&friendly_coasts, &enemy_coasts),
                &claimed,
                &mut outcome.counters,
                route_workspace,
            )?
        {
            claimed.extend(operation.members.iter().map(|member| member.unit_id));
            outcome.created.push(operation);
            outcome.counters.invasions_created = 1;
        }

        if !has_operation(input.execution, side, NavalOperationKind::Supply)
            && let Some(operation) = self.propose_supply(
                &input,
                side,
                &friendly_coasts,
                &claimed,
                &mut outcome.counters,
                route_workspace,
            )?
        {
            claimed.extend(operation.members.iter().map(|member| member.unit_id));
            outcome.created.push(operation);
            outcome.counters.supply_operations_created = 1;
        }

        if !has_operation(input.execution, side, NavalOperationKind::FastTransport)
            && let Some(operation) = self.propose_fast_transport(&input, side, &claimed)?
        {
            outcome.created.push(operation);
            outcome.counters.fast_transports_created = 1;
        }

        outcome
            .created
            .sort_by(|left, right| left.id.cmp(&right.id));
        Ok(outcome)
    }

    fn propose_invasion(
        &mut self,
        input: &NavalPlanningInput<'_>,
        side: usize,
        coasts: (&[CoastCell], &[CoastCell]),
        claimed: &BTreeSet<u64>,
        counters: &mut NavalPlanningCounters,
        route_workspace: &mut NavalRouteWorkspace,
    ) -> Result<Option<NavalOperation>, NavalPlanningError> {
        let (friendly_coasts, enemy_coasts) = coasts;
        if friendly_coasts.is_empty() || enemy_coasts.is_empty() {
            return Ok(None);
        }
        let eligible = eligible_units(input.units, side, claimed);
        counters.eligible_units = eligible.len() as u64;
        let coastal_force_units = eligible
            .iter()
            .filter(|unit| {
                friendly_coasts
                    .iter()
                    .any(|coast| unit.position.distance_squared(coast.point) < 9.0)
            })
            .count();
        counters.coastal_force_units = coastal_force_units as u64;
        if eligible.len() < 5 || coastal_force_units < 5 {
            return Ok(None);
        }

        let supported_staging = friendly_coasts
            .iter()
            .filter(|coast| {
                eligible
                    .iter()
                    .filter(|unit| {
                        unit.position.distance_squared(coast.point)
                            < INVASION_STAGING_FORCE_DISTANCE_SQUARED
                    })
                    .take(5)
                    .count()
                    == 5
            })
            .copied()
            .collect::<Vec<_>>();
        counters.supported_staging_cells = supported_staging.len() as u64;

        let mut checked = 0;
        for target in enemy_coasts {
            counters.invasion_targets_considered += 1;
            let Some(staging) = supported_staging.iter().min_by(|left, right| {
                left.point
                    .distance_squared(target.point)
                    .total_cmp(&right.point.distance_squared(target.point))
                    .then_with(|| left.index.cmp(&right.index))
            }) else {
                continue;
            };
            let direct_distance = staging.point.distance_squared(target.point);
            if !(INVASION_MIN_DISTANCE_SQUARED..=INVASION_MAX_DISTANCE_SQUARED)
                .contains(&direct_distance)
            {
                continue;
            }
            counters.distance_eligible_targets += 1;
            if nearby_friendly_land(input, target.index, side, 2) {
                counters.nearby_front_rejections += 1;
                continue;
            }
            let mut recruits = eligible
                .iter()
                .filter_map(|unit| {
                    let distance = unit.position.distance_squared(staging.point);
                    (distance < INVASION_STAGING_FORCE_DISTANCE_SQUARED).then_some((
                        distance,
                        unit.unit_id,
                        *unit,
                    ))
                })
                .collect::<Vec<_>>();
            recruits.sort_by(|left, right| {
                left.0
                    .total_cmp(&right.0)
                    .then_with(|| left.1.cmp(&right.1))
            });
            if recruits.len() < 5 {
                continue;
            }
            counters.assignment_eligible_targets += 1;
            if checked >= MAX_INVASION_PATH_CHECKS {
                break;
            }
            checked += 1;
            counters.route_checks += 1;
            let Some(start_water) = nearest_water(input.world, staging.index, NEAREST_WATER_RADIUS)
            else {
                continue;
            };
            let Some(target_water) = nearest_water(input.world, target.index, NEAREST_WATER_RADIUS)
            else {
                continue;
            };
            let path = sea_path(
                input.world,
                start_water,
                target_water,
                MAX_SEA_VISITED,
                route_workspace,
            )?;
            counters.route_visited_cells = counters
                .route_visited_cells
                .saturating_add(path.visited as u64);
            let Some(path_cells) = path.cells else {
                continue;
            };
            let max_assigned = ceil_percent(eligible.len(), 15).max(5).min(recruits.len());
            let selected = recruits
                .into_iter()
                .take(max_assigned)
                .map(|(_, _, unit)| unit)
                .collect::<Vec<_>>();
            let sequence = self.take_sequence()?;
            return Ok(Some(build_operation(
                sequence,
                input.execution_tick,
                NavalOperationKind::Invasion,
                side,
                Some(target.side),
                staging.point,
                target.point,
                path_waypoints(input.world, &path_cells),
                selected,
                max_assigned,
            )));
        }
        Ok(None)
    }

    fn propose_supply(
        &mut self,
        input: &NavalPlanningInput<'_>,
        side: usize,
        friendly_coasts: &[CoastCell],
        claimed: &BTreeSet<u64>,
        counters: &mut NavalPlanningCounters,
        route_workspace: &mut NavalRouteWorkspace,
    ) -> Result<Option<NavalOperation>, NavalPlanningError> {
        let Some(invasion) = input.execution.naval_operations.iter().find(|operation| {
            operation.side == side
                && operation.kind == NavalOperationKind::Invasion
                && operation.phase == NavalOperationPhase::Landing
        }) else {
            return Ok(None);
        };
        let Some(staging) = friendly_coasts.iter().min_by(|left, right| {
            left.point
                .distance_squared(invasion.target)
                .total_cmp(&right.point.distance_squared(invasion.target))
                .then_with(|| left.index.cmp(&right.index))
        }) else {
            return Ok(None);
        };
        if staging.point.distance_squared(invasion.target) > INVASION_MAX_DISTANCE_SQUARED {
            return Ok(None);
        }
        let mut recruits = eligible_units(input.units, side, claimed)
            .into_iter()
            .filter_map(|unit| {
                let distance = unit.position.distance_squared(staging.point);
                (distance < SUPPLY_ASSIGN_DISTANCE_SQUARED).then_some((
                    distance,
                    unit.unit_id,
                    unit,
                ))
            })
            .collect::<Vec<_>>();
        recruits.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        if recruits.len() < 3 {
            return Ok(None);
        }
        counters.route_checks += 1;
        let Some(start_water) = nearest_water(input.world, staging.index, NEAREST_WATER_RADIUS)
        else {
            return Ok(None);
        };
        let Some(target_index) = input
            .world
            .grid_index(invasion.target.lat, invasion.target.lng)
        else {
            return Ok(None);
        };
        let Some(target_water) = nearest_water(input.world, target_index, NEAREST_WATER_RADIUS)
        else {
            return Ok(None);
        };
        let path = sea_path(
            input.world,
            start_water,
            target_water,
            MAX_SEA_VISITED,
            route_workspace,
        )?;
        counters.route_visited_cells = counters
            .route_visited_cells
            .saturating_add(path.visited as u64);
        let Some(path_cells) = path.cells else {
            return Ok(None);
        };
        let max_assigned = ceil_percent(
            input.units.iter().filter(|unit| unit.side == side).count(),
            10,
        )
        .max(3)
        .min(recruits.len());
        let selected = recruits
            .into_iter()
            .take(max_assigned)
            .map(|(_, _, unit)| unit)
            .collect::<Vec<_>>();
        let sequence = self.take_sequence()?;
        Ok(Some(build_operation(
            sequence,
            input.execution_tick,
            NavalOperationKind::Supply,
            side,
            None,
            staging.point,
            invasion.target,
            path_waypoints(input.world, &path_cells),
            selected,
            max_assigned,
        )))
    }

    fn propose_fast_transport(
        &mut self,
        input: &NavalPlanningInput<'_>,
        side: usize,
        claimed: &BTreeSet<u64>,
    ) -> Result<Option<NavalOperation>, NavalPlanningError> {
        let Some(operations) = input.operations else {
            return Ok(None);
        };
        let eligible = eligible_units(input.units, side, claimed);
        if eligible.len() < 6 {
            return Ok(None);
        }
        let mut candidates = operations
            .task_forces
            .iter()
            .filter(|force| {
                force.side_index == side
                    && matches!(
                        force.phase,
                        TaskForcePhase::Assembling
                            | TaskForcePhase::Attacking
                            | TaskForcePhase::Consolidating
                    )
                    && force.target.is_some()
            })
            .filter_map(|force| {
                let target = force.target?;
                let stranded = eligible
                    .iter()
                    .filter(|unit| {
                        unit.position.distance_squared(Point {
                            lat: target.lat,
                            lng: target.lng,
                        }) > STRANDED_DISTANCE_SQUARED
                    })
                    .count();
                (stranded >= 5).then_some((stranded, force.id.as_str(), target))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
        let Some((_, _, target)) = candidates.first().copied() else {
            return Ok(None);
        };
        let target = Point {
            lat: target.lat,
            lng: target.lng,
        };
        let mut recruits = eligible
            .into_iter()
            .filter_map(|unit| {
                let distance = unit.position.distance_squared(target);
                (distance > STRANDED_DISTANCE_SQUARED).then_some((distance, unit.unit_id, unit))
            })
            .collect::<Vec<_>>();
        recruits.sort_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        let selected = recruits
            .into_iter()
            .take(5)
            .map(|(_, _, unit)| unit)
            .collect::<Vec<_>>();
        if selected.len() < 5 {
            return Ok(None);
        }
        let staging = Point {
            lat: selected.iter().map(|unit| unit.position.lat).sum::<f64>() / selected.len() as f64,
            lng: selected.iter().map(|unit| unit.position.lng).sum::<f64>() / selected.len() as f64,
        };
        let sequence = self.take_sequence()?;
        Ok(Some(build_operation(
            sequence,
            input.execution_tick,
            NavalOperationKind::FastTransport,
            side,
            None,
            staging,
            target,
            Vec::new(),
            selected,
            5,
        )))
    }

    fn take_sequence(&mut self) -> Result<u64, NavalPlanningError> {
        let sequence = self.next_operation_sequence;
        self.next_operation_sequence = sequence
            .checked_add(1)
            .ok_or(NavalPlanningError::SequenceOverflow)?;
        Ok(sequence)
    }
}

fn validate_input(input: &NavalPlanningInput<'_>) -> Result<(), NavalPlanningError> {
    input
        .world
        .validate()
        .map_err(|_| NavalPlanningError::InvalidInput("world"))?;
    let cell_count = input
        .world
        .width
        .checked_mul(input.world.height)
        .ok_or(NavalPlanningError::InvalidInput("world size"))?;
    if input.side_count == 0
        || input.dominant_side_map.len() != cell_count
        || input.hostility.len() != input.side_count * input.side_count
        || !input.topology.matches(input.world)
    {
        return Err(NavalPlanningError::InvalidInput("topology"));
    }
    let mut ids = BTreeSet::new();
    for unit in input.units {
        if unit.unit_id == 0
            || unit.side >= input.side_count
            || unit.country == 0
            || !unit.position.validate()
            || !ids.insert(unit.unit_id)
        {
            return Err(NavalPlanningError::InvalidInput("units"));
        }
    }
    Ok(())
}

fn native_operation_identity(id: &str) -> Result<Option<(usize, u64)>, NavalPlanningError> {
    let Some(identity) = id.strip_prefix("native-naval-") else {
        return Ok(None);
    };
    let Some((side, sequence)) = identity.split_once('-') else {
        return Err(NavalPlanningError::InvalidState("native operation id"));
    };
    let side = side
        .parse::<usize>()
        .map_err(|_| NavalPlanningError::InvalidState("native operation id"))?;
    let sequence = sequence
        .parse::<u64>()
        .map_err(|_| NavalPlanningError::InvalidState("native operation id"))?;
    if sequence == 0 {
        return Err(NavalPlanningError::InvalidState("native operation id"));
    }
    Ok(Some((side, sequence)))
}

fn sample_coasts(input: &NavalPlanningInput<'_>, side: usize) -> (Vec<CoastCell>, Vec<CoastCell>) {
    let stride = (input.topology.coastal_land_cells.len() / MAX_COASTAL_SCAN_ITEMS).max(1);
    let mut friendly = Vec::new();
    let mut enemy = Vec::new();
    let mut friendly_buckets = BTreeSet::new();
    let mut enemy_buckets = BTreeSet::new();
    for &index in input.topology.coastal_land_cells.iter().step_by(stride) {
        let dominant = input.dominant_side_map[index];
        let point = cell_point(input.world, index);
        let bucket = (point.lat.floor() as i32, point.lng.floor() as i32);
        if dominant == side as i16 {
            if friendly_buckets.insert(bucket) {
                friendly.push(CoastCell { index, point, side });
            }
            continue;
        }
        let Ok(enemy_side) = usize::try_from(dominant) else {
            continue;
        };
        if enemy_side < input.side_count
            && input.hostility[side * input.side_count + enemy_side] == 1
            && enemy_buckets.insert(bucket)
        {
            enemy.push(CoastCell {
                index,
                point,
                side: enemy_side,
            });
        }
    }
    (
        select_evenly_spaced(friendly, COAST_SAMPLE_LIMIT),
        select_evenly_spaced(enemy, COAST_SAMPLE_LIMIT),
    )
}

fn select_evenly_spaced<T: Copy>(items: Vec<T>, limit: usize) -> Vec<T> {
    if items.len() <= limit {
        return items;
    }
    let step = (items.len() - 1) as f64 / (limit - 1).max(1) as f64;
    (0..limit)
        .map(|index| items[(index as f64 * step + 0.5).floor() as usize])
        .collect()
}

fn claimed_units(
    execution: &OperationalExecutionState,
    operations: Option<&OperationalRuntimeState>,
) -> BTreeSet<u64> {
    let mut claimed = execution
        .naval_operations
        .iter()
        .flat_map(|operation| operation.members.iter().map(|member| member.unit_id))
        .chain(
            execution
                .defender_reactions
                .iter()
                .flat_map(|reaction| reaction.unit_ids.iter().copied()),
        )
        .collect::<BTreeSet<_>>();
    if let Some(operations) = operations {
        claimed.extend(
            operations
                .task_forces
                .iter()
                .flat_map(|force| force.members.iter().map(|member| member.unit_id)),
        );
    }
    claimed
}

fn eligible_units<'a>(
    units: &'a [ExecutionUnitInput],
    side: usize,
    claimed: &BTreeSet<u64>,
) -> Vec<&'a ExecutionUnitInput> {
    units
        .iter()
        .filter(|unit| {
            unit.side == side
                && !unit.deploying
                && !unit.engaged
                && !unit.transport
                && !unit.at_sea
                && !unit.operationally_assigned
                && !claimed.contains(&unit.unit_id)
        })
        .collect()
}

fn has_operation(
    execution: &OperationalExecutionState,
    side: usize,
    kind: NavalOperationKind,
) -> bool {
    execution
        .naval_operations
        .iter()
        .any(|operation| operation.side == side && operation.kind == kind)
}

fn nearby_friendly_land(
    input: &NavalPlanningInput<'_>,
    target: usize,
    side: usize,
    radius: isize,
) -> bool {
    let row = target / input.world.width;
    let column = target % input.world.width;
    for row_offset in -radius..=radius {
        for column_offset in -radius..=radius {
            if row_offset == 0 && column_offset == 0 {
                continue;
            }
            let neighbor_row = row as isize + row_offset;
            let neighbor_column = column as isize + column_offset;
            if neighbor_row < 0
                || neighbor_row >= input.world.height as isize
                || neighbor_column < 0
                || neighbor_column >= input.world.width as isize
            {
                continue;
            }
            let neighbor = neighbor_row as usize * input.world.width + neighbor_column as usize;
            if input.world.land_mask[neighbor] != 0
                && input.dominant_side_map[neighbor] == side as i16
            {
                return true;
            }
        }
    }
    false
}

fn nearest_water(world: WorldGridView<'_>, index: usize, radius: isize) -> Option<usize> {
    if index >= world.land_mask.len() {
        return None;
    }
    let row = index / world.width;
    let column = index % world.width;
    let mut best = None;
    let mut best_distance = isize::MAX;
    for row_offset in -radius..=radius {
        for column_offset in -radius..=radius {
            let candidate_row = row as isize + row_offset;
            let candidate_column = column as isize + column_offset;
            if candidate_row < 0
                || candidate_row >= world.height as isize
                || candidate_column < 0
                || candidate_column >= world.width as isize
            {
                continue;
            }
            let candidate = candidate_row as usize * world.width + candidate_column as usize;
            if world.land_mask[candidate] != 0 {
                continue;
            }
            let distance = row_offset * row_offset + column_offset * column_offset;
            if distance < best_distance {
                best_distance = distance;
                best = Some(candidate);
            }
        }
    }
    best
}

fn sea_path(
    world: WorldGridView<'_>,
    start: usize,
    target: usize,
    max_visited: usize,
    workspace: &mut NavalRouteWorkspace,
) -> Result<SeaPath, NavalPlanningError> {
    if start >= world.land_mask.len()
        || target >= world.land_mask.len()
        || world.land_mask[start] != 0
        || world.land_mask[target] != 0
    {
        return Ok(SeaPath {
            cells: None,
            visited: 0,
        });
    }
    if world.land_mask.len() > u32::MAX as usize {
        return Err(NavalPlanningError::InvalidInput(
            "world exceeds route index width",
        ));
    }
    workspace.prepare(world.land_mask.len(), max_visited);
    let generation = workspace.generation;
    workspace.queue.push(start);
    workspace.seen_generation[start] = generation;
    workspace.parent[start] = start as u32;
    let mut head = 0;
    let mut visited = 0;
    while head < workspace.queue.len() && visited < max_visited {
        let current = workspace.queue[head];
        head += 1;
        visited += 1;
        if current == target {
            let mut cells = vec![target];
            let mut walk = target;
            while walk != start {
                let previous = workspace.parent[walk];
                if previous == u32::MAX {
                    return Err(NavalPlanningError::InvalidInput("route parent"));
                }
                walk = previous as usize;
                cells.push(walk);
            }
            cells.reverse();
            return Ok(SeaPath {
                cells: Some(cells),
                visited,
            });
        }
        let row = current / world.width;
        let column = current % world.width;
        let mut push = |neighbor: usize| {
            if workspace.seen_generation[neighbor] != generation && world.land_mask[neighbor] == 0 {
                workspace.seen_generation[neighbor] = generation;
                workspace.parent[neighbor] = current as u32;
                workspace.queue.push(neighbor);
            }
        };
        if column + 1 < world.width {
            push(current + 1);
        }
        if column > 0 {
            push(current - 1);
        }
        if row + 1 < world.height {
            push(current + world.width);
        }
        if row > 0 {
            push(current - world.width);
        }
    }
    Ok(SeaPath {
        cells: None,
        visited,
    })
}

fn path_waypoints(world: WorldGridView<'_>, cells: &[usize]) -> Vec<Point> {
    if cells.len() <= 1 {
        return Vec::new();
    }
    let stride = (1.0 / world.grid_res).floor().max(1.0) as usize;
    let mut waypoints = cells
        .iter()
        .copied()
        .skip(stride)
        .step_by(stride)
        .map(|cell| cell_point(world, cell))
        .collect::<Vec<_>>();
    let target = cell_point(world, *cells.last().expect("path is nonempty"));
    if waypoints.last().copied() != Some(target) {
        waypoints.push(target);
    }
    waypoints
}

fn cell_point(world: WorldGridView<'_>, index: usize) -> Point {
    let row = index / world.width;
    let column = index % world.width;
    Point {
        lat: -90.0 + (row as f64 + 0.5) * world.grid_res,
        lng: -180.0 + (column as f64 + 0.5) * world.grid_res,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_operation(
    sequence: u64,
    tick: u64,
    kind: NavalOperationKind,
    side: usize,
    enemy_side: Option<usize>,
    staging: Point,
    target: Point,
    route: Vec<Point>,
    selected: Vec<&ExecutionUnitInput>,
    max_assigned_units: usize,
) -> NavalOperation {
    let kind_name = match kind {
        NavalOperationKind::Invasion => "invasion",
        NavalOperationKind::Supply => "supply",
        NavalOperationKind::FastTransport => "transport",
    };
    let mut members = selected
        .iter()
        .map(|unit| NavalMember {
            unit_id: unit.unit_id,
            role: match kind {
                NavalOperationKind::Invasion => "NAVAL_INVASION",
                NavalOperationKind::Supply => "NAVAL_SUPPLY",
                NavalOperationKind::FastTransport => "FAST_TRANSPORT",
            }
            .to_owned(),
            assigned_tick: tick,
        })
        .collect::<Vec<_>>();
    members.sort_by_key(|member| member.unit_id);
    NavalOperation {
        id: format!("native-naval-{side}-{sequence}"),
        signature: format!(
            "native:{side}:{kind_name}:{}:{}",
            (target.lat * 2.0).round() as i32,
            (target.lng * 2.0).round() as i32
        ),
        kind,
        phase: if kind == NavalOperationKind::FastTransport {
            NavalOperationPhase::Transit
        } else {
            NavalOperationPhase::Gathering
        },
        side,
        country: selected[0].country,
        enemy_side,
        max_assigned_units,
        members,
        staging,
        target,
        route,
        route_index: 0,
        progress: 0.0,
        started_tick: tick,
        phase_started_tick: tick,
        last_progress_tick: tick,
        completion_reason: None,
    }
}

fn ceil_percent(value: usize, percent: usize) -> usize {
    value.saturating_mul(percent).saturating_add(99) / 100
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{
        OperationalPoint, OperationalTaskForce, TaskForceMember, TaskForcePosture, TaskForceRole,
    };

    const WIDTH: usize = 20;
    const HEIGHT: usize = 10;

    fn island_maps() -> (Vec<u8>, Vec<i16>) {
        let mut land = vec![0; WIDTH * HEIGHT];
        let mut dominant = vec![-1; WIDTH * HEIGHT];
        for row in 4..=5 {
            for column in 2..=3 {
                let cell = row * WIDTH + column;
                land[cell] = 1;
                dominant[cell] = 0;
            }
            for column in 14..=15 {
                let cell = row * WIDTH + column;
                land[cell] = 1;
                dominant[cell] = 1;
            }
        }
        (land, dominant)
    }

    fn unit(unit_id: u64, side: usize, country: u16, point: Point) -> ExecutionUnitInput {
        ExecutionUnitInput {
            unit_id,
            side,
            country,
            position: point,
            transport: false,
            at_sea: false,
            deploying: false,
            engaged: false,
            operationally_assigned: false,
        }
    }

    fn units_at(count: u64, side: usize, country: u16, point: Point) -> Vec<ExecutionUnitInput> {
        (1..=count)
            .map(|unit_id| unit(unit_id, side, country, point))
            .collect()
    }

    fn invasion(tick: u64, staging: Point, target: Point, members: &[u64]) -> NavalOperation {
        NavalOperation {
            id: "existing-invasion".to_owned(),
            signature: "existing-invasion".to_owned(),
            kind: NavalOperationKind::Invasion,
            phase: NavalOperationPhase::Landing,
            side: 0,
            country: 10,
            enemy_side: Some(1),
            max_assigned_units: members.len(),
            members: members
                .iter()
                .copied()
                .map(|unit_id| NavalMember {
                    unit_id,
                    role: "NAVAL_INVASION".to_owned(),
                    assigned_tick: tick,
                })
                .collect(),
            staging,
            target,
            route: Vec::new(),
            route_index: 0,
            progress: 0.8,
            started_tick: tick,
            phase_started_tick: tick,
            last_progress_tick: tick,
            completion_reason: None,
        }
    }

    #[test]
    fn wire_is_strict_and_side_cadence_is_staggered() {
        let state = NavalPlanningState::bootstrap(3).unwrap();
        assert_eq!(state.schema, NAVAL_PLANNING_SCHEMA_VERSION);
        assert_eq!(state.next_operation_sequence, 1);
        assert_eq!(
            state
                .side_states
                .iter()
                .map(|side| side.next_reassess_tick)
                .collect::<Vec<_>>(),
            vec![150, 152, 154]
        );
        let value = serde_json::to_value(&state).unwrap();
        let mut unknown = value.clone();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("unknown".to_owned(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<NavalPlanningState>(unknown).is_err());
        assert_eq!(
            serde_json::from_value::<NavalPlanningState>(value).unwrap(),
            state
        );

        let mut execution = OperationalExecutionState::new();
        let mut operation = invasion(0, Point::default(), Point::default(), &[1]);
        operation.id = "native-naval-0-1".to_owned();
        execution.naval_operations.push(operation);
        assert!(state.validate_with_execution(3, &execution).is_err());
        let mut advanced = state.clone();
        advanced.next_operation_sequence = 2;
        assert!(advanced.validate_with_execution(3, &execution).is_ok());

        execution.naval_operations[0].id = "native-naval-invalid".to_owned();
        assert!(advanced.validate_with_execution(3, &execution).is_err());
    }

    #[test]
    fn sea_bfs_is_water_only_deterministic_and_does_not_wrap_columns() {
        let land = vec![0; 5];
        let world = WorldGridView::new(1.0, 5, 1, &land).unwrap();
        let mut workspace = NavalRouteWorkspace::default();
        let path = sea_path(world, 0, 4, 100, &mut workspace).unwrap();
        assert_eq!(path.cells.unwrap(), vec![0, 1, 2, 3, 4]);
        assert_eq!(path.visited, 5);

        let blocked = vec![0, 0, 1, 0, 0];
        let world = WorldGridView::new(1.0, 5, 1, &blocked).unwrap();
        let path = sea_path(world, 0, 4, 100, &mut workspace).unwrap();
        assert!(path.cells.is_none());
        assert_eq!(path.visited, 2);
    }

    #[test]
    fn native_reassessment_originates_one_routed_invasion_idempotently() {
        let (land, dominant) = island_maps();
        let world = WorldGridView::new(1.0, WIDTH, HEIGHT, &land).unwrap();
        let topology = NavalTopology::derive(world).unwrap();
        let staging = cell_point(world, 4 * WIDTH + 3);
        let units = units_at(8, 0, 10, staging);
        let hostility = [0, 1, 1, 0];
        let mut planner = NavalPlanningState::bootstrap(2).unwrap();
        let mut execution = OperationalExecutionState::new();
        let mut workspace = NavalRouteWorkspace::default();

        let first = planner
            .advance(
                NavalPlanningInput {
                    tick: 150,
                    execution_tick: 42,
                    units: &units,
                    operations: None,
                    execution: &execution,
                    topology: &topology,
                    world,
                    dominant_side_map: &dominant,
                    hostility: &hostility,
                    side_count: 2,
                },
                &mut workspace,
            )
            .unwrap();
        assert_eq!(first.counters.invasions_created, 1);
        assert_eq!(first.created.len(), 1);
        let operation = &first.created[0];
        assert_eq!(operation.kind, NavalOperationKind::Invasion);
        assert_eq!(operation.phase, NavalOperationPhase::Gathering);
        assert_eq!(operation.members.len(), 5);
        assert_eq!(operation.started_tick, 42);
        assert_eq!(operation.phase_started_tick, 42);
        assert_eq!(operation.last_progress_tick, 42);
        assert!(
            operation
                .members
                .iter()
                .all(|member| member.assigned_tick == 42)
        );
        assert!(!operation.route.is_empty());
        assert!(
            operation
                .route
                .iter()
                .all(|point| world.is_water(point.lat, point.lng))
        );
        execution.naval_operations.extend(first.created);

        let repeated = planner
            .advance(
                NavalPlanningInput {
                    tick: 450,
                    execution_tick: 450,
                    units: &units,
                    operations: None,
                    execution: &execution,
                    topology: &topology,
                    world,
                    dominant_side_map: &dominant,
                    hostility: &hostility,
                    side_count: 2,
                },
                &mut workspace,
            )
            .unwrap();
        assert!(repeated.created.is_empty());
        assert_eq!(planner.next_operation_sequence, 2);
    }

    #[test]
    fn landing_originates_supply_with_disjoint_members_and_resumes_exactly() {
        let (land, dominant) = island_maps();
        let world = WorldGridView::new(1.0, WIDTH, HEIGHT, &land).unwrap();
        let topology = NavalTopology::derive(world).unwrap();
        let staging = cell_point(world, 4 * WIDTH + 3);
        let target = cell_point(world, 4 * WIDTH + 14);
        let units = units_at(8, 0, 10, staging);
        let execution = OperationalExecutionState {
            naval_operations: vec![invasion(0, staging, target, &[1, 2, 3, 4, 5])],
            ..OperationalExecutionState::new()
        };
        let hostility = [0, 1, 1, 0];
        let mut first_planner = NavalPlanningState::bootstrap(2).unwrap();
        let mut resumed_planner = first_planner.clone();
        let mut first_workspace = NavalRouteWorkspace::default();
        let mut resumed_workspace = NavalRouteWorkspace::default();
        let input = NavalPlanningInput {
            tick: 150,
            execution_tick: 150,
            units: &units,
            operations: None,
            execution: &execution,
            topology: &topology,
            world,
            dominant_side_map: &dominant,
            hostility: &hostility,
            side_count: 2,
        };
        let first = first_planner.advance(input, &mut first_workspace).unwrap();
        let resumed = resumed_planner
            .advance(input, &mut resumed_workspace)
            .unwrap();
        assert_eq!(first, resumed);
        assert_eq!(first_planner, resumed_planner);
        assert_eq!(first.counters.supply_operations_created, 1);
        let supply = first
            .created
            .iter()
            .find(|operation| operation.kind == NavalOperationKind::Supply)
            .unwrap();
        assert_eq!(
            supply
                .members
                .iter()
                .map(|member| member.unit_id)
                .collect::<Vec<_>>(),
            vec![6, 7, 8]
        );
    }

    #[test]
    fn active_land_task_force_originates_fast_transport_for_stragglers() {
        let land = vec![1; WIDTH * HEIGHT];
        let dominant = vec![0; WIDTH * HEIGHT];
        let world = WorldGridView::new(1.0, WIDTH, HEIGHT, &land).unwrap();
        let topology = NavalTopology::derive(world).unwrap();
        let origin = cell_point(world, 4 * WIDTH + 2);
        let target = cell_point(world, 4 * WIDTH + 16);
        let units = units_at(10, 0, 10, origin);
        let hostility = [0, 1, 1, 0];
        let mut operations = OperationalRuntimeState::bootstrap(2, &hostility, &[10.0, 10.0]);
        operations.task_forces.push(OperationalTaskForce {
            id: "land-force".to_owned(),
            signature: "land-force".to_owned(),
            side_index: 0,
            plan_signature: "land-force".to_owned(),
            plan_type: "PUSH_FRONT".to_owned(),
            theater_id: None,
            target: Some(OperationalPoint {
                lat: target.lat,
                lng: target.lng,
            }),
            staging_anchor: Some(OperationalPoint {
                lat: origin.lat,
                lng: origin.lng,
            }),
            route: Vec::new(),
            phase: TaskForcePhase::Attacking,
            posture: TaskForcePosture::Balanced,
            members: (1..=3)
                .map(|unit_id| TaskForceMember {
                    unit_id,
                    role: TaskForceRole::Line,
                    assigned_tick: 0,
                    route_progress: 0.0,
                })
                .collect(),
            reserve_unit_ids: Vec::new(),
            desired_power: 3.0,
            launch_power: 3.0,
            current_power: 3.0,
            peak_power: 3.0,
            readiness: 1.0,
            max_assigned_units: 3,
            created_tick: 0,
            phase_started_tick: 0,
            last_progress_tick: 0,
            last_recovery_tick: 0,
            recovery_power: 0.0,
            progress: 0.0,
            withdrawal_anchor: Some(OperationalPoint {
                lat: origin.lat,
                lng: origin.lng,
            }),
            completion_reason: None,
            outcome: None,
            severe_surprise: false,
            parent_task_force_id: None,
            supply_invalidated_tick: None,
            intent_revision: 1,
        });
        let execution = OperationalExecutionState::new();
        let mut planner = NavalPlanningState::bootstrap(2).unwrap();
        let mut workspace = NavalRouteWorkspace::default();
        let outcome = planner
            .advance(
                NavalPlanningInput {
                    tick: 150,
                    execution_tick: 150,
                    units: &units,
                    operations: Some(&operations),
                    execution: &execution,
                    topology: &topology,
                    world,
                    dominant_side_map: &dominant,
                    hostility: &hostility,
                    side_count: 2,
                },
                &mut workspace,
            )
            .unwrap();
        assert_eq!(outcome.counters.fast_transports_created, 1);
        let transport = &outcome.created[0];
        assert_eq!(transport.kind, NavalOperationKind::FastTransport);
        assert_eq!(
            transport
                .members
                .iter()
                .map(|member| member.unit_id)
                .collect::<Vec<_>>(),
            vec![4, 5, 6, 7, 8]
        );
    }
}
