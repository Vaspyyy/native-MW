//! Stateless, deterministic native AI order resolution.
//!
//! The planner consumes immutable snapshots, validates the complete input before
//! producing output, and returns explicit assignment records for optional use by
//! the next planning tick. It intentionally owns no clock, random source, or
//! hidden assignment cache.

use std::{cmp::Ordering, collections::BTreeMap};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    combat::{UnitKind, wrapped_longitude_delta},
    direction::HostilityMatrix,
    movement::MovementFactors,
    simulation::{ResolvedCombatOrder, ResolvedUnitOrder},
    tactical::{NeighborOptions, SideKey, TacticalGrid, TacticalGridError, TacticalUnit},
    world::WorldGridView,
};

pub const AI_ORDER_SCHEMA_VERSION: &str = "ai-orders-v1";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AiOrderConfig {
    pub contact_scan_radius: f64,
    pub retreat_min_hostile_power: f64,
    pub retreat_multiple: f64,
    pub retreat_boost: f64,
    pub encircled_retreat_multiplier: f64,
    pub prior_assignment_stickiness: f64,
    pub reinforcement_readiness_threshold: f64,
    pub contact_plan_speed_multiplier: f64,
    pub front_plan_speed_multiplier: f64,
    pub reinforcement_plan_speed_multiplier: f64,
    pub field_plan_speed_multiplier: f64,
    pub max_units: usize,
    pub max_objectives: usize,
    pub max_grid_cells: usize,
    pub max_assignment_edges: usize,
}

