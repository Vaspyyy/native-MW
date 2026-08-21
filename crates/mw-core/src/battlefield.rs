//! Stateless resolution of live, position-dependent battlefield modifiers.
//!
//! The browser originally resolved these values only while building the native
//! hand-off.  They are not stable policy: moving into water, mountains, cities,
//! friendly influence, or an encirclement changes them.  This module keeps the
//! country-level primitives explicit and combines them with borrowed live maps
//! in one deterministic batch.  It deliberately does not mutate units or maps.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{
    ai::{ResolvedCombatModifiers, ResolvedMovementModifiers},
    combat::{UnitKind, wrapped_longitude_delta},
    direction::HostilityMatrix,
    tactical::{PairOptions, SideKey, TacticalGrid, TacticalGridError, TacticalUnit},
    world::{WorldGridError, WorldGridView},
};

pub const BATTLEFIELD_SCHEMA_VERSION: &str = "native-battlefield-v1";
pub const ATTRITION_DAMAGE: f64 = 0.06;
pub const ENCIRCLEMENT_DAMAGE_MULT: f64 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldAttritionResult {
    pub damage: f64,
    pub encircled: bool,
    pub encircled_ticks: u64,
    pub friendly_tiles: usize,
    pub supply_collapsed: bool,
}

/// Browser-parity attrition arithmetic. This is intentionally pure: callers
/// commit the returned damage through `Simulation::apply_batch_damage`.
fn calculate_attrition(
    config: BattlefieldConfig,
    maps: BattlefieldMapView<'_>,
    unit: BattlefieldUnitInput,
    frame: u64,
    target_land_size: f64,
    damage_taken_multiplier: f64,
) -> BattlefieldAttritionResult {
    let cell = maps.world.grid_index(unit.lat, unit.lng);
    let at_sea = cell.is_none_or(|i| maps.world.land_mask[i] == 0);
    if at_sea {
        return BattlefieldAttritionResult {
            damage: if unit.transport {
                0.0
            } else {
                ATTRITION_DAMAGE * 3.0
            },
            encircled: false,
            encircled_ticks: 0,
            friendly_tiles: 0,
            supply_collapsed: false,
        };
    }
    let encircled = !at_sea
        && !matches!(
            unit.country.combat_buff,
            BattlefieldBuff::Buff | BattlefieldBuff::Super
        )
        && cell.is_some_and(|i| {
            unit_is_encircled(
                config,
                maps,
                unit,
                BattlefieldCellState {
                    index: Some(i),
                    at_sea,
                    mountain_intensity: 0.0,
                    mountain: false,
                    current_cell_urban: false,
                    combat_urban: false,
                },
            )
        });
    let encircled_ticks = if encircled {
        unit.encircled_ticks.saturating_add(1)
    } else {
        0
    };
    let enemy = !at_sea
        && cell
            .is_some_and(|i| sides_are_hostile(maps.hostility, unit.side, maps.dominant_side[i]));
    let gate = (enemy || encircled)
        && !matches!(
            unit.country.combat_buff,
            BattlefieldBuff::Buff | BattlefieldBuff::Super
        )
        // Browser applies this tick's combat/speed boost, decrements the
        // counter, then evaluates attrition. A saved value of one therefore
        // no longer grants attrition immunity.
        && (unit.victory_boost_ticks <= 1 || encircled);
    if !gate {
        return BattlefieldAttritionResult {
            damage: 0.0,
            encircled,
            encircled_ticks,
            friendly_tiles: 0,
            supply_collapsed: false,
        };
    }
    let control = cell.map_or(0.0, |i| {
        if maps.world.land_mask[i] == 2 {
            maps.occupation[i] as f64
        } else {
            0.0
        }
    });
    let logistics = (target_land_size / 500.0 + 1.0).log10().max(1.0);
    let mut damage =
        ATTRITION_DAMAGE * (1.0 + control.abs() * 3.0) * logistics * (1.0 + frame as f64 / 8000.0);
    if encircled {
        let scale = if encircled_ticks > 360 {
            4.0
        } else if encircled_ticks > 180 {
            2.5
        } else if encircled_ticks > 60 {
            1.5
        } else {
            1.0
        };
        damage *= ENCIRCLEMENT_DAMAGE_MULT * scale;
    }
    let mut friendly_tiles = 0;
    let mut collapsed = false;
    if enemy
        && !encircled
        && let Some(i) = cell
    {
        let radius = (0.8 / maps.world.grid_res).round() as isize;
        let row = (i / maps.world.width) as isize;
        let col = (i % maps.world.width) as isize;
        for dr in -radius..=radius {
            for dc in -radius..=radius {
                if dr == 0 && dc == 0 {
                    continue;
                }
                let (r, c) = (row + dr, col + dc);
                if r >= 0
                    && c >= 0
                    && r < maps.world.height as isize
                    && c < maps.world.width as isize
                {
                    let n = r as usize * maps.world.width + c as usize;
                    if maps.world.land_mask[n] > 0 && maps.dominant_side[n] == unit.side as i16 {
                        friendly_tiles += 1;
                    }
                }
            }
        }
        match friendly_tiles {
            0 => {
                damage += 2.5;
                collapsed = true;
            }
            1..=2 => damage += 0.8,
            3..=7 => damage += 0.3,
            _ => {}
        }
    }
    BattlefieldAttritionResult {
        damage: damage * damage_taken_multiplier,
        encircled,
        encircled_ticks,
        friendly_tiles,
        supply_collapsed: collapsed,
    }
}

/// Browser country buff states that alter combat and territorial influence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BattlefieldBuff {
    #[default]
    None,
    Buff,
    Super,
    Godly,
    Weakened,
    Crippled,
}

/// Only advancing and collapsing phases affect the browser's battlefield math.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BattlefieldWarPhase {
    #[default]
    Stable,
    Advancing,
    Collapsing,
}

/// Country-owned inputs that must not be inferred from live terrain.
///
/// Combat uses the effective buff (which may include metadata fallback), while
/// influence uses the country's directly selected buff.  Keeping both values
/// avoids silently merging two distinct browser rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CountryBattlefieldPrimitives {
    pub combat_buff: BattlefieldBuff,
    pub influence_buff: BattlefieldBuff,
    pub attack_buff_percent: f64,
    pub defense_buff_percent: f64,
    pub capital_lost: bool,
    pub war_phase: BattlefieldWarPhase,
    pub conquest_mode: bool,
    pub ai_speed_multiplier: f64,
}

impl Default for CountryBattlefieldPrimitives {
    fn default() -> Self {
        Self {
            combat_buff: BattlefieldBuff::None,
            influence_buff: BattlefieldBuff::None,
            attack_buff_percent: 0.0,
            defense_buff_percent: 0.0,
            capital_lost: false,
            war_phase: BattlefieldWarPhase::Stable,
            conquest_mode: true,
            ai_speed_multiplier: 1.0,
        }
    }
}

/// Browser parity constants used by the live resolver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldConfig {
    pub unit_speed: f64,
    pub unit_naval_speed: f64,
    pub influence_rate: f64,
    pub influence_radius: f64,
    pub encirclement_radius: f64,
    pub armor_support_radius: f64,
    pub armor_support_memory_ticks: u64,
    pub alpen_mountain_speed_multiplier: f64,
    pub alpen_combat_multiplier: f64,
    pub native_speed_scale: f64,
    pub active_combat_exclusion_frames: u64,
    pub long_war_frame_threshold: u64,
    pub long_war_defense_multiplier: f64,
}

impl Default for BattlefieldConfig {
    fn default() -> Self {
        Self {
            unit_speed: 0.003,
            unit_naval_speed: 0.025,
            influence_rate: 0.18,
            influence_radius: 0.4,
            encirclement_radius: 0.7,
            armor_support_radius: 0.6,
            armor_support_memory_ticks: 12,
            alpen_mountain_speed_multiplier: 1.4,
            alpen_combat_multiplier: 1.12,
            // `resolveNativeRuntimeUnitPolicy` scales the browser AI profile at
            // the native hand-off because native simulation advances every tick.
            // The movement kernel applies the browser's final 0.8 scale exactly once.
            native_speed_scale: 1.0,
            active_combat_exclusion_frames: 5,
            long_war_frame_threshold: 6_000,
            long_war_defense_multiplier: 0.75,
        }
    }
}

/// Stable city coordinate needed for both exact-cell armor movement and the
/// sovereign-city combat/defense proximity rule.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldUrbanCenter {
    pub id: u64,
    pub country_id: u16,
    pub cell: usize,
    pub lat: f64,
    pub lng: f64,
}

/// Per-unit battlefield memory that must survive native save/reload.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldUnitState {
    pub is_alpenjager: bool,
    /// Original browser unit id (or a stable native equivalent) used to retain
    /// four-way tactical group ownership after integer ID remapping.
    pub cohesion_seed: f64,
    pub local_tactics_excluded: bool,
    pub encircled_ticks: u64,
    pub armor_support_last_tick: Option<u64>,
    pub last_ally_count: f64,
}

impl Default for BattlefieldUnitState {
    fn default() -> Self {
        Self {
            is_alpenjager: false,
            cohesion_seed: 0.0,
            local_tactics_excluded: false,
            encircled_ticks: 0,
            armor_support_last_tick: None,
            last_ally_count: 1.0,
        }
    }
}

