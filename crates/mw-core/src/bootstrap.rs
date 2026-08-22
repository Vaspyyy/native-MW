//! Deterministic construction of a playable native war from an MWSC scenario.

use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

use crate::{
    air::{
        AirCountryCoverage, AirError, AirPowerState, AirRole, AirTargetKind, AirWing, AirWingState,
        Airfield,
    },
    battlefield::{
        BattlefieldBuff, BattlefieldConfig, BattlefieldRuntimeState, BattlefieldUnitState,
        BattlefieldUrbanCenter, BattlefieldWarPhase, CountryBattlefieldPrimitives,
    },
    combat::{CombatUnit, UnitKind, formation_strength},
    dynamics::bootstrap_sides,
    economy::{
        EconomyState, PAYROLL_PER_UNIT, STARTING_RESERVE_CYCLES, TARGET_STARTING_PAYROLL_SHARE,
    },
    naval_planning::{NavalPlanningError, NavalPlanningState},
    operational_execution::OperationalExecutionState,
    operations::{CountryDesperationMode, CountryDesperationState, OperationalRuntimeState},
    production::{ProductionConfig, derive_scenario_production},
    reinforcement::{AIRCREW_PER_AIRCRAFT, ReinforcementError, ReinforcementState},
    runtime::{
        NativeRuntime, RuntimeCheckpoint, RuntimeConfig, RuntimeDiplomacy, RuntimeUnitPolicy,
    },
    scenario::DecodedScenario,
    simulation::{Simulation, SimulationConfig, SimulationUnit},
    strategic::StrategicSimulation,
    territory::{TerritoryCity, TerritoryConfig, TerritoryControl, TerritoryMaps},
    world::WorldGridView,
};

#[derive(Clone, Debug)]
pub struct NativeWarBootstrapConfig {
    pub sides: Vec<Vec<u16>>,
    pub hostility: Option<Vec<u8>>,
    pub production: ProductionConfig,
    pub war_grace_end: u64,
}