impl Default for AiOrderConfig {
    fn default() -> Self {
        Self {
            contact_scan_radius: 0.6,
            retreat_min_hostile_power: 5.0,
            retreat_multiple: 8.0,
            retreat_boost: 5.5,
            encircled_retreat_multiplier: 0.25,
            prior_assignment_stickiness: 8.0,
            reinforcement_readiness_threshold: 0.45,
            contact_plan_speed_multiplier: 1.0,
            front_plan_speed_multiplier: 1.0,
            reinforcement_plan_speed_multiplier: 0.75,
            field_plan_speed_multiplier: 1.0,
            max_units: 100_000,
            max_objectives: 10_000,
            max_grid_cells: 5_000_000,
            max_assignment_edges: 5_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedMovementModifiers {
    pub terrain_speed_multiplier: f64,
    pub speed_multiplier: f64,
    pub plan_speed_multiplier: f64,
    pub neutral_penalty: f64,
    pub push_readiness: f64,
}

impl Default for ResolvedMovementModifiers {
    fn default() -> Self {
        Self {
            terrain_speed_multiplier: 1.0,
            speed_multiplier: 1.0,
            plan_speed_multiplier: 1.0,
            neutral_penalty: 1.0,
            push_readiness: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedCombatModifiers {
    pub dealt_multiplier: f64,
    pub taken_multiplier: f64,
    pub defense_bonus: f64,
    pub long_war_defense: f64,
    pub mountain: bool,
    pub urban: bool,
    /// Exact current-cell flags are present only for the live battlefield resolver. They let
    /// simulation combine both units' terrain per contact without changing legacy resolved-order
    /// fixtures, whose `mountain`/`urban` values remain attacker-owned.
    pub current_cell_mountain: Option<bool>,
    pub current_cell_urban: Option<bool>,
}

impl Default for ResolvedCombatModifiers {
    fn default() -> Self {
        Self {
            dealt_multiplier: 1.0,
            taken_multiplier: 1.0,
            defense_bonus: 1.0,
            long_war_defense: 1.0,
            mountain: false,
            urban: false,
            current_cell_mountain: None,
            current_cell_urban: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AiUnitInput {
    pub id: u64,
    pub side: SideKey,
    pub sovereign: u64,
    pub kind: UnitKind,
    pub lat: f64,
    pub lng: f64,
    pub health: f64,
    pub max_health: f64,
    /// Formation-equivalent local strength. This is also the hostile centroid weight.
    pub combat_power: f64,
    /// Applied to this unit's combat power when counted as a nearby friendly.
    pub ally_weight: f64,
    pub at_sea: bool,
    pub transport: bool,
    pub base_speed: f64,
    pub movement: ResolvedMovementModifiers,
    pub combat: ResolvedCombatModifiers,
    pub prior_front_objective_id: Option<u64>,
    pub is_reserve: bool,
    pub reinforcement_eligible: bool,
    pub encircled: bool,
}

impl AiUnitInput {
    pub fn readiness(self) -> f64 {
        self.health / self.max_health
    }
}

/// An explicitly ordered objective. `side_pair[0]` owns the objective and
/// `side_pair[1]` is its opponent. Reverse-direction planning needs a separate
/// objective, which keeps asymmetric hostility unambiguous.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrontObjective {
    pub id: u64,
    pub side_pair: [SideKey; 2],
    pub segment_id: u64,
    pub lat: f64,
    pub lng: f64,
    pub capacity: usize,
    pub priority: i32,
}

impl FrontObjective {
    pub fn new(
        id: u64,
        side_pair: [SideKey; 2],
        segment_id: u64,
        lat: f64,
        lng: f64,
        capacity: usize,
        priority: i32,
    ) -> Result<Self, AiOrderError> {
        if !lat.is_finite()
            || !lng.is_finite()
            || !(-90.0..=90.0).contains(&lat)
            || !(-180.0..=180.0).contains(&lng)
            || capacity == 0
            || side_pair[0] == side_pair[1]
        {
            return Err(AiOrderError::InvalidObjective(id));
        }
        Ok(Self {
            id,
            side_pair,
            segment_id,
            lat,
            lng,
            capacity,
            priority,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AiWorldInput<'a> {
    pub grid_width: usize,
    pub grid_height: usize,
    pub grid_res: f64,
    pub land_mask: &'a [u8],
    pub dominant_side_map: &'a [i16],
    pub hostility: HostilityMatrix<'a>,
    pub frontline_latitude: Option<&'a [f32]>,
    pub frontline_longitude: Option<&'a [f32]>,
    pub objectives: &'a [FrontObjective],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentReason {
    Contact,
    Retreat,
    Front,
    Reinforce,
    Field,
    Hold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontAssignmentRecord {
    pub unit_id: u64,
    pub objective_id: Option<u64>,
    pub reason: AssignmentReason,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiPlanningCounters {
    pub input_units: usize,
    pub contact_orders: usize,
    pub retreat_orders: usize,
    pub sticky_assignments: usize,
    pub front_assignments: usize,
    pub reinforcement_assignments: usize,
    pub field_orders: usize,
    pub hold_orders: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AiPlanningResult {
    pub schema_version: &'static str,
    pub orders: Vec<ResolvedUnitOrder>,
    pub assignments: Vec<FrontAssignmentRecord>,
    pub counters: AiPlanningCounters,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AiOrderError {
    #[error("AI order configuration is invalid")]
    InvalidConfig,
    #[error("AI input exceeds configured planning bounds")]
    PlanningLimitExceeded,
    #[error("world planning input is invalid")]
    InvalidWorld,
    #[error("hostility matrix is invalid")]
    InvalidHostility,
    #[error("unit {0} contains invalid data")]
    InvalidUnit(u64),
    #[error("unit id {0} is duplicated")]
    DuplicateUnit(u64),
    #[error("front objective {0} contains invalid data")]
    InvalidObjective(u64),
    #[error("front objective id {0} is duplicated")]
    DuplicateObjective(u64),
    #[error(transparent)]
    Tactical(#[from] TacticalGridError),
}

#[derive(Clone, Copy, Debug, Default)]
struct ContactState {
    preferred_target_id: Option<u64>,
    preferred_distance_sq: f64,
    preferred_delta_lat: f64,
    preferred_delta_lng: f64,
    friendly_power: f64,
    hostile_power: f64,
    hostile_delta_lat: f64,
    hostile_delta_lng: f64,
    retreat: bool,
    retreat_dir: Option<(f64, f64)>,
}

#[derive(Clone, Copy, Debug)]
struct AssignmentEdge {
    unit_index: usize,
    objective_index: usize,
    distance_sq: f64,
}

/// Resolve one immutable planning snapshot atomically.
pub fn resolve_ai_orders(
    config: AiOrderConfig,
    units: &[AiUnitInput],
    world: AiWorldInput<'_>,
) -> Result<AiPlanningResult, AiOrderError> {
    validate_all(config, units, world)?;

    let mut sorted_units = units.to_vec();
    sorted_units.sort_unstable_by_key(|unit| unit.id);
    let contacts = discover_contacts(config, &sorted_units, world.hostility)?;
    let reinforcement = sorted_units
        .iter()
        .map(|unit| {
            unit.is_reserve
                || (unit.reinforcement_eligible
                    && unit.readiness() <= config.reinforcement_readiness_threshold)
        })
        .collect::<Vec<_>>();

    let objective_by_id = world
        .objectives
        .iter()
        .enumerate()
        .map(|(index, objective)| (objective.id, index))
        .collect::<BTreeMap<_, _>>();
    let mut assigned_objective = vec![None; sorted_units.len()];
    let mut occupancy = vec![0_usize; world.objectives.len()];
    let mut counters = AiPlanningCounters {
        input_units: sorted_units.len(),
        ..AiPlanningCounters::default()
    };

    preserve_sticky_assignments(
        config,
        &sorted_units,
        world,
        &contacts,
        &reinforcement,
        &objective_by_id,
        &mut assigned_objective,
        &mut occupancy,
        &mut counters,
    );
    let mut assignment_edges = 0;
    assign_main_fronts(
        config.max_assignment_edges,
        &mut assignment_edges,
        &sorted_units,
        world,
        &contacts,
        &reinforcement,
        &mut assigned_objective,
        &mut occupancy,
        &mut counters,
    )?;
    assign_reinforcements(
        config.max_assignment_edges,
        &mut assignment_edges,
        &sorted_units,
        world,
        &contacts,
        &reinforcement,
        &mut assigned_objective,
        &mut occupancy,
        &mut counters,
    )?;

    let world_grid = WorldGridView {
        grid_res: world.grid_res,
        width: world.grid_width,
        height: world.grid_height,
        land_mask: world.land_mask,
    };
    let mut fallback_sides = vec![false; world.hostility.max_sides];
    for (index, unit) in sorted_units.iter().enumerate() {
        let contact = contacts[index];
        let has_contact_direction = contact.preferred_target_id.is_some()
            && normalize(contact.preferred_delta_lat, contact.preferred_delta_lng).is_some();
        if reinforcement[index]
            && !contact.retreat
            && !has_contact_direction
            && assigned_objective[index].is_none()
        {
            fallback_sides[usize::from(unit.side)] = true;
        }
    }
    let mut friendly_cells = (0..world.hostility.max_sides)
        .map(|_| Vec::new())
        .collect::<Vec<_>>();
    if fallback_sides.iter().any(|needed| *needed) {
        for (cell, &side) in world.dominant_side_map.iter().enumerate() {
            if side >= 0 && world.land_mask[cell] != 0 && fallback_sides[side as usize] {
                friendly_cells[side as usize].push(cell);
            }
        }
    }
    let mut orders = Vec::with_capacity(sorted_units.len());
    let mut assignments = Vec::with_capacity(sorted_units.len());
    for (unit_index, unit) in sorted_units.iter().enumerate() {
        let contact = contacts[unit_index];
        let objective_index = assigned_objective[unit_index];
        let (direction, reason) = choose_direction(
            *unit,
            contact,
            reinforcement[unit_index],
            objective_index,
            world,
            world_grid,
            &friendly_cells[usize::from(unit.side)],
        );
        let (dir_lat, dir_lng) = direction.unwrap_or((0.0, 0.0));
        let movement_enabled = direction.is_some() && unit.base_speed > 0.0 && unit.health > 0.0;
        let reason_speed_multiplier = match reason {
            AssignmentReason::Contact => config.contact_plan_speed_multiplier,
            AssignmentReason::Front => config.front_plan_speed_multiplier,
            AssignmentReason::Reinforce => config.reinforcement_plan_speed_multiplier,
            AssignmentReason::Field => config.field_plan_speed_multiplier,
            AssignmentReason::Retreat | AssignmentReason::Hold => 1.0,
        };
        let retreat_boost = if reason == AssignmentReason::Retreat {
            config.retreat_boost
                * if unit.encircled {
                    config.encircled_retreat_multiplier
                } else {
                    1.0
                }
        } else {
            1.0
        };

        match reason {
            AssignmentReason::Contact => counters.contact_orders += 1,
            AssignmentReason::Retreat => counters.retreat_orders += 1,
            AssignmentReason::Field => counters.field_orders += 1,
            AssignmentReason::Hold => counters.hold_orders += 1,
            AssignmentReason::Front | AssignmentReason::Reinforce => {}
        }
        orders.push(ResolvedUnitOrder {
            unit_id: unit.id,
            preferred_target_id: contact.preferred_target_id,
            movement_enabled,
            dir_lat,
            dir_lng,
            factors: MovementFactors {
                base_speed: unit.base_speed,
                speed_mult: unit.movement.terrain_speed_multiplier * unit.movement.speed_multiplier,
                plan_speed_mult: unit.movement.plan_speed_multiplier * reason_speed_multiplier,
                neutral_penalty: unit.movement.neutral_penalty,
                retreat_boost,
                push_readiness: unit.movement.push_readiness,
            },
            combat: ResolvedCombatOrder {
                dealt_multiplier: unit.combat.dealt_multiplier,
                taken_multiplier: unit.combat.taken_multiplier,
                defense_bonus: unit.combat.defense_bonus,
                long_war_defense: unit.combat.long_war_defense,
                mountain: unit.combat.mountain,
                urban: unit.combat.urban,
                current_cell_mountain: unit.combat.current_cell_mountain,
                current_cell_urban: unit.combat.current_cell_urban,
            },
        });
        assignments.push(FrontAssignmentRecord {
            unit_id: unit.id,
            objective_id: objective_index.map(|index| world.objectives[index].id),
            reason,
        });
    }

    Ok(AiPlanningResult {
        schema_version: AI_ORDER_SCHEMA_VERSION,
        orders,
        assignments,
        counters,
    })
}

fn validate_all(
    config: AiOrderConfig,
    units: &[AiUnitInput],
    world: AiWorldInput<'_>,
) -> Result<(), AiOrderError> {
    let config_values = [
        config.contact_scan_radius,
        config.retreat_min_hostile_power,
        config.retreat_multiple,
        config.retreat_boost,
        config.encircled_retreat_multiplier,
        config.prior_assignment_stickiness,
        config.reinforcement_readiness_threshold,
        config.contact_plan_speed_multiplier,
        config.front_plan_speed_multiplier,
        config.reinforcement_plan_speed_multiplier,
        config.field_plan_speed_multiplier,
    ];
    if config_values.iter().any(|value| !value.is_finite())
        || config.contact_scan_radius <= 0.0
        || config.contact_scan_radius > 180.0
        || config.retreat_min_hostile_power < 0.0
        || config.retreat_multiple < 0.0
        || config.retreat_boost < 0.0
        || config.encircled_retreat_multiplier < 0.0
        || config.prior_assignment_stickiness < 0.0
        || !(0.0..=1.0).contains(&config.reinforcement_readiness_threshold)
        || config.contact_plan_speed_multiplier < 0.0
        || config.front_plan_speed_multiplier < 0.0
        || config.reinforcement_plan_speed_multiplier < 0.0
        || config.field_plan_speed_multiplier < 0.0
        || config.max_units == 0
        || config.max_objectives == 0
        || config.max_grid_cells == 0
        || config.max_assignment_edges == 0
    {
        return Err(AiOrderError::InvalidConfig);
    }
    if units.len() > config.max_units || world.objectives.len() > config.max_objectives {
        return Err(AiOrderError::PlanningLimitExceeded);
    }
    let cell_count = world
        .grid_width
        .checked_mul(world.grid_height)
        .ok_or(AiOrderError::InvalidWorld)?;
    if cell_count > config.max_grid_cells {
        return Err(AiOrderError::PlanningLimitExceeded);
    }
    WorldGridView::new(
        world.grid_res,
        world.grid_width,
        world.grid_height,
        world.land_mask,
    )
    .map_err(|_| AiOrderError::InvalidWorld)?;
    if world.dominant_side_map.len() != cell_count || world.land_mask.iter().any(|value| *value > 2)
    {
        return Err(AiOrderError::InvalidWorld);
    }
    validate_hostility(world.hostility)?;
    if world.dominant_side_map.iter().any(|side| {
        *side < -1 || (*side >= 0 && usize::try_from(*side).unwrap() >= world.hostility.max_sides)
    }) {
        return Err(AiOrderError::InvalidWorld);
    }
    match (world.frontline_latitude, world.frontline_longitude) {
        (None, None) => {}
        (Some(latitude), Some(longitude))
            if latitude.len() == cell_count
                && longitude.len() == cell_count
                && latitude.iter().all(|value| value.is_finite())
                && longitude.iter().all(|value| value.is_finite()) => {}
        _ => return Err(AiOrderError::InvalidWorld),
    }

    let mut unit_ids = BTreeMap::new();
    for unit in units {
        if unit_ids.insert(unit.id, ()).is_some() {
            return Err(AiOrderError::DuplicateUnit(unit.id));
        }
        validate_unit(*unit, world.hostility.max_sides)?;
    }
    let mut objective_ids = BTreeMap::new();
    for objective in world.objectives {
        if objective_ids.insert(objective.id, ()).is_some() {
            return Err(AiOrderError::DuplicateObjective(objective.id));
        }
        validate_objective(*objective, world.hostility.max_sides)?;
    }
    Ok(())
}

fn validate_hostility(hostility: HostilityMatrix<'_>) -> Result<(), AiOrderError> {
    if hostility.max_sides == 0 || hostility.max_sides > 128 {
        return Err(AiOrderError::InvalidHostility);
    }
    if let Some(relations) = hostility.relations {
        let expected = hostility
            .max_sides
            .checked_mul(hostility.max_sides)
            .ok_or(AiOrderError::InvalidHostility)?;
        if relations.len() != expected || relations.iter().any(|value| *value > 1) {
            return Err(AiOrderError::InvalidHostility);
        }
    }
    Ok(())
}

fn validate_unit(unit: AiUnitInput, max_sides: usize) -> Result<(), AiOrderError> {
    let values = [
        unit.lat,
        unit.lng,
        unit.health,
        unit.max_health,
        unit.combat_power,
        unit.ally_weight,
        unit.base_speed,
        unit.movement.terrain_speed_multiplier,
        unit.movement.speed_multiplier,
        unit.movement.plan_speed_multiplier,
        unit.movement.neutral_penalty,
        unit.movement.push_readiness,
        unit.combat.dealt_multiplier,
        unit.combat.taken_multiplier,
        unit.combat.defense_bonus,
        unit.combat.long_war_defense,
    ];
    if usize::from(unit.side) >= max_sides
        || values.iter().any(|value| !value.is_finite())
        || !(-90.0..=90.0).contains(&unit.lat)
        || !(-180.0..=180.0).contains(&unit.lng)
        || unit.max_health <= 0.0
        || unit.health < 0.0
        || unit.health > unit.max_health
        || unit.combat_power < 0.0
        || unit.ally_weight < 0.0
        || unit.base_speed < 0.0
        || unit.movement.terrain_speed_multiplier < 0.0
        || unit.movement.speed_multiplier < 0.0
        || unit.movement.plan_speed_multiplier < 0.0
        || unit.movement.neutral_penalty < 0.0
        || unit.movement.push_readiness < 0.0
        || unit.combat.dealt_multiplier < 0.0
        || unit.combat.taken_multiplier < 0.0
        || unit.combat.defense_bonus < 0.0
        || unit.combat.long_war_defense < 0.0
    {
        return Err(AiOrderError::InvalidUnit(unit.id));
    }
    Ok(())
}

fn validate_objective(objective: FrontObjective, max_sides: usize) -> Result<(), AiOrderError> {
    if objective.capacity == 0
        || objective.side_pair[0] == objective.side_pair[1]
        || objective
            .side_pair
            .iter()
            .any(|side| usize::from(*side) >= max_sides)
        || !objective.lat.is_finite()
        || !objective.lng.is_finite()
        || !(-90.0..=90.0).contains(&objective.lat)
        || !(-180.0..=180.0).contains(&objective.lng)
    {
        return Err(AiOrderError::InvalidObjective(objective.id));
    }
    Ok(())
}

fn discover_contacts(
    config: AiOrderConfig,
    units: &[AiUnitInput],
    hostility: HostilityMatrix<'_>,
) -> Result<Vec<ContactState>, AiOrderError> {
    let tactical_units = units
        .iter()
        .map(|unit| TacticalUnit {
            id: unit.id,
            side: Some(unit.side),
            lat: unit.lat,
            lng: unit.lng,
            strength: unit.combat_power,
            ally_weight: unit.ally_weight,
            is_armor: unit.kind == UnitKind::Armor,
            is_support: false,
        })
        .collect::<Vec<_>>();
    let mut grid = TacticalGrid::new(config.contact_scan_radius)?;
    grid.rebuild(&tactical_units)?;
    let radius_sq = config.contact_scan_radius * config.contact_scan_radius;
    let mut contacts = Vec::with_capacity(units.len());

    for (unit_index, unit) in units.iter().enumerate() {
        let mut contact = ContactState {
            preferred_distance_sq: f64::INFINITY,
            friendly_power: unit.combat_power * unit.ally_weight,
            ..ContactState::default()
        };
        let origin =
            crate::tactical::tactical_cell_coords(unit.lat, unit.lng, config.contact_scan_radius)?
                .ok_or(AiOrderError::InvalidUnit(unit.id))?;
        for &other_side in grid.by_side.keys() {
            let hostile = is_hostile(hostility, unit.side, other_side);
            if other_side != unit.side && !hostile {
                continue;
            }
            grid.for_each_neighbor_cell(
                other_side,
                origin,
                NeighborOptions { radius_cells: 1 },
                |cell| {
                    for &other_index in &cell.units {
                        if other_index == unit_index {
                            continue;
                        }
                        let other = units[other_index];
                        let delta_lat = other.lat - unit.lat;
                        let delta_lng = wrapped_longitude_delta(unit.lng, other.lng);
                        let distance_sq = delta_lat * delta_lat + delta_lng * delta_lng;
                        if distance_sq > radius_sq {
                            continue;
                        }
                        if hostile {
                            contact.hostile_power += other.combat_power;
                            contact.hostile_delta_lat += delta_lat * other.combat_power;
                            contact.hostile_delta_lng += delta_lng * other.combat_power;
                            let better = distance_sq < contact.preferred_distance_sq
                                || (distance_sq == contact.preferred_distance_sq
                                    && contact.preferred_target_id.is_none_or(|id| other.id < id));
                            if better {
                                contact.preferred_distance_sq = distance_sq;
                                contact.preferred_target_id = Some(other.id);
                                contact.preferred_delta_lat = delta_lat;
                                contact.preferred_delta_lng = delta_lng;
                            }
                        } else if other_side == unit.side {
                            contact.friendly_power += other.combat_power * other.ally_weight;
                        }
                    }
                },
            );
        }
        contact.retreat = contact.hostile_power >= config.retreat_min_hostile_power
            && contact.hostile_power > contact.friendly_power * config.retreat_multiple;
        if contact.retreat && contact.hostile_power > 0.0 {
            contact.retreat_dir = normalize(
                -contact.hostile_delta_lat / contact.hostile_power,
                -contact.hostile_delta_lng / contact.hostile_power,
            );
        }
        contacts.push(contact);
    }
    Ok(contacts)
}

#[allow(clippy::too_many_arguments)]
fn preserve_sticky_assignments(
    config: AiOrderConfig,
    units: &[AiUnitInput],
    world: AiWorldInput<'_>,
    contacts: &[ContactState],
    reinforcement: &[bool],
    objective_by_id: &BTreeMap<u64, usize>,
    assigned: &mut [Option<usize>],
    occupancy: &mut [usize],
    counters: &mut AiPlanningCounters,
) {
    let stickiness_sq = config.prior_assignment_stickiness.powi(2);
    let mut candidates = Vec::new();
    for (unit_index, unit) in units.iter().enumerate() {
        if contacts[unit_index].retreat || reinforcement[unit_index] {
            continue;
        }
        let Some(objective_index) = unit
            .prior_front_objective_id
            .and_then(|id| objective_by_id.get(&id).copied())
        else {
            continue;
        };
        let objective = world.objectives[objective_index];
        let distance_sq = objective_distance_sq(*unit, objective);
        if objective_applies(*unit, objective, world.hostility) && distance_sq <= stickiness_sq {
            candidates.push(AssignmentEdge {
                unit_index,
                objective_index,
                distance_sq,
            });
        }
    }
    candidates.sort_unstable_by(|left, right| {
        world.objectives[left.objective_index]
            .id
            .cmp(&world.objectives[right.objective_index].id)
            .then_with(|| left.distance_sq.total_cmp(&right.distance_sq))
            .then_with(|| units[left.unit_index].id.cmp(&units[right.unit_index].id))
    });
    for edge in candidates {
        let capacity = world.objectives[edge.objective_index].capacity;
        if assigned[edge.unit_index].is_none() && occupancy[edge.objective_index] < capacity {
            assigned[edge.unit_index] = Some(edge.objective_index);
            occupancy[edge.objective_index] += 1;
            counters.sticky_assignments += 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn assign_main_fronts(
    max_assignment_edges: usize,
    assignment_edges: &mut usize,
    units: &[AiUnitInput],
    world: AiWorldInput<'_>,
    contacts: &[ContactState],
    reinforcement: &[bool],
    assigned: &mut [Option<usize>],
    occupancy: &mut [usize],
    counters: &mut AiPlanningCounters,
) -> Result<(), AiOrderError> {
    let mut edges = Vec::new();
    for (unit_index, unit) in units.iter().enumerate() {
        if contacts[unit_index].retreat
            || reinforcement[unit_index]
            || assigned[unit_index].is_some()
        {
            continue;
        }
        for (objective_index, objective) in world.objectives.iter().enumerate() {
            if occupancy[objective_index] >= objective.capacity {
                continue;
            }
            if objective_applies(*unit, *objective, world.hostility) {
                if *assignment_edges == max_assignment_edges {
                    return Err(AiOrderError::PlanningLimitExceeded);
                }
                *assignment_edges += 1;
                edges.push(AssignmentEdge {
                    unit_index,
                    objective_index,
                    distance_sq: objective_distance_sq(*unit, *objective),
                });
            }
        }
    }
    edges.sort_unstable_by(|left, right| {
        world.objectives[right.objective_index]
            .priority
            .cmp(&world.objectives[left.objective_index].priority)
            .then_with(|| left.distance_sq.total_cmp(&right.distance_sq))
            .then_with(|| {
                world.objectives[left.objective_index]
                    .id
                    .cmp(&world.objectives[right.objective_index].id)
            })
            .then_with(|| units[left.unit_index].id.cmp(&units[right.unit_index].id))
    });
    for edge in edges {
        let capacity = world.objectives[edge.objective_index].capacity;
        if assigned[edge.unit_index].is_none() && occupancy[edge.objective_index] < capacity {
            assigned[edge.unit_index] = Some(edge.objective_index);
            occupancy[edge.objective_index] += 1;
            counters.front_assignments += 1;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn assign_reinforcements(
    max_assignment_edges: usize,
    assignment_edges: &mut usize,
    units: &[AiUnitInput],
    world: AiWorldInput<'_>,
    contacts: &[ContactState],
    reinforcement: &[bool],
    assigned: &mut [Option<usize>],
    occupancy: &mut [usize],
    counters: &mut AiPlanningCounters,
) -> Result<(), AiOrderError> {
    let mut unit_indices = (0..units.len())
        .filter(|index| reinforcement[*index] && !contacts[*index].retreat)
        .collect::<Vec<_>>();
    unit_indices.sort_unstable_by(|left, right| {
        units[*left]
            .readiness()
            .total_cmp(&units[*right].readiness())
            .then_with(|| units[*left].id.cmp(&units[*right].id))
    });
    for unit_index in unit_indices {
        let unit = units[unit_index];
        let mut best: Option<usize> = None;
        for (objective_index, objective) in world.objectives.iter().enumerate() {
            if occupancy[objective_index] >= objective.capacity {
                continue;
            }
            if !objective_applies(unit, *objective, world.hostility) {
                continue;
            }
            if *assignment_edges == max_assignment_edges {
                return Err(AiOrderError::PlanningLimitExceeded);
            }
            *assignment_edges += 1;
            let better = best.is_none_or(|best_index| {
                let best_objective = world.objectives[best_index];
                underfill_cmp(
                    occupancy[objective_index],
                    objective.capacity,
                    occupancy[best_index],
                    best_objective.capacity,
                )
                .then_with(|| best_objective.priority.cmp(&objective.priority))
                .then_with(|| {
                    objective_distance_sq(unit, *objective)
                        .total_cmp(&objective_distance_sq(unit, best_objective))
                })
                .then_with(|| objective.id.cmp(&best_objective.id))
                .is_lt()
            });
            if better {
                best = Some(objective_index);
            }
        }
        if let Some(objective_index) = best {
            assigned[unit_index] = Some(objective_index);
            occupancy[objective_index] += 1;
            counters.reinforcement_assignments += 1;
        }
    }
    Ok(())
}

fn choose_direction(
    unit: AiUnitInput,
    contact: ContactState,
    reinforcement: bool,
    objective_index: Option<usize>,
    world: AiWorldInput<'_>,
    world_grid: WorldGridView<'_>,
    friendly_cells: &[usize],
) -> (Option<(f64, f64)>, AssignmentReason) {
    if contact.retreat {
        return (contact.retreat_dir, AssignmentReason::Retreat);
    }
    if contact.preferred_target_id.is_some()
        && let Some(direction) = normalize(contact.preferred_delta_lat, contact.preferred_delta_lng)
    {
        return (Some(direction), AssignmentReason::Contact);
    }
    if let Some(index) = objective_index {
        let objective = world.objectives[index];
        let direction = direction_to(unit.lat, unit.lng, objective.lat, objective.lng);
        return (
            direction,
            if reinforcement {
                AssignmentReason::Reinforce
            } else {
                AssignmentReason::Front
            },
        );
    }
    if reinforcement {
        if let Some(direction) = direction_to_nearest_friendly_cell(unit, world, friendly_cells) {
            return (Some(direction), AssignmentReason::Reinforce);
        }
        return (None, AssignmentReason::Hold);
    }
    if let Some(direction) = sample_frontline_direction(unit, world, world_grid) {
        return (Some(direction), AssignmentReason::Field);
    }
    (None, AssignmentReason::Hold)
}

fn objective_applies(
    unit: AiUnitInput,
    objective: FrontObjective,
    hostility: HostilityMatrix<'_>,
) -> bool {
    unit.side == objective.side_pair[0]
        && is_hostile(hostility, objective.side_pair[0], objective.side_pair[1])
}

fn is_hostile(hostility: HostilityMatrix<'_>, left: SideKey, right: SideKey) -> bool {
    if left == right {
        return false;
    }
    match hostility.relations {
        None => true,
        Some(relations) => {
            let left = usize::from(left);
            let right = usize::from(right);
            left < hostility.max_sides
                && right < hostility.max_sides
                && relations[left * hostility.max_sides + right] == 1
        }
    }
}

fn objective_distance_sq(unit: AiUnitInput, objective: FrontObjective) -> f64 {
    let delta_lat = objective.lat - unit.lat;
    let delta_lng = wrapped_longitude_delta(unit.lng, objective.lng);
    delta_lat * delta_lat + delta_lng * delta_lng
}

fn direction_to(from_lat: f64, from_lng: f64, to_lat: f64, to_lng: f64) -> Option<(f64, f64)> {
    normalize(to_lat - from_lat, wrapped_longitude_delta(from_lng, to_lng))
}

fn normalize(lat: f64, lng: f64) -> Option<(f64, f64)> {
    let magnitude = lat.hypot(lng);
    if magnitude > 0.0 && magnitude.is_finite() {
        Some((lat / magnitude, lng / magnitude))
    } else {
        None
    }
}

fn sample_frontline_direction(
    unit: AiUnitInput,
    world: AiWorldInput<'_>,
    world_grid: WorldGridView<'_>,
) -> Option<(f64, f64)> {
    let (Some(latitude), Some(longitude)) = (world.frontline_latitude, world.frontline_longitude)
    else {
        return None;
    };
    let index = world_grid.grid_index(unit.lat, unit.lng)?;
    normalize(f64::from(latitude[index]), f64::from(longitude[index]))
}

fn direction_to_nearest_friendly_cell(
    unit: AiUnitInput,
    world: AiWorldInput<'_>,
    friendly_cells: &[usize],
) -> Option<(f64, f64)> {
    if friendly_cells.is_empty() {
        return None;
    }
    if friendly_cells.len() <= world.land_mask.len().isqrt().max(64) {
        let mut best: Option<(f64, usize, f64, f64)> = None;
        for &index in friendly_cells {
            let x = index % world.grid_width;
            let y = index / world.grid_width;
            let lat = (y as f64 + 0.5) * world.grid_res - 90.0;
            let lng = (x as f64 + 0.5) * world.grid_res - 180.0;
            let delta_lat = lat - unit.lat;
            let delta_lng = wrapped_longitude_delta(unit.lng, lng);
            let distance_sq = delta_lat * delta_lat + delta_lng * delta_lng;
            if best.is_none_or(|(best_distance, best_index, _, _)| {
                distance_sq < best_distance || (distance_sq == best_distance && index < best_index)
            }) {
                best = Some((distance_sq, index, lat, lng));
            }
        }
        let (_, _, lat, lng) = best?;
        return direction_to(unit.lat, unit.lng, lat, lng);
    }
    let wrapped_lng = crate::tactical::wrap_tactical_longitude(unit.lng);
    let fractional_x = (wrapped_lng + 180.0) / world.grid_res - 0.5;
    let fractional_y = (unit.lat + 90.0) / world.grid_res - 0.5;
    let raw_base_x = fractional_x.round() as isize;
    let raw_base_y = fractional_y.round() as isize;
    let base_x = raw_base_x.rem_euclid(world.grid_width as isize);
    let base_y = raw_base_y.clamp(0, world.grid_height as isize - 1);
    let fractional_offset = (fractional_x - raw_base_x as f64)
        .abs()
        .max((fractional_y - raw_base_y as f64).abs());
    let max_x_radius = world.grid_width.div_ceil(2);
    let max_y_radius = usize::try_from(base_y.max(world.grid_height as isize - 1 - base_y)).ok()?;
    let max_radius = max_x_radius.max(max_y_radius);
    let target_side = i16::try_from(unit.side).ok()?;
    let mut best: Option<(f64, usize, f64, f64)> = None;
    let visit = |dx: isize, dy: isize, best: &mut Option<(f64, usize, f64, f64)>| {
        let y = base_y + dy;
        if y < 0 || y >= world.grid_height as isize {
            return;
        }
        let x = (base_x + dx).rem_euclid(world.grid_width as isize) as usize;
        let y = y as usize;
        let index = y * world.grid_width + x;
        if world.land_mask[index] == 0 || world.dominant_side_map[index] != target_side {
            return;
        }
        let lat = (y as f64 + 0.5) * world.grid_res - 90.0;
        let lng = (x as f64 + 0.5) * world.grid_res - 180.0;
        let delta_lat = lat - unit.lat;
        let delta_lng = wrapped_longitude_delta(unit.lng, lng);
        let distance_sq = delta_lat * delta_lat + delta_lng * delta_lng;
        if best.is_none_or(|(best_distance, best_index, _, _)| {
            distance_sq < best_distance || (distance_sq == best_distance && index < best_index)
        }) {
            *best = Some((distance_sq, index, lat, lng));
        }
    };
    for radius in 0..=max_radius {
        let radius = radius as isize;
        if radius == 0 {
            visit(0, 0, &mut best);
        } else {
            for dx in -radius..=radius {
                visit(dx, -radius, &mut best);
                visit(dx, radius, &mut best);
            }
            for dy in (-radius + 1)..radius {
                visit(-radius, dy, &mut best);
                visit(radius, dy, &mut best);
            }
        }
        let next_radius = radius as f64 + 1.0;
        let lower_bound = (next_radius - fractional_offset).max(0.0) * world.grid_res;
        if best.is_some_and(|(distance_sq, _, _, _)| lower_bound * lower_bound > distance_sq) {
            break;
        }
    }
    let (_, _, lat, lng) = best?;
    direction_to(unit.lat, unit.lng, lat, lng)
}

fn underfill_cmp(
    left_occupancy: usize,
    left_capacity: usize,
    right_occupancy: usize,
    right_capacity: usize,
) -> Ordering {
    (left_occupancy as u128 * right_capacity as u128)
        .cmp(&(right_occupancy as u128 * left_capacity as u128))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(id: u64, side: SideKey, lat: f64, lng: f64) -> AiUnitInput {
        AiUnitInput {
            id,
            side,
            sovereign: u64::from(side) + 1,
            kind: UnitKind::Army,
            lat,
            lng,
            health: 100.0,
            max_health: 100.0,
            combat_power: 1.0,
            ally_weight: 1.0,
            at_sea: false,
            transport: false,
            base_speed: 0.003,
            movement: ResolvedMovementModifiers::default(),
            combat: ResolvedCombatModifiers::default(),
            prior_front_objective_id: None,
            is_reserve: false,
            reinforcement_eligible: false,
            encircled: false,
        }
    }

    fn objective(id: u64, lat: f64, lng: f64, capacity: usize, priority: i32) -> FrontObjective {
        FrontObjective::new(id, [0, 1], id, lat, lng, capacity, priority).unwrap()
    }

    fn world<'a>(
        land: &'a [u8],
        dominant: &'a [i16],
        relations: &'a [u8],
        objectives: &'a [FrontObjective],
        latitude: Option<&'a [f32]>,
        longitude: Option<&'a [f32]>,
    ) -> AiWorldInput<'a> {
        AiWorldInput {
            grid_width: 4,
            grid_height: 2,
            grid_res: 90.0,
            land_mask: land,
            dominant_side_map: dominant,
            hostility: HostilityMatrix::new(Some(relations), 2),
            frontline_latitude: latitude,
            frontline_longitude: longitude,
            objectives,
        }
    }

    fn standard_world<'a>(
        land: &'a [u8],
        dominant: &'a [i16],
        objectives: &'a [FrontObjective],
    ) -> AiWorldInput<'a> {
        world(land, dominant, &[0, 1, 1, 0], objectives, None, None)
    }

    #[test]
    fn input_and_objective_permutations_produce_identical_output() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let objectives_a = [
            objective(20, 0.0, 4.0, 1, 3),
            objective(10, 0.0, -4.0, 1, 3),
        ];
        let objectives_b = [objectives_a[1], objectives_a[0]];
        let units_a = [
            unit(3, 0, 0.0, -3.0),
            unit(1, 0, 0.0, 3.0),
            unit(2, 0, 20.0, 0.0),
        ];
        let units_b = [units_a[1], units_a[2], units_a[0]];

        let left = resolve_ai_orders(
            AiOrderConfig::default(),
            &units_a,
            standard_world(&land, &dominant, &objectives_a),
        )
        .unwrap();
        let right = resolve_ai_orders(
            AiOrderConfig::default(),
            &units_b,
            standard_world(&land, &dominant, &objectives_b),
        )
        .unwrap();

        assert_eq!(left, right);
        assert_eq!(
            left.orders
                .iter()
                .map(|order| order.unit_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn equally_near_contact_uses_lower_unit_id() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let units = [
            unit(10, 0, 0.0, 0.0),
            unit(3, 1, 0.0, 0.1),
            unit(2, 1, 0.0, -0.1),
        ];
        let result = resolve_ai_orders(
            AiOrderConfig::default(),
            &units,
            standard_world(&land, &dominant, &[]),
        )
        .unwrap();
        let order = result
            .orders
            .iter()
            .find(|order| order.unit_id == 10)
            .unwrap();
        assert_eq!(order.preferred_target_id, Some(2));
        assert_eq!((order.dir_lat, order.dir_lng), (0.0, -1.0));
    }

    #[test]
    fn hostility_is_directional() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let relations = [0, 1, 0, 0];
        let units = [unit(1, 0, 0.0, 0.0), unit(2, 1, 0.0, 0.1)];
        let result = resolve_ai_orders(
            AiOrderConfig::default(),
            &units,
            world(&land, &dominant, &relations, &[], None, None),
        )
        .unwrap();
        assert_eq!(result.orders[0].preferred_target_id, Some(2));
        assert_eq!(result.orders[1].preferred_target_id, None);
        assert_eq!(result.assignments[1].reason, AssignmentReason::Hold);
    }

    #[test]
    fn retreat_uses_inclusive_minimum_and_strict_multiple() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let mut config = AiOrderConfig {
            retreat_multiple: 1.0,
            ..AiOrderConfig::default()
        };
        let mut enemy = unit(2, 1, 0.0, 0.1);
        enemy.combat_power = 5.0;
        let result = resolve_ai_orders(
            config,
            &[unit(1, 0, 0.0, 0.0), enemy],
            standard_world(&land, &dominant, &[]),
        )
        .unwrap();
        assert_eq!(result.assignments[0].reason, AssignmentReason::Retreat);
        assert_eq!(result.orders[0].factors.retreat_boost, 5.5);

        let mut friendly = unit(1, 0, 0.0, 0.0);
        friendly.combat_power = 5.0;
        let exact = resolve_ai_orders(
            config,
            &[friendly, enemy],
            standard_world(&land, &dominant, &[]),
        )
        .unwrap();
        assert_eq!(exact.assignments[0].reason, AssignmentReason::Contact);

        config.retreat_min_hostile_power = 5.01;
        let below_minimum = resolve_ai_orders(
            config,
            &[unit(1, 0, 0.0, 0.0), enemy],
            standard_world(&land, &dominant, &[]),
        )
        .unwrap();
        assert_eq!(
            below_minimum.assignments[0].reason,
            AssignmentReason::Contact
        );
    }

    #[test]
    fn retreat_centroid_uses_wrapped_longitude_and_encirclement_multiplier() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let mut friendly = unit(1, 0, 0.0, 179.9);
        friendly.encircled = true;
        let mut enemy = unit(2, 1, 0.0, -179.9);
        enemy.combat_power = 5.0;
        let result = resolve_ai_orders(
            AiOrderConfig {
                retreat_multiple: 1.0,
                ..AiOrderConfig::default()
            },
            &[friendly, enemy],
            standard_world(&land, &dominant, &[]),
        )
        .unwrap();
        assert_eq!(result.assignments[0].reason, AssignmentReason::Retreat);
        assert_eq!(
            (result.orders[0].dir_lat, result.orders[0].dir_lng),
            (0.0, -1.0)
        );
        assert_eq!(result.orders[0].factors.retreat_boost, 5.5 * 0.25);
    }

    #[test]
    fn prior_assignment_stickiness_keeps_nearest_unit_within_capacity() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let objectives = [objective(10, 0.0, 0.0, 1, 1)];
        let mut near = unit(20, 0, 0.0, 0.1);
        near.prior_front_objective_id = Some(10);
        let mut far = unit(10, 0, 0.0, 1.0);
        far.prior_front_objective_id = Some(10);
        let result = resolve_ai_orders(
            AiOrderConfig::default(),
            &[far, near],
            standard_world(&land, &dominant, &objectives),
        )
        .unwrap();
        assert_eq!(result.counters.sticky_assignments, 1);
        assert_eq!(result.assignments[0].objective_id, None);
        assert_eq!(result.assignments[1].objective_id, Some(10));
    }

    #[test]
    fn assignment_edge_limit_counts_only_work_left_after_sticky_slots() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let objectives = [
            objective(10, 0.0, -1.0, 1, 1),
            objective(20, 0.0, 0.0, 1, 1),
            objective(30, 0.0, 1.0, 1, 1),
        ];
        let mut units = [
            unit(1, 0, 0.0, -1.0),
            unit(2, 0, 0.0, 0.0),
            unit(3, 0, 0.0, 1.0),
        ];
        for (unit, objective) in units.iter_mut().zip(objectives.iter()) {
            unit.prior_front_objective_id = Some(objective.id);
        }
        let config = AiOrderConfig {
            max_assignment_edges: 1,
            ..AiOrderConfig::default()
        };

        let result = resolve_ai_orders(
            config,
            &units,
            standard_world(&land, &dominant, &objectives),
        )
        .unwrap();
        assert_eq!(result.counters.sticky_assignments, 3);
        assert_eq!(result.counters.front_assignments, 0);

        let mut without_priors = units;
        for unit in &mut without_priors {
            unit.prior_front_objective_id = None;
        }
        assert!(matches!(
            resolve_ai_orders(
                config,
                &without_priors,
                standard_world(&land, &dominant, &objectives),
            ),
            Err(AiOrderError::PlanningLimitExceeded)
        ));
    }

    #[test]
    fn capacity_assignment_is_deterministic_by_distance_then_id() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let objectives = [
            objective(10, 0.0, -5.0, 1, 1),
            objective(20, 0.0, 5.0, 1, 1),
        ];
        let units = [
            unit(3, 0, 0.0, -4.0),
            unit(1, 0, 0.0, 4.0),
            unit(2, 0, 0.0, 0.0),
        ];
        let result = resolve_ai_orders(
            AiOrderConfig::default(),
            &units,
            standard_world(&land, &dominant, &objectives),
        )
        .unwrap();
        assert_eq!(result.assignments[0].objective_id, Some(20));
        assert_eq!(result.assignments[1].objective_id, None);
        assert_eq!(result.assignments[2].objective_id, Some(10));
    }

    #[test]
    fn reinforcement_selects_most_underfilled_front_after_main_assignment() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let objectives = [
            objective(10, 0.0, -1.0, 2, 10),
            objective(20, 0.0, 1.0, 1, 1),
        ];
        let main = unit(1, 0, 0.0, -1.0);
        let mut reserve = unit(2, 0, 0.0, 0.0);
        reserve.health = 45.0;
        reserve.reinforcement_eligible = true;
        let result = resolve_ai_orders(
            AiOrderConfig::default(),
            &[reserve, main],
            standard_world(&land, &dominant, &objectives),
        )
        .unwrap();
        assert_eq!(result.assignments[0].objective_id, Some(10));
        assert_eq!(result.assignments[1].objective_id, Some(20));
        assert_eq!(result.assignments[1].reason, AssignmentReason::Reinforce);
        assert_eq!(result.counters.reinforcement_assignments, 1);
    }

    #[test]
    fn reinforcement_edges_share_the_global_planning_limit() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let objectives = [
            objective(10, 0.0, -1.0, 1, 1),
            objective(20, 0.0, 1.0, 1, 1),
        ];
        let mut reserve = unit(1, 0, 0.0, 0.0);
        reserve.is_reserve = true;

        assert!(matches!(
            resolve_ai_orders(
                AiOrderConfig {
                    max_assignment_edges: 1,
                    ..AiOrderConfig::default()
                },
                &[reserve],
                standard_world(&land, &dominant, &objectives),
            ),
            Err(AiOrderError::PlanningLimitExceeded)
        ));
    }

    #[test]
    fn direction_field_is_normalized_fallback() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let mut latitude = [0.0_f32; 8];
        let mut longitude = [0.0_f32; 8];
        latitude[6] = 3.0;
        longitude[6] = 4.0;
        let result = resolve_ai_orders(
            AiOrderConfig::default(),
            &[unit(1, 0, 0.0, 0.0)],
            world(
                &land,
                &dominant,
                &[0, 1, 1, 0],
                &[],
                Some(&latitude),
                Some(&longitude),
            ),
        )
        .unwrap();
        assert_eq!(result.assignments[0].reason, AssignmentReason::Field);
        assert!((result.orders[0].dir_lat - 0.6).abs() < 1e-12);
        assert!((result.orders[0].dir_lng - 0.8).abs() < 1e-12);
    }

    #[test]
    fn no_objective_or_field_holds() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let result = resolve_ai_orders(
            AiOrderConfig::default(),
            &[unit(1, 0, 0.0, 0.0)],
            standard_world(&land, &dominant, &[]),
        )
        .unwrap();
        assert_eq!(result.assignments[0].reason, AssignmentReason::Hold);
        assert!(!result.orders[0].movement_enabled);
        assert_eq!(
            (result.orders[0].dir_lat, result.orders[0].dir_lng),
            (0.0, 0.0)
        );
    }

    #[test]
    fn browser_counted_land_value_two_is_valid() {
        let land = [2; 8];
        let dominant = [-1; 8];
        let result = resolve_ai_orders(
            AiOrderConfig::default(),
            &[unit(1, 0, 0.0, 0.0)],
            standard_world(&land, &dominant, &[]),
        )
        .unwrap();
        assert_eq!(result.assignments[0].reason, AssignmentReason::Hold);
    }

    #[test]
    fn expanding_nearest_friendly_search_matches_ascending_brute_force() {
        let land = [1_u8; 32];
        let mut dominant = [-1_i16; 32];
        for index in [0, 7, 9, 14, 18, 24, 31] {
            dominant[index] = 0;
        }
        let relations = [0_u8, 1, 1, 0];
        let world = AiWorldInput {
            grid_width: 8,
            grid_height: 4,
            grid_res: 45.0,
            land_mask: &land,
            dominant_side_map: &dominant,
            hostility: HostilityMatrix::new(Some(&relations), 2),
            frontline_latitude: None,
            frontline_longitude: None,
            objectives: &[],
        };
        let friendly_cells = dominant
            .iter()
            .enumerate()
            .filter_map(|(index, side)| (*side == 0).then_some(index))
            .collect::<Vec<_>>();
        for (id, lat, lng) in [
            (1, -90.0, -180.0),
            (2, 90.0, 180.0),
            (3, -12.5, 179.9),
            (4, 22.5, -67.5),
            (5, 0.0, 0.0),
        ] {
            let candidate = unit(id, 0, lat, lng);
            let expected = (0..land.len())
                .filter(|index| dominant[*index] == 0)
                .map(|index| {
                    let x = index % world.grid_width;
                    let y = index / world.grid_width;
                    let cell_lat = (y as f64 + 0.5) * world.grid_res - 90.0;
                    let cell_lng = (x as f64 + 0.5) * world.grid_res - 180.0;
                    let delta_lat = cell_lat - candidate.lat;
                    let delta_lng = wrapped_longitude_delta(candidate.lng, cell_lng);
                    (
                        delta_lat * delta_lat + delta_lng * delta_lng,
                        index,
                        cell_lat,
                        cell_lng,
                    )
                })
                .min_by(|left, right| {
                    left.0
                        .total_cmp(&right.0)
                        .then_with(|| left.1.cmp(&right.1))
                })
                .and_then(|(_, _, cell_lat, cell_lng)| {
                    direction_to(candidate.lat, candidate.lng, cell_lat, cell_lng)
                });
            assert_eq!(
                direction_to_nearest_friendly_cell(candidate, world, &friendly_cells),
                expected,
                "nearest friendly mismatch for ({lat}, {lng})"
            );
        }

        // More than 64 occupied cells selects the dense expanding-ring path.
        let dense_land = vec![1_u8; 200];
        let dense_dominant = (0..200)
            .map(|index| if index % 2 == 0 { 0_i16 } else { -1 })
            .collect::<Vec<_>>();
        let dense_world = AiWorldInput {
            grid_width: 20,
            grid_height: 10,
            grid_res: 18.0,
            land_mask: &dense_land,
            dominant_side_map: &dense_dominant,
            hostility: HostilityMatrix::new(Some(&relations), 2),
            frontline_latitude: None,
            frontline_longitude: None,
            objectives: &[],
        };
        let dense_cells = dense_dominant
            .iter()
            .enumerate()
            .filter_map(|(index, side)| (*side == 0).then_some(index))
            .collect::<Vec<_>>();
        let candidate = unit(6, 0, 71.25, 179.9);
        let expected_index = dense_cells
            .iter()
            .copied()
            .min_by(|left, right| {
                let distance = |index: usize| {
                    let x = index % dense_world.grid_width;
                    let y = index / dense_world.grid_width;
                    let lat = (y as f64 + 0.5) * dense_world.grid_res - 90.0;
                    let lng = (x as f64 + 0.5) * dense_world.grid_res - 180.0;
                    (lat - candidate.lat).powi(2)
                        + wrapped_longitude_delta(candidate.lng, lng).powi(2)
                };
                distance(*left)
                    .total_cmp(&distance(*right))
                    .then_with(|| left.cmp(right))
            })
            .unwrap();
        let expected_lat =
            (expected_index / dense_world.grid_width) as f64 * dense_world.grid_res - 81.0;
        let expected_lng =
            (expected_index % dense_world.grid_width) as f64 * dense_world.grid_res - 171.0;
        assert_eq!(
            direction_to_nearest_friendly_cell(candidate, dense_world, &dense_cells),
            direction_to(candidate.lat, candidate.lng, expected_lat, expected_lng)
        );
    }

    #[test]
    fn invalid_input_is_rejected_before_any_result() {
        let land = [1; 8];
        let dominant = [-1; 8];
        let duplicate = [unit(7, 0, 0.0, 0.0), unit(7, 1, 0.0, 0.1)];
        assert_eq!(
            resolve_ai_orders(
                AiOrderConfig::default(),
                &duplicate,
                standard_world(&land, &dominant, &[]),
            ),
            Err(AiOrderError::DuplicateUnit(7))
        );

        let mut nonfinite = unit(8, 0, 0.0, 0.0);
        nonfinite.combat_power = f64::NAN;
        assert_eq!(
            resolve_ai_orders(
                AiOrderConfig::default(),
                &[nonfinite],
                standard_world(&land, &dominant, &[]),
            ),
            Err(AiOrderError::InvalidUnit(8))
        );

        let out_of_range = [unit(9, 2, 0.0, 0.0)];
        assert_eq!(
            resolve_ai_orders(
                AiOrderConfig::default(),
                &out_of_range,
                standard_world(&land, &dominant, &[]),
            ),
            Err(AiOrderError::InvalidUnit(9))
        );
    }
}