/// Complete optional live-battlefield state owned by runtime/checkpoint v2.
#[derive(Clone, Debug, PartialEq)]
pub struct BattlefieldRuntimeState {
    pub config: BattlefieldConfig,
    pub mountains_enabled: bool,
    pub terrain_intensity: Vec<f32>,
    pub urban_centers: Vec<BattlefieldUrbanCenter>,
    pub countries: BTreeMap<u16, CountryBattlefieldPrimitives>,
    pub units: BTreeMap<u64, BattlefieldUnitState>,
}

impl BattlefieldRuntimeState {
    /// Validate complete state coverage against immutable grid/topology/live IDs.
    pub fn validate(
        &self,
        world: WorldGridView<'_>,
        max_sides: usize,
        country_to_side: &BTreeMap<u16, usize>,
        live_unit_ids: &[u64],
    ) -> Result<(), BattlefieldError> {
        validate_config(self.config)?;
        world.validate()?;
        let cells = world
            .width
            .checked_mul(world.height)
            .ok_or(BattlefieldError::InvalidConfig)?;
        validate_map_length("terrain_intensity", self.terrain_intensity.len(), cells)?;
        if self
            .terrain_intensity
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(BattlefieldError::InvalidBattlefieldState(
                "terrain intensity must be finite and within [0, 1]",
            ));
        }
        if max_sides == 0
            || country_to_side
                .iter()
                .any(|(country, side)| *country == 0 || *side >= max_sides)
            || self.countries.keys().ne(country_to_side.keys())
        {
            return Err(BattlefieldError::InvalidCountryTopology);
        }
        for (&country, primitives) in &self.countries {
            if !country_primitives_are_valid(*primitives) {
                return Err(BattlefieldError::InvalidBattlefieldCountry(country));
            }
        }
        let mut city_ids = BTreeSet::new();
        for city in &self.urban_centers {
            if city.id == 0
                || !city_ids.insert(city.id)
                || city.country_id == 0
                || !self.countries.contains_key(&city.country_id)
                || city.cell >= cells
                || world.grid_index(city.lat, city.lng) != Some(city.cell)
            {
                return Err(BattlefieldError::InvalidUrbanCenter(city.id));
            }
        }
        let live_ids = live_unit_ids.iter().copied().collect::<BTreeSet<_>>();
        if live_ids.len() != live_unit_ids.len()
            || live_ids.len() != self.units.len()
            || live_ids.iter().ne(self.units.keys())
        {
            return Err(BattlefieldError::BattlefieldUnitCoverage);
        }
        for (&id, state) in &self.units {
            if cohesion_group(state.cohesion_seed).is_none()
                || !state.last_ally_count.is_finite()
                || state.last_ally_count < 0.0
            {
                return Err(BattlefieldError::InvalidBattlefieldUnitState(id));
            }
        }
        Ok(())
    }

    /// Build the immutable exact-city-cell mask once when state is installed.
    pub fn urban_cell_mask(&self, world: WorldGridView<'_>) -> Result<Vec<u8>, BattlefieldError> {
        world.validate()?;
        let mut mask = vec![0; world.width * world.height];
        for city in &self.urban_centers {
            if city.cell >= mask.len() || world.grid_index(city.lat, city.lng) != Some(city.cell) {
                return Err(BattlefieldError::InvalidUrbanCenter(city.id));
            }
            mask[city.cell] = 1;
        }
        Ok(mask)
    }

    /// Exact browser sovereign-city proximity (`distance² < 0.04`).
    pub fn near_sovereign_city(&self, country_id: u16, lat: f64, lng: f64) -> bool {
        lat.is_finite()
            && lng.is_finite()
            && self.urban_centers.iter().any(|city| {
                city.country_id == country_id
                    && (lat - city.lat).powi(2) + (lng - city.lng).powi(2) < 0.04
            })
    }
}

/// Immutable dense maps sampled by a battlefield tick.
#[derive(Clone, Copy, Debug)]
pub struct BattlefieldMapView<'a> {
    pub world: WorldGridView<'a>,
    /// Mountain intensity in `[0, 1]`. `None` is an all-flat world.
    pub terrain_intensity: Option<&'a [f32]>,
    /// Non-zero cells are active theater city cells. `None` has no cities.
    pub urban_cells: Option<&'a [u8]>,
    pub world_control: &'a [u16],
    pub de_jure: &'a [u16],
    pub dominant_side: &'a [i16],
    pub occupation: &'a [f32],
    /// One dense Float32 influence plane per side.
    pub side_influence: &'a [Vec<f32>],
    pub country_to_side: &'a BTreeMap<u16, usize>,
    pub hostility: HostilityMatrix<'a>,
}

/// Stable and live per-unit values needed by the resolver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldUnitInput {
    pub id: u64,
    pub side: SideKey,
    pub sovereign: u16,
    pub kind: UnitKind,
    pub transport: bool,
    pub lat: f64,
    pub lng: f64,
    /// Browser formation-equivalent strength. Armor supplies `1.0`.
    pub formation_strength: f64,
    /// Whether this unit contributes to the side-zero/side-one unopposed test.
    pub counts_for_capitulation: bool,
    pub armor_supported: bool,
    pub is_alpenjager: bool,
    pub victory_boost_ticks: u64,
    pub encircled_ticks: u64,
    /// `None` means the unit has never fought. This distinction is lost if a
    /// sentinel zero is used while the simulation is still within frame five.
    pub last_combat_frame: Option<u64>,
    /// Browser `(lastAllyCount || 1)`, before division by five.
    pub last_ally_count: f64,
    /// The browser grants an additional defense/urban modifier within 0.2° of
    /// a city belonging to the unit's sovereign. The adapter resolves that
    /// spatial query once and supplies it explicitly.
    pub near_sovereign_city: bool,
    pub country: CountryBattlefieldPrimitives,
}

/// One borrowed, immutable logical-tick input.
#[derive(Clone, Copy, Debug)]
pub struct BattlefieldTickInput<'a> {
    pub tick: u64,
    pub frame: u64,
    pub mountains_enabled: bool,
    pub maps: BattlefieldMapView<'a>,
    /// Browser country-ledger controlled-cell totals, already summed across
    /// every side that this side treats as hostile.
    pub hostile_controlled_land_by_side: &'a [f64],
    pub units: &'a [BattlefieldUnitInput],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldCellState {
    pub index: Option<usize>,
    pub at_sea: bool,
    pub mountain_intensity: f64,
    pub mountain: bool,
    /// Exact active-city cell used by browser armor movement.
    pub current_cell_urban: bool,
    /// Active-city cell or the unit's sovereign-city proximity used by combat.
    pub combat_urban: bool,
}