#[derive(Debug, Error)]
pub enum NativeWarBootstrapError {
    #[error("invalid side topology: {0}")]
    Topology(String),
    #[error("scenario production: {0}")]
    Production(#[from] crate::production::ProductionError),
    #[error("territory: {0}")]
    Territory(#[from] crate::territory::TerritoryError),
    #[error("simulation: {0}")]
    Simulation(#[from] crate::simulation::SimulationError),
    #[error("air power: {0}")]
    Air(#[from] AirError),
    #[error("naval planning: {0}")]
    NavalPlanning(#[from] NavalPlanningError),
    #[error("reinforcement: {0}")]
    Reinforcement(#[from] ReinforcementError),
    #[error("strategic: {0}")]
    Strategic(#[from] crate::strategic::StrategicError),
    #[error("runtime: {0}")]
    Runtime(#[from] crate::runtime::RuntimeError),
    #[error("scenario battlefield metadata is invalid: {0}")]
    BattlefieldMetadata(String),
}

fn bootstrap_air_power(
    production: &crate::production::ScenarioProduction,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<AirPowerState, AirError> {
    let mut fields = Vec::new();
    let mut wings = Vec::new();
    for (&country_id, &side) in country_to_side {
        let country = production
            .countries
            .iter()
            .find(|country| country.country_id == country_id)
            .expect("side topology was derived from production countries");
        let position = production
            .cities
            .iter()
            .filter(|city| city.owner_id == country_id)
            .max_by(|left, right| {
                left.capital
                    .cmp(&right.capital)
                    .then_with(|| left.population.total_cmp(&right.population))
                    .then_with(|| right.city_id.cmp(&left.city_id))
            })
            .map(|city| (city.lat, city.lng))
            .or_else(|| {
                country.capital_cell.map(|cell| {
                    let x = cell % production.grid.width;
                    let y = cell / production.grid.width;
                    (
                        -90.0 + (y as f64 + 0.5) * production.grid.grid_res,
                        -180.0 + (x as f64 + 0.5) * production.grid.grid_res,
                    )
                })
            });
        let Some((lat, lng)) = position else {
            continue;
        };
        let field_id = fields.len() as u64 + 1;
        fields.push(Airfield {
            id: field_id,
            side,
            owner_country_id: country_id,
            controller_country_id: country_id,
            lat,
            lng,
            capacity: 8,
            health: 100.0,
            disabled: false,
            capture_repair_cycles: 0,
            capital: true,
        });
        for role in [AirRole::Fighter, AirRole::Strike] {
            wings.push(AirWing {
                id: wings.len() as u64 + 1,
                side,
                sovereign_country_id: country_id,
                airfield_id: field_id,
                return_airfield_id: None,
                role,
                quality: 50.0,
                max_count: 100,
                count: 100,
                lat,
                lng,
                state: AirWingState::Grounded,
                target_kind: None::<AirTargetKind>,
                target_id: None,
                rearm_ticks: 0,
                cooldown_ticks: 0,
                endurance_ticks: 0,
                next_mission_tick: None,
                force_mission: false,
            });
        }
    }
    let mut air_power = AirPowerState::new(fields, wings)?;
    air_power.country_coverage = country_to_side
        .keys()
        .copied()
        .map(|country_id| AirCountryCoverage {
            country_id,
            operations_coverage: 1.0,
        })
        .collect();
    air_power.validate()?;
    Ok(air_power)
}

fn parse_battlefield_buff(value: Option<&serde_json::Value>) -> BattlefieldBuff {
    match value.and_then(serde_json::Value::as_str) {
        Some("buff") => BattlefieldBuff::Buff,
        Some("super") => BattlefieldBuff::Super,
        Some("godly") => BattlefieldBuff::Godly,
        Some("weakened") => BattlefieldBuff::Weakened,
        Some("crippled") => BattlefieldBuff::Crippled,
        _ => BattlefieldBuff::None,
    }
}

fn scenario_country_metadata(
    scenario: &DecodedScenario,
) -> impl Iterator<Item = &serde_json::Map<String, serde_json::Value>> {
    scenario
        .metadata
        .get("metadata")
        .and_then(serde_json::Value::as_array)
        .or_else(|| {
            scenario
                .metadata
                .get("countries")
                .and_then(serde_json::Value::as_array)
        })
        .or_else(|| scenario.metadata.as_array())
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_object)
}

fn bootstrap_country_battlefield(
    scenario: &DecodedScenario,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<BTreeMap<u16, CountryBattlefieldPrimitives>, NativeWarBootstrapError> {
    let metadata = scenario_country_metadata(scenario)
        .filter_map(|country| {
            let id = country
                .get("id")?
                .as_u64()
                .and_then(|id| u16::try_from(id).ok())?;
            Some((id, country))
        })
        .collect::<BTreeMap<_, _>>();
    country_to_side
        .keys()
        .map(|&country_id| {
            let raw = metadata.get(&country_id).copied();
            let influence_buff = parse_battlefield_buff(raw.and_then(|raw| raw.get("buffState")));
            let hidden = parse_battlefield_buff(raw.and_then(|raw| raw.get("hiddenBuffState")));
            let combat_buff = if hidden == BattlefieldBuff::None {
                influence_buff
            } else {
                hidden
            };
            let number = |key: &str, fallback: f64| {
                raw.and_then(|raw| raw.get(key))
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(fallback)
            };
            let attack_buff_percent = number("attackBuffPercent", 0.0);
            let defense_buff_percent = number("defenseBuffPercent", 0.0);
            let ai_speed_multiplier = number("aiSpeedMultiplier", 1.0);
            if !attack_buff_percent.is_finite()
                || !defense_buff_percent.is_finite()
                || !ai_speed_multiplier.is_finite()
                || ai_speed_multiplier < 0.0
            {
                return Err(NativeWarBootstrapError::BattlefieldMetadata(format!(
                    "country {country_id} has invalid battlefield modifiers"
                )));
            }
            Ok((
                country_id,
                CountryBattlefieldPrimitives {
                    combat_buff,
                    influence_buff,
                    attack_buff_percent,
                    defense_buff_percent,
                    capital_lost: false,
                    war_phase: BattlefieldWarPhase::Stable,
                    conquest_mode: true,
                    ai_speed_multiplier,
                },
            ))
        })
        .collect()
}

fn bootstrap_terrain(
    scenario: &DecodedScenario,
) -> Result<(bool, Vec<f32>), NativeWarBootstrapError> {
    let cell_count = scenario
        .target
        .cell_count()
        .map_err(|error| NativeWarBootstrapError::BattlefieldMetadata(error.to_string()))?;
    let mut terrain = vec![0.0_f32; cell_count];
    let Some(entries) = scenario
        .metadata
        .get("mountainData")
        .and_then(serde_json::Value::as_array)
    else {
        // The stock MWSC does not contain the browser's separately loaded mountain GeoJSON.
        // Flat terrain is therefore explicit rather than pretending mountains are enabled.
        return Ok((false, terrain));
    };
    let source_cells = scenario
        .source
        .cell_count()
        .map_err(|error| NativeWarBootstrapError::BattlefieldMetadata(error.to_string()))?;
    let world = WorldGridView::new(
        scenario.target.grid_res,
        scenario.target.width,
        scenario.target.height,
        &scenario.land,
    )
    .map_err(|error| NativeWarBootstrapError::BattlefieldMetadata(error.to_string()))?;
    for (entry_index, entry) in entries.iter().enumerate() {
        let pair = entry.as_array().ok_or_else(|| {
            NativeWarBootstrapError::BattlefieldMetadata(format!(
                "mountainData[{entry_index}] must be [cell,intensity]"
            ))
        })?;
        if pair.len() != 2 {
            return Err(NativeWarBootstrapError::BattlefieldMetadata(format!(
                "mountainData[{entry_index}] must have two values"
            )));
        }
        let source_cell = pair[0]
            .as_u64()
            .and_then(|cell| usize::try_from(cell).ok())
            .filter(|cell| *cell < source_cells)
            .ok_or_else(|| {
                NativeWarBootstrapError::BattlefieldMetadata(format!(
                    "mountainData[{entry_index}] has an invalid source cell"
                ))
            })?;
        let intensity = pair[1]
            .as_f64()
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
            .ok_or_else(|| {
                NativeWarBootstrapError::BattlefieldMetadata(format!(
                    "mountainData[{entry_index}] has an invalid intensity"
                ))
            })? as f32;
        let source_x = source_cell % scenario.source.width;
        let source_y = source_cell / scenario.source.width;
        let lat = -90.0 + (source_y as f64 + 0.5) * scenario.source.grid_res;
        let lng = -180.0 + (source_x as f64 + 0.5) * scenario.source.grid_res;
        let target_cell = world.grid_index(lat, lng).ok_or_else(|| {
            NativeWarBootstrapError::BattlefieldMetadata(format!(
                "mountainData[{entry_index}] cannot be projected to the target grid"
            ))
        })?;
        terrain[target_cell] = terrain[target_cell].max(intensity);
    }
    Ok((true, terrain))
}

pub fn bootstrap_native_war(
    scenario: &DecodedScenario,
    config: &NativeWarBootstrapConfig,
) -> Result<NativeRuntime, NativeWarBootstrapError> {
    let production = derive_scenario_production(scenario, &config.production)?;
    let known: BTreeSet<u16> = production.countries.iter().map(|c| c.country_id).collect();
    let mut topology = Vec::new();
    let mut country_to_side = BTreeMap::new();
    for side in &config.sides {
        let mut ids = side.clone();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() != side.len() {
            return Err(NativeWarBootstrapError::Topology(
                "duplicate country in side".into(),
            ));
        }
        if ids.iter().any(|id| *id == 0 || !known.contains(id)) {
            return Err(NativeWarBootstrapError::Topology("unknown country".into()));
        }
        if ids.is_empty() {
            continue;
        }
        let n = topology.len();
        for id in &ids {
            if country_to_side.insert(*id, n).is_some() {
                return Err(NativeWarBootstrapError::Topology(
                    "country appears in multiple sides".into(),
                ));
            }
        }
        topology.push(ids);
    }
    if topology.len() < 2 {
        return Err(NativeWarBootstrapError::Topology(
            "at least two nonempty sides required".into(),
        ));
    }
    let n = topology.len();
    let hostility = if let Some(h) = &config.hostility {
        if h.len() != n * n {
            return Err(NativeWarBootstrapError::Topology(
                "hostility matrix length mismatch".into(),
            ));
        }
        h.iter().map(|v| u8::from(*v != 0)).collect::<Vec<_>>()
    } else {
        (0..n * n).map(|i| u8::from(i / n != i % n)).collect()
    };
    if (0..n).any(|side| hostility[side * n + side] != 0) {
        return Err(NativeWarBootstrapError::Topology(
            "a side cannot be hostile to itself".into(),
        ));
    }
    if !(0..n).any(|a| (0..n).any(|b| a != b && hostility[a * n + b] != 0)) {
        return Err(NativeWarBootstrapError::Topology(
            "no hostile direction".into(),
        ));
    }

    let mut units = Vec::new();
    let mut policies = Vec::new();
    let mut next_id = 1u64;
    for (side_index, countries) in topology.iter().enumerate() {
        for country_id in countries {
            let country = production
                .countries
                .iter()
                .find(|c| c.country_id == *country_id)
                .unwrap();
            let mut cells: Vec<usize> = scenario
                .land
                .iter()
                .enumerate()
                .filter_map(|(i, land)| {
                    (*land != 0 && scenario.world_control[i] == *country_id).then_some(i)
                })
                .collect();
            if cells.is_empty() {
                return Err(NativeWarBootstrapError::Topology(format!(
                    "country {country_id} has no owned land on the target grid"
                )));
            }
            if let Some(cap) = country.capital_cell
                && let Some(pos) = cells.iter().position(|c| *c == cap)
            {
                cells.rotate_left(pos);
            }
            let count = country.expected_army_units.floor() as usize;
            if count == 0 {
                return Err(NativeWarBootstrapError::Topology(format!(
                    "country {country_id} produces no starting formations"
                )));
            }
            for ordinal in 0..count {
                let cell = cells[ordinal % cells.len()];
                let x = cell % scenario.target.width;
                let y = cell / scenario.target.width;
                let lat = -90.0 + (y as f64 + 0.5) * scenario.target.grid_res;
                let lng = -180.0 + (x as f64 + 0.5) * scenario.target.grid_res;
                units.push(SimulationUnit {
                    combat: CombatUnit {
                        id: next_id,
                        side: side_index as u64,
                        sovereign: *country_id as u64,
                        kind: UnitKind::Army,
                        lat,
                        lng,
                        health: 100.0,
                        max_health: 100.0,
                        personnel: 1000,
                        personnel_capacity: 1000,
                        equipment: 0,
                        max_equipment: 0,
                        quality: 50.0,
                        transport: false,
                        armor_supported: false,
                        landing_penalty_active: false,
                        at_sea: false,
                        last_combat_tick: 0,
                        victory_boost_ticks: 0,
                    },
                    dir_lat: 0.0,
                    dir_lng: 0.0,
                    coast_stuck_ticks: 0,
                    armor_landing_penalty_until_tick: 0,
                    is_support: false,
                    ally_weight: 1.0,
                });
                policies.push((next_id, *country_id));
                next_id += 1;
            }
        }
    }
    let simulation = Simulation::new(SimulationConfig::default(), units)?;
    let cell_count = scenario
        .target
        .cell_count()
        .map_err(|e| NativeWarBootstrapError::Topology(e.to_string()))?;
    let mut runtime_land = vec![0u8; cell_count];
    let mut primary = vec![0u16; cell_count];
    let mut dominant = vec![-1i16; cell_count];
    let mut influence = vec![vec![0.0; cell_count]; n];
    let mut occupation = vec![0.0; cell_count];
    for i in 0..cell_count {
        if scenario.land[i] == 0 {
            continue;
        }
        runtime_land[i] = 1;
        if let Some(&side) = country_to_side.get(&scenario.world_control[i]) {
            runtime_land[i] = 2;
            primary[i] = scenario.world_control[i];
            dominant[i] = side as i16;
            influence[side][i] = 1.0;
            occupation[i] = if side % 2 == 0 { 1.0 } else { -1.0 };
        }
    }
    let cities = production
        .cities
        .iter()
        .map(|c| TerritoryCity {
            id: c.city_id,
            cell: c.cell,
            owner: c.owner_id,
            population: c.population,
            capital: c.capital,
        })
        .collect();
    let mut territory = TerritoryControl::new(TerritoryConfig {
        width: scenario.target.width,
        height: scenario.target.height,
        grid_resolution: scenario.target.grid_res,
        max_sides: n,
        tile_size: 32,
        maps: TerritoryMaps {
            land: runtime_land,
            world_control: scenario.world_control.clone(),
            de_jure: scenario.de_jure.clone(),
            primary_occupier: primary,
            dominant_side: dominant,
            occupation,
            side_influence: influence,
        },
        country_to_side: country_to_side.clone(),
        hostility_matrix: hostility.clone(),
        cities,
        protected_owner_ids: country_to_side.keys().copied().collect(),
        topology_revision: 1,
        world_revision: 1,
        city_revision: 1,
    })?;
    territory.enable_influence_runtime();
    // Browser war initialization assigns active cells in row-major order.
    // Every first controller transition queues center/orthogonal priority work,
    // including neighboring cells outside the theater scan, until the exact
    // bounded queue fills.
    for cell in 0..territory.total_cells() {
        if territory.land()[cell] == 2 {
            territory.queue_influence_runtime_cell(cell, true)?;
        }
    }
    let mut economies: Vec<EconomyState> = production
        .economy_states
        .iter()
        .filter(|e| country_to_side.contains_key(&e.country_id))
        .cloned()
        .collect();
    for economy in &mut economies {
        let payroll = simulation
            .units
            .iter()
            .filter(|u| u.combat.sovereign as u16 == economy.country_id)
            .count() as f64
            * PAYROLL_PER_UNIT;
        let base = (payroll / TARGET_STARTING_PAYROLL_SHARE).max(economy.base_income);
        economy.base_income = base;
        economy.income = base;
        economy.treasury = base * STARTING_RESERVE_CYCLES;
        economy.payroll_due = payroll;
    }
    let strategic = StrategicSimulation::new(economies, Vec::new())?;
    let unit_policies = policies
        .into_iter()
        .map(|(id, country)| {
            let mut p = RuntimeUnitPolicy::standard(id, country);
            let influence = p.influence.as_mut().unwrap();
            influence.owner_ally_country_ids = topology[country_to_side[&country]]
                .iter()
                .copied()
                .collect();
            // Native-started formations still use the browser mobilization ramp and temporal
            // noise. Sequential native IDs are the stable local seed at this boundary.
            influence.browser_temporal_seed = Some(id as f64);
            p
        })
        .collect();
    let (mountains_enabled, terrain_intensity) = bootstrap_terrain(scenario)?;
    let battlefield = BattlefieldRuntimeState {
        config: BattlefieldConfig::default(),
        mountains_enabled,
        terrain_intensity,
        urban_centers: production
            .cities
            .iter()
            .filter(|city| country_to_side.contains_key(&city.owner_id))
            .map(|city| BattlefieldUrbanCenter {
                id: city.city_id,
                country_id: city.owner_id,
                cell: city.cell,
                lat: city.lat,
                lng: city.lng,
            })
            .collect(),
        countries: bootstrap_country_battlefield(scenario, &country_to_side)?,
        units: simulation
            .units
            .iter()
            .map(|unit| {
                (
                    unit.combat.id,
                    BattlefieldUnitState {
                        // Browser unit identifiers often carry a fractional component; the
                        // four-way group is `floor(id * 1000) % 4`. Native bootstrap IDs are
                        // integers, so use an equivalent stable seed that does not collapse every
                        // formation into group zero.
                        cohesion_seed: (unit.combat.id % 4) as f64 / 1_000.0,
                        ..BattlefieldUnitState::default()
                    },
                )
            })
            .collect(),
    };
    let armor_crew_per_vehicle = simulation.config().combat.armor_crew_per_vehicle;
    let mut side_dynamics = bootstrap_sides(
        n,
        simulation.units.iter().map(|unit| {
            let personnel = if unit.combat.kind == UnitKind::Armor {
                unit.combat.equipment.saturating_mul(armor_crew_per_vehicle) as f64
            } else {
                unit.combat.personnel as f64
            };
            (unit.combat.side as usize, personnel)
        }),
    );
    let mut operational_strength = vec![0.0; n];
    let mut personnel_by_country = BTreeMap::<u16, f64>::new();
    for unit in &simulation.units {
        operational_strength[unit.combat.side as usize] += formation_strength(&unit.combat);
        let country = unit.combat.sovereign as u16;
        let personnel = if unit.combat.kind == UnitKind::Armor {
            unit.combat.equipment.saturating_mul(armor_crew_per_vehicle) as f64
        } else {
            unit.combat.personnel as f64
        };
        *personnel_by_country.entry(country).or_default() += personnel;
    }
    let mut operations = OperationalRuntimeState::bootstrap(n, &hostility, &operational_strength);
    operations.country_desperation = production
        .countries
        .iter()
        .map(|country| CountryDesperationState {
            country_id: country.country_id,
            mode: CountryDesperationMode::Normal,
            initial_cities: Some(
                production
                    .cities
                    .iter()
                    .filter(|city| city.owner_id == country.country_id)
                    .count() as u64,
            ),
            initial_manpower: Some(
                personnel_by_country
                    .get(&country.country_id)
                    .copied()
                    .unwrap_or(0.0),
            ),
            previous_controlled: Some(u64::from(country.initial_core_cells)),
            stall_ticks: 0,
        })
        .collect();
    let air_power = bootstrap_air_power(&production, &country_to_side)?;
    let next_air_wing_id = air_power
        .wings
        .iter()
        .map(|wing| wing.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| {
            NativeWarBootstrapError::Topology("air-wing ID allocator overflowed".into())
        })?;
    let reinforcement =
        ReinforcementState::bootstrap(&air_power, next_id, next_air_wing_id, &country_to_side, n)?;

    let mut population_pool = vec![0.0; n];
    for country in production.countries.iter() {
        if let Some(&side) = country_to_side.get(&country.country_id) {
            population_pool[side] += country.population * 0.01;
        }
    }
    let mut deployed_land_crews = vec![0.0; n];
    for unit in &simulation.units {
        let personnel = if unit.combat.kind == UnitKind::Armor {
            unit.combat.equipment.saturating_mul(armor_crew_per_vehicle) as f64
        } else {
            unit.combat.personnel as f64
        };
        deployed_land_crews[unit.combat.side as usize] += personnel;
    }
    let mut deployed_air_crews = vec![0.0; n];
    for wing in &air_power.wings {
        deployed_air_crews[wing.side] += f64::from(wing.count) * AIRCREW_PER_AIRCRAFT;
    }
    let personnel_reserves = (0..n)
        .map(|side| {
            let reserve =
                (population_pool[side] - deployed_land_crews[side] - deployed_air_crews[side])
                    .max(0.0);
            let added_personnel = reserve + deployed_air_crews[side];
            let dynamics = side_dynamics
                .get_mut(&side)
                .expect("side dynamics exactly cover bootstrap topology");
            dynamics.initial_personnel += added_personnel;
            dynamics.current_personnel += added_personnel;
            (side, reserve)
        })
        .collect();
    Ok(NativeRuntime::new(
        RuntimeConfig::default(),
        RuntimeCheckpoint {
            tick: 0,
            frame: 0,
            war_grace_end: config.war_grace_end,
            simulation,
            territory,
            strategic,
            scenario: production,
            diplomacy: RuntimeDiplomacy {
                hostility,
                active_sides: (0..n as u16).collect(),
            },
            unit_policies,
            battlefield: Some(battlefield),
            objectives: Vec::new(),
            prior_objective_by_unit: BTreeMap::new(),
            front_prior_by_unit: BTreeMap::new(),
            last_front_refresh_tick: None,
            casualties: BTreeMap::new(),
            casualties_by_victim: BTreeMap::new(),
            gameplay_rng: crate::gameplay_rng::GameplayRngState {
                state: crate::gameplay_rng::DEFAULT_GAMEPLAY_RNG_SEED,
            },
            personnel_reserves,
            side_dynamics: Some(side_dynamics),
            operations: Some(operations),
            naval_planning: Some(NavalPlanningState::bootstrap(n)?),
            operational_execution: Some(OperationalExecutionState::default()),
            air_power: Some(air_power),
            reinforcement: Some(reinforcement),
        },
    )?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{scenario::GridSpec, world::WorldGridView};

    fn scenario() -> DecodedScenario {
        let target = GridSpec::world(90.0).unwrap();
        DecodedScenario {
            metadata: json!({
                "countries": [
                    {"id": 30, "name": "Gamma", "gdp": 300.0, "population": 30_000.0},
                    {"id": 10, "name": "Alpha", "gdp": 100.0, "population": 10_000.0},
                    {"id": 20, "name": "Beta", "gdp": 200.0, "population": 20_000.0}
                ],
                "cities": [
                    {"id": 3, "name": "Gamma City", "lat": 45.0, "lng": 225.0, "pop": 300.0, "ownerId": 30, "isCapital": true},
                    {"id": 1, "name": "Alpha City", "lat": -45.0, "lng": -135.0, "pop": 100.0, "ownerId": 10, "isCapital": true},
                    {"id": 2, "name": "Beta City", "lat": -45.0, "lng": -45.0, "pop": 200.0, "ownerId": 20, "isCapital": true}
                ]
            }),
            source: target,
            target,
            entry_count: 8,
            world_control: vec![10, 20, 30, 0, 10, 20, 30, 0],
            de_jure: vec![10, 20, 30, 0, 10, 20, 30, 0],
            land: vec![1, 1, 1, 0, 1, 1, 1, 0],
            biome: vec![0; 8],
            province: vec![0; 8],
        }
    }

    fn config(sides: Vec<Vec<u16>>) -> NativeWarBootstrapConfig {
        NativeWarBootstrapConfig {
            sides,
            hostility: None,
            production: ProductionConfig::default(),
            war_grace_end: 600,
        }
    }

    #[test]
    fn bootstrap_is_deterministic_and_builds_initial_publication() {
        let scenario = scenario();
        let mut first =
            bootstrap_native_war(&scenario, &config(vec![vec![30, 10], vec![20]])).unwrap();
        let second =
            bootstrap_native_war(&scenario, &config(vec![vec![10, 30], vec![20]])).unwrap();
        let a = first.latest_snapshot();
        let b = second.latest_snapshot();
        assert_eq!(a.tick, 0);
        assert_eq!(a.frame, 0);
        assert_eq!(a.frame_snapshot.units, b.frame_snapshot.units);
        assert_eq!(a.frame_snapshot.units[0].id, 1);
        assert_eq!(a.frame_snapshot.units[0].sovereign, 10);
        assert_eq!(a.frame_snapshot.units[0].side, 0);
        let world = WorldGridView::new(
            scenario.target.grid_res,
            scenario.target.width,
            scenario.target.height,
            &scenario.land,
        )
        .unwrap();
        for unit in a.frame_snapshot.units.iter() {
            let cell = world.grid_index(unit.lat, unit.lng).unwrap();
            assert_eq!(
                scenario.world_control[cell],
                u16::try_from(unit.sovereign).unwrap()
            );
        }
        let state = first.checkpoint_state().unwrap();
        assert_eq!(state.economies.len(), 3);
        assert_eq!(state.naval_planning.as_ref().unwrap().side_states.len(), 2);
        assert_eq!(
            state
                .air_power
                .as_ref()
                .unwrap()
                .country_coverage
                .iter()
                .map(|coverage| (coverage.country_id, coverage.operations_coverage))
                .collect::<Vec<_>>(),
            vec![(10, 1.0), (20, 1.0), (30, 1.0)]
        );
        assert_eq!(state.territory_config.maps.land[3], 0);
        assert_eq!(state.territory_config.maps.land[0], 2);
        assert_eq!(state.territory_config.maps.dominant_side[0], 0);
        assert_eq!(state.territory_config.maps.dominant_side[1], 1);
        assert_eq!(state.territory_config.maps.dominant_side[2], 0);
        assert_eq!(state.territory_config.maps.side_influence[0][0], 1.0);
        assert_eq!(state.territory_config.maps.side_influence[1][1], 1.0);
        let influence_runtime = state.influence_runtime.as_ref().unwrap();
        assert!(influence_runtime.regular_queue.is_empty());
        assert_eq!(
            influence_runtime.priority_queue,
            vec![0, 1, 4, 2, 5, 3, 6, 7]
        );
        assert_eq!(
            influence_runtime.queued_cells,
            (0..8).map(|cell| (cell, 2)).collect::<Vec<_>>()
        );
        assert_eq!(state.territory_config.cities.len(), 3);
        let battlefield = state.battlefield.as_ref().unwrap();
        assert_eq!(battlefield.terrain_intensity, vec![0.0; 8]);
        assert!(!battlefield.mountains_enabled);
        assert_eq!(battlefield.countries.len(), 3);
        assert_eq!(battlefield.units.len(), a.frame_snapshot.units.len());
        assert_eq!(battlefield.urban_centers.len(), 3);
        for unit in a.frame_snapshot.units.iter() {
            assert_eq!(
                state
                    .unit_policies
                    .iter()
                    .find(|policy| policy.unit_id == unit.id)
                    .unwrap()
                    .influence
                    .as_ref()
                    .unwrap()
                    .browser_temporal_seed,
                Some(unit.id as f64)
            );
            assert_eq!(
                crate::battlefield::cohesion_group(battlefield.units[&unit.id].cohesion_seed),
                Some((unit.id % 4) as u8)
            );
        }
        assert!(battlefield.units.values().all(|unit| {
            unit.encircled_ticks == 0
                && unit.armor_support_last_tick.is_none()
                && unit.last_ally_count == 1.0
        }));
    }

    #[test]
    fn bootstrap_initializes_reinforcement_allocators_reserves_and_total_personnel() {
        let mut scenario = scenario();
        for country in scenario
            .metadata
            .get_mut("countries")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            let object = country.as_object_mut().unwrap();
            match object["id"].as_u64().unwrap() {
                10 => object.insert("population".into(), json!(1_000_000.0)),
                20 => object.insert("population".into(), json!(2_000_000.0)),
                _ => None,
            };
        }

        let mut runtime =
            bootstrap_native_war(&scenario, &config(vec![vec![10], vec![20]])).unwrap();
        let state = runtime.checkpoint_state().unwrap();
        let reinforcement = state.reinforcement.as_ref().unwrap();

        assert_eq!(reinforcement.next_unit_id, 7);
        assert_eq!(reinforcement.next_air_wing_id, 5);
        assert_eq!(
            reinforcement
                .countries
                .iter()
                .map(|country| (
                    country.country_id,
                    country.fighter_capacity,
                    country.strike_capacity,
                ))
                .collect::<Vec<_>>(),
            vec![(10, 100, 100), (20, 100, 100)]
        );
        assert_eq!(
            state.personnel_reserves,
            BTreeMap::from([(0, 6_800.0), (1, 16_800.0)])
        );
        let dynamics = state.side_dynamics.as_ref().unwrap();
        assert_eq!(dynamics[&0].initial_personnel, 10_000.0);
        assert_eq!(dynamics[&0].current_personnel, 10_000.0);
        assert_eq!(dynamics[&1].initial_personnel, 20_000.0);
        assert_eq!(dynamics[&1].current_personnel, 20_000.0);
    }

    #[test]
    fn bootstrap_projects_explicit_mountain_metadata_and_country_primitives() {
        let mut scenario = scenario();
        scenario.source = GridSpec::world(45.0).unwrap();
        let metadata = scenario.metadata.as_object_mut().unwrap();
        metadata.insert(
            "mountainData".into(),
            json!([[0, 0.25], [1, 0.75], [10, 0.5]]),
        );
        metadata
            .get_mut("countries")
            .unwrap()
            .as_array_mut()
            .unwrap()[1] = json!({
            "id": 10,
            "name": "Alpha",
            "gdp": 100.0,
            "population": 10_000.0,
            "buffState": "buff",
            "hiddenBuffState": "super",
            "attackBuffPercent": 12.5,
            "defenseBuffPercent": 8.0,
            "aiSpeedMultiplier": 1.25
        });

        let mut runtime =
            bootstrap_native_war(&scenario, &config(vec![vec![10], vec![20]])).unwrap();
        let state = runtime.checkpoint_state().unwrap();
        let battlefield = state.battlefield.unwrap();

        assert!(battlefield.mountains_enabled);
        // Source cells 0 and 1 both project into target cell zero, and max wins.
        assert_eq!(
            battlefield.terrain_intensity[0].to_bits(),
            0.75_f32.to_bits()
        );
        assert_eq!(
            battlefield.terrain_intensity[1].to_bits(),
            0.5_f32.to_bits()
        );
        assert!(
            battlefield.terrain_intensity[2..]
                .iter()
                .all(|value| *value == 0.0)
        );
        let alpha = battlefield.countries[&10];
        assert_eq!(alpha.combat_buff, BattlefieldBuff::Super);
        assert_eq!(alpha.influence_buff, BattlefieldBuff::Buff);
        assert_eq!(alpha.attack_buff_percent, 12.5);
        assert_eq!(alpha.defense_buff_percent, 8.0);
        assert_eq!(alpha.ai_speed_multiplier, 1.25);
    }

    #[test]
    fn bootstrap_rejects_invalid_mountain_metadata() {
        let mut scenario = scenario();
        scenario
            .metadata
            .as_object_mut()
            .unwrap()
            .insert("mountainData".into(), json!([[8, 0.5]]));

        assert!(matches!(
            bootstrap_native_war(&scenario, &config(vec![vec![10], vec![20]])),
            Err(NativeWarBootstrapError::BattlefieldMetadata(_))
        ));
    }

    #[test]
    fn default_and_directed_hostility_are_applied() {
        let scenario = scenario();
        let mut runtime =
            bootstrap_native_war(&scenario, &config(vec![vec![10], vec![20], vec![30]])).unwrap();
        let state = runtime.checkpoint_state().unwrap();
        assert_eq!(state.diplomacy.hostility, vec![0, 1, 1, 1, 0, 1, 1, 1, 0]);
        assert_eq!(state.territory_config.maps.occupation[0], 1.0);
        assert_eq!(state.territory_config.maps.occupation[1], -1.0);
        assert_eq!(state.territory_config.maps.occupation[2], 1.0);
        let mut directed = config(vec![vec![10], vec![20], vec![30]]);
        directed.hostility = Some(vec![0, 1, 0, 0, 0, 0, 0, 0, 0]);
        let mut runtime = bootstrap_native_war(&scenario, &directed).unwrap();
        assert_eq!(
            runtime.checkpoint_state().unwrap().diplomacy.hostility,
            vec![0, 1, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn rejects_invalid_topology() {
        let scenario = scenario();
        for sides in [
            vec![vec![10, 10], vec![20]],
            vec![vec![10], vec![99]],
            vec![vec![10]],
            vec![vec![10], vec![20], vec![30]],
        ] {
            let mut c = config(sides);
            if c.sides.len() == 3 {
                c.hostility = Some(vec![0; 9]);
            }
            assert!(bootstrap_native_war(&scenario, &c).is_err());
        }

        let mut self_hostile = config(vec![vec![10], vec![20]]);
        self_hostile.hostility = Some(vec![1, 1, 1, 0]);
        assert!(bootstrap_native_war(&scenario, &self_hostile).is_err());
    }
}
