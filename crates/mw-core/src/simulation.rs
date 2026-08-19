//! Deterministic orchestration for one native unit-simulation tick.
//!
//! Strategic planning remains outside this module. Callers resolve movement
//! headings, scalar speed factors, preferred targets, and combat modifiers,
//! then submit those immutable orders here. A tick takes one tactical snapshot,
//! executes units in reverse stable-storage order, commits combat immediately,
//! defers removal until the end, and publishes an ID-sorted immutable snapshot.

use std::{collections::BTreeMap, sync::Arc};

use thiserror::Error;

use crate::{
    combat::{
        CombatConfig, CombatContext, CombatError, CombatEvent, CombatUnit, UnitKind,
        formation_strength, resolve_direct_engagement, resolve_proximity_contact,
        wrapped_longitude_delta,
    },
    direction::HostilityMatrix,
    movement::{MovementError, MovementFactors, MovementInput, MovementState, integrate_unit_step},
    tactical::{
        NeighborOptions, SideKey, TacticalGrid, TacticalGridError, TacticalUnit,
        tactical_cell_coords,
    },
    world::{WorldGridError, WorldGridView},
};

pub const NATIVE_TICK_SCHEMA_VERSION: &str = "native-tick-v1";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedCombatOrder {
    pub dealt_multiplier: f64,
    pub taken_multiplier: f64,
    pub defense_bonus: f64,
    pub long_war_defense: f64,
    pub mountain: bool,
    pub urban: bool,
}

