//! Deterministic capitulation and treaty decisions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SURRENDER_SCHEMA_VERSION: &str = "surrender-v1";
pub const DEFENDED_CONTROL_PERCENT: f64 = 2.0;
pub const UNITLESS_CONTROL_PERCENT: f64 = 25.0;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CapitulationReason {
    StaleTerritoryData,
    RebellionRules,
    NoOwnedCells,
    DefendedControlCollapse,
    UnitlessControlCollapse,
    AboveThreshold,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CapitulationInput {
    pub has_fresh_territory_data: bool,
    pub is_rebel: bool,
    pub unit_count: u32,
    pub owned_cells: f64,
    pub controlled_cells: f64,
    pub initial_cells: f64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CapitulationDecision {
    pub capitulate: bool,
    pub reason: CapitulationReason,
    pub control_percent: f64,
    pub threshold: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CasualtyEntry {
    pub country_id: u16,
    pub casualties: f64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CasualtyShare {
    pub country_id: u16,
    pub casualties: f64,
    pub share: f64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WeightedQuota {
    pub country_id: u16,
    pub weight: f64,
    pub quota: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConflictResolutionKind {
    WhitePeace,
    FullCapitulation,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConflictResolution {
    pub kind: ConflictResolutionKind,
    pub winner_side: Option<u16>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OwnerTransfer {
    pub original_owner: u16,
    pub new_owner: u16,
    pub count: u32,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum SurrenderError {
    #[error("surrender input contains a non-finite number")]
    NonFinite,
    #[error("minimum casualty share must be between zero and one")]
    InvalidShare,
    #[error("quota total exceeds u32")]
    QuotaOverflow,
}

pub fn evaluate_capitulation(
    input: CapitulationInput,
) -> Result<CapitulationDecision, SurrenderError> {
    if ![
        input.owned_cells,
        input.controlled_cells,
        input.initial_cells,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err(SurrenderError::NonFinite);
    }
    let safe_initial = if input.initial_cells == 0.0 {
        1.0
    } else {
        input.initial_cells
    }
    .max(1.0);
    let control_percent = input.controlled_cells.max(0.0) / safe_initial * 100.0;
    if !input.has_fresh_territory_data {
        return Ok(CapitulationDecision {
            capitulate: false,
            reason: CapitulationReason::StaleTerritoryData,
            control_percent,
            threshold: None,
        });
    }
    if input.is_rebel {
        return Ok(CapitulationDecision {
            capitulate: false,
            reason: CapitulationReason::RebellionRules,
            control_percent,
            threshold: None,
        });
    }
    if input.owned_cells <= 0.0 {
        return Ok(CapitulationDecision {
            capitulate: true,
            reason: CapitulationReason::NoOwnedCells,
            control_percent,
            threshold: None,
        });
    }
    let threshold = if input.unit_count > 0 {
        DEFENDED_CONTROL_PERCENT
    } else {
        UNITLESS_CONTROL_PERCENT
    };
    let capitulate = control_percent < threshold;
    Ok(CapitulationDecision {
        capitulate,
        reason: if capitulate {
            if input.unit_count > 0 {
                CapitulationReason::DefendedControlCollapse
            } else {
                CapitulationReason::UnitlessControlCollapse
            }
        } else {
            CapitulationReason::AboveThreshold
        },
        control_percent,
        threshold: Some(threshold),
    })
}

pub fn eligible_casualty_attackers(
    entries: &[CasualtyEntry],
    minimum_share: f64,
) -> Result<Vec<CasualtyShare>, SurrenderError> {
    if !minimum_share.is_finite() || !(0.0..=1.0).contains(&minimum_share) {
        return Err(SurrenderError::InvalidShare);
    }
    if entries.iter().any(|entry| !entry.casualties.is_finite()) {
        return Err(SurrenderError::NonFinite);
    }
    let total = entries
        .iter()
        .filter(|entry| entry.country_id > 0)
        .map(|entry| entry.casualties.max(0.0))
        .sum::<f64>();
    if total <= 0.0 {
        return Ok(Vec::new());
    }
    let mut ranked = entries
        .iter()
        .filter(|entry| entry.country_id > 0)
        .map(|entry| CasualtyShare {
            country_id: entry.country_id,
            casualties: entry.casualties.max(0.0),
            share: entry.casualties.max(0.0) / total,
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .casualties
            .total_cmp(&left.casualties)
            .then_with(|| left.country_id.cmp(&right.country_id))
    });
    let selected = ranked
        .iter()
        .copied()
        .filter(|entry| entry.share >= minimum_share)
        .collect::<Vec<_>>();
    Ok(if selected.is_empty() {
        ranked.into_iter().take(1).collect()
    } else {
        selected
    })
}

pub fn largest_remainder_quotas(
    entries: &[CasualtyShare],
    total_cells: u64,
) -> Result<Vec<WeightedQuota>, SurrenderError> {
    let total = u32::try_from(total_cells).map_err(|_| SurrenderError::QuotaOverflow)?;
    let mut weighted = entries
        .iter()
        .filter(|entry| entry.country_id > 0)
        .map(|entry| (entry.country_id, entry.casualties.max(0.0)))
        .collect::<Vec<_>>();
    if weighted.is_empty() {
        return Ok(Vec::new());
    }
    if weighted.iter().any(|(_, weight)| !weight.is_finite()) {
        return Err(SurrenderError::NonFinite);
    }
    let mut weight_total = weighted.iter().map(|(_, weight)| *weight).sum::<f64>();
    if weight_total <= 0.0 {
        for (_, weight) in &mut weighted {
            *weight = 1.0;
        }
        weight_total = weighted.len() as f64;
    }
    let mut quotas = weighted
        .into_iter()
        .map(|(country_id, weight)| {
            let exact = weight / weight_total * f64::from(total);
            (
                WeightedQuota {
                    country_id,
                    weight,
                    quota: exact.floor() as u32,
                },
                exact % 1.0,
            )
        })
        .collect::<Vec<_>>();
    let assigned = quotas.iter().map(|(quota, _)| quota.quota).sum::<u32>();
    let remaining = total - assigned;
    let mut order = (0..quotas.len()).collect::<Vec<_>>();
    order.sort_by(|&left, &right| {
        quotas[right]
            .1
            .total_cmp(&quotas[left].1)
            .then_with(|| quotas[right].0.weight.total_cmp(&quotas[left].0.weight))
            .then_with(|| quotas[left].0.country_id.cmp(&quotas[right].0.country_id))
    });
    for index in 0..remaining as usize {
        quotas[order[index % order.len()]].0.quota += 1;
    }
    Ok(quotas.into_iter().map(|(quota, _)| quota).collect())
}

pub fn evaluate_global_conflict(
    active_side_indices: &[u16],
    active_hostile_pairs: &[(u16, u16)],
) -> Option<ConflictResolution> {
    let active = active_side_indices.iter().copied().collect::<BTreeSet<_>>();
    match active.len() {
        0 => Some(ConflictResolution {
            kind: ConflictResolutionKind::WhitePeace,
            winner_side: None,
        }),
        1 => Some(ConflictResolution {
            kind: ConflictResolutionKind::FullCapitulation,
            winner_side: active.first().copied(),
        }),
        _ if active_hostile_pairs.is_empty() => Some(ConflictResolution {
            kind: ConflictResolutionKind::WhitePeace,
            winner_side: None,
        }),
        _ => None,
    }
}

pub fn update_rebellion_failure_cycles(
    failed_cycles: u32,
    unit_count: u32,
    control_ratio: f64,
) -> Result<u32, SurrenderError> {
    if !control_ratio.is_finite() {
        return Err(SurrenderError::NonFinite);
    }
    Ok(if unit_count == 0 && control_ratio < 0.05 {
        failed_cycles.saturating_add(1)
    } else {
        0
    })
}

pub fn majority_owner_transfers(transfers: &[OwnerTransfer]) -> BTreeMap<u16, u16> {
    let mut counts = BTreeMap::<u16, BTreeMap<u16, u64>>::new();
    for transfer in transfers {
        if transfer.original_owner == 0
            || transfer.new_owner == 0
            || transfer.original_owner == transfer.new_owner
        {
            continue;
        }
        *counts
            .entry(transfer.original_owner)
            .or_default()
            .entry(transfer.new_owner)
            .or_default() += u64::from(transfer.count.max(1));
    }
    counts
        .into_iter()
        .filter_map(|(original, recipients)| {
            recipients
                .into_iter()
                .max_by(|(left_id, left_count), (right_id, right_count)| {
                    left_count
                        .cmp(right_count)
                        .then_with(|| right_id.cmp(left_id))
                })
                .map(|(recipient, _)| (original, recipient))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(controlled: f64, units: u32) -> CapitulationDecision {
        evaluate_capitulation(CapitulationInput {
            has_fresh_territory_data: true,
            is_rebel: false,
            unit_count: units,
            owned_cells: 100.0,
            controlled_cells: controlled,
            initial_cells: 100.0,
        })
        .unwrap()
    }

    #[test]
    fn capitulation_boundaries_are_strict() {
        assert!(!decision(2.0, 1).capitulate);
        assert!(decision(1.99, 1).capitulate);
        assert!(!decision(25.0, 0).capitulate);
        assert!(decision(24.99, 0).capitulate);
    }

    #[test]
    fn stale_and_rebel_checks_precede_owned_cells() {
        let mut input = CapitulationInput {
            has_fresh_territory_data: false,
            is_rebel: false,
            unit_count: 0,
            owned_cells: 0.0,
            controlled_cells: 0.0,
            initial_cells: 1.0,
        };
        assert_eq!(
            evaluate_capitulation(input).unwrap().reason,
            CapitulationReason::StaleTerritoryData
        );
        input.has_fresh_territory_data = true;
        input.is_rebel = true;
        assert_eq!(
            evaluate_capitulation(input).unwrap().reason,
            CapitulationReason::RebellionRules
        );
    }

    #[test]
    fn casualty_selection_and_largest_remainder_match_browser() {
        let eligible = eligible_casualty_attackers(
            &[
                CasualtyEntry {
                    country_id: 1,
                    casualties: 40.0,
                },
                CasualtyEntry {
                    country_id: 2,
                    casualties: 30.0,
                },
                CasualtyEntry {
                    country_id: 3,
                    casualties: 20.0,
                },
                CasualtyEntry {
                    country_id: 4,
                    casualties: 10.0,
                },
            ],
            0.25,
        )
        .unwrap();
        assert_eq!(
            eligible
                .iter()
                .map(|entry| entry.country_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            largest_remainder_quotas(&eligible, 17)
                .unwrap()
                .iter()
                .map(|entry| entry.quota)
                .collect::<Vec<_>>(),
            vec![10, 7]
        );
    }

    #[test]
    fn global_conflict_and_transfer_ties_are_deterministic() {
        assert_eq!(
            evaluate_global_conflict(&[0, 2], &[]),
            Some(ConflictResolution {
                kind: ConflictResolutionKind::WhitePeace,
                winner_side: None
            })
        );
        let transfers = majority_owner_transfers(&[
            OwnerTransfer {
                original_owner: 11,
                new_owner: 4,
                count: 3,
            },
            OwnerTransfer {
                original_owner: 11,
                new_owner: 3,
                count: 3,
            },
        ]);
        assert_eq!(transfers.get(&11), Some(&3));
    }
}
