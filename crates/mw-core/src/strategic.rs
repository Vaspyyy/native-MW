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
        CapitulationDecision, CapitulationInput, ConflictResolution, SurrenderError,
        evaluate_capitulation, evaluate_global_conflict,
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
                    && hostile_sides.contains(&country.side)
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

        self.cycle = next_cycle;
        self.economies = next_economies;
        self.occupations = next_occupations;
        let snapshot = Arc::new(StrategicSnapshot {
            schema_version: STRATEGIC_SCHEMA_VERSION.to_owned(),
            cycle: self.cycle,
            tick: input.tick,
            territory_generation: input.territory_generation,
            territory_commit_sequence: input.territory_commit_sequence,
            countries: country_snapshots.into(),
            occupations: self
                .occupations
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
        self.latest = Some(snapshot.clone());
        Ok((snapshot, counters))
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
