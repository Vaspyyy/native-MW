//! Pure read-only projection of immutable runtime publications into observer HUD text.

use std::collections::BTreeSet;

use mw_core::{
    AirRole, CommandBand, NavalOperationPhase, RuntimeSnapshot, RuntimeState, TaskForcePhase,
    UnitKind,
};

const MAX_LINE_CHARS: usize = 35;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserverHudModel {
    pub lines: Vec<String>,
}

impl ObserverHudModel {
    pub fn without_runtime(selected: Option<(u16, &str)>) -> Self {
        let mut lines = vec![
            "MODERN WARS // OBSERVER".to_owned(),
            "NO ACTIVE SIMULATION".to_owned(),
            String::new(),
        ];
        if let Some((country_id, country_name)) = selected {
            lines.push(clip_line(&format!(
                "{}  #{}",
                ascii_upper(country_name),
                country_id
            )));
            lines.push("START OR LOAD A WAR FOR LIVE DATA".to_owned());
            lines.push(String::new());
        }
        lines.extend([
            "LEFT CLICK COUNTRY".to_owned(),
            "H HIDE  R RESET  ESC QUIT".to_owned(),
        ]);
        Self { lines }
    }

    pub fn from_runtime(
        snapshot: &RuntimeSnapshot,
        selected_country_id: Option<u16>,
        country_name: &str,
        save_enabled: bool,
    ) -> Self {
        let mut lines = vec![
            clip_line(&format!("MODERN WARS // TICK {}", snapshot.tick)),
            clip_line(&format!(
                "{}  {} UNITS  {} COMBATS",
                runtime_state_label(snapshot.state),
                snapshot.frame_snapshot.units.len(),
                snapshot.frame_snapshot.events.len()
            )),
            String::new(),
        ];
        let Some(country_id) = selected_country_id else {
            lines.extend([
                "SELECT A COUNTRY TO INSPECT".to_owned(),
                String::new(),
                "LEFT CLICK COUNTRY".to_owned(),
                control_line(save_enabled),
            ]);
            return Self { lines };
        };

        let units = snapshot
            .frame_snapshot
            .units
            .iter()
            .filter(|unit| unit.sovereign == u64::from(country_id))
            .collect::<Vec<_>>();
        let unit_ids = units.iter().map(|unit| unit.id).collect::<BTreeSet<_>>();
        let territory = snapshot
            .territory_snapshot
            .countries
            .iter()
            .find(|country| country.country_id == country_id);
        let economy = snapshot
            .economy_snapshot
            .iter()
            .find(|economy| economy.country_id == country_id);
        let reinforcement = snapshot.reinforcement_snapshot.as_ref().and_then(|state| {
            state
                .countries
                .iter()
                .find(|country| country.country_id == country_id)
        });
        let material = snapshot
            .material_logistics_snapshot
            .as_ref()
            .and_then(|state| {
                state
                    .countries
                    .iter()
                    .find(|country| country.country_id == country_id)
            });
        let side = territory
            .and_then(|country| usize::try_from(country.side_index).ok())
            .or_else(|| units.first().map(|unit| unit.side as usize));

        lines.push(clip_line(&format!(
            "{}  #{}",
            ascii_upper(country_name),
            country_id
        )));
        lines.push(clip_line(&format!(
            "SIDE {}  {}",
            side.map_or_else(|| "-".to_owned(), |side| (side + 1).to_string()),
            economy.map_or("NO ECONOMY", |economy| {
                if economy.capitulated {
                    "CAPITULATED"
                } else {
                    command_band_label(economy.command_band)
                }
            })
        )));

        lines.push(String::new());
        lines.push("TERRITORY".to_owned());
        if let Some(territory) = territory {
            lines.push(clip_line(&format!(
                "CONTROL {} / OWN {}",
                compact_u64(territory.controlled),
                compact_u64(territory.owned)
            )));
            lines.push(clip_line(&format!(
                "CITIES {}/{}  FRONT {}",
                territory.cities_controlled,
                territory.cities_total,
                compact_u64(territory.frontline)
            )));
            lines.push(if territory.capital_held {
                "CAPITAL HELD".to_owned()
            } else {
                "CAPITAL LOST".to_owned()
            });
        } else {
            lines.push("NO LIVE TERRITORY RECORD".to_owned());
        }

        lines.push(String::new());
        lines.push("ECONOMY".to_owned());
        if let Some(economy) = economy {
            lines.push(clip_line(&format!(
                "TREASURY {}  INCOME {}",
                compact_f64(economy.treasury),
                compact_f64(economy.income)
            )));
            lines.push(clip_line(&format!(
                "PAYROLL {}  OCC {}",
                percent(economy.payroll_coverage),
                percent(economy.occupation_coverage)
            )));
            lines.push(clip_line(&format!(
                "CORE {}  CITY {}",
                percent(economy.core_control_ratio),
                percent(economy.city_control_ratio)
            )));
        } else {
            lines.push("NO LIVE ECONOMY RECORD".to_owned());
        }

        let armies = units
            .iter()
            .filter(|unit| unit.kind == UnitKind::Army)
            .count();
        let armor = units
            .iter()
            .filter(|unit| unit.kind == UnitKind::Armor)
            .count();
        let personnel = units
            .iter()
            .try_fold(0_u64, |sum, unit| sum.checked_add(unit.personnel))
            .unwrap_or(u64::MAX);
        let armor_equipment = units
            .iter()
            .filter(|unit| unit.kind == UnitKind::Armor)
            .fold((0_u64, 0_u64), |(live, capacity), unit| {
                (
                    live.saturating_add(unit.equipment),
                    capacity.saturating_add(unit.max_equipment),
                )
            });
        let casualties = snapshot
            .casualty_totals
            .get(&country_id)
            .copied()
            .unwrap_or(0.0);

        lines.push(String::new());
        lines.push("FORCES".to_owned());
        lines.push(clip_line(&format!("ARMY {}  ARMOR {}", armies, armor)));
        lines.push(clip_line(&format!(
            "PERSONNEL {}  LOSSES {}",
            compact_u64(personnel),
            compact_f64(casualties)
        )));
        lines.push(clip_line(&format!(
            "ARMOR {}/{}  RES {}",
            compact_u64(armor_equipment.0),
            compact_u64(armor_equipment.1),
            compact_u64(material.map_or(0, |record| record.reserve_armor))
        )));
        if let Some(side) = side {
            lines.push(clip_line(&format!(
                "SIDE MANPOWER {}",
                compact_f64(
                    snapshot
                        .personnel_reserves
                        .get(&side)
                        .copied()
                        .unwrap_or(0.0)
                )
            )));
        }

        let (fighters, fighter_capacity, strikes, strike_capacity) = snapshot
            .air_power_snapshot
            .as_ref()
            .map_or((0_u64, 0_u64, 0_u64, 0_u64), |air| {
                air.wings
                    .iter()
                    .filter(|wing| wing.sovereign_country_id == country_id)
                    .fold((0, 0, 0, 0), |totals, wing| match wing.role {
                        AirRole::Fighter => (
                            totals.0 + u64::from(wing.count),
                            totals.1 + u64::from(wing.max_count),
                            totals.2,
                            totals.3,
                        ),
                        AirRole::Strike => (
                            totals.0,
                            totals.1,
                            totals.2 + u64::from(wing.count),
                            totals.3 + u64::from(wing.max_count),
                        ),
                    })
            });
        let (ready_airfields, airfields) =
            snapshot
                .air_power_snapshot
                .as_ref()
                .map_or((0_usize, 0_usize), |air| {
                    let fields = air
                        .airfields
                        .iter()
                        .filter(|field| field.controller_country_id == country_id)
                        .collect::<Vec<_>>();
                    (
                        fields
                            .iter()
                            .filter(|field| !field.disabled && field.health > 0.0)
                            .count(),
                        fields.len(),
                    )
                });

        lines.push(String::new());
        lines.push("AIR POWER".to_owned());
        lines.push(clip_line(&format!(
            "FTR {}/{}  STR {}/{}",
            compact_u64(fighters),
            compact_u64(fighter_capacity),
            compact_u64(strikes),
            compact_u64(strike_capacity)
        )));
        lines.push(clip_line(&format!(
            "RES FTR {}  STR {}",
            reinforcement.map_or(0, |record| record.reserve_fighters),
            reinforcement.map_or(0, |record| record.reserve_strike)
        )));
        lines.push(clip_line(&format!(
            "FUNDING {}  FIELDS {}/{}",
            reinforcement.map_or_else(
                || "-".to_owned(),
                |record| { percent(record.operations_coverage) }
            ),
            ready_airfields,
            airfields
        )));

        let task_forces = side.map_or(0, |side| {
            snapshot.operational_snapshot.as_ref().map_or(0, |state| {
                state
                    .task_forces
                    .iter()
                    .filter(|force| {
                        force.side_index == side && force.phase != TaskForcePhase::Complete
                    })
                    .count()
            })
        });
        let naval_operations =
            snapshot
                .operational_execution_snapshot
                .as_ref()
                .map_or(0, |state| {
                    state
                        .naval_operations
                        .iter()
                        .filter(|operation| {
                            operation.country == country_id
                                && operation.phase != NavalOperationPhase::Complete
                        })
                        .count()
                });
        let defender_reactions = side.map_or(0, |side| {
            snapshot
                .operational_execution_snapshot
                .as_ref()
                .map_or(0, |state| {
                    state
                        .defender_reactions
                        .iter()
                        .filter(|reaction| reaction.side == side)
                        .count()
                })
        });
        let selected_combat_events = snapshot
            .frame_snapshot
            .events
            .iter()
            .filter(|event| {
                unit_ids.contains(&event.attacker_id) || unit_ids.contains(&event.target_id)
            })
            .count();

        lines.push(String::new());
        lines.push("OPERATIONS".to_owned());
        lines.push(clip_line(&format!(
            "TASK FORCES {}  NAVAL {}",
            task_forces, naval_operations
        )));
        lines.push(clip_line(&format!(
            "REACTIONS {}  COMBAT {}",
            defender_reactions, selected_combat_events
        )));
        lines.push(String::new());
        lines.push("LEFT CLICK COUNTRY".to_owned());
        lines.push(control_line(save_enabled));
        Self { lines }
    }
}