impl Default for ResolvedCombatOrder {
    fn default() -> Self {
        Self {
            dealt_multiplier: 1.0,
            taken_multiplier: 1.0,
            defense_bonus: 1.0,
            long_war_defense: 1.0,
            mountain: false,
            urban: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedUnitOrder {
    pub unit_id: u64,
    pub preferred_target_id: Option<u64>,
    pub movement_enabled: bool,
    pub dir_lat: f64,
    pub dir_lng: f64,
    pub factors: MovementFactors,
    pub combat: ResolvedCombatOrder,
}

impl ResolvedUnitOrder {
    pub fn hold(unit_id: u64) -> Self {
        Self {
            unit_id,
            preferred_target_id: None,
            movement_enabled: false,
            dir_lat: 0.0,
            dir_lng: 0.0,
            factors: MovementFactors {
                base_speed: 0.0,
                speed_mult: 1.0,
                plan_speed_mult: 1.0,
                neutral_penalty: 1.0,
                retreat_boost: 1.0,
                push_readiness: 1.0,
            },
            combat: ResolvedCombatOrder::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SimulationUnit {
    pub combat: CombatUnit,
    pub dir_lat: f64,
    pub dir_lng: f64,
    pub coast_stuck_ticks: u32,
    pub armor_landing_penalty_until_tick: u64,
    pub is_support: bool,
    pub ally_weight: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimulationConfig {
    pub tactical_cell_size: f64,
    pub combat: CombatConfig,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            tactical_cell_size: 0.6,
            combat: CombatConfig::default(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TickInput<'a> {
    pub tick: u64,
    pub frame: u64,
    pub war_grace_end: u64,
    pub world: WorldGridView<'a>,
    pub hostility: HostilityMatrix<'a>,
    pub orders: &'a [ResolvedUnitOrder],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitSnapshot {
    pub id: u64,
    pub side: SideKey,
    pub sovereign: u64,
    pub kind: UnitKind,
    pub lat: f64,
    pub lng: f64,
    pub health: f64,
    pub max_health: f64,
    pub health_fraction: f32,
    pub personnel: u64,
    pub equipment: u64,
    pub dir_lat: f64,
    pub dir_lng: f64,
    pub coast_stuck_ticks: u32,
    pub last_combat_tick: u64,
    pub victory_boost_ticks: u64,
    pub landing_penalty_active: bool,
    pub transport: bool,
    pub at_sea: bool,
}

/// Immutable data that may safely outlive further simulation ticks.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameSnapshot {
    pub schema_version: &'static str,
    pub tick: u64,
    pub frame: u64,
    pub units: Arc<[UnitSnapshot]>,
    pub events: Arc<[CombatEvent]>,
    pub removed_ids: Arc<[u64]>,
    pub abandoned_ids: Arc<[u64]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickCounters {
    pub input_units: usize,
    pub tactical_cells: usize,
    pub candidate_contacts: usize,
    pub accepted_contacts: usize,
    pub proximity_events: usize,
    pub direct_events: usize,
    pub moved_units: usize,
    pub held_units: usize,
    pub removed_units: usize,
    pub abandoned_orders: usize,
}

#[derive(Debug, Error, PartialEq)]
pub enum SimulationError {
    #[error("invalid simulation config")]
    InvalidConfig,
    #[error("duplicate unit id {0}")]
    DuplicateUnit(u64),
    #[error("duplicate order for unit id {0}")]
    DuplicateOrder(u64),
    #[error("invalid unit or order numeric data")]
    NonFiniteInput,
    #[error("invalid hostility matrix")]
    InvalidHostility,
    #[error("invalid side {0}")]
    InvalidSide(u64),
    #[error("world: {0}")]
    World(#[from] WorldGridError),
    #[error("movement: {0}")]
    Movement(#[from] MovementError),
    #[error("combat: {0}")]
    Combat(#[from] CombatError),
    #[error("tactical: {0}")]
    Tactical(#[from] TacticalGridError),
}

pub struct Simulation {
    config: SimulationConfig,
    pub units: Vec<SimulationUnit>,
    grid: TacticalGrid,
    tactical_units: Vec<TacticalUnit>,
    candidates: Vec<usize>,
    accepted: Vec<usize>,
    order_by_unit: Vec<Option<ResolvedUnitOrder>>,
    unit_index_by_id: BTreeMap<u64, usize>,
    events: Vec<CombatEvent>,
    removed: Vec<u64>,
    abandoned: Vec<u64>,
    latest: Option<FrameSnapshot>,
}

impl Simulation {
    pub fn new(
        config: SimulationConfig,
        units: Vec<SimulationUnit>,
    ) -> Result<Self, SimulationError> {
        validate_config(config)?;
        let grid = TacticalGrid::new(config.tactical_cell_size)?;
        let mut simulation = Self {
            config,
            units,
            grid,
            tactical_units: Vec::new(),
            candidates: Vec::new(),
            accepted: Vec::new(),
            order_by_unit: Vec::new(),
            unit_index_by_id: BTreeMap::new(),
            events: Vec::new(),
            removed: Vec::new(),
            abandoned: Vec::new(),
            latest: None,
        };
        simulation.rebuild_unit_index()?;
        simulation.validate_units(None)?;
        Ok(simulation)
    }

    /// Publish current unit state without attributing prior tick artifacts.
    /// This is intended for renderer initialization before the first step.
    pub fn initial_snapshot(&self, tick: u64, frame: u64) -> FrameSnapshot {
        let mut snapshot = self.make_snapshot(tick, frame);
        snapshot.events = Arc::from([]);
        snapshot.removed_ids = Arc::from([]);
        snapshot.abandoned_ids = Arc::from([]);
        snapshot
    }

    pub const fn config(&self) -> SimulationConfig {
        self.config
    }

    pub fn latest_snapshot(&self) -> Option<&FrameSnapshot> {
        self.latest.as_ref()
    }

    pub fn step(
        &mut self,
        input: TickInput<'_>,
    ) -> Result<(FrameSnapshot, TickCounters), SimulationError> {
        // Validate the complete boundary before mutating simulation state.
        input.world.validate()?;
        validate_hostility(input.hostility)?;
        self.rebuild_unit_index()?;
        self.validate_units(Some(input.hostility.max_sides))?;
        self.prepare_orders(input.orders)?;

        self.events.clear();
        self.removed.clear();
        self.abandoned.clear();
        for unit in &mut self.units {
            unit.combat.landing_penalty_active = unit.armor_landing_penalty_until_tick > input.tick;
        }
        self.rebuild_tactical_snapshot()?;

        let mut counters = TickCounters {
            input_units: self.units.len(),
            tactical_cells: self.grid.counters.cell_count,
            ..TickCounters::default()
        };
        let radius_cells =
            (self.config.combat.proximity_radius / self.grid.cell_size).ceil() as usize;
        let proximity_radius_sq =
            self.config.combat.proximity_radius * self.config.combat.proximity_radius;

        // This preserves the browser simulation's reverse stable-array loop.
        for attacker_idx in (0..self.units.len()).rev() {
            if !eligible_at_loop_start(&self.units[attacker_idx].combat) {
                continue;
            }

            let order = self.order_by_unit[attacker_idx]
                .unwrap_or_else(|| ResolvedUnitOrder::hold(self.units[attacker_idx].combat.id));
            let attacker_side = self.units[attacker_idx].combat.side as SideKey;
            let origin = tactical_cell_coords(
                self.units[attacker_idx].combat.lat,
                self.units[attacker_idx].combat.lng,
                self.grid.cell_size,
            )?
            .ok_or(SimulationError::NonFiniteInput)?;

            self.candidates.clear();
            for &target_side_key in self.grid.by_side.keys() {
                let target_side = usize::from(target_side_key);
                if target_side == usize::from(attacker_side)
                    || !is_hostile(input.hostility, usize::from(attacker_side), target_side)
                {
                    continue;
                }
                self.grid.for_each_neighbor_cell(
                    target_side_key,
                    origin,
                    NeighborOptions { radius_cells },
                    |cell| self.candidates.extend(cell.units.iter().copied()),
                );
            }
            // The JS orchestration canonicalizes tactical candidates by stable ID.
            self.candidates
                .sort_unstable_by_key(|&index| self.units[index].combat.id);

            self.accepted.clear();
            for target_idx in self.candidates.iter().copied() {
                counters.candidate_contacts += 1;
                if target_idx == attacker_idx
                    || !is_hostile(
                        input.hostility,
                        usize::from(attacker_side),
                        self.tactical_units[target_idx]
                            .side
                            .map_or(usize::MAX, usize::from),
                    )
                {
                    continue;
                }
                // The actor is live state; the target position comes from the
                // immutable start-of-tick tactical snapshot.
                let d_lat =
                    self.tactical_units[target_idx].lat - self.units[attacker_idx].combat.lat;
                let d_lng = wrapped_longitude_delta(
                    self.units[attacker_idx].combat.lng,
                    self.tactical_units[target_idx].lng,
                );
                if d_lat * d_lat + d_lng * d_lng < proximity_radius_sq {
                    counters.accepted_contacts += 1;
                    self.accepted.push(target_idx);
                }
            }

            // Target selection happens before proximity mutations. A preferred
            // accepted target wins; otherwise the first stable-ID contact wins.
            let selected_target = order
                .preferred_target_id
                .and_then(|id| {
                    self.accepted
                        .iter()
                        .copied()
                        .find(|&index| self.units[index].combat.id == id)
                })
                .or_else(|| self.accepted.first().copied());

            // Keep the reusable accepted buffer while mutating unit state.
            for position in 0..self.accepted.len() {
                let target_idx = self.accepted[position];
                // Mirrors `b && b.health > 0` in the reference orchestration.
                if self.units[target_idx].combat.health <= 0.0 {
                    continue;
                }
                let context = combat_context(input, order.combat);
                if let Some(event) = self.resolve_pair(attacker_idx, target_idx, false, &context)? {
                    counters.proximity_events += 1;
                    self.events.push(event);
                }
            }

            let mut direct_engaged = false;
            if let Some(target_idx) = selected_target
                && self.units[target_idx].combat.health > 0.0
            {
                let context = combat_context(input, order.combat);
                if let Some(event) = self.resolve_pair(attacker_idx, target_idx, true, &context)? {
                    counters.direct_events += 1;
                    self.events.push(event);
                    direct_engaged = true;
                }
            }

            if !direct_engaged && order.movement_enabled {
                let unit = &mut self.units[attacker_idx];
                let output = integrate_unit_step(
                    input.world,
                    MovementInput {
                        state: MovementState {
                            lat: unit.combat.lat,
                            lng: unit.combat.lng,
                            coast_stuck_ticks: unit.coast_stuck_ticks,
                        },
                        dir_lat: order.dir_lat,
                        dir_lng: order.dir_lng,
                        factors: order.factors,
                        is_transport: unit.combat.transport,
                        is_at_sea: unit.combat.at_sea,
                    },
                )?;
                unit.combat.lat = output.state.lat;
                unit.combat.lng = output.state.lng;
                unit.dir_lat = output.applied_dir_lat;
                unit.dir_lng = output.applied_dir_lng;
                unit.coast_stuck_ticks = output.state.coast_stuck_ticks;
                counters.moved_units += 1;
                if output.abandon_target {
                    self.abandoned.push(unit.combat.id);
                    counters.abandoned_orders += 1;
                }
            } else {
                counters.held_units += 1;
            }
        }

        self.units.retain(|unit| {
            if defeated(&unit.combat) {
                self.removed.push(unit.combat.id);
                false
            } else {
                true
            }
        });
        self.removed.sort_unstable();
        self.abandoned.sort_unstable();
        counters.removed_units = self.removed.len();

        let snapshot = self.make_snapshot(input.tick, input.frame);
        self.latest = Some(snapshot.clone());
        Ok((snapshot, counters))
    }

    fn rebuild_unit_index(&mut self) -> Result<(), SimulationError> {
        self.unit_index_by_id.clear();
        for (index, unit) in self.units.iter().enumerate() {
            if self
                .unit_index_by_id
                .insert(unit.combat.id, index)
                .is_some()
            {
                return Err(SimulationError::DuplicateUnit(unit.combat.id));
            }
        }
        Ok(())
    }

    fn prepare_orders(&mut self, orders: &[ResolvedUnitOrder]) -> Result<(), SimulationError> {
        self.order_by_unit.clear();
        self.order_by_unit.resize(self.units.len(), None);
        for &order in orders {
            validate_order(order)?;
            let Some(&index) = self.unit_index_by_id.get(&order.unit_id) else {
                // Orders are immutable per scenario step. Once a unit has been
                // removed, its stale order is simply no longer consumed.
                continue;
            };
            if self.order_by_unit[index].replace(order).is_some() {
                return Err(SimulationError::DuplicateOrder(order.unit_id));
            }
        }
        Ok(())
    }

    fn validate_units(&self, max_sides: Option<usize>) -> Result<(), SimulationError> {
        for unit in &self.units {
            let combat = &unit.combat;
            if combat.side > u64::from(SideKey::MAX)
                || max_sides.is_some_and(|count| combat.side as usize >= count)
            {
                return Err(SimulationError::InvalidSide(combat.side));
            }
            if ![
                combat.lat,
                combat.lng,
                combat.health,
                combat.max_health,
                combat.quality,
                unit.dir_lat,
                unit.dir_lng,
                unit.ally_weight,
            ]
            .into_iter()
            .all(f64::is_finite)
                || combat.max_health <= 0.0
                || unit.ally_weight < 0.0
            {
                return Err(SimulationError::NonFiniteInput);
            }
        }
        Ok(())
    }

    fn rebuild_tactical_snapshot(&mut self) -> Result<(), SimulationError> {
        self.tactical_units.clear();
        self.tactical_units
            .extend(self.units.iter().map(|unit| {
                TacticalUnit {
                    id: unit.combat.id,
                    side: eligible_for_tactical_snapshot(&unit.combat)
                        .then_some(unit.combat.side as SideKey),
                    lat: unit.combat.lat,
                    lng: unit.combat.lng,
                    strength: formation_strength(&unit.combat),
                    ally_weight: unit.ally_weight,
                    is_armor: unit.combat.kind == UnitKind::Armor,
                    is_support: unit.is_support,
                }
            }));
        self.grid.rebuild(&self.tactical_units)?;
        Ok(())
    }

    fn resolve_pair(
        &mut self,
        attacker_idx: usize,
        target_idx: usize,
        direct: bool,
        context: &CombatContext<'_>,
    ) -> Result<Option<CombatEvent>, SimulationError> {
        // Combat kernels own pair mutation. Copying two records keeps this
        // adapter O(1) while avoiding a full-unit-vector clone per contact.
        let mut pair = [
            self.units[attacker_idx].combat.clone(),
            self.units[target_idx].combat.clone(),
        ];
        let event = if direct {
            resolve_direct_engagement(&mut pair, 0, 1, context, &self.config.combat)?
        } else {
            resolve_proximity_contact(&mut pair, 0, 1, context, &self.config.combat)?
        };
        self.units[attacker_idx].combat = pair[0].clone();
        self.units[target_idx].combat = pair[1].clone();
        Ok(event)
    }

    fn make_snapshot(&self, tick: u64, frame: u64) -> FrameSnapshot {
        let mut units = self
            .units
            .iter()
            .map(|unit| UnitSnapshot {
                id: unit.combat.id,
                side: unit.combat.side as SideKey,
                sovereign: unit.combat.sovereign,
                kind: unit.combat.kind,
                lat: unit.combat.lat,
                lng: unit.combat.lng,
                health: unit.combat.health,
                max_health: unit.combat.max_health,
                health_fraction: (unit.combat.health / unit.combat.max_health).clamp(0.0, 1.0)
                    as f32,
                personnel: unit.combat.personnel,
                equipment: unit.combat.equipment,
                dir_lat: unit.dir_lat,
                dir_lng: unit.dir_lng,
                coast_stuck_ticks: unit.coast_stuck_ticks,
                last_combat_tick: unit.combat.last_combat_tick,
                victory_boost_ticks: unit.combat.victory_boost_ticks,
                landing_penalty_active: unit.armor_landing_penalty_until_tick > tick,
                transport: unit.combat.transport,
                at_sea: unit.combat.at_sea,
            })
            .collect::<Vec<_>>();
        units.sort_unstable_by_key(|unit| unit.id);
        FrameSnapshot {
            schema_version: NATIVE_TICK_SCHEMA_VERSION,
            tick,
            frame,
            units: units.into(),
            events: self.events.clone().into(),
            removed_ids: self.removed.clone().into(),
            abandoned_ids: self.abandoned.clone().into(),
        }
    }
}

fn validate_config(config: SimulationConfig) -> Result<(), SimulationError> {
    let combat = config.combat;
    if ![
        config.tactical_cell_size,
        combat.combat_damage,
        combat.proximity_radius,
        combat.direct_radius,
        combat.target_jitter_scale,
        combat.unit_speed,
        combat.unit_naval_speed,
    ]
    .into_iter()
    .all(f64::is_finite)
        || config.tactical_cell_size <= 0.0
        || config.tactical_cell_size > 180.0
        || combat.combat_damage < 0.0
        || combat.proximity_radius <= 0.0
        || combat.direct_radius <= 0.0
        || combat.target_jitter_scale < 0.0
        || combat.unit_speed <= 0.0
        || combat.unit_naval_speed <= 0.0
    {
        return Err(SimulationError::InvalidConfig);
    }
    Ok(())
}

fn validate_hostility(hostility: HostilityMatrix<'_>) -> Result<(), SimulationError> {
    let Some(expected) = hostility.max_sides.checked_mul(hostility.max_sides) else {
        return Err(SimulationError::InvalidHostility);
    };
    if hostility.max_sides == 0
        || hostility.max_sides > usize::from(SideKey::MAX) + 1
        || hostility
            .relations
            .is_some_and(|relations| relations.len() != expected)
    {
        return Err(SimulationError::InvalidHostility);
    }
    Ok(())
}

fn validate_order(order: ResolvedUnitOrder) -> Result<(), SimulationError> {
    if ![
        order.dir_lat,
        order.dir_lng,
        order.factors.base_speed,
        order.factors.speed_mult,
        order.factors.plan_speed_mult,
        order.factors.neutral_penalty,
        order.factors.retreat_boost,
        order.factors.push_readiness,
        order.combat.dealt_multiplier,
        order.combat.taken_multiplier,
        order.combat.defense_bonus,
        order.combat.long_war_defense,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err(SimulationError::NonFiniteInput);
    }
    if order.movement_enabled {
        let move_distance = order.factors.base_speed
            * order.factors.speed_mult
            * order.factors.plan_speed_mult
            * order.factors.neutral_penalty
            * order.factors.retreat_boost
            * order.factors.push_readiness
            * 0.8;
        if !move_distance.is_finite()
            || !(order.dir_lat * move_distance).is_finite()
            || !(order.dir_lng * move_distance).is_finite()
        {
            return Err(SimulationError::NonFiniteInput);
        }
    }
    Ok(())
}

fn is_hostile(hostility: HostilityMatrix<'_>, attacker: usize, target: usize) -> bool {
    attacker != target
        && attacker < hostility.max_sides
        && target < hostility.max_sides
        && hostility
            .relations
            .is_none_or(|relations| relations[attacker * hostility.max_sides + target] == 1)
}

fn combat_context<'a>(input: TickInput<'a>, order: ResolvedCombatOrder) -> CombatContext<'a> {
    CombatContext {
        sim_tick: input.tick,
        frame: input.frame,
        war_grace_end: input.war_grace_end,
        attacker_damage_dealt_multiplier: order.dealt_multiplier,
        attacker_damage_taken_multiplier: order.taken_multiplier,
        defense_bonus: order.defense_bonus,
        long_war_defense: order.long_war_defense,
        mountain: order.mountain,
        urban: order.urban,
        world: Some(input.world),
    }
}

fn eligible_for_tactical_snapshot(unit: &CombatUnit) -> bool {
    unit.health > 0.0 && (unit.kind == UnitKind::Armor || unit.personnel > 0)
}

fn eligible_at_loop_start(unit: &CombatUnit) -> bool {
    unit.health > 0.0 && (unit.kind == UnitKind::Armor || unit.personnel > 0)
}

fn defeated(unit: &CombatUnit) -> bool {
    unit.health <= 0.0
        || (unit.kind == UnitKind::Army && unit.personnel == 0)
        || (unit.kind == UnitKind::Armor && unit.equipment == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn army(id: u64, side: u64, lat: f64, lng: f64) -> SimulationUnit {
        SimulationUnit {
            combat: CombatUnit {
                id,
                side,
                sovereign: side + 10,
                kind: UnitKind::Army,
                lat,
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

    fn all_land() -> Vec<u8> {
        vec![1; 360 * 180]
    }

    fn world(mask: &[u8]) -> WorldGridView<'_> {
        WorldGridView::new(1.0, 360, 180, mask).unwrap()
    }

    fn move_order(id: u64) -> ResolvedUnitOrder {
        ResolvedUnitOrder {
            unit_id: id,
            preferred_target_id: None,
            movement_enabled: true,
            dir_lat: 0.0,
            dir_lng: 1.0,
            factors: MovementFactors {
                base_speed: 1.25,
                speed_mult: 1.0,
                plan_speed_mult: 1.0,
                neutral_penalty: 1.0,
                retreat_boost: 1.0,
                push_readiness: 1.0,
            },
            combat: ResolvedCombatOrder::default(),
        }
    }

    fn input<'a>(
        mask: &'a [u8],
        orders: &'a [ResolvedUnitOrder],
        relations: Option<&'a [u8]>,
    ) -> TickInput<'a> {
        TickInput {
            tick: 10,
            frame: 10,
            war_grace_end: 0,
            world: world(mask),
            hostility: HostilityMatrix {
                relations,
                max_sides: 2,
            },
            orders,
        }
    }

    #[test]
    fn resolved_movement_publishes_an_owned_sorted_snapshot() {
        let mask = all_land();
        let mut simulation = Simulation::new(
            SimulationConfig::default(),
            vec![army(9, 0, 0.0, 0.0), army(2, 0, 4.0, 0.0)],
        )
        .unwrap();
        let orders = [move_order(9)];
        let (snapshot, counters) = simulation.step(input(&mask, &orders, None)).unwrap();
        assert_eq!(counters.moved_units, 1);
        assert_eq!(
            snapshot
                .units
                .iter()
                .map(|unit| unit.id)
                .collect::<Vec<_>>(),
            [2, 9]
        );
        assert_eq!(snapshot.units[1].lng, 1.0);
        let retained = snapshot.clone();
        simulation.units[0].combat.lng = 20.0;
        assert_eq!(retained.units[1].lng, 1.0);
    }

    #[test]
    fn proximity_contact_does_not_suppress_movement_when_direct_gate_misses() {
        let mask = all_land();
        let mut simulation = Simulation::new(
            SimulationConfig::default(),
            vec![army(1, 0, 0.0, 0.0), army(2, 1, 0.1, 0.0)],
        )
        .unwrap();
        let orders = [move_order(1)];
        let (snapshot, counters) = simulation.step(input(&mask, &orders, None)).unwrap();
        assert!(counters.proximity_events >= 1);
        assert_eq!(counters.moved_units, 1);
        assert!(snapshot.units.iter().find(|unit| unit.id == 1).unwrap().lng > 0.0);
    }

    #[test]
    fn directed_hostility_can_disable_one_direction() {
        let mask = all_land();
        let relations = [0, 1, 0, 0];
        let mut simulation = Simulation::new(
            SimulationConfig::default(),
            vec![army(1, 0, 0.0, 0.0), army(2, 1, 0.1, 0.0)],
        )
        .unwrap();
        let (snapshot, counters) = simulation
            .step(input(&mask, &[], Some(&relations)))
            .unwrap();
        assert_eq!(counters.proximity_events, 1);
        assert_eq!(snapshot.events[0].attacker_id, 1);
    }

    #[test]
    fn all_contacts_are_applied_before_preferred_direct_target() {
        let mask = all_land();
        let mut config = SimulationConfig::default();
        config.combat.direct_radius = 1.0;
        config.combat.target_jitter_scale = 0.0;
        let mut simulation = Simulation::new(
            config,
            vec![
                army(10, 0, 0.0, 0.0),
                army(30, 1, 0.1, 0.0),
                army(20, 1, 0.2, 0.0),
            ],
        )
        .unwrap();
        let mut order = ResolvedUnitOrder::hold(10);
        order.preferred_target_id = Some(30);
        let (snapshot, _) = simulation.step(input(&mask, &[order], None)).unwrap();
        let attacks = snapshot
            .events
            .iter()
            .filter(|event| event.attacker_id == 10)
            .collect::<Vec<_>>();
        assert_eq!(attacks[0].target_id, 20);
        assert_eq!(attacks[1].target_id, 30);
        assert_eq!(attacks[2].target_id, 30);
    }

    #[test]
    fn cleanup_is_deferred_and_removed_ids_are_sorted() {
        let mask = all_land();
        let mut a = army(9, 0, 0.0, 0.0);
        let mut b = army(2, 1, 0.1, 0.0);
        a.combat.health = 0.0;
        b.combat.personnel = 0;
        let mut simulation = Simulation::new(SimulationConfig::default(), vec![a, b]).unwrap();
        let (snapshot, counters) = simulation.step(input(&mask, &[], None)).unwrap();
        assert_eq!(&*snapshot.removed_ids, &[2, 9]);
        assert!(snapshot.units.is_empty());
        assert_eq!(counters.removed_units, 2);
    }

    #[test]
    fn invalid_orders_fail_before_any_state_mutation() {
        let mask = all_land();
        let mut simulation =
            Simulation::new(SimulationConfig::default(), vec![army(1, 0, 0.0, 0.0)]).unwrap();
        let before = simulation.units.clone();
        let mut bad = move_order(1);
        bad.dir_lng = f64::NAN;
        assert_eq!(
            simulation.step(input(&mask, &[bad], None)),
            Err(SimulationError::NonFiniteInput)
        );
        assert_eq!(simulation.units, before);
    }

    #[test]
    fn overflow_in_a_later_reverse_loop_order_is_rejected_atomically() {
        let mask = all_land();
        let mut simulation = Simulation::new(
            SimulationConfig::default(),
            vec![army(1, 0, 8.0, 0.0), army(2, 0, 0.0, 0.0)],
        )
        .unwrap();
        let before = simulation.units.clone();
        let mut bad = move_order(1);
        bad.factors.base_speed = 1e308;
        bad.factors.speed_mult = 1e308;
        let orders = [bad, move_order(2)];
        assert_eq!(
            simulation.step(input(&mask, &orders, None)),
            Err(SimulationError::NonFiniteInput)
        );
        assert_eq!(simulation.units, before);
    }

    #[test]
    fn stale_orders_and_removed_preferred_targets_are_ignored_on_later_steps() {
        let mask = all_land();
        let mut target = army(2, 1, 0.0, 0.0);
        target.combat.health = 0.1;
        target.combat.personnel = 1;
        let mut simulation = Simulation::new(
            SimulationConfig::default(),
            vec![army(1, 0, 0.0, 0.0), target],
        )
        .unwrap();
        let mut attacker = ResolvedUnitOrder::hold(1);
        attacker.preferred_target_id = Some(2);
        let orders = [attacker, ResolvedUnitOrder::hold(2)];
        let (first, _) = simulation.step(input(&mask, &orders, None)).unwrap();
        assert_eq!(&*first.removed_ids, &[2]);

        let second_input = TickInput {
            tick: 11,
            frame: 11,
            ..input(&mask, &orders, None)
        };
        let (second, _) = simulation.step(second_input).unwrap();
        assert_eq!(second.units.len(), 1);
        assert_eq!(second.units[0].id, 1);
    }

    #[test]
    fn armor_landing_penalty_expires_from_logical_tick() {
        let mask = all_land();
        let mut armor = army(1, 0, 0.0, 0.0);
        armor.combat.kind = UnitKind::Armor;
        armor.combat.personnel = 0;
        armor.combat.personnel_capacity = 0;
        armor.combat.equipment = 100;
        armor.combat.max_equipment = 100;
        armor.armor_landing_penalty_until_tick = 11;
        let mut simulation = Simulation::new(
            SimulationConfig::default(),
            vec![armor, army(2, 1, 0.1, 0.0)],
        )
        .unwrap();
        let orders = [ResolvedUnitOrder::hold(1), ResolvedUnitOrder::hold(2)];
        let (first, _) = simulation.step(input(&mask, &orders, None)).unwrap();
        let penalized = first
            .events
            .iter()
            .find(|event| {
                event.layer == crate::combat::CombatLayer::Proximity && event.attacker_id == 1
            })
            .unwrap()
            .target_damage;
        assert!(
            first
                .units
                .iter()
                .find(|unit| unit.id == 1)
                .unwrap()
                .landing_penalty_active
        );

        let second_input = TickInput {
            tick: 11,
            frame: 11,
            ..input(&mask, &orders, None)
        };
        let (second, _) = simulation.step(second_input).unwrap();
        let unpenalized = second
            .events
            .iter()
            .find(|event| {
                event.layer == crate::combat::CombatLayer::Proximity && event.attacker_id == 1
            })
            .unwrap()
            .target_damage;
        assert!(unpenalized > penalized * 3.0);
        assert!(
            !second
                .units
                .iter()
                .find(|unit| unit.id == 1)
                .unwrap()
                .landing_penalty_active
        );
    }
}