/// Static influence inputs. The runtime may subsequently apply its per-tick
/// mobilization ramp and deterministic temporal radius/delta noise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldInfluenceModifiers {
    pub radius: f64,
    pub delta: f64,
    pub concentration_bonus: f64,
    /// False while the unit's last combat frame is at most five frames old.
    pub eligible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldUnitResult {
    pub unit_id: u64,
    pub cell: BattlefieldCellState,
    pub encircled: bool,
    pub encircled_ticks: u64,
    pub base_speed: f64,
    pub movement: ResolvedMovementModifiers,
    pub combat: ResolvedCombatModifiers,
    pub influence: BattlefieldInfluenceModifiers,
    pub attrition: BattlefieldAttritionResult,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BattlefieldTickResult {
    pub schema_version: &'static str,
    pub tick: u64,
    pub frame: u64,
    /// Preserves the caller's stable unit storage order.
    pub units: Vec<BattlefieldUnitResult>,
}

/// Live values used by the pair-once friendly-tactics prepass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldLocalUnitInput {
    pub id: u64,
    pub side: SideKey,
    pub sovereign: u16,
    pub kind: UnitKind,
    pub lat: f64,
    pub lng: f64,
    pub formation_strength: f64,
    pub refuses_offense: bool,
    /// Heading published by the preceding tick, sampled before new AI orders.
    pub previous_dir_lat: f64,
    pub previous_dir_lng: f64,
    pub task_force_key: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BattlefieldVector {
    pub lat: f64,
    pub lng: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldCohesionGroup {
    pub side: SideKey,
    pub group: u8,
    pub lat: f64,
    pub lng: f64,
    pub dir_lat: f64,
    pub dir_lng: f64,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldLocalUnitResult {
    pub unit_id: u64,
    pub side: SideKey,
    pub lat: f64,
    pub lng: f64,
    pub cohesion_group: u8,
    pub armor_supported: bool,
    pub armor_support_last_tick: Option<u64>,
    pub last_ally_count: f64,
    pub repulsion: Option<BattlefieldVector>,
    pub task_force_key: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BattlefieldLocalTacticsResult {
    pub schema_version: &'static str,
    pub tick: u64,
    pub groups: BTreeMap<(SideKey, u8), BattlefieldCohesionGroup>,
    /// Preserves the input unit order.
    pub units: Vec<BattlefieldLocalUnitResult>,
}

/// A post-AI heading plus the browser gates around cohesion and repulsion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldDirectionInput {
    pub unit_id: u64,
    pub dir_lat: f64,
    pub dir_lng: f64,
    pub is_plan_unit: bool,
    pub at_sea: bool,
    pub active_retreat: bool,
    pub occupation_garrison_holding: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldDirectionResult {
    pub unit_id: u64,
    pub dir_lat: f64,
    pub dir_lng: f64,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BattlefieldError {
    #[error(transparent)]
    World(#[from] WorldGridError),
    #[error("{map} length {actual} does not match battlefield grid size {expected}")]
    MapLength {
        map: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("side-influence row count does not match the hostility side count")]
    SideInfluenceRows,
    #[error("hostility matrix has invalid dimensions")]
    InvalidHostility,
    #[error("hostile controlled-land totals have invalid dimensions or values")]
    InvalidHostileLandTotals,
    #[error("country-to-side topology contains an invalid side")]
    InvalidCountryTopology,
    #[error("duplicate battlefield unit id {0}")]
    DuplicateUnit(u64),
    #[error("battlefield unit {0} contains invalid data")]
    InvalidUnit(u64),
    #[error("battlefield country {0} contains invalid primitives")]
    InvalidBattlefieldCountry(u16),
    #[error("battlefield unit state {0} contains invalid data")]
    InvalidBattlefieldUnitState(u64),
    #[error("battlefield state unit ids do not exactly cover the live simulation")]
    BattlefieldUnitCoverage,
    #[error("urban center {0} is invalid")]
    InvalidUrbanCenter(u64),
    #[error("invalid battlefield state: {0}")]
    InvalidBattlefieldState(&'static str),
    #[error("battlefield configuration is invalid")]
    InvalidConfig,
    #[error(transparent)]
    Tactical(#[from] TacticalGridError),
}

/// Browser armor movement precedence: sea, then mountain, then urban.
pub const fn armor_speed_multiplier(at_sea: bool, mountain: bool, urban: bool) -> f64 {
    if at_sea {
        0.75
    } else if mountain {
        0.45
    } else if urban {
        0.75
    } else {
        1.6
    }
}

/// Unsupported armor captures at one quarter of its supported pressure.
pub const fn armor_influence_multiplier(supported: bool) -> f64 {
    if supported { 1.0 } else { 0.25 }
}

/// Whether a unit may stamp influence at `frame` under the browser's active
/// combat exclusion. A future combat marker is conservatively still active.
pub const fn active_combat_influence_eligible(
    frame: u64,
    last_combat_frame: Option<u64>,
    exclusion_frames: u64,
) -> bool {
    match last_combat_frame {
        None => true,
        Some(last) if last > frame => false,
        Some(last) => frame - last > exclusion_frames,
    }
}

/// Stable four-way group derived from the preserved browser/native seed.
pub fn cohesion_group(cohesion_seed: f64) -> Option<u8> {
    let scaled = (cohesion_seed * 1_000.0).floor();
    if !scaled.is_finite() || scaled < 0.0 || scaled > u64::MAX as f64 {
        return None;
    }
    Some((scaled as u64 % 4) as u8)
}

/// Recompute local friendly strength, armor support memory, repulsion, and the
/// four stable cohesion centroids from one immutable live-unit snapshot.
pub fn resolve_local_tactics(
    tick: u64,
    state: &BattlefieldRuntimeState,
    units: &[BattlefieldLocalUnitInput],
) -> Result<BattlefieldLocalTacticsResult, BattlefieldError> {
    validate_config(state.config)?;
    let mut input_ids = BTreeSet::new();
    let mut sides = BTreeSet::new();
    let mut tactical_units = Vec::with_capacity(units.len());
    let mut ally_weights = Vec::with_capacity(units.len());
    let mut excluded = Vec::with_capacity(units.len());
    let mut can_support_armor = Vec::with_capacity(units.len());
    let mut groups = BTreeMap::<(SideKey, u8), GroupAccumulator>::new();
    let mut results = Vec::with_capacity(units.len());

    for unit in units {
        if !input_ids.insert(unit.id) {
            return Err(BattlefieldError::DuplicateUnit(unit.id));
        }
        let Some(memory) = state.units.get(&unit.id) else {
            return Err(BattlefieldError::BattlefieldUnitCoverage);
        };
        let Some(country) = state.countries.get(&unit.sovereign) else {
            return Err(BattlefieldError::InvalidUnit(unit.id));
        };
        if ![
            unit.lat,
            unit.lng,
            unit.formation_strength,
            unit.previous_dir_lat,
            unit.previous_dir_lng,
            memory.cohesion_seed,
            memory.last_ally_count,
        ]
        .into_iter()
        .all(f64::is_finite)
            || unit.formation_strength < 0.0
            || memory.cohesion_seed < 0.0
            || memory.last_ally_count < 0.0
            || memory
                .armor_support_last_tick
                .is_some_and(|last| last > tick)
        {
            return Err(BattlefieldError::InvalidBattlefieldUnitState(unit.id));
        }
        let Some(group) = cohesion_group(memory.cohesion_seed) else {
            return Err(BattlefieldError::InvalidBattlefieldUnitState(unit.id));
        };
        let strength = unit.formation_strength.max(0.0);
        let ally_weight = strength
            * match country.influence_buff {
                BattlefieldBuff::Super => 200.0,
                BattlefieldBuff::Buff => 50.0,
                BattlefieldBuff::None
                | BattlefieldBuff::Godly
                | BattlefieldBuff::Weakened
                | BattlefieldBuff::Crippled => 1.0,
            };
        if !ally_weight.is_finite() {
            return Err(BattlefieldError::InvalidUnit(unit.id));
        }
        let direct_support =
            unit.kind != UnitKind::Armor && strength > 0.0 && !unit.refuses_offense;
        let armor_support_last_tick = if unit.kind == UnitKind::Armor {
            memory.armor_support_last_tick
        } else {
            None
        };
        let armor_supported = armor_support_last_tick
            .is_some_and(|last| tick - last <= state.config.armor_support_memory_ticks);
        results.push(BattlefieldLocalUnitResult {
            unit_id: unit.id,
            side: unit.side,
            lat: unit.lat,
            lng: unit.lng,
            cohesion_group: group,
            armor_supported,
            armor_support_last_tick,
            last_ally_count: strength,
            repulsion: None,
            task_force_key: unit.task_force_key,
        });
        groups.entry((unit.side, group)).or_default().add(*unit);
        sides.insert(unit.side);
        ally_weights.push(ally_weight);
        excluded.push(memory.local_tactics_excluded);
        can_support_armor.push(direct_support);
        tactical_units.push(TacticalUnit {
            id: unit.id,
            side: Some(unit.side),
            lat: unit.lat,
            lng: unit.lng,
            strength,
            ally_weight,
            is_armor: unit.kind == UnitKind::Armor,
            is_support: direct_support,
        });
    }
    if input_ids.len() != state.units.len() || input_ids.iter().ne(state.units.keys()) {
        return Err(BattlefieldError::BattlefieldUnitCoverage);
    }

    let mut grid = TacticalGrid::new(state.config.armor_support_radius)?;
    grid.rebuild(&tactical_units)?;
    let tactical_radius_sq = state.config.armor_support_radius.powi(2);
    let repulsion_radius_sq = 0.45_f64.powi(2);
    for side in sides {
        grid.for_each_unordered_neighbor_pair(
            side,
            PairOptions {
                radius_cells: 1,
                radius_sq: Some(tactical_radius_sq),
            },
            |_| true,
            |pair| {
                let left = pair.left_index;
                let right = pair.right_index;
                let left_weight = browser_truthy_weight(ally_weights[left]);
                let right_weight = browser_truthy_weight(ally_weights[right]);
                if !excluded[left] {
                    results[left].last_ally_count += right_weight;
                }
                if !excluded[right] {
                    results[right].last_ally_count += left_weight;
                }
                if units[left].kind == UnitKind::Armor
                    && !excluded[left]
                    && can_support_armor[right]
                    && pair.distance_sq < tactical_radius_sq
                {
                    results[left].armor_supported = true;
                    results[left].armor_support_last_tick = Some(tick);
                }
                if units[right].kind == UnitKind::Armor
                    && !excluded[right]
                    && can_support_armor[left]
                    && pair.distance_sq < tactical_radius_sq
                {
                    results[right].armor_supported = true;
                    results[right].armor_support_last_tick = Some(tick);
                }
                if pair.distance_sq >= repulsion_radius_sq || pair.distance_sq <= 0.000_01 {
                    return;
                }
                let distance = pair.distance_sq.sqrt();
                let delta_lat = units[left].lat - units[right].lat;
                let delta_lng = wrapped_longitude_delta(units[right].lng, units[left].lng);
                let repulsion_scale = if units[left].task_force_key.is_some()
                    && units[left].task_force_key == units[right].task_force_key
                {
                    0.35
                } else {
                    1.0
                };
                if !excluded[left] {
                    add_repulsion(
                        &mut results[left].repulsion,
                        delta_lat / distance * repulsion_scale,
                        delta_lng / distance * repulsion_scale,
                    );
                }
                if !excluded[right] {
                    add_repulsion(
                        &mut results[right].repulsion,
                        -delta_lat / distance * repulsion_scale,
                        -delta_lng / distance * repulsion_scale,
                    );
                }
            },
        );
    }
    for result in &mut results {
        if result.last_ally_count == 0.0 {
            result.last_ally_count = 1.0;
        }
        if !result.last_ally_count.is_finite()
            || result
                .repulsion
                .is_some_and(|vector| !vector.lat.is_finite() || !vector.lng.is_finite())
        {
            return Err(BattlefieldError::InvalidUnit(result.unit_id));
        }
    }
    let groups = groups
        .into_iter()
        .map(|(key, group)| (key, group.finish(key.0, key.1)))
        .collect::<BTreeMap<_, _>>();
    if groups.values().any(|group| {
        !group.lat.is_finite()
            || !group.lng.is_finite()
            || !group.dir_lat.is_finite()
            || !group.dir_lng.is_finite()
    }) {
        return Err(BattlefieldError::InvalidBattlefieldState(
            "cohesion group accumulation overflowed",
        ));
    }
    Ok(BattlefieldLocalTacticsResult {
        schema_version: BATTLEFIELD_SCHEMA_VERSION,
        tick,
        groups,
        units: results,
    })
}

/// Optionally blend post-AI headings with the prepass's cohesion, alignment,
/// and pair-once repulsion, preserving input order and unit IDs.
pub fn apply_cohesion_and_repulsion(
    config: BattlefieldConfig,
    tactics: &BattlefieldLocalTacticsResult,
    directions: &[BattlefieldDirectionInput],
) -> Result<Vec<BattlefieldDirectionResult>, BattlefieldError> {
    validate_config(config)?;
    let by_id = tactics
        .units
        .iter()
        .map(|unit| (unit.unit_id, unit))
        .collect::<BTreeMap<_, _>>();
    if by_id.len() != tactics.units.len() {
        return Err(BattlefieldError::BattlefieldUnitCoverage);
    }
    let mut seen = BTreeSet::new();
    let mut output = Vec::with_capacity(directions.len());
    for direction in directions {
        if !seen.insert(direction.unit_id) {
            return Err(BattlefieldError::DuplicateUnit(direction.unit_id));
        }
        let Some(local) = by_id.get(&direction.unit_id) else {
            return Err(BattlefieldError::BattlefieldUnitCoverage);
        };
        if !direction.dir_lat.is_finite() || !direction.dir_lng.is_finite() {
            return Err(BattlefieldError::InvalidUnit(direction.unit_id));
        }
        let group = tactics.groups.get(&(local.side, local.cohesion_group));
        let mut lat = direction.dir_lat;
        let mut lng = direction.dir_lng;
        if let Some(group) = group
            && !direction.at_sea
            && !direction.active_retreat
            && !direction.occupation_garrison_holding
        {
            let delta_lat = group.lat - local.lat;
            let delta_lng = group.lng - local.lng;
            let distance = delta_lat.hypot(delta_lng);
            if distance > 0.1 {
                let strength = if direction.is_plan_unit { 0.025 } else { 0.06 };
                lat += delta_lat / distance * strength;
                lng += delta_lng / distance * strength;
            }
            if group.dir_lat.abs() > 0.01 || group.dir_lng.abs() > 0.01 {
                let strength = if direction.is_plan_unit { 0.1 } else { 0.25 };
                lat += group.dir_lat * strength;
                lng += group.dir_lng * strength;
            }
            normalize_direction(&mut lat, &mut lng);
        }
        if let Some(repulsion) = local.repulsion
            && !direction.active_retreat
            && !direction.occupation_garrison_holding
        {
            let magnitude = repulsion.lat.hypot(repulsion.lng);
            if magnitude > 0.0 {
                let strength = if direction.is_plan_unit { 0.25 } else { 0.4 };
                lat = lat * (1.0 - strength) + repulsion.lat / magnitude * strength;
                lng = lng * (1.0 - strength) + repulsion.lng / magnitude * strength;
                normalize_direction(&mut lat, &mut lng);
            }
        }
        if !lat.is_finite() || !lng.is_finite() {
            return Err(BattlefieldError::InvalidUnit(direction.unit_id));
        }
        output.push(BattlefieldDirectionResult {
            unit_id: direction.unit_id,
            dir_lat: lat,
            dir_lng: lng,
        });
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug, Default)]
struct GroupAccumulator {
    lat: f64,
    lng: f64,
    dir_lat: f64,
    dir_lng: f64,
    count: usize,
}

impl GroupAccumulator {
    fn add(&mut self, unit: BattlefieldLocalUnitInput) {
        self.lat += unit.lat;
        self.lng += unit.lng;
        self.dir_lat += unit.previous_dir_lat;
        self.dir_lng += unit.previous_dir_lng;
        self.count += 1;
    }

    fn finish(self, side: SideKey, group: u8) -> BattlefieldCohesionGroup {
        let divisor = self.count.max(1) as f64;
        BattlefieldCohesionGroup {
            side,
            group,
            lat: self.lat / divisor,
            lng: self.lng / divisor,
            dir_lat: self.dir_lat / divisor,
            dir_lng: self.dir_lng / divisor,
            count: self.count,
        }
    }
}

fn browser_truthy_weight(weight: f64) -> f64 {
    if weight == 0.0 { 1.0 } else { weight }
}

fn add_repulsion(target: &mut Option<BattlefieldVector>, lat: f64, lng: f64) {
    let vector = target.get_or_insert(BattlefieldVector::default());
    vector.lat += lat;
    vector.lng += lng;
}

fn normalize_direction(lat: &mut f64, lng: &mut f64) {
    let magnitude = lat.hypot(*lng);
    if magnitude > 0.0 {
        *lat /= magnitude;
        *lng /= magnitude;
    }
}

/// Resolve every live unit without mutating the supplied simulation or maps.
pub fn resolve_battlefield_tick(
    config: BattlefieldConfig,
    input: BattlefieldTickInput<'_>,
) -> Result<BattlefieldTickResult, BattlefieldError> {
    validate_config(config)?;
    validate_maps(input.maps)?;
    validate_units(input.units, input.maps)?;
    if input.hostile_controlled_land_by_side.len() != input.maps.hostility.max_sides
        || input
            .hostile_controlled_land_by_side
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(BattlefieldError::InvalidHostileLandTotals);
    }

    // This intentionally mirrors the browser/native hand-off's side-zero and
    // side-one test, including its behavior in a topology with more sides.
    let mut side_zero_strength = 0.0;
    let mut side_one_strength = 0.0;
    for unit in input.units {
        if !unit.counts_for_capitulation {
            continue;
        }
        if unit.side == 0 {
            side_zero_strength += unit.formation_strength;
        } else if unit.side == 1 {
            side_one_strength += unit.formation_strength;
        }
    }
    let unopposed_multiplier = if (side_zero_strength > 0.0 && side_one_strength == 0.0)
        || (side_one_strength > 0.0 && side_zero_strength == 0.0)
    {
        6.0
    } else {
        1.0
    };

    let mut units = Vec::with_capacity(input.units.len());
    for unit in input.units {
        let result = resolve_unit(
            config,
            input,
            *unit,
            unopposed_multiplier,
            input.hostile_controlled_land_by_side[usize::from(unit.side)],
        )?;
        units.push(result);
    }
    Ok(BattlefieldTickResult {
        schema_version: BATTLEFIELD_SCHEMA_VERSION,
        tick: input.tick,
        frame: input.frame,
        units,
    })
}

fn resolve_unit(
    config: BattlefieldConfig,
    input: BattlefieldTickInput<'_>,
    unit: BattlefieldUnitInput,
    unopposed_multiplier: f64,
    target_land_size: f64,
) -> Result<BattlefieldUnitResult, BattlefieldError> {
    let maps = input.maps;
    let index = maps.world.grid_index(unit.lat, unit.lng);
    let at_sea = index.is_none_or(|cell| maps.world.land_mask[cell] == 0);
    let raw_mountain_intensity = if input.mountains_enabled {
        index
            .and_then(|cell| maps.terrain_intensity.map(|terrain| terrain[cell]))
            .map_or(0.0, f64::from)
    } else {
        0.0
    };
    if !raw_mountain_intensity.is_finite() || raw_mountain_intensity > 1.0 {
        return Err(BattlefieldError::InvalidUnit(unit.id));
    }
    let mountain_intensity = raw_mountain_intensity.max(0.0);
    let mountain = mountain_intensity > 0.0;
    let current_cell_is_urban = index
        .and_then(|cell| maps.urban_cells.map(|urban| urban[cell]))
        .is_some_and(|urban| urban != 0);
    let combat_urban = current_cell_is_urban || (!at_sea && unit.near_sovereign_city);
    let cell = BattlefieldCellState {
        index,
        at_sea,
        mountain_intensity,
        mountain,
        current_cell_urban: current_cell_is_urban,
        combat_urban,
    };

    let encircled = unit_is_encircled(config, maps, unit, cell);
    let encircled_ticks = if encircled {
        unit.encircled_ticks.saturating_add(1)
    } else {
        0
    };

    let (mut dealt_multiplier, mut taken_multiplier, mut terrain_speed_multiplier) =
        country_combat_primitives(unit);
    if mountain {
        terrain_speed_multiplier *= 1.0 - 0.65 * mountain_intensity;
        dealt_multiplier *= 1.0 - 0.4 * mountain_intensity;
        taken_multiplier *= 1.0 - 0.4 * mountain_intensity;
    }
    if unit.is_alpenjager {
        if mountain {
            terrain_speed_multiplier *= config.alpen_mountain_speed_multiplier;
        }
        dealt_multiplier *= config.alpen_combat_multiplier;
        taken_multiplier *= 1.0 / config.alpen_combat_multiplier;
    }
    if encircled
        && !matches!(
            unit.country.combat_buff,
            BattlefieldBuff::Buff | BattlefieldBuff::Super
        )
    {
        dealt_multiplier *= encirclement_combat_multiplier(encircled_ticks);
        taken_multiplier *= 4.0;
    }

    let mut defense_bonus = 1.0;
    if !at_sea {
        if let Some(cell) = index {
            if maps.de_jure[cell] == unit.sovereign {
                defense_bonus *= 0.65;
            }
            // `getControlValue` returns zero outside active theater land (2).
            let control = if maps.world.land_mask[cell] == 2 {
                f64::from(maps.occupation[cell])
            } else {
                0.0
            };
            if maps.world_control[cell] == unit.sovereign && control.abs() < 0.2 {
                defense_bonus *= 0.85;
            }
        }
        if unit.near_sovereign_city {
            defense_bonus *= 0.45;
        }
    }

    let own_influence = index.map_or(0.0, |cell| {
        f64::from(maps.side_influence[usize::from(unit.side)][cell])
    });
    if !own_influence.is_finite() || own_influence < 0.0 {
        return Err(BattlefieldError::InvalidUnit(unit.id));
    }
    let land_speed_multiplier = if !at_sea
        && index.is_some_and(|cell| maps.dominant_side[cell] == unit.side as i16)
        && own_influence > 0.5
    {
        1.8
    } else {
        1.2
    };
    let owner = index.map_or(0, |cell| maps.world_control[cell]);
    let in_neutral = !at_sea && owner > 0 && !maps.country_to_side.contains_key(&owner);
    let mut base_speed = if at_sea {
        config.unit_naval_speed
    } else {
        config.unit_speed
    };
    if unit.kind == UnitKind::Armor {
        base_speed *= armor_speed_multiplier(at_sea, mountain, current_cell_is_urban);
    }
    let movement = ResolvedMovementModifiers {
        terrain_speed_multiplier,
        speed_multiplier: land_speed_multiplier
            * unit.country.ai_speed_multiplier
            * config.native_speed_scale,
        plan_speed_multiplier: 1.0,
        neutral_penalty: if in_neutral { 0.15 } else { 1.0 },
        push_readiness: 1.0,
    };
    let combat = ResolvedCombatModifiers {
        dealt_multiplier,
        taken_multiplier,
        defense_bonus,
        long_war_defense: if input.frame > config.long_war_frame_threshold {
            config.long_war_defense_multiplier
        } else {
            1.0
        },
        mountain,
        urban: combat_urban,
        current_cell_mountain: Some(mountain),
        current_cell_urban: Some(current_cell_is_urban),
    };

    let mut influence_radius = config.influence_radius;
    let mut influence_multiplier = 1.0;
    if mountain {
        influence_radius *= 1.0 - mountain_intensity * 0.65;
        influence_multiplier *= 1.0 - mountain_intensity * 0.5;
    }
    match unit.country.influence_buff {
        BattlefieldBuff::None => {}
        BattlefieldBuff::Buff => influence_multiplier = 2.5,
        BattlefieldBuff::Super => influence_multiplier = 8.0,
        BattlefieldBuff::Godly => {
            influence_multiplier = 45.0;
            influence_radius *= 0.5;
        }
        BattlefieldBuff::Weakened => influence_multiplier = 0.7,
        BattlefieldBuff::Crippled => influence_multiplier = 0.4,
    }
    let attack_factor = 1.0 + unit.country.attack_buff_percent / 100.0;
    if attack_factor > 0.0 {
        influence_multiplier *= attack_factor;
    }
    if unit.victory_boost_ticks > 0 {
        influence_multiplier *= 3.0;
        influence_radius *= 1.4;
    }
    if at_sea {
        influence_multiplier *= 0.4;
    }
    influence_multiplier *= match unit.kind {
        UnitKind::Armor => armor_influence_multiplier(unit.armor_supported),
        UnitKind::Army => unit.formation_strength,
    };
    let concentration_source = if unit.last_ally_count == 0.0 {
        1.0
    } else {
        unit.last_ally_count
    };
    let influence = BattlefieldInfluenceModifiers {
        radius: influence_radius,
        delta: config.influence_rate * unopposed_multiplier * influence_multiplier,
        concentration_bonus: (concentration_source / 5.0).min(2.5),
        eligible: active_combat_influence_eligible(
            input.frame,
            unit.last_combat_frame,
            config.active_combat_exclusion_frames,
        ),
    };

    let attrition = calculate_attrition(
        config,
        maps,
        unit,
        input.frame,
        target_land_size,
        combat.taken_multiplier,
    );

    if !result_numbers_are_valid(base_speed, movement, combat, influence)
        || !attrition.damage.is_finite()
        || attrition.damage < 0.0
    {
        return Err(BattlefieldError::InvalidUnit(unit.id));
    }
    Ok(BattlefieldUnitResult {
        unit_id: unit.id,
        cell,
        encircled,
        encircled_ticks,
        base_speed,
        movement,
        combat,
        influence,
        attrition,
    })
}

fn country_combat_primitives(unit: BattlefieldUnitInput) -> (f64, f64, f64) {
    let mut dealt = 1.0;
    let mut taken = 1.0;
    let mut speed = 1.0;
    if unit.victory_boost_ticks > 0 {
        dealt *= 1.4;
        speed *= 1.3;
    }
    if unit.country.capital_lost {
        dealt *= 0.8;
        taken *= 1.15;
        speed *= 0.9;
    }
    match unit.country.war_phase {
        BattlefieldWarPhase::Collapsing => dealt *= 0.7,
        BattlefieldWarPhase::Advancing if !unit.country.conquest_mode => dealt *= 1.15,
        BattlefieldWarPhase::Stable | BattlefieldWarPhase::Advancing => {}
    }
    match unit.country.combat_buff {
        BattlefieldBuff::None => {}
        BattlefieldBuff::Buff => (dealt, taken, speed) = (2.5, 0.6, 1.3),
        BattlefieldBuff::Super => (dealt, taken, speed) = (10.0, 0.2, 1.8),
        BattlefieldBuff::Godly => (dealt, taken, speed) = (40.0, 0.015, 2.2),
        BattlefieldBuff::Weakened => (dealt, taken, speed) = (0.7, 1.4, 0.7),
        BattlefieldBuff::Crippled => (dealt, taken, speed) = (0.4, 2.5, 0.7),
    }
    let attack_factor = 1.0 + unit.country.attack_buff_percent / 100.0;
    if attack_factor > 0.0 {
        dealt *= attack_factor;
    }
    let defense_factor = 1.0 + unit.country.defense_buff_percent / 100.0;
    if defense_factor > 0.01 {
        taken *= 1.0 / defense_factor;
    }
    (dealt, taken, speed)
}

fn encirclement_combat_multiplier(encircled_ticks: u64) -> f64 {
    if encircled_ticks > 180 {
        0.15
    } else if encircled_ticks > 60 {
        0.2
    } else {
        0.25
    }
}

fn unit_is_encircled(
    config: BattlefieldConfig,
    maps: BattlefieldMapView<'_>,
    unit: BattlefieldUnitInput,
    cell: BattlefieldCellState,
) -> bool {
    if cell.at_sea
        || matches!(
            unit.country.combat_buff,
            BattlefieldBuff::Buff | BattlefieldBuff::Super
        )
    {
        return false;
    }
    let Some(index) = cell.index else {
        return false;
    };
    let radius_cells = (config.encirclement_radius / maps.world.grid_res).round() as isize;
    let diagonal = (radius_cells as f64 * 0.7).round() as isize;
    let offsets = [
        (0, radius_cells),
        (0, -radius_cells),
        (radius_cells, 0),
        (-radius_cells, 0),
        (diagonal, diagonal),
        (-diagonal, -diagonal),
        (diagonal, -diagonal),
        (-diagonal, diagonal),
    ];
    let row = (index / maps.world.width) as isize;
    let column = (index % maps.world.width) as isize;
    let mut enemy_count = 0_u8;
    for (column_offset, row_offset) in offsets {
        let next_row = row + row_offset;
        let next_column = column + column_offset;
        if next_row < 0
            || next_row >= maps.world.height as isize
            || next_column < 0
            || next_column >= maps.world.width as isize
        {
            continue;
        }
        let next = next_row as usize * maps.world.width + next_column as usize;
        if maps.world.land_mask[next] > 0
            && sides_are_hostile(maps.hostility, unit.side, maps.dominant_side[next])
        {
            enemy_count += 1;
        }
    }
    // Browser uses `enemyCount / offsets.length > 0.875`, which for eight
    // samples requires all eight even when an out-of-bounds sample was skipped.
    f64::from(enemy_count) / 8.0 > 0.875
}

fn sides_are_hostile(hostility: HostilityMatrix<'_>, left: SideKey, right: i16) -> bool {
    if right < 0 || usize::from(left) == right as usize {
        return false;
    }
    let left = usize::from(left);
    let right = right as usize;
    if left >= hostility.max_sides || right >= hostility.max_sides {
        return false;
    }
    hostility
        .relations
        .is_none_or(|relations| relations[left * hostility.max_sides + right] == 1)
}

fn validate_config(config: BattlefieldConfig) -> Result<(), BattlefieldError> {
    let values = [
        config.unit_speed,
        config.unit_naval_speed,
        config.influence_rate,
        config.influence_radius,
        config.encirclement_radius,
        config.armor_support_radius,
        config.alpen_mountain_speed_multiplier,
        config.alpen_combat_multiplier,
        config.native_speed_scale,
        config.long_war_defense_multiplier,
    ];
    if !values.into_iter().all(f64::is_finite)
        || config.unit_speed <= 0.0
        || config.unit_naval_speed <= 0.0
        || config.influence_rate < 0.0
        || config.influence_radius <= 0.0
        || config.encirclement_radius < 0.0
        || config.armor_support_radius <= 0.0
        || config.alpen_mountain_speed_multiplier <= 0.0
        || config.alpen_combat_multiplier <= 0.0
        || config.native_speed_scale < 0.0
        || config.long_war_defense_multiplier < 0.0
    {
        return Err(BattlefieldError::InvalidConfig);
    }
    Ok(())
}

fn country_primitives_are_valid(country: CountryBattlefieldPrimitives) -> bool {
    [
        country.attack_buff_percent,
        country.defense_buff_percent,
        country.ai_speed_multiplier,
    ]
    .into_iter()
    .all(f64::is_finite)
        && country.ai_speed_multiplier >= 0.0
}

fn validate_maps(maps: BattlefieldMapView<'_>) -> Result<(), BattlefieldError> {
    maps.world.validate()?;
    let cells = maps
        .world
        .width
        .checked_mul(maps.world.height)
        .ok_or(BattlefieldError::InvalidConfig)?;
    validate_map_length("world_control", maps.world_control.len(), cells)?;
    validate_map_length("de_jure", maps.de_jure.len(), cells)?;
    validate_map_length("dominant_side", maps.dominant_side.len(), cells)?;
    validate_map_length("occupation", maps.occupation.len(), cells)?;
    if let Some(terrain) = maps.terrain_intensity {
        validate_map_length("terrain_intensity", terrain.len(), cells)?;
    }
    if let Some(urban) = maps.urban_cells {
        validate_map_length("urban_cells", urban.len(), cells)?;
    }
    if maps.hostility.max_sides == 0 || maps.side_influence.len() != maps.hostility.max_sides {
        return Err(BattlefieldError::SideInfluenceRows);
    }
    for row in maps.side_influence {
        validate_map_length("side_influence", row.len(), cells)?;
    }
    if let Some(relations) = maps.hostility.relations {
        let expected = maps
            .hostility
            .max_sides
            .checked_mul(maps.hostility.max_sides)
            .ok_or(BattlefieldError::InvalidHostility)?;
        if relations.len() != expected {
            return Err(BattlefieldError::InvalidHostility);
        }
    }
    if maps
        .country_to_side
        .iter()
        .any(|(country, side)| *country == 0 || *side >= maps.hostility.max_sides)
    {
        return Err(BattlefieldError::InvalidCountryTopology);
    }
    Ok(())
}

fn validate_map_length(
    map: &'static str,
    actual: usize,
    expected: usize,
) -> Result<(), BattlefieldError> {
    if actual != expected {
        return Err(BattlefieldError::MapLength {
            map,
            expected,
            actual,
        });
    }
    Ok(())
}

fn validate_units(
    units: &[BattlefieldUnitInput],
    maps: BattlefieldMapView<'_>,
) -> Result<(), BattlefieldError> {
    let mut ids = BTreeSet::new();
    for unit in units {
        if !ids.insert(unit.id) {
            return Err(BattlefieldError::DuplicateUnit(unit.id));
        }
        let country = unit.country;
        if usize::from(unit.side) >= maps.hostility.max_sides
            || unit.sovereign == 0
            || maps.country_to_side.get(&unit.sovereign).copied() != Some(usize::from(unit.side))
            || ![
                unit.lat,
                unit.lng,
                unit.formation_strength,
                unit.last_ally_count,
                country.attack_buff_percent,
                country.defense_buff_percent,
                country.ai_speed_multiplier,
            ]
            .into_iter()
            .all(f64::is_finite)
            || unit.formation_strength < 0.0
            || unit.last_ally_count < 0.0
            || !country_primitives_are_valid(country)
        {
            return Err(BattlefieldError::InvalidUnit(unit.id));
        }
    }
    Ok(())
}

fn result_numbers_are_valid(
    base_speed: f64,
    movement: ResolvedMovementModifiers,
    combat: ResolvedCombatModifiers,
    influence: BattlefieldInfluenceModifiers,
) -> bool {
    [
        base_speed,
        movement.terrain_speed_multiplier,
        movement.speed_multiplier,
        movement.plan_speed_multiplier,
        movement.neutral_penalty,
        movement.push_readiness,
        combat.dealt_multiplier,
        combat.taken_multiplier,
        combat.defense_bonus,
        combat.long_war_defense,
        influence.radius,
        influence.delta,
        influence.concentration_bonus,
    ]
    .into_iter()
    .all(|value| value.is_finite() && value >= 0.0)
        && influence.radius > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: usize = 7;
    const HEIGHT: usize = 7;

    struct Maps {
        land: Vec<u8>,
        terrain: Vec<f32>,
        urban: Vec<u8>,
        world_control: Vec<u16>,
        de_jure: Vec<u16>,
        dominant: Vec<i16>,
        occupation: Vec<f32>,
        influence: Vec<Vec<f32>>,
        countries: BTreeMap<u16, usize>,
        hostility: Vec<u8>,
    }

    impl Maps {
        fn flat() -> Self {
            let cells = WIDTH * HEIGHT;
            Self {
                land: vec![1; cells],
                terrain: vec![0.0; cells],
                urban: vec![0; cells],
                world_control: vec![1; cells],
                de_jure: vec![1; cells],
                dominant: vec![0; cells],
                occupation: vec![0.0; cells],
                influence: vec![vec![0.0; cells], vec![0.0; cells]],
                countries: BTreeMap::from([(1, 0), (2, 1)]),
                hostility: vec![0, 1, 1, 0],
            }
        }

        fn view(&self) -> BattlefieldMapView<'_> {
            BattlefieldMapView {
                world: WorldGridView::new(1.0, WIDTH, HEIGHT, &self.land).unwrap(),
                terrain_intensity: Some(&self.terrain),
                urban_cells: Some(&self.urban),
                world_control: &self.world_control,
                de_jure: &self.de_jure,
                dominant_side: &self.dominant,
                occupation: &self.occupation,
                side_influence: &self.influence,
                country_to_side: &self.countries,
                hostility: HostilityMatrix::new(Some(&self.hostility), 2),
            }
        }
    }

    fn coordinate(cell: usize) -> (f64, f64) {
        let row = cell / WIDTH;
        let column = cell % WIDTH;
        (row as f64 - 90.0 + 0.5, column as f64 - 180.0 + 0.5)
    }

    fn army(id: u64, cell: usize) -> BattlefieldUnitInput {
        let (lat, lng) = coordinate(cell);
        BattlefieldUnitInput {
            id,
            side: 0,
            sovereign: 1,
            kind: UnitKind::Army,
            transport: false,
            lat,
            lng,
            formation_strength: 1.0,
            counts_for_capitulation: true,
            armor_supported: false,
            is_alpenjager: false,
            victory_boost_ticks: 0,
            encircled_ticks: 0,
            last_combat_frame: None,
            last_ally_count: 1.0,
            near_sovereign_city: false,
            country: CountryBattlefieldPrimitives::default(),
        }
    }

    fn resolve(maps: &Maps, unit: BattlefieldUnitInput) -> BattlefieldUnitResult {
        resolve_battlefield_tick(
            BattlefieldConfig {
                encirclement_radius: 1.0,
                ..BattlefieldConfig::default()
            },
            BattlefieldTickInput {
                tick: 10,
                frame: 100,
                mountains_enabled: true,
                maps: maps.view(),
                hostile_controlled_land_by_side: &[0.0, 0.0],
                units: &[unit],
            },
        )
        .unwrap()
        .units[0]
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1.0e-12,
            "actual {actual}, expected {expected}"
        );
    }

    #[test]
    fn resolves_live_cell_terrain_city_defense_and_friendly_speed() {
        let mut maps = Maps::flat();
        let center = 3 * WIDTH + 3;
        maps.land[center] = 2;
        maps.terrain[center] = 0.5;
        maps.urban[center] = 1;
        maps.occupation[center] = 0.1;
        maps.influence[0][center] = 0.75;
        let mut unit = army(7, center);
        unit.kind = UnitKind::Armor;
        unit.armor_supported = false;
        unit.near_sovereign_city = true;

        let result = resolve(&maps, unit);
        assert_eq!(result.cell.index, Some(center));
        assert!(!result.cell.at_sea);
        assert!(result.cell.mountain);
        assert!(result.cell.current_cell_urban);
        assert!(result.cell.combat_urban);
        assert_close(result.cell.mountain_intensity, 0.5);
        // Mountain takes precedence over urban for armor movement.
        assert_close(result.base_speed, 0.003 * 0.45);
        assert_close(result.movement.terrain_speed_multiplier, 0.675);
        assert_close(result.movement.speed_multiplier, 1.8);
        assert_close(result.combat.dealt_multiplier, 0.8);
        assert_close(result.combat.taken_multiplier, 0.8);
        assert_close(result.combat.defense_bonus, 0.65 * 0.85 * 0.45);
        assert_close(result.influence.radius, 0.4 * 0.675);
        // One side is unopposed and unsupported armor has quarter pressure.
        assert_close(result.influence.delta, 0.18 * 6.0 * 0.75 * 0.25);
    }

    #[test]
    fn armor_sea_precedence_and_army_formation_pressure_match_browser() {
        let mut maps = Maps::flat();
        let center = 3 * WIDTH + 3;
        maps.land[center] = 0;
        maps.terrain[center] = 1.0;
        maps.urban[center] = 1;
        let mut armor = army(1, center);
        armor.kind = UnitKind::Armor;
        armor.armor_supported = true;
        let sea = resolve(&maps, armor);
        assert!(sea.cell.at_sea);
        assert_close(sea.base_speed, 0.025 * 0.75);
        assert_close(sea.influence.delta, 0.18 * 6.0 * 0.5 * 0.4);

        maps.land[center] = 1;
        maps.terrain[center] = 0.0;
        let mut formation = army(2, center);
        formation.formation_strength = 2.25;
        let army_result = resolve(&maps, formation);
        assert_close(army_result.base_speed, 0.003);
        assert_close(army_result.influence.delta, 0.18 * 6.0 * 2.25);
    }

    #[test]
    fn sovereign_city_proximity_affects_combat_but_not_armor_movement() {
        let mut maps = Maps::flat();
        let center = 3 * WIDTH + 3;
        let mut armor = army(8, center);
        armor.kind = UnitKind::Armor;
        armor.near_sovereign_city = true;

        let near_city = resolve(&maps, armor);
        assert!(!near_city.cell.current_cell_urban);
        assert!(near_city.cell.combat_urban);
        assert!(near_city.combat.urban);
        assert_close(near_city.base_speed, 0.003 * 1.6);
        assert_close(near_city.combat.defense_bonus, 0.65 * 0.85 * 0.45);

        armor.near_sovereign_city = false;
        maps.urban[center] = 1;
        let on_city_cell = resolve(&maps, armor);
        assert!(on_city_cell.cell.current_cell_urban);
        assert!(on_city_cell.cell.combat_urban);
        assert_close(on_city_cell.base_speed, 0.003 * 0.75);
        // Merely occupying a city cell does not grant sovereign-city defense.
        assert_close(on_city_cell.combat.defense_bonus, 0.65 * 0.85);
    }

    #[test]
    fn encirclement_requires_all_eight_samples_and_advances_duration_first() {
        let mut maps = Maps::flat();
        let center = 3 * WIDTH + 3;
        for row in 2..=4 {
            for column in 2..=4 {
                if row != 3 || column != 3 {
                    maps.dominant[row * WIDTH + column] = 1;
                }
            }
        }
        let mut unit = army(9, center);
        unit.encircled_ticks = 60;
        let result = resolve(&maps, unit);
        assert!(result.encircled);
        assert_eq!(result.encircled_ticks, 61);
        assert_close(result.combat.dealt_multiplier, 0.2);
        assert_close(result.combat.taken_multiplier, 4.0);

        maps.dominant[2 * WIDTH + 2] = 0;
        let result = resolve(&maps, unit);
        assert!(!result.encircled);
        assert_eq!(result.encircled_ticks, 0);
        assert_close(result.combat.dealt_multiplier, 1.0);
    }

    #[test]
    fn buff_and_super_are_encirclement_immune_but_godly_is_not() {
        let mut maps = Maps::flat();
        let center = 3 * WIDTH + 3;
        for row in 2..=4 {
            for column in 2..=4 {
                if row != 3 || column != 3 {
                    maps.dominant[row * WIDTH + column] = 1;
                }
            }
        }
        let mut unit = army(3, center);
        unit.encircled_ticks = 180;
        unit.country.combat_buff = BattlefieldBuff::Buff;
        let immune = resolve(&maps, unit);
        assert!(!immune.encircled);
        assert_eq!(immune.encircled_ticks, 0);
        assert_close(immune.combat.dealt_multiplier, 2.5);

        unit.country.combat_buff = BattlefieldBuff::Godly;
        let godly = resolve(&maps, unit);
        assert!(godly.encircled);
        assert_eq!(godly.encircled_ticks, 181);
        assert_close(godly.combat.dealt_multiplier, 40.0 * 0.15);
        assert_close(godly.combat.taken_multiplier, 0.015 * 4.0);
    }

    #[test]
    fn country_buff_overrides_prior_combat_bases_but_influence_buff_is_distinct() {
        let maps = Maps::flat();
        let center = 3 * WIDTH + 3;
        let mut unit = army(4, center);
        unit.victory_boost_ticks = 1;
        unit.country = CountryBattlefieldPrimitives {
            combat_buff: BattlefieldBuff::Buff,
            influence_buff: BattlefieldBuff::Godly,
            attack_buff_percent: 20.0,
            defense_buff_percent: 25.0,
            capital_lost: true,
            war_phase: BattlefieldWarPhase::Collapsing,
            conquest_mode: false,
            ai_speed_multiplier: 1.5,
        };
        let result = resolve(&maps, unit);
        // Buff assignment replaces victory/capital/phase combat values, then
        // the continuous country sliders are applied.
        assert_close(result.combat.dealt_multiplier, 2.5 * 1.2);
        assert_close(result.combat.taken_multiplier, 0.6 / 1.25);
        assert_close(result.movement.terrain_speed_multiplier, 1.3);
        assert_close(result.movement.speed_multiplier, 1.2 * 1.5);
        assert_close(result.influence.radius, 0.4 * 0.5 * 1.4);
        assert_close(result.influence.delta, 0.18 * 6.0 * 45.0 * 1.2 * 3.0);
    }

    #[test]
    fn active_combat_exclusion_has_an_inclusive_five_frame_window() {
        assert!(active_combat_influence_eligible(100, None, 5));
        assert!(!active_combat_influence_eligible(100, Some(100), 5));
        assert!(!active_combat_influence_eligible(100, Some(95), 5));
        assert!(active_combat_influence_eligible(100, Some(94), 5));
        assert!(!active_combat_influence_eligible(100, Some(101), 5));
    }

    #[test]
    fn attrition_matches_sea_supply_victory_and_encirclement_gates() {
        let mut maps = Maps::flat();
        let corner = 0;
        maps.world_control.fill(2);
        maps.dominant.fill(1);
        maps.occupation[corner] = 0.5;
        let unit = army(20, corner);
        let collapsed = resolve(&maps, unit);
        assert!(!collapsed.encircled);
        assert!(collapsed.attrition.supply_collapsed);
        assert_eq!(collapsed.attrition.friendly_tiles, 0);
        assert!(collapsed.attrition.damage > 2.5);
        maps.land[corner] = 2;
        let active_theater = resolve(&maps, unit);
        assert!(active_theater.attrition.damage > collapsed.attrition.damage);

        let mut boosted = unit;
        boosted.victory_boost_ticks = 1;
        assert!(resolve(&maps, boosted).attrition.damage > 0.0);
        boosted.victory_boost_ticks = 2;
        assert_eq!(resolve(&maps, boosted).attrition.damage, 0.0);

        let center = 3 * WIDTH + 3;
        maps.dominant.fill(1);
        let mut encircled = army(21, center);
        encircled.encircled_ticks = 60;
        let pressure = resolve(&maps, encircled);
        assert!(pressure.encircled);
        assert_eq!(pressure.encircled_ticks, 61);
        assert!(pressure.attrition.damage > ATTRITION_DAMAGE * 5.0 * 1.5);

        maps.land[center] = 0;
        let mut naval = army(22, center);
        let sea = resolve(&maps, naval);
        assert_close(sea.attrition.damage, ATTRITION_DAMAGE * 3.0);
        naval.transport = true;
        assert_eq!(resolve(&maps, naval).attrition.damage, 0.0);
    }

    fn battlefield_state(
        countries: BTreeMap<u16, CountryBattlefieldPrimitives>,
        units: impl IntoIterator<Item = (u64, BattlefieldUnitState)>,
    ) -> BattlefieldRuntimeState {
        BattlefieldRuntimeState {
            config: BattlefieldConfig::default(),
            mountains_enabled: true,
            terrain_intensity: vec![0.0; WIDTH * HEIGHT],
            urban_centers: Vec::new(),
            countries,
            units: units.into_iter().collect(),
        }
    }

    fn local_unit(
        id: u64,
        sovereign: u16,
        kind: UnitKind,
        lat: f64,
        lng: f64,
        strength: f64,
    ) -> BattlefieldLocalUnitInput {
        BattlefieldLocalUnitInput {
            id,
            side: 0,
            sovereign,
            kind,
            lat,
            lng,
            formation_strength: strength,
            refuses_offense: false,
            previous_dir_lat: 0.0,
            previous_dir_lng: 0.0,
            task_force_key: None,
        }
    }

    #[test]
    fn armor_support_is_strict_at_radius_and_memory_includes_age_twelve() {
        let countries = BTreeMap::from([(1, CountryBattlefieldPrimitives::default())]);
        let armor = local_unit(1, 1, UnitKind::Armor, 0.0, 0.0, 1.0);
        let support = local_unit(2, 1, UnitKind::Army, 0.0, 0.6, 1.0);
        let mut state = battlefield_state(
            countries,
            [
                (
                    1,
                    BattlefieldUnitState {
                        armor_support_last_tick: Some(88),
                        ..BattlefieldUnitState::default()
                    },
                ),
                (2, BattlefieldUnitState::default()),
            ],
        );

        let boundary = resolve_local_tactics(100, &state, &[armor, support]).unwrap();
        assert!(boundary.units[0].armor_supported);
        assert_eq!(boundary.units[0].armor_support_last_tick, Some(88));

        state.units.get_mut(&1).unwrap().armor_support_last_tick = Some(87);
        let expired = resolve_local_tactics(100, &state, &[armor, support]).unwrap();
        assert!(!expired.units[0].armor_supported);
        assert_eq!(expired.units[0].armor_support_last_tick, Some(87));

        let mut inside = support;
        inside.lng = 0.599;
        let refreshed = resolve_local_tactics(100, &state, &[armor, inside]).unwrap();
        assert!(refreshed.units[0].armor_supported);
        assert_eq!(refreshed.units[0].armor_support_last_tick, Some(100));

        state.units.get_mut(&1).unwrap().local_tactics_excluded = true;
        let excluded = resolve_local_tactics(101, &state, &[armor, inside]).unwrap();
        assert!(!excluded.units[0].armor_supported);
        assert_eq!(excluded.units[0].armor_support_last_tick, Some(87));
    }

    #[test]
    fn local_ally_strength_uses_formation_and_browser_buff_weights() {
        let buffed = CountryBattlefieldPrimitives {
            influence_buff: BattlefieldBuff::Buff,
            ..CountryBattlefieldPrimitives::default()
        };
        let countries = BTreeMap::from([(1, buffed), (2, CountryBattlefieldPrimitives::default())]);
        let state = battlefield_state(
            countries,
            [
                (1, BattlefieldUnitState::default()),
                (2, BattlefieldUnitState::default()),
            ],
        );
        let first = local_unit(1, 1, UnitKind::Army, 0.0, 0.0, 2.0);
        let second = local_unit(2, 2, UnitKind::Army, 0.0, 0.5, 3.0);
        let result = resolve_local_tactics(10, &state, &[first, second]).unwrap();
        assert_close(result.units[0].last_ally_count, 2.0 + 3.0);
        assert_close(result.units[1].last_ally_count, 3.0 + 2.0 * 50.0);
    }

    #[test]
    fn shared_task_force_reduces_only_its_pairwise_repulsion() {
        let countries = BTreeMap::from([(1, CountryBattlefieldPrimitives::default())]);
        let state = battlefield_state(
            countries,
            [
                (1, BattlefieldUnitState::default()),
                (2, BattlefieldUnitState::default()),
            ],
        );
        let mut first = local_unit(1, 1, UnitKind::Army, 0.0, 0.0, 1.0);
        let mut second = local_unit(2, 1, UnitKind::Army, 0.0, 0.2, 1.0);
        first.task_force_key = Some(7);
        second.task_force_key = Some(7);
        let shared = resolve_local_tactics(10, &state, &[first, second]).unwrap();
        assert_close(shared.units[0].repulsion.unwrap().lng.abs(), 0.35);

        second.task_force_key = Some(8);
        let distinct = resolve_local_tactics(10, &state, &[first, second]).unwrap();
        assert_close(distinct.units[0].repulsion.unwrap().lng.abs(), 1.0);
    }

    #[test]
    fn cohesion_uses_saved_four_way_group_and_normalizes_post_ai_heading() {
        assert_eq!(cohesion_group(0.000_1), Some(0));
        assert_eq!(cohesion_group(0.001_1), Some(1));
        assert_eq!(cohesion_group(0.002_1), Some(2));
        assert_eq!(cohesion_group(0.003_1), Some(3));
        assert_eq!(cohesion_group(0.004_1), Some(0));

        let countries = BTreeMap::from([(1, CountryBattlefieldPrimitives::default())]);
        let state = battlefield_state(
            countries,
            [
                (
                    1,
                    BattlefieldUnitState {
                        cohesion_seed: 0.000_1,
                        ..BattlefieldUnitState::default()
                    },
                ),
                (
                    2,
                    BattlefieldUnitState {
                        cohesion_seed: 0.004_1,
                        ..BattlefieldUnitState::default()
                    },
                ),
            ],
        );
        let mut first = local_unit(1, 1, UnitKind::Army, 0.0, 0.0, 1.0);
        first.previous_dir_lng = 1.0;
        let mut second = local_unit(2, 1, UnitKind::Army, 1.0, 0.0, 1.0);
        second.previous_dir_lng = 1.0;
        let tactics = resolve_local_tactics(1, &state, &[first, second]).unwrap();
        let group = tactics.groups.get(&(0, 0)).unwrap();
        assert_eq!(group.count, 2);
        assert_close(group.lat, 0.5);
        assert_close(group.dir_lng, 1.0);

        let directions = apply_cohesion_and_repulsion(
            state.config,
            &tactics,
            &[BattlefieldDirectionInput {
                unit_id: 1,
                dir_lat: 1.0,
                dir_lng: 0.0,
                is_plan_unit: false,
                at_sea: false,
                active_retreat: false,
                occupation_garrison_holding: false,
            }],
        )
        .unwrap();
        assert_close(directions[0].dir_lat.hypot(directions[0].dir_lng), 1.0);
        assert!(directions[0].dir_lng > 0.0);
    }

    #[test]
    fn battlefield_state_validates_exact_grid_topology_city_and_unit_coverage() {
        let maps = Maps::flat();
        let mut state = battlefield_state(
            BTreeMap::from([
                (1, CountryBattlefieldPrimitives::default()),
                (2, CountryBattlefieldPrimitives::default()),
            ]),
            [(7, BattlefieldUnitState::default())],
        );
        let center = 3 * WIDTH + 3;
        let (lat, lng) = coordinate(center);
        state.urban_centers.push(BattlefieldUrbanCenter {
            id: 99,
            country_id: 1,
            cell: center,
            lat,
            lng,
        });
        state
            .validate(maps.view().world, 2, &maps.countries, &[7])
            .unwrap();
        assert_eq!(state.urban_cell_mask(maps.view().world).unwrap()[center], 1);
        assert!(state.near_sovereign_city(1, lat + 0.1, lng));

        state.terrain_intensity[0] = f32::NAN;
        assert!(matches!(
            state.validate(maps.view().world, 2, &maps.countries, &[7]),
            Err(BattlefieldError::InvalidBattlefieldState(_))
        ));
        state.terrain_intensity[0] = 0.0;
        assert_eq!(
            state.validate(maps.view().world, 2, &maps.countries, &[8]),
            Err(BattlefieldError::BattlefieldUnitCoverage)
        );
    }

    #[test]
    fn edge_cells_cannot_pass_the_fixed_eight_sample_denominator() {
        let mut maps = Maps::flat();
        maps.dominant.fill(1);
        let result = resolve(&maps, army(5, 0));
        assert!(!result.encircled);
        assert_eq!(result.encircled_ticks, 0);
    }

    #[test]
    fn invalid_inputs_fail_before_any_partial_result_is_returned() {
        let maps = Maps::flat();
        let mut duplicate = army(6, 0);
        duplicate.lat = f64::NAN;
        let units = [army(6, 0), duplicate];
        assert_eq!(
            resolve_battlefield_tick(
                BattlefieldConfig::default(),
                BattlefieldTickInput {
                    tick: 1,
                    frame: 1,
                    mountains_enabled: true,
                    maps: maps.view(),
                    hostile_controlled_land_by_side: &[0.0, 0.0],
                    units: &units,
                },
            ),
            Err(BattlefieldError::DuplicateUnit(6))
        );

        let mut bad_maps = Maps::flat();
        bad_maps.urban.pop();
        assert!(matches!(
            resolve_battlefield_tick(
                BattlefieldConfig::default(),
                BattlefieldTickInput {
                    tick: 1,
                    frame: 1,
                    mountains_enabled: true,
                    maps: bad_maps.view(),
                    hostile_controlled_land_by_side: &[0.0, 0.0],
                    units: &[army(7, 0)],
                },
            ),
            Err(BattlefieldError::MapLength {
                map: "urban_cells",
                ..
            })
        ));
    }
}
