//! Production-facing adapters from decoded scenarios and immutable simulation
//! snapshots into the strategic kernels.
//!
//! The adapters in this module deliberately do not invent formations. Scenario
//! metadata establishes economic and territorial baselines; live unit snapshots
//! are the only source of payroll, formation counts, and occupation garrisons.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use serde_json::{Map, Value};
use thiserror::Error;

use crate::{
    combat::{PERSONNEL_PER_FORMATION, UnitKind},
    economy::{EconomyError, EconomySeed, EconomyState, create_economy_state},
    occupation::OccupationState,
    scenario::{DecodedScenario, GridSpec},
    simulation::{FrameSnapshot, NATIVE_TICK_SCHEMA_VERSION},
    strategic::{CountryCycleInput, OccupationCycleRecord, StrategicCycleInput},
    territory::{CountryAggregate, TERRITORY_SCHEMA_VERSION, TerritorySnapshot},
};

pub const PRODUCTION_SCHEMA_VERSION: &str = "production-input-v1";
pub const DEFAULT_UNIT_DENSITY_FACTOR: f64 = 0.066;
pub const DEFAULT_MAX_UNITS_PER_SIDE: u32 = 2_400;
pub const DEFAULT_MIN_EXPECTED_ARMY_UNITS: u32 = 3;
pub const DEFAULT_DENSITY_REFERENCE_CELLS: f64 = 1_500.0;
pub const DEFAULT_DENSITY_EXPONENT: f64 = 0.45;
pub const ARMOR_PAYROLL_PER_100: f64 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProductionConfig {
    pub unit_density_factor: f64,
    pub max_units_per_side: u32,
    pub min_expected_army_units: u32,
    pub density_reference_cells: f64,
    pub density_exponent: f64,
    pub max_countries: usize,
    pub max_cities: usize,
    pub max_units: usize,
    pub max_sides: usize,
}

