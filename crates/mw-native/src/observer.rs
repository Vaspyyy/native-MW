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
    use std::{collections::BTreeMap, sync::Arc};

    use mw_core::{
        AIR_SCHEMA_VERSION, AirCountryCoverage, AirPowerState, AirTargetKind, AirWing,
        AirWingState, Airfield, CombatEvent, CombatLayer, CountryAggregate, DefenderReaction,
        DefenderReactionKind, EconomySeed, FrameSnapshot, GameplayRngState,
        MATERIAL_LOGISTICS_SCHEMA_VERSION, MaterialLogisticsCountry, MaterialLogisticsState,
        NATIVE_RUNTIME_SCHEMA_VERSION, NATIVE_TICK_SCHEMA_VERSION, NavalMember, NavalOperation,
        NavalOperationKind, OperationalExecutionState, OperationalPoint, OperationalRuntimeState,
        OperationalTaskForce, Point, REINFORCEMENT_SCHEMA_VERSION, ReinforcementCountry,
        ReinforcementState, RuntimeStepCounters, TERRITORY_SCHEMA_VERSION, TaskForceMember,
        TaskForcePosture, TaskForceRole, TerritorySnapshot, UnitSnapshot, create_economy_state,
    };

    use super::*;

    const SELECTED_COUNTRY: u16 = 42;

    fn unit(
        id: u64,
        country: u16,
        side: u16,
        kind: UnitKind,
        personnel: u64,
        equipment: u64,
        max_equipment: u64,
    ) -> UnitSnapshot {
        UnitSnapshot {
            id,
            side,
            sovereign: u64::from(country),
            kind,
            lat: 50.0 + id as f64 * 0.01,
            lng: 10.0 + id as f64 * 0.01,
            health: 100.0,
            max_health: 100.0,
            health_fraction: 1.0,
            personnel,
            personnel_capacity: personnel,
            equipment,
            max_equipment,
            dir_lat: 0.0,
            dir_lng: 0.0,
            coast_stuck_ticks: 0,
            last_combat_tick: 41,
            victory_boost_ticks: 0,
            landing_penalty_active: false,
            transport: false,
            at_sea: false,
            armor_supported: kind == UnitKind::Armor,
            is_alpenjager: false,
            encircled_ticks: 0,
            mountain_intensity: 0.0,
        }
    }

    fn combat(attacker_id: u64, target_id: u64) -> CombatEvent {
        CombatEvent {
            schema_version: "1",
            layer: CombatLayer::Proximity,
            attacker_id,
            target_id,
            target_damage: 4.0,
            attacker_damage: 2.0,
            transport_self_damage: 0.0,
            target_personnel_loss: 40,
            attacker_personnel_loss: 20,
            target_equipment_loss: 0,
            attacker_equipment_loss: 0,
            target_resulting_health: 96.0,
            attacker_resulting_health: 98.0,
            target_knockback_blocked: false,
            attacker_knockback_blocked: false,
        }
    }

    fn task_force(phase: TaskForcePhase) -> OperationalTaskForce {
        OperationalTaskForce {
            id: "observer-task-force".to_owned(),
            signature: "observer-plan".to_owned(),
            side_index: 0,
            plan_signature: "observer-plan".to_owned(),
            plan_type: "PUSH_FRONT".to_owned(),
            theater_id: None,
            target: Some(OperationalPoint {
                lat: 51.0,
                lng: 12.0,
            }),
            staging_anchor: Some(OperationalPoint {
                lat: 50.0,
                lng: 10.0,
            }),
            route: Vec::new(),
            phase,
            posture: TaskForcePosture::Balanced,
            members: vec![TaskForceMember {
                unit_id: 1,
                role: TaskForceRole::Line,
                assigned_tick: 1,
                route_progress: 0.25,
            }],
            reserve_unit_ids: Vec::new(),
            desired_power: 1.0,
            launch_power: 1.0,
            current_power: 1.0,
            peak_power: 1.0,
            readiness: 1.0,
            max_assigned_units: 1,
            created_tick: 1,
            phase_started_tick: 1,
            last_progress_tick: 41,
            last_recovery_tick: 0,
            recovery_power: 0.0,
            progress: 0.25,
            withdrawal_anchor: None,
            completion_reason: None,
            outcome: None,
            severe_surprise: false,
            parent_task_force_id: None,
            supply_invalidated_tick: None,
            intent_revision: 1,
        }
    }

    fn rich_runtime_snapshot() -> RuntimeSnapshot {
        let frame_snapshot = FrameSnapshot {
            schema_version: NATIVE_TICK_SCHEMA_VERSION,
            tick: 42,
            frame: 7,
            units: Arc::from([
                unit(1, SELECTED_COUNTRY, 0, UnitKind::Army, 1_200, 0, 0),
                unit(2, SELECTED_COUNTRY, 0, UnitKind::Armor, 800, 90, 120),
                unit(99, 77, 1, UnitKind::Army, 900, 0, 0),
            ]),
            events: Arc::from([combat(1, 99)]),
            removed_ids: Arc::from([]),
            abandoned_ids: Arc::from([]),
        };
        let territory_snapshot = TerritorySnapshot {
            schema_version: TERRITORY_SCHEMA_VERSION.to_owned(),
            generation: 3,
            commit_sequence: 4,
            topology_revision: 1,
            world_revision: 1,
            city_revision: 1,
            processed_tiles: 8,
            processed_items: 1_000,
            pending_dirty_tiles_at_commit: 0,
            land_cells: 1_500,
            positive_occupation_cells: 50,
            negative_occupation_cells: 0,
            countries: vec![CountryAggregate {
                country_id: SELECTED_COUNTRY,
                side_index: 0,
                owned: 1_000,
                controlled: 800,
                frontline: 123,
                cities_total: 7,
                cities_controlled: 5,
                capital_held: true,
                ..CountryAggregate::default()
            }],
            sides: Vec::new(),
        };
        let mut economy = create_economy_state(EconomySeed {
            country_id: SELECTED_COUNTRY,
            gdp: 1_000_000.0,
            population: 10_000_000.0,
            territory_units: 1_000.0,
            initial_core_cells: 1_000,
            initial_city_population: 10_000_000.0,
        })
        .unwrap();
        economy.treasury = 2_500_000.0;
        economy.income = 12_500.0;
        economy.payroll_coverage = 0.75;
        economy.occupation_coverage = 0.60;
        economy.core_control_ratio = 0.80;
        economy.city_control_ratio = 5.0 / 7.0;
        economy.command_band = CommandBand::Strained;

        let air_power = AirPowerState {
            schema: AIR_SCHEMA_VERSION.to_owned(),
            country_coverage: vec![AirCountryCoverage {
                country_id: SELECTED_COUNTRY,
                operations_coverage: 0.5,
            }],
            airfields: vec![
                Airfield {
                    id: 10,
                    side: 0,
                    owner_country_id: SELECTED_COUNTRY,
                    controller_country_id: SELECTED_COUNTRY,
                    lat: 50.0,
                    lng: 10.0,
                    capacity: 1,
                    health: 100.0,
                    disabled: false,
                    capture_repair_cycles: 0,
                    capital: true,
                },
                Airfield {
                    id: 11,
                    side: 0,
                    owner_country_id: SELECTED_COUNTRY,
                    controller_country_id: SELECTED_COUNTRY,
                    lat: 51.0,
                    lng: 11.0,
                    capacity: 1,
                    health: 0.0,
                    disabled: true,
                    capture_repair_cycles: 2,
                    capital: false,
                },
            ],
            wings: vec![
                AirWing {
                    id: 20,
                    side: 0,
                    sovereign_country_id: SELECTED_COUNTRY,
                    airfield_id: 10,
                    return_airfield_id: None,
                    role: AirRole::Fighter,
                    quality: 60.0,
                    max_count: 24,
                    count: 12,
                    lat: 50.0,
                    lng: 10.0,
                    state: AirWingState::Patrol,
                    target_kind: None,
                    target_id: None,
                    rearm_ticks: 0,
                    cooldown_ticks: 0,
                    endurance_ticks: 100,
                    next_mission_tick: Some(120),
                    force_mission: false,
                },
                AirWing {
                    id: 21,
                    side: 0,
                    sovereign_country_id: SELECTED_COUNTRY,
                    airfield_id: 10,
                    return_airfield_id: Some(10),
                    role: AirRole::Strike,
                    quality: 55.0,
                    max_count: 16,
                    count: 8,
                    lat: 50.5,
                    lng: 10.5,
                    state: AirWingState::Attacking,
                    target_kind: Some(AirTargetKind::Army),
                    target_id: Some(99),
                    rearm_ticks: 0,
                    cooldown_ticks: 0,
                    endurance_ticks: 0,
                    next_mission_tick: Some(180),
                    force_mission: false,
                },
            ],
        };

        let mut operations = OperationalRuntimeState::bootstrap(2, &[0, 1, 1, 0], &[2.0, 1.0]);
        operations.task_forces = vec![
            task_force(TaskForcePhase::Attacking),
            task_force(TaskForcePhase::Complete),
        ];
        let mut execution = OperationalExecutionState::new();
        execution.naval_operations.push(NavalOperation {
            id: "observer-naval".to_owned(),
            signature: "observer-naval".to_owned(),
            kind: NavalOperationKind::Invasion,
            phase: NavalOperationPhase::Transit,
            side: 0,
            country: SELECTED_COUNTRY,
            enemy_side: Some(1),
            max_assigned_units: 1,
            members: vec![NavalMember {
                unit_id: 1,
                role: "LINE".to_owned(),
                assigned_tick: 1,
            }],
            staging: Point {
                lat: 50.0,
                lng: 10.0,
            },
            target: Point {
                lat: 52.0,
                lng: 14.0,
            },
            route: Vec::new(),
            route_index: 0,
            progress: 0.25,
            started_tick: 1,
            phase_started_tick: 1,
            last_progress_tick: 41,
            completion_reason: None,
        });
        execution.defender_reactions.push(DefenderReaction {
            id: "observer-reaction".to_owned(),
            sequence: 1,
            threat_signature: "observer-threat".to_owned(),
            side: 0,
            enemy_side: 1,
            kind: DefenderReactionKind::Landing,
            target: Point {
                lat: 51.0,
                lng: 12.0,
            },
            unit_ids: vec![2],
            max_units: 1,
            started_tick: 1,
            last_progress_tick: 41,
            best_distance_squared: Some(1.0),
            landing_defeated_tick: None,
        });

        RuntimeSnapshot {
            schema_version: NATIVE_RUNTIME_SCHEMA_VERSION,
            tick: 42,
            frame: 7,
            game_calendar_snapshot: None,
            state: RuntimeState::Running,
            frame_snapshot: Arc::new(frame_snapshot),
            territory_snapshot: Arc::new(territory_snapshot),
            economy_snapshot: Arc::from([economy]),
            strategic_snapshot: None,
            operational_snapshot: Some(Arc::new(operations.snapshot(42))),
            operational_execution_snapshot: Some(Arc::new(execution)),
            air_power_snapshot: Some(Arc::new(air_power)),
            strategic_missile_snapshot: None,
            counters: RuntimeStepCounters::default(),
            pending_render_updates: 0,
            casualty_totals: Arc::new(BTreeMap::from([(SELECTED_COUNTRY, 3_456.0)])),
            casualties_by_victim: Arc::new(BTreeMap::new()),
            gameplay_rng_state: GameplayRngState::default(),
            personnel_reserves: Arc::new(BTreeMap::from([(0, 12_000.0)])),
            reinforcement_snapshot: Some(Arc::new(ReinforcementState {
                schema: REINFORCEMENT_SCHEMA_VERSION.to_owned(),
                next_unit_id: 100,
                next_air_wing_id: 22,
                countries: vec![ReinforcementCountry {
                    country_id: SELECTED_COUNTRY,
                    fighter_capacity: 24,
                    strike_capacity: 16,
                    reserve_fighters: 4,
                    reserve_strike: 7,
                    air_operations_due: 100.0,
                    operations_coverage: 0.5,
                    replacement_spent: 25.0,
                }],
            })),
            material_logistics_snapshot: Some(Arc::new(MaterialLogisticsState {
                schema: MATERIAL_LOGISTICS_SCHEMA_VERSION.to_owned(),
                countries: vec![MaterialLogisticsCountry {
                    country_id: SELECTED_COUNTRY,
                    armor_capacity: 150,
                    reserve_armor: 30,
                    armor_quality: 0.8,
                    armor_replacement_spent: 20.0,
                    airfield_repair_spent: 5.0,
                }],
            })),
        }
    }

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

    #[test]
    fn rich_runtime_projection_covers_the_complete_observer_boundary() {
        let model = ObserverHudModel::from_runtime(
            &rich_runtime_snapshot(),
            Some(SELECTED_COUNTRY),
            "Test Republic",
            true,
        );

        for expected in [
            "CONTROL 800 / OWN 1.0K",
            "CITIES 5/7  FRONT 123",
            "TREASURY 2.5M  INCOME 12.5K",
            "PAYROLL 75%  OCC 60%",
            "CORE 80%  CITY 71%",
            "ARMY 1  ARMOR 1",
            "PERSONNEL 2.0K  LOSSES 3.5K",
            "ARMOR 90/120  RES 30",
            "SIDE MANPOWER 12.0K",
            "FTR 12/24  STR 8/16",
            "RES FTR 4  STR 7",
            "FUNDING 50%  FIELDS 1/2",
            "TASK FORCES 1  NAVAL 1",
            "REACTIONS 1  COMBAT 1",
            "H HIDE  R RESET  S SAVE  ESC QUIT",
        ] {
            assert!(
                model.lines.iter().any(|line| line == expected),
                "missing observer line {expected:?} in {:#?}",
                model.lines
            );
        }
    }

    #[test]
    fn newer_copy_on_write_state_cannot_mutate_a_published_observer_snapshot() {
        let published = Arc::new(rich_runtime_snapshot());
        let frozen = ObserverHudModel::from_runtime(
            &published,
            Some(SELECTED_COUNTRY),
            "Test Republic",
            true,
        );
        let mut next = published.as_ref().clone();
        assert!(Arc::ptr_eq(&published.frame_snapshot, &next.frame_snapshot));
        assert!(Arc::ptr_eq(
            &published.territory_snapshot,
            &next.territory_snapshot
        ));

        Arc::make_mut(&mut next.territory_snapshot).countries[0].controlled = 1;
        Arc::make_mut(&mut next.economy_snapshot)[0].treasury = 1.0;
        Arc::make_mut(&mut Arc::make_mut(&mut next.frame_snapshot).units)[0].personnel = 1;
        Arc::make_mut(&mut next.casualty_totals).insert(SELECTED_COUNTRY, 1.0);
        Arc::make_mut(next.air_power_snapshot.as_mut().unwrap()).wings[0].count = 1;
        Arc::make_mut(next.reinforcement_snapshot.as_mut().unwrap()).countries[0]
            .reserve_fighters = 1;
        Arc::make_mut(next.material_logistics_snapshot.as_mut().unwrap()).countries[0]
            .reserve_armor = 1;
        Arc::make_mut(next.operational_snapshot.as_mut().unwrap())
            .task_forces
            .clear();
        Arc::make_mut(next.operational_execution_snapshot.as_mut().unwrap())
            .naval_operations
            .clear();

        assert!(!Arc::ptr_eq(
            &published.frame_snapshot,
            &next.frame_snapshot
        ));
        assert!(!Arc::ptr_eq(
            &published.territory_snapshot,
            &next.territory_snapshot
        ));
        assert_eq!(
            ObserverHudModel::from_runtime(
                &published,
                Some(SELECTED_COUNTRY),
                "Test Republic",
                true,
            ),
            frozen
        );
        assert_ne!(
            ObserverHudModel::from_runtime(&next, Some(SELECTED_COUNTRY), "Test Republic", true,),
            frozen
        );
    }
}
