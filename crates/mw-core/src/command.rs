//! Deterministic per-unit command-band policy resolution.
//!
//! The browser caches a seeded discipline value on each formation and chooses
//! a country home target whenever its command band enters breakdown or mutiny.
//! Native checkpoints persist the resolved discipline and target. New native
//! transitions use the same discipline thresholds and a stable first-cell
//! fallback instead of the browser's gameplay RNG reservoir.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::economy::{CommandBand, command_refusal_share};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandUnitState {
    pub id: u64,
    pub sovereign_id: u16,
    pub side: usize,
    pub discipline: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandHomeTarget {
    pub cell: usize,
    pub lat: f64,
    pub lng: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedCommandPolicy {
    pub band: CommandBand,
    pub discipline: f64,
    pub refuses_offense: bool,
    pub return_home: bool,
    pub self_defense_only: bool,
    pub home_target: Option<CommandHomeTarget>,
}

#[derive(Clone, Copy, Debug)]
pub struct CommandWorld<'a> {
    pub grid_resolution: f64,
    pub width: usize,
    pub height: usize,
    pub land: &'a [u8],
    pub world_control: &'a [u16],
    pub dominant_side: &'a [i16],
    pub country_side: &'a BTreeMap<u16, usize>,
    /// Exact city coordinates are retained when the controlled capital is used.
    pub capital_targets: &'a BTreeMap<u16, CommandHomeTarget>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CommandResolveError {
    #[error("command world dimensions or map lengths are invalid")]
    InvalidWorld,
    #[error("command unit {0} is invalid or duplicated")]
    InvalidUnit(u64),
    #[error("command unit {unit_id} disagrees with country {country_id} topology")]
    InvalidTopology { unit_id: u64, country_id: u16 },
}

/// Exact browser discipline seed for a stable source unit id.
pub fn browser_discipline(unit_seed: f64, sovereign_id: u16) -> f64 {
    let seed = (unit_seed * 99_991.0 + f64::from(sovereign_id) * 7_919.0).sin() * 43_758.545_3;
    (seed - seed.floor()).abs()
}

pub const fn refusal_share(band: CommandBand) -> f64 {
    command_refusal_share(band)
}

pub fn resolve_command_policy(
    unit: CommandUnitState,
    band: CommandBand,
    world: CommandWorld<'_>,
) -> Result<ResolvedCommandPolicy, CommandResolveError> {
    validate_world(world)?;
    if unit.id == 0
        || unit.sovereign_id == 0
        || !unit.discipline.is_finite()
        || !(0.0..1.0).contains(&unit.discipline)
    {
        return Err(CommandResolveError::InvalidUnit(unit.id));
    }
    if world.country_side.get(&unit.sovereign_id).copied() != Some(unit.side) {
        return Err(CommandResolveError::InvalidTopology {
            unit_id: unit.id,
            country_id: unit.sovereign_id,
        });
    }
    let return_home = matches!(band, CommandBand::Breakdown | CommandBand::Mutiny);
    Ok(ResolvedCommandPolicy {
        band,
        discipline: unit.discipline,
        refuses_offense: unit.discipline < refusal_share(band),
        return_home,
        self_defense_only: band == CommandBand::Mutiny,
        home_target: return_home
            .then(|| home_target(unit.sovereign_id, unit.side, world))
            .flatten(),
    })
}

/// Resolve in ascending unit-id order so a complete validated map can be
/// installed atomically by the runtime.
pub fn resolve_command_batch(
    units: &[CommandUnitState],
    country_bands: &BTreeMap<u16, CommandBand>,
    world: CommandWorld<'_>,
) -> Result<BTreeMap<u64, ResolvedCommandPolicy>, CommandResolveError> {
    validate_world(world)?;
    let mut ordered = units.to_vec();
    ordered.sort_by_key(|unit| unit.id);
    let mut output = BTreeMap::new();
    for unit in ordered {
        let band = country_bands
            .get(&unit.sovereign_id)
            .copied()
            .unwrap_or(CommandBand::Paid);
        let policy = resolve_command_policy(unit, band, world)?;
        if output.insert(unit.id, policy).is_some() {
            return Err(CommandResolveError::InvalidUnit(unit.id));
        }
    }
    Ok(output)
}

