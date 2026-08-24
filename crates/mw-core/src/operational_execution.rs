//! Deterministic execution kernels for naval movement and defender reactions.
//!
//! The state in this module is deliberately renderer- and terrain-independent.
//! The runtime observes whether units are at sea and supplies stable threat
//! records; this kernel owns continuation, phase changes, assignments, and
//! movement steering. It never attempts to write `at_sea` itself.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

pub const OPERATIONAL_EXECUTION_SCHEMA_VERSION: &str = "native-operational-execution-v1";

const NAVAL_STALL_TICKS: u64 = 600;
const LANDING_OPERATION_TICKS: u64 = 900;
const SUPPLY_DELIVERED_TICKS: u64 = 600;
const REACTION_STALL_TICKS: u64 = 1_800;
const LANDING_DEFEATED_TICKS: u64 = 600;
const GATHER_DISTANCE_SQUARED: f64 = 0.5;
const ROUTE_ARRIVAL_DISTANCE_SQUARED: f64 = 2.0;
const LANDING_DISTANCE_SQUARED: f64 = 2.0;
const FAST_TRANSPORT_ARRIVAL_DISTANCE_SQUARED: f64 = 2.25;
const REACTION_ARRIVAL_DISTANCE_SQUARED: f64 = 1.0;
const REACTION_RECRUIT_MIN_DISTANCE_SQUARED: f64 = 9.0;
const REACTION_RECRUIT_MAX_DISTANCE_SQUARED: f64 = 100.0;
const PROGRESS_EPSILON: f64 = 1.0e-9;

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Point {
    pub lat: f64,
    pub lng: f64,
}

