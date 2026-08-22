//! Deterministic operational-AI continuation state and pure staging kernels.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{combat::wrapped_longitude_delta, dynamics::WarPosture};

pub const OPERATIONAL_AI_SCHEMA_VERSION: &str = "native-operational-ai-v1";
pub const OPERATIONAL_OVERRIDE_HISTORY_LIMIT: usize = 64;
const SUPPLY_COLLAPSE_MEMBER_SHARE: f64 = 0.35;
const SUPPLY_COLLAPSE_MEMORY_TICKS: u64 = 15;
const SUPPLY_OVERRIDE_TICKS: u64 = 300;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalPoint {
    pub lat: f64,
    pub lng: f64,
}

impl OperationalPoint {
    fn validate(self) -> bool {
        self.lat.is_finite()
            && self.lng.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lng)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IntelStatus {
    Fresh,
    Stale,
    Degraded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalIntelConfig {
    pub scan_interval_ticks: u64,
    pub fresh_ticks: u64,
    pub stale_ticks: u64,
    pub expire_ticks: u64,
}

impl Default for OperationalIntelConfig {
    fn default() -> Self {
        Self {
            scan_interval_ticks: 150,
            fresh_ticks: 300,
            stale_ticks: 1_200,
            expire_ticks: 1_800,
        }
    }
}

impl OperationalIntelConfig {
    fn validate(self) -> bool {
        self.scan_interval_ticks > 0
            && self.fresh_ticks > 0
            && self.fresh_ticks <= self.stale_ticks
            && self.stale_ticks <= self.expire_ticks
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrewarEnemyPower {
    pub side_index: usize,
    pub power: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalContact {
    pub key: String,
    pub enemy_side_index: usize,
    pub sector_id: String,
    pub unit_id: u64,
    pub country_id: Option<u16>,
    pub domain: String,
    pub kind: String,
    pub lat: f64,
    pub lng: f64,
    pub velocity_lat: f64,
    pub velocity_lng: f64,
    pub observed_power: f64,
    pub base_confidence: f64,
    pub confidence: f64,
    pub observed_tick: u64,
    pub age_ticks: u64,
    pub status: IntelStatus,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalIntelState {
    pub last_scan_tick: u64,
    pub revision: u64,
    pub config: OperationalIntelConfig,
    pub prewar_enemy_power: Vec<PrewarEnemyPower>,
    pub contacts: Vec<OperationalContact>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationalPosture {
    Offensive,
    Defensive,
}

impl From<OperationalPosture> for WarPosture {
    fn from(value: OperationalPosture) -> Self {
        match value {
            OperationalPosture::Offensive => Self::Offensive,
            OperationalPosture::Defensive => Self::Defensive,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationalOverrideSource {
    LastStand,
    SupplyCollapse,
    OffensiveDesperation,
    DefensiveDesperation,
    DefenderReaction,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalOverride {
    pub posture: OperationalPosture,
    pub source: OperationalOverrideSource,
    pub started_tick: u64,
    pub expires_tick: Option<u64>,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalSideState {
    pub side_index: usize,
    pub hostile_side_indices: Vec<usize>,
    pub intel: OperationalIntelState,
    pub r#override: Option<OperationalOverride>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskForcePhase {
    Assembling,
    Attacking,
    Consolidating,
    Culminated,
    Withdrawing,
    Regrouping,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskForcePosture {
    Aggressive,
    Balanced,
    Defensive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskForceRole {
    Spearhead,
    Line,
    Reserve,
    Support,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskForceMember {
    pub unit_id: u64,
    pub role: TaskForceRole,
    pub assigned_tick: u64,
    pub route_progress: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalTaskForce {
    pub id: String,
    pub signature: String,
    pub side_index: usize,
    pub plan_signature: String,
    pub plan_type: String,
    pub theater_id: Option<String>,
    pub target: Option<OperationalPoint>,
    pub staging_anchor: Option<OperationalPoint>,
    pub route: Vec<OperationalPoint>,
    pub phase: TaskForcePhase,
    pub posture: TaskForcePosture,
    pub members: Vec<TaskForceMember>,
    pub reserve_unit_ids: Vec<u64>,
    pub desired_power: f64,
    pub launch_power: f64,
    pub current_power: f64,
    pub peak_power: f64,
    pub readiness: f64,
    pub max_assigned_units: usize,
    pub created_tick: u64,
    pub phase_started_tick: u64,
    pub last_progress_tick: u64,
    pub last_recovery_tick: u64,
    pub recovery_power: f64,
    pub progress: f64,
    pub withdrawal_anchor: Option<OperationalPoint>,
    pub completion_reason: Option<String>,
    pub outcome: Option<String>,
    pub severe_surprise: bool,
    pub parent_task_force_id: Option<String>,
    pub supply_invalidated_tick: Option<u64>,
    pub intent_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CountryDesperationMode {
    Normal,
    LastStand,
    DefensiveDesperation,
    OffensiveDesperation,
    UnderMobilized,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountryDesperationState {
    pub country_id: u16,
    pub mode: CountryDesperationMode,
    pub initial_cities: Option<u64>,
    pub initial_manpower: Option<f64>,
    pub previous_controlled: Option<u64>,
    pub stall_ticks: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalOverrideEvent {
    pub sequence: u64,
    pub side_index: usize,
    pub posture: Option<OperationalPosture>,
    pub source: OperationalOverrideSource,
    pub started_tick: u64,
    pub expires_tick: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationalRuntimeState {
    pub schema: String,
    pub sides: Vec<OperationalSideState>,
    pub task_forces: Vec<OperationalTaskForce>,
    pub country_desperation: Vec<CountryDesperationState>,
    pub override_events: Vec<OperationalOverrideEvent>,
    pub next_override_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationalSnapshot {
    pub schema_version: &'static str,
    pub tick: u64,
    pub sides: Vec<OperationalSideState>,
    pub task_forces: Vec<OperationalTaskForce>,
    pub country_desperation: Vec<CountryDesperationState>,
    pub override_events: Vec<OperationalOverrideEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OperationalUnitInput {
    pub unit_id: u64,
    pub side_index: usize,
    pub country_id: u16,
    pub position: OperationalPoint,
    pub power: f64,
    pub readiness: f64,
    pub supply_collapsed_tick: Option<u64>,
    pub encircled_ticks: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TacticalContactObservation {
    pub observer_side: usize,
    pub enemy_side: usize,
    pub target_unit_id: u64,
    pub target_country_id: u16,
    pub target_position: OperationalPoint,
    pub observed_power: f64,
    pub kind: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CountryOperationalInput {
    pub country_id: u16,
    pub initial_land: u64,
    pub controlled: u64,
    pub cities_controlled: u64,
    pub current_personnel: f64,
    pub offensive_role: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OperationalSteering {
    pub unit_id: u64,
    pub task_force_id: String,
    pub phase: TaskForcePhase,
    pub role: TaskForceRole,
    pub target: OperationalPoint,
    pub dir_lat: f64,
    pub dir_lng: f64,
    pub speed_multiplier: f64,
    pub movement_enabled: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum OperationalError {
    #[error("operational AI state is invalid: {0}")]
    Invalid(&'static str),
}

impl OperationalRuntimeState {
    pub fn bootstrap(side_count: usize, hostility: &[u8], strength: &[f64]) -> Self {
        let sides = (0..side_count)
            .map(|side| {
                let hostile_side_indices = (0..side_count)
                    .filter(|other| {
                        hostility
                            .get(side * side_count + *other)
                            .copied()
                            .unwrap_or(0)
                            == 1
                    })
                    .collect::<Vec<_>>();
                let prewar_enemy_power = hostile_side_indices
                    .iter()
                    .map(|&enemy| PrewarEnemyPower {
                        side_index: enemy,
                        power: strength.get(enemy).copied().unwrap_or(0.0).max(0.0),
                    })
                    .collect();
                OperationalSideState {
                    side_index: side,
                    hostile_side_indices,
                    intel: OperationalIntelState {
                        last_scan_tick: 0,
                        revision: 0,
                        config: OperationalIntelConfig::default(),
                        prewar_enemy_power,
                        contacts: Vec::new(),
                    },
                    r#override: None,
                }
            })
            .collect();
        Self {
            schema: OPERATIONAL_AI_SCHEMA_VERSION.to_owned(),
            sides,
            task_forces: Vec::new(),
            country_desperation: Vec::new(),
            override_events: Vec::new(),
            next_override_sequence: 1,
        }
    }

    pub fn validate(
        &self,
        side_count: usize,
        live_units: &BTreeMap<u64, usize>,
        countries: &BTreeSet<u16>,
        tick: u64,
    ) -> Result<(), OperationalError> {
        if self.schema != OPERATIONAL_AI_SCHEMA_VERSION || self.sides.len() != side_count {
            return Err(OperationalError::Invalid("schema or side coverage"));
        }
        for (expected, side) in self.sides.iter().enumerate() {
            if side.side_index != expected
                || !strictly_sorted_unique(&side.hostile_side_indices)
                || side
                    .hostile_side_indices
                    .iter()
                    .any(|enemy| *enemy >= side_count || *enemy == expected)
                || !side.intel.config.validate()
                || side.intel.last_scan_tick > tick
            {
                return Err(OperationalError::Invalid("observer side state"));
            }
            if side.intel.prewar_enemy_power.len() != side.hostile_side_indices.len() {
                return Err(OperationalError::Invalid("prewar hostile coverage"));
            }
            for (enemy, record) in side
                .hostile_side_indices
                .iter()
                .zip(&side.intel.prewar_enemy_power)
            {
                if record.side_index != *enemy || !finite_non_negative(record.power) {
                    return Err(OperationalError::Invalid("prewar hostile power"));
                }
            }
            let mut previous_key: Option<&str> = None;
            for contact in &side.intel.contacts {
                if contact.key.is_empty()
                    || previous_key.is_some_and(|previous| previous >= contact.key.as_str())
                    || !side
                        .hostile_side_indices
                        .contains(&contact.enemy_side_index)
                    || !live_units.contains_key(&contact.unit_id)
                    || contact
                        .country_id
                        .is_some_and(|country| !countries.contains(&country))
                    || !(OperationalPoint {
                        lat: contact.lat,
                        lng: contact.lng,
                    })
                    .validate()
                    || ![
                        contact.velocity_lat,
                        contact.velocity_lng,
                        contact.observed_power,
                        contact.base_confidence,
                        contact.confidence,
                    ]
                    .into_iter()
                    .all(f64::is_finite)
                    || contact.observed_power < 0.0
                    || !(0.0..=1.0).contains(&contact.base_confidence)
                    || !(0.0..=1.0).contains(&contact.confidence)
                    || contact.observed_tick > tick
                    || contact.age_ticks > tick.saturating_sub(contact.observed_tick)
                {
                    return Err(OperationalError::Invalid("observer contact"));
                }
                previous_key = Some(&contact.key);
            }
            if let Some(current) = &side.r#override
                && (current.started_tick > tick
                    || current
                        .expires_tick
                        .is_some_and(|expires| expires < current.started_tick)
                    || current.sequence >= self.next_override_sequence)
            {
                return Err(OperationalError::Invalid("current posture override"));
            }
        }

        let mut prior_id: Option<&str> = None;
        let mut claimed_units = BTreeSet::new();
        for task_force in &self.task_forces {
            if task_force.id.is_empty()
                || prior_id.is_some_and(|prior| prior >= task_force.id.as_str())
                || task_force.side_index >= side_count
                || task_force.target.is_some_and(|point| !point.validate())
                || task_force
                    .staging_anchor
                    .is_some_and(|point| !point.validate())
                || task_force.route.iter().any(|point| !point.validate())
                || task_force
                    .withdrawal_anchor
                    .is_some_and(|point| !point.validate())
                || ![
                    task_force.desired_power,
                    task_force.launch_power,
                    task_force.current_power,
                    task_force.peak_power,
                    task_force.readiness,
                    task_force.recovery_power,
                    task_force.progress,
                ]
                .into_iter()
                .all(f64::is_finite)
                || task_force.desired_power < 0.0
                || task_force.launch_power < 0.0
                || task_force.current_power < 0.0
                || task_force.peak_power < 0.0
                || task_force.recovery_power < 0.0
                || !(0.0..=1.0).contains(&task_force.readiness)
                || !(0.0..=1.0).contains(&task_force.progress)
                || task_force.max_assigned_units == 0
                || [
                    task_force.created_tick,
                    task_force.phase_started_tick,
                    task_force.last_progress_tick,
                    task_force.last_recovery_tick,
                ]
                .into_iter()
                .any(|value| value > tick)
                || task_force
                    .supply_invalidated_tick
                    .is_some_and(|value| value > tick)
            {
                return Err(OperationalError::Invalid("task force"));
            }
            let mut previous_member = None;
            let mut member_ids = BTreeSet::new();
            for member in &task_force.members {
                if previous_member.is_some_and(|prior| prior >= member.unit_id)
                    || live_units.get(&member.unit_id) != Some(&task_force.side_index)
                    || !claimed_units.insert(member.unit_id)
                    || member.assigned_tick > tick
                    || !member.route_progress.is_finite()
                    || !(0.0..=1.0).contains(&member.route_progress)
                {
                    return Err(OperationalError::Invalid("task force member"));
                }
                member_ids.insert(member.unit_id);
                previous_member = Some(member.unit_id);
            }
            if !strictly_sorted_unique(&task_force.reserve_unit_ids)
                || task_force
                    .reserve_unit_ids
                    .iter()
                    .any(|unit_id| !member_ids.contains(unit_id))
            {
                return Err(OperationalError::Invalid("task force reserve"));
            }
            prior_id = Some(&task_force.id);
        }

        let mut previous_country = None;
        for country in &self.country_desperation {
            if previous_country.is_some_and(|prior| prior >= country.country_id)
                || !countries.contains(&country.country_id)
                || country
                    .initial_manpower
                    .is_some_and(|value| !finite_non_negative(value))
            {
                return Err(OperationalError::Invalid("country desperation"));
            }
            previous_country = Some(country.country_id);
        }
        let mut previous_sequence = None;
        for event in &self.override_events {
            if previous_sequence.is_some_and(|prior| prior >= event.sequence)
                || event.sequence == 0
                || event.sequence >= self.next_override_sequence
                || event.side_index >= side_count
                || event.started_tick > tick
                || event
                    .expires_tick
                    .is_some_and(|expires| expires < event.started_tick)
            {
                return Err(OperationalError::Invalid("override history"));
            }
            previous_sequence = Some(event.sequence);
        }
        if self.next_override_sequence == 0 {
            return Err(OperationalError::Invalid("override sequence"));
        }
        Ok(())
    }

    pub fn pre_tick(&mut self, tick: u64) {
        for side in &mut self.sides {
            side.intel.contacts.retain_mut(|contact| {
                let age = tick.saturating_sub(contact.observed_tick);
                if age > side.intel.config.expire_ticks {
                    return false;
                }
                contact.age_ticks = age;
                contact.confidence =
                    decayed_confidence(contact.base_confidence, age, side.intel.config);
                contact.status = if age <= side.intel.config.fresh_ticks {
                    IntelStatus::Fresh
                } else if age <= side.intel.config.stale_ticks {
                    IntelStatus::Stale
                } else {
                    IntelStatus::Degraded
                };
                true
            });
            side.intel
                .contacts
                .sort_by(|left, right| left.key.cmp(&right.key));
            side.intel.revision = side.intel.revision.saturating_add(1);
        }
    }

    pub fn known_hostile_strength(&self, observer_side: usize) -> Option<f64> {
        let side = self.sides.get(observer_side)?;
        if side.side_index != observer_side || side.hostile_side_indices.is_empty() {
            return None;
        }
        let mut total = 0.0;
        for &enemy in &side.hostile_side_indices {
            let prewar = side
                .intel
                .prewar_enemy_power
                .iter()
                .find(|record| record.side_index == enemy)
                .map_or(0.0, |record| record.power);
            let mut observed = 0.0;
            let mut contacts = 0;
            for contact in &side.intel.contacts {
                if contact.enemy_side_index == enemy {
                    observed += contact.observed_power * (0.6 + contact.confidence * 0.4);
                    contacts += 1;
                }
            }
            total += if contacts > 0 {
                observed.max(prewar * 0.4)
            } else {
                prewar
            };
        }
        Some(total.max(0.0))
    }

    pub fn posture_override(&self, side_index: usize) -> Option<WarPosture> {
        self.sides
            .get(side_index)
            .and_then(|side| side.r#override.as_ref())
            .map(|value| value.posture.into())
    }

    pub fn ingest_tactical_contacts(
        &mut self,
        tick: u64,
        observations: &[TacticalContactObservation],
    ) {
        let mut touched = BTreeSet::new();
        for observation in observations {
            let Some(side) = self.sides.get_mut(observation.observer_side) else {
                continue;
            };
            if !side.hostile_side_indices.contains(&observation.enemy_side)
                || !observation.target_position.validate()
                || !finite_non_negative(observation.observed_power)
            {
                continue;
            }
            let key = format!(
                "native:{}:{}",
                observation.enemy_side, observation.target_unit_id
            );
            let previous = side
                .intel
                .contacts
                .iter()
                .find(|contact| contact.key == key)
                .cloned();
            let elapsed = previous.as_ref().map_or(1, |contact| {
                tick.saturating_sub(contact.observed_tick).max(1)
            }) as f64;
            let velocity_lat = previous.as_ref().map_or(0.0, |contact| {
                (observation.target_position.lat - contact.lat) / elapsed
            });
            let velocity_lng = previous.as_ref().map_or(0.0, |contact| {
                wrapped_longitude_delta(contact.lng, observation.target_position.lng) / elapsed
            });
            side.intel.contacts.retain(|contact| contact.key != key);
            side.intel.contacts.push(OperationalContact {
                key,
                enemy_side_index: observation.enemy_side,
                sector_id: "native-local".to_owned(),
                unit_id: observation.target_unit_id,
                country_id: Some(observation.target_country_id),
                domain: "LAND".to_owned(),
                kind: observation.kind.clone(),
                lat: observation.target_position.lat,
                lng: observation.target_position.lng,
                velocity_lat,
                velocity_lng,
                observed_power: observation.observed_power,
                base_confidence: 0.88,
                confidence: 0.88,
                observed_tick: tick,
                age_ticks: 0,
                status: IntelStatus::Fresh,
                source: "native-tactical".to_owned(),
            });
            side.intel.last_scan_tick = tick;
            touched.insert(observation.observer_side);
        }
        for side_index in touched {
            let side = &mut self.sides[side_index];
            side.intel
                .contacts
                .sort_by(|left, right| left.key.cmp(&right.key));
            side.intel.revision = side.intel.revision.saturating_add(1);
        }
    }

    pub fn task_force_key_by_unit(&self) -> BTreeMap<u64, u64> {
        let mut result = BTreeMap::new();
        for task_force in &self.task_forces {
            let key = stable_task_force_key(&task_force.id);
            for member in &task_force.members {
                result.insert(member.unit_id, key);
            }
        }
        result
    }

    pub fn contains_unit(&self, unit_id: u64) -> bool {
        self.task_forces.iter().any(|task_force| {
            task_force
                .members
                .iter()
                .any(|member| member.unit_id == unit_id)
        })
    }

    pub fn advance_task_forces(
        &mut self,
        tick: u64,
        units: &[OperationalUnitInput],
        collapsing_sides: &BTreeSet<usize>,
    ) {
        let by_id = self.retain_live_unit_references(units);
        for task_force in &mut self.task_forces {
            if task_force.members.is_empty() {
                transition_task_force(task_force, TaskForcePhase::Complete, tick);
                task_force.completion_reason = Some("NO_LIVE_MEMBERS".to_owned());
                task_force.outcome = Some("NO_LIVE_MEMBERS".to_owned());
                continue;
            }
            task_force.current_power = task_force
                .members
                .iter()
                .filter_map(|member| by_id.get(&member.unit_id))
                .map(|unit| unit.power.max(0.0))
                .sum();
            task_force.peak_power = task_force.peak_power.max(task_force.current_power);
            task_force.readiness = task_force
                .members
                .iter()
                .filter_map(|member| by_id.get(&member.unit_id))
                .map(|unit| unit.readiness.clamp(0.0, 1.0))
                .sum::<f64>()
                / task_force.members.len() as f64;

            let collapsed = task_force
                .members
                .iter()
                .filter(|member| {
                    by_id
                        .get(&member.unit_id)
                        .and_then(|unit| unit.supply_collapsed_tick)
                        .is_some_and(|collapse_tick| {
                            collapse_tick <= tick
                                && tick - collapse_tick <= SUPPLY_COLLAPSE_MEMORY_TICKS
                        })
                })
                .count();
            let severe_encirclement = task_force
                .members
                .iter()
                .filter(|member| {
                    by_id
                        .get(&member.unit_id)
                        .is_some_and(|unit| unit.encircled_ticks >= 60)
                })
                .count();
            let formation_threshold =
                (task_force.members.len() as f64 * SUPPLY_COLLAPSE_MEMBER_SHARE).ceil() as usize;
            if task_force.phase == TaskForcePhase::Regrouping
                && collapsing_sides.contains(&task_force.side_index)
            {
                task_force.plan_type = "DEFEND".to_owned();
                task_force.completion_reason = Some("COLLAPSING_DEFENSE".to_owned());
                task_force.outcome = None;
                for member in &mut task_force.members {
                    member.route_progress =
                        role_route_progress(task_force.phase, member.role, task_force.progress);
                }
                continue;
            }
            match task_force.phase {
                TaskForcePhase::Assembling => {
                    let launch_readiness = match task_force.posture {
                        TaskForcePosture::Aggressive => 0.65,
                        TaskForcePosture::Balanced => 0.75,
                        TaskForcePosture::Defensive => 0.85,
                    };
                    if task_force.readiness >= launch_readiness {
                        transition_task_force(task_force, TaskForcePhase::Attacking, tick);
                        task_force.launch_power = task_force.current_power.max(0.000_1);
                        task_force.completion_reason = None;
                    }
                }
                TaskForcePhase::Attacking => {
                    let power_ratio =
                        task_force.current_power / task_force.launch_power.max(0.000_1);
                    if task_force.progress >= 1.0 {
                        transition_task_force(task_force, TaskForcePhase::Consolidating, tick);
                        task_force.completion_reason = None;
                    } else {
                        let reason = if task_force.severe_surprise {
                            Some("SEVERE_SURPRISE")
                        } else if collapsed >= formation_threshold {
                            Some("SUPPLY_COLLAPSE")
                        } else if severe_encirclement >= formation_threshold {
                            Some("ENCIRCLEMENT_RISK")
                        } else if power_ratio < 0.55 {
                            Some("POWER_LOSS")
                        } else {
                            None
                        };
                        if let Some(reason) = reason {
                            transition_task_force(task_force, TaskForcePhase::Culminated, tick);
                            task_force.completion_reason = Some(reason.to_owned());
                            if reason == "SUPPLY_COLLAPSE" {
                                task_force.supply_invalidated_tick = Some(tick);
                                task_force.intent_revision =
                                    task_force.intent_revision.saturating_add(1);
                            }
                        }
                    }
                }
                TaskForcePhase::Culminated if tick > task_force.phase_started_tick => {
                    transition_task_force(task_force, TaskForcePhase::Withdrawing, tick);
                }
                TaskForcePhase::Withdrawing => {
                    let anchor = task_force.withdrawal_anchor.or(task_force.staging_anchor);
                    if let Some(anchor) = anchor {
                        let arrived = task_force
                            .members
                            .iter()
                            .filter(|member| {
                                by_id
                                    .get(&member.unit_id)
                                    .is_some_and(|unit| distance_sq(unit.position, anchor) <= 1.0)
                            })
                            .count();
                        if arrived * 100 >= task_force.members.len() * 65 {
                            transition_task_force(task_force, TaskForcePhase::Regrouping, tick);
                            task_force.last_recovery_tick = tick;
                            task_force.recovery_power = task_force.current_power;
                            task_force.completion_reason = None;
                        }
                    }
                }
                TaskForcePhase::Regrouping => {
                    let baseline = task_force
                        .launch_power
                        .max(task_force.peak_power)
                        .max(0.000_1);
                    if task_force.current_power >= task_force.recovery_power + baseline * 0.02 {
                        task_force.recovery_power = task_force.current_power;
                        task_force.last_recovery_tick = tick;
                    }
                    if task_force.current_power / baseline >= 0.7
                        || tick.saturating_sub(task_force.last_recovery_tick) >= 1_200
                    {
                        transition_task_force(task_force, TaskForcePhase::Complete, tick);
                        let outcome = if task_force.current_power / baseline >= 0.7 {
                            "REGROUPED"
                        } else {
                            "REGROUP_PLATEAU"
                        };
                        task_force.completion_reason = Some(outcome.to_owned());
                        task_force.outcome = Some(outcome.to_owned());
                    }
                }
                TaskForcePhase::Consolidating
                    if tick.saturating_sub(task_force.phase_started_tick) >= 300 =>
                {
                    transition_task_force(task_force, TaskForcePhase::Complete, tick);
                    task_force.completion_reason = Some("OBJECTIVE_SECURED".to_owned());
                    task_force.outcome = Some("OBJECTIVE_SECURED".to_owned());
                }
                TaskForcePhase::Culminated
                | TaskForcePhase::Consolidating
                | TaskForcePhase::Complete => {}
            }
            for member in &mut task_force.members {
                member.route_progress =
                    role_route_progress(task_force.phase, member.role, task_force.progress);
            }
        }
        self.task_forces
            .retain(|task_force| task_force.phase != TaskForcePhase::Complete);
    }

    pub fn steering(&self, units: &[OperationalUnitInput]) -> Vec<OperationalSteering> {
        let by_id = units
            .iter()
            .map(|unit| (unit.unit_id, *unit))
            .collect::<BTreeMap<_, _>>();
        let mut result = Vec::new();
        for task_force in &self.task_forces {
            for member in &task_force.members {
                let Some(unit) = by_id.get(&member.unit_id) else {
                    continue;
                };
                let (target, speed_multiplier, movement_enabled) = match task_force.phase {
                    TaskForcePhase::Assembling => (
                        task_force.staging_anchor.or(task_force.target),
                        if member.role == TaskForceRole::Reserve {
                            1.05
                        } else {
                            1.55
                        },
                        true,
                    ),
                    TaskForcePhase::Attacking => (
                        corridor_point(task_force, member.route_progress),
                        match member.role {
                            TaskForceRole::Spearhead => 2.15,
                            TaskForceRole::Line => 1.65,
                            TaskForceRole::Support => 1.4,
                            TaskForceRole::Reserve if member.route_progress > 0.0 => 1.3,
                            TaskForceRole::Reserve => 0.35,
                        },
                        true,
                    ),
                    TaskForcePhase::Consolidating => (task_force.target, 0.8, true),
                    TaskForcePhase::Culminated => (Some(unit.position), 0.25, false),
                    TaskForcePhase::Withdrawing => (
                        task_force.withdrawal_anchor.or(task_force.staging_anchor),
                        1.85,
                        true,
                    ),
                    TaskForcePhase::Regrouping => (
                        task_force.withdrawal_anchor.or(task_force.staging_anchor),
                        0.45,
                        true,
                    ),
                    TaskForcePhase::Complete => (None, 0.0, false),
                };
                let Some(target) = target else {
                    continue;
                };
                let (dir_lat, dir_lng) = normalized_direction(unit.position, target);
                result.push(OperationalSteering {
                    unit_id: member.unit_id,
                    task_force_id: task_force.id.clone(),
                    phase: task_force.phase,
                    role: member.role,
                    target,
                    dir_lat,
                    dir_lng,
                    speed_multiplier,
                    movement_enabled: movement_enabled && (dir_lat != 0.0 || dir_lng != 0.0),
                });
            }
        }
        result.sort_by_key(|steering| steering.unit_id);
        result
    }

    pub fn post_movement(&mut self, tick: u64, units: &[OperationalUnitInput]) {
        let by_id = self.retain_live_unit_references(units);
        for task_force in &mut self.task_forces {
            let corridor = corridor(task_force);
            let mut best_progress = task_force.progress;
            for member in &mut task_force.members {
                if let Some(unit) = by_id.get(&member.unit_id) {
                    let actual = progress_along_corridor(&corridor, unit.position);
                    member.route_progress = member.route_progress.max(actual).clamp(0.0, 1.0);
                    best_progress = best_progress.max(actual);
                }
            }
            if best_progress > task_force.progress + 0.02 {
                task_force.progress = best_progress.min(1.0);
                task_force.last_progress_tick = tick;
            }
        }
        self.task_forces
            .retain(|task_force| !task_force.members.is_empty());
    }

    fn retain_live_unit_references(
        &mut self,
        units: &[OperationalUnitInput],
    ) -> BTreeMap<u64, OperationalUnitInput> {
        let by_id = units
            .iter()
            .map(|unit| (unit.unit_id, *unit))
            .collect::<BTreeMap<_, _>>();
        for side in &mut self.sides {
            side.intel
                .contacts
                .retain(|contact| by_id.contains_key(&contact.unit_id));
        }
        for task_force in &mut self.task_forces {
            task_force.members.retain(|member| {
                by_id
                    .get(&member.unit_id)
                    .is_some_and(|unit| unit.side_index == task_force.side_index)
            });
            task_force.reserve_unit_ids.retain(|unit_id| {
                task_force
                    .members
                    .binary_search_by_key(unit_id, |member| member.unit_id)
                    .is_ok()
            });
        }
        by_id
    }

    pub fn update_country_desperation(
        &mut self,
        tick: u64,
        interval: u64,
        countries: &[CountryOperationalInput],
    ) {
        let by_id = countries
            .iter()
            .map(|country| (country.country_id, *country))
            .collect::<BTreeMap<_, _>>();
        for state in &mut self.country_desperation {
            let Some(input) = by_id.get(&state.country_id) else {
                continue;
            };
            let initial_land = input.initial_land.max(1);
            let initial_cities = *state.initial_cities.get_or_insert(input.cities_controlled);
            let initial_manpower = *state
                .initial_manpower
                .get_or_insert(input.current_personnel.max(0.0));
            let previous = state.previous_controlled.unwrap_or(input.controlled);
            let delta = input.controlled.abs_diff(previous);
            state.previous_controlled = Some(input.controlled);
            let stall_threshold = ((initial_land as f64 * 0.002).floor() as u64).max(1);
            if delta <= stall_threshold {
                state.stall_ticks = state.stall_ticks.saturating_add(interval);
            } else {
                state.stall_ticks = state.stall_ticks.saturating_sub(interval.saturating_mul(2));
            }
            let control_ratio = input.controlled as f64 / initial_land as f64;
            let city_ratio = input.cities_controlled as f64 / initial_cities.max(1) as f64;
            let mobilization_ratio = input.current_personnel.max(0.0) / initial_manpower.max(1.0);
            state.mode = if control_ratio <= 0.22 || city_ratio <= 0.2 {
                CountryDesperationMode::LastStand
            } else if control_ratio <= 0.4 || city_ratio <= 0.35 {
                CountryDesperationMode::DefensiveDesperation
            } else if input.offensive_role
                && tick >= 1_200
                && state.stall_ticks >= 900
                && control_ratio > 0.45
            {
                CountryDesperationMode::OffensiveDesperation
            } else if mobilization_ratio < 0.75 {
                CountryDesperationMode::UnderMobilized
            } else {
                CountryDesperationMode::Normal
            };
        }
    }

    pub fn evolve_overrides(&mut self, tick: u64, country_to_side: &BTreeMap<u16, usize>) {
        for side_index in 0..self.sides.len() {
            let modes = self
                .country_desperation
                .iter()
                .filter(|country| country_to_side.get(&country.country_id) == Some(&side_index))
                .map(|country| country.mode)
                .collect::<Vec<_>>();
            let recent_supply_collapse = self.task_forces.iter().any(|task_force| {
                task_force.side_index == side_index
                    && task_force.supply_invalidated_tick.is_some_and(|collapse| {
                        tick.saturating_sub(collapse) <= SUPPLY_OVERRIDE_TICKS
                    })
            });
            let viable_offensive = self.task_forces.iter().any(|task_force| {
                task_force.side_index == side_index
                    && matches!(
                        task_force.phase,
                        TaskForcePhase::Assembling | TaskForcePhase::Attacking
                    )
            });
            let desired = if modes.contains(&CountryDesperationMode::LastStand) {
                Some((
                    OperationalPosture::Defensive,
                    OperationalOverrideSource::LastStand,
                    None,
                ))
            } else if recent_supply_collapse {
                Some((
                    OperationalPosture::Defensive,
                    OperationalOverrideSource::SupplyCollapse,
                    Some(tick.saturating_add(SUPPLY_OVERRIDE_TICKS)),
                ))
            } else if modes.contains(&CountryDesperationMode::DefensiveDesperation) {
                Some((
                    OperationalPosture::Defensive,
                    OperationalOverrideSource::DefensiveDesperation,
                    None,
                ))
            } else if viable_offensive
                && modes.contains(&CountryDesperationMode::OffensiveDesperation)
            {
                Some((
                    OperationalPosture::Offensive,
                    OperationalOverrideSource::OffensiveDesperation,
                    None,
                ))
            } else {
                None
            };
            let current = self.sides[side_index].r#override.clone();
            let unchanged = current
                .as_ref()
                .zip(desired)
                .is_some_and(|(current, desired)| {
                    current.posture == desired.0 && current.source == desired.1
                })
                || (current.is_none() && desired.is_none());
            if unchanged {
                continue;
            }
            let sequence = self.next_override_sequence;
            self.next_override_sequence = self.next_override_sequence.saturating_add(1);
            let (_posture, source, expires_tick) = desired.unwrap_or_else(|| {
                let current = current
                    .as_ref()
                    .expect("changed empty override has prior value");
                (current.posture, current.source, Some(tick))
            });
            self.override_events.push(OperationalOverrideEvent {
                sequence,
                side_index,
                posture: desired.map(|value| value.0),
                source,
                started_tick: tick,
                expires_tick,
            });
            self.sides[side_index].r#override = desired.map(|value| OperationalOverride {
                posture: value.0,
                source: value.1,
                started_tick: tick,
                expires_tick: value.2,
                sequence,
            });
        }
        if self.override_events.len() > OPERATIONAL_OVERRIDE_HISTORY_LIMIT {
            let remove = self.override_events.len() - OPERATIONAL_OVERRIDE_HISTORY_LIMIT;
            self.override_events.drain(..remove);
        }
    }

    pub fn snapshot(&self, tick: u64) -> OperationalSnapshot {
        OperationalSnapshot {
            schema_version: OPERATIONAL_AI_SCHEMA_VERSION,
            tick,
            sides: self.sides.clone(),
            task_forces: self.task_forces.clone(),
            country_desperation: self.country_desperation.clone(),
            override_events: self.override_events.clone(),
        }
    }
}

fn finite_non_negative(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

fn stable_task_force_key(id: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn strictly_sorted_unique<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn decayed_confidence(base: f64, age: u64, config: OperationalIntelConfig) -> f64 {
    let age = age as f64;
    let confidence = if age <= config.fresh_ticks as f64 {
        base * (1.0 - 0.15 * age / config.fresh_ticks as f64)
    } else if age <= config.stale_ticks as f64 {
        let progress = (age - config.fresh_ticks as f64)
            / (config.stale_ticks - config.fresh_ticks).max(1) as f64;
        base * (0.85 - progress * 0.5)
    } else {
        let progress = (age - config.stale_ticks as f64)
            / (config.expire_ticks - config.stale_ticks).max(1) as f64;
        base * (0.35 - progress * 0.3)
    };
    confidence.clamp(0.0, 1.0)
}

fn transition_task_force(task_force: &mut OperationalTaskForce, phase: TaskForcePhase, tick: u64) {
    task_force.phase = phase;
    task_force.phase_started_tick = tick;
    task_force.last_progress_tick = tick;
}

fn role_route_progress(phase: TaskForcePhase, role: TaskForceRole, progress: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    if phase != TaskForcePhase::Attacking {
        return progress;
    }
    match role {
        TaskForceRole::Spearhead => (0.35 + progress * 0.75).min(1.0),
        TaskForceRole::Line => (0.2 + progress * 0.85).min(1.0),
        TaskForceRole::Support => (0.1 + progress * 0.58).min(0.72),
        TaskForceRole::Reserve if progress >= 0.4 => (progress * 0.5).min(0.45),
        TaskForceRole::Reserve => 0.0,
    }
}

fn corridor(task_force: &OperationalTaskForce) -> Vec<OperationalPoint> {
    let mut points = Vec::new();
    if let Some(point) = task_force.staging_anchor {
        points.push(point);
    }
    points.extend(task_force.route.iter().copied());
    if let Some(point) = task_force.target {
        points.push(point);
    }
    points.dedup();
    points
}

fn corridor_point(task_force: &OperationalTaskForce, progress: f64) -> Option<OperationalPoint> {
    let points = corridor(task_force);
    match points.as_slice() {
        [] => None,
        [point] => Some(*point),
        _ => {
            let scaled = progress.clamp(0.0, 1.0) * (points.len() - 1) as f64;
            let segment = (scaled.floor() as usize).min(points.len() - 2);
            let fraction = scaled - segment as f64;
            let left = points[segment];
            let right = points[segment + 1];
            Some(OperationalPoint {
                lat: left.lat + (right.lat - left.lat) * fraction,
                lng: left.lng + wrapped_longitude_delta(left.lng, right.lng) * fraction,
            })
        }
    }
}

fn progress_along_corridor(points: &[OperationalPoint], position: OperationalPoint) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    let mut best = (f64::INFINITY, 0.0);
    for segment in 0..points.len() - 1 {
        let left = points[segment];
        let right = points[segment + 1];
        let delta_lat = right.lat - left.lat;
        let delta_lng = wrapped_longitude_delta(left.lng, right.lng);
        let from_left_lat = position.lat - left.lat;
        let from_left_lng = wrapped_longitude_delta(left.lng, position.lng);
        let denominator = delta_lat * delta_lat + delta_lng * delta_lng;
        let fraction = if denominator > 0.0 {
            ((from_left_lat * delta_lat + from_left_lng * delta_lng) / denominator).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let projected = OperationalPoint {
            lat: left.lat + delta_lat * fraction,
            lng: left.lng + delta_lng * fraction,
        };
        let distance = distance_sq(position, projected);
        let progress = (segment as f64 + fraction) / (points.len() - 1) as f64;
        if distance < best.0 || (distance == best.0 && progress > best.1) {
            best = (distance, progress);
        }
    }
    best.1
}

fn normalized_direction(from: OperationalPoint, to: OperationalPoint) -> (f64, f64) {
    let lat = to.lat - from.lat;
    let lng = wrapped_longitude_delta(from.lng, to.lng);
    let magnitude = lat.hypot(lng);
    if magnitude > 0.0 && magnitude.is_finite() {
        (lat / magnitude, lng / magnitude)
    } else {
        (0.0, 0.0)
    }
}

fn distance_sq(left: OperationalPoint, right: OperationalPoint) -> f64 {
    let lat = right.lat - left.lat;
    let lng = wrapped_longitude_delta(left.lng, right.lng);
    lat * lat + lng * lng
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(lat: f64, lng: f64) -> OperationalPoint {
        OperationalPoint { lat, lng }
    }

    fn units() -> BTreeMap<u64, usize> {
        BTreeMap::from([(1, 0), (2, 0), (3, 0), (9, 1)])
    }

    fn countries() -> BTreeSet<u16> {
        BTreeSet::from([10, 20])
    }

    fn task_force() -> OperationalTaskForce {
        OperationalTaskForce {
            id: "tf-1".to_owned(),
            signature: "plan:1".to_owned(),
            side_index: 0,
            plan_signature: "plan:1".to_owned(),
            plan_type: "PUSH_FRONT".to_owned(),
            theater_id: None,
            target: Some(point(0.0, 10.0)),
            staging_anchor: Some(point(0.0, 0.0)),
            route: Vec::new(),
            phase: TaskForcePhase::Attacking,
            posture: TaskForcePosture::Balanced,
            members: (1..=3)
                .map(|unit_id| TaskForceMember {
                    unit_id,
                    role: TaskForceRole::Line,
                    assigned_tick: 0,
                    route_progress: 0.2,
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
            withdrawal_anchor: Some(point(0.0, 0.0)),
            completion_reason: None,
            outcome: None,
            severe_surprise: false,
            parent_task_force_id: None,
            supply_invalidated_tick: None,
            intent_revision: 0,
        }
    }

    fn state() -> OperationalRuntimeState {
        let mut state = OperationalRuntimeState::bootstrap(2, &[0, 1, 1, 0], &[3.0, 10.0]);
        state.task_forces.push(task_force());
        state.country_desperation = vec![
            CountryDesperationState {
                country_id: 10,
                mode: CountryDesperationMode::Normal,
                initial_cities: Some(2),
                initial_manpower: Some(100.0),
                previous_controlled: Some(100),
                stall_ticks: 0,
            },
            CountryDesperationState {
                country_id: 20,
                mode: CountryDesperationMode::Normal,
                initial_cities: Some(2),
                initial_manpower: Some(100.0),
                previous_controlled: Some(100),
                stall_ticks: 0,
            },
        ];
        state
    }

    #[test]
    fn intel_decay_and_known_power_match_browser_fallback() {
        let mut state = state();
        state.ingest_tactical_contacts(
            10,
            &[TacticalContactObservation {
                observer_side: 0,
                enemy_side: 1,
                target_unit_id: 9,
                target_country_id: 20,
                target_position: point(1.0, 2.0),
                observed_power: 4.0,
                kind: "army".to_owned(),
            }],
        );
        assert_eq!(state.known_hostile_strength(0), Some(4.0));
        state.pre_tick(311);
        assert_eq!(state.sides[0].intel.contacts[0].status, IntelStatus::Stale);
        state.pre_tick(1_811);
        assert!(state.sides[0].intel.contacts.is_empty());
        assert_eq!(state.known_hostile_strength(0), Some(10.0));
    }

    #[test]
    fn planning_prunes_contacts_for_units_destroyed_before_ai_resolution() {
        let mut state = state();
        state.ingest_tactical_contacts(
            10,
            &[TacticalContactObservation {
                observer_side: 0,
                enemy_side: 1,
                target_unit_id: 9,
                target_country_id: 20,
                target_position: point(1.0, 2.0),
                observed_power: 4.0,
                kind: "army".to_owned(),
            }],
        );
        state.pre_tick(11);

        let surviving_units = [1, 2, 3].map(|unit_id| OperationalUnitInput {
            unit_id,
            side_index: 0,
            country_id: 10,
            position: point(0.0, unit_id as f64),
            power: 1.0,
            readiness: 1.0,
            supply_collapsed_tick: None,
            encircled_ticks: 0,
        });
        state.advance_task_forces(11, &surviving_units, &BTreeSet::new());

        assert!(state.sides[0].intel.contacts.is_empty());
        let live_units = BTreeMap::from([(1, 0), (2, 0), (3, 0)]);
        assert!(state.validate(2, &live_units, &countries(), 11).is_ok());
    }

    #[test]
    fn membership_is_sticky_and_supply_collapse_invalidates_only_its_force() {
        let mut state = state();
        let inputs = [
            OperationalUnitInput {
                unit_id: 1,
                side_index: 0,
                country_id: 10,
                position: point(0.0, 1.0),
                power: 1.0,
                readiness: 1.0,
                supply_collapsed_tick: Some(1),
                encircled_ticks: 0,
            },
            OperationalUnitInput {
                unit_id: 2,
                side_index: 0,
                country_id: 10,
                position: point(0.0, 2.0),
                power: 1.0,
                readiness: 1.0,
                supply_collapsed_tick: Some(1),
                encircled_ticks: 0,
            },
            OperationalUnitInput {
                unit_id: 3,
                side_index: 0,
                country_id: 10,
                position: point(0.0, 3.0),
                power: 1.0,
                readiness: 1.0,
                supply_collapsed_tick: None,
                encircled_ticks: 0,
            },
        ];
        state.advance_task_forces(1, &inputs, &BTreeSet::new());
        let task_force = &state.task_forces[0];
        assert_eq!(task_force.phase, TaskForcePhase::Culminated);
        assert_eq!(task_force.supply_invalidated_tick, Some(1));
        assert_eq!(
            task_force
                .members
                .iter()
                .map(|member| member.unit_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn supply_collapse_window_is_inclusive_and_expires_after_fifteen_ticks() {
        let inputs = [1, 2, 3].map(|unit_id| OperationalUnitInput {
            unit_id,
            side_index: 0,
            country_id: 10,
            position: point(0.0, unit_id as f64),
            power: 1.0,
            readiness: 1.0,
            supply_collapsed_tick: (unit_id <= 2).then_some(1),
            encircled_ticks: 0,
        });

        let mut inclusive = state();
        inclusive.advance_task_forces(16, &inputs, &BTreeSet::new());
        assert_eq!(
            inclusive.task_forces[0].completion_reason.as_deref(),
            Some("SUPPLY_COLLAPSE")
        );

        let mut expired = state();
        expired.advance_task_forces(17, &inputs, &BTreeSet::new());
        assert_eq!(expired.task_forces[0].phase, TaskForcePhase::Attacking);
        assert_eq!(expired.task_forces[0].supply_invalidated_tick, None);
    }

    #[test]
    fn collapsing_side_holds_regrouping_force_as_defense() {
        let mut state = state();
        state.task_forces[0].phase = TaskForcePhase::Regrouping;
        let inputs = [1, 2, 3].map(|unit_id| OperationalUnitInput {
            unit_id,
            side_index: 0,
            country_id: 10,
            position: point(0.0, unit_id as f64),
            power: 1.0,
            readiness: 1.0,
            supply_collapsed_tick: None,
            encircled_ticks: 0,
        });

        state.advance_task_forces(1, &inputs, &BTreeSet::from([0]));

        let task_force = &state.task_forces[0];
        assert_eq!(task_force.phase, TaskForcePhase::Regrouping);
        assert_eq!(task_force.plan_type, "DEFEND");
        assert_eq!(
            task_force.completion_reason.as_deref(),
            Some("COLLAPSING_DEFENSE")
        );
        assert_eq!(task_force.outcome, None);
    }

    #[test]
    fn steering_uses_role_progress_and_post_movement_advances_route() {
        let mut state = state();
        let inputs = [OperationalUnitInput {
            unit_id: 1,
            side_index: 0,
            country_id: 10,
            position: point(0.0, 1.0),
            power: 1.0,
            readiness: 1.0,
            supply_collapsed_tick: None,
            encircled_ticks: 0,
        }];
        let steering = state.steering(&inputs);
        assert_eq!(steering.len(), 1);
        assert!(steering[0].dir_lng > 0.0);
        state.post_movement(1, &inputs);
        assert!(state.task_forces[0].progress > 0.0);
    }

    #[test]
    fn task_force_repulsion_key_survives_other_force_removal() {
        let mut state = state();
        let mut second = state.task_forces[0].clone();
        second.id = "tf-2".to_owned();
        second.signature = "tf-2".to_owned();
        second.side_index = 1;
        second.members[0].unit_id = 2;
        state.task_forces.push(second);
        let before = state.task_force_key_by_unit()[&2];

        state.task_forces.remove(0);

        assert_eq!(state.task_force_key_by_unit()[&2], before);
        assert_eq!(before, stable_task_force_key("tf-2"));
    }

    #[test]
    fn last_stand_override_wins_and_validation_rejects_cross_side_membership() {
        let mut state = state();
        state.country_desperation[0].mode = CountryDesperationMode::LastStand;
        state.evolve_overrides(1, &BTreeMap::from([(10, 0), (20, 1)]));
        assert_eq!(state.posture_override(0), Some(WarPosture::Defensive));
        assert!(state.validate(2, &units(), &countries(), 1).is_ok());
        state.task_forces[0].members[0].unit_id = 9;
        assert_eq!(
            state.validate(2, &units(), &countries(), 1),
            Err(OperationalError::Invalid("task force member"))
        );
    }
}
