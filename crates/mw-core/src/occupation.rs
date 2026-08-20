//! Deterministic occupation funding, garrison, and resistance consequences.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::economy::{OCCUPATION_COST_SHARE, OCCUPATION_YIELD_SHARE};

pub const OCCUPATION_SCHEMA_VERSION: &str = "occupation-v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OccupationState {
    pub victim_id: u16,
    pub annexer_id: u16,
    pub base_income: f64,
    pub core_cells: u32,
    pub expected_army_units: f64,
    pub resistance: f64,
    pub occupation_coverage: f64,
    pub garrison_coverage: f64,
    pub garrison_assigned: f64,
    pub required_garrison: u32,
    pub held_ratio: f64,
    pub active_rebellion: bool,
    pub queued_at_cycle: u64,
    pub cooldown_until_cycle: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OccupationCycleInput {
    pub held_cells: u32,
    pub garrison_strength: f64,
    pub annexer_occupation_coverage: f64,
    pub casualty_pressure: f64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OccupationAssessment {
    pub victim_id: u16,
    pub annexer_id: u16,
    pub held_ratio: f64,
    pub required_garrison: u32,
    pub garrison_coverage: f64,
    pub occupation_due: f64,
    pub occupation_yield: f64,
    pub resistance_delta: f64,
    pub resistance: f64,
    pub rebellion_ready: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OccupationControllerCandidate {
    pub country_id: u16,
    pub controlled_cells: u32,
    pub casualties: f64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RebellionCandidate {
    pub victim_id: u16,
    pub annexer_id: u16,
    pub resistance: f64,
    pub active: bool,
    pub queued_at_cycle: u64,
    pub cooldown_until_cycle: u64,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum OccupationError {
    #[error("country ids must be positive and distinct")]
    InvalidCountries,
    #[error("occupation input contains a non-finite number")]
    NonFinite,
}

pub fn required_garrison(expected_army_units: f64) -> Result<u32, OccupationError> {
    if !expected_army_units.is_finite() {
        return Err(OccupationError::NonFinite);
    }
    Ok((expected_army_units.max(0.0) * 0.15).ceil().max(3.0) as u32)
}

pub fn resistance_delta(
    occupation_coverage: f64,
    garrison_coverage: f64,
    casualty_pressure: f64,
) -> Result<f64, OccupationError> {
    if ![occupation_coverage, garrison_coverage, casualty_pressure]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(OccupationError::NonFinite);
    }
    let funding = occupation_coverage.clamp(0.0, 1.0);
    let garrison = garrison_coverage.clamp(0.0, 1.0);
    let casualties = casualty_pressure.clamp(0.0, 1.0);
    Ok(
        (12.0 * (1.0 - funding) + 6.0 * (1.0 - garrison) + 4.0 * casualties
            - 4.0 * funding * garrison)
            .clamp(-4.0, 22.0),
    )
}

pub fn garrison_priority(
    resistance: f64,
    garrison_coverage: f64,
    held_ratio: f64,
    required_garrison: u32,
) -> Result<f64, OccupationError> {
    if ![resistance, garrison_coverage, held_ratio]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(OccupationError::NonFinite);
    }
    Ok(45.0
        + resistance.clamp(0.0, 100.0) * 0.65
        + (1.0 - garrison_coverage.clamp(0.0, 1.0)) * 35.0
        + held_ratio.clamp(0.0, 1.0) * 5.0
        + (f64::from(required_garrison) * 0.5).min(10.0))
}

pub fn assess_occupation(
    state: &OccupationState,
    input: OccupationCycleInput,
) -> Result<(OccupationState, OccupationAssessment), OccupationError> {
    validate_state(state)?;
    if ![
        input.garrison_strength,
        input.annexer_occupation_coverage,
        input.casualty_pressure,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err(OccupationError::NonFinite);
    }
    let mut next = state.clone();
    let core_cells = state.core_cells.max(1);
    let held_ratio = (f64::from(input.held_cells) / f64::from(core_cells)).clamp(0.0, 1.0);
    let required = required_garrison(state.expected_army_units)?;
    let garrison_coverage =
        (input.garrison_strength.max(0.0) / f64::from(required)).clamp(0.0, 1.0);
    let coverage = input.annexer_occupation_coverage.clamp(0.0, 1.0);
    let due = state.base_income * OCCUPATION_COST_SHARE * held_ratio;
    let yield_amount = state.base_income * OCCUPATION_YIELD_SHARE * held_ratio;
    let delta = resistance_delta(coverage, garrison_coverage, input.casualty_pressure)?;
    let resistance = (state.resistance.max(0.0) + delta).clamp(0.0, 100.0);

    next.held_ratio = held_ratio;
    next.required_garrison = required;
    next.garrison_assigned = input.garrison_strength.max(0.0);
    next.garrison_coverage = garrison_coverage;
    next.occupation_coverage = coverage;
    next.resistance = resistance;

    Ok((
        next,
        OccupationAssessment {
            victim_id: state.victim_id,
            annexer_id: state.annexer_id,
            held_ratio,
            required_garrison: required,
            garrison_coverage,
            occupation_due: due,
            occupation_yield: yield_amount,
            resistance_delta: delta,
            resistance,
            rebellion_ready: resistance >= 100.0 && !state.active_rebellion,
        },
    ))
}

pub fn select_occupation_controller(
    candidates: &[OccupationControllerCandidate],
) -> Option<OccupationControllerCandidate> {
    let mut candidates = candidates
        .iter()
        .copied()
        .filter(|entry| entry.country_id > 0 && entry.controlled_cells > 0)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .controlled_cells
            .cmp(&left.controlled_cells)
            .then_with(|| right.casualties.total_cmp(&left.casualties))
            .then_with(|| left.country_id.cmp(&right.country_id))
    });
    candidates.first().copied()
}

