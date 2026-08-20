//! Atomic pay-cycle orchestration for economy, occupation, and surrender.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    economy::{
        CommandBand, EconomyCycleInput, EconomyError, EconomyState, PAY_CYCLE_TICKS,
        compute_current_income, desertion_rate, settle_economy_cycle,
    },
    occupation::{
        OccupationAssessment, OccupationError, OccupationState, required_garrison, resistance_delta,
    },
    surrender::{
        CapitulationDecision, CapitulationInput, CasualtyEntry, ConflictResolution,
        ConflictResolutionKind, SurrenderError, eligible_casualty_attackers, evaluate_capitulation,
        evaluate_global_conflict, largest_remainder_quotas,
    },
};

pub const STRATEGIC_SCHEMA_VERSION: &str = "strategic-cycle-v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CountryCycleInput {
    pub country_id: u16,
    pub side: u16,
    pub owned_cells: u32,
    pub controlled_cells: u32,
    pub core_controlled: u32,
    pub initial_cells: u32,
    pub city_population_controlled: f64,
    pub unit_count: u32,
    pub payroll_due: f64,
    pub capital_held: bool,
    pub is_rebel: bool,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OccupationCycleRecord {
    pub victim_id: u16,
    pub held_cells: u32,
    pub garrison_strength: f64,
    pub casualty_pressure: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StrategicCycleInput {
    pub tick: u64,
    #[serde(default)]
    pub force: bool,
    pub territory_generation: u64,
    pub territory_commit_sequence: u64,
    pub territory_fresh: bool,
    pub countries: Vec<CountryCycleInput>,
    #[serde(default)]
    pub occupations: Vec<OccupationCycleRecord>,
    #[serde(default)]
    pub active_sides: Vec<u16>,
    #[serde(default)]
    pub active_hostile_pairs: Vec<(u16, u16)>,
    /// Sides with at least one outgoing active hostility edge. `None` preserves the
    /// legacy symmetric-pair contract for standalone fixtures; production derivation
    /// always supplies `Some`, including an explicitly empty set.
    #[serde(default)]
    pub capitulation_active_sides: Option<Vec<u16>>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StrategicEventKind {
    BudgetDeficit,
    CommandBandChanged,
    ResistanceWarning,
    CapitulationTriggered,
    TreatyResolved,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StrategicEvent {
    pub kind: StrategicEventKind,
    pub country_id: Option<u16>,
    pub related_country_id: Option<u16>,
    pub previous_band: Option<CommandBand>,
    pub next_band: Option<CommandBand>,
    pub value: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DesertionCommand {
    pub country_id: u16,
    pub rate: f64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SurrenderCommand {
    pub country_id: u16,
    pub side: u16,
    pub decision: CapitulationDecision,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CountryStrategicSnapshot {
    pub country_id: u16,
    pub side: u16,
    pub economy: EconomyState,
    pub capitulation: CapitulationDecision,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StrategicSnapshot {
    pub schema_version: String,
    pub cycle: u64,
    pub tick: u64,
    pub territory_generation: u64,
    pub territory_commit_sequence: u64,
    pub countries: Arc<[CountryStrategicSnapshot]>,
    pub occupations: Arc<[OccupationState]>,
    pub occupation_assessments: Arc<[OccupationAssessment]>,
    pub desertions: Arc<[DesertionCommand]>,
    pub surrenders: Arc<[SurrenderCommand]>,
    pub events: Arc<[StrategicEvent]>,
    pub conflict_resolution: Option<ConflictResolution>,
}

/// A completely evaluated pay cycle that has not yet changed authoritative state.
///
/// Runtime orchestration may apply the published strategic consequences to its
/// other kernels and only commit this transaction once every boundary succeeds.
#[derive(Clone, Debug)]
pub struct PreparedStrategicCycle {
    base_cycle: u64,
    next_economies: BTreeMap<u16, EconomyState>,
    next_occupations: BTreeMap<u16, OccupationState>,
    snapshot: Arc<StrategicSnapshot>,
    counters: StrategicCounters,
}

impl PreparedStrategicCycle {
    pub fn snapshot(&self) -> Arc<StrategicSnapshot> {
        self.snapshot.clone()
    }

    pub const fn counters(&self) -> StrategicCounters {
        self.counters
    }

    pub fn economy(&self, country_id: u16) -> Option<&EconomyState> {
        self.next_economies.get(&country_id)
    }

    /// Add the occupation created by this cycle's capitulation consequence.
    /// Existing victims are rejected: changing an ongoing occupation requires
    /// an explicit future policy rather than an accidental overwrite.
    pub fn register_occupation(
        &mut self,
        occupation: OccupationState,
    ) -> Result<(), StrategicError> {
        if occupation.victim_id == 0
            || occupation.annexer_id == 0
            || occupation.victim_id == occupation.annexer_id
        {
            return Err(OccupationError::InvalidCountries.into());
        }
        if ![
            occupation.base_income,
            occupation.expected_army_units,
            occupation.resistance,
            occupation.occupation_coverage,
            occupation.garrison_coverage,
            occupation.garrison_assigned,
            occupation.held_ratio,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            return Err(OccupationError::NonFinite.into());
        }
        if !self.next_economies.contains_key(&occupation.victim_id) {
            return Err(StrategicError::UnknownCountry(occupation.victim_id));
        }
        if !self.next_economies.contains_key(&occupation.annexer_id) {
            return Err(StrategicError::UnknownCountry(occupation.annexer_id));
        }
        if self.next_occupations.contains_key(&occupation.victim_id) {
            return Err(StrategicError::DuplicateOccupation(occupation.victim_id));
        }
        self.next_occupations
            .insert(occupation.victim_id, occupation);
        Arc::make_mut(&mut self.snapshot).occupations = self
            .next_occupations
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .into();
        Ok(())
    }

    /// Fold the post-capitulation global result into the same immutable cycle
    /// publication. Re-registering an already published result is idempotent.
    pub fn register_conflict_resolution(&mut self, resolution: ConflictResolution) {
        let snapshot = Arc::make_mut(&mut self.snapshot);
        if snapshot.conflict_resolution.is_some() {
            return;
        }
        let mut events = snapshot.events.to_vec();
        events.push(StrategicEvent {
            kind: StrategicEventKind::TreatyResolved,
            country_id: None,
            related_country_id: None,
            previous_band: None,
            next_band: None,
            value: None,
        });
        snapshot.events = events.into();
        snapshot.conflict_resolution = Some(resolution);
        self.counters.events = snapshot.events.len();
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SurrenderUnitPosition {
    pub country_id: u16,
    pub lat: f64,
    pub lng: f64,
    pub health: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct SurrenderAllocationInput<'a> {
    pub victim_country_id: u16,
    pub hostile_attacker_ids: &'a [u16],
    /// Complete victim -> attacker casualty ledger. Only the victim row is read.
    pub casualties_by_victim: &'a BTreeMap<u16, BTreeMap<u16, f64>>,
    pub width: usize,
    pub height: usize,
    pub grid_resolution: f64,
    pub land: &'a [u8],
    pub world_control: &'a [u16],
    pub de_jure: &'a [u16],
    pub primary_occupier: &'a [u16],
    pub units: &'a [SurrenderUnitPosition],
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SurrenderCellTransfer {
    pub cell: usize,
    pub original_owner: u16,
    pub new_owner: u16,
}

/// Fully bounded, deterministic mutations for one capitulating country.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SurrenderAllocationPlan {
    pub victim_country_id: u16,
    pub primary_annexer_id: u16,
    pub transfers: Vec<SurrenderCellTransfer>,
}

/// Terminal conflict effects are deliberately bounded to stopping the runtime
/// and publishing the already-decided result. UI/treaty presentation stays out
/// of the simulation core.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConflictResolutionPlan {
    pub kind: ConflictResolutionKind,
    pub winner_side: Option<u16>,
    pub stop_simulation: bool,
}

impl From<ConflictResolution> for ConflictResolutionPlan {
    fn from(value: ConflictResolution) -> Self {
        Self {
            kind: value.kind,
            winner_side: value.winner_side,
            stop_simulation: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StrategicCounters {
    pub countries_processed: usize,
    pub occupations_processed: usize,
    pub capitulations: usize,
    pub desertion_commands: usize,
    pub events: usize,
}

#[derive(Debug, Error)]
pub enum StrategicError {
    #[error("strategic cycle is not due")]
    NotDue,
    #[error("strategic cycle counter overflowed")]
    CycleOverflow,
    #[error("strategic tick {received} must be greater than previous tick {previous}")]
    NonMonotonicTick { previous: u64, received: u64 },
    #[error("territory generation {received} must not be less than previous generation {previous}")]
    NonMonotonicTerritoryGeneration { previous: u64, received: u64 },
    #[error(
        "territory commit sequence {received} must not be less than previous sequence {previous}"
    )]
    NonMonotonicTerritoryCommitSequence { previous: u64, received: u64 },
    #[error("duplicate country id {0}")]
    DuplicateCountry(u16),
    #[error("duplicate occupation victim id {0}")]
    DuplicateOccupation(u16),
    #[error("unknown country id {0}")]
    UnknownCountry(u16),
    #[error("country input contains invalid numeric data")]
    InvalidCountryInput,
    #[error("prepared strategic cycle was based on cycle {prepared}, current cycle is {current}")]
    StalePreparedCycle { prepared: u64, current: u64 },
    #[error("surrender allocation input is invalid: {0}")]
    InvalidSurrenderAllocation(&'static str),
    #[error("capitulation has no active hostile recipient")]
    NoHostileRecipient,
    #[error("capitulation has no deterministic recipient")]
    NoDeterministicRecipient,
    #[error("capitulation allocation could not satisfy its bounded quotas")]
    IncompleteSurrenderAllocation,
    #[error("economy: {0}")]
    Economy(#[from] EconomyError),
    #[error("occupation: {0}")]
    Occupation(#[from] OccupationError),
    #[error("surrender: {0}")]
    Surrender(#[from] SurrenderError),
}

pub struct StrategicSimulation {
    cycle: u64,
    economies: BTreeMap<u16, EconomyState>,
    occupations: BTreeMap<u16, OccupationState>,
    latest: Option<Arc<StrategicSnapshot>>,
}

impl StrategicSimulation {
    pub fn new(
        economies: impl IntoIterator<Item = EconomyState>,
        occupations: impl IntoIterator<Item = OccupationState>,
    ) -> Result<Self, StrategicError> {
        Self::restore(0, economies, occupations)
    }

    /// Restore a checkpoint at an already completed browser pay-cycle.
    ///
    /// The cycle is gameplay state: occupation queue and cooldown fields are
    /// expressed in this coordinate system, so a mid-war hand-off must not
    /// silently reset it to zero.
    pub fn restore(
        cycle: u64,
        economies: impl IntoIterator<Item = EconomyState>,
        occupations: impl IntoIterator<Item = OccupationState>,
    ) -> Result<Self, StrategicError> {
        let mut economy_map = BTreeMap::new();
        for economy in economies {
            let country_id = economy.country_id;
            if economy_map.insert(country_id, economy).is_some() {
                return Err(StrategicError::DuplicateCountry(country_id));
            }
        }
        let mut occupation_map = BTreeMap::new();
        for occupation in occupations {
            let victim_id = occupation.victim_id;
            if occupation_map.insert(victim_id, occupation).is_some() {
                return Err(StrategicError::DuplicateOccupation(victim_id));
            }
        }
        for occupation in occupation_map.values() {
            if !economy_map.contains_key(&occupation.victim_id) {
                return Err(StrategicError::UnknownCountry(occupation.victim_id));
            }
            if !economy_map.contains_key(&occupation.annexer_id) {
                return Err(StrategicError::UnknownCountry(occupation.annexer_id));
            }
        }
        Ok(Self {
            cycle,
            economies: economy_map,
            occupations: occupation_map,
            latest: None,
        })
    }

    pub fn cycle(&self) -> u64 {
        self.cycle
    }

    pub fn economies(&self) -> &BTreeMap<u16, EconomyState> {
        &self.economies
    }

    pub fn occupations(&self) -> &BTreeMap<u16, OccupationState> {
        &self.occupations
    }

    pub fn latest_snapshot(&self) -> Option<Arc<StrategicSnapshot>> {
        self.latest.clone()
    }

    pub fn run_cycle(
        &mut self,
        input: &StrategicCycleInput,
    ) -> Result<(Arc<StrategicSnapshot>, StrategicCounters), StrategicError> {
        let prepared = self.prepare_cycle(input)?;
        self.commit_cycle(prepared)
    }

    /// Evaluate a complete cycle without mutating authoritative strategic state.
    pub fn prepare_cycle(
        &self,
        input: &StrategicCycleInput,
    ) -> Result<PreparedStrategicCycle, StrategicError> {
        if !input.force && !input.tick.is_multiple_of(PAY_CYCLE_TICKS) {
            return Err(StrategicError::NotDue);
        }
        let next_cycle = self
            .cycle
            .checked_add(1)
            .ok_or(StrategicError::CycleOverflow)?;
        self.validate_progress(input)?;
        let countries = validate_country_inputs(&input.countries, &self.economies)?;
        let occupation_inputs = validate_occupation_inputs(&input.occupations, &self.occupations)?;

        // Work entirely in private copies. An error cannot leak a partially
        // settled treasury, resistance value, or surrender decision.
        let mut next_economies = self.economies.clone();
        let mut next_occupations = self.occupations.clone();
        let mut due_by_annexer = BTreeMap::<u16, f64>::new();
        let mut yield_by_annexer = BTreeMap::<u16, f64>::new();
        let mut prepared_occupation = Vec::with_capacity(occupation_inputs.len());

        // Browser ordering: first resolve held territory, garrison, due, and
        // yield. Resistance waits until the annexer's new funding coverage is
        // known later in the same cycle.
        for record in occupation_inputs.values() {
            let state = next_occupations
                .get(&record.victim_id)
                .ok_or(StrategicError::UnknownCountry(record.victim_id))?;
            let held_ratio =
                (f64::from(record.held_cells) / f64::from(state.core_cells.max(1))).clamp(0.0, 1.0);
            let required = required_garrison(state.expected_army_units)?;
            let garrison_coverage =
                (record.garrison_strength.max(0.0) / f64::from(required)).clamp(0.0, 1.0);
            let due = state.base_income * crate::economy::OCCUPATION_COST_SHARE * held_ratio;
            let yield_amount =
                state.base_income * crate::economy::OCCUPATION_YIELD_SHARE * held_ratio;
            *due_by_annexer.entry(state.annexer_id).or_default() += due;
            *yield_by_annexer.entry(state.annexer_id).or_default() += yield_amount;
            prepared_occupation.push((
                *record,
                held_ratio,
                required,
                garrison_coverage,
                due,
                yield_amount,
            ));
        }

        let mut events = Vec::new();
        for (&country_id, country) in &countries {
            let state = next_economies
                .get(&country_id)
                .ok_or(StrategicError::UnknownCountry(country_id))?;
            if state.capitulated || !country.active {
                continue;
            }
            let core_ratio =
                f64::from(country.core_controlled) / f64::from(state.initial_core_cells.max(1));
            let city_ratio = if state.initial_city_population > 0.0 {
                country.city_population_controlled / state.initial_city_population
            } else {
                core_ratio
            };
            let income = compute_current_income(
                state.base_income,
                core_ratio,
                city_ratio,
                country.capital_held,
            )?;
            let previous_band = state.command_band;
            let previous_coverage = state.payroll_coverage;
            let settled = settle_economy_cycle(
                state,
                EconomyCycleInput {
                    income,
                    occupation_yield: yield_by_annexer.get(&country_id).copied().unwrap_or(0.0),
                    payroll_due: country.payroll_due,
                    occupation_due: due_by_annexer.get(&country_id).copied().unwrap_or(0.0),
                    core_control_ratio: core_ratio,
                    city_control_ratio: city_ratio,
                    capital_held: country.capital_held,
                },
            )?;
            if previous_coverage >= 0.999 && settled.payroll_coverage < 0.999 {
                events.push(StrategicEvent {
                    kind: StrategicEventKind::BudgetDeficit,
                    country_id: Some(country_id),
                    related_country_id: None,
                    previous_band: Some(previous_band),
                    next_band: Some(settled.command_band),
                    value: Some(settled.payroll_coverage),
                });
            }
            if previous_band != settled.command_band {
                events.push(StrategicEvent {
                    kind: StrategicEventKind::CommandBandChanged,
                    country_id: Some(country_id),
                    related_country_id: None,
                    previous_band: Some(previous_band),
                    next_band: Some(settled.command_band),
                    value: Some(settled.arrears_cycles),
                });
            }
            next_economies.insert(country_id, settled);
        }

        let mut assessments = Vec::with_capacity(prepared_occupation.len());
        for (record, held_ratio, required, garrison_coverage, due, yield_amount) in
            prepared_occupation
        {
            let mut state = next_occupations
                .get(&record.victim_id)
                .cloned()
                .ok_or(StrategicError::UnknownCountry(record.victim_id))?;
            let previous_resistance = state.resistance;
            let coverage = next_economies
                .get(&state.annexer_id)
                .map_or(0.0, |economy| economy.occupation_coverage);
            let delta = resistance_delta(coverage, garrison_coverage, record.casualty_pressure)?;
            state.held_ratio = held_ratio;
            state.required_garrison = required;
            state.garrison_assigned = record.garrison_strength.max(0.0);
            state.garrison_coverage = garrison_coverage;
            state.occupation_coverage = coverage;
            state.resistance = (state.resistance.max(0.0) + delta).clamp(0.0, 100.0);
            if previous_resistance < 75.0 && state.resistance >= 75.0 {
                events.push(StrategicEvent {
                    kind: StrategicEventKind::ResistanceWarning,
                    country_id: Some(state.victim_id),
                    related_country_id: Some(state.annexer_id),
                    previous_band: None,
                    next_band: None,
                    value: Some(state.resistance),
                });
            }
            assessments.push(OccupationAssessment {
                victim_id: state.victim_id,
                annexer_id: state.annexer_id,
                held_ratio,
                required_garrison: required,
                garrison_coverage,
                occupation_due: due,
                occupation_yield: yield_amount,
                resistance_delta: delta,
                resistance: state.resistance,
                rebellion_ready: state.resistance >= 100.0 && !state.active_rebellion,
            });
            next_occupations.insert(state.victim_id, state);
        }

        let active_sides = input.active_sides.iter().copied().collect::<BTreeSet<_>>();
        let active_hostile_pairs = input
            .active_hostile_pairs
            .iter()
            .filter_map(|&(left, right)| {
                (left != right && active_sides.contains(&left) && active_sides.contains(&right))
                    .then_some((left.min(right), left.max(right)))
            })
            .collect::<BTreeSet<_>>();
        let hostile_sides = active_hostile_pairs
            .iter()
            .flat_map(|&(left, right)| [left, right])
            .collect::<BTreeSet<_>>();
        let capitulation_active_sides = input
            .capitulation_active_sides
            .as_ref()
            .map(|sides| {
                sides
                    .iter()
                    .copied()
                    .filter(|side| active_sides.contains(side))
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_else(|| hostile_sides.clone());
        let mut decisions = BTreeMap::new();
        for (&country_id, country) in &countries {
            let decision = evaluate_capitulation(CapitulationInput {
                has_fresh_territory_data: input.territory_fresh,
                is_rebel: country.is_rebel,
                unit_count: country.unit_count,
                owned_cells: f64::from(country.owned_cells),
                controlled_cells: f64::from(country.controlled_cells),
                initial_cells: f64::from(country.initial_cells),
            })?;
            decisions.insert(country_id, decision);
        }

        // Browser ordering is side ascending and country array reverse. The
        // immutable input preserves that country order. Only the first active,
        // hostile-side capitulation becomes a command; the caller applies it
        // and re-enters on the next tick with updated sides and territory.
        let mut candidate_order = input.countries.iter().enumerate().collect::<Vec<_>>();
        candidate_order.sort_by(|(left_index, left), (right_index, right)| {
            left.side
                .cmp(&right.side)
                .then_with(|| right_index.cmp(left_index))
        });
        let surrender_country = candidate_order
            .into_iter()
            .find(|(_, country)| {
                country.active
                    && active_sides.contains(&country.side)
                    && capitulation_active_sides.contains(&country.side)
                    && next_economies
                        .get(&country.country_id)
                        .is_some_and(|economy| !economy.capitulated)
                    && decisions
                        .get(&country.country_id)
                        .is_some_and(|decision| decision.capitulate)
            })
            .map(|(_, country)| country.country_id);

        let mut country_snapshots = Vec::with_capacity(countries.len());
        let mut desertions = Vec::new();
        let mut surrenders = Vec::new();
        for (&country_id, country) in &countries {
            let mut economy = next_economies
                .get(&country_id)
                .cloned()
                .ok_or(StrategicError::UnknownCountry(country_id))?;
            let decision = decisions[&country_id];
            if surrender_country == Some(country_id) {
                economy.capitulated = true;
                next_economies.insert(country_id, economy.clone());
                surrenders.push(SurrenderCommand {
                    country_id,
                    side: country.side,
                    decision,
                });
                events.push(StrategicEvent {
                    kind: StrategicEventKind::CapitulationTriggered,
                    country_id: Some(country_id),
                    related_country_id: None,
                    previous_band: None,
                    next_band: None,
                    value: Some(decision.control_percent),
                });
            }
            let rate = desertion_rate(economy.command_band);
            if country.active && rate > 0.0 && country.unit_count > 0 {
                desertions.push(DesertionCommand { country_id, rate });
            }
            country_snapshots.push(CountryStrategicSnapshot {
                country_id,
                side: country.side,
                economy,
                capitulation: decision,
            });
        }
        let resolution = if surrender_country.is_some() {
            None
        } else {
            let normalized_active_sides = active_sides.iter().copied().collect::<Vec<_>>();
            let normalized_hostile_pairs = active_hostile_pairs.iter().copied().collect::<Vec<_>>();
            evaluate_global_conflict(&normalized_active_sides, &normalized_hostile_pairs)
        };
        if resolution.is_some() {
            events.push(StrategicEvent {
                kind: StrategicEventKind::TreatyResolved,
                country_id: None,
                related_country_id: None,
                previous_band: None,
                next_band: None,
                value: None,
            });
        }

        let snapshot = Arc::new(StrategicSnapshot {
            schema_version: STRATEGIC_SCHEMA_VERSION.to_owned(),
            cycle: next_cycle,
            tick: input.tick,
            territory_generation: input.territory_generation,
            territory_commit_sequence: input.territory_commit_sequence,
            countries: country_snapshots.into(),
            occupations: next_occupations
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .into(),
            occupation_assessments: assessments.into(),
            desertions: desertions.into(),
            surrenders: surrenders.into(),
            events: events.into(),
            conflict_resolution: resolution,
        });
        let counters = StrategicCounters {
            countries_processed: snapshot.countries.len(),
            occupations_processed: snapshot.occupation_assessments.len(),
            capitulations: snapshot.surrenders.len(),
            desertion_commands: snapshot.desertions.len(),
            events: snapshot.events.len(),
        };
        Ok(PreparedStrategicCycle {
            base_cycle: self.cycle,
            next_economies,
            next_occupations,
            snapshot,
            counters,
        })
    }

    /// Atomically publish a previously evaluated cycle.
    pub fn commit_cycle(
        &mut self,
        prepared: PreparedStrategicCycle,
    ) -> Result<(Arc<StrategicSnapshot>, StrategicCounters), StrategicError> {
        if prepared.base_cycle != self.cycle {
            return Err(StrategicError::StalePreparedCycle {
                prepared: prepared.base_cycle,
                current: self.cycle,
            });
        }
        self.cycle = prepared.snapshot.cycle;
        self.economies = prepared.next_economies;
        self.occupations = prepared.next_occupations;
        self.latest = Some(prepared.snapshot.clone());
        Ok((prepared.snapshot, prepared.counters))
    }

    fn validate_progress(&self, input: &StrategicCycleInput) -> Result<(), StrategicError> {
        let Some(previous) = &self.latest else {
            return Ok(());
        };
        if input.tick <= previous.tick {
            return Err(StrategicError::NonMonotonicTick {
                previous: previous.tick,
                received: input.tick,
            });
        }
        if input.territory_generation < previous.territory_generation {
            return Err(StrategicError::NonMonotonicTerritoryGeneration {
                previous: previous.territory_generation,
                received: input.territory_generation,
            });
        }
        if input.territory_commit_sequence < previous.territory_commit_sequence {
            return Err(StrategicError::NonMonotonicTerritoryCommitSequence {
                previous: previous.territory_commit_sequence,
                received: input.territory_commit_sequence,
            });
        }
        Ok(())
    }
}

/// Reproduce the browser capitulation allocator without mutating territory.
/// Existing hostile primary occupation always wins its cell; only remaining
/// victim-owned cells consume casualty-weighted quotas.
pub fn plan_surrender_allocation(
    input: SurrenderAllocationInput<'_>,
) -> Result<SurrenderAllocationPlan, StrategicError> {
    let total_cells =
        input
            .width
            .checked_mul(input.height)
            .ok_or(StrategicError::InvalidSurrenderAllocation(
                "grid dimensions overflow",
            ))?;
    if input.victim_country_id == 0
        || input.width == 0
        || input.height == 0
        || !input.grid_resolution.is_finite()
        || input.grid_resolution <= 0.0
        || input.land.len() != total_cells
        || input.world_control.len() != total_cells
        || input.de_jure.len() != total_cells
        || input.primary_occupier.len() != total_cells
    {
        return Err(StrategicError::InvalidSurrenderAllocation(
            "map dimensions or victim are invalid",
        ));
    }
    if input.units.iter().any(|unit| {
        unit.country_id == 0
            || ![unit.lat, unit.lng, unit.health]
                .into_iter()
                .all(f64::is_finite)
    }) {
        return Err(StrategicError::InvalidSurrenderAllocation(
            "unit position is invalid",
        ));
    }

    let hostile = input
        .hostile_attacker_ids
        .iter()
        .copied()
        .filter(|country_id| *country_id > 0 && *country_id != input.victim_country_id)
        .collect::<BTreeSet<_>>();
    if hostile.is_empty() {
        return Err(StrategicError::NoHostileRecipient);
    }
    let casualty_row = input.casualties_by_victim.get(&input.victim_country_id);
    if casualty_row.is_some_and(|row| {
        row.iter()
            .any(|(&country_id, &loss)| country_id == 0 || !loss.is_finite() || loss < 0.0)
    }) {
        return Err(StrategicError::InvalidSurrenderAllocation(
            "casualty row is invalid",
        ));
    }
    let casualty = |country_id: u16| {
        casualty_row
            .and_then(|row| row.get(&country_id))
            .copied()
            .unwrap_or(0.0)
    };
    let entries = hostile
        .iter()
        .copied()
        .map(|country_id| CasualtyEntry {
            country_id,
            casualties: casualty(country_id),
        })
        .collect::<Vec<_>>();
    let mut selected = eligible_casualty_attackers(&entries, 0.25)?;

    let mut victim_owned_cells = Vec::new();
    let mut physical_control_counts = BTreeMap::<u16, u64>::new();
    let mut victim_lat_sum = 0.0;
    let mut victim_lng_sum = 0.0;
    let mut victim_core_count = 0_u64;
    let mut unoccupied_count = 0_u64;
    for cell in 0..total_cells {
        if input.de_jure[cell] == input.victim_country_id && input.land[cell] > 0 {
            let (lat, lng) = cell_position(cell, input.width, input.grid_resolution);
            victim_lat_sum += lat;
            victim_lng_sum += lng;
            victim_core_count += 1;
            let controller = if input.primary_occupier[cell] > 0 {
                input.primary_occupier[cell]
            } else {
                input.world_control[cell]
            };
            if hostile.contains(&controller) {
                *physical_control_counts.entry(controller).or_default() += 1;
            }
        }
        if input.world_control[cell] == input.victim_country_id && input.land[cell] > 0 {
            victim_owned_cells.push(cell);
            if !hostile.contains(&input.primary_occupier[cell]) {
                unoccupied_count += 1;
            }
        }
    }
    let victim_center = if victim_core_count > 0 {
        (
            victim_lat_sum / victim_core_count as f64,
            victim_lng_sum / victim_core_count as f64,
        )
    } else {
        (0.0, 0.0)
    };

    if selected.is_empty() {
        let physical = physical_control_counts
            .iter()
            .max_by(|(left_id, left_count), (right_id, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| casualty(**left_id).total_cmp(&casualty(**right_id)))
                    .then_with(|| right_id.cmp(left_id))
            })
            .map(|(&country_id, _)| country_id);
        let fallback = physical.or_else(|| {
            input
                .units
                .iter()
                .filter(|unit| hostile.contains(&unit.country_id) && unit.health > 0.0)
                .min_by(|left, right| {
                    distance_to_center(left, victim_center)
                        .total_cmp(&distance_to_center(right, victim_center))
                        .then_with(|| left.country_id.cmp(&right.country_id))
                })
                .map(|unit| unit.country_id)
        });
        let country_id = fallback.ok_or(StrategicError::NoDeterministicRecipient)?;
        selected.push(crate::surrender::CasualtyShare {
            country_id,
            casualties: casualty(country_id),
            share: 1.0,
        });
    }

    let quotas = largest_remainder_quotas(&selected, unoccupied_count)?;
    let selected_ids = selected
        .iter()
        .map(|entry| entry.country_id)
        .collect::<BTreeSet<_>>();
    let mut centroids = BTreeMap::<u16, (f64, f64, u64)>::new();
    for country_id in &selected_ids {
        centroids.insert(*country_id, (0.0, 0.0, 0));
    }
    for cell in 0..total_cells {
        if let Some(centroid) = centroids.get_mut(&input.world_control[cell]) {
            let (lat, lng) = cell_position(cell, input.width, input.grid_resolution);
            centroid.0 += lat;
            centroid.1 += lng;
            centroid.2 += 1;
        }
    }
    let centroids = centroids
        .into_iter()
        .map(|(country_id, (lat_sum, lng_sum, count))| {
            let center = if count > 0 {
                (lat_sum / count as f64, lng_sum / count as f64)
            } else {
                victim_center
            };
            (country_id, center)
        })
        .collect::<BTreeMap<_, _>>();
    let quota_by_country = quotas
        .iter()
        .map(|quota| (quota.country_id, quota.quota))
        .collect::<BTreeMap<_, _>>();
    let mut used_by_country = BTreeMap::<u16, u32>::new();
    let mut assignments = BTreeMap::<usize, u16>::new();
    for &cell in &victim_owned_cells {
        let physical = input.primary_occupier[cell];
        if hostile.contains(&physical) {
            assignments.insert(cell, physical);
            continue;
        }
        let cell_center = cell_position(cell, input.width, input.grid_resolution);
        let recipient = selected
            .iter()
            .filter(|entry| {
                used_by_country.get(&entry.country_id).copied().unwrap_or(0)
                    < quota_by_country
                        .get(&entry.country_id)
                        .copied()
                        .unwrap_or(0)
            })
            .min_by(|left, right| {
                wrapped_distance_squared(cell_center, centroids[&left.country_id])
                    .total_cmp(&wrapped_distance_squared(
                        cell_center,
                        centroids[&right.country_id],
                    ))
                    .then_with(|| left.country_id.cmp(&right.country_id))
            })
            .map(|entry| entry.country_id)
            .ok_or(StrategicError::IncompleteSurrenderAllocation)?;
        assignments.insert(cell, recipient);
        *used_by_country.entry(recipient).or_default() += 1;
    }

    let mut final_control_counts = BTreeMap::<u16, u64>::new();
    for cell in 0..total_cells {
        if input.de_jure[cell] != input.victim_country_id || input.land[cell] == 0 {
            continue;
        }
        let controller = assignments
            .get(&cell)
            .copied()
            .unwrap_or(input.world_control[cell]);
        if hostile.contains(&controller) {
            *final_control_counts.entry(controller).or_default() += 1;
        }
    }
    let primary_annexer_id = final_control_counts
        .iter()
        .max_by(|(left_id, left_count), (right_id, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| casualty(**left_id).total_cmp(&casualty(**right_id)))
                .then_with(|| right_id.cmp(left_id))
        })
        .map(|(&country_id, _)| country_id)
        .or_else(|| selected.first().map(|entry| entry.country_id))
        .ok_or(StrategicError::NoDeterministicRecipient)?;
    let transfers = victim_owned_cells
        .into_iter()
        .map(|cell| SurrenderCellTransfer {
            cell,
            original_owner: input.victim_country_id,
            new_owner: assignments[&cell],
        })
        .collect();
    Ok(SurrenderAllocationPlan {
        victim_country_id: input.victim_country_id,
        primary_annexer_id,
        transfers,
    })
}

fn cell_position(cell: usize, width: usize, resolution: f64) -> (f64, f64) {
    let y = cell / width;
    let x = cell % width;
    (y as f64 * resolution - 90.0, x as f64 * resolution - 180.0)
}

fn distance_to_center(unit: &SurrenderUnitPosition, center: (f64, f64)) -> f64 {
    wrapped_distance_squared((unit.lat, unit.lng), center)
}

fn wrapped_distance_squared(left: (f64, f64), right: (f64, f64)) -> f64 {
    let dlat = left.0 - right.0;
    let mut dlng = left.1 - right.1;
    if dlng > 180.0 {
        dlng -= 360.0;
    } else if dlng < -180.0 {
        dlng += 360.0;
    }
    dlat * dlat + dlng * dlng
}

fn validate_country_inputs(
    inputs: &[CountryCycleInput],
    economies: &BTreeMap<u16, EconomyState>,
) -> Result<BTreeMap<u16, CountryCycleInput>, StrategicError> {
    let mut result = BTreeMap::new();
    for input in inputs {
        if input.country_id == 0
            || ![input.payroll_due, input.city_population_controlled]
                .into_iter()
                .all(f64::is_finite)
            || input.payroll_due < 0.0
            || input.city_population_controlled < 0.0
        {
            return Err(StrategicError::InvalidCountryInput);
        }
        if !economies.contains_key(&input.country_id) {
            return Err(StrategicError::UnknownCountry(input.country_id));
        }
        if result.insert(input.country_id, *input).is_some() {
            return Err(StrategicError::DuplicateCountry(input.country_id));
        }
    }
    Ok(result)
}

fn validate_occupation_inputs(
    inputs: &[OccupationCycleRecord],
    occupations: &BTreeMap<u16, OccupationState>,
) -> Result<BTreeMap<u16, OccupationCycleRecord>, StrategicError> {
    let mut result = BTreeMap::new();
    for input in inputs {
        if input.victim_id == 0
            || ![input.garrison_strength, input.casualty_pressure]
                .into_iter()
                .all(f64::is_finite)
            || input.garrison_strength < 0.0
        {
            return Err(StrategicError::InvalidCountryInput);
        }
        if !occupations.contains_key(&input.victim_id) {
            return Err(StrategicError::UnknownCountry(input.victim_id));
        }
        if result.insert(input.victim_id, *input).is_some() {
            return Err(StrategicError::DuplicateOccupation(input.victim_id));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::economy::{EconomySeed, create_economy_state};

    fn economy(country_id: u16) -> EconomyState {
        create_economy_state(EconomySeed {
            country_id,
            gdp: 100.0,
            population: 0.0,
            territory_units: 0.0,
            initial_core_cells: 100,
            initial_city_population: 100.0,
        })
        .unwrap()
    }

    fn occupation() -> OccupationState {
        OccupationState {
            victim_id: 2,
            annexer_id: 1,
            base_income: 100.0,
            core_cells: 100,
            expected_army_units: 20.0,
            resistance: 70.0,
            occupation_coverage: 1.0,
            garrison_coverage: 1.0,
            garrison_assigned: 3.0,
            required_garrison: 3,
            held_ratio: 1.0,
            active_rebellion: false,
            queued_at_cycle: 0,
            cooldown_until_cycle: 0,
        }
    }

    fn input() -> StrategicCycleInput {
        StrategicCycleInput {
            tick: 600,
            force: false,
            territory_generation: 1,
            territory_commit_sequence: 1,
            territory_fresh: true,
            countries: vec![
                CountryCycleInput {
                    country_id: 1,
                    side: 0,
                    owned_cells: 100,
                    controlled_cells: 100,
                    core_controlled: 100,
                    initial_cells: 100,
                    city_population_controlled: 100.0,
                    unit_count: 1,
                    payroll_due: 10.0,
                    capital_held: true,
                    is_rebel: false,
                    active: true,
                },
                CountryCycleInput {
                    country_id: 2,
                    side: 1,
                    owned_cells: 100,
                    controlled_cells: 1,
                    core_controlled: 1,
                    initial_cells: 100,
                    city_population_controlled: 0.0,
                    unit_count: 1,
                    payroll_due: 10.0,
                    capital_held: false,
                    is_rebel: false,
                    active: true,
                },
            ],
            occupations: vec![OccupationCycleRecord {
                victim_id: 2,
                held_cells: 99,
                garrison_strength: 0.0,
                casualty_pressure: 1.0,
            }],
            active_sides: vec![0, 1],
            active_hostile_pairs: vec![(0, 1)],
            capitulation_active_sides: None,
        }
    }

    #[test]
    fn cycle_orders_funding_before_resistance_and_surrender() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let (snapshot, counters) = simulation.run_cycle(&input()).unwrap();
        assert_eq!(snapshot.cycle, 1);
        assert_eq!(counters.occupations_processed, 1);
        assert!(snapshot.occupation_assessments[0].resistance >= 75.0);
        assert!(snapshot.countries[1].capitulation.capitulate);
        assert_eq!(counters.capitulations, 1);
    }

    #[test]
    fn invalid_late_input_is_atomic() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let before = simulation.economies().clone();
        let mut invalid = input();
        invalid.occupations[0].casualty_pressure = f64::NAN;
        assert!(simulation.run_cycle(&invalid).is_err());
        assert_eq!(simulation.cycle(), 0);
        assert_eq!(simulation.economies(), &before);
        assert!(simulation.latest_snapshot().is_none());
    }

    #[test]
    fn not_due_does_not_mutate() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let mut not_due = input();
        not_due.tick = 601;
        assert!(matches!(
            simulation.run_cycle(&not_due),
            Err(StrategicError::NotDue)
        ));
        assert_eq!(simulation.cycle(), 0);
    }

    #[test]
    fn first_cycle_accepts_zero_progress_markers() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let mut first = input();
        first.tick = 0;
        first.territory_generation = 0;
        first.territory_commit_sequence = 0;

        let (snapshot, _) = simulation.run_cycle(&first).unwrap();
        assert_eq!(snapshot.tick, 0);
        assert_eq!(snapshot.territory_generation, 0);
        assert_eq!(snapshot.territory_commit_sequence, 0);
    }

    #[test]
    fn restore_preserves_the_browser_pay_cycle_coordinate() {
        let mut simulation =
            StrategicSimulation::restore(37, [economy(1), economy(2)], [occupation()]).unwrap();
        assert_eq!(simulation.cycle(), 37);

        let (snapshot, _) = simulation.run_cycle(&input()).unwrap();
        assert_eq!(snapshot.cycle, 38);
        assert_eq!(simulation.cycle(), 38);
    }

    #[test]
    fn cycle_overflow_is_rejected_before_mutation() {
        let mut simulation =
            StrategicSimulation::restore(u64::MAX, [economy(1), economy(2)], [occupation()])
                .unwrap();
        let economies = simulation.economies().clone();
        let occupations = simulation.occupations().clone();

        assert!(matches!(
            simulation.run_cycle(&input()),
            Err(StrategicError::CycleOverflow)
        ));
        assert_eq!(simulation.cycle(), u64::MAX);
        assert_eq!(simulation.economies(), &economies);
        assert_eq!(simulation.occupations(), &occupations);
        assert!(simulation.latest_snapshot().is_none());
    }

    #[test]
    fn duplicate_and_regressing_ticks_are_rejected_atomically() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let (published, _) = simulation.run_cycle(&input()).unwrap();
        let economies = simulation.economies().clone();
        let occupations = simulation.occupations().clone();

        let mut duplicate = input();
        duplicate.territory_generation = 2;
        duplicate.territory_commit_sequence = 2;
        assert!(matches!(
            simulation.run_cycle(&duplicate),
            Err(StrategicError::NonMonotonicTick {
                previous: 600,
                received: 600
            })
        ));

        let mut regressing = duplicate;
        regressing.tick = 0;
        assert!(matches!(
            simulation.run_cycle(&regressing),
            Err(StrategicError::NonMonotonicTick {
                previous: 600,
                received: 0
            })
        ));
        assert_eq!(simulation.cycle(), 1);
        assert_eq!(simulation.economies(), &economies);
        assert_eq!(simulation.occupations(), &occupations);
        assert!(Arc::ptr_eq(
            &simulation.latest_snapshot().unwrap(),
            &published
        ));
    }

    #[test]
    fn stable_territory_markers_are_allowed_but_regressions_are_atomic() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        simulation.run_cycle(&input()).unwrap();

        let mut stable = input();
        stable.tick = 1_200;
        let (stable_snapshot, _) = simulation.run_cycle(&stable).unwrap();
        assert_eq!(stable_snapshot.territory_generation, 1);
        assert_eq!(stable_snapshot.territory_commit_sequence, 1);

        let economies = simulation.economies().clone();
        let occupations = simulation.occupations().clone();
        let published = simulation.latest_snapshot().unwrap();
        let mut regressing_generation = input();
        regressing_generation.tick = 1_800;
        regressing_generation.territory_generation = 0;
        assert!(matches!(
            simulation.run_cycle(&regressing_generation),
            Err(StrategicError::NonMonotonicTerritoryGeneration {
                previous: 1,
                received: 0
            })
        ));

        let mut regressing_commit = input();
        regressing_commit.tick = 1_800;
        regressing_commit.territory_generation = 2;
        regressing_commit.territory_commit_sequence = 0;
        assert!(matches!(
            simulation.run_cycle(&regressing_commit),
            Err(StrategicError::NonMonotonicTerritoryCommitSequence {
                previous: 1,
                received: 0
            })
        ));
        assert_eq!(simulation.cycle(), 2);
        assert_eq!(simulation.economies(), &economies);
        assert_eq!(simulation.occupations(), &occupations);
        assert!(Arc::ptr_eq(
            &simulation.latest_snapshot().unwrap(),
            &published
        ));
    }

    #[test]
    fn inactive_or_non_hostile_country_cannot_capitulate() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let mut peaceful = input();
        peaceful.active_hostile_pairs.clear();
        peaceful.countries[1].active = false;
        peaceful.countries[1].owned_cells = 0;
        peaceful.countries[1].controlled_cells = 0;

        let (snapshot, counters) = simulation.run_cycle(&peaceful).unwrap();
        assert!(snapshot.surrenders.is_empty());
        assert_eq!(counters.capitulations, 0);
        assert!(!simulation.economies()[&2].capitulated);
        assert_eq!(
            snapshot.conflict_resolution.map(|value| value.kind),
            Some(crate::surrender::ConflictResolutionKind::WhitePeace)
        );
    }

    #[test]
    fn directed_capitulation_eligibility_requires_outgoing_hostility() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let mut cycle = input();
        // The canonical pair keeps the war active, but only side 0 has an
        // outgoing hostility edge. Side 1 therefore cannot capitulate.
        cycle.capitulation_active_sides = Some(vec![0]);

        let (snapshot, counters) = simulation.run_cycle(&cycle).unwrap();
        assert!(snapshot.surrenders.is_empty());
        assert_eq!(counters.capitulations, 0);
        assert!(!simulation.economies()[&2].capitulated);
        assert!(snapshot.conflict_resolution.is_none());
    }

    #[test]
    fn inactive_country_cannot_emit_desertion_commands() {
        let mut inactive_economy = economy(2);
        inactive_economy.treasury = 0.0;
        inactive_economy.arrears_cycles = 5.0;
        inactive_economy.command_band = crate::economy::CommandBand::Mutiny;
        let mut simulation =
            StrategicSimulation::new([economy(1), inactive_economy], [occupation()]).unwrap();
        let mut cycle = input();
        cycle.countries[1].active = false;

        let (snapshot, _) = simulation.run_cycle(&cycle).unwrap();
        assert_eq!(
            simulation.economies()[&2].command_band,
            crate::economy::CommandBand::Mutiny
        );
        assert!(snapshot.desertions.is_empty());
    }

    #[test]
    fn stale_unrelated_hostile_pair_cannot_suppress_white_peace() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let mut peaceful = input();
        peaceful.active_hostile_pairs = vec![(8, 9), (9, 8), (0, 7), (1, 1)];

        let (snapshot, counters) = simulation.run_cycle(&peaceful).unwrap();
        assert!(snapshot.surrenders.is_empty());
        assert_eq!(counters.capitulations, 0);
        assert_eq!(
            snapshot.conflict_resolution.map(|value| value.kind),
            Some(crate::surrender::ConflictResolutionKind::WhitePeace)
        );
    }

    #[test]
    fn only_first_browser_order_capitulation_is_published() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2), economy(3)], []).unwrap();
        let mut cycle = input();
        cycle.occupations.clear();
        cycle.countries[0].owned_cells = 0;
        cycle.countries[0].controlled_cells = 0;
        cycle.countries.push(CountryCycleInput {
            country_id: 3,
            side: 0,
            owned_cells: 0,
            controlled_cells: 0,
            core_controlled: 0,
            initial_cells: 100,
            city_population_controlled: 0.0,
            unit_count: 0,
            payroll_due: 0.0,
            capital_held: false,
            is_rebel: false,
            active: true,
        });

        let (snapshot, counters) = simulation.run_cycle(&cycle).unwrap();
        assert_eq!(counters.capitulations, 1);
        assert_eq!(snapshot.surrenders.len(), 1);
        assert_eq!(snapshot.surrenders[0].country_id, 3);
        assert!(snapshot.conflict_resolution.is_none());
        assert!(!simulation.economies()[&1].capitulated);
        assert!(simulation.economies()[&3].capitulated);
    }

    #[test]
    fn prepared_cycle_is_inert_until_committed() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let before_economies = simulation.economies().clone();
        let prepared = simulation.prepare_cycle(&input()).unwrap();

        assert_eq!(prepared.snapshot().cycle, 1);
        assert_eq!(prepared.counters().capitulations, 1);
        assert_eq!(simulation.cycle(), 0);
        assert_eq!(simulation.economies(), &before_economies);
        assert!(simulation.latest_snapshot().is_none());

        let (published, counters) = simulation.commit_cycle(prepared).unwrap();
        assert_eq!(published.cycle, 1);
        assert_eq!(counters.capitulations, 1);
        assert!(simulation.economies()[&2].capitulated);
    }

    #[test]
    fn stale_prepared_cycle_cannot_overwrite_a_committed_cycle() {
        let mut simulation =
            StrategicSimulation::new([economy(1), economy(2)], [occupation()]).unwrap();
        let first = simulation.prepare_cycle(&input()).unwrap();
        let stale = simulation.prepare_cycle(&input()).unwrap();
        simulation.commit_cycle(first).unwrap();
        let economies = simulation.economies().clone();
        let published = simulation.latest_snapshot().unwrap();

        assert!(matches!(
            simulation.commit_cycle(stale),
            Err(StrategicError::StalePreparedCycle {
                prepared: 0,
                current: 1
            })
        ));
        assert_eq!(simulation.economies(), &economies);
        assert!(Arc::ptr_eq(
            &simulation.latest_snapshot().unwrap(),
            &published
        ));
    }

    #[test]
    fn prepared_cycle_registers_new_occupation_in_state_and_publication() {
        let simulation = StrategicSimulation::new([economy(1), economy(2)], []).unwrap();
        let mut cycle = input();
        cycle.occupations.clear();
        let mut prepared = simulation.prepare_cycle(&cycle).unwrap();
        let mut state = occupation();
        state.queued_at_cycle = prepared.snapshot().cycle;

        assert!(prepared.economy(1).is_some());
        prepared.register_occupation(state.clone()).unwrap();
        assert_eq!(&*prepared.snapshot().occupations, &[state.clone()]);
        assert!(matches!(
            prepared.register_occupation(state),
            Err(StrategicError::DuplicateOccupation(2))
        ));
    }

    #[test]
    fn surrender_plan_preserves_physical_occupation_then_uses_quotas_and_centroids() {
        let land = vec![1; 10];
        let mut world_control = vec![2; 10];
        world_control[0] = 1;
        world_control[9] = 3;
        let de_jure = vec![2; 10];
        let mut primary_occupier = vec![0; 10];
        primary_occupier[2] = 3;
        let ledger = BTreeMap::from([(2, BTreeMap::from([(1, 75.0), (3, 25.0)]))]);
        let plan = plan_surrender_allocation(SurrenderAllocationInput {
            victim_country_id: 2,
            hostile_attacker_ids: &[3, 1, 3],
            casualties_by_victim: &ledger,
            width: 10,
            height: 1,
            grid_resolution: 1.0,
            land: &land,
            world_control: &world_control,
            de_jure: &de_jure,
            primary_occupier: &primary_occupier,
            units: &[],
        })
        .unwrap();

        assert_eq!(plan.victim_country_id, 2);
        assert_eq!(plan.primary_annexer_id, 1);
        assert_eq!(plan.transfers.len(), 8);
        let recipients = plan
            .transfers
            .iter()
            .map(|transfer| (transfer.cell, transfer.new_owner))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(recipients[&2], 3);
        assert_eq!(recipients.values().filter(|&&id| id == 1).count(), 5);
        assert_eq!(recipients.values().filter(|&&id| id == 3).count(), 3);
    }

    #[test]
    fn surrender_fallback_prefers_physical_control_then_nearest_live_unit() {
        let land = vec![1; 4];
        let world_control = vec![2; 4];
        let de_jure = vec![2; 4];
        let mut occupied = vec![0; 4];
        occupied[0] = 4;
        occupied[1] = 4;
        occupied[2] = 3;
        let empty_ledger = BTreeMap::new();
        let physical = plan_surrender_allocation(SurrenderAllocationInput {
            victim_country_id: 2,
            hostile_attacker_ids: &[3, 4],
            casualties_by_victim: &empty_ledger,
            width: 4,
            height: 1,
            grid_resolution: 1.0,
            land: &land,
            world_control: &world_control,
            de_jure: &de_jure,
            primary_occupier: &occupied,
            units: &[],
        })
        .unwrap();
        assert_eq!(physical.primary_annexer_id, 4);

        let clear = vec![0; 4];
        let units = [
            SurrenderUnitPosition {
                country_id: 4,
                lat: -90.0,
                lng: -178.5,
                health: 100.0,
            },
            SurrenderUnitPosition {
                country_id: 3,
                lat: -90.0,
                lng: -178.5,
                health: 100.0,
            },
        ];
        let nearest = plan_surrender_allocation(SurrenderAllocationInput {
            victim_country_id: 2,
            hostile_attacker_ids: &[4, 3],
            casualties_by_victim: &empty_ledger,
            width: 4,
            height: 1,
            grid_resolution: 1.0,
            land: &land,
            world_control: &world_control,
            de_jure: &de_jure,
            primary_occupier: &clear,
            units: &units,
        })
        .unwrap();
        assert_eq!(nearest.primary_annexer_id, 3);
        assert!(
            nearest
                .transfers
                .iter()
                .all(|transfer| transfer.new_owner == 3)
        );
    }

    #[test]
    fn occupation_requires_known_victim_and_annexer_economies() {
        assert!(matches!(
            StrategicSimulation::new([economy(2)], [occupation()]),
            Err(StrategicError::UnknownCountry(1))
        ));
        assert!(matches!(
            StrategicSimulation::new([economy(1)], [occupation()]),
            Err(StrategicError::UnknownCountry(2))
        ));
    }
}
