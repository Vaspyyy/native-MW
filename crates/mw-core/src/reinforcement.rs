//! Deterministic reinforcement and logistics state.
//!
//! This module owns the persisted replacement reserves and ID allocators.  A
//! pay cycle is staged against clones of every mutable input, validated, and
//! installed only after the complete result is valid.  Expected resource
//! bounds (personnel, marker, and airfield capacity) are reported in the
//! outcome rather than treated as corrupt state.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    air::{AirPowerState, AirRole, AirTargetKind, AirWing, AirWingState, Airfield},
    combat::{CombatUnit, UNIT_HEALTH, UnitKind},
    economy::EconomyState,
    simulation::SimulationUnit,
};

pub const REINFORCEMENT_SCHEMA_VERSION: &str = "native-reinforcement-v1";
pub const FIGHTER_OPERATIONS_PER_100: f64 = 1.0;
pub const STRIKE_OPERATIONS_PER_100: f64 = 1.5;
pub const FIGHTER_REPLACEMENT_COST: f64 = 0.15;
pub const STRIKE_REPLACEMENT_COST: f64 = 0.20;
pub const AIRCRAFT_REPLACEMENT_PERCENT: u32 = 1;
pub const AIR_WING_TARGET_SIZE: u32 = 24;
pub const MAX_WINGS_PER_ROLE_PER_COUNTRY: u32 = 8;
pub const MAX_LIVE_AIR_WING_MARKERS: usize = 256;
pub const AIRCREW_PER_AIRCRAFT: f64 = 1.0;
pub const MATERIAL_LOGISTICS_SCHEMA_VERSION: &str = "native-material-logistics-v1";
pub const ARMOR_REPLACEMENT_COST: f64 = 0.05;
pub const ARMOR_REPLACEMENT_PERCENT: u64 = 1;
pub const ARMOR_GROUP_TARGET_SIZE: u64 = 100;
pub const MAX_ARMOR_GROUPS_PER_COUNTRY: u64 = 12;
pub const ARMOR_CREW_PER_VEHICLE: f64 = 2.0;
pub const AIRFIELD_REPAIR_COST: f64 = 2.0;
pub const AIRFIELD_REPAIR_PER_CYCLE: f64 = 25.0;
pub const AIRFIELD_CAPTURE_REPAIR_CYCLES: u64 = 2;
pub const MAX_ARMOR_CAPACITY: u64 = 50_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaterialLogisticsCountry {
    pub country_id: u16,
    pub armor_capacity: u64,
    pub reserve_armor: u64,
    pub armor_quality: f64,
    pub armor_replacement_spent: f64,
    pub airfield_repair_spent: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaterialLogisticsState {
    pub schema: String,
    /// Strictly ordered, complete stable-country records.
    pub countries: Vec<MaterialLogisticsCountry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArmorFormationCreationOutcome {
    pub desired_formations: u64,
    pub formations_before: u64,
    pub formations_after: u64,
    pub created_unit_id: Option<u64>,
    pub created_equipment: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountryMaterialPayCycleOutcome {
    pub country_id: u16,
    pub airfields_repaired: u32,
    pub airfield_repair_spent: f64,
    pub armor_purchased: u64,
    pub armor_reinforced: u64,
    pub armor_creation: ArmorFormationCreationOutcome,
    pub air: CountryAirPayCycleOutcome,
    pub evacuated_aircraft: u64,
    pub lost_aircraft: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaterialPayCycleOutcome {
    /// Country outcomes are strictly ordered by country ID.
    pub countries: Vec<CountryMaterialPayCycleOutcome>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReinforcementCountry {
    pub country_id: u16,
    pub fighter_capacity: u32,
    pub strike_capacity: u32,
    pub reserve_fighters: u32,
    pub reserve_strike: u32,
    pub air_operations_due: f64,
    pub operations_coverage: f64,
    pub replacement_spent: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReinforcementState {
    pub schema: String,
    /// The next never-issued land-unit ID. Air settlement does not consume it.
    pub next_unit_id: u64,
    /// The next never-issued air-wing ID.
    pub next_air_wing_id: u64,
    /// Strictly ordered, complete stable-country records.
    pub countries: Vec<ReinforcementCountry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AirWingCreationStatus {
    AtDesiredCount,
    Created,
    NoReserve,
    NoPersonnel,
    GlobalMarkerCap,
    NoEligibleAirfield,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirWingCreationOutcome {
    pub role: AirRole,
    pub desired_wings: u32,
    pub wings_before: u32,
    pub wings_after: u32,
    /// Browser parity deliberately bounds creation to one marker per role and
    /// country per cycle. This remains non-zero when another cycle is needed.
    pub remaining_missing_wings: u32,
    pub created_wing_id: Option<u64>,
    pub created_aircraft: u32,
    pub status: AirWingCreationStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountryAirPayCycleOutcome {
    pub country_id: u16,
    pub operations_spent: f64,
    pub fighters_purchased: u32,
    pub strike_purchased: u32,
    pub fighters_reinforced: u32,
    pub strike_reinforced: u32,
    pub wing_creation: Vec<AirWingCreationOutcome>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirPayCycleOutcome {
    /// Country outcomes are strictly ordered by country ID.
    pub countries: Vec<CountryAirPayCycleOutcome>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ReinforcementError {
    #[error("invalid reinforcement state: {0}")]
    InvalidState(&'static str),
    #[error("invalid reinforcement world: {0}")]
    InvalidWorld(&'static str),
    #[error("reinforcement arithmetic overflow: {0}")]
    Overflow(&'static str),
    #[error("invalid air state: {0}")]
    Air(&'static str),
}

impl MaterialLogisticsState {
    /// Construct a canonical material state from stable country profiles. Live
    /// armor stays deployed and replacement reserves start empty.
    pub fn bootstrap(
        units: &[SimulationUnit],
        armor_profiles: &BTreeMap<u16, (u64, f64)>,
        country_to_side: &BTreeMap<u16, usize>,
        side_count: usize,
    ) -> Result<Self, ReinforcementError> {
        validate_topology(country_to_side, side_count)?;
        if armor_profiles
            .keys()
            .copied()
            .ne(country_to_side.keys().copied())
        {
            return Err(ReinforcementError::InvalidWorld("armor profiles"));
        }
        let mut live_capacity = BTreeMap::<u16, u64>::new();
        for unit in units {
            let country_id = u16::try_from(unit.combat.sovereign)
                .map_err(|_| ReinforcementError::InvalidWorld("unit country"))?;
            let Some(&side) = country_to_side.get(&country_id) else {
                return Err(ReinforcementError::InvalidWorld("unit country"));
            };
            if unit.combat.side as usize != side {
                return Err(ReinforcementError::InvalidWorld("unit side"));
            }
            if unit.combat.kind == UnitKind::Armor {
                let capacity = live_capacity.entry(country_id).or_default();
                *capacity = capacity
                    .checked_add(unit.combat.max_equipment.max(unit.combat.equipment))
                    .ok_or(ReinforcementError::Overflow("armor capacity"))?;
            }
        }
        let state = Self {
            schema: MATERIAL_LOGISTICS_SCHEMA_VERSION.to_owned(),
            countries: country_to_side
                .keys()
                .copied()
                .map(|country_id| {
                    let (profile_capacity, quality) = armor_profiles[&country_id];
                    MaterialLogisticsCountry {
                        country_id,
                        armor_capacity: profile_capacity
                            .max(live_capacity.get(&country_id).copied().unwrap_or(0))
                            .min(MAX_ARMOR_CAPACITY),
                        reserve_armor: 0,
                        armor_quality: quality.clamp(0.0, 100.0),
                        armor_replacement_spent: 0.0,
                        airfield_repair_spent: 0.0,
                    }
                })
                .collect(),
        };
        state.validate(units, country_to_side, side_count)?;
        Ok(state)
    }

    pub fn validate(
        &self,
        units: &[SimulationUnit],
        country_to_side: &BTreeMap<u16, usize>,
        side_count: usize,
    ) -> Result<(), ReinforcementError> {
        if self.schema != MATERIAL_LOGISTICS_SCHEMA_VERSION {
            return Err(ReinforcementError::InvalidState(
                "material logistics schema",
            ));
        }
        validate_topology(country_to_side, side_count)?;
        if self.countries.len() != country_to_side.len()
            || !strictly_ordered(self.countries.iter().map(|country| country.country_id))
            || self
                .countries
                .iter()
                .map(|country| country.country_id)
                .ne(country_to_side.keys().copied())
        {
            return Err(ReinforcementError::InvalidState(
                "material countries must exactly cover stable topology in ID order",
            ));
        }
        let mut inventory = country_to_side
            .keys()
            .copied()
            .map(|country_id| (country_id, 0_u64))
            .collect::<BTreeMap<_, _>>();
        for unit in units {
            let country_id = u16::try_from(unit.combat.sovereign)
                .map_err(|_| ReinforcementError::InvalidWorld("unit country"))?;
            let Some(&side) = country_to_side.get(&country_id) else {
                return Err(ReinforcementError::InvalidWorld("unit country"));
            };
            if unit.combat.side as usize != side {
                return Err(ReinforcementError::InvalidWorld("unit side"));
            }
            if unit.combat.kind == UnitKind::Armor {
                let count = inventory
                    .get_mut(&country_id)
                    .expect("unit topology was validated");
                *count = count
                    .checked_add(unit.combat.equipment)
                    .ok_or(ReinforcementError::Overflow("armor inventory"))?;
            }
        }
        for country in &self.countries {
            if country.country_id == 0
                || country.armor_capacity > MAX_ARMOR_CAPACITY
                || !country.armor_quality.is_finite()
                || !(0.0..=100.0).contains(&country.armor_quality)
                || !country.armor_replacement_spent.is_finite()
                || country.armor_replacement_spent < 0.0
                || !country.airfield_repair_spent.is_finite()
                || country.airfield_repair_spent < 0.0
                || inventory[&country.country_id]
                    .checked_add(country.reserve_armor)
                    .is_none_or(|total| total > country.armor_capacity)
            {
                return Err(ReinforcementError::InvalidState(
                    "material logistics country record",
                ));
            }
        }
        Ok(())
    }
}

impl ReinforcementState {
    /// Derive stable capacities from the sum of each country's wing maxima.
    /// Replacement reserves begin empty; current aircraft remain in wings.
    pub fn bootstrap(
        air_power: &AirPowerState,
        next_unit_id: u64,
        next_air_wing_id: u64,
        country_to_side: &BTreeMap<u16, usize>,
        side_count: usize,
    ) -> Result<Self, ReinforcementError> {
        air_power
            .validate()
            .map_err(|_| ReinforcementError::Air("validation"))?;
        validate_topology(country_to_side, side_count)?;
        validate_air_topology(air_power, country_to_side, side_count)?;

        let mut capacities = country_to_side
            .keys()
            .copied()
            .map(|country_id| (country_id, (0_u32, 0_u32)))
            .collect::<BTreeMap<_, _>>();
        for wing in &air_power.wings {
            let entry = capacities
                .get_mut(&wing.sovereign_country_id)
                .ok_or(ReinforcementError::InvalidWorld("wing country"))?;
            let capacity = match wing.role {
                AirRole::Fighter => &mut entry.0,
                AirRole::Strike => &mut entry.1,
            };
            *capacity = capacity
                .checked_add(wing.max_count)
                .ok_or(ReinforcementError::Overflow("aircraft capacity"))?;
        }

        let state = Self {
            schema: REINFORCEMENT_SCHEMA_VERSION.to_owned(),
            next_unit_id,
            next_air_wing_id,
            countries: capacities
                .into_iter()
                .map(
                    |(country_id, (fighter_capacity, strike_capacity))| ReinforcementCountry {
                        country_id,
                        fighter_capacity,
                        strike_capacity,
                        reserve_fighters: 0,
                        reserve_strike: 0,
                        air_operations_due: 0.0,
                        operations_coverage: 1.0,
                        replacement_spent: 0.0,
                    },
                )
                .collect(),
        };
        state.validate(air_power, country_to_side, side_count)?;
        Ok(state)
    }

    pub fn validate(
        &self,
        air_power: &AirPowerState,
        country_to_side: &BTreeMap<u16, usize>,
        side_count: usize,
    ) -> Result<(), ReinforcementError> {
        if self.schema != REINFORCEMENT_SCHEMA_VERSION {
            return Err(ReinforcementError::InvalidState("schema"));
        }
        if self.next_unit_id == 0 || self.next_air_wing_id == 0 {
            return Err(ReinforcementError::InvalidState(
                "next IDs must be positive",
            ));
        }
        if air_power
            .wings
            .last()
            .is_some_and(|wing| wing.id >= self.next_air_wing_id)
        {
            return Err(ReinforcementError::InvalidState(
                "next air-wing ID must exceed issued IDs",
            ));
        }
        validate_topology(country_to_side, side_count)?;
        air_power
            .validate()
            .map_err(|_| ReinforcementError::Air("validation"))?;
        validate_air_topology(air_power, country_to_side, side_count)?;

        if self.countries.len() != country_to_side.len()
            || !strictly_ordered(self.countries.iter().map(|country| country.country_id))
            || self
                .countries
                .iter()
                .map(|country| country.country_id)
                .ne(country_to_side.keys().copied())
        {
            return Err(ReinforcementError::InvalidState(
                "countries must exactly cover stable topology in ID order",
            ));
        }

        let mut inventory = country_to_side
            .keys()
            .copied()
            .map(|country_id| (country_id, (0_u32, 0_u32)))
            .collect::<BTreeMap<_, _>>();
        for wing in &air_power.wings {
            let entry = inventory
                .get_mut(&wing.sovereign_country_id)
                .ok_or(ReinforcementError::InvalidWorld("wing country"))?;
            let count = match wing.role {
                AirRole::Fighter => &mut entry.0,
                AirRole::Strike => &mut entry.1,
            };
            *count = count
                .checked_add(wing.count)
                .ok_or(ReinforcementError::Overflow("aircraft inventory"))?;
        }
        for country in &self.countries {
            if country.country_id == 0
                || !country.air_operations_due.is_finite()
                || country.air_operations_due < 0.0
                || !country.operations_coverage.is_finite()
                || !(0.0..=1.0).contains(&country.operations_coverage)
                || !country.replacement_spent.is_finite()
                || country.replacement_spent < 0.0
            {
                return Err(ReinforcementError::InvalidState("country record"));
            }
            let (fighters, strike) = inventory[&country.country_id];
            if fighters
                .checked_add(country.reserve_fighters)
                .is_none_or(|total| total > country.fighter_capacity)
                || strike
                    .checked_add(country.reserve_strike)
                    .is_none_or(|total| total > country.strike_capacity)
            {
                return Err(ReinforcementError::InvalidState(
                    "aircraft inventory exceeds capacity",
                ));
            }
        }
        Ok(())
    }

    /// Settle one complete air operations and replacement cycle atomically.
    ///
    /// All inputs are cloned before mutation. On any error, `self`, air power,
    /// economies, and personnel reserves remain byte-for-byte unchanged.
    pub fn settle_air_pay_cycle(
        &mut self,
        air_power: &mut AirPowerState,
        economies: &mut BTreeMap<u16, EconomyState>,
        personnel_reserves: &mut BTreeMap<usize, f64>,
        country_to_side: &BTreeMap<u16, usize>,
        side_count: usize,
    ) -> Result<AirPayCycleOutcome, ReinforcementError> {
        self.validate(air_power, country_to_side, side_count)?;
        validate_economies(economies, country_to_side)?;
        validate_personnel_reserves(personnel_reserves, side_count)?;

        let mut next_state = self.clone();
        let mut next_air = air_power.clone();
        let mut next_economies = economies.clone();
        let mut next_personnel = personnel_reserves.clone();
        let mut outcome = AirPayCycleOutcome {
            countries: Vec::with_capacity(next_state.countries.len()),
        };

        for country_index in 0..next_state.countries.len() {
            let country_id = next_state.countries[country_index].country_id;
            let economy = next_economies
                .get_mut(&country_id)
                .expect("economy topology was validated");

            // Browser settlement removes capitulated countries from combined-arms
            // maintenance entirely. Publish the grounded policy, but do not count
            // operations, spend treasury, purchase replacements, consume an old
            // reserve, claim personnel, or allocate a new marker.
            if economy.capitulated {
                let record = &mut next_state.countries[country_index];
                record.air_operations_due = 0.0;
                record.operations_coverage = 0.0;
                record.replacement_spent = 0.0;
                set_air_coverage(&mut next_air, country_id, 0.0)?;
                outcome.countries.push(CountryAirPayCycleOutcome {
                    country_id,
                    operations_spent: 0.0,
                    fighters_purchased: 0,
                    strike_purchased: 0,
                    fighters_reinforced: 0,
                    strike_reinforced: 0,
                    wing_creation: Vec::new(),
                });
                continue;
            }

            let side = country_to_side[&country_id];

            let (operational_fighters, operational_strike) =
                operational_aircraft(&next_air, country_id)?;
            let operations_due = f64::from(operational_fighters) / 100.0
                * FIGHTER_OPERATIONS_PER_100
                + f64::from(operational_strike) / 100.0 * STRIKE_OPERATIONS_PER_100;
            let can_operate = economy.payroll_coverage >= 0.999
                && economy.occupation_coverage >= 0.999
                && economy.arrears_cycles < 1.0;
            let operations_spent = if can_operate {
                economy.treasury.min(operations_due)
            } else {
                0.0
            };
            economy.treasury = (economy.treasury - operations_spent).max(0.0);
            let operations_coverage = if operations_due > 0.0 {
                operations_spent / operations_due
            } else {
                1.0
            };

            let record = &mut next_state.countries[country_index];
            record.air_operations_due = operations_due;
            record.operations_coverage = operations_coverage;
            record.replacement_spent = 0.0;
            set_air_coverage(&mut next_air, country_id, operations_coverage)?;

            let fully_funded = can_operate && operations_coverage >= 0.999;
            let mut fighters_purchased = 0;
            let mut strike_purchased = 0;
            if fully_funded {
                let (fighters, strike) = total_aircraft(&next_air, record)?;
                fighters_purchased = replacement_purchase(
                    record.fighter_capacity,
                    fighters,
                    FIGHTER_REPLACEMENT_COST,
                    economy.treasury,
                );
                let fighter_spent = f64::from(fighters_purchased) * FIGHTER_REPLACEMENT_COST;
                economy.treasury = (economy.treasury - fighter_spent).max(0.0);
                record.reserve_fighters = record
                    .reserve_fighters
                    .checked_add(fighters_purchased)
                    .ok_or(ReinforcementError::Overflow("fighter reserve"))?;

                strike_purchased = replacement_purchase(
                    record.strike_capacity,
                    strike,
                    STRIKE_REPLACEMENT_COST,
                    economy.treasury,
                );
                let strike_spent = f64::from(strike_purchased) * STRIKE_REPLACEMENT_COST;
                economy.treasury = (economy.treasury - strike_spent).max(0.0);
                record.reserve_strike = record
                    .reserve_strike
                    .checked_add(strike_purchased)
                    .ok_or(ReinforcementError::Overflow("strike reserve"))?;
                record.replacement_spent = fighter_spent + strike_spent;
            }

            let (fighters_reinforced, strike_reinforced) = reinforce_existing_wings(
                &mut next_air,
                record,
                country_id,
                side,
                &mut next_personnel,
            )?;
            let wing_creation = [AirRole::Fighter, AirRole::Strike]
                .into_iter()
                .map(|role| {
                    create_one_missing_wing(
                        &mut next_air,
                        record,
                        role,
                        country_id,
                        side,
                        &mut next_state.next_air_wing_id,
                        &mut next_personnel,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;

            outcome.countries.push(CountryAirPayCycleOutcome {
                country_id,
                operations_spent,
                fighters_purchased,
                strike_purchased,
                fighters_reinforced,
                strike_reinforced,
                wing_creation,
            });
        }

        next_state.validate(&next_air, country_to_side, side_count)?;
        validate_economies(&next_economies, country_to_side)?;
        validate_personnel_reserves(&next_personnel, side_count)?;
        *self = next_state;
        *air_power = next_air;
        *economies = next_economies;
        *personnel_reserves = next_personnel;
        Ok(outcome)
    }

    /// Settle the v11 material cycle atomically in browser order: air
    /// operations, airfield repair, armor/fighter/strike purchases, existing
    /// formation reinforcement, then at most one new armor and air marker per
    /// bounded category.
    #[allow(clippy::too_many_arguments)]
    pub fn settle_material_pay_cycle(
        &mut self,
        material: &mut MaterialLogisticsState,
        units: &mut Vec<SimulationUnit>,
        air_power: &mut AirPowerState,
        economies: &mut BTreeMap<u16, EconomyState>,
        personnel_reserves: &mut BTreeMap<usize, f64>,
        country_to_side: &BTreeMap<u16, usize>,
        side_count: usize,
        home_positions: &BTreeMap<u16, (f64, f64)>,
        max_units_per_side: u32,
    ) -> Result<MaterialPayCycleOutcome, ReinforcementError> {
        self.validate(air_power, country_to_side, side_count)?;
        material.validate(units, country_to_side, side_count)?;
        validate_economies(economies, country_to_side)?;
        validate_personnel_reserves(personnel_reserves, side_count)?;
        if max_units_per_side == 0 {
            return Err(ReinforcementError::InvalidWorld("land marker cap"));
        }

        let mut next_state = self.clone();
        let mut next_material = material.clone();
        let mut next_units = units.clone();
        let mut next_air = air_power.clone();
        let mut next_economies = economies.clone();
        let mut next_personnel = personnel_reserves.clone();
        let mut outcome = MaterialPayCycleOutcome {
            countries: Vec::with_capacity(next_state.countries.len()),
        };

        for country_index in 0..next_state.countries.len() {
            let country_id = next_state.countries[country_index].country_id;
            if next_material.countries[country_index].country_id != country_id {
                return Err(ReinforcementError::InvalidState(
                    "material and reinforcement country order",
                ));
            }
            let economy = next_economies
                .get_mut(&country_id)
                .expect("economy topology was validated");
            let side = country_to_side[&country_id];

            if economy.capitulated {
                let record = &mut next_state.countries[country_index];
                record.reserve_fighters = 0;
                record.reserve_strike = 0;
                record.air_operations_due = 0.0;
                record.operations_coverage = 0.0;
                record.replacement_spent = 0.0;
                let material_record = &mut next_material.countries[country_index];
                material_record.reserve_armor = 0;
                material_record.armor_replacement_spent = 0.0;
                material_record.airfield_repair_spent = 0.0;
                set_air_coverage(&mut next_air, country_id, 0.0)?;
                let (evacuated_aircraft, lost_aircraft) =
                    evacuate_defeated_wings(&mut next_air, country_id, side)?;
                let formations = armor_formation_count(&next_units, country_id)?;
                let desired = desired_armor_formations(material_record.armor_capacity);
                outcome.countries.push(CountryMaterialPayCycleOutcome {
                    country_id,
                    airfields_repaired: 0,
                    airfield_repair_spent: 0.0,
                    armor_purchased: 0,
                    armor_reinforced: 0,
                    armor_creation: ArmorFormationCreationOutcome {
                        desired_formations: desired,
                        formations_before: formations,
                        formations_after: formations,
                        created_unit_id: None,
                        created_equipment: 0,
                    },
                    air: CountryAirPayCycleOutcome {
                        country_id,
                        operations_spent: 0.0,
                        fighters_purchased: 0,
                        strike_purchased: 0,
                        fighters_reinforced: 0,
                        strike_reinforced: 0,
                        wing_creation: Vec::new(),
                    },
                    evacuated_aircraft,
                    lost_aircraft,
                });
                continue;
            }

            let (operational_fighters, operational_strike) =
                operational_aircraft(&next_air, country_id)?;
            let operations_due = f64::from(operational_fighters) / 100.0
                * FIGHTER_OPERATIONS_PER_100
                + f64::from(operational_strike) / 100.0 * STRIKE_OPERATIONS_PER_100;
            let can_operate = economy.payroll_coverage >= 0.999
                && economy.occupation_coverage >= 0.999
                && economy.arrears_cycles < 1.0;
            let operations_spent = if can_operate {
                economy.treasury.min(operations_due)
            } else {
                0.0
            };
            economy.treasury = (economy.treasury - operations_spent).max(0.0);
            let operations_coverage = if operations_due > 0.0 {
                operations_spent / operations_due
            } else {
                1.0
            };
            let fully_funded = can_operate && operations_coverage >= 0.999;

            let record = &mut next_state.countries[country_index];
            record.air_operations_due = operations_due;
            record.operations_coverage = operations_coverage;
            record.replacement_spent = 0.0;
            set_air_coverage(&mut next_air, country_id, operations_coverage)?;
            let material_record = &mut next_material.countries[country_index];
            material_record.armor_replacement_spent = 0.0;
            material_record.airfield_repair_spent = 0.0;

            let mut airfields_repaired = 0_u32;
            if fully_funded {
                for field in next_air.airfields.iter_mut().filter(|field| {
                    field.controller_country_id == country_id && field.health < 100.0
                }) {
                    if economy.treasury < AIRFIELD_REPAIR_COST {
                        break;
                    }
                    if repair_airfield(field) {
                        economy.treasury -= AIRFIELD_REPAIR_COST;
                        material_record.airfield_repair_spent += AIRFIELD_REPAIR_COST;
                        airfields_repaired = airfields_repaired.saturating_add(1);
                    }
                }
            }

            let mut armor_purchased = 0_u64;
            let mut fighters_purchased = 0_u32;
            let mut strike_purchased = 0_u32;
            if fully_funded {
                let live_armor = total_armor(&next_units, material_record)?;
                armor_purchased = armor_replacement_purchase(
                    material_record.armor_capacity,
                    live_armor,
                    economy.treasury,
                );
                let armor_spent = armor_purchased as f64 * ARMOR_REPLACEMENT_COST;
                economy.treasury = (economy.treasury - armor_spent).max(0.0);
                material_record.reserve_armor = material_record
                    .reserve_armor
                    .checked_add(armor_purchased)
                    .ok_or(ReinforcementError::Overflow("armor reserve"))?;
                material_record.armor_replacement_spent = armor_spent;

                let (fighters, strike) = total_aircraft(&next_air, record)?;
                fighters_purchased = replacement_purchase(
                    record.fighter_capacity,
                    fighters,
                    FIGHTER_REPLACEMENT_COST,
                    economy.treasury,
                );
                let fighter_spent = f64::from(fighters_purchased) * FIGHTER_REPLACEMENT_COST;
                economy.treasury = (economy.treasury - fighter_spent).max(0.0);
                record.reserve_fighters = record
                    .reserve_fighters
                    .checked_add(fighters_purchased)
                    .ok_or(ReinforcementError::Overflow("fighter reserve"))?;

                strike_purchased = replacement_purchase(
                    record.strike_capacity,
                    strike,
                    STRIKE_REPLACEMENT_COST,
                    economy.treasury,
                );
                let strike_spent = f64::from(strike_purchased) * STRIKE_REPLACEMENT_COST;
                economy.treasury = (economy.treasury - strike_spent).max(0.0);
                record.reserve_strike = record
                    .reserve_strike
                    .checked_add(strike_purchased)
                    .ok_or(ReinforcementError::Overflow("strike reserve"))?;
                record.replacement_spent = armor_spent + fighter_spent + strike_spent;
            }

            let armor_reinforced = reinforce_existing_armor(
                &mut next_units,
                material_record,
                country_id,
                side,
                &mut next_personnel,
            )?;
            let (fighters_reinforced, strike_reinforced) = reinforce_existing_wings(
                &mut next_air,
                record,
                country_id,
                side,
                &mut next_personnel,
            )?;
            let armor_creation = create_one_missing_armor_formation(
                &mut next_units,
                material_record,
                country_id,
                side,
                &mut next_state.next_unit_id,
                &mut next_personnel,
                home_positions.get(&country_id).copied(),
                max_units_per_side,
            )?;
            let wing_creation = [AirRole::Fighter, AirRole::Strike]
                .into_iter()
                .map(|role| {
                    create_one_missing_wing(
                        &mut next_air,
                        record,
                        role,
                        country_id,
                        side,
                        &mut next_state.next_air_wing_id,
                        &mut next_personnel,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;

            outcome.countries.push(CountryMaterialPayCycleOutcome {
                country_id,
                airfields_repaired,
                airfield_repair_spent: material_record.airfield_repair_spent,
                armor_purchased,
                armor_reinforced,
                armor_creation,
                air: CountryAirPayCycleOutcome {
                    country_id,
                    operations_spent,
                    fighters_purchased,
                    strike_purchased,
                    fighters_reinforced,
                    strike_reinforced,
                    wing_creation,
                },
                evacuated_aircraft: 0,
                lost_aircraft: 0,
            });
        }

        next_state.validate(&next_air, country_to_side, side_count)?;
        next_material.validate(&next_units, country_to_side, side_count)?;
        validate_economies(&next_economies, country_to_side)?;
        validate_personnel_reserves(&next_personnel, side_count)?;
        *self = next_state;
        *material = next_material;
        *units = next_units;
        *air_power = next_air;
        *economies = next_economies;
        *personnel_reserves = next_personnel;
        Ok(outcome)
    }
}

/// Convenience constructor for callers that prefer a free function.
pub fn bootstrap_reinforcement_state(
    air_power: &AirPowerState,
    next_unit_id: u64,
    next_air_wing_id: u64,
    country_to_side: &BTreeMap<u16, usize>,
    side_count: usize,
) -> Result<ReinforcementState, ReinforcementError> {
    ReinforcementState::bootstrap(
        air_power,
        next_unit_id,
        next_air_wing_id,
        country_to_side,
        side_count,
    )
}

fn validate_topology(
    country_to_side: &BTreeMap<u16, usize>,
    side_count: usize,
) -> Result<(), ReinforcementError> {
    if side_count == 0
        || country_to_side.is_empty()
        || country_to_side
            .iter()
            .any(|(&country, &side)| country == 0 || side >= side_count)
    {
        return Err(ReinforcementError::InvalidWorld("country/side topology"));
    }
    Ok(())
}

fn validate_air_topology(
    air_power: &AirPowerState,
    country_to_side: &BTreeMap<u16, usize>,
    side_count: usize,
) -> Result<(), ReinforcementError> {
    let coverage = air_power
        .country_coverage
        .iter()
        .map(|entry| entry.country_id)
        .collect::<BTreeSet<_>>();
    if coverage.iter().copied().ne(country_to_side.keys().copied())
        || air_power.airfields.iter().any(|field| {
            field.side >= side_count
                || !country_to_side.contains_key(&field.owner_country_id)
                || country_to_side.get(&field.controller_country_id) != Some(&field.side)
        })
        || air_power.wings.iter().any(|wing| {
            wing.side >= side_count
                || country_to_side.get(&wing.sovereign_country_id) != Some(&wing.side)
        })
    {
        return Err(ReinforcementError::InvalidWorld("air topology"));
    }
    Ok(())
}

fn validate_economies(
    economies: &BTreeMap<u16, EconomyState>,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<(), ReinforcementError> {
    if economies
        .keys()
        .copied()
        .ne(country_to_side.keys().copied())
        || economies.iter().any(|(&country_id, economy)| {
            economy.country_id != country_id
                || !economy.treasury.is_finite()
                || economy.treasury < 0.0
                || !economy.payroll_coverage.is_finite()
                || !(0.0..=1.0).contains(&economy.payroll_coverage)
                || !economy.occupation_coverage.is_finite()
                || !(0.0..=1.0).contains(&economy.occupation_coverage)
                || !economy.arrears_cycles.is_finite()
                || economy.arrears_cycles < 0.0
        })
    {
        return Err(ReinforcementError::InvalidWorld("economy funding"));
    }
    Ok(())
}

fn validate_personnel_reserves(
    personnel_reserves: &BTreeMap<usize, f64>,
    side_count: usize,
) -> Result<(), ReinforcementError> {
    if personnel_reserves.len() != side_count
        || personnel_reserves.keys().copied().ne(0..side_count)
        || personnel_reserves
            .values()
            .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(ReinforcementError::InvalidWorld("personnel reserves"));
    }
    Ok(())
}

fn operational_aircraft(
    air_power: &AirPowerState,
    country_id: u16,
) -> Result<(u32, u32), ReinforcementError> {
    let mut fighters = 0_u32;
    let mut strike = 0_u32;
    for wing in air_power.wings.iter().filter(|wing| {
        wing.sovereign_country_id == country_id && wing.state != AirWingState::Evacuated
    }) {
        let count = match wing.role {
            AirRole::Fighter => &mut fighters,
            AirRole::Strike => &mut strike,
        };
        *count = count
            .checked_add(wing.count)
            .ok_or(ReinforcementError::Overflow("operational aircraft"))?;
    }
    Ok((fighters, strike))
}

fn total_aircraft(
    air_power: &AirPowerState,
    country: &ReinforcementCountry,
) -> Result<(u32, u32), ReinforcementError> {
    let mut fighters = country.reserve_fighters;
    let mut strike = country.reserve_strike;
    for wing in air_power
        .wings
        .iter()
        .filter(|wing| wing.sovereign_country_id == country.country_id)
    {
        let count = match wing.role {
            AirRole::Fighter => &mut fighters,
            AirRole::Strike => &mut strike,
        };
        *count = count
            .checked_add(wing.count)
            .ok_or(ReinforcementError::Overflow("aircraft inventory"))?;
    }
    Ok((fighters, strike))
}

fn replacement_purchase(capacity: u32, current: u32, cost: f64, budget: f64) -> u32 {
    let missing = capacity.saturating_sub(current);
    let cycle_limit = capacity.saturating_mul(AIRCRAFT_REPLACEMENT_PERCENT) / 100;
    let affordable = (budget.max(0.0) / cost).floor().min(f64::from(u32::MAX)) as u32;
    missing.min(cycle_limit).min(affordable)
}

fn armor_replacement_purchase(capacity: u64, current: u64, budget: f64) -> u64 {
    let missing = capacity.saturating_sub(current);
    let cycle_limit = capacity.saturating_mul(ARMOR_REPLACEMENT_PERCENT) / 100;
    let affordable = (budget.max(0.0) / ARMOR_REPLACEMENT_COST)
        .floor()
        .min(u64::MAX as f64) as u64;
    missing.min(cycle_limit).min(affordable)
}

fn total_armor(
    units: &[SimulationUnit],
    country: &MaterialLogisticsCountry,
) -> Result<u64, ReinforcementError> {
    units
        .iter()
        .filter(|unit| {
            unit.combat.kind == UnitKind::Armor
                && unit.combat.sovereign == u64::from(country.country_id)
        })
        .try_fold(country.reserve_armor, |total, unit| {
            total
                .checked_add(unit.combat.equipment)
                .ok_or(ReinforcementError::Overflow("armor inventory"))
        })
}

fn reinforce_existing_armor(
    units: &mut [SimulationUnit],
    country: &mut MaterialLogisticsCountry,
    country_id: u16,
    side: usize,
    personnel_reserves: &mut BTreeMap<usize, f64>,
) -> Result<u64, ReinforcementError> {
    let mut reinforced = 0_u64;
    for unit in units.iter_mut().filter(|unit| {
        unit.combat.kind == UnitKind::Armor
            && unit.combat.sovereign == u64::from(country_id)
            && unit.combat.health > 0.0
    }) {
        if country.reserve_armor == 0 {
            break;
        }
        let missing = unit
            .combat
            .max_equipment
            .saturating_sub(unit.combat.equipment);
        let transfer = claim_crewed_equipment(
            missing.min(country.reserve_armor),
            side,
            ARMOR_CREW_PER_VEHICLE,
            personnel_reserves,
        )?;
        if transfer == 0 {
            continue;
        }
        unit.combat.equipment = unit
            .combat
            .equipment
            .checked_add(transfer)
            .ok_or(ReinforcementError::Overflow("armor formation equipment"))?;
        country.reserve_armor -= transfer;
        let restored_health =
            UNIT_HEALTH * unit.combat.equipment as f64 / unit.combat.max_equipment.max(1) as f64;
        unit.combat.health = unit.combat.health.max(restored_health.min(UNIT_HEALTH));
        reinforced = reinforced
            .checked_add(transfer)
            .ok_or(ReinforcementError::Overflow("reinforced armor"))?;
    }
    Ok(reinforced)
}

fn desired_armor_formations(capacity: u64) -> u64 {
    capacity
        .div_ceil(ARMOR_GROUP_TARGET_SIZE)
        .min(MAX_ARMOR_GROUPS_PER_COUNTRY)
}

fn armor_formation_count(
    units: &[SimulationUnit],
    country_id: u16,
) -> Result<u64, ReinforcementError> {
    u64::try_from(
        units
            .iter()
            .filter(|unit| {
                unit.combat.kind == UnitKind::Armor
                    && unit.combat.sovereign == u64::from(country_id)
            })
            .count(),
    )
    .map_err(|_| ReinforcementError::Overflow("armor formation count"))
}

#[allow(clippy::too_many_arguments)]
fn create_one_missing_armor_formation(
    units: &mut Vec<SimulationUnit>,
    country: &mut MaterialLogisticsCountry,
    country_id: u16,
    side: usize,
    next_unit_id: &mut u64,
    personnel_reserves: &mut BTreeMap<usize, f64>,
    home_position: Option<(f64, f64)>,
    max_units_per_side: u32,
) -> Result<ArmorFormationCreationOutcome, ReinforcementError> {
    let desired = desired_armor_formations(country.armor_capacity);
    let before = armor_formation_count(units, country_id)?;
    let mut result = ArmorFormationCreationOutcome {
        desired_formations: desired,
        formations_before: before,
        formations_after: before,
        created_unit_id: None,
        created_equipment: 0,
    };
    let side_count = units
        .iter()
        .filter(|unit| unit.combat.side as usize == side)
        .count();
    if before >= desired
        || country.reserve_armor == 0
        || side_count >= max_units_per_side as usize
        || home_position.is_none()
    {
        return Ok(result);
    }
    let target_size = country.armor_capacity.div_ceil(desired.max(1));
    let equipment = claim_crewed_equipment(
        target_size.min(country.reserve_armor),
        side,
        ARMOR_CREW_PER_VEHICLE,
        personnel_reserves,
    )?;
    if equipment == 0 {
        return Ok(result);
    }
    let id = *next_unit_id;
    *next_unit_id = next_unit_id
        .checked_add(1)
        .ok_or(ReinforcementError::Overflow("land unit ID"))?;
    let (lat, lng) = home_position.expect("checked above");
    country.reserve_armor -= equipment;
    units.push(SimulationUnit {
        combat: CombatUnit {
            id,
            side: side as u64,
            sovereign: u64::from(country_id),
            kind: UnitKind::Armor,
            lat,
            lng,
            health: UNIT_HEALTH,
            max_health: UNIT_HEALTH,
            personnel: 0,
            personnel_capacity: 0,
            equipment,
            max_equipment: target_size,
            quality: country.armor_quality,
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
    result.formations_after += 1;
    result.created_unit_id = Some(id);
    result.created_equipment = equipment;
    Ok(result)
}

fn claim_crewed_equipment(
    requested: u64,
    side: usize,
    crew_per_equipment: f64,
    personnel_reserves: &mut BTreeMap<usize, f64>,
) -> Result<u64, ReinforcementError> {
    let reserve = personnel_reserves
        .get_mut(&side)
        .ok_or(ReinforcementError::InvalidWorld("personnel reserve side"))?;
    let available = (*reserve / crew_per_equipment).floor().min(u64::MAX as f64) as u64;
    let claimed = requested.min(available);
    *reserve -= claimed as f64 * crew_per_equipment;
    Ok(claimed)
}

fn repair_airfield(field: &mut Airfield) -> bool {
    if field.health <= 0.0 && field.capture_repair_cycles < AIRFIELD_CAPTURE_REPAIR_CYCLES {
        field.capture_repair_cycles += 1;
        if field.capture_repair_cycles >= AIRFIELD_CAPTURE_REPAIR_CYCLES {
            field.health = 50.0;
            field.disabled = false;
        }
        return true;
    }
    if field.health >= 100.0 {
        return false;
    }
    field.capture_repair_cycles = field
        .capture_repair_cycles
        .max(AIRFIELD_CAPTURE_REPAIR_CYCLES);
    field.health = (field.health + AIRFIELD_REPAIR_PER_CYCLE).min(100.0);
    field.disabled = false;
    true
}

fn evacuate_defeated_wings(
    air_power: &mut AirPowerState,
    country_id: u16,
    side: usize,
) -> Result<(u64, u64), ReinforcementError> {
    let mut evacuated = 0_u64;
    let mut lost = 0_u64;
    let mut removed_ids = BTreeSet::new();
    for index in (0..air_power.wings.len()).rev() {
        if air_power.wings[index].sovereign_country_id != country_id {
            continue;
        }
        let destination = eligible_allied_airfield(air_power, index, country_id, side);
        if let Some(field_id) = destination {
            let field = air_power
                .airfields
                .iter()
                .find(|field| field.id == field_id)
                .expect("eligible allied field came from air state");
            evacuated = evacuated
                .checked_add(u64::from(air_power.wings[index].count))
                .ok_or(ReinforcementError::Overflow("evacuated aircraft"))?;
            let wing = &mut air_power.wings[index];
            wing.airfield_id = field_id;
            wing.lat = field.lat;
            wing.lng = field.lng;
            wing.state = AirWingState::Evacuated;
            wing.return_airfield_id = None;
            wing.target_kind = None;
            wing.target_id = None;
            wing.rearm_ticks = 0;
            wing.endurance_ticks = 0;
            wing.force_mission = false;
        } else {
            let wing = air_power.wings.remove(index);
            lost = lost
                .checked_add(u64::from(wing.count))
                .ok_or(ReinforcementError::Overflow("lost aircraft"))?;
            removed_ids.insert(wing.id);
        }
    }
    if !removed_ids.is_empty() {
        for wing in &mut air_power.wings {
            if wing.target_id.is_some_and(|id| removed_ids.contains(&id)) {
                wing.state = AirWingState::Grounded;
                wing.return_airfield_id = None;
                wing.target_kind = None;
                wing.target_id = None;
                wing.rearm_ticks = 0;
                wing.endurance_ticks = 0;
            }
        }
    }
    air_power
        .validate()
        .map_err(|_| ReinforcementError::Air("capitulation evacuation"))?;
    Ok((evacuated, lost))
}

fn eligible_allied_airfield(
    air_power: &AirPowerState,
    wing_index: usize,
    country_id: u16,
    side: usize,
) -> Option<u64> {
    const FERRY_RANGE_KM: f64 = 2_400.0;
    let wing = &air_power.wings[wing_index];
    air_power
        .airfields
        .iter()
        .filter(|field| {
            field.side == side
                && field.controller_country_id != country_id
                && field.owner_country_id != country_id
                && effective_capacity(field) > 0
        })
        .filter_map(|field| {
            let (_, allied) = field_occupancy(air_power, field);
            let capacity = effective_capacity(field) / 2;
            if allied >= capacity {
                return None;
            }
            let distance = haversine_km(wing.lat, wing.lng, field.lat, field.lng);
            (distance <= FERRY_RANGE_KM).then_some((distance, field.id))
        })
        .min_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        })
        .map(|(_, id)| id)
}

fn set_air_coverage(
    air_power: &mut AirPowerState,
    country_id: u16,
    operations_coverage: f64,
) -> Result<(), ReinforcementError> {
    let index = air_power
        .country_coverage
        .binary_search_by_key(&country_id, |entry| entry.country_id)
        .map_err(|_| ReinforcementError::InvalidWorld("air coverage country"))?;
    air_power.country_coverage[index].operations_coverage = operations_coverage;
    Ok(())
}

fn reinforce_existing_wings(
    air_power: &mut AirPowerState,
    country: &mut ReinforcementCountry,
    country_id: u16,
    side: usize,
    personnel_reserves: &mut BTreeMap<usize, f64>,
) -> Result<(u32, u32), ReinforcementError> {
    let mut fighters = 0_u32;
    let mut strike = 0_u32;
    for wing in air_power
        .wings
        .iter_mut()
        .filter(|wing| wing.sovereign_country_id == country_id)
    {
        let reserve = match wing.role {
            AirRole::Fighter => &mut country.reserve_fighters,
            AirRole::Strike => &mut country.reserve_strike,
        };
        let transfer = claim_aircrew(
            wing.max_count.saturating_sub(wing.count).min(*reserve),
            side,
            personnel_reserves,
        )?;
        wing.count = wing
            .count
            .checked_add(transfer)
            .ok_or(ReinforcementError::Overflow("wing aircraft"))?;
        *reserve -= transfer;
        let total = match wing.role {
            AirRole::Fighter => &mut fighters,
            AirRole::Strike => &mut strike,
        };
        *total = total
            .checked_add(transfer)
            .ok_or(ReinforcementError::Overflow("reinforced aircraft"))?;
    }
    Ok((fighters, strike))
}

fn create_one_missing_wing(
    air_power: &mut AirPowerState,
    country: &mut ReinforcementCountry,
    role: AirRole,
    country_id: u16,
    side: usize,
    next_air_wing_id: &mut u64,
    personnel_reserves: &mut BTreeMap<usize, f64>,
) -> Result<AirWingCreationOutcome, ReinforcementError> {
    let capacity = match role {
        AirRole::Fighter => country.fighter_capacity,
        AirRole::Strike => country.strike_capacity,
    };
    let desired_wings = capacity
        .div_ceil(AIR_WING_TARGET_SIZE)
        .min(MAX_WINGS_PER_ROLE_PER_COUNTRY);
    let wings_before = u32::try_from(
        air_power
            .wings
            .iter()
            .filter(|wing| wing.sovereign_country_id == country_id && wing.role == role)
            .count(),
    )
    .map_err(|_| ReinforcementError::Overflow("wing count"))?;
    let mut result = AirWingCreationOutcome {
        role,
        desired_wings,
        wings_before,
        wings_after: wings_before,
        remaining_missing_wings: desired_wings.saturating_sub(wings_before),
        created_wing_id: None,
        created_aircraft: 0,
        status: AirWingCreationStatus::AtDesiredCount,
    };
    if wings_before >= desired_wings {
        return Ok(result);
    }

    let reserve = match role {
        AirRole::Fighter => &mut country.reserve_fighters,
        AirRole::Strike => &mut country.reserve_strike,
    };
    if *reserve == 0 {
        result.status = AirWingCreationStatus::NoReserve;
        return Ok(result);
    }
    if live_air_wing_markers(air_power) >= MAX_LIVE_AIR_WING_MARKERS {
        result.status = AirWingCreationStatus::GlobalMarkerCap;
        return Ok(result);
    }

    let probe = air_power
        .wings
        .iter()
        .find(|wing| wing.sovereign_country_id == country_id && wing.role == role)
        .or_else(|| {
            air_power
                .wings
                .iter()
                .find(|wing| wing.sovereign_country_id == country_id)
        });
    let (probe_lat, probe_lng, quality) = probe
        .map(|wing| (wing.lat, wing.lng, wing.quality))
        .unwrap_or((0.0, 0.0, 50.0));
    let Some(airfield_id) = eligible_airfield(air_power, country_id, side, probe_lat, probe_lng)
    else {
        result.status = AirWingCreationStatus::NoEligibleAirfield;
        return Ok(result);
    };

    let target_size = capacity.div_ceil(desired_wings.max(1));
    let requested = target_size.min(*reserve);
    let equipment = claim_aircrew(requested, side, personnel_reserves)?;
    if equipment == 0 {
        result.status = AirWingCreationStatus::NoPersonnel;
        return Ok(result);
    }
    let field = air_power
        .airfields
        .iter()
        .find(|field| field.id == airfield_id)
        .expect("eligible field came from air state");
    let id = *next_air_wing_id;
    *next_air_wing_id = next_air_wing_id
        .checked_add(1)
        .ok_or(ReinforcementError::Overflow("next air-wing ID"))?;
    *reserve -= equipment;
    air_power.wings.push(AirWing {
        id,
        side,
        sovereign_country_id: country_id,
        airfield_id,
        return_airfield_id: None,
        role,
        quality,
        max_count: target_size,
        count: equipment,
        lat: field.lat,
        lng: field.lng,
        state: match role {
            AirRole::Fighter => AirWingState::Patrol,
            AirRole::Strike => AirWingState::Grounded,
        },
        target_kind: None::<AirTargetKind>,
        target_id: None,
        rearm_ticks: 0,
        cooldown_ticks: 0,
        endurance_ticks: 0,
        next_mission_tick: None,
        force_mission: false,
    });
    result.wings_after += 1;
    result.remaining_missing_wings = desired_wings.saturating_sub(result.wings_after);
    result.created_wing_id = Some(id);
    result.created_aircraft = equipment;
    result.status = AirWingCreationStatus::Created;
    Ok(result)
}

fn claim_aircrew(
    requested: u32,
    side: usize,
    personnel_reserves: &mut BTreeMap<usize, f64>,
) -> Result<u32, ReinforcementError> {
    let reserve = personnel_reserves
        .get_mut(&side)
        .ok_or(ReinforcementError::InvalidWorld("personnel reserve side"))?;
    let available = (*reserve / AIRCREW_PER_AIRCRAFT)
        .floor()
        .min(f64::from(u32::MAX)) as u32;
    let claimed = requested.min(available);
    *reserve -= f64::from(claimed) * AIRCREW_PER_AIRCRAFT;
    Ok(claimed)
}

fn live_air_wing_markers(air_power: &AirPowerState) -> usize {
    air_power
        .wings
        .iter()
        .filter(|wing| wing.count > 0 && wing.state != AirWingState::Evacuated)
        .count()
}

fn eligible_airfield(
    air_power: &AirPowerState,
    country_id: u16,
    side: usize,
    lat: f64,
    lng: f64,
) -> Option<u64> {
    let mut best: Option<(bool, f64, u64)> = None;
    for field in &air_power.airfields {
        if field.side != side || effective_capacity(field) == 0 {
            continue;
        }
        let national = field.controller_country_id == country_id;
        let (national_wings, allied_wings) = field_occupancy(air_power, field);
        let capacity = effective_capacity(field);
        let has_capacity = if national {
            national_wings + allied_wings < capacity
        } else {
            allied_wings < capacity / 2
        };
        if !has_capacity {
            continue;
        }
        let distance = haversine_km(lat, lng, field.lat, field.lng);
        let replace = best.is_none_or(|(best_national, best_distance, best_id)| {
            national && !best_national
                || (national == best_national
                    && (distance < best_distance
                        || (distance == best_distance && field.id < best_id)))
        });
        if replace {
            best = Some((national, distance, field.id));
        }
    }
    best.map(|(_, _, id)| id)
}

fn field_occupancy(air_power: &AirPowerState, field: &Airfield) -> (u32, u32) {
    air_power
        .wings
        .iter()
        .filter(|wing| wing.airfield_id == field.id && wing.state != AirWingState::Evacuated)
        .fold((0, 0), |(national, allied), wing| {
            if wing.sovereign_country_id == field.controller_country_id {
                (national + 1, allied)
            } else {
                (national, allied + 1)
            }
        })
}

fn effective_capacity(field: &Airfield) -> u32 {
    if field.disabled || field.health <= 0.0 {
        0
    } else if field.health <= 50.0 {
        field.capacity.min(1)
    } else {
        field.capacity
    }
}

fn haversine_km(a_lat: f64, a_lng: f64, b_lat: f64, b_lng: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6_371.0;
    let lat1 = a_lat.to_radians();
    let lat2 = b_lat.to_radians();
    let d_lat = (b_lat - a_lat).to_radians();
    let mut d_lng = b_lng - a_lng;
    if d_lng > 180.0 {
        d_lng -= 360.0;
    } else if d_lng < -180.0 {
        d_lng += 360.0;
    }
    let d_lng = d_lng.to_radians();
    let h = (d_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (d_lng / 2.0).sin().powi(2);
    EARTH_RADIUS_KM * 2.0 * h.sqrt().atan2((1.0 - h).max(0.0).sqrt())
}

fn strictly_ordered<T: Ord + Copy>(values: impl Iterator<Item = T>) -> bool {
    let mut previous = None;
    for value in values {
        if previous.is_some_and(|previous| previous >= value) {
            return false;
        }
        previous = Some(value);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::air::AirCountryCoverage;
    use crate::economy::{CommandBand, EconomyState};

    fn topology() -> BTreeMap<u16, usize> {
        BTreeMap::from([(1, 0), (2, 1)])
    }

    fn economy(country_id: u16, treasury: f64) -> EconomyState {
        EconomyState {
            country_id,
            economic_strength: 1.0,
            base_income: 1.0,
            treasury,
            income: 1.0,
            occupation_yield: 0.0,
            payroll_due: 0.0,
            occupation_due: 0.0,
            payroll_coverage: 1.0,
            occupation_coverage: 1.0,
            arrears_cycles: 0.0,
            command_band: CommandBand::Paid,
            mutiny_recovery_cycles: 0,
            initial_core_cells: 1,
            initial_city_population: 0.0,
            core_control_ratio: 1.0,
            city_control_ratio: 1.0,
            capital_held: true,
            last_event_band: CommandBand::Paid,
            capitulated: false,
        }
    }

    fn field(id: u64, side: usize, country_id: u16, capacity: u32) -> Airfield {
        Airfield {
            id,
            side,
            owner_country_id: country_id,
            controller_country_id: country_id,
            lat: side as f64 * 10.0,
            lng: 0.0,
            capacity,
            health: 100.0,
            disabled: false,
            capture_repair_cycles: 0,
            capital: true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn wing(
        id: u64,
        side: usize,
        country_id: u16,
        field_id: u64,
        role: AirRole,
        count: u32,
        max_count: u32,
        state: AirWingState,
    ) -> AirWing {
        AirWing {
            id,
            side,
            sovereign_country_id: country_id,
            airfield_id: field_id,
            return_airfield_id: None,
            role,
            quality: 60.0,
            max_count,
            count,
            lat: side as f64 * 10.0,
            lng: 0.0,
            state,
            target_kind: None,
            target_id: None,
            rearm_ticks: 0,
            cooldown_ticks: 0,
            endurance_ticks: 0,
            next_mission_tick: None,
            force_mission: false,
        }
    }

    fn air_state() -> AirPowerState {
        let mut air = AirPowerState::new(
            vec![field(10, 0, 1, 4), field(20, 1, 2, 4)],
            vec![
                wing(
                    100,
                    0,
                    1,
                    10,
                    AirRole::Fighter,
                    50,
                    100,
                    AirWingState::Patrol,
                ),
                wing(
                    101,
                    0,
                    1,
                    10,
                    AirRole::Strike,
                    25,
                    100,
                    AirWingState::Evacuated,
                ),
                wing(
                    200,
                    1,
                    2,
                    20,
                    AirRole::Fighter,
                    100,
                    100,
                    AirWingState::Patrol,
                ),
                wing(
                    201,
                    1,
                    2,
                    20,
                    AirRole::Strike,
                    100,
                    100,
                    AirWingState::Grounded,
                ),
            ],
        )
        .unwrap();
        air.country_coverage = vec![
            AirCountryCoverage {
                country_id: 1,
                operations_coverage: 1.0,
            },
            AirCountryCoverage {
                country_id: 2,
                operations_coverage: 1.0,
            },
        ];
        air
    }

    fn worlds(
        treasury: f64,
    ) -> (
        ReinforcementState,
        AirPowerState,
        BTreeMap<u16, EconomyState>,
        BTreeMap<usize, f64>,
    ) {
        let air = air_state();
        let state = ReinforcementState::bootstrap(&air, 500, 202, &topology(), 2).unwrap();
        (
            state,
            air,
            BTreeMap::from([(1, economy(1, treasury)), (2, economy(2, treasury))]),
            BTreeMap::from([(0, 1_000.0), (1, 1_000.0)]),
        )
    }

    #[test]
    fn serde_is_strict_and_bootstrap_is_canonical() {
        let air = air_state();
        let state = ReinforcementState::bootstrap(&air, 500, 202, &topology(), 2).unwrap();
        assert_eq!(state.schema, REINFORCEMENT_SCHEMA_VERSION);
        assert_eq!(state.next_unit_id, 500);
        assert_eq!(state.countries[0].fighter_capacity, 100);
        assert_eq!(state.countries[0].strike_capacity, 100);
        assert_eq!(state.countries[0].reserve_fighters, 0);

        let mut wire = serde_json::to_value(&state).unwrap();
        wire.as_object_mut()
            .unwrap()
            .insert("extra".into(), 1.into());
        assert!(serde_json::from_value::<ReinforcementState>(wire).is_err());
        let missing = serde_json::json!({
            "schema": REINFORCEMENT_SCHEMA_VERSION,
            "nextUnitId": 1,
            "nextAirWingId": 202
        });
        assert!(serde_json::from_value::<ReinforcementState>(missing).is_err());
    }

    #[test]
    fn bootstrap_rejects_air_topology_disagreement_and_stale_id() {
        let mut air = air_state();
        air.wings[0].side = 1;
        assert!(matches!(
            ReinforcementState::bootstrap(&air, 500, 202, &topology(), 2),
            Err(ReinforcementError::Air(_)) | Err(ReinforcementError::InvalidWorld(_))
        ));

        let air = air_state();
        assert!(matches!(
            ReinforcementState::bootstrap(&air, 500, 201, &topology(), 2),
            Err(ReinforcementError::InvalidState(_))
        ));
    }

    #[test]
    fn operations_exclude_evacuated_wings_and_pay_before_replacements() {
        let (mut state, mut air, mut economies, mut personnel) = worlds(1.0);
        let outcome = state
            .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2)
            .unwrap();
        let country = &outcome.countries[0];
        assert_eq!(state.countries[0].air_operations_due, 0.5);
        assert_eq!(country.operations_spent, 0.5);
        assert_eq!(country.fighters_purchased, 1);
        assert_eq!(country.strike_purchased, 1);
        assert_eq!(state.countries[0].replacement_spent, 0.35);
        assert!((economies[&1].treasury - 0.15).abs() < 1e-12);
        assert_eq!(air.country_coverage[0].operations_coverage, 1.0);
    }

    #[test]
    fn funding_gate_blocks_operations_and_all_replacement_purchases() {
        let (mut state, mut air, mut economies, mut personnel) = worlds(100.0);
        economies.get_mut(&1).unwrap().payroll_coverage = 0.998;
        let outcome = state
            .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2)
            .unwrap();
        assert_eq!(outcome.countries[0].operations_spent, 0.0);
        assert_eq!(outcome.countries[0].fighters_purchased, 0);
        assert_eq!(outcome.countries[0].strike_purchased, 0);
        assert_eq!(state.countries[0].operations_coverage, 0.0);
        assert_eq!(economies[&1].treasury, 100.0);
        assert_eq!(air.country_coverage[0].operations_coverage, 0.0);
    }

    #[test]
    fn capitulated_country_publishes_zero_without_touching_resources_or_wings() {
        let (mut state, mut air, mut economies, mut personnel) = worlds(100.0);
        state.countries[0].reserve_fighters = 40;
        state.countries[0].reserve_strike = 50;
        economies.get_mut(&1).unwrap().capitulated = true;
        let wings_before = air.wings.clone();
        let treasury_before = economies[&1].treasury;
        let personnel_before = personnel[&0];
        let next_air_wing_id_before = state.next_air_wing_id;

        let outcome = state
            .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2)
            .unwrap();

        assert_eq!(
            outcome.countries[0],
            CountryAirPayCycleOutcome {
                country_id: 1,
                operations_spent: 0.0,
                fighters_purchased: 0,
                strike_purchased: 0,
                fighters_reinforced: 0,
                strike_reinforced: 0,
                wing_creation: Vec::new(),
            }
        );
        assert_eq!(state.countries[0].air_operations_due, 0.0);
        assert_eq!(state.countries[0].operations_coverage, 0.0);
        assert_eq!(state.countries[0].replacement_spent, 0.0);
        assert_eq!(state.countries[0].reserve_fighters, 40);
        assert_eq!(state.countries[0].reserve_strike, 50);
        assert_eq!(state.next_air_wing_id, next_air_wing_id_before);
        assert_eq!(air.wings, wings_before);
        assert_eq!(air.country_coverage[0].operations_coverage, 0.0);
        assert_eq!(economies[&1].treasury, treasury_before);
        assert_eq!(personnel[&0], personnel_before);
    }

    #[test]
    fn partial_operations_spend_prevents_replacements() {
        let (mut state, mut air, mut economies, mut personnel) = worlds(0.25);
        let outcome = state
            .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2)
            .unwrap();
        assert_eq!(outcome.countries[0].operations_spent, 0.25);
        assert_eq!(state.countries[0].operations_coverage, 0.5);
        assert_eq!(state.countries[0].replacement_spent, 0.0);
        assert_eq!(outcome.countries[0].fighters_purchased, 0);
    }

    #[test]
    fn existing_wings_fill_in_id_order_and_consume_one_crew_each() {
        let (mut state, mut air, mut economies, mut personnel) = worlds(100.0);
        state.countries[0].reserve_fighters = 50;
        state.countries[0].fighter_capacity = 150;
        personnel.insert(0, 20.0);
        state
            .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2)
            .unwrap();
        assert_eq!(air.wings[0].count, 70);
        assert_eq!(state.countries[0].reserve_fighters, 31);
        assert_eq!(personnel[&0], 0.0);
    }

    #[test]
    fn creates_one_missing_role_wing_with_monotonic_id() {
        let mut air = air_state();
        air.wings
            .retain(|wing| !(wing.sovereign_country_id == 1 && wing.role == AirRole::Strike));
        let mut state = ReinforcementState::bootstrap(&air, 500, 202, &topology(), 2).unwrap();
        state.countries[0].strike_capacity = 48;
        state.countries[0].reserve_strike = 30;
        let mut economies = BTreeMap::from([(1, economy(1, 100.0)), (2, economy(2, 100.0))]);
        let mut personnel = BTreeMap::from([(0, 30.0), (1, 0.0)]);
        let outcome = state
            .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2)
            .unwrap();
        let created = &outcome.countries[0].wing_creation[1];
        assert_eq!(created.status, AirWingCreationStatus::Created);
        assert_eq!(created.created_wing_id, Some(202));
        assert_eq!(created.created_aircraft, 24);
        assert_eq!(created.remaining_missing_wings, 1);
        assert_eq!(state.next_air_wing_id, 203);
        // One crew fills the existing fighter with this cycle's purchase,
        // then 24 crews staff the newly created strike wing.
        assert_eq!(personnel[&0], 5.0);
        assert_eq!(air.wings.last().unwrap().state, AirWingState::Grounded);
    }

    #[test]
    fn wing_creation_reports_airfield_and_personnel_bounds() {
        let mut air = air_state();
        air.wings
            .retain(|wing| !(wing.sovereign_country_id == 1 && wing.role == AirRole::Strike));
        air.airfields[0].capacity = 1;
        let mut state = ReinforcementState::bootstrap(&air, 500, 202, &topology(), 2).unwrap();
        state.countries[0].strike_capacity = 24;
        state.countries[0].reserve_strike = 24;
        let mut economies = BTreeMap::from([(1, economy(1, 100.0)), (2, economy(2, 100.0))]);
        let mut personnel = BTreeMap::from([(0, 24.0), (1, 0.0)]);
        let outcome = state
            .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2)
            .unwrap();
        assert_eq!(
            outcome.countries[0].wing_creation[1].status,
            AirWingCreationStatus::NoEligibleAirfield
        );
        assert_eq!(state.next_air_wing_id, 202);

        air.airfields[0].capacity = 4;
        personnel.insert(0, 0.0);
        let outcome = state
            .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2)
            .unwrap();
        assert_eq!(
            outcome.countries[0].wing_creation[1].status,
            AirWingCreationStatus::NoPersonnel
        );
    }

    #[test]
    fn validation_failure_is_atomic_for_every_mutable_input() {
        let (mut state, mut air, mut economies, mut personnel) = worlds(100.0);
        personnel.remove(&1);
        let before_state = state.clone();
        let before_air = air.clone();
        let before_economies = economies.clone();
        let before_personnel = personnel.clone();
        assert!(
            state
                .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2,)
                .is_err()
        );
        assert_eq!(state, before_state);
        assert_eq!(air, before_air);
        assert_eq!(economies, before_economies);
        assert_eq!(personnel, before_personnel);
    }

    #[test]
    fn late_id_overflow_rolls_back_operations_spend_purchase_and_claimed_crew() {
        let mut air = air_state();
        air.wings
            .retain(|wing| !(wing.sovereign_country_id == 1 && wing.role == AirRole::Strike));
        let mut state = ReinforcementState::bootstrap(&air, 500, u64::MAX, &topology(), 2).unwrap();
        state.countries[0].strike_capacity = 24;
        state.countries[0].reserve_strike = 24;
        let mut economies = BTreeMap::from([(1, economy(1, 100.0)), (2, economy(2, 100.0))]);
        let mut personnel = BTreeMap::from([(0, 24.0), (1, 0.0)]);
        let before_state = state.clone();
        let before_air = air.clone();
        let before_economies = economies.clone();
        let before_personnel = personnel.clone();

        assert_eq!(
            state.settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2,),
            Err(ReinforcementError::Overflow("next air-wing ID"))
        );
        assert_eq!(state, before_state);
        assert_eq!(air, before_air);
        assert_eq!(economies, before_economies);
        assert_eq!(personnel, before_personnel);
    }

    #[test]
    fn cycle_order_is_country_id_then_fighter_then_strike() {
        let (mut state, mut air, mut economies, mut personnel) = worlds(100.0);
        state.countries[0].reserve_fighters = 10;
        state.countries[0].fighter_capacity = 110;
        state.countries[1].reserve_fighters = 10;
        state.countries[1].fighter_capacity = 110;
        personnel.insert(0, 10.0);
        personnel.insert(1, 10.0);
        let outcome = state
            .settle_air_pay_cycle(&mut air, &mut economies, &mut personnel, &topology(), 2)
            .unwrap();
        assert_eq!(
            outcome
                .countries
                .iter()
                .map(|country| country.country_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            outcome.countries[0]
                .wing_creation
                .iter()
                .map(|entry| entry.role)
                .collect::<Vec<_>>(),
            vec![AirRole::Fighter, AirRole::Strike]
        );
    }

    fn armor_unit(id: u64, country_id: u16, side: usize, equipment: u64) -> SimulationUnit {
        SimulationUnit {
            combat: CombatUnit {
                id,
                side: side as u64,
                sovereign: country_id as u64,
                kind: UnitKind::Armor,
                lat: side as f64 * 10.0,
                lng: 0.0,
                health: 50.0,
                max_health: 100.0,
                personnel: 0,
                personnel_capacity: 0,
                equipment,
                max_equipment: 100,
                quality: 60.0,
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
        }
    }

    type MaterialWorld = (
        ReinforcementState,
        MaterialLogisticsState,
        Vec<SimulationUnit>,
        AirPowerState,
        BTreeMap<u16, EconomyState>,
        BTreeMap<usize, f64>,
    );

    fn material_world() -> MaterialWorld {
        let air = air_state();
        let units = vec![armor_unit(1, 1, 0, 50)];
        let profiles = BTreeMap::from([(1, (100_u64, 70.0)), (2, (0_u64, 70.0))]);
        let material =
            MaterialLogisticsState::bootstrap(&units, &profiles, &topology(), 2).unwrap();
        let reinforcement = ReinforcementState::bootstrap(&air, 500, 202, &topology(), 2).unwrap();
        (
            reinforcement,
            material,
            units,
            air,
            BTreeMap::from([(1, economy(1, 100.0)), (2, economy(2, 100.0))]),
            BTreeMap::from([(0, 200.0), (1, 0.0)]),
        )
    }

    #[test]
    fn material_bootstrap_is_strict_and_ordered() {
        let (_, material, units, _, _, _) = material_world();
        assert_eq!(material.schema, MATERIAL_LOGISTICS_SCHEMA_VERSION);
        assert_eq!(
            material
                .countries
                .iter()
                .map(|c| c.country_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let mut wire = serde_json::to_value(&material).unwrap();
        wire.as_object_mut()
            .unwrap()
            .insert("extra".into(), 1.into());
        assert!(serde_json::from_value::<MaterialLogisticsState>(wire).is_err());
        let mut invalid = material.clone();
        invalid.countries[0].reserve_armor = invalid.countries[0].armor_capacity + 1;
        assert!(invalid.validate(&units, &topology(), 2).is_err());
    }

    #[test]
    fn material_cycle_repairs_then_buys_and_reinforces_armor() {
        let (mut reinforcement, mut material, mut units, mut air, mut economies, mut personnel) =
            material_world();
        air.airfields[0].health = 50.0;
        air.airfields[0].disabled = true;
        let before = economies[&1].treasury;
        let outcome = reinforcement
            .settle_material_pay_cycle(
                &mut material,
                &mut units,
                &mut air,
                &mut economies,
                &mut personnel,
                &topology(),
                2,
                &BTreeMap::from([(1, (0.0, 0.0)), (2, (10.0, 0.0))]),
                100,
            )
            .unwrap();
        let country = &outcome.countries[0];
        assert_eq!(country.airfields_repaired, 1);
        assert!(country.airfield_repair_spent > 0.0);
        assert!(country.armor_purchased > 0);
        assert_eq!(country.armor_reinforced, 1);
        assert!(units[0].combat.health > 50.0);
        assert!(personnel[&0] < 200.0);
        assert!(economies[&1].treasury < before);
    }

    #[test]
    fn material_capitulation_clears_reserves_and_evacuates_wings() {
        let (mut reinforcement, mut material, mut units, mut air, mut economies, mut personnel) =
            material_world();
        material.countries[0].reserve_armor = 20;
        reinforcement.countries[0].reserve_fighters = 10;
        reinforcement.countries[0].reserve_strike = 10;
        economies.get_mut(&1).unwrap().capitulated = true;
        let outcome = reinforcement
            .settle_material_pay_cycle(
                &mut material,
                &mut units,
                &mut air,
                &mut economies,
                &mut personnel,
                &topology(),
                2,
                &BTreeMap::from([(1, (0.0, 0.0)), (2, (10.0, 0.0))]),
                100,
            )
            .unwrap();
        assert_eq!(material.countries[0].reserve_armor, 0);
        assert_eq!(reinforcement.countries[0].reserve_fighters, 0);
        assert_eq!(reinforcement.countries[0].reserve_strike, 0);
        assert!(
            outcome.countries[0].evacuated_aircraft > 0 || outcome.countries[0].lost_aircraft > 0
        );
    }

    #[test]
    fn material_late_allocator_overflow_is_atomic() {
        let (mut reinforcement, mut material, mut units, mut air, mut economies, mut personnel) =
            material_world();
        reinforcement.next_unit_id = u64::MAX;
        material.countries[0].armor_capacity = 200;
        material.countries[0].reserve_armor = 150;
        personnel.insert(0, 1_000.0);
        let before = (
            reinforcement.clone(),
            material.clone(),
            units.clone(),
            air.clone(),
            economies.clone(),
            personnel.clone(),
        );
        assert_eq!(
            reinforcement.settle_material_pay_cycle(
                &mut material,
                &mut units,
                &mut air,
                &mut economies,
                &mut personnel,
                &topology(),
                2,
                &BTreeMap::from([(1, (0.0, 0.0)), (2, (10.0, 0.0))]),
                100
            ),
            Err(ReinforcementError::Overflow("land unit ID"))
        );
        assert_eq!(
            (reinforcement, material, units, air, economies, personnel),
            before
        );
    }
}