pub fn select_rebellion_candidates(
    records: &[RebellionCandidate],
    active_by_annexer: &[(u16, usize)],
    active_total: usize,
    cycle: u64,
    max_active: usize,
    max_per_annexer: usize,
) -> Result<Vec<RebellionCandidate>, OccupationError> {
    if records.iter().any(|record| !record.resistance.is_finite()) {
        return Err(OccupationError::NonFinite);
    }
    let mut counts = std::collections::BTreeMap::from_iter(active_by_annexer.iter().copied());
    let available = max_active.saturating_sub(active_total);
    let mut eligible = records
        .iter()
        .copied()
        .filter(|record| {
            record.victim_id > 0
                && record.annexer_id > 0
                && record.resistance >= 100.0
                && !record.active
                && record.cooldown_until_cycle <= cycle
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        right
            .resistance
            .total_cmp(&left.resistance)
            .then_with(|| left.queued_at_cycle.cmp(&right.queued_at_cycle))
            .then_with(|| left.victim_id.cmp(&right.victim_id))
    });
    let mut selected = Vec::with_capacity(available);
    for record in eligible {
        if selected.len() >= available {
            break;
        }
        let count = counts.entry(record.annexer_id).or_default();
        if *count >= max_per_annexer {
            continue;
        }
        *count += 1;
        selected.push(record);
    }
    Ok(selected)
}

fn validate_state(state: &OccupationState) -> Result<(), OccupationError> {
    if state.victim_id == 0 || state.annexer_id == 0 || state.victim_id == state.annexer_id {
        return Err(OccupationError::InvalidCountries);
    }
    if ![
        state.base_income,
        state.expected_army_units,
        state.resistance,
        state.occupation_coverage,
        state.garrison_coverage,
        state.garrison_assigned,
        state.held_ratio,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err(OccupationError::NonFinite);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> OccupationState {
        OccupationState {
            victim_id: 2,
            annexer_id: 1,
            base_income: 100.0,
            core_cells: 100,
            expected_army_units: 21.0,
            resistance: 90.0,
            occupation_coverage: 1.0,
            garrison_coverage: 0.0,
            garrison_assigned: 0.0,
            required_garrison: 4,
            held_ratio: 1.0,
            active_rebellion: false,
            queued_at_cycle: 0,
            cooldown_until_cycle: 0,
        }
    }

    #[test]
    fn browser_garrison_and_resistance_math() {
        assert_eq!(required_garrison(0.0), Ok(3));
        assert_eq!(required_garrison(20.0), Ok(3));
        assert_eq!(required_garrison(21.0), Ok(4));
        assert_eq!(resistance_delta(1.0, 1.0, 0.0), Ok(-4.0));
        assert_eq!(resistance_delta(0.0, 0.0, 1.0), Ok(22.0));
    }

    #[test]
    fn assessment_updates_all_consequences() {
        let (next, assessment) = assess_occupation(
            &state(),
            OccupationCycleInput {
                held_cells: 50,
                garrison_strength: 2.0,
                annexer_occupation_coverage: 0.0,
                casualty_pressure: 1.0,
            },
        )
        .unwrap();
        assert_eq!(assessment.held_ratio, 0.5);
        assert_eq!(assessment.occupation_due, 7.5);
        assert_eq!(assessment.occupation_yield, 12.5);
        assert_eq!(next.garrison_coverage, 0.5);
        assert_eq!(next.resistance, 100.0);
        assert!(assessment.rebellion_ready);
    }

    #[test]
    fn controller_tie_breaks_by_country_id() {
        let selected = select_occupation_controller(&[
            OccupationControllerCandidate {
                country_id: 2,
                controlled_cells: 10,
                casualties: 50.0,
            },
            OccupationControllerCandidate {
                country_id: 1,
                controlled_cells: 10,
                casualties: 50.0,
            },
        ])
        .unwrap();
        assert_eq!(selected.country_id, 1);
    }
}
