//! Deterministic war-economy primitives ported from the browser simulation.
//!
//! The module deliberately has no wall-clock or renderer dependencies. A caller
//! supplies one coherent territory census and all due amounts at a pay-cycle
//! boundary, then commits the returned state atomically.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const ECONOMY_SCHEMA_VERSION: &str = "economy-v1";
pub const PAY_CYCLE_TICKS: u64 = 600;
pub const PAYROLL_PER_UNIT: f64 = 1.0;
pub const RECRUITMENT_COST: f64 = 3.0;
pub const STARTING_RESERVE_CYCLES: f64 = 6.0;
pub const TARGET_STARTING_PAYROLL_SHARE: f64 = 0.7;
pub const CAPITAL_LOSS_INCOME_MULT: f64 = 0.65;
pub const OCCUPATION_YIELD_SHARE: f64 = 0.25;
pub const OCCUPATION_COST_SHARE: f64 = 0.15;
pub const MUTINY_RECOVERY_CYCLES: u32 = 3;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CommandBand {
    #[default]
    Paid,
    Strained,
    Unpaid,
    Breakdown,
    Mutiny,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EconomySeed {
    pub country_id: u16,
    #[serde(default)]
    pub gdp: f64,
    #[serde(default)]
    pub population: f64,
    #[serde(default)]
    pub territory_units: f64,
    #[serde(default)]
    pub initial_core_cells: u32,
    #[serde(default)]
    pub initial_city_population: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EconomyState {
    pub country_id: u16,
    pub economic_strength: f64,
    pub base_income: f64,
    pub treasury: f64,
    pub income: f64,
    pub occupation_yield: f64,
    pub payroll_due: f64,
    pub occupation_due: f64,
    pub payroll_coverage: f64,
    pub occupation_coverage: f64,
    pub arrears_cycles: f64,
    pub command_band: CommandBand,
    pub mutiny_recovery_cycles: u32,
    pub initial_core_cells: u32,
    pub initial_city_population: f64,
    pub core_control_ratio: f64,
    pub city_control_ratio: f64,
    pub capital_held: bool,
    pub last_event_band: CommandBand,
    pub capitulated: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EconomyCycleInput {
    #[serde(default)]
    pub income: f64,
    #[serde(default)]
    pub occupation_yield: f64,
    #[serde(default)]
    pub payroll_due: f64,
    #[serde(default)]
    pub occupation_due: f64,
    #[serde(default = "one")]
    pub core_control_ratio: f64,
    #[serde(default = "one")]
    pub city_control_ratio: f64,
    #[serde(default = "yes")]
    pub capital_held: bool,
}

const fn one() -> f64 {
    1.0
}

const fn yes() -> bool {
    true
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum EconomyError {
    #[error("economy input contains a non-finite number")]
    NonFinite,
    #[error("country id must be positive")]
    InvalidCountry,
}

pub fn compute_economic_strength(gdp: f64, population: f64, territory_units: f64) -> f64 {
    if gdp.is_finite() && gdp > 0.0 {
        return gdp.sqrt() * 2.5;
    }
    if population.is_finite() && population > 0.0 {
        return population.sqrt() * 0.15;
    }
    if territory_units.is_finite() {
        territory_units.max(0.0)
    } else {
        0.0
    }
}

pub fn create_economy_state(seed: EconomySeed) -> Result<EconomyState, EconomyError> {
    if seed.country_id == 0 {
        return Err(EconomyError::InvalidCountry);
    }
    if ![
        seed.gdp,
        seed.population,
        seed.territory_units,
        seed.initial_city_population,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err(EconomyError::NonFinite);
    }
    let economic_strength =
        compute_economic_strength(seed.gdp, seed.population, seed.territory_units);
    let base_income = (economic_strength / TARGET_STARTING_PAYROLL_SHARE).max(3.0);
    Ok(EconomyState {
        country_id: seed.country_id,
        economic_strength,
        base_income,
        treasury: base_income * STARTING_RESERVE_CYCLES,
        income: base_income,
        occupation_yield: 0.0,
        payroll_due: 0.0,
        occupation_due: 0.0,
        payroll_coverage: 1.0,
        occupation_coverage: 1.0,
        arrears_cycles: 0.0,
        command_band: CommandBand::Paid,
        mutiny_recovery_cycles: 0,
        initial_core_cells: seed.initial_core_cells.max(1),
        initial_city_population: seed.initial_city_population.max(0.0),
        core_control_ratio: 1.0,
        city_control_ratio: 1.0,
        capital_held: true,
        last_event_band: CommandBand::Paid,
        capitulated: false,
    })
}

pub fn compute_current_income(
    base_income: f64,
    core_control_ratio: f64,
    city_control_ratio: f64,
    capital_held: bool,
) -> Result<f64, EconomyError> {
    if ![base_income, core_control_ratio, city_control_ratio]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(EconomyError::NonFinite);
    }
    let productive_control = (0.6 * core_control_ratio.clamp(0.0, 1.0)
        + 0.4 * city_control_ratio.clamp(0.0, 1.0))
    .max(0.05);
    Ok(base_income.max(0.0)
        * productive_control
        * if capital_held {
            1.0
        } else {
            CAPITAL_LOSS_INCOME_MULT
        })
}

pub fn command_band(arrears_cycles: f64, mutiny_recovery_cycles: u32) -> CommandBand {
    let arrears = if arrears_cycles.is_finite() {
        arrears_cycles.max(0.0)
    } else {
        0.0
    };
    if arrears >= 5.0 {
        CommandBand::Mutiny
    } else if arrears >= 3.0 {
        CommandBand::Breakdown
    } else if arrears >= 2.0 {
        CommandBand::Unpaid
    } else if arrears >= 1.0 || mutiny_recovery_cycles > 0 {
        CommandBand::Strained
    } else {
        CommandBand::Paid
    }
}

pub const fn command_refusal_share(band: CommandBand) -> f64 {
    match band {
        CommandBand::Strained => 0.25,
        CommandBand::Unpaid => 0.6,
        CommandBand::Breakdown | CommandBand::Mutiny => 1.0,
        CommandBand::Paid => 0.0,
    }
}

pub const fn desertion_rate(band: CommandBand) -> f64 {
    match band {
        CommandBand::Mutiny => 0.03,
        CommandBand::Breakdown => 0.01,
        _ => 0.0,
    }
}

pub fn settle_economy_cycle(
    state: &EconomyState,
    input: EconomyCycleInput,
) -> Result<EconomyState, EconomyError> {
    validate_state(state)?;
    if ![
        input.income,
        input.occupation_yield,
        input.payroll_due,
        input.occupation_due,
        input.core_control_ratio,
        input.city_control_ratio,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err(EconomyError::NonFinite);
    }

    let mut next = state.clone();
    let previous_band = command_band(next.arrears_cycles, next.mutiny_recovery_cycles);
    next.income = input.income.max(0.0);
    next.occupation_yield = input.occupation_yield.max(0.0);
    next.payroll_due = input.payroll_due.max(0.0);
    next.occupation_due = input.occupation_due.max(0.0);
    next.core_control_ratio = input.core_control_ratio.clamp(0.0, 1.0);
    next.city_control_ratio = input.city_control_ratio.clamp(0.0, 1.0);
    next.capital_held = input.capital_held;
    next.treasury = (next.treasury + next.income + next.occupation_yield).max(0.0);

    // Browser compatibility: payroll and occupation share one proportional
    // coverage ratio; payroll is not paid first.
    let total_due = next.payroll_due + next.occupation_due;
    let coverage = if total_due > 0.0 {
        (next.treasury / total_due).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let spend = next.treasury.min(total_due);
    next.treasury -= spend;
    next.payroll_coverage = if next.payroll_due > 0.0 {
        coverage
    } else {
        1.0
    };
    next.occupation_coverage = if next.occupation_due > 0.0 {
        coverage
    } else {
        1.0
    };

    if next.payroll_coverage >= 0.999 {
        next.arrears_cycles = (next.arrears_cycles - 1.0).max(0.0);
        if next.arrears_cycles == 0.0 && next.mutiny_recovery_cycles > 0 {
            next.mutiny_recovery_cycles -= 1;
        }
    } else {
        if next.mutiny_recovery_cycles > 0 {
            next.mutiny_recovery_cycles = MUTINY_RECOVERY_CYCLES;
        }
        next.arrears_cycles = (next.arrears_cycles + (1.0 - next.payroll_coverage)).max(0.0);
    }

    let raw_band = command_band(next.arrears_cycles, 0);
    if previous_band != CommandBand::Mutiny && raw_band == CommandBand::Mutiny {
        next.mutiny_recovery_cycles = MUTINY_RECOVERY_CYCLES;
    }
    next.command_band = command_band(next.arrears_cycles, next.mutiny_recovery_cycles);
    Ok(next)
}

fn validate_state(state: &EconomyState) -> Result<(), EconomyError> {
    if state.country_id == 0 {
        return Err(EconomyError::InvalidCountry);
    }
    if ![
        state.economic_strength,
        state.base_income,
        state.treasury,
        state.income,
        state.occupation_yield,
        state.payroll_due,
        state.occupation_due,
        state.payroll_coverage,
        state.occupation_coverage,
        state.arrears_cycles,
        state.initial_city_population,
        state.core_control_ratio,
        state.city_control_ratio,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err(EconomyError::NonFinite);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> EconomySeed {
        EconomySeed {
            country_id: 1,
            gdp: 100.0,
            population: 0.0,
            territory_units: 0.0,
            initial_core_cells: 100,
            initial_city_population: 0.0,
        }
    }

    #[test]
    fn economic_strength_and_reserve_match_browser() {
        assert_eq!(compute_economic_strength(100.0, 999_999.0, 0.0), 25.0);
        assert_eq!(compute_economic_strength(0.0, 10_000.0, 0.0), 15.0);
        assert_eq!(compute_economic_strength(0.0, 0.0, 7.0), 7.0);
        let state = create_economy_state(seed()).unwrap();
        assert_eq!(state.base_income, 25.0 / 0.7);
        assert_eq!(state.treasury, state.base_income * 6.0);
    }

    #[test]
    fn productive_floor_and_capital_loss_match_browser() {
        assert_eq!(compute_current_income(100.0, 1.0, 1.0, true), Ok(100.0));
        assert_eq!(compute_current_income(100.0, 0.0, 0.0, false), Ok(3.25));
    }

    #[test]
    fn five_empty_cycles_enter_mutiny_then_recover() {
        let mut state = create_economy_state(seed()).unwrap();
        state.treasury = 0.0;
        for _ in 0..5 {
            state = settle_economy_cycle(
                &state,
                EconomyCycleInput {
                    payroll_due: 10.0,
                    core_control_ratio: 1.0,
                    city_control_ratio: 1.0,
                    capital_held: true,
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert_eq!(state.command_band, CommandBand::Mutiny);
        assert_eq!(state.mutiny_recovery_cycles, 3);
        for _ in 0..8 {
            state.treasury = 10.0;
            state = settle_economy_cycle(
                &state,
                EconomyCycleInput {
                    payroll_due: 10.0,
                    core_control_ratio: 1.0,
                    city_control_ratio: 1.0,
                    capital_held: true,
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert_eq!(state.command_band, CommandBand::Paid);
        assert_eq!(state.arrears_cycles, 0.0);
        assert_eq!(state.mutiny_recovery_cycles, 0);
    }

    #[test]
    fn coverage_is_shared_between_due_categories() {
        let mut state = create_economy_state(seed()).unwrap();
        state.treasury = 5.0;
        let next = settle_economy_cycle(
            &state,
            EconomyCycleInput {
                payroll_due: 5.0,
                occupation_due: 5.0,
                core_control_ratio: 1.0,
                city_control_ratio: 1.0,
                capital_held: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(next.payroll_coverage, 0.5);
        assert_eq!(next.occupation_coverage, 0.5);
    }
}
