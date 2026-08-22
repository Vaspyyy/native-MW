//! Deterministic air-operation state and mission execution.
//!
//! The browser simulation treats aircraft as persistent markers: every six
//! simulation ticks they move, while mission selection is staggered over a
//! 120-tick cycle. This module keeps that state independent from rendering and
//! returns land damage for the owning runtime to commit atomically.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::economy::CommandBand;

pub const AIR_SCHEMA_VERSION: &str = "native-air-v2";
pub const AIR_TICK_INTERVAL: u64 = 6;
pub const AIR_MISSION_INTERVAL: u64 = 120;
pub const FIGHTER_RANGE_KM: f64 = 800.0;
pub const STRIKE_RANGE_KM: f64 = 1_200.0;
pub const FERRY_RANGE_KM: f64 = STRIKE_RANGE_KM * 2.0;
pub const FIGHTER_ENDURANCE_TICKS: u64 = 600;
pub const FIGHTER_REARM_TICKS: u64 = 180;
pub const STRIKE_COOLDOWN_TICKS: u64 = 600;
pub const STRIKE_REARM_TICKS: u64 = 300;

const FIGHTER_SPEED_DEGREES: f64 = 0.1;
const STRIKE_SPEED_DEGREES: f64 = 0.075;
const RETURN_SPEED_DEGREES: f64 = 0.08;
const DEFAULT_PRIORITY_RADIUS_KM: f64 = 300.0;
const EARTH_RADIUS_KM: f64 = 6_371.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Airfield {
    pub id: u64,
    pub side: usize,
    pub owner_country_id: u16,
    pub controller_country_id: u16,
    pub lat: f64,
    pub lng: f64,
    /// Maximum wing-marker capacity at full health.
    pub capacity: u32,
    pub health: f64,
    pub disabled: bool,
    pub capture_repair_cycles: u64,
    pub capital: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AirRole {
    Fighter,
    Strike,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AirWingState {
    Grounded,
    Patrol,
    Intercept,
    Attacking,
    Returning,
    Rearming,
    Evacuated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AirTargetKind {
    Army,
    Armor,
    Airfield,
    AirWing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirWing {
    pub id: u64,
    pub side: usize,
    pub sovereign_country_id: u16,
    /// The field whose capacity this wing occupies until it reaches another.
    pub airfield_id: u64,
    #[serde(deserialize_with = "required_option")]
    pub return_airfield_id: Option<u64>,
    pub role: AirRole,
    pub quality: f64,
    pub max_count: u32,
    pub count: u32,
    pub lat: f64,
    pub lng: f64,
    pub state: AirWingState,
    #[serde(deserialize_with = "required_option")]
    pub target_kind: Option<AirTargetKind>,
    #[serde(deserialize_with = "required_option")]
    pub target_id: Option<u64>,
    /// Remaining timers are decremented only on an air tick, matching browser
    /// pause/continuation behavior even if simulation ticks are skipped.
    pub rearm_ticks: u64,
    pub cooldown_ticks: u64,
    pub endurance_ticks: u64,
    #[serde(deserialize_with = "required_option")]
    pub next_mission_tick: Option<u64>,
    pub force_mission: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirCountryCoverage {
    pub country_id: u16,
    pub operations_coverage: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirPowerState {
    pub schema: String,
    /// Persistent funding policy consumed by every mission decision. Entries
    /// are strictly ordered by country id so checkpoint continuation is
    /// deterministic and missing policy cannot silently become fully funded.
    pub country_coverage: Vec<AirCountryCoverage>,
    pub airfields: Vec<Airfield>,
    pub wings: Vec<AirWing>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirUnitTarget {
    pub id: u64,
    pub side: usize,
    pub country_id: u16,
    pub lat: f64,
    pub lng: f64,
    pub kind: AirTargetKind,
    /// Armor equipment for armor targets and local cluster size for armies.
    pub strength: f64,
    /// Allows callers with an already-resolved operation sector to avoid
    /// rebuilding the area list. Explicit priority never widens validity.
    pub priority_area: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirPriorityArea {
    pub side: usize,
    pub lat: f64,
    pub lng: f64,
    pub radius_km: f64,
}

impl AirPriorityArea {
    pub const fn with_default_radius(side: usize, lat: f64, lng: f64) -> Self {
        Self {
            side,
            lat,
            lng,
            radius_km: DEFAULT_PRIORITY_RADIUS_KM,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirfieldController {
    pub side: usize,
    pub controller_country_id: u16,
}

/// Borrowed, coherent inputs for one native air execution step.
#[derive(Clone, Copy, Debug)]
pub struct AirWorldInput<'a> {
    pub tick: u64,
    pub side_count: usize,
    /// Row-major directed hostility matrix. Values must be zero or one.
    pub hostility: &'a [u8],
    pub command_bands: &'a BTreeMap<u16, CommandBand>,
    /// Authoritative persisted funding coverage. Every live wing country must
    /// be present; callers may override persisted values only for same-tick
    /// policy such as capitulation.
    pub air_operations_coverage: &'a BTreeMap<u16, f64>,
    pub targets: &'a [AirUnitTarget],
    pub priority_areas: &'a [AirPriorityArea],
    /// Sparse authoritative updates. A side change captures and disables the
    /// field; an update on the same side only changes its controller.
    pub airfield_controllers: &'a BTreeMap<u64, AirfieldController>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirfieldCaptureOutcome {
    pub airfield_id: u64,
    pub previous_side: usize,
    pub side: usize,
    pub previous_controller_country_id: u16,
    pub controller_country_id: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirAdvanceOutcome {
    pub tick: u64,
    pub updated: bool,
    /// `(target unit id, attacker country id, requested damage)`.
    pub land_damage: Vec<(u64, u16, f64)>,
    /// `(target airfield id, attacker country id, requested damage)`.
    pub airfield_damage: Vec<(u64, u16, f64)>,
    /// `(target wing id, attacker country id, actual aircraft lost)`.
    pub wing_losses: Vec<(u64, u16, u32)>,
    /// Land units hit this step, sorted and deduplicated for reaction planning.
    pub defender_reactions: Vec<u64>,
    pub airfield_captures: Vec<AirfieldCaptureOutcome>,
    pub missions_selected: u32,
    pub interceptions_completed: u32,
    pub strikes_completed: u32,
    pub wings_destroyed: u32,
}

impl AirAdvanceOutcome {
    fn new(tick: u64) -> Self {
        Self {
            tick,
            updated: false,
            land_damage: Vec::new(),
            airfield_damage: Vec::new(),
            wing_losses: Vec::new(),
            defender_reactions: Vec::new(),
            airfield_captures: Vec::new(),
            missions_selected: 0,
            interceptions_completed: 0,
            strikes_completed: 0,
            wings_destroyed: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AirError {
    #[error("invalid air state: {0}")]
    InvalidState(&'static str),
    #[error("invalid air world: {0}")]
    InvalidWorld(&'static str),
}

impl Default for AirPowerState {
    fn default() -> Self {
        Self::empty()
    }
}

impl AirPowerState {
    pub fn empty() -> Self {
        Self {
            schema: AIR_SCHEMA_VERSION.to_owned(),
            country_coverage: Vec::new(),
            airfields: Vec::new(),
            wings: Vec::new(),
        }
    }

    /// Construct canonical wire state. Deserialized checkpoints remain strict:
    /// callers must invoke `validate` before installing them.
    pub fn new(mut airfields: Vec<Airfield>, mut wings: Vec<AirWing>) -> Result<Self, AirError> {
        airfields.sort_by_key(|field| field.id);
        wings.sort_by_key(|wing| wing.id);
        let country_coverage = wings
            .iter()
            .map(|wing| wing.sovereign_country_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|country_id| AirCountryCoverage {
                country_id,
                operations_coverage: 1.0,
            })
            .collect();
        let state = Self {
            schema: AIR_SCHEMA_VERSION.to_owned(),
            country_coverage,
            airfields,
            wings,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), AirError> {
        if self.schema != AIR_SCHEMA_VERSION {
            return Err(AirError::InvalidState("schema"));
        }
        if !strictly_ordered(
            self.country_coverage
                .iter()
                .map(|coverage| u64::from(coverage.country_id)),
        ) || self.country_coverage.iter().any(|coverage| {
            !coverage.operations_coverage.is_finite()
                || !(0.0..=1.0).contains(&coverage.operations_coverage)
        }) {
            return Err(AirError::InvalidState(
                "country coverage must have unique ordered positive ids and finite coverage",
            ));
        }
        if !strictly_ordered(self.airfields.iter().map(|field| field.id)) {
            return Err(AirError::InvalidState(
                "airfields must have unique ordered ids",
            ));
        }
        if !strictly_ordered(self.wings.iter().map(|wing| wing.id)) {
            return Err(AirError::InvalidState("wings must have unique ordered ids"));
        }

        let field_ids = self
            .airfields
            .iter()
            .map(|field| field.id)
            .collect::<BTreeSet<_>>();
        for field in &self.airfields {
            if field.id == 0
                || field.owner_country_id == 0
                || field.controller_country_id == 0
                || !valid_position(field.lat, field.lng)
                || field.capacity == 0
                || !field.health.is_finite()
                || !(0.0..=100.0).contains(&field.health)
                || (field.health == 0.0 && !field.disabled)
            {
                return Err(AirError::InvalidState("airfield"));
            }
        }

        let wing_ids = self
            .wings
            .iter()
            .map(|wing| wing.id)
            .collect::<BTreeSet<_>>();
        let coverage_countries = self
            .country_coverage
            .iter()
            .map(|coverage| coverage.country_id)
            .collect::<BTreeSet<_>>();
        for wing in &self.wings {
            if wing.id == 0
                || wing.sovereign_country_id == 0
                || !coverage_countries.contains(&wing.sovereign_country_id)
                || wing.max_count == 0
                || wing.count == 0
                || wing.count > wing.max_count
                || !wing.quality.is_finite()
                || !(0.0..=100.0).contains(&wing.quality)
                || !valid_position(wing.lat, wing.lng)
                || !field_ids.contains(&wing.airfield_id)
                || wing
                    .return_airfield_id
                    .is_some_and(|field_id| !field_ids.contains(&field_id))
                || wing.endurance_ticks >= FIGHTER_ENDURANCE_TICKS
                || wing
                    .next_mission_tick
                    .is_some_and(|tick| tick % AIR_TICK_INTERVAL != 0)
                || wing.target_kind.is_some() != wing.target_id.is_some()
            {
                return Err(AirError::InvalidState("wing"));
            }

            match wing.state {
                AirWingState::Intercept => {
                    if wing.role != AirRole::Fighter
                        || wing.target_kind != Some(AirTargetKind::AirWing)
                        || wing.target_id == Some(wing.id)
                        || !wing.target_id.is_some_and(|id| wing_ids.contains(&id))
                        || wing.return_airfield_id.is_some()
                    {
                        return Err(AirError::InvalidState("fighter intercept"));
                    }
                }
                AirWingState::Attacking => {
                    if wing.role != AirRole::Strike
                        || !matches!(
                            wing.target_kind,
                            Some(
                                AirTargetKind::Army
                                    | AirTargetKind::Armor
                                    | AirTargetKind::Airfield
                            )
                        )
                        || wing.return_airfield_id.is_some()
                        || (wing.target_kind == Some(AirTargetKind::Airfield)
                            && !wing.target_id.is_some_and(|id| field_ids.contains(&id)))
                    {
                        return Err(AirError::InvalidState("strike attack"));
                    }
                }
                AirWingState::Returning => {
                    if wing.return_airfield_id.is_none()
                        || wing.target_kind.is_some()
                        || wing.target_id.is_some()
                    {
                        return Err(AirError::InvalidState("returning wing"));
                    }
                }
                _ => {
                    if wing.return_airfield_id.is_some()
                        || wing.target_kind.is_some()
                        || wing.target_id.is_some()
                    {
                        return Err(AirError::InvalidState("idle wing target"));
                    }
                }
            }
            if (wing.state == AirWingState::Rearming) != (wing.rearm_ticks > 0) {
                return Err(AirError::InvalidState("wing rearm timer"));
            }
        }
        Ok(())
    }

    /// Stage, validate, and atomically install one air step. Controller updates
    /// are applied on every call; missions and movement only run every six
    /// ticks.
    pub fn advance(&mut self, input: AirWorldInput<'_>) -> Result<AirAdvanceOutcome, AirError> {
        self.validate()?;
        validate_world(self, input)?;

        let mut next = self.clone();
        let mut outcome = AirAdvanceOutcome::new(input.tick);
        next.apply_airfield_controllers(input.airfield_controllers, &mut outcome);

        if input.tick.is_multiple_of(AIR_TICK_INTERVAL) {
            outcome.updated = true;
            next.advance_air_tick(input, &mut outcome);
        }
        if !outcome.airfield_captures.is_empty() {
            outcome.updated = true;
        }

        next.validate()?;
        *self = next;
        Ok(outcome)
    }

    fn apply_airfield_controllers(
        &mut self,
        controllers: &BTreeMap<u64, AirfieldController>,
        outcome: &mut AirAdvanceOutcome,
    ) {
        for field in &mut self.airfields {
            let Some(controller) = controllers.get(&field.id) else {
                continue;
            };
            let previous_side = field.side;
            let previous_controller = field.controller_country_id;
            if previous_side != controller.side {
                field.side = controller.side;
                field.controller_country_id = controller.controller_country_id;
                field.health = 0.0;
                field.disabled = true;
                field.capture_repair_cycles = 0;
                outcome.airfield_captures.push(AirfieldCaptureOutcome {
                    airfield_id: field.id,
                    previous_side,
                    side: controller.side,
                    previous_controller_country_id: previous_controller,
                    controller_country_id: controller.controller_country_id,
                });
            } else {
                field.controller_country_id = controller.controller_country_id;
            }
        }
    }

    fn advance_air_tick(&mut self, input: AirWorldInput<'_>, outcome: &mut AirAdvanceOutcome) {
        let mut mission_due = BTreeSet::new();
        for wing in &mut self.wings {
            if wing.count > 0
                && wing.state != AirWingState::Evacuated
                && wing_mission_is_due(wing, input.tick)
            {
                mission_due.insert(wing.id);
            }
        }

        // Browser candidates are snapshots built before any wing acts.
        let strike_candidates = self.strike_candidates(input.targets);
        let mut reactions = BTreeSet::new();

        for index in 0..self.wings.len() {
            if self.wings[index].count == 0 || self.wings[index].state == AirWingState::Evacuated {
                continue;
            }
            let mut wing = self.wings[index].clone();
            let field_index = self.field_index(wing.airfield_id);
            let field_operational = field_index
                .is_some_and(|field_index| self.field_is_operational_for(field_index, wing.side));

            if !field_operational {
                if let Some(replacement) = self.find_eligible_airfield(&wing) {
                    wing.return_airfield_id = Some(replacement);
                    clear_target(&mut wing);
                    wing.state = AirWingState::Returning;
                } else {
                    ground_wing(&mut wing);
                }
            }
            if wing.state == AirWingState::Grounded && !field_operational {
                self.wings[index] = wing;
                continue;
            }

            let band = input
                .command_bands
                .get(&wing.sovereign_country_id)
                .copied()
                .unwrap_or(CommandBand::Paid);
            let coverage = input
                .air_operations_coverage
                .get(&wing.sovereign_country_id)
                .copied()
                .expect("air world validation requires wing-country coverage");
            let policy = aircraft_policy(band);
            if policy.fighters_grounded || coverage < 0.25 {
                ground_wing(&mut wing);
                if field_operational {
                    let field = &self.airfields[field_index.expect("operational field")];
                    wing.lat = field.lat;
                    wing.lng = field.lng;
                }
                self.wings[index] = wing;
                continue;
            }

            if wing.role == AirRole::Strike
                && !policy.strikes
                && wing.state == AirWingState::Attacking
            {
                let airfield_id = wing.airfield_id;
                begin_return(&mut wing, airfield_id);
            }

            if wing.state == AirWingState::Rearming {
                wing.rearm_ticks = wing.rearm_ticks.saturating_sub(AIR_TICK_INTERVAL);
                if wing.rearm_ticks == 0 {
                    wing.state = match wing.role {
                        AirRole::Fighter => AirWingState::Patrol,
                        AirRole::Strike => AirWingState::Grounded,
                    };
                }
                self.wings[index] = wing;
                continue;
            }

            if wing.state == AirWingState::Returning {
                let Some(destination_id) = wing.return_airfield_id else {
                    ground_wing(&mut wing);
                    self.wings[index] = wing;
                    continue;
                };
                let Some(destination_index) = self.field_index(destination_id) else {
                    ground_wing(&mut wing);
                    self.wings[index] = wing;
                    continue;
                };
                let destination = &self.airfields[destination_index];
                if move_toward(
                    &mut wing,
                    destination.lat,
                    destination.lng,
                    RETURN_SPEED_DEGREES,
                ) {
                    wing.airfield_id = destination_id;
                    wing.return_airfield_id = None;
                    wing.state = AirWingState::Rearming;
                    wing.rearm_ticks = match wing.role {
                        AirRole::Fighter => FIGHTER_REARM_TICKS,
                        AirRole::Strike => STRIKE_REARM_TICKS,
                    };
                }
                self.wings[index] = wing;
                continue;
            }

            let should_select = mission_due.contains(&wing.id);
            match wing.role {
                AirRole::Fighter => self.advance_fighter(
                    &mut wing,
                    should_select,
                    field_index,
                    policy.home_defense,
                    input,
                    outcome,
                ),
                AirRole::Strike => self.advance_strike(
                    &mut wing,
                    should_select,
                    field_index,
                    policy.strikes,
                    coverage,
                    &strike_candidates,
                    input,
                    outcome,
                    &mut reactions,
                ),
            }
            self.wings[index] = wing;
        }

        // A wing can have its target destroyed after it acted. Never publish a
        // dangling intercept reference in the immutable continuation state.
        let surviving_wings = self
            .wings
            .iter()
            .filter(|wing| wing.count > 0)
            .map(|wing| wing.id)
            .collect::<BTreeSet<_>>();
        for wing in &mut self.wings {
            if wing.state == AirWingState::Intercept
                && !wing
                    .target_id
                    .is_some_and(|id| surviving_wings.contains(&id))
            {
                let airfield_id = wing.airfield_id;
                begin_return(wing, airfield_id);
            }
        }
        let before = self.wings.len();
        self.wings.retain(|wing| wing.count > 0);
        outcome.wings_destroyed = (before - self.wings.len()) as u32;
        outcome.defender_reactions = reactions.into_iter().collect();
    }

    #[allow(clippy::too_many_arguments)]
    fn advance_fighter(
        &mut self,
        wing: &mut AirWing,
        should_select: bool,
        field_index: Option<usize>,
        home_defense: bool,
        input: AirWorldInput<'_>,
        outcome: &mut AirAdvanceOutcome,
    ) {
        if wing.state == AirWingState::Grounded
            && field_index.is_some_and(|index| self.airfields[index].health > 0.0)
        {
            wing.state = AirWingState::Patrol;
        }
        if should_select
            && let Some(target_id) = self.choose_intercept_target(
                wing,
                field_index,
                home_defense,
                input.hostility,
                input.side_count,
                input.priority_areas,
            )
        {
            wing.target_kind = Some(AirTargetKind::AirWing);
            wing.target_id = Some(target_id);
            wing.state = AirWingState::Intercept;
            outcome.missions_selected += 1;
        }

        if wing.state == AirWingState::Intercept {
            let target_index = wing.target_id.and_then(|id| self.wing_index(id));
            let target_valid = target_index.is_some_and(|index| {
                let target = &self.wings[index];
                target.count > 0
                    && hostile(input.hostility, input.side_count, wing.side, target.side)
                    && field_index.is_none_or(|field_index| {
                        haversine_km(
                            self.airfields[field_index].lat,
                            self.airfields[field_index].lng,
                            target.lat,
                            target.lng,
                        ) <= FIGHTER_RANGE_KM
                    })
            });
            if !target_valid {
                let airfield_id = wing.airfield_id;
                begin_return(wing, airfield_id);
                return;
            }

            let target_index = target_index.expect("validated target");
            let target_lat = self.wings[target_index].lat;
            let target_lng = self.wings[target_index].lng;
            if move_toward(wing, target_lat, target_lng, FIGHTER_SPEED_DEGREES) {
                let target_id = self.wings[target_index].id;
                let requested_loss = (2.0 * quality_multiplier(wing.quality) * wing_strength(wing))
                    .round()
                    .max(1.0) as u32;
                let actual_loss = apply_wing_loss(&mut self.wings[target_index], requested_loss);
                if actual_loss > 0 {
                    outcome
                        .wing_losses
                        .push((target_id, wing.sovereign_country_id, actual_loss));
                }

                if self.wings[target_index].role == AirRole::Fighter
                    && self.wings[target_index].count > 0
                {
                    let return_attacker = self.wings[target_index].sovereign_country_id;
                    let return_loss = (1.25
                        * quality_multiplier(self.wings[target_index].quality)
                        * wing_strength(&self.wings[target_index]))
                    .round()
                    .max(1.0) as u32;
                    let actual_return_loss = return_loss.min(wing.count);
                    wing.count -= actual_return_loss;
                    if actual_return_loss > 0 {
                        outcome
                            .wing_losses
                            .push((wing.id, return_attacker, actual_return_loss));
                    }
                }
                let airfield_id = wing.airfield_id;
                begin_return(wing, airfield_id);
                outcome.interceptions_completed += 1;
            }
        }

        wing.endurance_ticks = wing.endurance_ticks.saturating_add(AIR_TICK_INTERVAL);
        if wing.endurance_ticks >= FIGHTER_ENDURANCE_TICKS {
            wing.endurance_ticks = 0;
            let airfield_id = wing.airfield_id;
            begin_return(wing, airfield_id);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn advance_strike(
        &mut self,
        wing: &mut AirWing,
        should_select: bool,
        field_index: Option<usize>,
        strikes_allowed: bool,
        coverage: f64,
        candidates: &[StrikeCandidate],
        input: AirWorldInput<'_>,
        outcome: &mut AirAdvanceOutcome,
        reactions: &mut BTreeSet<u64>,
    ) {
        wing.cooldown_ticks = wing.cooldown_ticks.saturating_sub(AIR_TICK_INTERVAL);
        if should_select
            && strikes_allowed
            && coverage >= 0.999
            && wing.cooldown_ticks == 0
            && field_index.is_some_and(|index| self.field_is_operational_for(index, wing.side))
            && let Some(candidate) = self.select_strike_target(
                wing,
                field_index.expect("operational field"),
                candidates,
                input,
            )
        {
            wing.target_kind = Some(candidate.kind);
            wing.target_id = Some(candidate.id);
            wing.state = AirWingState::Attacking;
            outcome.missions_selected += 1;
        }

        if wing.state != AirWingState::Attacking {
            return;
        }
        let candidate = candidates.iter().find(|candidate| {
            Some(candidate.kind) == wing.target_kind && Some(candidate.id) == wing.target_id
        });
        let valid = candidate.is_some_and(|candidate| {
            hostile(input.hostility, input.side_count, wing.side, candidate.side)
                && field_index.is_none_or(|field_index| {
                    haversine_km(
                        self.airfields[field_index].lat,
                        self.airfields[field_index].lng,
                        candidate.lat,
                        candidate.lng,
                    ) <= STRIKE_RANGE_KM
                })
        });
        if !valid {
            let airfield_id = wing.airfield_id;
            begin_return(wing, airfield_id);
            return;
        }
        let candidate = candidate.expect("validated strike candidate");
        if !move_toward(wing, candidate.lat, candidate.lng, STRIKE_SPEED_DEGREES) {
            return;
        }

        let base_damage = 10.0 * quality_multiplier(wing.quality) * wing_strength(wing);
        let damage = base_damage
            * match candidate.kind {
                AirTargetKind::Armor => 2.5,
                AirTargetKind::Airfield => 1.5,
                AirTargetKind::Army => 1.0,
                AirTargetKind::AirWing => unreachable!("air wings are not strike candidates"),
            };
        match candidate.kind {
            AirTargetKind::Army | AirTargetKind::Armor => {
                outcome
                    .land_damage
                    .push((candidate.id, wing.sovereign_country_id, damage));
                reactions.insert(candidate.id);
            }
            AirTargetKind::Airfield => {
                self.damage_airfield(candidate.id, damage, wing.sovereign_country_id, outcome);
            }
            AirTargetKind::AirWing => unreachable!("air wings are not strike candidates"),
        }
        wing.cooldown_ticks = STRIKE_COOLDOWN_TICKS;
        let airfield_id = wing.airfield_id;
        begin_return(wing, airfield_id);
        outcome.strikes_completed += 1;
    }

    fn choose_intercept_target(
        &self,
        wing: &AirWing,
        field_index: Option<usize>,
        home_defense: bool,
        hostility: &[u8],
        side_count: usize,
        areas: &[AirPriorityArea],
    ) -> Option<u64> {
        let mut best: Option<(bool, f64, u64)> = None;
        for target in &self.wings {
            if target.id == wing.id
                || target.count == 0
                || matches!(
                    target.state,
                    AirWingState::Grounded | AirWingState::Evacuated
                )
                || !hostile(hostility, side_count, wing.side, target.side)
            {
                continue;
            }
            let distance = haversine_km(wing.lat, wing.lng, target.lat, target.lng);
            if distance > FIGHTER_RANGE_KM {
                continue;
            }
            if home_defense
                && let Some(field_index) = field_index
                && haversine_km(
                    self.airfields[field_index].lat,
                    self.airfields[field_index].lng,
                    target.lat,
                    target.lng,
                ) > FIGHTER_RANGE_KM
            {
                continue;
            }
            let prioritized = in_priority_area(target.lat, target.lng, wing.side, areas);
            let score = if target.role == AirRole::Strike {
                10_000.0 - distance
            } else {
                -distance
            };
            let replace = best.is_none_or(|(best_priority, best_score, best_id)| {
                prioritized && !best_priority
                    || (prioritized == best_priority
                        && (score > best_score || (score == best_score && target.id < best_id)))
            });
            if replace {
                best = Some((prioritized, score, target.id));
            }
        }
        best.map(|(_, _, id)| id)
    }

    fn select_strike_target<'a>(
        &self,
        wing: &AirWing,
        field_index: usize,
        candidates: &'a [StrikeCandidate],
        input: AirWorldInput<'_>,
    ) -> Option<&'a StrikeCandidate> {
        let source = &self.airfields[field_index];
        let mut best: Option<(bool, f64, &StrikeCandidate)> = None;
        for candidate in candidates {
            if !hostile(input.hostility, input.side_count, wing.side, candidate.side) {
                continue;
            }
            let distance = haversine_km(source.lat, source.lng, candidate.lat, candidate.lng);
            if distance > STRIKE_RANGE_KM {
                continue;
            }
            let base_score = match candidate.kind {
                AirTargetKind::Armor => 300.0 + candidate.strength.max(0.0),
                AirTargetKind::Airfield if candidate.strength > 0.0 => 200.0 + candidate.strength,
                AirTargetKind::Army => 100.0 + candidate.strength.max(1.0) * 5.0,
                AirTargetKind::Airfield | AirTargetKind::AirWing => continue,
            };
            let prioritized = candidate.explicit_priority
                || in_priority_area(
                    candidate.lat,
                    candidate.lng,
                    wing.side,
                    input.priority_areas,
                );
            let score = base_score - distance * 0.01;
            let replace = best.is_none_or(|(best_priority, best_score, best_candidate)| {
                prioritized && !best_priority
                    || (prioritized == best_priority
                        && (score > best_score
                            || (score == best_score
                                && lexical_id_before(candidate.id, best_candidate.id))))
            });
            if replace {
                best = Some((prioritized, score, candidate));
            }
        }
        best.map(|(_, _, candidate)| candidate)
    }

    fn strike_candidates(&self, targets: &[AirUnitTarget]) -> Vec<StrikeCandidate> {
        let mut units = targets.to_vec();
        units.sort_by_key(|target| (target.id, target.kind));
        let mut candidates = units
            .into_iter()
            .map(|target| StrikeCandidate {
                id: target.id,
                side: target.side,
                lat: target.lat,
                lng: target.lng,
                kind: target.kind,
                strength: target.strength,
                explicit_priority: target.priority_area,
            })
            .collect::<Vec<_>>();
        candidates.extend(self.airfields.iter().map(|field| StrikeCandidate {
            id: field.id,
            side: field.side,
            lat: field.lat,
            lng: field.lng,
            kind: AirTargetKind::Airfield,
            strength: field.health,
            explicit_priority: false,
        }));
        candidates
    }

    fn damage_airfield(
        &mut self,
        field_id: u64,
        damage: f64,
        attacker_country_id: u16,
        outcome: &mut AirAdvanceOutcome,
    ) {
        let Some(field_index) = self.field_index(field_id) else {
            return;
        };
        let was_operational = self.airfields[field_index].health > 0.0;
        self.airfields[field_index].health = (self.airfields[field_index].health - damage).max(0.0);
        if self.airfields[field_index].health == 0.0 {
            self.airfields[field_index].disabled = true;
        }
        outcome
            .airfield_damage
            .push((field_id, attacker_country_id, damage));

        if was_operational && self.airfields[field_index].health == 0.0 {
            let based_loss = (damage * 0.25).round().max(1.0) as u32;
            for based_wing in &mut self.wings {
                if based_wing.airfield_id != field_id || based_wing.count == 0 {
                    continue;
                }
                let wing_id = based_wing.id;
                let actual = apply_wing_loss(based_wing, based_loss);
                if actual > 0 {
                    outcome
                        .wing_losses
                        .push((wing_id, attacker_country_id, actual));
                }
            }
        }
    }

    fn field_index(&self, id: u64) -> Option<usize> {
        self.airfields
            .binary_search_by_key(&id, |field| field.id)
            .ok()
    }

    fn wing_index(&self, id: u64) -> Option<usize> {
        self.wings.binary_search_by_key(&id, |wing| wing.id).ok()
    }

    fn field_is_operational_for(&self, index: usize, side: usize) -> bool {
        let field = &self.airfields[index];
        field.side == side && field.health > 0.0 && !field.disabled
    }

    fn find_eligible_airfield(&self, wing: &AirWing) -> Option<u64> {
        let mut best: Option<(bool, f64, u64)> = None;
        for field in &self.airfields {
            if field.side != wing.side || field.health <= 0.0 || field.disabled {
                continue;
            }
            let distance = haversine_km(wing.lat, wing.lng, field.lat, field.lng);
            if distance > FERRY_RANGE_KM {
                continue;
            }
            let national = field.controller_country_id == wing.sovereign_country_id;
            let (national_wings, allied_wings) = self.field_occupancy(field.id);
            let capacity = effective_capacity(field);
            let has_capacity = if national {
                national_wings + allied_wings < capacity
            } else {
                allied_wings < capacity / 2
            };
            if !has_capacity {
                continue;
            }
            let replace = best.is_none_or(|(best_national, best_distance, best_id)| {
                national && !best_national
                    || (national == best_national
                        && (distance < best_distance
                            || (distance == best_distance && field.id < best_id)))
            });
            if replace {
                best = Some((national, distance, field.id));
            }
        }
        best.map(|(_, _, id)| id)
    }

    fn field_occupancy(&self, field_id: u64) -> (u32, u32) {
        let controller = self
            .field_index(field_id)
            .map(|index| self.airfields[index].controller_country_id)
            .unwrap_or(0);
        self.wings
            .iter()
            .filter(|wing| wing.airfield_id == field_id && wing.state != AirWingState::Evacuated)
            .fold((0, 0), |(national, allied), wing| {
                if wing.sovereign_country_id == controller {
                    (national + 1, allied)
                } else {
                    (national, allied + 1)
                }
            })
    }
}

#[derive(Clone, Copy, Debug)]
struct StrikeCandidate {
    id: u64,
    side: usize,
    lat: f64,
    lng: f64,
    kind: AirTargetKind,
    strength: f64,
    explicit_priority: bool,
}

#[derive(Clone, Copy, Debug)]
struct AircraftPolicy {
    fighters_grounded: bool,
    home_defense: bool,
    strikes: bool,
}

fn aircraft_policy(band: CommandBand) -> AircraftPolicy {
    match band {
        CommandBand::Paid => AircraftPolicy {
            fighters_grounded: false,
            home_defense: false,
            strikes: true,
        },
        CommandBand::Strained => AircraftPolicy {
            fighters_grounded: false,
            home_defense: true,
            strikes: false,
        },
        CommandBand::Unpaid | CommandBand::Breakdown | CommandBand::Mutiny => AircraftPolicy {
            fighters_grounded: true,
            home_defense: false,
            strikes: false,
        },
    }
}

fn validate_world(state: &AirPowerState, input: AirWorldInput<'_>) -> Result<(), AirError> {
    let hostility_len = input
        .side_count
        .checked_mul(input.side_count)
        .ok_or(AirError::InvalidWorld("hostility dimensions overflow"))?;
    if input.side_count == 0 || input.hostility.len() != hostility_len {
        return Err(AirError::InvalidWorld("hostility matrix"));
    }
    for (index, &relation) in input.hostility.iter().enumerate() {
        if relation > 1 || (index / input.side_count == index % input.side_count && relation != 0) {
            return Err(AirError::InvalidWorld("hostility relation"));
        }
    }
    if state
        .airfields
        .iter()
        .any(|field| field.side >= input.side_count)
        || state.wings.iter().any(|wing| wing.side >= input.side_count)
    {
        return Err(AirError::InvalidWorld("state side"));
    }
    if input
        .command_bands
        .keys()
        .any(|&country_id| country_id == 0)
        || input
            .air_operations_coverage
            .iter()
            .any(|(&country_id, &coverage)| {
                country_id == 0 || !coverage.is_finite() || !(0.0..=1.0).contains(&coverage)
            })
    {
        return Err(AirError::InvalidWorld("country air policy"));
    }
    if state.wings.iter().any(|wing| {
        !input
            .air_operations_coverage
            .contains_key(&wing.sovereign_country_id)
    }) {
        return Err(AirError::InvalidWorld("missing wing country coverage"));
    }

    let mut target_ids = BTreeSet::new();
    for target in input.targets {
        if target.id == 0
            || target.country_id == 0
            || target.side >= input.side_count
            || !matches!(target.kind, AirTargetKind::Army | AirTargetKind::Armor)
            || !valid_position(target.lat, target.lng)
            || !target.strength.is_finite()
            || target.strength < 0.0
            || !target_ids.insert(target.id)
        {
            return Err(AirError::InvalidWorld("land target"));
        }
    }
    for area in input.priority_areas {
        if area.side >= input.side_count
            || !valid_position(area.lat, area.lng)
            || !area.radius_km.is_finite()
            || area.radius_km <= 0.0
        {
            return Err(AirError::InvalidWorld("priority area"));
        }
    }
    for (&field_id, controller) in input.airfield_controllers {
        if state.field_index(field_id).is_none()
            || controller.side >= input.side_count
            || controller.controller_country_id == 0
        {
            return Err(AirError::InvalidWorld("airfield controller"));
        }
    }
    Ok(())
}

fn wing_mission_is_due(wing: &mut AirWing, tick: u64) -> bool {
    if wing.force_mission {
        wing.force_mission = false;
        wing.next_mission_tick = Some(tick.saturating_add(AIR_MISSION_INTERVAL));
        return true;
    }
    let next = match wing.next_mission_tick {
        Some(next) => next,
        None => {
            let cycle_start = tick - tick % AIR_MISSION_INTERVAL;
            let mut scheduled = cycle_start.saturating_add(mission_offset(wing.id));
            if scheduled < tick {
                scheduled = scheduled.saturating_add(AIR_MISSION_INTERVAL);
            }
            wing.next_mission_tick = Some(scheduled);
            scheduled
        }
    };
    if tick < next {
        return false;
    }
    wing.next_mission_tick = Some(tick.saturating_add(AIR_MISSION_INTERVAL));
    true
}

fn mission_offset(wing_id: u64) -> u64 {
    let mut hash = 0_u32;
    for byte in wing_id.to_string().bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    let slots = (AIR_MISSION_INTERVAL / AIR_TICK_INTERVAL).max(1);
    u64::from(hash) % slots * AIR_TICK_INTERVAL
}

fn hostile(relations: &[u8], side_count: usize, left: usize, right: usize) -> bool {
    left != right
        && left < side_count
        && right < side_count
        && relations[left * side_count + right] == 1
}

fn effective_capacity(field: &Airfield) -> u32 {
    if field.disabled || field.health <= 0.0 {
        0
    } else if field.health <= 50.0 {
        field.capacity.min(1)
    } else {
        field.capacity
    }
}

fn quality_multiplier(quality: f64) -> f64 {
    0.75 + quality.clamp(0.0, 100.0) / 200.0
}

fn wing_strength(wing: &AirWing) -> f64 {
    (f64::from(wing.count) / f64::from(wing.max_count)).max(0.2)
}

fn apply_wing_loss(wing: &mut AirWing, requested: u32) -> u32 {
    let actual = requested.min(wing.count);
    wing.count -= actual;
    actual
}

fn ground_wing(wing: &mut AirWing) {
    wing.state = AirWingState::Grounded;
    wing.return_airfield_id = None;
    wing.rearm_ticks = 0;
    clear_target(wing);
}

fn begin_return(wing: &mut AirWing, fallback_field_id: u64) {
    wing.state = AirWingState::Returning;
    if wing.return_airfield_id.is_none() {
        wing.return_airfield_id = Some(fallback_field_id);
    }
    wing.rearm_ticks = 0;
    clear_target(wing);
}

fn clear_target(wing: &mut AirWing) {
    wing.target_kind = None;
    wing.target_id = None;
}

fn move_toward(wing: &mut AirWing, target_lat: f64, target_lng: f64, distance: f64) -> bool {
    let delta_lat = target_lat - wing.lat;
    let delta_lng = wrapped_longitude_delta(target_lng, wing.lng);
    let degrees = (delta_lat * delta_lat + delta_lng * delta_lng).sqrt();
    if degrees <= distance || degrees == 0.0 {
        wing.lat = target_lat;
        wing.lng = normalize_longitude(target_lng);
        return true;
    }
    wing.lat += delta_lat / degrees * distance;
    wing.lng = normalize_longitude(wing.lng + delta_lng / degrees * distance);
    false
}

fn haversine_km(left_lat: f64, left_lng: f64, right_lat: f64, right_lng: f64) -> f64 {
    let left_lat_radians = left_lat.to_radians();
    let right_lat_radians = right_lat.to_radians();
    let delta_lat = (right_lat - left_lat).to_radians();
    let delta_lng = wrapped_longitude_delta(right_lng, left_lng).to_radians();
    let a = (delta_lat / 2.0).sin().powi(2)
        + left_lat_radians.cos() * right_lat_radians.cos() * (delta_lng / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.sqrt().atan2((1.0 - a).max(0.0).sqrt())
}

fn wrapped_longitude_delta(target: f64, source: f64) -> f64 {
    let mut delta = target - source;
    if delta > 180.0 {
        delta -= 360.0;
    } else if delta < -180.0 {
        delta += 360.0;
    }
    delta
}

fn normalize_longitude(mut longitude: f64) -> f64 {
    if longitude > 180.0 {
        longitude -= 360.0;
    } else if longitude < -180.0 {
        longitude += 360.0;
    }
    longitude
}

fn in_priority_area(
    target_lat: f64,
    target_lng: f64,
    requesting_side: usize,
    areas: &[AirPriorityArea],
) -> bool {
    areas.iter().any(|area| {
        area.side == requesting_side
            && haversine_km(area.lat, area.lng, target_lat, target_lng) <= area.radius_km
    })
}

fn lexical_id_before(left: u64, right: u64) -> bool {
    left.to_string() < right.to_string()
}

fn valid_position(lat: f64, lng: f64) -> bool {
    lat.is_finite()
        && lng.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lng)
}

fn strictly_ordered(mut ids: impl Iterator<Item = u64>) -> bool {
    let Some(mut previous) = ids.next() else {
        return true;
    };
    if previous == 0 {
        return false;
    }
    for id in ids {
        if id <= previous {
            return false;
        }
        previous = id;
    }
    true
}

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(id: u64, side: usize, country: u16, lat: f64, lng: f64) -> Airfield {
        Airfield {
            id,
            side,
            owner_country_id: country,
            controller_country_id: country,
            lat,
            lng,
            capacity: 3,
            health: 100.0,
            disabled: false,
            capture_repair_cycles: 2,
            capital: true,
        }
    }

    fn wing(
        id: u64,
        side: usize,
        country: u16,
        airfield_id: u64,
        role: AirRole,
        lat: f64,
        lng: f64,
    ) -> AirWing {
        AirWing {
            id,
            side,
            sovereign_country_id: country,
            airfield_id,
            return_airfield_id: None,
            role,
            quality: 50.0,
            max_count: 24,
            count: 24,
            lat,
            lng,
            state: AirWingState::Grounded,
            target_kind: None,
            target_id: None,
            rearm_ticks: 0,
            cooldown_ticks: 0,
            endurance_ticks: 0,
            next_mission_tick: None,
            force_mission: true,
        }
    }

    fn advance(
        state: &mut AirPowerState,
        tick: u64,
        bands: &BTreeMap<u16, CommandBand>,
        targets: &[AirUnitTarget],
        controllers: &BTreeMap<u64, AirfieldController>,
    ) -> AirAdvanceOutcome {
        let coverage = state
            .country_coverage
            .iter()
            .map(|coverage| (coverage.country_id, coverage.operations_coverage))
            .collect::<BTreeMap<_, _>>();
        state
            .advance(AirWorldInput {
                tick,
                side_count: 2,
                hostility: &[0, 1, 1, 0],
                command_bands: bands,
                air_operations_coverage: &coverage,
                targets,
                priority_areas: &[],
                airfield_controllers: controllers,
            })
            .unwrap()
    }

    #[test]
    fn wire_requires_nullable_fields_and_rejects_unknown_fields() {
        let value = serde_json::to_value(wing(1, 0, 1, 10, AirRole::Fighter, 0.0, 0.0)).unwrap();
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove("targetId");
        assert!(serde_json::from_value::<AirWing>(missing).is_err());
        let mut unknown = value;
        unknown
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), true.into());
        assert!(serde_json::from_value::<AirWing>(unknown).is_err());

        let state = AirPowerState::new(
            vec![field(10, 0, 1, 0.0, 0.0)],
            vec![wing(1, 0, 1, 10, AirRole::Fighter, 0.0, 0.0)],
        )
        .unwrap();
        let mut missing_coverage = serde_json::to_value(&state).unwrap();
        missing_coverage
            .as_object_mut()
            .unwrap()
            .remove("countryCoverage");
        assert!(serde_json::from_value::<AirPowerState>(missing_coverage).is_err());
    }

    #[test]
    fn constructor_canonicalizes_and_validation_rejects_dangling_intercept() {
        let state = AirPowerState::new(
            vec![field(20, 1, 2, 0.0, 1.0), field(10, 0, 1, 0.0, 0.0)],
            vec![
                wing(2, 1, 2, 20, AirRole::Strike, 0.0, 1.0),
                wing(1, 0, 1, 10, AirRole::Fighter, 0.0, 0.0),
            ],
        )
        .unwrap();
        assert_eq!(state.airfields[0].id, 10);
        assert_eq!(state.wings[0].id, 1);
        assert_eq!(
            state.country_coverage,
            vec![
                AirCountryCoverage {
                    country_id: 1,
                    operations_coverage: 1.0,
                },
                AirCountryCoverage {
                    country_id: 2,
                    operations_coverage: 1.0,
                },
            ]
        );

        let mut bad = state;
        bad.wings[0].state = AirWingState::Intercept;
        bad.wings[0].target_kind = Some(AirTargetKind::AirWing);
        bad.wings[0].target_id = Some(99);
        assert_eq!(
            bad.validate(),
            Err(AirError::InvalidState("fighter intercept"))
        );
    }

    #[test]
    fn fighter_interception_applies_browser_losses_and_return_fire() {
        let mut attacker = wing(1, 0, 1, 10, AirRole::Fighter, 0.0, 0.0);
        attacker.state = AirWingState::Patrol;
        let mut target = wing(2, 1, 2, 20, AirRole::Fighter, 0.0, 0.05);
        target.state = AirWingState::Patrol;
        target.force_mission = false;
        target.next_mission_tick = Some(120);
        let mut state = AirPowerState::new(
            vec![field(10, 0, 1, 0.0, 0.0), field(20, 1, 2, 0.0, 0.05)],
            vec![attacker, target],
        )
        .unwrap();

        let outcome = advance(&mut state, 0, &BTreeMap::new(), &[], &BTreeMap::new());
        assert_eq!(outcome.interceptions_completed, 1);
        assert_eq!(outcome.wing_losses, vec![(2, 1, 2), (1, 2, 1)]);
        assert_eq!(state.wings[0].count, 23);
        assert_eq!(state.wings[1].count, 22);
        assert_eq!(state.wings[0].state, AirWingState::Returning);
    }

    #[test]
    fn priority_strike_applies_armor_damage_and_attribution() {
        let bomber = wing(1, 0, 1, 10, AirRole::Strike, 0.0, 0.0);
        let mut state = AirPowerState::new(vec![field(10, 0, 1, 0.0, 0.0)], vec![bomber]).unwrap();
        let targets = [
            AirUnitTarget {
                id: 100,
                side: 1,
                country_id: 2,
                lat: 0.0,
                lng: 0.0,
                kind: AirTargetKind::Armor,
                strength: 100.0,
                priority_area: false,
            },
            AirUnitTarget {
                id: 200,
                side: 1,
                country_id: 2,
                lat: 0.0,
                lng: 0.0,
                kind: AirTargetKind::Armor,
                strength: 1.0,
                priority_area: true,
            },
        ];

        let outcome = advance(&mut state, 0, &BTreeMap::new(), &targets, &BTreeMap::new());
        assert_eq!(outcome.land_damage, vec![(200, 1, 25.0)]);
        assert_eq!(outcome.defender_reactions, vec![200]);
        assert_eq!(outcome.strikes_completed, 1);
        assert_eq!(state.wings[0].cooldown_ticks, STRIKE_COOLDOWN_TICKS);
        assert_eq!(state.wings[0].state, AirWingState::Returning);
    }

    #[test]
    fn strained_command_is_home_defense_only_and_blocks_strikes() {
        let mut fighter = wing(1, 0, 1, 10, AirRole::Fighter, 0.0, 0.0);
        fighter.state = AirWingState::Patrol;
        let bomber = wing(2, 0, 1, 10, AirRole::Strike, 0.0, 0.0);
        let mut distant_enemy = wing(3, 1, 2, 20, AirRole::Strike, 0.0, 10.0);
        distant_enemy.state = AirWingState::Returning;
        distant_enemy.return_airfield_id = Some(20);
        distant_enemy.force_mission = false;
        distant_enemy.next_mission_tick = Some(120);
        let mut state = AirPowerState::new(
            vec![field(10, 0, 1, 0.0, 0.0), field(20, 1, 2, 0.0, 10.0)],
            vec![fighter, bomber, distant_enemy],
        )
        .unwrap();
        let bands = BTreeMap::from([(1, CommandBand::Strained)]);

        let outcome = advance(&mut state, 0, &bands, &[], &BTreeMap::new());
        assert_eq!(outcome.missions_selected, 0);
        assert_eq!(state.wings[0].state, AirWingState::Patrol);
        assert_eq!(state.wings[1].state, AirWingState::Grounded);
    }

    #[test]
    fn persisted_country_coverage_grounds_wings_and_is_strictly_validated() {
        let fighter = wing(1, 0, 1, 10, AirRole::Fighter, 0.0, 0.0);
        let mut state = AirPowerState::new(vec![field(10, 0, 1, 0.0, 0.0)], vec![fighter]).unwrap();
        state.country_coverage[0].operations_coverage = 0.24;

        let outcome = advance(&mut state, 0, &BTreeMap::new(), &[], &BTreeMap::new());
        assert_eq!(outcome.missions_selected, 0);
        assert_eq!(state.wings[0].state, AirWingState::Grounded);

        let mut duplicate = state.clone();
        duplicate
            .country_coverage
            .push(duplicate.country_coverage[0]);
        assert_eq!(
            duplicate.validate(),
            Err(AirError::InvalidState(
                "country coverage must have unique ordered positive ids and finite coverage"
            ))
        );
        let mut missing = state;
        missing.country_coverage.clear();
        assert_eq!(missing.validate(), Err(AirError::InvalidState("wing")));
    }

    #[test]
    fn controller_update_captures_before_air_tick_and_is_atomic() {
        let mut state = AirPowerState::new(vec![field(10, 0, 1, 0.0, 0.0)], Vec::new()).unwrap();
        let controllers = BTreeMap::from([(
            10,
            AirfieldController {
                side: 1,
                controller_country_id: 2,
            },
        )]);
        let outcome = advance(&mut state, 1, &BTreeMap::new(), &[], &controllers);
        assert!(outcome.updated);
        assert_eq!(outcome.airfield_captures.len(), 1);
        assert_eq!(state.airfields[0].side, 1);
        assert_eq!(state.airfields[0].health, 0.0);
        assert!(state.airfields[0].disabled);

        let before = state.clone();
        let coverage = state
            .country_coverage
            .iter()
            .map(|coverage| (coverage.country_id, coverage.operations_coverage))
            .collect::<BTreeMap<_, _>>();
        let error = state
            .advance(AirWorldInput {
                tick: 2,
                side_count: 2,
                hostility: &[0, 2, 1, 0],
                command_bands: &BTreeMap::new(),
                air_operations_coverage: &coverage,
                targets: &[],
                priority_areas: &[],
                airfield_controllers: &BTreeMap::new(),
            })
            .unwrap_err();
        assert_eq!(error, AirError::InvalidWorld("hostility relation"));
        assert_eq!(state, before);
    }

    #[test]
    fn destroyed_airfield_damages_based_wings() {
        let bomber = wing(1, 0, 1, 10, AirRole::Strike, 0.0, 0.0);
        let mut based_enemy = wing(2, 1, 2, 20, AirRole::Fighter, 0.0, 0.0);
        based_enemy.force_mission = false;
        based_enemy.next_mission_tick = Some(120);
        let mut target_field = field(20, 1, 2, 0.0, 0.0);
        target_field.health = 10.0;
        let mut state = AirPowerState::new(
            vec![field(10, 0, 1, 0.0, 0.0), target_field],
            vec![bomber, based_enemy],
        )
        .unwrap();

        let outcome = advance(&mut state, 0, &BTreeMap::new(), &[], &BTreeMap::new());
        assert_eq!(outcome.airfield_damage, vec![(20, 1, 15.0)]);
        assert_eq!(outcome.wing_losses, vec![(2, 1, 4)]);
        assert_eq!(state.airfields[1].health, 0.0);
        assert!(state.airfields[1].disabled);
    }

    #[test]
    fn mission_offset_matches_browser_decimal_hash() {
        assert_eq!(mission_offset(1), 54);
        assert_eq!(mission_offset(123), 60);
    }
}
