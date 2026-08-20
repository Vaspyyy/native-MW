//! Deterministic construction of a playable native war from an MWSC scenario.

use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

use crate::{
    combat::{CombatUnit, UnitKind},
    economy::{
        EconomyState, PAYROLL_PER_UNIT, STARTING_RESERVE_CYCLES, TARGET_STARTING_PAYROLL_SHARE,
    },
    production::{ProductionConfig, derive_scenario_production},
    runtime::{
        NativeRuntime, RuntimeCheckpoint, RuntimeConfig, RuntimeDiplomacy, RuntimeUnitPolicy,
    },
    scenario::DecodedScenario,
    simulation::{Simulation, SimulationConfig, SimulationUnit},
    strategic::StrategicSimulation,
    territory::{TerritoryCity, TerritoryConfig, TerritoryControl, TerritoryMaps},
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
    #[error("strategic: {0}")]
    Strategic(#[from] crate::strategic::StrategicError),
    #[error("runtime: {0}")]
    Runtime(#[from] crate::runtime::RuntimeError),
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
    let territory = TerritoryControl::new(TerritoryConfig {
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
            p.influence.as_mut().unwrap().owner_ally_country_ids = topology
                [country_to_side[&country]]
                .iter()
                .copied()
                .collect();
            p
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
            objectives: Vec::new(),
            prior_objective_by_unit: BTreeMap::new(),
            front_prior_by_unit: BTreeMap::new(),
            last_front_refresh_tick: None,
            casualties: BTreeMap::new(),
            casualties_by_victim: BTreeMap::new(),
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
        assert_eq!(state.territory_config.maps.land[3], 0);
        assert_eq!(state.territory_config.maps.land[0], 2);
        assert_eq!(state.territory_config.maps.dominant_side[0], 0);
        assert_eq!(state.territory_config.maps.dominant_side[1], 1);
        assert_eq!(state.territory_config.maps.dominant_side[2], 0);
        assert_eq!(state.territory_config.maps.side_influence[0][0], 1.0);
        assert_eq!(state.territory_config.maps.side_influence[1][1], 1.0);
        assert_eq!(state.territory_config.cities.len(), 3);
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
