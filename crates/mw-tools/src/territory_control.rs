//! Frozen `territory-control-v1` JSON fixture/oracle adapter.

use anyhow::{Context, Result, bail};
use mw_core::{
    CellStateUpdate, CountryAggregate, InfluenceSource, SideAggregate, TERRITORY_SCHEMA_VERSION,
    TerritoryCity, TerritoryConfig, TerritoryControl, TerritoryMaps, TerritorySnapshot,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    hint::black_box,
    path::PathBuf,
    time::Instant,
};

const EXPECTED_LAND_VALUE: u8 = 2;
const EXPECTED_HYSTERESIS: f64 = 0.15;
const EXPECTED_CITY_RESISTANCE: f64 = 0.35;
const EXPECTED_HOSTILE_DECAY: f64 = 0.5;
const EXPECTED_RECLAIM_MULTIPLIER: f64 = 1.5;

pub fn run_fixture_command(args: Vec<String>) -> Result<()> {
    let (path, compact) = parse_fixture_args(args)?;
    let fixture = load_fixture(&path)?;
    print_json(&build_report(&fixture)?, compact)
}

pub fn run_bench_command(args: Vec<String>) -> Result<()> {
    let options = parse_bench_args(args)?;
    let fixture = load_fixture(&options.path)?;
    for _ in 0..options.warmup {
        black_box(run_benchmark_sample(
            &fixture,
            options.ticks,
            options.budget,
        )?);
    }
    let mut samples = Vec::with_capacity(options.repeat);
    for _ in 0..options.repeat {
        samples.push(run_benchmark_sample(
            &fixture,
            options.ticks,
            options.budget,
        )?);
    }
    let full_times = samples
        .iter()
        .map(|sample| sample.full_ms)
        .collect::<Vec<_>>();
    let persistent_times = samples
        .iter()
        .map(|sample| sample.persistent_ms)
        .collect::<Vec<_>>();
    let source_count = fixture
        .operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::ApplySources { sources } => Some(sources.len()),
            _ => None,
        })
        .sum::<usize>();
    let dirty_seed_count = if fixture.benchmark_dirty_cells.is_empty() {
        fixture
            .operations
            .iter()
            .filter_map(|operation| match operation {
                Operation::MarkCells { cell_indices, .. } => Some(cell_indices.len()),
                _ => None,
            })
            .sum()
    } else {
        fixture.benchmark_dirty_cells.len()
    };
    let first = samples.first().context("benchmark produced no samples")?;
    let output = json!({
        "schema": TERRITORY_SCHEMA_VERSION,
        "mode": "bench",
        "repeat": options.repeat,
        "warmup": options.warmup,
        "ticks": options.ticks,
        "budget": options.budget,
        "cells": fixture.config.width * fixture.config.height,
        "sources": source_count,
        "dirtySeedCells": dirty_seed_count,
        "full": { "medianMs": percentile(&full_times, 0.5), "p95Ms": percentile(&full_times, 0.95) },
        "persistent": { "medianMs": percentile(&persistent_times, 0.5), "p95Ms": percentile(&persistent_times, 0.95) },
        "processedItems": samples.iter().map(|sample| sample.processed_items).collect::<Vec<_>>(),
        "committedGenerations": samples.iter().map(|sample| sample.committed_generations).collect::<Vec<_>>(),
        "controllerChanges": samples.iter().map(|sample| sample.controller_changes).collect::<Vec<_>>(),
        "creditChanges": samples.iter().map(|sample| sample.credit_changes).collect::<Vec<_>>(),
        "remainingItems": samples.iter().map(|sample| sample.remaining_items).collect::<Vec<_>>(),
        "activeGenerations": samples.iter().map(|sample| sample.active_generation).collect::<Vec<_>>(),
        "dirtyTiles": samples.iter().map(|sample| sample.dirty_tiles).collect::<Vec<_>>(),
        "ownershipProjectionBytes": first.ownership_projection_bytes,
        "checksum": first.checksum,
    });
    print_json(&output, options.compact)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    schema: String,
    config: FixtureConfig,
    #[serde(default)]
    side_uids: Vec<String>,
    country_to_side: Vec<[i64; 2]>,
    hostility_matrix: Vec<u8>,
    maps: FixtureMaps,
    #[serde(default)]
    cities: Vec<FixtureCity>,
    #[serde(default)]
    operations: Vec<Operation>,
    #[serde(default)]
    benchmark_dirty_cells: Vec<usize>,
    #[serde(default)]
    topology_revision: u64,
    #[serde(default)]
    world_revision: u64,
    #[serde(default)]
    city_revision: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureConfig {
    width: usize,
    height: usize,
    grid_res: f64,
    max_sides: usize,
    #[serde(default = "default_tile_size")]
    tile_size: usize,
    #[serde(default = "default_land_value")]
    counted_land_value: u8,
    #[serde(default = "default_hysteresis")]
    hysteresis: f64,
    #[serde(default = "default_city_resistance")]
    city_resistance: f64,
    #[serde(default = "default_hostile_decay")]
    hostile_decay: f64,
    #[serde(default = "default_reclaim_multiplier")]
    reclaim_multiplier: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureMaps {
    land: Vec<i64>,
    world_control: Vec<i64>,
    de_jure: Vec<i64>,
    primary_occupier: Vec<i64>,
    dominant_side: Vec<i64>,
    occupation: Vec<f64>,
    side_influence: Vec<Vec<f64>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureCity {
    #[serde(default)]
    id: Value,
    #[serde(alias = "gridIndex")]
    cell_index: usize,
    #[serde(default, alias = "sovereignId")]
    owner_id: i64,
    #[serde(default, alias = "pop")]
    population: f64,
    #[serde(default, alias = "isCapital")]
    is_capital: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "op",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Operation {
    ApplySources {
        #[serde(default)]
        sources: Vec<FixtureSource>,
    },
    MarkCells {
        #[serde(default)]
        cell_indices: Vec<usize>,
        #[serde(default = "default_true")]
        include_neighbor_tiles: bool,
    },
    Mutate {
        #[serde(default)]
        changes: BTreeMap<String, Value>,
        #[serde(default)]
        mark_cells: Vec<usize>,
        #[serde(default = "default_true")]
        include_neighbor_tiles: bool,
    },
    Advance {
        #[serde(default = "default_budget")]
        budget: usize,
    },
    Flush {
        #[serde(default = "default_budget")]
        budget: usize,
    },
    Reset,
    Replace {
        maps: FixtureMaps,
        world_revision: Option<u64>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureSource {
    #[serde(default, rename = "id")]
    _id: Value,
    side_index: usize,
    sovereign_id: i64,
    #[serde(default)]
    beneficiary_id: i64,
    lat: f64,
    lng: f64,
    radius: f64,
    delta: f64,
    #[serde(default = "default_concentration")]
    concentration_bonus: f64,
    #[serde(default)]
    role: String,
    #[serde(default)]
    owner_ally_country_ids: Vec<i64>,
    #[serde(default)]
    support_country_ids_by_side: BTreeMap<String, Vec<i64>>,
    #[serde(default)]
    refuses_offense: bool,
    #[serde(default)]
    is_rebel: bool,
    #[serde(default)]
    rebel_id: i64,
    #[serde(default)]
    rebel_country_ids: Vec<i64>,
    #[serde(default)]
    rebel_core_by_country: BTreeMap<String, i64>,
}

fn default_tile_size() -> usize {
    32
}
fn default_land_value() -> u8 {
    2
}
fn default_hysteresis() -> f64 {
    0.15
}
fn default_city_resistance() -> f64 {
    0.35
}
fn default_hostile_decay() -> f64 {
    0.5
}
fn default_reclaim_multiplier() -> f64 {
    1.5
}
fn default_concentration() -> f64 {
    1.0
}
fn default_budget() -> usize {
    16_384
}
fn default_true() -> bool {
    true
}

struct Runner {
    control: TerritoryControl,
    config: FixtureConfig,
    side_uids: Vec<String>,
    country_to_side: BTreeMap<u16, usize>,
    world_revision: u64,
}

impl Runner {
    fn new(fixture: &Fixture) -> Result<Self> {
        validate_fixture(fixture)?;
        let country_to_side = mapping(&fixture.country_to_side, fixture.config.max_sides)?;
        let maps = maps(&fixture.maps, &fixture.config)?;
        let cities = cities(
            &fixture.cities,
            fixture.config.width * fixture.config.height,
        )?;
        let control = TerritoryControl::new(TerritoryConfig {
            width: fixture.config.width,
            height: fixture.config.height,
            grid_resolution: fixture.config.grid_res,
            max_sides: fixture.config.max_sides,
            tile_size: fixture.config.tile_size,
            maps,
            country_to_side: country_to_side.clone(),
            hostility_matrix: fixture.hostility_matrix.clone(),
            cities,
            protected_owner_ids: BTreeSet::new(),
            topology_revision: fixture.topology_revision,
            world_revision: fixture.world_revision,
            city_revision: fixture.city_revision,
        })?;
        Ok(Self {
            control,
            config: fixture.config.clone(),
            side_uids: fixture.side_uids.clone(),
            country_to_side,
            world_revision: fixture.world_revision,
        })
    }

    fn status_json(&self) -> Value {
        let status = self.control.census_status();
        json!({
            "hasSnapshot": status.has_snapshot,
            "commitSequence": status.commit_sequence,
            "activeGeneration": status.active_generation,
            "activeProcessedItems": status.active_processed_items,
            "activeTotalItems": status.active_total_items,
            "dirtyTiles": status.dirty_tile_indices.len(),
            "dirtyTileIndices": status.dirty_tile_indices,
            "mutationSequence": status.mutation_sequence,
            "topologyRevision": status.topology_revision,
            "worldRevision": status.world_revision,
            "cityRevision": status.city_revision,
        })
    }

    fn apply_sources(&mut self, sources: &[FixtureSource]) -> Result<Value> {
        let sources = sources
            .iter()
            .enumerate()
            .map(|(index, source)| self.source(source, index))
            .collect::<Result<Vec<_>>>()?;
        let result = self.control.apply_influence_sources(&sources)?;
        Ok(json!({
            "sources": sources.len(),
            "touchedCells": result.processed_source_cells,
            "controllerChanges": result.controller_change_count,
            "creditChanges": result.credit_change_count,
        }))
    }

    fn source(&self, input: &FixtureSource, index: usize) -> Result<InfluenceSource> {
        let sovereign = country_id(input.sovereign_id, "source.sovereignId")?;
        let beneficiary = optional_country_id(input.beneficiary_id, "source.beneficiaryId")?;
        let allies = input
            .owner_ally_country_ids
            .iter()
            .map(|id| country_id(*id, "source.ownerAllyCountryIds"))
            .collect::<Result<BTreeSet<_>>>()?;
        let mut protected = BTreeSet::new();
        if input.role.is_empty() || input.role == "OFFENSE" {
            for (side, ids) in &input.support_country_ids_by_side {
                let side = side
                    .parse::<usize>()
                    .context("support side key must be numeric")?;
                if side >= self.config.max_sides {
                    bail!("support side is out of range");
                }
                for id in ids {
                    let id = country_id(*id, "support country")?;
                    if side != input.side_index && self.country_to_side.get(&id) == Some(&side) {
                        protected.insert(id);
                    }
                }
            }
        }
        let rebel_de_jure = if input.is_rebel {
            Some(country_id(input.rebel_id, "source.rebelId")?)
        } else {
            None
        };
        let rebel_countries = input
            .rebel_country_ids
            .iter()
            .map(|id| country_id(*id, "source.rebelCountryIds"))
            .collect::<Result<BTreeSet<_>>>()?;
        let mut credit_restrictions = BTreeMap::new();
        for rebel in rebel_countries {
            let key = rebel.to_string();
            let core = input
                .rebel_core_by_country
                .get(&key)
                .with_context(|| format!("missing rebelCoreByCountry entry for {rebel}"))?;
            credit_restrictions.insert(rebel, country_id(*core, "source.rebelCoreByCountry")?);
        }
        Ok(InfluenceSource {
            id: index as u64,
            side: input.side_index,
            sovereign,
            beneficiary,
            lat: input.lat,
            lng: input.lng,
            radius: input.radius,
            delta: input.delta,
            concentration_bonus: input.concentration_bonus,
            owner_ally_country_ids: allies,
            protected_owner_ids: protected,
            rebel_de_jure,
            credit_de_jure: None,
            credit_de_jure_by_country: credit_restrictions,
            refuses_offense: input.refuses_offense,
        })
    }

    fn advance(&mut self, budget: usize) -> Value {
        let result = self.control.advance_census(budget);
        json!({
            "processedItems": result.processed_items,
            "committed": result.committed,
            "discarded": false,
            "generation": result.generation,
            "remainingItems": result.remaining_items,
            "dirtyTiles": self.control.dirty_tiles().len(),
            "hasSnapshot": result.has_snapshot,
            "snapshot": result.snapshot.as_deref().map(|snapshot| self.snapshot_json(snapshot)),
        })
    }

    fn flush(&mut self, budget: usize) -> Result<Value> {
        if budget == 0 {
            bail!("flush budget must be positive");
        }
        let mut processed = 0;
        let mut committed = 0;
        while self.control.census_status().active_generation.is_some()
            || !self.control.dirty_tiles().is_empty()
        {
            let result = self.control.advance_census(budget);
            processed += result.processed_items;
            committed += usize::from(result.committed);
            if result.processed_items == 0 && !result.committed {
                bail!("territory ledger flush made no deterministic progress");
            }
        }
        Ok(json!({
            "processedItems": processed,
            "committedGenerations": committed,
            "snapshot": self.control.snapshot().as_deref().map(|snapshot| self.snapshot_json(snapshot)),
        }))
    }

    fn mutate(&mut self, changes: &BTreeMap<String, Value>) -> Result<usize> {
        let mut cell_updates = BTreeMap::<usize, CellStateUpdate>::new();
        let mut influence = BTreeMap::<usize, Vec<(usize, f64)>>::new();
        let mut writes = 0;
        for (name, value) in changes {
            if name == "sideInfluence" {
                let object = value
                    .as_object()
                    .context("sideInfluence mutation must be an object")?;
                for (side, entries) in object {
                    let side = side
                        .parse::<usize>()
                        .context("mutate side key must be numeric")?;
                    if side >= self.config.max_sides {
                        bail!("mutate side is out of range");
                    }
                    for (cell, value) in mutation_pairs(entries)? {
                        influence.entry(side).or_default().push((cell, value));
                        writes += 1;
                    }
                }
                continue;
            }
            for (cell, value) in mutation_pairs(value)? {
                if cell >= self.control.total_cells() {
                    bail!("mutate index is out of range");
                }
                let update = cell_updates.entry(cell).or_default();
                match name.as_str() {
                    "land" => update.land = Some(checked_u8(value, "land")?),
                    "worldControl" => {
                        update.world_control = Some(checked_u16(value, "worldControl")?)
                    }
                    "deJure" => update.de_jure = Some(checked_u16(value, "deJure")?),
                    "primaryOccupier" => {
                        update.primary_occupier = Some(checked_u16(value, "primaryOccupier")?)
                    }
                    "dominantSide" => {
                        update.dominant_side =
                            Some(checked_controller(value, self.config.max_sides)?)
                    }
                    "occupation" => {
                        if !value.is_finite() {
                            bail!("occupation must be finite");
                        }
                        update.occupation = Some(value);
                    }
                    _ => bail!("unknown mutable map {name:?}"),
                }
                writes += 1;
            }
        }
        for entries in influence.values() {
            for (cell, value) in entries {
                if *cell >= self.control.total_cells() || !value.is_finite() || *value < 0.0 {
                    bail!("invalid sideInfluence mutation");
                }
            }
        }
        let updates = cell_updates.into_iter().collect::<Vec<_>>();
        self.control.set_cell_states_untracked(&updates)?;
        for (side, entries) in influence {
            self.control.set_side_influence_cells(side, &entries)?;
        }
        Ok(writes)
    }

    fn snapshot_json(&self, snapshot: &TerritorySnapshot) -> Value {
        let countries = snapshot
            .countries
            .iter()
            .map(|country| self.country_json(country))
            .collect::<Vec<_>>();
        let country_by_id = snapshot
            .countries
            .iter()
            .zip(countries.iter())
            .map(|(country, value)| json!([country.country_id, value]))
            .collect::<Vec<_>>();
        let sides = snapshot
            .sides
            .iter()
            .map(|side| self.side_json(side))
            .collect::<Vec<_>>();
        let side_by_index = snapshot
            .sides
            .iter()
            .zip(sides.iter())
            .map(|(side, value)| json!([side.side_index, value]))
            .collect::<Vec<_>>();
        json!({
            "generation": snapshot.generation,
            "commitSequence": snapshot.commit_sequence,
            "topologyRevision": snapshot.topology_revision,
            "worldRevision": snapshot.world_revision,
            "cityRevision": snapshot.city_revision,
            "processedTiles": snapshot.processed_tiles,
            "processedItems": snapshot.processed_items,
            "pendingDirtyTilesAtCommit": snapshot.pending_dirty_tiles_at_commit,
            "landCells": snapshot.land_cells,
            "positiveOccupationCells": snapshot.positive_occupation_cells,
            "negativeOccupationCells": snapshot.negative_occupation_cells,
            "countries": countries,
            "countryById": country_by_id,
            "sides": sides,
            "sideByIndex": side_by_index,
        })
    }

    fn country_json(&self, country: &CountryAggregate) -> Value {
        let side_uid = (country.side_index >= 0)
            .then(|| self.side_uids.get(country.side_index as usize))
            .flatten();
        json!({
            "countryId": country.country_id, "sideIndex": country.side_index, "sideUid": side_uid,
            "owned": country.owned, "controlled": country.controlled, "creditedTerritory": country.credited_territory,
            "frontline": country.frontline, "deJureTotal": country.de_jure_total, "coreControlled": country.core_controlled,
            "coreControlRatio": country.core_control_ratio, "deJureNotHeld": country.de_jure_not_held,
            "deJureControlBySide": map_pairs(&country.de_jure_control_by_side),
            "deJureControlByCountry": map_pairs(&country.de_jure_control_by_country),
            "citiesTotal": country.cities_total, "citiesControlled": country.cities_controlled,
            "cityPopulationTotal": country.city_population_total,
            "cityPopulationControlled": country.city_population_controlled,
            "capitalsTotal": country.capitals_total, "capitalsHeld": country.capitals_held,
            "capitalHeld": country.capital_held,
            "cityControlBySide": map_pairs(&country.city_control_by_side),
            "cityPopulationBySide": map_pairs(&country.city_population_by_side),
            "capitalControlBySide": map_pairs(&country.capital_control_by_side),
        })
    }

    fn side_json(&self, side: &SideAggregate) -> Value {
        json!({
            "sideIndex": side.side_index, "sideUid": self.side_uids.get(side.side_index),
            "countryIds": side.country_ids, "territory": side.territory,
            "ownedTerritory": side.owned_territory, "homeTerritoryControlled": side.home_territory_controlled,
            "frontline": side.frontline, "deJureCellsControlled": side.de_jure_cells_controlled,
            "citiesControlled": side.cities_controlled,
            "cityPopulationControlled": side.city_population_controlled,
            "capitalsControlled": side.capitals_controlled,
        })
    }

    fn maps_json(&self) -> Value {
        json!({
            "land": self.control.land(), "worldControl": self.control.world_control(),
            "deJure": self.control.de_jure(), "primaryOccupier": self.control.primary_occupier(),
            "dominantSide": self.control.dominant_side(),
            "occupation": self.control.occupation().iter().map(|value| f64::from(*value)).collect::<Vec<_>>(),
            "sideInfluence": self.control.all_side_influence().iter().map(|row| row.iter().map(|value| f64::from(*value)).collect::<Vec<_>>()).collect::<Vec<_>>(),
        })
    }

    fn render_json(&self) -> Option<Value> {
        self.control.snapshot()?;
        let mut all = Vec::new();
        let mut tiles = Vec::new();
        for tile in 0..self.control.total_tiles() {
            let bounds = self.control.tile_bounds(tile).ok()?;
            let mut payload = Vec::new();
            for y in bounds.min_y..bounds.max_y {
                for x in bounds.min_x..bounds.max_x {
                    let cell = y * self.config.width + x;
                    let primary = self.control.primary_occupier()[cell];
                    payload.push(if primary > 0 {
                        primary
                    } else {
                        self.control.world_control()[cell]
                    });
                }
            }
            all.extend_from_slice(&payload);
            tiles.push(json!({
                "tileIndex": tile,
                "bounds": { "minX": bounds.min_x, "minY": bounds.min_y, "maxX": bounds.max_x, "maxY": bounds.max_y },
                "payload": payload,
                "hash": hash(&payload),
            }));
        }
        Some(json!({
            "tileSize": self.config.tile_size,
            "tiles": tiles,
            "totalBytes": all.len() * 4,
            "checksum": hash(&all),
        }))
    }
}

fn build_report(fixture: &Fixture) -> Result<Value> {
    let mut runner = Runner::new(fixture)?;
    let mut operations = Vec::with_capacity(fixture.operations.len());
    for (index, operation) in fixture.operations.iter().enumerate() {
        let (name, result) = match operation {
            Operation::ApplySources { sources } => ("applySources", runner.apply_sources(sources)?),
            Operation::MarkCells {
                cell_indices,
                include_neighbor_tiles,
            } => (
                "markCells",
                json!({ "addedDirtyTiles": runner.control.mark_cells(cell_indices, *include_neighbor_tiles)? }),
            ),
            Operation::Mutate {
                changes,
                mark_cells,
                include_neighbor_tiles,
            } => {
                let writes = runner.mutate(changes)?;
                let added = runner
                    .control
                    .mark_cells(mark_cells, *include_neighbor_tiles)?;
                (
                    "mutate",
                    json!({ "writes": writes, "addedDirtyTiles": added }),
                )
            }
            Operation::Advance { budget } => ("advance", runner.advance(*budget)),
            Operation::Flush { budget } => ("flush", runner.flush(*budget)?),
            Operation::Reset => {
                runner.control.reset();
                ("reset", json!({ "reset": true }))
            }
            Operation::Replace {
                maps: replacement,
                world_revision,
            } => {
                let revision = world_revision.unwrap_or(runner.world_revision);
                runner
                    .control
                    .replace_maps_and_reset(maps(replacement, &runner.config)?, revision)?;
                runner.world_revision = revision;
                (
                    "replace",
                    json!({ "replaced": true, "worldRevision": revision }),
                )
            }
        };
        operations.push(json!({ "operationIndex": index, "op": name, "result": result, "status": runner.status_json() }));
    }
    Ok(json!({
        "schema": TERRITORY_SCHEMA_VERSION,
        "config": config_json(&fixture.config),
        "operationResults": operations,
        "final": {
            "status": runner.status_json(),
            "maps": runner.maps_json(),
            "snapshot": runner.control.snapshot().as_deref().map(|snapshot| runner.snapshot_json(snapshot)),
            "render": runner.render_json(),
        },
    }))
}

fn validate_fixture(fixture: &Fixture) -> Result<()> {
    if fixture.schema != TERRITORY_SCHEMA_VERSION {
        bail!("unsupported territory schema {:?}", fixture.schema);
    }
    let config = &fixture.config;
    if config.width == 0
        || config.height == 0
        || config.max_sides == 0
        || config.tile_size == 0
        || !config.grid_res.is_finite()
        || config.grid_res <= 0.0
    {
        bail!("invalid territory grid config");
    }
    for (name, actual, expected) in [
        (
            "countedLandValue",
            f64::from(config.counted_land_value),
            f64::from(EXPECTED_LAND_VALUE),
        ),
        ("hysteresis", config.hysteresis, EXPECTED_HYSTERESIS),
        (
            "cityResistance",
            config.city_resistance,
            EXPECTED_CITY_RESISTANCE,
        ),
        ("hostileDecay", config.hostile_decay, EXPECTED_HOSTILE_DECAY),
        (
            "reclaimMultiplier",
            config.reclaim_multiplier,
            EXPECTED_RECLAIM_MULTIPLIER,
        ),
    ] {
        if actual != expected {
            bail!("unsupported config.{name}={actual}; expected {expected}");
        }
    }
    if fixture.hostility_matrix.len() != config.max_sides * config.max_sides {
        bail!("hostilityMatrix has invalid length");
    }
    Ok(())
}

fn mapping(entries: &[[i64; 2]], max_sides: usize) -> Result<BTreeMap<u16, usize>> {
    let mut result = BTreeMap::new();
    let mut prior = 0;
    for [country, side] in entries {
        let country = country_id(*country, "countryToSide country")?;
        if country <= prior {
            bail!("countryToSide must be strictly sorted by country id");
        }
        let side = usize::try_from(*side).context("countryToSide side must be non-negative")?;
        if side >= max_sides {
            bail!("countryToSide side is out of range");
        }
        result.insert(country, side);
        prior = country;
    }
    Ok(result)
}

fn maps(input: &FixtureMaps, config: &FixtureConfig) -> Result<TerritoryMaps> {
    let cells = config
        .width
        .checked_mul(config.height)
        .context("cell count overflow")?;
    for (name, actual) in [
        ("land", input.land.len()),
        ("worldControl", input.world_control.len()),
        ("deJure", input.de_jure.len()),
        ("primaryOccupier", input.primary_occupier.len()),
        ("dominantSide", input.dominant_side.len()),
        ("occupation", input.occupation.len()),
    ] {
        if actual != cells {
            bail!("maps.{name} must have {cells} entries");
        }
    }
    if input.side_influence.len() != config.max_sides {
        bail!("maps.sideInfluence must have maxSides rows");
    }
    if input.side_influence.iter().any(|row| row.len() != cells) {
        bail!("maps.sideInfluence rows must have {cells} entries");
    }
    Ok(TerritoryMaps {
        land: input
            .land
            .iter()
            .map(|value| checked_u8(*value as f64, "land"))
            .collect::<Result<_>>()?,
        world_control: input
            .world_control
            .iter()
            .map(|value| checked_u16(*value as f64, "worldControl"))
            .collect::<Result<_>>()?,
        de_jure: input
            .de_jure
            .iter()
            .map(|value| checked_u16(*value as f64, "deJure"))
            .collect::<Result<_>>()?,
        primary_occupier: input
            .primary_occupier
            .iter()
            .map(|value| checked_u16(*value as f64, "primaryOccupier"))
            .collect::<Result<_>>()?,
        dominant_side: input
            .dominant_side
            .iter()
            .map(|value| checked_controller(*value as f64, config.max_sides))
            .collect::<Result<_>>()?,
        occupation: input
            .occupation
            .iter()
            .map(|value| {
                if !value.is_finite() {
                    bail!("occupation must be finite");
                }
                Ok(*value as f32)
            })
            .collect::<Result<_>>()?,
        side_influence: input
            .side_influence
            .iter()
            .enumerate()
            .map(|(side, row)| {
                row.iter()
                    .enumerate()
                    .map(|(cell, value)| {
                        if !value.is_finite() || *value < 0.0 {
                            bail!("invalid sideInfluence[{side}][{cell}]");
                        }
                        Ok(*value as f32)
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

fn cities(input: &[FixtureCity], cells: usize) -> Result<Vec<TerritoryCity>> {
    input
        .iter()
        .enumerate()
        .map(|(index, city)| {
            let _ = &city.id;
            if city.cell_index >= cells {
                bail!("city cellIndex is out of range");
            }
            if !city.population.is_finite() || city.population < 0.0 {
                bail!("city population is invalid");
            }
            Ok(TerritoryCity {
                id: index as u64,
                cell: city.cell_index,
                owner: optional_country_id(city.owner_id, "city ownerId")?,
                population: city.population,
                capital: city.is_capital,
            })
        })
        .collect()
}

fn mutation_pairs(value: &Value) -> Result<Vec<(usize, f64)>> {
    value
        .as_array()
        .context("mutation entries must be an array")?
        .iter()
        .map(|pair| {
            let pair = pair.as_array().context("mutation entry must be a pair")?;
            if pair.len() != 2 {
                bail!("mutation entry must be a pair");
            }
            let cell = pair[0]
                .as_u64()
                .context("mutation cell must be a non-negative integer")?
                as usize;
            let value = pair[1].as_f64().context("mutation value must be numeric")?;
            Ok((cell, value))
        })
        .collect()
}

fn checked_u8(value: f64, name: &str) -> Result<u8> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value > u8::MAX as f64 {
        bail!("{name} value is out of range");
    }
    Ok(value as u8)
}
fn checked_u16(value: f64, name: &str) -> Result<u16> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value > u16::MAX as f64 {
        bail!("{name} value is out of range");
    }
    Ok(value as u16)
}
fn checked_controller(value: f64, max_sides: usize) -> Result<i16> {
    if !value.is_finite() || value.fract() != 0.0 || value < -1.0 || value >= max_sides as f64 {
        bail!("dominantSide controller is out of range");
    }
    Ok(value as i16)
}
fn country_id(value: i64, name: &str) -> Result<u16> {
    let id = optional_country_id(value, name)?;
    if id == 0 {
        bail!("{name} must be positive");
    }
    Ok(id)
}
fn optional_country_id(value: i64, name: &str) -> Result<u16> {
    u16::try_from(value).with_context(|| format!("{name} is out of range"))
}
fn map_pairs<K: serde::Serialize, V: serde::Serialize>(map: &BTreeMap<K, V>) -> Vec<Value> {
    map.iter().map(|(key, value)| json!([key, value])).collect()
}
fn config_json(config: &FixtureConfig) -> Value {
    json!({ "width": config.width, "height": config.height, "gridRes": config.grid_res,
        "maxSides": config.max_sides, "tileSize": config.tile_size,
        "countedLandValue": config.counted_land_value, "hysteresis": config.hysteresis,
        "cityResistance": config.city_resistance, "hostileDecay": config.hostile_decay,
        "reclaimMultiplier": config.reclaim_multiplier })
}
fn hash(values: &[u16]) -> String {
    let mut hash = 0x811c_9dc5_u32;
    for value in values {
        let value = u32::from(*value);
        for shift in (0..32).step_by(8) {
            hash ^= (value >> shift) & 0xff;
            hash = hash.wrapping_mul(0x0100_0193);
        }
    }
    format!("{hash:08x}")
}

fn load_fixture(path: &PathBuf) -> Result<Fixture> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let fixture: Fixture = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    validate_fixture(&fixture)?;
    Ok(fixture)
}
fn print_json(value: &Value, compact: bool) -> Result<()> {
    println!(
        "{}",
        if compact {
            serde_json::to_string(value)?
        } else {
            serde_json::to_string_pretty(value)?
        }
    );
    Ok(())
}

fn parse_fixture_args(args: Vec<String>) -> Result<(PathBuf, bool)> {
    let mut path = None;
    let mut compact = false;
    for arg in args {
        if arg == "--json" {
            compact = true;
        } else if arg.starts_with('-') {
            bail!("unknown territory-control-fixture option {arg:?}");
        } else if path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            bail!("territory-control-fixture accepts one fixture path");
        }
    }
    Ok((
        path.context("usage: territory-control-fixture <fixture.json> [--json]")?,
        compact,
    ))
}

struct BenchOptions {
    path: PathBuf,
    repeat: usize,
    warmup: usize,
    ticks: usize,
    budget: usize,
    compact: bool,
}
fn parse_bench_args(args: Vec<String>) -> Result<BenchOptions> {
    let mut path = None;
    let (mut repeat, mut warmup, mut ticks, mut budget, mut compact) = (7, 2, 3, 16_384, false);
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let mut value = None;
        let flag = if let Some((flag, supplied)) = arg.split_once('=') {
            value = Some(supplied.to_owned());
            flag
        } else {
            arg.as_str()
        };
        match flag {
            "--repeat" | "--warmup" | "--ticks" | "--budget" => {
                let text = if let Some(value) = value {
                    value
                } else {
                    index += 1;
                    args.get(index)
                        .with_context(|| format!("{flag} needs a value"))?
                        .clone()
                };
                let parsed = text
                    .parse::<usize>()
                    .with_context(|| format!("invalid {flag}"))?;
                match flag {
                    "--repeat" => repeat = parsed,
                    "--warmup" => warmup = parsed,
                    "--ticks" => ticks = parsed,
                    _ => budget = parsed,
                }
            }
            "--json" => compact = true,
            _ if flag.starts_with('-') => bail!("unknown territory-control-bench option {flag:?}"),
            _ if path.is_none() => path = Some(PathBuf::from(flag)),
            _ => bail!("territory-control-bench accepts one fixture path"),
        }
        index += 1;
    }
    if repeat == 0 || ticks == 0 || budget == 0 {
        bail!("repeat, ticks, and budget must be positive");
    }
    Ok(BenchOptions { path: path.context("usage: territory-control-bench <fixture.json> [--repeat N] [--warmup N] [--ticks N] [--budget N] [--json]")?, repeat, warmup, ticks, budget, compact })
}

struct BenchmarkSample {
    full_ms: f64,
    persistent_ms: f64,
    processed_items: usize,
    committed_generations: usize,
    controller_changes: usize,
    credit_changes: usize,
    remaining_items: usize,
    active_generation: Option<u64>,
    dirty_tiles: usize,
    ownership_projection_bytes: usize,
    checksum: Option<String>,
}
fn run_benchmark_sample(fixture: &Fixture, ticks: usize, budget: usize) -> Result<BenchmarkSample> {
    let sources = fixture
        .operations
        .iter()
        .filter_map(|operation| {
            if let Operation::ApplySources { sources } = operation {
                Some(sources.as_slice())
            } else {
                None
            }
        })
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let dirty = if fixture.benchmark_dirty_cells.is_empty() {
        fixture
            .operations
            .iter()
            .filter_map(|operation| {
                if let Operation::MarkCells { cell_indices, .. } = operation {
                    Some(cell_indices.as_slice())
                } else {
                    None
                }
            })
            .flatten()
            .copied()
            .collect::<Vec<_>>()
    } else {
        fixture.benchmark_dirty_cells.clone()
    };
    let mut runner = Runner::new(fixture)?;
    let full_start = Instant::now();
    runner.apply_sources(&sources)?;
    runner.flush(budget)?;
    let full_ms = full_start.elapsed().as_secs_f64() * 1000.0;
    let persistent_start = Instant::now();
    let (mut processed_items, mut committed_generations) = (0, 0);
    let (mut controller_changes, mut credit_changes) = (0, 0);
    for _ in 0..ticks {
        let result = runner.apply_sources(&sources)?;
        controller_changes += result["controllerChanges"].as_u64().unwrap_or(0) as usize;
        credit_changes += result["creditChanges"].as_u64().unwrap_or(0) as usize;
        runner.control.mark_cells(&dirty, true)?;
        let flushed = runner.flush(budget)?;
        processed_items += flushed["processedItems"].as_u64().unwrap_or(0) as usize;
        committed_generations += flushed["committedGenerations"].as_u64().unwrap_or(0) as usize;
    }
    let persistent_ms = persistent_start.elapsed().as_secs_f64() * 1000.0;
    let status = runner.control.census_status();
    let remaining_items = status
        .active_total_items
        .saturating_sub(status.active_processed_items);
    let rendered = runner.render_json();
    Ok(BenchmarkSample {
        full_ms,
        persistent_ms,
        processed_items,
        committed_generations,
        controller_changes,
        credit_changes,
        remaining_items,
        active_generation: status.active_generation,
        dirty_tiles: runner.control.dirty_tiles().len(),
        ownership_projection_bytes: rendered
            .as_ref()
            .and_then(|value| value["totalBytes"].as_u64())
            .unwrap_or(0) as usize,
        checksum: rendered
            .as_ref()
            .and_then(|value| value["checksum"].as_str())
            .map(str::to_owned),
    })
}
fn percentile(values: &[f64], percentile: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted.get(index).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &[u8] = include_bytes!("../../../fixtures/territory-control-v1.json");

    fn fixture() -> Fixture {
        serde_json::from_slice(FIXTURE).unwrap()
    }

    #[test]
    fn canonical_fixture_runs_deterministically() {
        let fixture = fixture();
        let first = build_report(&fixture).unwrap();
        let second = build_report(&fixture).unwrap();
        assert_eq!(first, second);
        assert_eq!(first["schema"], TERRITORY_SCHEMA_VERSION);
        assert_eq!(first["operationResults"].as_array().unwrap().len(), 7);
        assert_eq!(first["final"]["snapshot"]["landCells"], 53);
        assert_eq!(first["final"]["render"]["totalBytes"], 216);
    }

    #[test]
    fn unsupported_math_constants_are_rejected() {
        let mut fixture = fixture();
        fixture.config.hostile_decay = 0.25;
        assert!(Runner::new(&fixture).is_err());
    }

    #[test]
    fn reset_and_replace_operations_execute() {
        let mut fixture = fixture();
        fixture.operations = vec![
            Operation::Flush { budget: 100 },
            Operation::Reset,
            Operation::Replace {
                maps: fixture.maps.clone(),
                world_revision: Some(9),
            },
        ];
        let report = build_report(&fixture).unwrap();
        assert_eq!(
            report["operationResults"][1]["status"]["hasSnapshot"],
            false
        );
        assert_eq!(report["operationResults"][2]["status"]["worldRevision"], 9);
    }
}