impl Point {
    pub fn validate(self) -> bool {
        self.lat.is_finite()
            && self.lng.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lng)
    }

    pub fn distance_squared(self, other: Self) -> f64 {
        let d_lat = other.lat - self.lat;
        let d_lng = wrapped_longitude_delta(self.lng, other.lng);
        d_lat * d_lat + d_lng * d_lng
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NavalOperationKind {
    Invasion,
    Supply,
    FastTransport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NavalOperationPhase {
    Gathering,
    Embarkation,
    Transit,
    Landing,
    Delivered,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DefenderReactionKind {
    NavalTransit,
    Landing,
    LandOffensive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DefenderThreatKind {
    NavalInvasion,
    LandOffensive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefenderThreatPhase {
    Transit,
    Landing,
    Execution,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavalMember {
    pub unit_id: u64,
    pub role: String,
    pub assigned_tick: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavalOperation {
    pub id: String,
    pub signature: String,
    pub kind: NavalOperationKind,
    pub phase: NavalOperationPhase,
    pub side: usize,
    pub country: u16,
    #[serde(deserialize_with = "required_option")]
    pub enemy_side: Option<usize>,
    pub max_assigned_units: usize,
    pub members: Vec<NavalMember>,
    pub staging: Point,
    pub target: Point,
    pub route: Vec<Point>,
    pub route_index: usize,
    pub progress: f64,
    pub started_tick: u64,
    pub phase_started_tick: u64,
    pub last_progress_tick: u64,
    #[serde(deserialize_with = "required_option")]
    pub completion_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefenderReaction {
    pub id: String,
    pub sequence: u64,
    pub threat_signature: String,
    pub side: usize,
    pub enemy_side: usize,
    pub kind: DefenderReactionKind,
    pub target: Point,
    pub unit_ids: Vec<u64>,
    pub max_units: usize,
    pub started_tick: u64,
    pub last_progress_tick: u64,
    #[serde(deserialize_with = "required_option")]
    pub best_distance_squared: Option<f64>,
    #[serde(deserialize_with = "required_option")]
    pub landing_defeated_tick: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalExecutionState {
    pub schema: String,
    pub naval_operations: Vec<NavalOperation>,
    pub defender_reactions: Vec<DefenderReaction>,
    pub next_reaction_sequence: u64,
}

impl Default for OperationalExecutionState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExecutionUnitInput {
    pub unit_id: u64,
    pub side: usize,
    pub country: u16,
    pub position: Point,
    pub transport: bool,
    pub at_sea: bool,
    pub deploying: bool,
    pub engaged: bool,
    pub operationally_assigned: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DefenderThreat {
    pub signature: String,
    pub defender_side: usize,
    pub enemy_side: usize,
    pub kind: DefenderThreatKind,
    pub phase: DefenderThreatPhase,
    pub target: Point,
    pub enemy_force: usize,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionSteering {
    pub unit_id: u64,
    pub assignment_id: String,
    pub target: Point,
    pub dir_lat: f64,
    pub dir_lng: f64,
    pub speed_multiplier: f64,
    pub movement_enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportUpdate {
    pub unit_id: u64,
    pub transport: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavalTransition {
    pub operation_id: String,
    pub from: NavalOperationPhase,
    pub to: NavalOperationPhase,
    pub tick: u64,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefenderReactionEventKind {
    Created,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefenderReactionEvent {
    pub reaction_id: String,
    pub threat_signature: String,
    pub kind: DefenderReactionEventKind,
    pub tick: u64,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OperationalExecutionCounters {
    pub naval_operations_advanced: u64,
    pub naval_phase_transitions: u64,
    pub naval_operations_completed: u64,
    pub defender_reactions_created: u64,
    pub defender_reactions_cancelled: u64,
    pub defender_units_recruited: u64,
    pub defender_units_arrived: u64,
    pub steering_orders: u64,
    pub transport_updates: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OperationalExecutionOutcome {
    pub steering: Vec<ExecutionSteering>,
    pub transport_updates: Vec<TransportUpdate>,
    pub naval_transitions: Vec<NavalTransition>,
    pub defender_reaction_events: Vec<DefenderReactionEvent>,
    pub counters: OperationalExecutionCounters,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileCounters {
    pub naval_member_references_pruned: u64,
    pub defender_member_references_pruned: u64,
    pub naval_operations_pruned: u64,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum OperationalExecutionError {
    #[error("operational execution state is invalid: {0}")]
    InvalidState(&'static str),
    #[error("naval operation {id:?} is invalid: {reason}")]
    InvalidNavalOperation { id: String, reason: &'static str },
    #[error("defender reaction {id:?} is invalid: {reason}")]
    InvalidDefenderReaction { id: String, reason: &'static str },
    #[error("operational execution input is invalid: {0}")]
    InvalidInput(&'static str),
    #[error("defender threat {signature:?} is invalid: {reason}")]
    InvalidThreat {
        signature: String,
        reason: &'static str,
    },
    #[error("assignment {assignment:?} references missing unit {unit_id}")]
    MissingUnit { assignment: String, unit_id: u64 },
    #[error("unit {unit_id} has conflicting assignments {first:?} and {second:?}")]
    ConflictingAssignment {
        unit_id: u64,
        first: String,
        second: String,
    },
}

impl OperationalExecutionState {
    pub fn new() -> Self {
        Self {
            schema: OPERATIONAL_EXECUTION_SCHEMA_VERSION.to_owned(),
            naval_operations: Vec::new(),
            defender_reactions: Vec::new(),
            next_reaction_sequence: 1,
        }
    }

    /// Rebase persisted lifecycle timestamps while preserving their elapsed
    /// age at a checkpoint boundary. This upgrades legacy native logical-tick
    /// timers into browser-frame timers before batched playback begins.
    pub fn rebase_timer_coordinates(&mut self, source_now: u64, target_now: u64) {
        // Zero is also the browser's falsy "not yet recorded" sentinel and
        // must remain zero instead of becoming a synthetic historical time.
        let rebase = |value: u64| {
            if value == 0 {
                0
            } else {
                target_now.saturating_sub(source_now.saturating_sub(value))
            }
        };
        for operation in &mut self.naval_operations {
            operation.started_tick = rebase(operation.started_tick);
            operation.phase_started_tick = rebase(operation.phase_started_tick);
            operation.last_progress_tick = rebase(operation.last_progress_tick);
            for member in &mut operation.members {
                member.assigned_tick = rebase(member.assigned_tick);
            }
        }
        for reaction in &mut self.defender_reactions {
            reaction.started_tick = rebase(reaction.started_tick);
            reaction.last_progress_tick = rebase(reaction.last_progress_tick);
            reaction.landing_defeated_tick = reaction.landing_defeated_tick.map(rebase);
        }
    }

    pub fn snapshot(&self) -> Self {
        self.clone()
    }

    /// Validate serialized state and all live unit references.
    pub fn validate(
        &self,
        side_count: usize,
        live_units: &BTreeMap<u64, ExecutionUnitInput>,
        tick: u64,
    ) -> Result<(), OperationalExecutionError> {
        self.validate_shape(tick, side_count)?;
        validate_live_unit_map(side_count, live_units)?;

        for operation in &self.naval_operations {
            for member in &operation.members {
                let unit = live_units.get(&member.unit_id).ok_or_else(|| {
                    OperationalExecutionError::MissingUnit {
                        assignment: operation.id.clone(),
                        unit_id: member.unit_id,
                    }
                })?;
                if unit.side != operation.side {
                    return Err(OperationalExecutionError::InvalidNavalOperation {
                        id: operation.id.clone(),
                        reason: "member side",
                    });
                }
                if unit.operationally_assigned {
                    return Err(OperationalExecutionError::InvalidNavalOperation {
                        id: operation.id.clone(),
                        reason: "member operational assignment",
                    });
                }
            }
        }
        for reaction in &self.defender_reactions {
            for &unit_id in &reaction.unit_ids {
                let unit = live_units.get(&unit_id).ok_or_else(|| {
                    OperationalExecutionError::MissingUnit {
                        assignment: reaction.id.clone(),
                        unit_id,
                    }
                })?;
                if unit.side != reaction.side {
                    return Err(OperationalExecutionError::InvalidDefenderReaction {
                        id: reaction.id.clone(),
                        reason: "member side",
                    });
                }
                if unit.operationally_assigned {
                    return Err(OperationalExecutionError::InvalidDefenderReaction {
                        id: reaction.id.clone(),
                        reason: "member operational assignment",
                    });
                }
            }
        }
        Ok(())
    }

    /// Convenience validation for callers that naturally own a unit slice.
    pub fn validate_units(
        &self,
        side_count: usize,
        units: &[ExecutionUnitInput],
        tick: u64,
    ) -> Result<(), OperationalExecutionError> {
        let live_units = collect_live_units(units, Some(side_count))?;
        self.validate(side_count, &live_units, tick)
    }

    /// Validate persistent structure without requiring ephemeral unit state.
    pub fn validate_shape(
        &self,
        tick: u64,
        side_count: usize,
    ) -> Result<(), OperationalExecutionError> {
        if self.schema != OPERATIONAL_EXECUTION_SCHEMA_VERSION {
            return Err(OperationalExecutionError::InvalidState("schema"));
        }
        if side_count == 0 {
            return Err(OperationalExecutionError::InvalidState("side count"));
        }
        if self.next_reaction_sequence == 0 {
            return Err(OperationalExecutionError::InvalidState(
                "next reaction sequence",
            ));
        }
        if !self
            .naval_operations
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
        {
            return Err(OperationalExecutionError::InvalidState(
                "naval operation ordering",
            ));
        }
        if !self.defender_reactions.windows(2).all(|pair| {
            (pair[0].side, pair[0].sequence, pair[0].id.as_str())
                < (pair[1].side, pair[1].sequence, pair[1].id.as_str())
        }) {
            return Err(OperationalExecutionError::InvalidState(
                "defender reaction ordering",
            ));
        }

        let mut assignments = BTreeMap::<u64, String>::new();
        let mut signatures = BTreeSet::new();
        for operation in &self.naval_operations {
            operation.validate_shape(side_count, tick)?;
            if !signatures.insert(operation.signature.as_str()) {
                return Err(OperationalExecutionError::InvalidNavalOperation {
                    id: operation.id.clone(),
                    reason: "duplicate signature",
                });
            }
            for member in &operation.members {
                register_assignment(&mut assignments, member.unit_id, operation.id.as_str())?;
            }
        }

        let mut reaction_sides = BTreeSet::new();
        let mut max_sequence = 0;
        for reaction in &self.defender_reactions {
            reaction.validate_shape(side_count, tick)?;
            if !reaction_sides.insert(reaction.side) {
                return Err(OperationalExecutionError::InvalidDefenderReaction {
                    id: reaction.id.clone(),
                    reason: "duplicate defender side",
                });
            }
            max_sequence = max_sequence.max(reaction.sequence);
            for &unit_id in &reaction.unit_ids {
                register_assignment(&mut assignments, unit_id, reaction.id.as_str())?;
            }
        }
        if self.next_reaction_sequence <= max_sequence {
            return Err(OperationalExecutionError::InvalidState(
                "next reaction sequence is not ahead",
            ));
        }
        Ok(())
    }

    /// Prune references invalidated by committed combat or strategic changes.
    /// No phase clock, progress clock, or reaction lifetime is advanced.
    pub fn retain_live_units(
        &mut self,
        live_units: &BTreeMap<u64, ExecutionUnitInput>,
    ) -> ReconcileCounters {
        let mut counters = ReconcileCounters::default();

        self.naval_operations.retain_mut(|operation| {
            let had_members = !operation.members.is_empty();
            let before = operation.members.len();
            operation.members.retain(|member| {
                live_units
                    .get(&member.unit_id)
                    .is_some_and(|unit| unit.side == operation.side)
            });
            counters.naval_member_references_pruned += (before - operation.members.len()) as u64;
            let retain = operation.phase != NavalOperationPhase::Complete
                && (!had_members || !operation.members.is_empty());
            if !retain {
                counters.naval_operations_pruned += 1;
            }
            retain
        });

        for reaction in &mut self.defender_reactions {
            let before = reaction.unit_ids.len();
            reaction.unit_ids.retain(|unit_id| {
                live_units
                    .get(unit_id)
                    .is_some_and(|unit| unit.side == reaction.side)
            });
            counters.defender_member_references_pruned += (before - reaction.unit_ids.len()) as u64;
            if reaction.unit_ids.is_empty() {
                reaction.best_distance_squared = None;
            }
        }
        counters
    }

    /// Advance all execution state exactly once for `tick`.
    pub fn advance(
        &mut self,
        tick: u64,
        units: &[ExecutionUnitInput],
        threats: &[DefenderThreat],
    ) -> Result<OperationalExecutionOutcome, OperationalExecutionError> {
        let side_count = infer_side_count(self, units, threats);
        self.validate_shape(tick, side_count)?;
        let live_units = collect_live_units(units, Some(side_count))?;
        validate_threats(threats, side_count)?;
        self.retain_live_units(&live_units);
        self.validate(side_count, &live_units, tick)?;

        let mut outcome = OperationalExecutionOutcome::default();
        for operation in &mut self.naval_operations {
            outcome.counters.naval_operations_advanced += 1;
            advance_naval_operation(operation, tick, &live_units, &mut outcome);
        }
        self.naval_operations
            .retain(|operation| operation.phase != NavalOperationPhase::Complete);

        self.advance_defender_reactions(tick, side_count, &live_units, threats, &mut outcome)?;

        self.naval_operations.sort_by(|a, b| a.id.cmp(&b.id));
        self.defender_reactions.sort_by(|a, b| {
            (a.side, a.sequence, a.id.as_str()).cmp(&(b.side, b.sequence, b.id.as_str()))
        });
        outcome.steering.sort_by(|a, b| {
            (a.unit_id, a.assignment_id.as_str()).cmp(&(b.unit_id, b.assignment_id.as_str()))
        });
        outcome
            .transport_updates
            .sort_by_key(|update| update.unit_id);
        outcome.naval_transitions.sort_by(|a, b| {
            (a.operation_id.as_str(), a.tick).cmp(&(b.operation_id.as_str(), b.tick))
        });
        outcome.defender_reaction_events.sort_by(|a, b| {
            (a.reaction_id.as_str(), a.tick).cmp(&(b.reaction_id.as_str(), b.tick))
        });
        outcome.counters.steering_orders = outcome.steering.len() as u64;
        outcome.counters.transport_updates = outcome.transport_updates.len() as u64;

        self.validate(side_count, &live_units, tick)?;
        Ok(outcome)
    }

    fn advance_defender_reactions(
        &mut self,
        tick: u64,
        side_count: usize,
        live_units: &BTreeMap<u64, ExecutionUnitInput>,
        threats: &[DefenderThreat],
        outcome: &mut OperationalExecutionOutcome,
    ) -> Result<(), OperationalExecutionError> {
        let active_threats = threats
            .iter()
            .filter(|threat| threat.active)
            .map(|threat| ((threat.kind, threat.signature.as_str()), threat))
            .collect::<BTreeMap<_, _>>();

        let mut retained = Vec::with_capacity(self.defender_reactions.len());
        for mut reaction in std::mem::take(&mut self.defender_reactions) {
            let threat_key = (
                reaction_threat_kind(reaction.kind),
                reaction.threat_signature.as_str(),
            );
            let Some(threat) = active_threats.get(&threat_key) else {
                cancel_reaction(reaction, tick, "THREAT_ABSENT", outcome);
                continue;
            };
            if threat.defender_side != reaction.side || threat.enemy_side != reaction.enemy_side {
                cancel_reaction(reaction, tick, "THREAT_SIDE_CHANGED", outcome);
                continue;
            }
            let Some(kind) = threat.reaction_kind() else {
                cancel_reaction(reaction, tick, "THREAT_PHASE_ENDED", outcome);
                continue;
            };

            if kind != reaction.kind {
                reaction.kind = kind;
                reaction.last_progress_tick = tick;
                reaction.best_distance_squared = None;
            }
            if reaction.target.distance_squared(threat.target) > PROGRESS_EPSILON {
                reaction.target = threat.target;
                reaction.last_progress_tick = tick;
                reaction.best_distance_squared = None;
            }

            let mut arrived = 0_u64;
            reaction.unit_ids.retain(|unit_id| {
                let Some(unit) = live_units.get(unit_id) else {
                    return false;
                };
                if unit.side != reaction.side {
                    return false;
                }
                if unit.position.distance_squared(reaction.target)
                    < REACTION_ARRIVAL_DISTANCE_SQUARED
                {
                    arrived += 1;
                    false
                } else {
                    true
                }
            });
            if arrived > 0 {
                reaction.last_progress_tick = tick;
                reaction.best_distance_squared = None;
                outcome.counters.defender_units_arrived += arrived;
            }

            if kind == DefenderReactionKind::Landing {
                if threat.enemy_force < 3 {
                    let defeated_tick = *reaction.landing_defeated_tick.get_or_insert(tick);
                    if tick.saturating_sub(defeated_tick) > LANDING_DEFEATED_TICKS {
                        cancel_reaction(reaction, tick, "LANDING_DEFEATED", outcome);
                        continue;
                    }
                } else {
                    reaction.landing_defeated_tick = None;
                }
            } else {
                reaction.landing_defeated_tick = None;
            }

            update_reaction_distance_progress(&mut reaction, tick, live_units);
            if tick.saturating_sub(reaction.last_progress_tick) > REACTION_STALL_TICKS {
                cancel_reaction(reaction, tick, "NO_PROGRESS", outcome);
                continue;
            }
            retained.push(reaction);
        }
        self.defender_reactions = retained;

        let occupied_sides = self
            .defender_reactions
            .iter()
            .map(|reaction| reaction.side)
            .collect::<BTreeSet<_>>();
        let mut candidates = threats
            .iter()
            .filter(|threat| {
                threat.active
                    && !occupied_sides.contains(&threat.defender_side)
                    && threat.reaction_kind().is_some()
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| {
            let a_kind = a.reaction_kind().expect("filtered reaction kind");
            let b_kind = b.reaction_kind().expect("filtered reaction kind");
            reaction_priority(b_kind)
                .cmp(&reaction_priority(a_kind))
                .then_with(|| b.enemy_force.cmp(&a.enemy_force))
                .then_with(|| a.defender_side.cmp(&b.defender_side))
                .then_with(|| a.enemy_side.cmp(&b.enemy_side))
                .then_with(|| a.signature.cmp(&b.signature))
        });

        let own_unit_counts = count_units_by_side(side_count, live_units.values());
        let mut newly_occupied = occupied_sides;
        for threat in candidates {
            if newly_occupied.contains(&threat.defender_side) {
                continue;
            }
            let kind = threat.reaction_kind().expect("filtered reaction kind");
            let max_units = desired_reaction_units(
                kind,
                threat,
                own_unit_counts[threat.defender_side],
                live_units,
            );
            if max_units < minimum_reaction_force(kind) {
                continue;
            }
            newly_occupied.insert(threat.defender_side);
            let sequence = self.next_reaction_sequence;
            self.next_reaction_sequence = self.next_reaction_sequence.checked_add(1).ok_or(
                OperationalExecutionError::InvalidState("reaction sequence overflow"),
            )?;
            let reaction = DefenderReaction {
                id: format!("reaction-{}-{sequence}", threat.defender_side),
                sequence,
                threat_signature: threat.signature.clone(),
                side: threat.defender_side,
                enemy_side: threat.enemy_side,
                kind,
                target: threat.target,
                unit_ids: Vec::new(),
                max_units,
                started_tick: tick,
                last_progress_tick: tick,
                best_distance_squared: None,
                landing_defeated_tick: None,
            };
            outcome
                .defender_reaction_events
                .push(DefenderReactionEvent {
                    reaction_id: reaction.id.clone(),
                    threat_signature: reaction.threat_signature.clone(),
                    kind: DefenderReactionEventKind::Created,
                    tick,
                    reason: "THREAT_DETECTED",
                });
            outcome.counters.defender_reactions_created += 1;
            self.defender_reactions.push(reaction);
        }

        let naval_assignments = self
            .naval_operations
            .iter()
            .flat_map(|operation| operation.members.iter().map(|member| member.unit_id))
            .collect::<BTreeSet<_>>();
        let mut assigned_reaction_units = self
            .defender_reactions
            .iter()
            .flat_map(|reaction| reaction.unit_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        let threat_by_signature = threats
            .iter()
            .map(|threat| ((threat.kind, threat.signature.as_str()), threat))
            .collect::<BTreeMap<_, _>>();

        for reaction in &mut self.defender_reactions {
            let threat_key = (
                reaction_threat_kind(reaction.kind),
                reaction.threat_signature.as_str(),
            );
            let Some(threat) = threat_by_signature.get(&threat_key) else {
                continue;
            };
            let desired = desired_reaction_units(
                reaction.kind,
                threat,
                own_unit_counts[reaction.side],
                live_units,
            );
            reaction.max_units = reaction.max_units.max(desired);

            let slots = reaction.max_units.saturating_sub(reaction.unit_ids.len());
            if slots > 0
                && !(reaction.kind == DefenderReactionKind::Landing && threat.enemy_force < 3)
            {
                let mut recruits = live_units
                    .values()
                    .filter(|unit| {
                        unit.side == reaction.side
                            && !unit.deploying
                            && !unit.engaged
                            && !unit.transport
                            && !unit.operationally_assigned
                            && !naval_assignments.contains(&unit.unit_id)
                            && !assigned_reaction_units.contains(&unit.unit_id)
                    })
                    .filter_map(|unit| {
                        let distance = unit.position.distance_squared(reaction.target);
                        (distance > REACTION_RECRUIT_MIN_DISTANCE_SQUARED
                            && distance <= REACTION_RECRUIT_MAX_DISTANCE_SQUARED)
                            .then_some((distance, unit.unit_id))
                    })
                    .collect::<Vec<_>>();
                recruits.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
                let mut recruited_here = 0_u64;
                for (_, unit_id) in recruits.into_iter().take(slots) {
                    reaction.unit_ids.push(unit_id);
                    assigned_reaction_units.insert(unit_id);
                    recruited_here += 1;
                }
                outcome.counters.defender_units_recruited += recruited_here;
                if reaction.unit_ids.len() > reaction.max_units {
                    return Err(OperationalExecutionError::InvalidDefenderReaction {
                        id: reaction.id.clone(),
                        reason: "assignment cap",
                    });
                }
                if recruited_here > 0 {
                    reaction.last_progress_tick = tick;
                    reaction.best_distance_squared = None;
                }
                reaction.unit_ids.sort_unstable();
            }

            for &unit_id in &reaction.unit_ids {
                let Some(unit) = live_units.get(&unit_id) else {
                    continue;
                };
                if unit.engaged || unit.deploying || unit.transport || unit.operationally_assigned {
                    continue;
                }
                if let Some(steering) =
                    make_steering(unit, reaction.id.as_str(), reaction.target, 2.0)
                {
                    outcome.steering.push(steering);
                }
            }
        }
        Ok(())
    }
}

impl NavalOperation {
    fn validate_shape(
        &self,
        side_count: usize,
        tick: u64,
    ) -> Result<(), OperationalExecutionError> {
        let invalid = |reason| OperationalExecutionError::InvalidNavalOperation {
            id: self.id.clone(),
            reason,
        };
        if !valid_identifier(&self.id) {
            return Err(invalid("id"));
        }
        if !valid_identifier(&self.signature) {
            return Err(invalid("signature"));
        }
        if self.side >= side_count {
            return Err(invalid("side"));
        }
        if self
            .enemy_side
            .is_some_and(|side| side >= side_count || side == self.side)
        {
            return Err(invalid("enemy side"));
        }
        if self.max_assigned_units == 0 || self.members.len() > self.max_assigned_units {
            return Err(invalid("assignment cap"));
        }
        if !self
            .members
            .windows(2)
            .all(|pair| pair[0].unit_id < pair[1].unit_id)
        {
            return Err(invalid("member ordering"));
        }
        for member in &self.members {
            if !valid_role(&member.role) {
                return Err(invalid("member role"));
            }
            if member.assigned_tick > tick {
                return Err(invalid("member assigned tick"));
            }
        }
        if !self.staging.validate()
            || !self.target.validate()
            || self.route.iter().any(|point| !point.validate())
        {
            return Err(invalid("point"));
        }
        if self.route_index > self.route.len() {
            return Err(invalid("route index"));
        }
        if !self.progress.is_finite() || !(0.0..=1.0).contains(&self.progress) {
            return Err(invalid("progress"));
        }
        if self.started_tick > tick
            || self.phase_started_tick < self.started_tick
            || self.phase_started_tick > tick
            || self.last_progress_tick < self.started_tick
            || self.last_progress_tick > tick
        {
            return Err(invalid("tick"));
        }
        match self.kind {
            NavalOperationKind::Invasion
                if matches!(self.phase, NavalOperationPhase::Delivered) =>
            {
                return Err(invalid("invasion phase"));
            }
            NavalOperationKind::Supply if matches!(self.phase, NavalOperationPhase::Landing) => {
                return Err(invalid("supply phase"));
            }
            NavalOperationKind::FastTransport
                if !matches!(
                    self.phase,
                    NavalOperationPhase::Transit | NavalOperationPhase::Complete
                ) =>
            {
                return Err(invalid("fast transport phase"));
            }
            _ => {}
        }
        if self.phase == NavalOperationPhase::Complete {
            if self.completion_reason.as_deref().is_none_or(str::is_empty) {
                return Err(invalid("completion reason"));
            }
        } else if self.completion_reason.is_some() {
            return Err(invalid("premature completion reason"));
        }
        Ok(())
    }
}

impl DefenderReaction {
    fn validate_shape(
        &self,
        side_count: usize,
        tick: u64,
    ) -> Result<(), OperationalExecutionError> {
        let invalid = |reason| OperationalExecutionError::InvalidDefenderReaction {
            id: self.id.clone(),
            reason,
        };
        if !valid_identifier(&self.id) || !valid_identifier(&self.threat_signature) {
            return Err(invalid("id or threat signature"));
        }
        if self.sequence == 0 {
            return Err(invalid("sequence"));
        }
        if self.side >= side_count || self.enemy_side >= side_count || self.side == self.enemy_side
        {
            return Err(invalid("side"));
        }
        if !self.target.validate() {
            return Err(invalid("target"));
        }
        if self.unit_ids.len() > self.max_units
            || (self.max_units == 0 && self.kind != DefenderReactionKind::Landing)
        {
            return Err(invalid("assignment cap"));
        }
        if !self.unit_ids.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(invalid("unit ordering"));
        }
        if self.started_tick > tick
            || self.last_progress_tick < self.started_tick
            || self.last_progress_tick > tick
            || self.landing_defeated_tick.is_some_and(|value| value > tick)
        {
            return Err(invalid("tick"));
        }
        if self
            .best_distance_squared
            .is_some_and(|distance| !distance.is_finite() || distance < 0.0)
        {
            return Err(invalid("best distance"));
        }
        if self.kind != DefenderReactionKind::Landing && self.landing_defeated_tick.is_some() {
            return Err(invalid("landing defeated tick"));
        }
        Ok(())
    }
}

impl DefenderThreat {
    fn reaction_kind(&self) -> Option<DefenderReactionKind> {
        match (self.kind, self.phase) {
            (DefenderThreatKind::NavalInvasion, DefenderThreatPhase::Transit) => {
                Some(DefenderReactionKind::NavalTransit)
            }
            (DefenderThreatKind::NavalInvasion, DefenderThreatPhase::Landing) => {
                Some(DefenderReactionKind::Landing)
            }
            (DefenderThreatKind::LandOffensive, DefenderThreatPhase::Execution) => {
                Some(DefenderReactionKind::LandOffensive)
            }
            _ => None,
        }
    }
}

fn advance_naval_operation(
    operation: &mut NavalOperation,
    tick: u64,
    live_units: &BTreeMap<u64, ExecutionUnitInput>,
    outcome: &mut OperationalExecutionOutcome,
) {
    if operation.kind == NavalOperationKind::FastTransport {
        advance_fast_transport(operation, tick, live_units, outcome);
        return;
    }

    let live_members = operation
        .members
        .iter()
        .filter_map(|member| live_units.get(&member.unit_id))
        .copied()
        .collect::<Vec<_>>();
    let from = operation.phase;
    match operation.phase {
        NavalOperationPhase::Gathering => {
            let required = match operation.kind {
                NavalOperationKind::Invasion => operation.max_assigned_units.min(5),
                NavalOperationKind::Supply => operation.max_assigned_units.min(3),
                NavalOperationKind::FastTransport => unreachable!(),
            };
            let gathered = live_members
                .iter()
                .filter(|unit| {
                    unit.position.distance_squared(operation.staging) < GATHER_DISTANCE_SQUARED
                })
                .count();
            record_naval_progress(
                operation,
                0.2 * (gathered as f64 / required as f64).min(1.0),
                tick,
            );
            if gathered >= required {
                transition_naval(
                    operation,
                    NavalOperationPhase::Embarkation,
                    tick,
                    None,
                    outcome,
                );
            }
        }
        NavalOperationPhase::Embarkation => {
            let at_sea = live_members.iter().filter(|unit| unit.at_sea).count();
            let total = live_members.len();
            let share = at_sea as f64 / total.max(1) as f64;
            record_naval_progress(operation, 0.2 + share * 0.25, tick);
            if total > 0 && at_sea >= sixty_percent_ceil(total) {
                transition_naval(operation, NavalOperationPhase::Transit, tick, None, outcome);
            }
        }
        NavalOperationPhase::Transit => {
            advance_route(operation, &live_members, tick);
            let landed = live_members
                .iter()
                .filter(|unit| {
                    !unit.at_sea
                        && unit.position.distance_squared(operation.target)
                            < LANDING_DISTANCE_SQUARED
                })
                .count();
            let needed = match operation.kind {
                NavalOperationKind::Invasion => 3,
                NavalOperationKind::Supply => 2,
                NavalOperationKind::FastTransport => unreachable!(),
            };
            let route_share = operation.route_index as f64 / (operation.route.len() + 1) as f64;
            let landing_share = (landed as f64 / needed as f64).min(1.0);
            record_naval_progress(
                operation,
                0.45 + route_share * 0.15 + landing_share * 0.15,
                tick,
            );
            if landed >= needed {
                let next = if operation.kind == NavalOperationKind::Invasion {
                    NavalOperationPhase::Landing
                } else {
                    NavalOperationPhase::Delivered
                };
                transition_naval(operation, next, tick, None, outcome);
            }
        }
        NavalOperationPhase::Landing => {
            if tick.saturating_sub(operation.phase_started_tick) > LANDING_OPERATION_TICKS {
                transition_naval(
                    operation,
                    NavalOperationPhase::Complete,
                    tick,
                    Some("LANDING_ESTABLISHED"),
                    outcome,
                );
            }
        }
        NavalOperationPhase::Delivered => {
            if tick.saturating_sub(operation.phase_started_tick) > SUPPLY_DELIVERED_TICKS {
                transition_naval(
                    operation,
                    NavalOperationPhase::Complete,
                    tick,
                    Some("SUPPLY_DELIVERED"),
                    outcome,
                );
            }
        }
        NavalOperationPhase::Complete => {}
    }

    if operation.phase != NavalOperationPhase::Complete
        && !matches!(
            operation.phase,
            NavalOperationPhase::Landing | NavalOperationPhase::Delivered
        )
        && tick.saturating_sub(operation.last_progress_tick) > NAVAL_STALL_TICKS
    {
        transition_naval(
            operation,
            NavalOperationPhase::Complete,
            tick,
            Some("STALLED"),
            outcome,
        );
    }

    if from != operation.phase && operation.phase == NavalOperationPhase::Complete {
        outcome.counters.naval_operations_completed += 1;
    }
    emit_naval_outputs(operation, live_units, outcome);
}

fn advance_fast_transport(
    operation: &mut NavalOperation,
    tick: u64,
    live_units: &BTreeMap<u64, ExecutionUnitInput>,
    outcome: &mut OperationalExecutionOutcome,
) {
    let original_members = operation.members.len();
    let mut remaining = Vec::with_capacity(original_members);
    for member in std::mem::take(&mut operation.members) {
        let Some(unit) = live_units.get(&member.unit_id) else {
            continue;
        };
        if unit.position.distance_squared(operation.target)
            <= FAST_TRANSPORT_ARRIVAL_DISTANCE_SQUARED
        {
            if unit.transport {
                outcome.transport_updates.push(TransportUpdate {
                    unit_id: unit.unit_id,
                    transport: false,
                });
            }
        } else {
            remaining.push(member);
        }
    }
    let arrived = original_members.saturating_sub(remaining.len());
    operation.members = remaining;
    if arrived > 0 {
        let increment = arrived as f64 / operation.max_assigned_units.max(1) as f64;
        record_naval_progress(operation, (operation.progress + increment).min(0.99), tick);
    }

    if operation.members.is_empty() {
        transition_naval(
            operation,
            NavalOperationPhase::Complete,
            tick,
            Some("ARRIVED"),
            outcome,
        );
        outcome.counters.naval_operations_completed += 1;
    } else if tick.saturating_sub(operation.last_progress_tick) > NAVAL_STALL_TICKS {
        transition_naval(
            operation,
            NavalOperationPhase::Complete,
            tick,
            Some("STALLED"),
            outcome,
        );
        outcome.counters.naval_operations_completed += 1;
    }
    emit_naval_outputs(operation, live_units, outcome);
}

fn advance_route(operation: &mut NavalOperation, members: &[ExecutionUnitInput], tick: u64) {
    if members.is_empty() {
        return;
    }
    let quorum = sixty_percent_ceil(members.len());
    while let Some(waypoint) = operation.route.get(operation.route_index).copied() {
        let arrived = members
            .iter()
            .filter(|unit| {
                unit.position.distance_squared(waypoint) < ROUTE_ARRIVAL_DISTANCE_SQUARED
            })
            .count();
        if arrived < quorum {
            break;
        }
        operation.route_index += 1;
        operation.last_progress_tick = tick;
    }
}

fn emit_naval_outputs(
    operation: &NavalOperation,
    live_units: &BTreeMap<u64, ExecutionUnitInput>,
    outcome: &mut OperationalExecutionOutcome,
) {
    let (speed, desired_transport) = match operation.phase {
        NavalOperationPhase::Gathering | NavalOperationPhase::Embarkation => (2.0, true),
        NavalOperationPhase::Transit => (
            if operation.kind == NavalOperationKind::FastTransport {
                6.0
            } else {
                2.5
            },
            true,
        ),
        NavalOperationPhase::Landing | NavalOperationPhase::Delivered => (1.5, false),
        NavalOperationPhase::Complete => (0.0, false),
    };
    let target = naval_steering_target(operation);
    for member in &operation.members {
        let Some(unit) = live_units.get(&member.unit_id) else {
            continue;
        };
        if unit.transport != desired_transport {
            outcome.transport_updates.push(TransportUpdate {
                unit_id: unit.unit_id,
                transport: desired_transport,
            });
        }
        if speed > 0.0
            && let Some(steering) = make_steering(unit, operation.id.as_str(), target, speed)
        {
            outcome.steering.push(steering);
        }
    }
}

fn naval_steering_target(operation: &NavalOperation) -> Point {
    if operation.kind == NavalOperationKind::FastTransport {
        return operation.target;
    }
    match operation.phase {
        NavalOperationPhase::Gathering => operation.staging,
        NavalOperationPhase::Embarkation | NavalOperationPhase::Transit => operation
            .route
            .get(operation.route_index)
            .copied()
            .unwrap_or(operation.target),
        NavalOperationPhase::Landing
        | NavalOperationPhase::Delivered
        | NavalOperationPhase::Complete => operation.target,
    }
}

fn transition_naval(
    operation: &mut NavalOperation,
    phase: NavalOperationPhase,
    tick: u64,
    reason: Option<&str>,
    outcome: &mut OperationalExecutionOutcome,
) {
    if operation.phase == phase {
        return;
    }
    let from = operation.phase;
    operation.phase = phase;
    operation.phase_started_tick = tick;
    operation.last_progress_tick = tick;
    operation.completion_reason = reason.map(str::to_owned);
    operation.progress = match phase {
        NavalOperationPhase::Gathering => operation.progress.max(0.0),
        NavalOperationPhase::Embarkation => operation.progress.max(0.2),
        NavalOperationPhase::Transit => operation.progress.max(0.45),
        NavalOperationPhase::Landing | NavalOperationPhase::Delivered => {
            operation.progress.max(0.75)
        }
        NavalOperationPhase::Complete => 1.0,
    };
    outcome.naval_transitions.push(NavalTransition {
        operation_id: operation.id.clone(),
        from,
        to: phase,
        tick,
        reason: reason.map(str::to_owned),
    });
    outcome.counters.naval_phase_transitions += 1;
}

fn record_naval_progress(operation: &mut NavalOperation, progress: f64, tick: u64) {
    let progress = progress.clamp(0.0, 1.0);
    if progress > operation.progress + PROGRESS_EPSILON {
        operation.progress = progress;
        operation.last_progress_tick = tick;
    }
}

fn make_steering(
    unit: &ExecutionUnitInput,
    assignment_id: &str,
    target: Point,
    speed_multiplier: f64,
) -> Option<ExecutionSteering> {
    let d_lat = target.lat - unit.position.lat;
    let d_lng = wrapped_longitude_delta(unit.position.lng, target.lng);
    let distance = (d_lat * d_lat + d_lng * d_lng).sqrt();
    let (dir_lat, dir_lng) = if distance > 1.0e-12 {
        (d_lat / distance, d_lng / distance)
    } else {
        (0.0, 0.0)
    };
    Some(ExecutionSteering {
        unit_id: unit.unit_id,
        assignment_id: assignment_id.to_owned(),
        target,
        dir_lat,
        dir_lng,
        speed_multiplier,
        movement_enabled: distance > 0.01 && !unit.engaged && !unit.deploying,
    })
}

fn desired_reaction_units(
    kind: DefenderReactionKind,
    threat: &DefenderThreat,
    own_units: usize,
    live_units: &BTreeMap<u64, ExecutionUnitInput>,
) -> usize {
    match kind {
        DefenderReactionKind::NavalTransit => 10.min(own_units * 15 / 100),
        DefenderReactionKind::LandOffensive => {
            if threat.enemy_force < 3 {
                0
            } else {
                ceil_three_halves(threat.enemy_force).min(own_units * 30 / 100)
            }
        }
        DefenderReactionKind::Landing => {
            if threat.enemy_force < 3 {
                return 0;
            }
            let local_defenders = live_units
                .values()
                .filter(|unit| {
                    unit.side == threat.defender_side
                        && !unit.deploying
                        && unit.position.distance_squared(threat.target) < 9.0
                })
                .count();
            ceil_three_halves(threat.enemy_force).saturating_sub(local_defenders)
        }
    }
}

fn minimum_reaction_force(kind: DefenderReactionKind) -> usize {
    match kind {
        DefenderReactionKind::NavalTransit | DefenderReactionKind::LandOffensive => 3,
        DefenderReactionKind::Landing => 1,
    }
}

fn update_reaction_distance_progress(
    reaction: &mut DefenderReaction,
    tick: u64,
    live_units: &BTreeMap<u64, ExecutionUnitInput>,
) {
    let distances = reaction
        .unit_ids
        .iter()
        .filter_map(|unit_id| live_units.get(unit_id))
        .map(|unit| unit.position.distance_squared(reaction.target))
        .collect::<Vec<_>>();
    if distances.is_empty() {
        reaction.best_distance_squared = None;
        return;
    }
    let mean = distances.iter().sum::<f64>() / distances.len() as f64;
    match reaction.best_distance_squared {
        Some(best) if mean + PROGRESS_EPSILON < best => {
            reaction.best_distance_squared = Some(mean);
            reaction.last_progress_tick = tick;
        }
        None => reaction.best_distance_squared = Some(mean),
        Some(_) => {}
    }
}

fn cancel_reaction(
    reaction: DefenderReaction,
    tick: u64,
    reason: &'static str,
    outcome: &mut OperationalExecutionOutcome,
) {
    outcome
        .defender_reaction_events
        .push(DefenderReactionEvent {
            reaction_id: reaction.id,
            threat_signature: reaction.threat_signature,
            kind: DefenderReactionEventKind::Cancelled,
            tick,
            reason,
        });
    outcome.counters.defender_reactions_cancelled += 1;
}

fn reaction_priority(kind: DefenderReactionKind) -> u8 {
    match kind {
        DefenderReactionKind::Landing => 3,
        DefenderReactionKind::LandOffensive => 2,
        DefenderReactionKind::NavalTransit => 1,
    }
}

fn reaction_threat_kind(kind: DefenderReactionKind) -> DefenderThreatKind {
    match kind {
        DefenderReactionKind::NavalTransit | DefenderReactionKind::Landing => {
            DefenderThreatKind::NavalInvasion
        }
        DefenderReactionKind::LandOffensive => DefenderThreatKind::LandOffensive,
    }
}

fn count_units_by_side<'a>(
    side_count: usize,
    units: impl Iterator<Item = &'a ExecutionUnitInput>,
) -> Vec<usize> {
    let mut counts = vec![0; side_count];
    for unit in units {
        counts[unit.side] += 1;
    }
    counts
}

fn validate_threats(
    threats: &[DefenderThreat],
    side_count: usize,
) -> Result<(), OperationalExecutionError> {
    let mut signatures = BTreeSet::new();
    for threat in threats {
        let invalid = |reason| OperationalExecutionError::InvalidThreat {
            signature: threat.signature.clone(),
            reason,
        };
        if !valid_identifier(&threat.signature) {
            return Err(invalid("signature"));
        }
        if !signatures.insert((threat.kind, threat.signature.as_str())) {
            return Err(invalid("duplicate signature"));
        }
        if threat.defender_side >= side_count
            || threat.enemy_side >= side_count
            || threat.defender_side == threat.enemy_side
        {
            return Err(invalid("side"));
        }
        if !threat.target.validate() {
            return Err(invalid("target"));
        }
        if threat.active && threat.reaction_kind().is_none() {
            return Err(invalid("kind and phase"));
        }
    }
    Ok(())
}

fn collect_live_units(
    units: &[ExecutionUnitInput],
    side_count: Option<usize>,
) -> Result<BTreeMap<u64, ExecutionUnitInput>, OperationalExecutionError> {
    let mut result = BTreeMap::new();
    for &unit in units {
        if !unit.position.validate() {
            return Err(OperationalExecutionError::InvalidInput("unit position"));
        }
        if side_count.is_some_and(|count| unit.side >= count) {
            return Err(OperationalExecutionError::InvalidInput("unit side"));
        }
        if result.insert(unit.unit_id, unit).is_some() {
            return Err(OperationalExecutionError::InvalidInput("duplicate unit id"));
        }
    }
    Ok(result)
}

fn validate_live_unit_map(
    side_count: usize,
    units: &BTreeMap<u64, ExecutionUnitInput>,
) -> Result<(), OperationalExecutionError> {
    for (&unit_id, unit) in units {
        if unit_id != unit.unit_id {
            return Err(OperationalExecutionError::InvalidInput("unit map key"));
        }
        if unit.side >= side_count {
            return Err(OperationalExecutionError::InvalidInput("unit side"));
        }
        if !unit.position.validate() {
            return Err(OperationalExecutionError::InvalidInput("unit position"));
        }
    }
    Ok(())
}

fn register_assignment(
    assignments: &mut BTreeMap<u64, String>,
    unit_id: u64,
    assignment: &str,
) -> Result<(), OperationalExecutionError> {
    if let Some(first) = assignments.insert(unit_id, assignment.to_owned()) {
        return Err(OperationalExecutionError::ConflictingAssignment {
            unit_id,
            first,
            second: assignment.to_owned(),
        });
    }
    Ok(())
}

fn infer_side_count(
    state: &OperationalExecutionState,
    units: &[ExecutionUnitInput],
    threats: &[DefenderThreat],
) -> usize {
    let state_max = state
        .naval_operations
        .iter()
        .flat_map(|operation| [Some(operation.side), operation.enemy_side])
        .flatten()
        .chain(
            state
                .defender_reactions
                .iter()
                .flat_map(|reaction| [reaction.side, reaction.enemy_side]),
        )
        .max();
    let unit_max = units.iter().map(|unit| unit.side).max();
    let threat_max = threats
        .iter()
        .flat_map(|threat| [threat.defender_side, threat.enemy_side])
        .max();
    state_max
        .into_iter()
        .chain(unit_max)
        .chain(threat_max)
        .max()
        .map_or(1, |side| side.saturating_add(1))
}

fn valid_identifier(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn valid_role(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= 64 && !value.chars().any(char::is_control)
}

fn ceil_three_halves(value: usize) -> usize {
    value.saturating_mul(3).saturating_add(1) / 2
}

fn sixty_percent_ceil(value: usize) -> usize {
    value.saturating_mul(3).saturating_add(4) / 5
}

pub fn wrapped_longitude_delta(from: f64, to: f64) -> f64 {
    let mut delta = to - from;
    while delta > 180.0 {
        delta -= 360.0;
    }
    while delta < -180.0 {
        delta += 360.0;
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(lat: f64, lng: f64) -> Point {
        Point { lat, lng }
    }

    fn unit(unit_id: u64, side: usize, lat: f64, lng: f64) -> ExecutionUnitInput {
        ExecutionUnitInput {
            unit_id,
            side,
            country: (side + 1) as u16,
            position: point(lat, lng),
            transport: false,
            at_sea: false,
            deploying: false,
            engaged: false,
            operationally_assigned: false,
        }
    }

    fn member(unit_id: u64) -> NavalMember {
        NavalMember {
            unit_id,
            role: "LINE".to_owned(),
            assigned_tick: 0,
        }
    }

    fn operation(kind: NavalOperationKind, members: &[u64]) -> NavalOperation {
        NavalOperation {
            id: "naval-1".to_owned(),
            signature: "naval-signature-1".to_owned(),
            kind,
            phase: if kind == NavalOperationKind::FastTransport {
                NavalOperationPhase::Transit
            } else {
                NavalOperationPhase::Gathering
            },
            side: 0,
            country: 1,
            enemy_side: Some(1),
            max_assigned_units: members.len().max(5),
            members: members.iter().copied().map(member).collect(),
            staging: point(0.0, 0.0),
            target: point(0.0, 20.0),
            route: vec![point(0.0, 10.0)],
            route_index: 0,
            progress: 0.0,
            started_tick: 0,
            phase_started_tick: 0,
            last_progress_tick: 0,
            completion_reason: None,
        }
    }

    fn state_with(operation: NavalOperation) -> OperationalExecutionState {
        OperationalExecutionState {
            naval_operations: vec![operation],
            ..OperationalExecutionState::new()
        }
    }

    fn naval_threat(phase: DefenderThreatPhase, enemy_force: usize) -> DefenderThreat {
        DefenderThreat {
            signature: "threat-naval-1".to_owned(),
            defender_side: 0,
            enemy_side: 1,
            kind: DefenderThreatKind::NavalInvasion,
            phase,
            target: point(0.0, 0.0),
            enemy_force,
            active: true,
        }
    }

    #[test]
    fn threat_signatures_are_unique_within_each_operational_domain() {
        let naval = naval_threat(DefenderThreatPhase::Transit, 5);
        let land = DefenderThreat {
            signature: naval.signature.clone(),
            defender_side: naval.defender_side,
            enemy_side: naval.enemy_side,
            kind: DefenderThreatKind::LandOffensive,
            phase: DefenderThreatPhase::Execution,
            target: naval.target,
            enemy_force: naval.enemy_force,
            active: true,
        };

        assert!(validate_threats(&[naval.clone(), land], 2).is_ok());
        assert_eq!(
            validate_threats(&[naval.clone(), naval], 2),
            Err(OperationalExecutionError::InvalidThreat {
                signature: "threat-naval-1".to_owned(),
                reason: "duplicate signature",
            })
        );
    }

    #[test]
    fn serde_is_strict_and_nullable_fields_are_required() {
        let state = OperationalExecutionState::new();
        assert_eq!(state.schema, OPERATIONAL_EXECUTION_SCHEMA_VERSION);
        assert_eq!(state.next_reaction_sequence, 1);

        let value = serde_json::to_value(&state).unwrap();
        let mut object = value.as_object().unwrap().clone();
        object.insert("unknown".to_owned(), serde_json::Value::Bool(true));
        assert!(
            serde_json::from_value::<OperationalExecutionState>(serde_json::Value::Object(object))
                .is_err()
        );
        assert!(serde_json::from_str::<Point>(r#"{"lat":0,"lng":0,"extra":1}"#).is_err());

        let encoded =
            serde_json::to_value(state_with(operation(NavalOperationKind::Invasion, &[1])))
                .unwrap();
        let mut missing = encoded.clone();
        missing["navalOperations"][0]
            .as_object_mut()
            .unwrap()
            .remove("enemySide");
        assert!(serde_json::from_value::<OperationalExecutionState>(missing).is_err());
        assert!(serde_json::from_value::<OperationalExecutionState>(encoded).is_ok());

        let mut invalid = state;
        invalid.schema = "wrong".to_owned();
        assert_eq!(
            invalid.validate_shape(0, 2),
            Err(OperationalExecutionError::InvalidState("schema"))
        );
    }

    #[test]
    fn invasion_advances_through_all_phases() {
        let mut units = (1..=5).map(|id| unit(id, 0, 0.0, 0.0)).collect::<Vec<_>>();
        let mut state = state_with(operation(NavalOperationKind::Invasion, &[1, 2, 3, 4, 5]));

        let gathering = state.advance(1, &units, &[]).unwrap();
        assert_eq!(
            gathering.naval_transitions[0].to,
            NavalOperationPhase::Embarkation
        );
        assert!(
            gathering
                .transport_updates
                .iter()
                .all(|update| update.transport)
        );
        assert!(
            gathering
                .steering
                .iter()
                .all(|order| order.target == point(0.0, 10.0))
        );

        for current in units.iter_mut().take(3) {
            current.at_sea = true;
            current.transport = true;
        }
        let embarkation = state.advance(2, &units, &[]).unwrap();
        assert_eq!(
            embarkation.naval_transitions[0].to,
            NavalOperationPhase::Transit
        );

        for (index, current) in units.iter_mut().enumerate() {
            current.at_sea = index >= 3;
            current.transport = true;
            if index < 3 {
                current.position = point(0.0, 20.0);
            }
        }
        let transit = state.advance(3, &units, &[]).unwrap();
        assert_eq!(
            transit.naval_transitions[0].to,
            NavalOperationPhase::Landing
        );
        assert!(
            transit
                .transport_updates
                .iter()
                .all(|update| !update.transport)
        );

        for current in &mut units {
            current.transport = false;
        }
        let landing = state.advance(904, &units, &[]).unwrap();
        assert_eq!(
            landing.naval_transitions[0].to,
            NavalOperationPhase::Complete
        );
        assert_eq!(
            landing.naval_transitions[0].reason.as_deref(),
            Some("LANDING_ESTABLISHED")
        );
        assert!(state.naval_operations.is_empty());
    }

    #[test]
    fn supply_uses_three_unit_gather_and_two_unit_delivery_thresholds() {
        let mut units = (1..=3).map(|id| unit(id, 0, 0.0, 0.0)).collect::<Vec<_>>();
        let mut supply = operation(NavalOperationKind::Supply, &[1, 2, 3]);
        supply.max_assigned_units = 3;
        let mut state = state_with(supply);
        assert_eq!(
            state.advance(1, &units, &[]).unwrap().naval_transitions[0].to,
            NavalOperationPhase::Embarkation
        );

        for current in units.iter_mut().take(2) {
            current.at_sea = true;
            current.transport = true;
        }
        assert_eq!(
            state.advance(2, &units, &[]).unwrap().naval_transitions[0].to,
            NavalOperationPhase::Transit
        );
        for (index, current) in units.iter_mut().enumerate() {
            current.at_sea = index == 2;
            current.transport = true;
            if index < 2 {
                current.position = point(0.0, 20.0);
            }
        }
        assert_eq!(
            state.advance(3, &units, &[]).unwrap().naval_transitions[0].to,
            NavalOperationPhase::Delivered
        );
        for current in &mut units {
            current.transport = false;
        }
        assert_eq!(
            state.advance(604, &units, &[]).unwrap().naval_transitions[0].to,
            NavalOperationPhase::Complete
        );
    }

    #[test]
    fn stalled_operation_is_cancelled_from_real_progress_clock() {
        let mut current = unit(1, 0, 20.0, 20.0);
        current.transport = true;
        let mut state = state_with(operation(NavalOperationKind::Invasion, &[1]));
        let outcome = state.advance(601, &[current], &[]).unwrap();
        assert_eq!(
            outcome.naval_transitions[0].to,
            NavalOperationPhase::Complete
        );
        assert_eq!(
            outcome.naval_transitions[0].reason.as_deref(),
            Some("STALLED")
        );
        assert_eq!(
            outcome.transport_updates,
            vec![TransportUpdate {
                unit_id: 1,
                transport: false
            }]
        );
        assert!(state.naval_operations.is_empty());
    }

    #[test]
    fn fast_transport_releases_each_arrival_and_steers_remaining_member() {
        let mut first = unit(1, 0, 0.0, 19.0);
        let mut second = unit(2, 0, 0.0, 0.0);
        first.transport = true;
        second.transport = true;
        let mut state = state_with(operation(NavalOperationKind::FastTransport, &[1, 2]));

        let first_tick = state.advance(1, &[first, second], &[]).unwrap();
        assert_eq!(
            first_tick.transport_updates,
            vec![TransportUpdate {
                unit_id: 1,
                transport: false
            }]
        );
        assert_eq!(first_tick.steering.len(), 1);
        assert_eq!(first_tick.steering[0].unit_id, 2);
        assert_eq!(first_tick.steering[0].target, point(0.0, 20.0));
        assert_eq!(first_tick.steering[0].speed_multiplier, 6.0);
        assert_eq!(state.naval_operations[0].members[0].unit_id, 2);

        second.position = point(0.0, 19.0);
        let second_tick = state.advance(2, &[first, second], &[]).unwrap();
        assert_eq!(
            second_tick.naval_transitions[0].reason.as_deref(),
            Some("ARRIVED")
        );
        assert!(state.naval_operations.is_empty());
    }

    #[test]
    fn wrapped_longitude_is_used_for_steering_and_distance() {
        let first = point(0.0, 179.0);
        let second = point(0.0, -179.0);
        assert_eq!(first.distance_squared(second), 4.0);
        let order = make_steering(&unit(1, 0, 0.0, 179.0), "test", second, 2.0).unwrap();
        assert_eq!(order.dir_lng, 1.0);
    }

    #[test]
    fn transit_reaction_recruits_nearest_eligible_units_deterministically() {
        let mut units = (1..=20)
            .map(|id| unit(id, 0, 0.0, 3.1 + id as f64 * 0.01))
            .collect::<Vec<_>>();
        units[0].deploying = true;
        units[1].engaged = true;
        units[2].transport = true;
        units[3].operationally_assigned = true;
        let threat = naval_threat(DefenderThreatPhase::Transit, 5);
        let mut state = OperationalExecutionState::new();

        let outcome = state
            .advance(1, &units, std::slice::from_ref(&threat))
            .unwrap();
        assert_eq!(outcome.counters.defender_reactions_created, 1);
        assert_eq!(outcome.counters.defender_units_recruited, 3);
        assert_eq!(state.defender_reactions[0].max_units, 3);
        assert_eq!(state.defender_reactions[0].unit_ids, vec![5, 6, 7]);
        assert!(
            outcome
                .steering
                .iter()
                .all(|order| order.speed_multiplier == 2.0)
        );

        let continued = state.advance(2, &units, &[threat]).unwrap();
        assert_eq!(continued.counters.defender_reactions_created, 0);
        assert_eq!(state.defender_reactions[0].unit_ids, vec![5, 6, 7]);
    }

    #[test]
    fn land_reaction_caps_at_thirty_percent_and_landing_recruits_ratio_deficit() {
        let units = (1..=20)
            .map(|id| unit(id, 0, 0.0, 4.0 + id as f64 * 0.01))
            .collect::<Vec<_>>();
        let land = DefenderThreat {
            signature: "land-1".to_owned(),
            defender_side: 0,
            enemy_side: 1,
            kind: DefenderThreatKind::LandOffensive,
            phase: DefenderThreatPhase::Execution,
            target: point(0.0, 0.0),
            enemy_force: 5,
            active: true,
        };
        let mut land_state = OperationalExecutionState::new();
        land_state.advance(1, &units, &[land]).unwrap();
        assert_eq!(land_state.defender_reactions[0].max_units, 6);
        assert_eq!(land_state.defender_reactions[0].unit_ids.len(), 6);

        let mut landing_units = vec![unit(1, 0, 0.0, 1.0), unit(2, 0, 0.0, 2.0)];
        landing_units.extend((3..=12).map(|id| unit(id, 0, 0.0, 4.0 + id as f64 * 0.01)));
        let landing = naval_threat(DefenderThreatPhase::Landing, 4);
        let mut landing_state = OperationalExecutionState::new();
        landing_state
            .advance(1, &landing_units, &[landing])
            .unwrap();
        assert_eq!(landing_state.defender_reactions[0].max_units, 4);
        assert_eq!(landing_state.defender_reactions[0].unit_ids.len(), 4);
    }

    #[test]
    fn reaction_arrivals_release_units_and_absent_threat_cancels_plan() {
        let units = (1..=20)
            .map(|id| unit(id, 0, 0.0, 3.1 + id as f64 * 0.01))
            .collect::<Vec<_>>();
        let threat = naval_threat(DefenderThreatPhase::Transit, 5);
        let mut state = OperationalExecutionState::new();
        state
            .advance(1, &units, std::slice::from_ref(&threat))
            .unwrap();

        let arrived_id = state.defender_reactions[0].unit_ids[0];
        let mut moved = units.clone();
        moved[(arrived_id - 1) as usize].position = point(0.0, 0.5);
        let arrival = state.advance(2, &moved, &[threat]).unwrap();
        assert_eq!(arrival.counters.defender_units_arrived, 1);
        assert!(!state.defender_reactions[0].unit_ids.contains(&arrived_id));

        let cancelled = state.advance(3, &moved, &[]).unwrap();
        assert_eq!(cancelled.counters.defender_reactions_cancelled, 1);
        assert_eq!(
            cancelled.defender_reaction_events[0].reason,
            "THREAT_ABSENT"
        );
        assert!(state.defender_reactions.is_empty());
    }

    #[test]
    fn defeated_landing_and_stalled_reaction_cancel_on_exact_clocks() {
        let units = (1..=20)
            .map(|id| unit(id, 0, 0.0, 4.0 + id as f64 * 0.01))
            .collect::<Vec<_>>();
        let mut state = OperationalExecutionState::new();
        let landing = naval_threat(DefenderThreatPhase::Landing, 4);
        state.advance(1, &units, &[landing]).unwrap();

        let weak = naval_threat(DefenderThreatPhase::Landing, 2);
        state
            .advance(10, &units, std::slice::from_ref(&weak))
            .unwrap();
        assert_eq!(state.defender_reactions[0].landing_defeated_tick, Some(10));
        assert_eq!(
            state
                .advance(611, &units, &[weak])
                .unwrap()
                .counters
                .defender_reactions_cancelled,
            1
        );

        let mut stalled_state = OperationalExecutionState::new();
        let static_unit = unit(1, 0, 0.0, 4.0);
        stalled_state.defender_reactions.push(DefenderReaction {
            id: "reaction-0-1".to_owned(),
            sequence: 1,
            threat_signature: "threat-naval-1".to_owned(),
            side: 0,
            enemy_side: 1,
            kind: DefenderReactionKind::NavalTransit,
            target: point(0.0, 0.0),
            unit_ids: vec![1],
            max_units: 1,
            started_tick: 0,
            last_progress_tick: 0,
            best_distance_squared: Some(16.0),
            landing_defeated_tick: None,
        });
        stalled_state.next_reaction_sequence = 2;
        let stalled_threat = naval_threat(DefenderThreatPhase::Transit, 3);
        assert_eq!(
            stalled_state
                .advance(1_801, &[static_unit], &[stalled_threat])
                .unwrap()
                .defender_reaction_events[0]
                .reason,
            "NO_PROGRESS"
        );
    }

    #[test]
    fn validation_rejects_missing_and_conflicting_unit_references() {
        let operation = operation(NavalOperationKind::Invasion, &[1]);
        let state = state_with(operation.clone());
        let no_units = BTreeMap::new();
        assert!(matches!(
            state.validate(2, &no_units, 0),
            Err(OperationalExecutionError::MissingUnit { unit_id: 1, .. })
        ));

        let mut conflicting = state_with(operation);
        conflicting.defender_reactions.push(DefenderReaction {
            id: "reaction-0-1".to_owned(),
            sequence: 1,
            threat_signature: "threat-1".to_owned(),
            side: 0,
            enemy_side: 1,
            kind: DefenderReactionKind::NavalTransit,
            target: point(0.0, 0.0),
            unit_ids: vec![1],
            max_units: 1,
            started_tick: 0,
            last_progress_tick: 0,
            best_distance_squared: None,
            landing_defeated_tick: None,
        });
        conflicting.next_reaction_sequence = 2;
        assert!(matches!(
            conflicting.validate_shape(0, 2),
            Err(OperationalExecutionError::ConflictingAssignment { unit_id: 1, .. })
        ));
    }

    #[test]
    fn validation_rejects_cross_kernel_operational_ownership() {
        let mut assigned = unit(1, 0, 0.0, 0.0);
        assigned.operationally_assigned = true;
        let live = collect_live_units(&[assigned], Some(2)).unwrap();

        let naval = state_with(operation(NavalOperationKind::Invasion, &[1]));
        assert_eq!(
            naval.validate(2, &live, 0),
            Err(OperationalExecutionError::InvalidNavalOperation {
                id: "naval-1".to_owned(),
                reason: "member operational assignment",
            })
        );

        let mut reaction = OperationalExecutionState::new();
        reaction.defender_reactions.push(DefenderReaction {
            id: "reaction-0-1".to_owned(),
            sequence: 1,
            threat_signature: "threat-1".to_owned(),
            side: 0,
            enemy_side: 1,
            kind: DefenderReactionKind::NavalTransit,
            target: point(0.0, 0.0),
            unit_ids: vec![1],
            max_units: 1,
            started_tick: 0,
            last_progress_tick: 0,
            best_distance_squared: None,
            landing_defeated_tick: None,
        });
        reaction.next_reaction_sequence = 2;
        assert_eq!(
            reaction.validate(2, &live, 0),
            Err(OperationalExecutionError::InvalidDefenderReaction {
                id: "reaction-0-1".to_owned(),
                reason: "member operational assignment",
            })
        );
    }

    #[test]
    fn post_commit_reconciliation_prunes_references_without_advancing_clocks() {
        let mut state = state_with(operation(NavalOperationKind::Invasion, &[1, 2]));
        state.defender_reactions.push(DefenderReaction {
            id: "reaction-1-1".to_owned(),
            sequence: 1,
            threat_signature: "threat-1".to_owned(),
            side: 1,
            enemy_side: 0,
            kind: DefenderReactionKind::LandOffensive,
            target: point(0.0, 0.0),
            unit_ids: vec![3],
            max_units: 1,
            started_tick: 0,
            last_progress_tick: 0,
            best_distance_squared: Some(16.0),
            landing_defeated_tick: None,
        });
        state.next_reaction_sequence = 2;
        let live = collect_live_units(&[unit(2, 0, 0.0, 0.0)], Some(2)).unwrap();
        let counters = state.retain_live_units(&live);
        assert_eq!(counters.naval_member_references_pruned, 1);
        assert_eq!(counters.defender_member_references_pruned, 1);
        assert_eq!(state.naval_operations[0].members[0].unit_id, 2);
        assert_eq!(state.defender_reactions[0].last_progress_tick, 0);
    }

    #[test]
    fn timer_rebase_preserves_elapsed_ages_in_both_coordinate_systems() {
        let mut naval = operation(NavalOperationKind::Invasion, &[1]);
        naval.started_tick = 90;
        naval.phase_started_tick = 95;
        naval.last_progress_tick = 98;
        naval.members[0].assigned_tick = 91;
        naval.members.push(NavalMember {
            unit_id: 3,
            role: "RESERVE".to_owned(),
            assigned_tick: 0,
        });
        let mut state = state_with(naval);
        state.defender_reactions.push(DefenderReaction {
            id: "reaction-1-1".to_owned(),
            sequence: 1,
            threat_signature: "threat-1".to_owned(),
            side: 1,
            enemy_side: 0,
            kind: DefenderReactionKind::Landing,
            target: point(0.0, 0.0),
            unit_ids: vec![2],
            max_units: 1,
            started_tick: 80,
            last_progress_tick: 99,
            best_distance_squared: None,
            landing_defeated_tick: Some(85),
        });
        state.next_reaction_sequence = 2;

        state.rebase_timer_coordinates(100, 40);
        let operation = &state.naval_operations[0];
        assert_eq!(
            (
                operation.started_tick,
                operation.phase_started_tick,
                operation.last_progress_tick,
                operation.members[0].assigned_tick,
            ),
            (30, 35, 38, 31)
        );
        assert_eq!(operation.members[1].assigned_tick, 0);
        let reaction = &state.defender_reactions[0];
        assert_eq!(
            (
                reaction.started_tick,
                reaction.last_progress_tick,
                reaction.landing_defeated_tick,
            ),
            (20, 39, Some(25))
        );

        state.rebase_timer_coordinates(40, 100);
        let operation = &state.naval_operations[0];
        assert_eq!(
            (
                operation.started_tick,
                operation.phase_started_tick,
                operation.last_progress_tick,
                operation.members[0].assigned_tick,
            ),
            (90, 95, 98, 91)
        );
        assert_eq!(operation.members[1].assigned_tick, 0);
        let reaction = &state.defender_reactions[0];
        assert_eq!(
            (
                reaction.started_tick,
                reaction.last_progress_tick,
                reaction.landing_defeated_tick,
            ),
            (80, 99, Some(85))
        );
    }
}
