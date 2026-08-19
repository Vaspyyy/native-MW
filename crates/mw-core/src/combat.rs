//! Deterministic, pair-level land combat matching the browser simulation.
//!
//! This module deliberately does not select targets, remove defeated units, or
//! perform strategic simulation. Callers supply an ordered attacker/target pair
//! and retain control over the surrounding tick.

use thiserror::Error;

use crate::world::WorldGridView;

pub const COMBAT_SCHEMA_VERSION: &str = "1";
pub const UNIT_HEALTH: f64 = 100.0;
pub const PERSONNEL_PER_FORMATION: f64 = 1_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitKind {
    Army,
    Armor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CombatUnit {
    pub id: u64,
    pub side: u64,
    pub sovereign: u64,
    pub kind: UnitKind,
    pub lat: f64,
    pub lng: f64,
    pub health: f64,
    pub max_health: f64,
    pub personnel: u64,
    pub personnel_capacity: u64,
    pub equipment: u64,
    pub max_equipment: u64,
    pub quality: f64,
    pub transport: bool,
    pub armor_supported: bool,
    pub landing_penalty_active: bool,
    pub at_sea: bool,
    pub last_combat_tick: u64,
    pub victory_boost_ticks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CombatConfig {
    pub combat_damage: f64,
    pub proximity_radius: f64,
    pub direct_radius: f64,
    pub target_jitter_scale: f64,
    pub armor_crew_per_vehicle: u64,
    pub unit_speed: f64,
    pub unit_naval_speed: f64,
}

impl Default for CombatConfig {
    fn default() -> Self {
        Self {
            combat_damage: 0.7,
            proximity_radius: 0.3,
            direct_radius: 0.05,
            target_jitter_scale: 0.08,
            armor_crew_per_vehicle: 2,
            unit_speed: 0.003,
            unit_naval_speed: 0.025,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CombatContext<'a> {
    pub sim_tick: u64,
    pub frame: u64,
    pub war_grace_end: u64,
    /// Damage-dealt multiplier of the supplied attacker (the current unit).
    pub attacker_damage_dealt_multiplier: f64,
    /// Damage-taken multiplier of the supplied attacker (the current unit).
    pub attacker_damage_taken_multiplier: f64,
    pub defense_bonus: f64,
    pub long_war_defense: f64,
    pub mountain: bool,
    pub urban: bool,
    pub world: Option<WorldGridView<'a>>,
}

impl Default for CombatContext<'_> {
    fn default() -> Self {
        Self {
            sim_tick: 0,
            frame: 0,
            war_grace_end: 0,
            attacker_damage_dealt_multiplier: 1.0,
            attacker_damage_taken_multiplier: 1.0,
            defense_bonus: 1.0,
            long_war_defense: 1.0,
            mountain: false,
            urban: false,
            world: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DamageApply {
    pub requested_damage: f64,
    pub effective_damage: f64,
    pub personnel_loss: u64,
    pub equipment_loss: u64,
    pub resulting_health: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatLayer {
    Proximity,
    Direct,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CombatEvent {
    pub schema_version: &'static str,
    pub layer: CombatLayer,
    pub attacker_id: u64,
    pub target_id: u64,
    /// Damage calculated for the target before either unit is mutated.
    pub target_damage: f64,
    /// Normal retaliation calculated for the attacker before mutation.
    pub attacker_damage: f64,
    /// Transport vulnerability hit applied to the attacker before both normal hits.
    pub transport_self_damage: f64,
    pub target_personnel_loss: u64,
    pub attacker_personnel_loss: u64,
    pub target_equipment_loss: u64,
    pub attacker_equipment_loss: u64,
    pub target_resulting_health: f64,
    pub attacker_resulting_health: f64,
    pub target_knockback_blocked: bool,
    pub attacker_knockback_blocked: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CombatError {
    #[error("combat unit index {index} is out of bounds for {len} units")]
    IndexOutOfBounds { index: usize, len: usize },
    #[error("attacker and target indices must be distinct")]
    SameUnit,
    #[error("combat input contains a non-finite numeric value")]
    NonFiniteInput,
    #[error("world grid is invalid")]
    InvalidWorld,
    #[error("combat configuration requires positive radii and movement speeds")]
    InvalidConfig,
}

/// Mirrors `combined-arms.js::getQualityMultiplier` for finite qualities.
pub fn quality_multiplier(quality: f64) -> f64 {
    0.75 + quality.clamp(0.0, 100.0) / 200.0
}

/// Mirrors `combined-arms.js::getArmorCombatMultiplier`.
pub fn matchup_multiplier(
    attacker: UnitKind,
    target: UnitKind,
    mountain: bool,
    urban: bool,
    supported: bool,
) -> f64 {
    let mut multiplier = match (attacker, target) {
        (UnitKind::Armor, UnitKind::Army) => {
            if mountain {
                0.65
            } else if urban {
                1.0
            } else {
                2.0
            }
        }
        (UnitKind::Army, UnitKind::Armor) => {
            if mountain {
                0.8
            } else if urban {
                0.7
            } else {
                0.35
            }
        }
        (UnitKind::Armor, UnitKind::Armor) => {
            if mountain {
                0.6
            } else if urban {
                0.8
            } else {
                1.0
            }
        }
        (UnitKind::Army, UnitKind::Army) => 1.0,
    };
    if attacker == UnitKind::Armor && supported {
        multiplier *= 1.15;
    }
    multiplier
}

pub fn formation_strength(unit: &CombatUnit) -> f64 {
    match unit.kind {
        UnitKind::Armor => 1.0,
        UnitKind::Army => unit.personnel as f64 / PERSONNEL_PER_FORMATION,
    }
}

/// Return the shortest one-wrap longitudinal delta `to - from`.
pub fn wrapped_longitude_delta(from: f64, to: f64) -> f64 {
    let mut delta = to - from;
    if delta > 180.0 {
        delta -= 360.0;
    } else if delta < -180.0 {
        delta += 360.0;
    }
    delta
}

pub fn wrapped_distance_squared(a: &CombatUnit, b: &CombatUnit) -> f64 {
    let d_lat = b.lat - a.lat;
    let d_lng = wrapped_longitude_delta(a.lng, b.lng);
    d_lat * d_lat + d_lng * d_lng
}

/// Browser target jitter is based only on the current/attacking unit id.
pub fn jittered_target_distance(
    attacker: &CombatUnit,
    target: &CombatUnit,
    jitter_scale: f64,
) -> f64 {
    let phase = attacker.id as f64 * 100.0;
    let jittered_lat = target.lat + phase.sin() * jitter_scale;
    let jittered_lng = target.lng + phase.cos() * jitter_scale;
    let d_lat = jittered_lat - attacker.lat;
    let d_lng = wrapped_longitude_delta(attacker.lng, jittered_lng);
    (d_lat * d_lat + d_lng * d_lng).sqrt()
}

pub fn combined_arms_damage(
    base_damage: f64,
    attacker: &CombatUnit,
    target: &CombatUnit,
    mountain: bool,
    urban: bool,
) -> f64 {
    combined_arms_damage_with_landing_penalty(base_damage, attacker, target, mountain, urban, true)
}

fn combined_arms_damage_with_landing_penalty(
    base_damage: f64,
    attacker: &CombatUnit,
    target: &CombatUnit,
    mountain: bool,
    urban: bool,
    apply_landing_penalty: bool,
) -> f64 {
    let landing_multiplier = if apply_landing_penalty
        && attacker.kind == UnitKind::Armor
        && attacker.landing_penalty_active
    {
        0.3
    } else {
        1.0
    };
    let strength = if attacker.kind == UnitKind::Army {
        formation_strength(attacker).max(0.0)
    } else {
        1.0
    };
    base_damage
        * matchup_multiplier(
            attacker.kind,
            target.kind,
            mountain,
            urban,
            attacker.armor_supported,
        )
        * if attacker.kind == UnitKind::Armor {
            quality_multiplier(attacker.quality)
        } else {
            1.0
        }
        * landing_multiplier
        * strength
}

/// Apply damage with the browser's discrete personnel/equipment accounting.
pub fn apply_land_unit_damage(
    target: &mut CombatUnit,
    damage: f64,
    crew_per_vehicle: u64,
) -> DamageApply {
    let mut result = DamageApply {
        requested_damage: damage,
        resulting_health: target.health,
        ..DamageApply::default()
    };
    if !damage.is_finite() || damage <= 0.0 || !target.health.is_finite() || target.health <= 0.0 {
        return result;
    }

    let effective_damage = target.health.min(damage);
    result.effective_damage = effective_damage;
    match target.kind {
        UnitKind::Armor => {
            let before = target.equipment;
            let next_health = (target.health - effective_damage).max(0.0);
            let maximum = if target.max_equipment == 0 {
                before
            } else {
                target.max_equipment
            };
            let proportional = ((maximum as f64) * (next_health / UNIT_HEALTH)).ceil();
            let next = before.min(proportional.max(0.0) as u64);
            result.equipment_loss = before - next;
            result.personnel_loss = result.equipment_loss.saturating_mul(crew_per_vehicle);
            target.equipment = next;
        }
        UnitKind::Army => {
            let before = target.personnel;
            let max_health = if target.max_health == 0.0 {
                UNIT_HEALTH
            } else {
                target.max_health
            }
            .max(1.0);
            let capacity = if target.personnel_capacity == 0 {
                before
            } else {
                target.personnel_capacity
            }
            .max(before);
            let next_health = (target.health - effective_damage).max(0.0);
            let next = before.min(
                ((capacity as f64) * (next_health / max_health))
                    .round()
                    .max(0.0) as u64,
            );
            result.personnel_loss = before - next;
            target.personnel = next;
            // `applyLandUnitDamage` zeroes health before its final subtraction.
            if target.personnel == 0 {
                target.health = 0.0;
            }
        }
    }
    target.health = (target.health - effective_damage).max(0.0);
    result.resulting_health = target.health;
    result
}

pub fn resolve_proximity_contact(
    units: &mut [CombatUnit],
    attacker_idx: usize,
    target_idx: usize,
    context: &CombatContext<'_>,
    config: &CombatConfig,
) -> Result<Option<CombatEvent>, CombatError> {
    validate_pair(units, attacker_idx, target_idx, context, config)?;
    let distance_squared = {
        let (attacker, target) = (&units[attacker_idx], &units[target_idx]);
        wrapped_distance_squared(attacker, target)
    };
    // Browser proximity contact uses a strict `< 0.09` gate.
    if distance_squared >= config.proximity_radius * config.proximity_radius
        || context.frame < context.war_grace_end
    {
        return Ok(None);
    }

    let (attacker, target) = two_units_mut(units, attacker_idx, target_idx);
    let mut proximity_damage = config.combat_damage
        * 0.45
        * context.attacker_damage_dealt_multiplier
        * (1.0 - distance_squared.sqrt() / config.proximity_radius);
    if attacker.at_sea && target.at_sea {
        proximity_damage *= 2.2;
    }
    if target.transport && !attacker.transport {
        proximity_damage *= 1.05;
    }
    let transport_self_damage = if attacker.transport && !target.transport {
        let damage = proximity_damage * 1.05 * context.attacker_damage_taken_multiplier;
        proximity_damage *= 0.85;
        damage
    } else {
        0.0
    };

    // Preserve the browser's immediate expression/effect order. Each later
    // combined-arms calculation observes the casualties from the prior hit.
    let extra_apply = apply_land_unit_damage(
        attacker,
        transport_self_damage,
        config.armor_crew_per_vehicle,
    );
    let target_damage = combined_arms_damage(
        proximity_damage,
        attacker,
        target,
        context.mountain,
        context.urban,
    );
    let target_apply = apply_land_unit_damage(target, target_damage, config.armor_crew_per_vehicle);
    let attacker_damage = combined_arms_damage_with_landing_penalty(
        proximity_damage * 0.8 * context.attacker_damage_taken_multiplier,
        target,
        attacker,
        context.mountain,
        context.urban,
        false,
    );
    let attacker_apply =
        apply_land_unit_damage(attacker, attacker_damage, config.armor_crew_per_vehicle);
    attacker.last_combat_tick = context.frame;
    target.last_combat_tick = context.frame;
    if target.health <= 0.0 {
        attacker.victory_boost_ticks = 240;
    }

    Ok(Some(CombatEvent {
        schema_version: COMBAT_SCHEMA_VERSION,
        layer: CombatLayer::Proximity,
        attacker_id: attacker.id,
        target_id: target.id,
        target_damage,
        attacker_damage,
        transport_self_damage,
        target_personnel_loss: target_apply.personnel_loss,
        attacker_personnel_loss: extra_apply
            .personnel_loss
            .saturating_add(attacker_apply.personnel_loss),
        target_equipment_loss: target_apply.equipment_loss,
        attacker_equipment_loss: extra_apply
            .equipment_loss
            .saturating_add(attacker_apply.equipment_loss),
        target_resulting_health: target.health,
        attacker_resulting_health: attacker.health,
        target_knockback_blocked: false,
        attacker_knockback_blocked: false,
    }))
}

pub fn resolve_direct_engagement(
    units: &mut [CombatUnit],
    attacker_idx: usize,
    target_idx: usize,
    context: &CombatContext<'_>,
    config: &CombatConfig,
) -> Result<Option<CombatEvent>, CombatError> {
    validate_pair(units, attacker_idx, target_idx, context, config)?;
    let distance = {
        let (attacker, target) = (&units[attacker_idx], &units[target_idx]);
        jittered_target_distance(attacker, target, config.target_jitter_scale)
    };
    // Browser moves only when `dist > 0.05`; equality enters direct combat.
    if distance > config.direct_radius {
        return Ok(None);
    }

    let (attacker, target) = two_units_mut(units, attacker_idx, target_idx);
    let target_damage = combined_arms_damage(
        config.combat_damage * context.attacker_damage_dealt_multiplier * 0.7,
        attacker,
        target,
        context.mountain,
        context.urban,
    );
    let attacker_damage = combined_arms_damage_with_landing_penalty(
        config.combat_damage
            * 0.8
            * context.attacker_damage_taken_multiplier
            * context.defense_bonus
            * context.long_war_defense,
        target,
        attacker,
        context.mountain,
        context.urban,
        false,
    );

    attacker.last_combat_tick = context.frame;
    target.last_combat_tick = context.frame;
    let target_apply = apply_land_unit_damage(target, target_damage, config.armor_crew_per_vehicle);
    let attacker_apply =
        apply_land_unit_damage(attacker, attacker_damage, config.armor_crew_per_vehicle);

    let (target_blocked, attacker_blocked) = apply_direct_knockback(
        attacker,
        target,
        target_damage,
        attacker_damage,
        context.world,
        config,
    );
    if target.health <= 0.0 {
        attacker.victory_boost_ticks = 180;
    }

    Ok(Some(CombatEvent {
        schema_version: COMBAT_SCHEMA_VERSION,
        layer: CombatLayer::Direct,
        attacker_id: attacker.id,
        target_id: target.id,
        target_damage,
        attacker_damage,
        transport_self_damage: 0.0,
        target_personnel_loss: target_apply.personnel_loss,
        attacker_personnel_loss: attacker_apply.personnel_loss,
        target_equipment_loss: target_apply.equipment_loss,
        attacker_equipment_loss: attacker_apply.equipment_loss,
        target_resulting_health: target.health,
        attacker_resulting_health: attacker.health,
        target_knockback_blocked: target_blocked,
        attacker_knockback_blocked: attacker_blocked,
    }))
}

fn apply_direct_knockback(
    attacker: &mut CombatUnit,
    target: &mut CombatUnit,
    target_damage: f64,
    attacker_damage: f64,
    world: Option<WorldGridView<'_>>,
    config: &CombatConfig,
) -> (bool, bool) {
    let d_lat = target.lat - attacker.lat;
    let d_lng = wrapped_longitude_delta(attacker.lng, target.lng);
    let distance_squared = d_lat * d_lat + d_lng * d_lng;
    if distance_squared <= 0.0 {
        return (false, false);
    }
    let distance = distance_squared.sqrt();
    let nx = d_lng / distance;
    let ny = d_lat / distance;
    let base_push = if attacker.at_sea {
        config.unit_naval_speed
    } else {
        config.unit_speed
    } * 1.2;
    let total_damage = {
        let total = target_damage + attacker_damage;
        if total == 0.0 { 1e-6 } else { total }
    };
    let target_factor = (target_damage / total_damage * 1.5).min(1.5);
    let self_factor = (attacker_damage / total_damage).min(1.0);
    let target_move = (
        ny * base_push * target_factor,
        nx * base_push * target_factor,
    );
    let attacker_move = (
        -ny * base_push * 0.5 * self_factor,
        -nx * base_push * 0.5 * self_factor,
    );

    // Browser applies target push before attacker recoil.
    let target_blocked = apply_push(target, target_move.0, target_move.1, world);
    let attacker_blocked = apply_push(attacker, attacker_move.0, attacker_move.1, world);
    (target_blocked, attacker_blocked)
}

/// Returns true only when the browser-style water guard blocks the push.
fn apply_push(
    unit: &mut CombatUnit,
    d_lat: f64,
    d_lng: f64,
    world: Option<WorldGridView<'_>>,
) -> bool {
    if !d_lat.is_finite() || !d_lng.is_finite() {
        return false;
    }
    let new_lat = (unit.lat + d_lat).clamp(-89.9, 89.9);
    let mut new_lng = unit.lng + d_lng;
    if new_lng > 180.0 {
        new_lng -= 360.0;
    } else if new_lng < -180.0 {
        new_lng += 360.0;
    }
    if !unit.at_sea && world.is_some_and(|grid| grid.is_water(new_lat, new_lng)) {
        return true;
    }
    unit.lat = new_lat;
    unit.lng = new_lng;
    false
}

fn validate_pair(
    units: &[CombatUnit],
    attacker_idx: usize,
    target_idx: usize,
    context: &CombatContext<'_>,
    config: &CombatConfig,
) -> Result<(), CombatError> {
    if attacker_idx >= units.len() {
        return Err(CombatError::IndexOutOfBounds {
            index: attacker_idx,
            len: units.len(),
        });
    }
    if target_idx >= units.len() {
        return Err(CombatError::IndexOutOfBounds {
            index: target_idx,
            len: units.len(),
        });
    }
    if attacker_idx == target_idx {
        return Err(CombatError::SameUnit);
    }
    if context.world.is_some_and(|world| world.validate().is_err()) {
        return Err(CombatError::InvalidWorld);
    }
    let finite_config = [
        config.combat_damage,
        config.proximity_radius,
        config.direct_radius,
        config.target_jitter_scale,
        config.unit_speed,
        config.unit_naval_speed,
    ]
    .into_iter()
    .all(f64::is_finite);
    let finite_context = [
        context.attacker_damage_dealt_multiplier,
        context.attacker_damage_taken_multiplier,
        context.defense_bonus,
        context.long_war_defense,
    ]
    .into_iter()
    .all(f64::is_finite);
    let finite_units = [&units[attacker_idx], &units[target_idx]]
        .into_iter()
        .all(|unit| {
            [
                unit.lat,
                unit.lng,
                unit.health,
                unit.max_health,
                unit.quality,
            ]
            .into_iter()
            .all(f64::is_finite)
        });
    if !finite_config || !finite_context || !finite_units {
        return Err(CombatError::NonFiniteInput);
    }
    if config.proximity_radius <= 0.0
        || config.direct_radius <= 0.0
        || config.target_jitter_scale < 0.0
        || config.unit_speed <= 0.0
        || config.unit_naval_speed <= 0.0
        || config.combat_damage < 0.0
    {
        return Err(CombatError::InvalidConfig);
    }
    Ok(())
}

fn two_units_mut(
    units: &mut [CombatUnit],
    attacker_idx: usize,
    target_idx: usize,
) -> (&mut CombatUnit, &mut CombatUnit) {
    if attacker_idx < target_idx {
        let (left, right) = units.split_at_mut(target_idx);
        (&mut left[attacker_idx], &mut right[0])
    } else {
        let (left, right) = units.split_at_mut(attacker_idx);
        (&mut right[0], &mut left[target_idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn army(id: u64, lat: f64, lng: f64) -> CombatUnit {
        CombatUnit {
            id,
            side: id,
            sovereign: id,
            kind: UnitKind::Army,
            lat,
            lng,
            health: 100.0,
            max_health: 100.0,
            personnel: 1_000,
            personnel_capacity: 1_000,
            equipment: 0,
            max_equipment: 0,
            quality: 50.0,
            transport: false,
            armor_supported: false,
            landing_penalty_active: false,
            at_sea: false,
            last_combat_tick: 0,
            victory_boost_ticks: 0,
        }
    }

    fn armor(id: u64, lat: f64, lng: f64) -> CombatUnit {
        CombatUnit {
            kind: UnitKind::Armor,
            equipment: 100,
            max_equipment: 100,
            ..army(id, lat, lng)
        }
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }

    #[test]
    fn quality_and_all_matchups_match_combined_arms() {
        close(quality_multiplier(-10.0), 0.75);
        close(quality_multiplier(0.0), 0.75);
        close(quality_multiplier(50.0), 1.0);
        close(quality_multiplier(100.0), 1.25);
        close(quality_multiplier(110.0), 1.25);
        close(
            matchup_multiplier(UnitKind::Army, UnitKind::Army, false, false, false),
            1.0,
        );
        close(
            matchup_multiplier(UnitKind::Armor, UnitKind::Army, false, false, false),
            2.0,
        );
        close(
            matchup_multiplier(UnitKind::Armor, UnitKind::Army, false, true, false),
            1.0,
        );
        close(
            matchup_multiplier(UnitKind::Armor, UnitKind::Army, true, true, false),
            0.65,
        );
        close(
            matchup_multiplier(UnitKind::Army, UnitKind::Armor, false, false, false),
            0.35,
        );
        close(
            matchup_multiplier(UnitKind::Army, UnitKind::Armor, false, true, false),
            0.7,
        );
        close(
            matchup_multiplier(UnitKind::Army, UnitKind::Armor, true, true, false),
            0.8,
        );
        close(
            matchup_multiplier(UnitKind::Armor, UnitKind::Armor, false, false, false),
            1.0,
        );
        close(
            matchup_multiplier(UnitKind::Armor, UnitKind::Armor, false, true, false),
            0.8,
        );
        close(
            matchup_multiplier(UnitKind::Armor, UnitKind::Armor, true, true, false),
            0.6,
        );
        close(
            matchup_multiplier(UnitKind::Armor, UnitKind::Army, false, false, true),
            2.3,
        );
    }

    #[test]
    fn army_casualties_use_nonnegative_js_rounding() {
        let mut unit = army(1, 0.0, 0.0);
        let applied = apply_land_unit_damage(&mut unit, 0.05, 2);
        assert_eq!(applied.personnel_loss, 0); // 999.5 rounds to 1000 in JS.
        assert_eq!(unit.personnel, 1_000);
        close(unit.health, 99.95);
        let applied = apply_land_unit_damage(&mut unit, 0.1, 2);
        assert_eq!(applied.personnel_loss, 1);
        assert_eq!(unit.personnel, 999);
    }

    #[test]
    fn armor_damage_updates_equipment_and_crew() {
        let mut unit = armor(1, 0.0, 0.0);
        let applied = apply_land_unit_damage(&mut unit, 10.1, 2);
        assert_eq!(unit.equipment, 90);
        assert_eq!(applied.equipment_loss, 10);
        assert_eq!(applied.personnel_loss, 20);
        close(unit.health, 89.9);
    }

    #[test]
    fn lethal_personnel_order_zeroes_health_before_final_subtraction() {
        let mut unit = army(1, 0.0, 0.0);
        unit.personnel = 1;
        unit.personnel_capacity = 1;
        let applied = apply_land_unit_damage(&mut unit, 50.1, 2);
        assert_eq!(applied.personnel_loss, 1);
        assert_eq!(unit.personnel, 0);
        assert_eq!(unit.health, 0.0);
    }

    #[test]
    fn proximity_has_strict_boundary_and_war_grace() {
        let config = CombatConfig::default();
        let mut units = vec![army(1, 0.0, 0.0), army(2, 0.3, 0.0)];
        assert!(
            resolve_proximity_contact(&mut units, 0, 1, &CombatContext::default(), &config)
                .unwrap()
                .is_none()
        );
        units[1].lat = 0.299;
        let grace = CombatContext {
            frame: 9,
            war_grace_end: 10,
            ..CombatContext::default()
        };
        assert!(
            resolve_proximity_contact(&mut units, 0, 1, &grace, &config)
                .unwrap()
                .is_none()
        );
        let end = CombatContext {
            frame: 10,
            war_grace_end: 10,
            ..CombatContext::default()
        };
        assert!(
            resolve_proximity_contact(&mut units, 0, 1, &end, &config)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn proximity_transport_and_sea_order_are_exact() {
        let mut attacker = army(1, 0.0, 0.0);
        attacker.transport = true;
        attacker.at_sea = true;
        let mut target = army(2, 0.0, 0.0);
        target.at_sea = true;
        let mut units = vec![attacker, target];
        let event = resolve_proximity_contact(
            &mut units,
            0,
            1,
            &CombatContext::default(),
            &CombatConfig::default(),
        )
        .unwrap()
        .unwrap();
        // 0.7 * .45 * 2.2 = .693; extra=.72765, then base becomes .58905.
        close(event.transport_self_damage, 0.72765);
        // The extra hit reduces the attacker's strength to .993 before its
        // target hit; that hit reduces the target to .994 before retaliation.
        close(event.target_damage, 0.58905 * 0.993);
        close(event.attacker_damage, 0.47124 * 0.994);
        close(units[0].health, 100.0 - 0.72765 - 0.47124 * 0.994);
        close(units[1].health, 100.0 - 0.58905 * 0.993);
        assert_eq!(event.attacker_personnel_loss, 12);
        assert_eq!(event.target_personnel_loss, 6);
    }

    #[test]
    fn proximity_lethal_target_loses_retaliation_strength_and_grants_longer_boost() {
        let config = CombatConfig {
            combat_damage: 1_000.0,
            ..CombatConfig::default()
        };
        let mut units = vec![army(1, 0.0, 0.0), army(2, 0.0, 0.0)];
        let event = resolve_proximity_contact(&mut units, 0, 1, &CombatContext::default(), &config)
            .unwrap()
            .unwrap();
        assert_eq!(event.target_resulting_health, 0.0);
        assert_eq!(event.attacker_damage, 0.0);
        assert_eq!(event.attacker_resulting_health, 100.0);
        assert_eq!(units[0].victory_boost_ticks, 240);
    }

    #[test]
    fn direct_is_asymmetric_and_equality_engages() {
        let config = CombatConfig {
            target_jitter_scale: 0.0,
            ..CombatConfig::default()
        };
        let mut units = vec![army(1, 0.0, 0.0), armor(2, 0.05, 0.0)];
        let context = CombatContext {
            attacker_damage_dealt_multiplier: 2.0,
            attacker_damage_taken_multiplier: 0.5,
            defense_bonus: 0.65,
            long_war_defense: 0.75,
            ..CombatContext::default()
        };
        let event = resolve_direct_engagement(&mut units, 0, 1, &context, &config)
            .unwrap()
            .unwrap();
        close(event.target_damage, 0.7 * 2.0 * 0.7 * 0.35);
        close(event.attacker_damage, 0.7 * 0.8 * 0.5 * 0.65 * 0.75 * 2.0);
    }

    #[test]
    fn direct_lethal_target_still_retaliates_and_grants_boost() {
        let config = CombatConfig {
            target_jitter_scale: 0.0,
            combat_damage: 200.0,
            ..CombatConfig::default()
        };
        let mut units = vec![army(1, 0.0, 0.0), army(2, 0.0, 0.0)];
        let event = resolve_direct_engagement(&mut units, 0, 1, &CombatContext::default(), &config)
            .unwrap()
            .unwrap();
        assert_eq!(event.target_resulting_health, 0.0);
        assert_eq!(event.attacker_resulting_health, 0.0);
        assert_eq!(units[0].victory_boost_ticks, 180);
    }

    #[test]
    fn defender_landing_penalty_does_not_reduce_retaliation() {
        let mut defender = armor(2, 0.0, 0.0);
        defender.landing_penalty_active = true;
        let mut proximity_units = vec![army(1, 0.0, 0.0), defender.clone()];
        let proximity = resolve_proximity_contact(
            &mut proximity_units,
            0,
            1,
            &CombatContext::default(),
            &CombatConfig::default(),
        )
        .unwrap()
        .unwrap();
        close(proximity.attacker_damage, 0.7 * 0.45 * 0.8 * 2.0);

        let config = CombatConfig {
            target_jitter_scale: 0.0,
            ..CombatConfig::default()
        };
        let mut direct_units = vec![army(1, 0.0, 0.0), defender];
        let direct =
            resolve_direct_engagement(&mut direct_units, 0, 1, &CombatContext::default(), &config)
                .unwrap()
                .unwrap();
        close(direct.attacker_damage, 0.7 * 0.8 * 2.0);
    }

    #[test]
    fn dateline_distance_and_jitter_take_short_path() {
        let a = army(0, 0.0, 179.99);
        let b = army(2, 0.0, -179.99);
        close(wrapped_distance_squared(&a, &b), 0.0004);
        close(jittered_target_distance(&a, &b, 0.0), 0.02);
    }

    #[test]
    fn knockback_wraps_clamps_and_respects_land_water_guard() {
        // Two longitude cells: first land, second water. Target is pushed east.
        let land = [1, 0];
        let world = WorldGridView::new(180.0, 2, 1, &land).unwrap();
        let config = CombatConfig {
            target_jitter_scale: 0.0,
            direct_radius: 2.0,
            unit_speed: 100.0,
            ..CombatConfig::default()
        };
        let mut units = vec![army(1, 89.89, -1.0), army(2, 89.89, 0.0)];
        let context = CombatContext {
            world: Some(world),
            ..CombatContext::default()
        };
        let event = resolve_direct_engagement(&mut units, 0, 1, &context, &config)
            .unwrap()
            .unwrap();
        assert!(event.target_knockback_blocked);
        assert!(!event.attacker_knockback_blocked);
        assert_eq!(units[1].lng, 0.0);

        // Without a water guard, a northward push clamps latitude.
        let mut north = vec![army(1, 89.89, 0.0), army(2, 89.899, 0.0)];
        let config = CombatConfig {
            target_jitter_scale: 0.0,
            direct_radius: 1.0,
            unit_speed: 100.0,
            ..CombatConfig::default()
        };
        resolve_direct_engagement(&mut north, 0, 1, &CombatContext::default(), &config).unwrap();
        assert_eq!(north[1].lat, 89.9);

        // A one-step eastward push wraps through +180 exactly once.
        let mut seam = vec![army(1, 0.0, 179.89), army(2, 0.0, 179.99)];
        let seam_config = CombatConfig {
            target_jitter_scale: 0.0,
            direct_radius: 1.0,
            unit_speed: 1.0,
            ..CombatConfig::default()
        };
        resolve_direct_engagement(&mut seam, 0, 1, &CombatContext::default(), &seam_config)
            .unwrap();
        assert!(seam[1].lng < -179.0);

        // Sea units are never blocked by the land-unit water guard.
        let mut sea_units = vec![army(1, 89.89, -1.0), army(2, 89.89, 0.0)];
        sea_units[1].at_sea = true;
        let sea_config = CombatConfig {
            target_jitter_scale: 0.0,
            direct_radius: 2.0,
            unit_speed: 100.0,
            ..CombatConfig::default()
        };
        let event = resolve_direct_engagement(&mut sea_units, 0, 1, &context, &sea_config)
            .unwrap()
            .unwrap();
        assert!(!event.target_knockback_blocked);
    }

    #[test]
    fn invalid_indices_aliases_nonfinite_and_bad_config_are_rejected() {
        let mut units = vec![army(1, 0.0, 0.0), army(2, 0.0, 0.0)];
        assert_eq!(
            resolve_proximity_contact(
                &mut units,
                2,
                0,
                &CombatContext::default(),
                &CombatConfig::default()
            ),
            Err(CombatError::IndexOutOfBounds { index: 2, len: 2 })
        );
        assert_eq!(
            resolve_proximity_contact(
                &mut units,
                0,
                0,
                &CombatContext::default(),
                &CombatConfig::default()
            ),
            Err(CombatError::SameUnit)
        );
        units[1].lat = f64::NAN;
        assert_eq!(
            resolve_direct_engagement(
                &mut units,
                0,
                1,
                &CombatContext::default(),
                &CombatConfig::default()
            ),
            Err(CombatError::NonFiniteInput)
        );
        units[1].lat = 0.0;
        let bad = CombatConfig {
            proximity_radius: 0.0,
            ..CombatConfig::default()
        };
        assert_eq!(
            resolve_proximity_contact(&mut units, 0, 1, &CombatContext::default(), &bad),
            Err(CombatError::InvalidConfig)
        );

        let invalid_world = WorldGridView {
            grid_res: 1.0,
            width: 2,
            height: 2,
            land_mask: &[1],
        };
        let bad_context = CombatContext {
            world: Some(invalid_world),
            ..CombatContext::default()
        };
        assert_eq!(
            resolve_direct_engagement(&mut units, 0, 1, &bad_context, &CombatConfig::default()),
            Err(CombatError::InvalidWorld)
        );
    }
}