fn home_target(country: u16, side: usize, world: CommandWorld<'_>) -> Option<CommandHomeTarget> {
    if let Some(&target) = world.capital_targets.get(&country)
        && target.cell < world.land.len()
        && world.dominant_side[target.cell] == side as i16
    {
        return Some(target);
    }
    world.land.iter().enumerate().find_map(|(cell, _)| {
        eligible(cell, country, side, world).then(|| {
            let row = cell / world.width;
            let column = cell % world.width;
            // Browser fallback uses the cell's south-west corner, not center.
            CommandHomeTarget {
                cell,
                lat: row as f64 * world.grid_resolution - 90.0,
                lng: column as f64 * world.grid_resolution - 180.0,
            }
        })
    })
}

fn eligible(cell: usize, country: u16, side: usize, world: CommandWorld<'_>) -> bool {
    cell < world.land.len()
        && world.land[cell] > 0
        && world.world_control[cell] == country
        && world.dominant_side[cell] == side as i16
}

fn validate_world(world: CommandWorld<'_>) -> Result<(), CommandResolveError> {
    let Some(cells) = world.width.checked_mul(world.height) else {
        return Err(CommandResolveError::InvalidWorld);
    };
    if world.width == 0
        || world.height == 0
        || !world.grid_resolution.is_finite()
        || world.grid_resolution <= 0.0
        || world.land.len() != cells
        || world.world_control.len() != cells
        || world.dominant_side.len() != cells
        || world
            .country_side
            .iter()
            .any(|(&country, &side)| country == 0 || side > i16::MAX as usize)
        || world.capital_targets.iter().any(|(&country, target)| {
            !world.country_side.contains_key(&country)
                || target.cell >= cells
                || !target.lat.is_finite()
                || !target.lng.is_finite()
        })
    {
        return Err(CommandResolveError::InvalidWorld);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world<'a>(
        land: &'a [u8],
        control: &'a [u16],
        dominant: &'a [i16],
        countries: &'a BTreeMap<u16, usize>,
        capitals: &'a BTreeMap<u16, CommandHomeTarget>,
    ) -> CommandWorld<'a> {
        CommandWorld {
            grid_resolution: 1.0,
            width: 3,
            height: 1,
            land,
            world_control: control,
            dominant_side: dominant,
            country_side: countries,
            capital_targets: capitals,
        }
    }

    #[test]
    fn bands_have_browser_refusal_shares() {
        assert_eq!(refusal_share(CommandBand::Paid), 0.0);
        assert_eq!(refusal_share(CommandBand::Strained), 0.25);
        assert_eq!(refusal_share(CommandBand::Unpaid), 0.6);
        assert_eq!(refusal_share(CommandBand::Breakdown), 1.0);
        assert_eq!(refusal_share(CommandBand::Mutiny), 1.0);
    }

    #[test]
    fn mutiny_returns_to_controlled_capital_and_self_defends() {
        let land = [1, 1, 1];
        let control = [1, 2, 2];
        let dominant = [0, 0, 1];
        let countries = BTreeMap::from([(1, 0), (2, 1)]);
        let capitals = BTreeMap::from([(
            1,
            CommandHomeTarget {
                cell: 1,
                lat: 0.25,
                lng: 0.5,
            },
        )]);
        let policy = resolve_command_policy(
            CommandUnitState {
                id: 1,
                sovereign_id: 1,
                side: 0,
                discipline: 0.5,
            },
            CommandBand::Mutiny,
            world(&land, &control, &dominant, &countries, &capitals),
        )
        .unwrap();
        assert!(policy.refuses_offense);
        assert!(policy.return_home);
        assert!(policy.self_defense_only);
        assert_eq!(policy.home_target, capitals.get(&1).copied());
    }

    #[test]
    fn fallback_and_batch_order_are_deterministic() {
        let land = [1, 1, 1];
        let control = [1, 1, 2];
        let dominant = [0, 0, 1];
        let countries = BTreeMap::from([(1, 0), (2, 1)]);
        let capitals = BTreeMap::new();
        let policies = resolve_command_batch(
            &[
                CommandUnitState {
                    id: 2,
                    sovereign_id: 1,
                    side: 0,
                    discipline: 0.7,
                },
                CommandUnitState {
                    id: 1,
                    sovereign_id: 1,
                    side: 0,
                    discipline: 0.2,
                },
            ],
            &BTreeMap::from([(1, CommandBand::Breakdown)]),
            world(&land, &control, &dominant, &countries, &capitals),
        )
        .unwrap();
        assert_eq!(policies.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(policies[&1].home_target.unwrap().cell, 0);
    }
}