fn control_line(save_enabled: bool) -> String {
    if save_enabled {
        "H HIDE  R RESET  S SAVE  ESC QUIT".to_owned()
    } else {
        "H HIDE  R RESET  ESC QUIT".to_owned()
    }
}

fn runtime_state_label(state: RuntimeState) -> &'static str {
    match state {
        RuntimeState::Running => "RUNNING",
        RuntimeState::AwaitingStrategicEffects { .. } => "SETTLING",
        RuntimeState::ConflictResolved { .. } => "WAR ENDED",
        RuntimeState::Poisoned => "FAILED",
    }
}

fn command_band_label(band: CommandBand) -> &'static str {
    match band {
        CommandBand::Paid => "PAID",
        CommandBand::Strained => "STRAINED",
        CommandBand::Unpaid => "UNPAID",
        CommandBand::Breakdown => "BREAKDOWN",
        CommandBand::Mutiny => "MUTINY",
    }
}

fn ascii_upper(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_uppercase)
        .map(|character| {
            if character.is_ascii_graphic() || character == ' ' {
                character
            } else {
                '?'
            }
        })
        .collect()
}

fn clip_line(value: &str) -> String {
    value.chars().take(MAX_LINE_CHARS).collect()
}

fn compact_u64(value: u64) -> String {
    compact_f64(value as f64)
}

