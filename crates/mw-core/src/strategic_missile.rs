//! Browser-parity autonomous strategic missiles and observer-visible effects.
//!
//! This module owns only deterministic missile state and launch/flight decisions. The runtime
//! remains responsible for applying returned land-damage commands atomically with the rest of a
//! native tick.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    gameplay_rng::GameplayRng,
    simulation::{DamageCommand, SimulationUnit},
};

pub const STRATEGIC_MISSILE_SCHEMA_VERSION: &str = "native-strategic-missile-v1";
pub const MISSILE_LAUNCH_CHANCE: f64 = 0.01;
pub const MISSILE_BASE_DAMAGE_MULTIPLIER: f64 = 4.0;
pub const MISSILE_DAMAGE_RADIUS: f64 = 0.5;
pub const MISSILE_EXPLOSION_LIFE: u32 = 30;
pub const MISSILE_EXPLOSION_MAX_RADIUS: f64 = 20.0;
pub const MISSILE_TRAIL_LIMIT: usize = 40;

const MISSILE_BASE_STEP: f64 = 0.0055;
const MISSILE_NEXT_POSITION_STEP: f64 = 0.005;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MissilePoint {
    pub lat: f64,
    pub lng: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MissileBase {
    pub lat: f64,
    pub lng: f64,
    pub side_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissilePhase {
    Rising,
    Falling,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StrategicMissile {
    /// Browser `launchBomb()` consumes a gameplay draw for this otherwise cosmetic identity.
    pub id: f64,
    pub start_lat: f64,
    pub start_lng: f64,
    pub target_lat: f64,
    pub target_lng: f64,
    pub current_lat: f64,
    pub current_lng: f64,
    pub next_lat: f64,
    pub next_lng: f64,
    pub progress: f64,
    pub side_index: usize,
    pub phase: MissilePhase,
    pub trail: Vec<MissilePoint>,
    pub peak_alt: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MissileExplosion {
    pub lat: f64,
    pub lng: f64,
    pub life: u32,
    pub max_radius: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StrategicMissileState {
    pub schema: String,
    pub enabled: bool,
    /// Resolved date/technology gate. Exact handoffs retain the browser decision without porting
    /// campaign time into native sandbox mode.
    pub technology_allowed: bool,
    pub bases: Vec<MissileBase>,
    pub missiles: Vec<StrategicMissile>,
    pub explosions: Vec<MissileExplosion>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MissileImpact {
    pub side_index: usize,
    pub target: MissilePoint,
    pub damage_commands: Vec<DamageCommand>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StrategicMissileAdvanceOutcome {
    pub launches: usize,
    pub impacts: Vec<MissileImpact>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StrategicMissileError {
    #[error("strategic missile state is invalid: {0}")]
    InvalidState(&'static str),
    #[error("strategic missile topology is invalid")]
    InvalidTopology,
}

impl StrategicMissileState {
    pub fn disabled() -> Self {
        Self {
            schema: STRATEGIC_MISSILE_SCHEMA_VERSION.to_owned(),
            enabled: false,
            technology_allowed: false,
            bases: Vec::new(),
            missiles: Vec::new(),
            explosions: Vec::new(),
        }
    }

    /// Build browser-style silo positions from row-major side-controlled cells. Each silo consumes
    /// one draw from the shared gameplay stream, matching war initialization. The browser's user
    /// or scenario-name disable flag does not suppress silo generation; only its date gate does.
    pub fn bootstrap(
        controlled_cells_by_side: &[Vec<usize>],
        grid_width: usize,
        grid_resolution: f64,
        enabled: bool,
        technology_allowed: bool,
        rng: &mut GameplayRng,
    ) -> Result<Self, StrategicMissileError> {
        if grid_width == 0 || !grid_resolution.is_finite() || grid_resolution <= 0.0 {
            return Err(StrategicMissileError::InvalidTopology);
        }
        let mut bases = Vec::new();
        if technology_allowed {
            for (side_index, cells) in controlled_cells_by_side.iter().enumerate() {
                if cells.is_empty() {
                    continue;
                }
                let base_count = (cells.len() / 500).clamp(2, 8);
                for _ in 0..base_count {
                    let cell = cells[(rng.next_f64() * cells.len() as f64).floor() as usize];
                    let x = cell % grid_width;
                    let y = cell / grid_width;
                    bases.push(MissileBase {
                        lat: y as f64 * grid_resolution - 90.0 + grid_resolution * 0.5,
                        lng: x as f64 * grid_resolution - 180.0 + grid_resolution * 0.5,
                        side_index,
                    });
                }
            }
        }
        let state = Self {
            schema: STRATEGIC_MISSILE_SCHEMA_VERSION.to_owned(),
            enabled,
            technology_allowed,
            bases,
            missiles: Vec::new(),
            explosions: Vec::new(),
        };
        state.validate(controlled_cells_by_side.len())?;
        Ok(state)
    }

    pub fn validate(&self, side_count: usize) -> Result<(), StrategicMissileError> {
        if self.schema != STRATEGIC_MISSILE_SCHEMA_VERSION {
            return Err(StrategicMissileError::InvalidState("schema"));
        }
        if side_count == 0 {
            return Err(StrategicMissileError::InvalidTopology);
        }
        for base in &self.bases {
            validate_ground_point(base.lat, base.lng)?;
            if base.side_index >= side_count {
                return Err(StrategicMissileError::InvalidState("base side"));
            }
        }
        for missile in &self.missiles {
            if !missile.id.is_finite()
                || !(0.0..1.0).contains(&missile.id)
                || !missile.progress.is_finite()
                || !(0.0..1.0).contains(&missile.progress)
                || !missile.peak_alt.is_finite()
                || !(1.5..4.0).contains(&missile.peak_alt)
                || missile.side_index >= side_count
                || missile.trail.len() > MISSILE_TRAIL_LIMIT
            {
                return Err(StrategicMissileError::InvalidState("missile"));
            }
            validate_ground_point(missile.start_lat, missile.start_lng)?;
            validate_ground_point(missile.target_lat, missile.target_lng)?;
            validate_flight_point(missile.current_lat, missile.current_lng)?;
            validate_flight_point(missile.next_lat, missile.next_lng)?;
            for point in &missile.trail {
                validate_flight_point(point.lat, point.lng)?;
            }
        }
        for explosion in &self.explosions {
            validate_ground_point(explosion.lat, explosion.lng)?;
            if explosion.life == 0
                || explosion.life >= MISSILE_EXPLOSION_LIFE
                || explosion.max_radius.to_bits() != MISSILE_EXPLOSION_MAX_RADIUS.to_bits()
            {
                return Err(StrategicMissileError::InvalidState("explosion"));
            }
        }
        Ok(())
    }

    /// Advance existing flights, decide one autonomous launch, then age explosions in the exact
    /// browser order. Returned damage remains unapplied so the owning runtime can roll back.
    pub fn advance(
        &mut self,
        active_sides: &[u16],
        hostility: &[u8],
        side_count: usize,
        units: &[SimulationUnit],
        combat_damage: f64,
        rng: &mut GameplayRng,
    ) -> Result<StrategicMissileAdvanceOutcome, StrategicMissileError> {
        self.validate(side_count)?;
        if hostility.len() != side_count.saturating_mul(side_count)
            || !combat_damage.is_finite()
            || combat_damage <= 0.0
            || units.iter().any(|unit| {
                unit.combat.side >= side_count as u64
                    || !unit.combat.lat.is_finite()
                    || !unit.combat.lng.is_finite()
            })
        {
            return Err(StrategicMissileError::InvalidTopology);
        }

        if !self.enabled || !self.technology_allowed {
            self.missiles.clear();
        }

        let mut outcome = StrategicMissileAdvanceOutcome::default();
        for missile_index in (0..self.missiles.len()).rev() {
            let missile = &mut self.missiles[missile_index];
            let step = MISSILE_BASE_STEP
                * if missile.phase == MissilePhase::Falling {
                    1.0 + (missile.progress - 0.5) * 2.5
                } else {
                    1.0
                };
            missile.progress += step;
            if missile.phase == MissilePhase::Rising && missile.progress >= 0.5 {
                missile.phase = MissilePhase::Falling;
            }
            let t = missile.progress;
            missile.current_lat = missile.start_lat
                + (missile.target_lat - missile.start_lat) * t
                + (std::f64::consts::PI * t).sin() * missile.peak_alt;
            missile.current_lng = missile.start_lng + (missile.target_lng - missile.start_lng) * t;
            let next_t = (t + MISSILE_NEXT_POSITION_STEP).min(1.0);
            missile.next_lat = missile.start_lat
                + (missile.target_lat - missile.start_lat) * next_t
                + (std::f64::consts::PI * next_t).sin() * missile.peak_alt;
            missile.next_lng =
                missile.start_lng + (missile.target_lng - missile.start_lng) * next_t;
            missile.trail.push(MissilePoint {
                lat: missile.current_lat,
                lng: missile.current_lng,
            });
            if missile.trail.len() > MISSILE_TRAIL_LIMIT {
                missile.trail.remove(0);
            }

            if missile.progress >= 1.0 {
                let target = MissilePoint {
                    lat: missile.target_lat,
                    lng: missile.target_lng,
                };
                self.explosions.push(MissileExplosion {
                    lat: target.lat,
                    lng: target.lng,
                    life: MISSILE_EXPLOSION_LIFE,
                    max_radius: MISSILE_EXPLOSION_MAX_RADIUS,
                });
                let damage_commands = units
                    .iter()
                    .filter_map(|unit| {
                        let unit_side = unit.combat.side as usize;
                        if hostility[missile.side_index * side_count + unit_side] == 0 {
                            return None;
                        }
                        // Deliberately do not wrap longitude: this is the browser expression.
                        let d_lat = unit.combat.lat - target.lat;
                        let d_lng = unit.combat.lng - target.lng;
                        let distance_sq = d_lat * d_lat + d_lng * d_lng;
                        if distance_sq >= MISSILE_DAMAGE_RADIUS * MISSILE_DAMAGE_RADIUS {
                            return None;
                        }
                        let distance = distance_sq.sqrt();
                        let falloff = 1.0 - distance / MISSILE_DAMAGE_RADIUS;
                        Some(DamageCommand {
                            unit_id: unit.combat.id,
                            damage: combat_damage
                                * MISSILE_BASE_DAMAGE_MULTIPLIER
                                * falloff.max(0.2),
                        })
                    })
                    .collect();
                outcome.impacts.push(MissileImpact {
                    side_index: missile.side_index,
                    target,
                    damage_commands,
                });
                self.missiles.remove(missile_index);
            }
        }

        if self.enabled && self.technology_allowed {
            let active_set = active_sides
                .iter()
                .filter_map(|side| {
                    let side = usize::from(*side);
                    (side < side_count).then_some(side)
                })
                .collect::<BTreeSet<_>>();
            let active = active_set.into_iter().collect::<Vec<_>>();
            if active.len() >= 2 && !self.bases.is_empty() && rng.next_f64() < MISSILE_LAUNCH_CHANCE
            {
                let launcher_side = active[(rng.next_f64() * active.len() as f64).floor() as usize];
                let enemies = active
                    .iter()
                    .copied()
                    .filter(|enemy| hostility[launcher_side * side_count + *enemy] != 0)
                    .collect::<Vec<_>>();
                if !enemies.is_empty() {
                    let target_side =
                        enemies[(rng.next_f64() * enemies.len() as f64).floor() as usize];
                    let launchers = self
                        .bases
                        .iter()
                        .filter(|base| base.side_index == launcher_side)
                        .copied()
                        .collect::<Vec<_>>();
                    let targets = units
                        .iter()
                        .filter(|unit| unit.combat.side as usize == target_side)
                        .collect::<Vec<_>>();
                    if !launchers.is_empty() && !targets.is_empty() {
                        let launcher =
                            launchers[(rng.next_f64() * launchers.len() as f64).floor() as usize];
                        let target =
                            targets[(rng.next_f64() * targets.len() as f64).floor() as usize];
                        let id = rng.next_f64();
                        let peak_alt = 1.5 + rng.next_f64() * 2.5;
                        self.missiles.push(StrategicMissile {
                            id,
                            start_lat: launcher.lat,
                            start_lng: launcher.lng,
                            target_lat: target.combat.lat,
                            target_lng: target.combat.lng,
                            current_lat: launcher.lat,
                            current_lng: launcher.lng,
                            next_lat: launcher.lat,
                            next_lng: launcher.lng,
                            progress: 0.0,
                            side_index: launcher_side,
                            phase: MissilePhase::Rising,
                            trail: Vec::new(),
                            peak_alt,
                        });
                        outcome.launches = 1;
                    }
                }
            }
        }

        for index in (0..self.explosions.len()).rev() {
            self.explosions[index].life -= 1;
            if self.explosions[index].life == 0 {
                self.explosions.remove(index);
            }
        }
        self.validate(side_count)?;
        Ok(outcome)
    }
}

fn validate_ground_point(lat: f64, lng: f64) -> Result<(), StrategicMissileError> {
    if !lat.is_finite()
        || !lng.is_finite()
        || !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lng)
    {
        return Err(StrategicMissileError::InvalidState("position"));
    }
    Ok(())
}

fn validate_flight_point(lat: f64, lng: f64) -> Result<(), StrategicMissileError> {
    // The browser adds a positive ballistic arc below four degrees to a linear interpolation
    // between ground points. Longitude remains between its two ground endpoints.
    if !lat.is_finite()
        || !lng.is_finite()
        || !(-90.0..94.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lng)
    {
        return Err(StrategicMissileError::InvalidState("position"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        combat::{CombatUnit, UnitKind},
        gameplay_rng::GameplayRng,
        simulation::SimulationUnit,
    };

    use super::*;

    fn unit(id: u64, side: u64, lat: f64, lng: f64) -> SimulationUnit {
        SimulationUnit {
            combat: CombatUnit {
                id,
                side,
                sovereign: side + 1,
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
            },
            dir_lat: 0.0,
            dir_lng: 0.0,
            coast_stuck_ticks: 0,
            armor_landing_penalty_until_tick: 0,
            is_support: false,
            ally_weight: 1.0,
        }
    }

    fn state_with_missile(progress: f64, phase: MissilePhase) -> StrategicMissileState {
        StrategicMissileState {
            schema: STRATEGIC_MISSILE_SCHEMA_VERSION.to_owned(),
            enabled: true,
            technology_allowed: true,
            bases: vec![MissileBase {
                lat: 0.0,
                lng: 0.0,
                side_index: 0,
            }],
            missiles: vec![StrategicMissile {
                id: 0.25,
                start_lat: 0.0,
                start_lng: 0.0,
                target_lat: 10.0,
                target_lng: 20.0,
                current_lat: 0.0,
                current_lng: 0.0,
                next_lat: 0.0,
                next_lng: 0.0,
                progress,
                side_index: 0,
                phase,
                trail: Vec::new(),
                peak_alt: 2.0,
            }],
            explosions: Vec::new(),
        }
    }

    #[test]
    fn bootstrap_uses_browser_base_count_and_row_major_cells() {
        let mut rng = GameplayRng::new(7);
        let state = StrategicMissileState::bootstrap(
            &[vec![0, 1, 2], vec![3; 4_100]],
            360,
            1.0,
            true,
            true,
            &mut rng,
        )
        .unwrap();
        assert_eq!(state.bases.len(), 10);
        assert_eq!(
            state
                .bases
                .iter()
                .filter(|base| base.side_index == 0)
                .count(),
            2
        );
        assert_eq!(
            state
                .bases
                .iter()
                .filter(|base| base.side_index == 1)
                .count(),
            8
        );
    }

    #[test]
    fn disabled_launches_still_seed_silos_and_advance_the_shared_rng() {
        let mut rng = GameplayRng::new(7);
        let before = rng.state();
        let state = StrategicMissileState::bootstrap(
            &[vec![0, 1], vec![2, 3]],
            360,
            1.0,
            false,
            true,
            &mut rng,
        )
        .unwrap();

        assert!(!state.enabled);
        assert_eq!(state.bases.len(), 4);
        assert_ne!(rng.state(), before);
    }

    #[test]
    fn rising_flight_matches_browser_equations_and_caps_trail() {
        let mut state = state_with_missile(0.49, MissilePhase::Rising);
        state.missiles[0].trail = vec![MissilePoint { lat: 0.0, lng: 0.0 }; 40];
        let mut rng = GameplayRng::new(1);
        state
            .advance(&[0, 1], &[0, 1, 1, 0], 2, &[], 0.7, &mut rng)
            .unwrap();
        let missile = &state.missiles[0];
        assert_eq!(missile.phase, MissilePhase::Rising);
        assert_eq!(missile.progress.to_bits(), 0.4955_f64.to_bits());
        assert_eq!(missile.trail.len(), 40);
        assert_eq!(
            missile.current_lng.to_bits(),
            (20.0_f64 * 0.4955_f64).to_bits()
        );
        let expected_lat = 10.0 * 0.4955 + (std::f64::consts::PI * 0.4955).sin() * 2.0;
        assert_eq!(missile.current_lat.to_bits(), expected_lat.to_bits());
    }

    #[test]
    fn impact_uses_strict_radius_falloff_and_ages_new_explosion() {
        let mut state = state_with_missile(0.999, MissilePhase::Falling);
        let units = vec![
            unit(1, 1, 10.0, 20.0),
            unit(2, 1, 10.3, 20.0),
            unit(3, 1, 10.5, 20.0),
            unit(4, 0, 10.0, 20.0),
        ];
        let mut rng = GameplayRng::new(2);
        let outcome = state
            .advance(&[], &[0, 1, 1, 0], 2, &units, 0.7, &mut rng)
            .unwrap();
        assert!(state.missiles.is_empty());
        assert_eq!(state.explosions[0].life, 29);
        assert_eq!(outcome.impacts.len(), 1);
        assert_eq!(outcome.impacts[0].damage_commands.len(), 2);
        assert_eq!(outcome.impacts[0].damage_commands[0].damage, 2.8);
        assert!((outcome.impacts[0].damage_commands[1].damage - 1.12).abs() < 1e-12);
    }

    #[test]
    fn disabled_state_clears_flights_without_consuming_rng() {
        let mut state = state_with_missile(0.2, MissilePhase::Rising);
        state.enabled = false;
        let mut rng = GameplayRng::new(123);
        let before = rng.state();
        let outcome = state
            .advance(&[0, 1], &[0, 1, 1, 0], 2, &[], 0.7, &mut rng)
            .unwrap();
        assert!(state.missiles.is_empty());
        assert_eq!(outcome, StrategicMissileAdvanceOutcome::default());
        assert_eq!(rng.state(), before);
    }

    #[test]
    fn autonomous_launch_matches_browser_mulberry32_draw_order() {
        let mut state = StrategicMissileState {
            schema: STRATEGIC_MISSILE_SCHEMA_VERSION.to_owned(),
            enabled: true,
            technology_allowed: true,
            bases: vec![
                MissileBase {
                    lat: 1.0,
                    lng: 2.0,
                    side_index: 0,
                },
                MissileBase {
                    lat: 3.0,
                    lng: 4.0,
                    side_index: 1,
                },
            ],
            missiles: Vec::new(),
            explosions: Vec::new(),
        };
        let units = vec![unit(10, 0, 5.0, 6.0), unit(20, 1, 7.0, 8.0)];
        // Seed 35's first browser Mulberry32 draw is 0.007453745696693659.
        let mut rng = GameplayRng::new(35);
        let outcome = state
            .advance(&[0, 1], &[0, 1, 1, 0], 2, &units, 0.7, &mut rng)
            .unwrap();
        assert_eq!(outcome.launches, 1);
        assert!(outcome.impacts.is_empty());
        assert_eq!(state.missiles.len(), 1);
        let missile = &state.missiles[0];
        assert_eq!(missile.side_index, 1);
        assert_eq!((missile.start_lat, missile.start_lng), (3.0, 4.0));
        assert_eq!((missile.target_lat, missile.target_lng), (5.0, 6.0));
        assert_eq!(missile.id.to_bits(), 0.28429954079911113_f64.to_bits());
        assert_eq!(missile.peak_alt.to_bits(), 2.4548331261612475_f64.to_bits());
        assert_eq!(rng.state().state, 4_231_026_134);
    }

    #[test]
    fn explosion_expires_after_its_last_browser_frame() {
        let mut state = StrategicMissileState {
            schema: STRATEGIC_MISSILE_SCHEMA_VERSION.to_owned(),
            enabled: false,
            technology_allowed: true,
            bases: Vec::new(),
            missiles: Vec::new(),
            explosions: vec![MissileExplosion {
                lat: 0.0,
                lng: 0.0,
                life: 1,
                max_radius: MISSILE_EXPLOSION_MAX_RADIUS,
            }],
        };
        state
            .advance(
                &[0, 1],
                &[0, 1, 1, 0],
                2,
                &[],
                0.7,
                &mut GameplayRng::new(1),
            )
            .unwrap();
        assert!(state.explosions.is_empty());
    }

    #[test]
    fn validation_rejects_unbounded_flight_and_trail_coordinates() {
        let mut state = state_with_missile(0.25, MissilePhase::Rising);
        state.missiles[0].current_lat = 1.0e12;
        assert_eq!(
            state.validate(2),
            Err(StrategicMissileError::InvalidState("position"))
        );

        let mut state = state_with_missile(0.25, MissilePhase::Rising);
        state.missiles[0].trail.push(MissilePoint {
            lat: 0.0,
            lng: -181.0,
        });
        assert_eq!(
            state.validate(2),
            Err(StrategicMissileError::InvalidState("position"))
        );
    }
}