impl Default for ProductionConfig {
    fn default() -> Self {
        Self {
            unit_density_factor: DEFAULT_UNIT_DENSITY_FACTOR,
            max_units_per_side: DEFAULT_MAX_UNITS_PER_SIDE,
            min_expected_army_units: DEFAULT_MIN_EXPECTED_ARMY_UNITS,
            density_reference_cells: DEFAULT_DENSITY_REFERENCE_CELLS,
            density_exponent: DEFAULT_DENSITY_EXPONENT,
            max_countries: u16::MAX as usize,
            max_cities: 1_000_000,
            max_units: 1_000_000,
            max_sides: 4_096,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProductionCountry {
    pub country_id: u16,
    pub name: String,
    pub gdp: f64,
    pub population: f64,
    pub is_rebel: bool,
    pub initial_core_cells: u32,
    pub initial_owned_land_cells: u32,
    pub initial_city_population: f64,
    pub capital_cell: Option<usize>,
    pub expected_army_units: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProductionCity {
    pub city_id: u64,
    pub name: String,
    /// Zero means the city could not be attributed at its exact scenario cell.
    pub owner_id: u16,
    pub cell: usize,
    pub lat: f64,
    pub lng: f64,
    pub population: f64,
    pub capital: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScenarioProductionCounters {
    pub countries: usize,
    pub cities: usize,
    pub unresolved_city_owners: usize,
    pub economy_seeds: usize,
    pub land_cells: usize,
}

/// Compact, owned scenario baselines. Dense maps remain owned by the runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct ScenarioProduction {
    pub schema_version: &'static str,
    pub grid: GridSpec,
    pub countries: Arc<[ProductionCountry]>,
    pub cities: Arc<[ProductionCity]>,
    pub economy_seeds: Arc<[EconomySeed]>,
    pub economy_states: Arc<[EconomyState]>,
    pub counters: ScenarioProductionCounters,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerritoryCommitMarker {
    pub generation: u64,
    pub commit_sequence: u64,
    pub topology_revision: u64,
    pub world_revision: u64,
    pub city_revision: u64,
}

impl From<&TerritorySnapshot> for TerritoryCommitMarker {
    fn from(snapshot: &TerritorySnapshot) -> Self {
        Self {
            generation: snapshot.generation,
            commit_sequence: snapshot.commit_sequence,
            topology_revision: snapshot.topology_revision,
            world_revision: snapshot.world_revision,
            city_revision: snapshot.city_revision,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StrategicDerivationInput<'a> {
    pub tick: u64,
    pub force: bool,
    pub scenario: &'a ScenarioProduction,
    pub grid: GridSpec,
    pub de_jure: &'a [u16],
    pub territory: &'a TerritorySnapshot,
    pub expected_territory: TerritoryCommitMarker,
    pub territory_fresh: bool,
    pub frame: &'a FrameSnapshot,
    pub country_to_side: &'a BTreeMap<u16, usize>,
    pub side_count: usize,
    pub hostility_matrix: &'a [u8],
    pub economies: &'a BTreeMap<u16, EconomyState>,
    pub occupations: &'a BTreeMap<u16, OccupationState>,
    pub casualties: &'a BTreeMap<u16, f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StrategicDerivationCounters {
    pub countries: usize,
    pub units: usize,
    pub payroll_units: usize,
    pub garrison_units: usize,
    pub occupations: usize,
    pub active_sides: usize,
    pub hostile_pairs: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StrategicDerivationOutput {
    pub input: StrategicCycleInput,
    pub counters: StrategicDerivationCounters,
}

#[derive(Debug, Error, PartialEq)]
pub enum ProductionError {
    #[error("invalid production configuration")]
    InvalidConfig,
    #[error("scenario grid or dense map lengths are invalid")]
    InvalidScenarioGrid,
    #[error("scenario metadata has no country array")]
    MissingCountries,
    #[error("scenario contains too many {kind}: {actual}, limit {limit}")]
    Limit {
        kind: &'static str,
        actual: usize,
        limit: usize,
    },
    #[error("country metadata at index {index} is invalid: {reason}")]
    InvalidCountry { index: usize, reason: &'static str },
    #[error("duplicate country id {0}")]
    DuplicateCountry(u16),
    #[error("scenario dense maps reference unknown country id {0}")]
    UnknownMapCountry(u16),
    #[error("city metadata at index {index} is invalid: {reason}")]
    InvalidCity { index: usize, reason: &'static str },
    #[error("duplicate or colliding city id {0}")]
    DuplicateCity(u64),
    #[error("city {city_id} references unknown country id {country_id}")]
    UnknownCityCountry { city_id: u64, country_id: u16 },
    #[error("economy seed: {0}")]
    Economy(#[from] EconomyError),
    #[error("strategic grid or de-jure map is invalid")]
    InvalidStrategicGrid,
    #[error("territory snapshot is stale or does not match the required commit")]
    StaleTerritory,
    #[error("territory snapshot is partial or uses an incompatible schema")]
    PartialTerritory,
    #[error("frame snapshot does not match the strategic tick or schema")]
    StaleFrame,
    #[error("duplicate territory aggregate for country id {0}")]
    DuplicateCountryAggregate(u16),
    #[error("duplicate territory aggregate for side {0}")]
    DuplicateSideAggregate(usize),
    #[error("missing territory aggregate for country id {0}")]
    MissingCountryAggregate(u16),
    #[error("missing territory aggregate for side {0}")]
    MissingSideAggregate(usize),
    #[error("invalid side mapping for country id {0}")]
    InvalidCountrySide(u16),
    #[error("missing economy state for country id {0}")]
    MissingEconomy(u16),
    #[error("invalid hostility matrix")]
    InvalidHostility,
    #[error("duplicate unit id {0}")]
    DuplicateUnit(u64),
    #[error("unit {0} has invalid identity, side, or numeric state")]
    InvalidUnit(u64),
    #[error("occupation state for victim {0} is invalid")]
    InvalidOccupation(u16),
    #[error("casualty state for country {0} is invalid")]
    InvalidCasualties(u16),
    #[error("strategic aggregate exceeds its u32 wire range")]
    AggregateOverflow,
    #[error("derived strategic numeric state is non-finite")]
    NonFinite,
}

/// Browser-compatible area model used when establishing an army baseline.
pub fn estimate_territory_army_units(
    cell_count: u64,
    config: &ProductionConfig,
) -> Result<f64, ProductionError> {
    validate_config(config)?;
    let cells = cell_count.max(1) as f64;
    let size_factor = (cells / config.density_reference_cells).max(1.0);
    let density_scale = 1.0 / size_factor.powf(config.density_exponent);
    let estimate = (cells * config.unit_density_factor * density_scale).floor();
    if !estimate.is_finite() {
        return Err(ProductionError::NonFinite);
    }
    Ok(estimate
        .max(f64::from(config.min_expected_army_units))
        .min(f64::from(config.max_units_per_side)))
}

/// Normalize scenario metadata and construct deterministic economy baselines.
pub fn derive_scenario_production(
    scenario: &DecodedScenario,
    config: &ProductionConfig,
) -> Result<ScenarioProduction, ProductionError> {
    validate_config(config)?;
    let cells = checked_grid(scenario.target)?;
    if scenario.world_control.len() != cells
        || scenario.de_jure.len() != cells
        || scenario.land.len() != cells
    {
        return Err(ProductionError::InvalidScenarioGrid);
    }

    let raw_countries =
        country_array(&scenario.metadata).ok_or(ProductionError::MissingCountries)?;
    enforce_limit("countries", raw_countries.len(), config.max_countries)?;
    let mut countries = Vec::with_capacity(raw_countries.len());
    let mut country_ids = BTreeSet::new();
    for (index, raw) in raw_countries.iter().enumerate() {
        let object = raw.as_object().ok_or(ProductionError::InvalidCountry {
            index,
            reason: "entry must be an object",
        })?;
        // Zero is the dense-map sentinel and some scenarios retain a matching
        // "Empty Land" metadata row. It is not an economy-bearing country.
        if object.get("id").and_then(Value::as_u64) == Some(0) {
            continue;
        }
        let country_id = required_country_id(object, "id")
            .map_err(|reason| ProductionError::InvalidCountry { index, reason })?;
        if !country_ids.insert(country_id) {
            return Err(ProductionError::DuplicateCountry(country_id));
        }
        let gdp = optional_nonnegative_number(object, &["gdp"])
            .map_err(|reason| ProductionError::InvalidCountry { index, reason })?
            .unwrap_or(0.0);
        let population = optional_nonnegative_number(object, &["pop", "population"])
            .map_err(|reason| ProductionError::InvalidCountry { index, reason })?
            .unwrap_or(0.0);
        let name = optional_string(object, "name")
            .map_err(|reason| ProductionError::InvalidCountry { index, reason })?
            .unwrap_or_else(|| format!("Country {country_id}"));
        countries.push(ProductionCountry {
            country_id,
            name,
            gdp,
            population,
            is_rebel: object
                .get("isRebel")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            initial_core_cells: 0,
            initial_owned_land_cells: 0,
            initial_city_population: 0.0,
            capital_cell: None,
            expected_army_units: 0.0,
        });
    }
    countries.sort_unstable_by_key(|country| country.country_id);
    let index_by_country = countries
        .iter()
        .enumerate()
        .map(|(index, country)| (country.country_id, index))
        .collect::<BTreeMap<_, _>>();

    let mut land_cells = 0_usize;
    for cell in 0..cells {
        if scenario.land[cell] == 0 {
            continue;
        }
        land_cells += 1;
        let de_jure = scenario.de_jure[cell];
        if de_jure > 0 && !index_by_country.contains_key(&de_jure) {
            return Err(ProductionError::UnknownMapCountry(de_jure));
        }
        let owner = scenario.world_control[cell];
        if owner > 0 {
            let index = *index_by_country
                .get(&owner)
                .ok_or(ProductionError::UnknownMapCountry(owner))?;
            // Browser startWar records country.initialCells from the current
            // world-control theater, not from the de-jure map. Economy,
            // capitulation, and area-based unit baselines all use that value.
            countries[index].initial_core_cells = countries[index]
                .initial_core_cells
                .checked_add(1)
                .ok_or(ProductionError::AggregateOverflow)?;
            countries[index].initial_owned_land_cells = countries[index]
                .initial_owned_land_cells
                .checked_add(1)
                .ok_or(ProductionError::AggregateOverflow)?;
        }
    }

    let raw_cities = scenario
        .metadata
        .get("cities")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    enforce_limit("cities", raw_cities.len(), config.max_cities)?;
    let mut cities = Vec::with_capacity(raw_cities.len());
    let mut city_ids = BTreeSet::new();
    let mut unresolved_city_owners = 0;
    for (index, raw) in raw_cities.iter().enumerate() {
        let object = raw.as_object().ok_or(ProductionError::InvalidCity {
            index,
            reason: "entry must be an object",
        })?;
        let city_id = normalized_city_id(object.get("id"), index)?;
        if !city_ids.insert(city_id) {
            return Err(ProductionError::DuplicateCity(city_id));
        }
        let lat = required_number(object, "lat")
            .map_err(|reason| ProductionError::InvalidCity { index, reason })?;
        let lng = required_number(object, "lng")
            .map_err(|reason| ProductionError::InvalidCity { index, reason })?;
        let cell = grid_index(scenario.target, lat, lng).ok_or(ProductionError::InvalidCity {
            index,
            reason: "coordinates are outside the scenario grid",
        })?;
        let population = optional_nonnegative_number(object, &["pop", "population"])
            .map_err(|reason| ProductionError::InvalidCity { index, reason })?
            .unwrap_or(0.0);
        let owner_id = optional_country_id(object, &["ownerId", "sovereignId"])
            .map_err(|reason| ProductionError::InvalidCity { index, reason })?
            // startWar assigns active-theater city sovereignty from the
            // current ownership grid before economy initialization.
            .unwrap_or(scenario.world_control[cell]);
        if owner_id > 0 && !index_by_country.contains_key(&owner_id) {
            return Err(ProductionError::UnknownCityCountry {
                city_id,
                country_id: owner_id,
            });
        }
        if owner_id == 0 {
            unresolved_city_owners += 1;
        }
        let name = optional_string(object, "name")
            .map_err(|reason| ProductionError::InvalidCity { index, reason })?
            .unwrap_or_else(|| format!("City {city_id}"));
        cities.push(ProductionCity {
            city_id,
            name,
            owner_id,
            cell,
            lat,
            lng: wrap_longitude(lng),
            population,
            capital: object
                .get("isCapital")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    cities.sort_by_key(|city| (city.cell, city.city_id));
    for city in &cities {
        if city.owner_id == 0 {
            continue;
        }
        let country = &mut countries[index_by_country[&city.owner_id]];
        country.initial_city_population += city.population;
        if city.capital && country.capital_cell.is_none() {
            country.capital_cell = Some(city.cell);
        }
    }

    let mut economy_seeds = Vec::with_capacity(countries.len());
    let mut economy_states = Vec::with_capacity(countries.len());
    for country in &mut countries {
        if !country.initial_city_population.is_finite() {
            return Err(ProductionError::NonFinite);
        }
        country.expected_army_units =
            estimate_territory_army_units(u64::from(country.initial_core_cells), config)?;
        let seed = EconomySeed {
            country_id: country.country_id,
            gdp: country.gdp,
            population: country.population,
            territory_units: country.expected_army_units,
            initial_core_cells: country.initial_core_cells,
            initial_city_population: country.initial_city_population,
        };
        economy_states.push(create_economy_state(seed)?);
        economy_seeds.push(seed);
    }

    let counters = ScenarioProductionCounters {
        countries: countries.len(),
        cities: cities.len(),
        unresolved_city_owners,
        economy_seeds: economy_seeds.len(),
        land_cells,
    };
    Ok(ScenarioProduction {
        schema_version: PRODUCTION_SCHEMA_VERSION,
        grid: scenario.target,
        countries: countries.into(),
        cities: cities.into(),
        economy_seeds: economy_seeds.into(),
        economy_states: economy_states.into(),
        counters,
    })
}

/// Derive one coherent strategic-cycle input from immutable committed views.
pub fn derive_strategic_cycle_input(
    source: StrategicDerivationInput<'_>,
    config: &ProductionConfig,
) -> Result<StrategicDerivationOutput, ProductionError> {
    validate_config(config)?;
    validate_strategic_boundary(&source, config)?;

    let countries_by_id = source
        .scenario
        .countries
        .iter()
        .map(|country| (country.country_id, country))
        .collect::<BTreeMap<_, _>>();
    let aggregates = country_aggregates(source.territory)?;
    let side_aggregates = side_aggregates(source.territory)?;

    let mut active_sides = BTreeSet::new();
    for (&country_id, &side) in source.country_to_side {
        if country_id == 0
            || side >= source.side_count
            || side > u16::MAX as usize
            || !countries_by_id.contains_key(&country_id)
        {
            return Err(ProductionError::InvalidCountrySide(country_id));
        }
        let economy = source
            .economies
            .get(&country_id)
            .ok_or(ProductionError::MissingEconomy(country_id))?;
        validate_economy(economy)?;
        let aggregate = aggregates
            .get(&country_id)
            .copied()
            .ok_or(ProductionError::MissingCountryAggregate(country_id))?;
        if aggregate.side_index != side as i16 {
            return Err(ProductionError::InvalidCountrySide(country_id));
        }
        if !side_aggregates.contains_key(&side) {
            return Err(ProductionError::MissingSideAggregate(side));
        }
        if !economy.capitulated {
            active_sides.insert(side as u16);
        }
    }

    for (&country_id, casualties) in source.casualties {
        if !casualties.is_finite()
            || *casualties < 0.0
            || !countries_by_id.contains_key(&country_id)
        {
            return Err(ProductionError::InvalidCasualties(country_id));
        }
    }

    let mut payroll = BTreeMap::<u16, f64>::new();
    let mut unit_counts = BTreeMap::<u16, u32>::new();
    let mut garrisons = BTreeMap::<u16, f64>::new();
    let mut unit_ids = BTreeSet::new();
    let mut counters = StrategicDerivationCounters::default();
    enforce_limit("units", source.frame.units.len(), config.max_units)?;
    for unit in source.frame.units.iter() {
        counters.units += 1;
        if !unit_ids.insert(unit.id) {
            return Err(ProductionError::DuplicateUnit(unit.id));
        }
        validate_unit(unit, source.country_to_side, source.side_count)?;
        let country_id =
            u16::try_from(unit.sovereign).map_err(|_| ProductionError::InvalidUnit(unit.id))?;
        if unit.health <= 0.0 {
            continue;
        }
        let due = match unit.kind {
            UnitKind::Armor => unit.equipment as f64 / 100.0 * ARMOR_PAYROLL_PER_100,
            UnitKind::Army => unit.personnel_capacity as f64 / PERSONNEL_PER_FORMATION,
        };
        if !due.is_finite() {
            return Err(ProductionError::NonFinite);
        }
        *payroll.entry(country_id).or_default() += due;
        counters.payroll_units += 1;
        let counts_for_capitulation = match unit.kind {
            UnitKind::Army => true,
            UnitKind::Armor => unit.equipment > 0,
        };
        if counts_for_capitulation {
            let count = unit_counts.entry(country_id).or_default();
            *count = count
                .checked_add(1)
                .ok_or(ProductionError::AggregateOverflow)?;
        }

        if unit.kind != UnitKind::Army {
            continue;
        }
        let Some(cell) = grid_index(source.grid, unit.lat, unit.lng) else {
            continue;
        };
        let victim_id = source.de_jure[cell];
        let Some(occupation) = source.occupations.get(&victim_id) else {
            continue;
        };
        let annexer_side = *source
            .country_to_side
            .get(&occupation.annexer_id)
            .ok_or(ProductionError::InvalidOccupation(victim_id))?;
        if usize::from(unit.side) != annexer_side {
            continue;
        }
        let strength = unit.personnel as f64 / PERSONNEL_PER_FORMATION;
        if !strength.is_finite() {
            return Err(ProductionError::NonFinite);
        }
        *garrisons.entry(victim_id).or_default() += strength;
        counters.garrison_units += 1;
    }

    let mut countries = Vec::with_capacity(source.country_to_side.len());
    for (&country_id, &side) in source.country_to_side {
        let scenario_country = countries_by_id[&country_id];
        let economy = &source.economies[&country_id];
        let aggregate = aggregates[&country_id];
        let country = CountryCycleInput {
            country_id,
            side: side as u16,
            owned_cells: u32_count(aggregate.owned)?,
            controlled_cells: u32_count(aggregate.controlled)?,
            core_controlled: u32_count(aggregate.core_controlled)?,
            initial_cells: economy.initial_core_cells,
            city_population_controlled: aggregate.city_population_controlled,
            unit_count: unit_counts.get(&country_id).copied().unwrap_or(0),
            payroll_due: payroll.get(&country_id).copied().unwrap_or(0.0),
            capital_held: aggregate.capital_held,
            is_rebel: scenario_country.is_rebel,
            active: !economy.capitulated,
        };
        if !country.city_population_controlled.is_finite()
            || country.city_population_controlled < 0.0
            || !country.payroll_due.is_finite()
            || country.payroll_due < 0.0
        {
            return Err(ProductionError::NonFinite);
        }
        countries.push(country);
    }
    countries.sort_unstable_by_key(|country| (country.side, country.country_id));
    counters.countries = countries.len();

    let mut occupations = Vec::with_capacity(source.occupations.len());
    for (&victim_id, state) in source.occupations {
        validate_occupation(state, victim_id, &countries_by_id, source.economies)?;
        let aggregate = aggregates
            .get(&victim_id)
            .copied()
            .ok_or(ProductionError::MissingCountryAggregate(victim_id))?;
        let annexer_side = *source
            .country_to_side
            .get(&state.annexer_id)
            .ok_or(ProductionError::InvalidOccupation(victim_id))?;
        let held = aggregate
            .de_jure_control_by_side
            .get(&annexer_side)
            .copied()
            .unwrap_or(0);
        if held > u64::from(state.core_cells.max(1)) {
            return Err(ProductionError::InvalidOccupation(victim_id));
        }
        let casualties = source.casualties.get(&victim_id).copied().unwrap_or(0.0);
        let denominator = (state.expected_army_units * PERSONNEL_PER_FORMATION).max(1.0);
        let casualty_pressure = (casualties / denominator).clamp(0.0, 1.0);
        let record = OccupationCycleRecord {
            victim_id,
            held_cells: u32_count(held)?,
            garrison_strength: garrisons.get(&victim_id).copied().unwrap_or(0.0),
            casualty_pressure,
        };
        if !record.garrison_strength.is_finite() || !record.casualty_pressure.is_finite() {
            return Err(ProductionError::NonFinite);
        }
        occupations.push(record);
    }
    counters.occupations = occupations.len();

    let active_sides = active_sides.into_iter().collect::<Vec<_>>();
    counters.active_sides = active_sides.len();
    let capitulation_active_sides = active_sides
        .iter()
        .copied()
        .filter(|&left| {
            active_sides.iter().copied().any(|right| {
                left != right
                    && source.hostility_matrix
                        [usize::from(left) * source.side_count + usize::from(right)]
                        == 1
            })
        })
        .collect::<Vec<_>>();
    let mut active_hostile_pairs = Vec::new();
    for (position, &left) in active_sides.iter().enumerate() {
        for &right in &active_sides[position + 1..] {
            let left_index = usize::from(left);
            let right_index = usize::from(right);
            if source.hostility_matrix[left_index * source.side_count + right_index] == 1
                || source.hostility_matrix[right_index * source.side_count + left_index] == 1
            {
                active_hostile_pairs.push((left, right));
            }
        }
    }
    counters.hostile_pairs = active_hostile_pairs.len();

    Ok(StrategicDerivationOutput {
        input: StrategicCycleInput {
            tick: source.tick,
            force: source.force,
            territory_generation: source.territory.generation,
            territory_commit_sequence: source.territory.commit_sequence,
            territory_fresh: true,
            countries,
            occupations,
            active_sides,
            active_hostile_pairs,
            capitulation_active_sides: Some(capitulation_active_sides),
        },
        counters,
    })
}

fn validate_config(config: &ProductionConfig) -> Result<(), ProductionError> {
    if !config.unit_density_factor.is_finite()
        || config.unit_density_factor < 0.0
        || config.max_units_per_side == 0
        || config.min_expected_army_units == 0
        || config.min_expected_army_units > config.max_units_per_side
        || !config.density_reference_cells.is_finite()
        || config.density_reference_cells <= 0.0
        || !config.density_exponent.is_finite()
        || config.density_exponent < 0.0
        || config.max_countries == 0
        || config.max_countries > u16::MAX as usize
        || config.max_cities == 0
        || config.max_units == 0
        || config.max_sides == 0
        || config.max_sides > u16::MAX as usize + 1
    {
        return Err(ProductionError::InvalidConfig);
    }
    Ok(())
}

fn validate_strategic_boundary(
    source: &StrategicDerivationInput<'_>,
    config: &ProductionConfig,
) -> Result<(), ProductionError> {
    let cells = checked_grid(source.grid)?;
    if source.grid != source.scenario.grid || source.de_jure.len() != cells {
        return Err(ProductionError::InvalidStrategicGrid);
    }
    if !source.territory_fresh
        || TerritoryCommitMarker::from(source.territory) != source.expected_territory
    {
        return Err(ProductionError::StaleTerritory);
    }
    if source.territory.schema_version != TERRITORY_SCHEMA_VERSION {
        return Err(ProductionError::PartialTerritory);
    }
    if source.frame.schema_version != NATIVE_TICK_SCHEMA_VERSION || source.frame.tick != source.tick
    {
        return Err(ProductionError::StaleFrame);
    }
    if source.side_count == 0 || source.side_count > config.max_sides {
        return Err(ProductionError::InvalidHostility);
    }
    let matrix_len = source
        .side_count
        .checked_mul(source.side_count)
        .ok_or(ProductionError::InvalidHostility)?;
    if source.hostility_matrix.len() != matrix_len {
        return Err(ProductionError::InvalidHostility);
    }
    for left in 0..source.side_count {
        for right in 0..source.side_count {
            let value = source.hostility_matrix[left * source.side_count + right];
            if value > 1 || (left == right && value != 0) {
                return Err(ProductionError::InvalidHostility);
            }
        }
    }
    enforce_limit(
        "countries",
        source.country_to_side.len(),
        config.max_countries,
    )?;
    Ok(())
}

fn validate_economy(state: &EconomyState) -> Result<(), ProductionError> {
    let numbers = [
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
    ];
    if state.country_id == 0
        || state.initial_core_cells == 0
        || numbers.iter().any(|value| !value.is_finite())
    {
        return Err(ProductionError::MissingEconomy(state.country_id));
    }
    Ok(())
}

fn validate_occupation(
    state: &OccupationState,
    key: u16,
    countries: &BTreeMap<u16, &ProductionCountry>,
    economies: &BTreeMap<u16, EconomyState>,
) -> Result<(), ProductionError> {
    let numbers = [
        state.base_income,
        state.expected_army_units,
        state.resistance,
        state.occupation_coverage,
        state.garrison_coverage,
        state.garrison_assigned,
        state.held_ratio,
    ];
    if key == 0
        || state.victim_id != key
        || state.annexer_id == 0
        || state.annexer_id == key
        || state.core_cells == 0
        || numbers.iter().any(|value| !value.is_finite())
        || state.expected_army_units < 0.0
        || !countries.contains_key(&key)
        || !countries.contains_key(&state.annexer_id)
        || !economies.contains_key(&key)
        || !economies.contains_key(&state.annexer_id)
    {
        return Err(ProductionError::InvalidOccupation(key));
    }
    Ok(())
}

fn validate_unit(
    unit: &crate::simulation::UnitSnapshot,
    mapping: &BTreeMap<u16, usize>,
    side_count: usize,
) -> Result<(), ProductionError> {
    let country_id = u16::try_from(unit.sovereign)
        .ok()
        .filter(|country_id| *country_id > 0)
        .ok_or(ProductionError::InvalidUnit(unit.id))?;
    let expected_side = mapping
        .get(&country_id)
        .copied()
        .ok_or(ProductionError::InvalidUnit(unit.id))?;
    let side = usize::from(unit.side);
    let numbers = [
        unit.lat,
        unit.lng,
        unit.health,
        unit.max_health,
        f64::from(unit.health_fraction),
        unit.dir_lat,
        unit.dir_lng,
    ];
    if side >= side_count
        || side != expected_side
        || numbers.iter().any(|value| !value.is_finite())
        || unit.health < 0.0
        || unit.max_health <= 0.0
        || !(0.0..=1.0).contains(&unit.health_fraction)
    {
        return Err(ProductionError::InvalidUnit(unit.id));
    }
    Ok(())
}

fn country_aggregates(
    territory: &TerritorySnapshot,
) -> Result<BTreeMap<u16, &CountryAggregate>, ProductionError> {
    let mut output = BTreeMap::new();
    for aggregate in &territory.countries {
        if aggregate.country_id == 0
            || !aggregate.core_control_ratio.is_finite()
            || !aggregate.city_population_total.is_finite()
            || !aggregate.city_population_controlled.is_finite()
            || aggregate.city_population_total < 0.0
            || aggregate.city_population_controlled < 0.0
        {
            return Err(ProductionError::MissingCountryAggregate(
                aggregate.country_id,
            ));
        }
        if output.insert(aggregate.country_id, aggregate).is_some() {
            return Err(ProductionError::DuplicateCountryAggregate(
                aggregate.country_id,
            ));
        }
    }
    Ok(output)
}

fn side_aggregates(
    territory: &TerritorySnapshot,
) -> Result<BTreeMap<usize, &crate::territory::SideAggregate>, ProductionError> {
    let mut output = BTreeMap::new();
    for aggregate in &territory.sides {
        if !aggregate.city_population_controlled.is_finite()
            || aggregate.city_population_controlled < 0.0
        {
            return Err(ProductionError::MissingSideAggregate(aggregate.side_index));
        }
        if output.insert(aggregate.side_index, aggregate).is_some() {
            return Err(ProductionError::DuplicateSideAggregate(
                aggregate.side_index,
            ));
        }
    }
    Ok(output)
}

fn checked_grid(grid: GridSpec) -> Result<usize, ProductionError> {
    if grid.width == 0 || grid.height == 0 || !grid.grid_res.is_finite() || grid.grid_res <= 0.0 {
        return Err(ProductionError::InvalidScenarioGrid);
    }
    grid.width
        .checked_mul(grid.height)
        .ok_or(ProductionError::InvalidScenarioGrid)
}

fn country_array(metadata: &Value) -> Option<&[Value]> {
    metadata
        .get("metadata")
        .and_then(Value::as_array)
        .or_else(|| metadata.get("countries").and_then(Value::as_array))
        .or_else(|| metadata.as_array())
        .map(Vec::as_slice)
}

fn required_country_id(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<u16, &'static str> {
    let id = object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|id| u16::try_from(id).ok())
        .filter(|id| *id > 0)
        .ok_or("country id must be a positive u16")?;
    Ok(id)
}

fn optional_country_id(
    object: &Map<String, Value>,
    keys: &[&str],
) -> Result<Option<u16>, &'static str> {
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let id = value
            .as_u64()
            .and_then(|id| u16::try_from(id).ok())
            .filter(|id| *id > 0)
            .ok_or("city owner must be a positive u16")?;
        return Ok(Some(id));
    }
    Ok(None)
}

fn required_number(object: &Map<String, Value>, key: &'static str) -> Result<f64, &'static str> {
    let value = object
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or("required number is missing or non-finite")?;
    Ok(value)
}

fn optional_nonnegative_number(
    object: &Map<String, Value>,
    keys: &[&str],
) -> Result<Option<f64>, &'static str> {
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let number = value
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0)
            .ok_or("numeric value must be finite and non-negative")?;
        return Ok(Some(number));
    }
    Ok(None)
}

fn optional_string(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<Option<String>, &'static str> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err("text value must be a string"),
    }
}

fn normalized_city_id(value: Option<&Value>, index: usize) -> Result<u64, ProductionError> {
    match value {
        Some(Value::Number(number)) => {
            number
                .as_u64()
                .filter(|id| *id > 0)
                .ok_or(ProductionError::InvalidCity {
                    index,
                    reason: "city id must be a positive integer",
                })
        }
        Some(Value::String(value)) if !value.is_empty() => {
            Ok(stable_nonzero_hash(&format!("city:string:{value}")))
        }
        None | Some(Value::Null) => Ok(stable_nonzero_hash(&format!("city:index:{index}"))),
        _ => Err(ProductionError::InvalidCity {
            index,
            reason: "city id must be an integer or string",
        }),
    }
}

fn stable_nonzero_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash.max(1)
}

fn wrap_longitude(lng: f64) -> f64 {
    ((((lng + 180.0) % 360.0) + 360.0) % 360.0) - 180.0
}

fn grid_index(grid: GridSpec, lat: f64, lng: f64) -> Option<usize> {
    if grid.width == 0
        || grid.height == 0
        || !grid.grid_res.is_finite()
        || grid.grid_res <= 0.0
        || !lat.is_finite()
        || !lng.is_finite()
    {
        return None;
    }
    let normalized_lat = lat.clamp(-90.0, 90.0);
    let x = ((wrap_longitude(lng) + 180.0) / grid.grid_res).floor();
    let y = (((normalized_lat + 90.0) / grid.grid_res).floor() as usize).min(grid.height - 1);
    if x < 0.0 || x >= grid.width as f64 {
        return None;
    }
    y.checked_mul(grid.width)?.checked_add(x as usize)
}

fn enforce_limit(kind: &'static str, actual: usize, limit: usize) -> Result<(), ProductionError> {
    if actual > limit {
        Err(ProductionError::Limit {
            kind,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn u32_count(value: u64) -> Result<u32, ProductionError> {
    u32::try_from(value).map_err(|_| ProductionError::AggregateOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        simulation::UnitSnapshot,
        tactical::SideKey,
        territory::{CountryAggregate, SideAggregate},
    };
    use serde_json::json;

    fn sample_scenario() -> DecodedScenario {
        let grid = GridSpec {
            grid_res: 90.0,
            width: 4,
            height: 2,
        };
        DecodedScenario {
            metadata: json!({
                "metadata": [
                    {"id": 0, "name": "Empty Land"},
                    {"id": 2, "name": "Beta", "gdp": 64, "population": 2000},
                    {"id": 1, "name": "Alpha", "gdp": 100, "pop": 1000}
                ],
                "cities": [
                    {"id": 10, "name": "Alpha City", "lat": -45, "lng": -90,
                     "pop": 100, "isCapital": true},
                    {"id": 11, "name": "Alpha Outpost", "lat": 45, "lng": 90,
                     "population": 50, "ownerId": 1},
                    {"id": "unresolved", "lat": 45, "lng": -90, "pop": 25}
                ]
            }),
            source: grid,
            target: grid,
            entry_count: 6,
            world_control: vec![1, 2, 0, 0, 0, 0, 0, 2],
            de_jure: vec![1, 1, 0, 0, 0, 0, 0, 2],
            land: vec![1, 1, 0, 0, 0, 0, 0, 1],
            biome: vec![0; 8],
            province: vec![0; 8],
        }
    }

    #[test]
    fn area_model_matches_browser_boundaries() {
        let config = ProductionConfig::default();
        assert_eq!(estimate_territory_army_units(0, &config).unwrap(), 3.0);
        assert_eq!(estimate_territory_army_units(1_500, &config).unwrap(), 99.0);
        assert_eq!(
            estimate_territory_army_units(6_000, &config).unwrap(),
            212.0
        );
        assert_eq!(
            estimate_territory_army_units(1_000_000, &config).unwrap(),
            2_400.0
        );
    }

    #[test]
    fn normalizes_metadata_maps_cities_and_economy_seeds() {
        let production =
            derive_scenario_production(&sample_scenario(), &ProductionConfig::default()).unwrap();
        assert_eq!(production.schema_version, PRODUCTION_SCHEMA_VERSION);
        assert_eq!(production.countries.len(), 2);
        assert_eq!(production.cities.len(), 3);
        assert_eq!(production.counters.unresolved_city_owners, 1);

        let alpha = &production.countries[0];
        assert_eq!(alpha.country_id, 1);
        assert_eq!(alpha.initial_core_cells, 1);
        assert_eq!(alpha.initial_owned_land_cells, 1);
        assert_eq!(alpha.initial_city_population, 50.0);
        assert_eq!(alpha.capital_cell, None);
        assert_eq!(alpha.expected_army_units, 3.0);

        let beta = &production.countries[1];
        assert_eq!(beta.initial_core_cells, 2);
        assert_eq!(beta.initial_owned_land_cells, 2);
        assert_eq!(beta.initial_city_population, 100.0);
        assert_eq!(beta.capital_cell, Some(1));
        assert_eq!(production.economy_seeds[0].initial_core_cells, 1);
        assert_eq!(production.economy_states[1].initial_core_cells, 2);
    }

    fn aggregate(
        country_id: u16,
        side_index: i16,
        owned: u64,
        controlled: u64,
        core_controlled: u64,
    ) -> CountryAggregate {
        CountryAggregate {
            country_id,
            side_index,
            owned,
            controlled,
            core_controlled,
            de_jure_total: owned.max(1),
            capital_held: true,
            ..CountryAggregate::default()
        }
    }

    fn unit(
        id: u64,
        side: SideKey,
        sovereign: u64,
        kind: UnitKind,
        position: (f64, f64),
        formation: (u64, u64),
        equipment: u64,
    ) -> UnitSnapshot {
        UnitSnapshot {
            id,
            side,
            sovereign,
            kind,
            lat: position.0,
            lng: position.1,
            health: 100.0,
            max_health: 100.0,
            health_fraction: 1.0,
            personnel: formation.0,
            personnel_capacity: formation.1,
            equipment,
            max_equipment: equipment,
            dir_lat: 0.0,
            dir_lng: 0.0,
            coast_stuck_ticks: 0,
            last_combat_tick: 0,
            victory_boost_ticks: 0,
            landing_penalty_active: false,
            transport: false,
            at_sea: false,
        }
    }

    #[test]
    fn derives_payroll_garrisons_casualties_and_directed_hostility() {
        let config = ProductionConfig::default();
        let scenario = sample_scenario();
        let production = derive_scenario_production(&scenario, &config).unwrap();
        let economies = production
            .economy_states
            .iter()
            .cloned()
            .map(|state| (state.country_id, state))
            .collect::<BTreeMap<_, _>>();
        let country_to_side = BTreeMap::from([(1_u16, 0_usize), (2, 1)]);

        let mut country_one = aggregate(1, 0, 1, 2, 2);
        country_one.city_population_controlled = 150.0;
        let mut country_two = aggregate(2, 1, 2, 1, 0);
        country_two.capital_held = false;
        country_two.de_jure_control_by_side.insert(0, 1);
        let territory = TerritorySnapshot {
            schema_version: TERRITORY_SCHEMA_VERSION.to_owned(),
            generation: 7,
            commit_sequence: 3,
            topology_revision: 1,
            world_revision: 2,
            city_revision: 3,
            processed_tiles: 1,
            // A committed sparse test snapshot need not expose a full-grid work count.
            processed_items: 1,
            pending_dirty_tiles_at_commit: 0,
            land_cells: 3,
            positive_occupation_cells: 0,
            negative_occupation_cells: 0,
            countries: vec![country_one, country_two],
            sides: vec![
                SideAggregate {
                    side_index: 0,
                    country_ids: vec![1],
                    ..SideAggregate::default()
                },
                SideAggregate {
                    side_index: 1,
                    country_ids: vec![2],
                    ..SideAggregate::default()
                },
            ],
        };
        let frame = FrameSnapshot {
            schema_version: NATIVE_TICK_SCHEMA_VERSION,
            tick: 600,
            frame: 60,
            units: vec![
                // Unit id zero is valid and mirrors existing simulation fixtures.
                unit(0, 0, 1, UnitKind::Army, (45.0, 90.0), (500, 1_000), 0),
                unit(2, 0, 1, UnitKind::Armor, (-45.0, -90.0), (0, 0), 50),
                unit(3, 1, 2, UnitKind::Army, (-45.0, 90.0), (1_500, 2_000), 0),
            ]
            .into(),
            events: Vec::new().into(),
            removed_ids: Vec::new().into(),
            abandoned_ids: Vec::new().into(),
        };
        let occupation = OccupationState {
            victim_id: 2,
            annexer_id: 1,
            base_income: 20.0,
            core_cells: 1,
            expected_army_units: 10.0,
            resistance: 0.0,
            occupation_coverage: 1.0,
            garrison_coverage: 0.0,
            garrison_assigned: 0.0,
            required_garrison: 3,
            held_ratio: 1.0,
            active_rebellion: false,
            queued_at_cycle: 0,
            cooldown_until_cycle: 0,
        };
        let occupations = BTreeMap::from([(2_u16, occupation)]);
        let casualties = BTreeMap::from([(2_u16, 5_000.0)]);
        let marker = TerritoryCommitMarker::from(&territory);
        let output = derive_strategic_cycle_input(
            StrategicDerivationInput {
                tick: 600,
                force: false,
                scenario: &production,
                grid: scenario.target,
                de_jure: &scenario.de_jure,
                territory: &territory,
                expected_territory: marker,
                territory_fresh: true,
                frame: &frame,
                country_to_side: &country_to_side,
                side_count: 2,
                // Directed input; strategic output canonicalizes the pair.
                hostility_matrix: &[0, 1, 0, 0],
                economies: &economies,
                occupations: &occupations,
                casualties: &casualties,
            },
            &config,
        )
        .unwrap();

        assert_eq!(output.input.active_sides, [0, 1]);
        assert_eq!(output.input.active_hostile_pairs, [(0, 1)]);
        assert_eq!(output.input.capitulation_active_sides, Some(vec![0]));
        assert_eq!(output.input.countries[0].unit_count, 2);
        assert_eq!(output.input.countries[0].payroll_due, 2.5);
        assert_eq!(output.input.countries[1].unit_count, 1);
        assert_eq!(output.input.countries[1].payroll_due, 2.0);
        assert_eq!(output.input.occupations[0].held_cells, 1);
        assert_eq!(output.input.occupations[0].garrison_strength, 0.5);
        assert_eq!(output.input.occupations[0].casualty_pressure, 0.5);
        assert_eq!(output.counters.garrison_units, 1);
    }

    #[test]
    fn rejects_duplicate_countries_and_unknown_dense_map_ids() {
        let mut duplicate = sample_scenario();
        duplicate.metadata["metadata"] = json!([{"id": 1}, {"id": 1}]);
        assert_eq!(
            derive_scenario_production(&duplicate, &ProductionConfig::default()),
            Err(ProductionError::DuplicateCountry(1))
        );

        let mut unknown = sample_scenario();
        unknown.de_jure[0] = 99;
        assert_eq!(
            derive_scenario_production(&unknown, &ProductionConfig::default()),
            Err(ProductionError::UnknownMapCountry(99))
        );
    }
}