fn compact_f64(value: f64) -> String {
    let absolute = value.abs();
    if absolute >= 1_000_000_000.0 {
        format!("{:.1}B", value / 1_000_000_000.0)
    } else if absolute >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if absolute >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else {
        format!("{value:.0}")
    }
}

fn percent(value: f64) -> String {
    format!("{:.0}%", value.clamp(0.0, 1.0) * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_panel_is_read_only_and_selection_aware() {
        let idle = ObserverHudModel::without_runtime(None);
        assert!(idle.lines.iter().any(|line| line == "NO ACTIVE SIMULATION"));
        assert!(!idle.lines.iter().any(|line| line.contains("SAVE")));

        let selected = ObserverHudModel::without_runtime(Some((7, "Cote d'Ivoire")));
        assert!(
            selected
                .lines
                .iter()
                .any(|line| line.contains("COTE D'IVOIRE  #7"))
        );
    }

    #[test]
    fn compact_numbers_and_percentages_are_stable() {
        assert_eq!(compact_u64(999), "999");
        assert_eq!(compact_u64(1_250), "1.2K");
        assert_eq!(compact_f64(2_500_000.0), "2.5M");
        assert_eq!(percent(0.999), "100%");
        assert_eq!(percent(-1.0), "0%");
    }

    #[test]
    fn line_clipping_matches_bitmap_panel_capacity() {
        assert_eq!(clip_line(&"X".repeat(100)).len(), MAX_LINE_CHARS);
    }
}
