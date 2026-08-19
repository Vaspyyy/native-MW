//! Final unit-movement integration shared by native simulation frontends.
//!
//! Target selection and steering happen before this kernel. This module mirrors
//! the browser's final movement, coastline, and geographic-boundary pass.

use thiserror::Error;

use crate::world::WorldGridView;

pub const MOVEMENT_SCHEMA_VERSION: &str = "1";

const COAST_STUCK_LIMIT: u32 = 60;
const LATITUDE_LIMIT: f64 = 89.9;
const DEFLECTION_ANGLES: [f64; 8] = [-90.0, 90.0, -45.0, 45.0, -135.0, 135.0, -30.0, 30.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementState {
    pub lat: f64,
    pub lng: f64,
    pub coast_stuck_ticks: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementFactors {
    pub base_speed: f64,
    pub speed_mult: f64,
    pub plan_speed_mult: f64,
    pub neutral_penalty: f64,
    pub retreat_boost: f64,
    pub push_readiness: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementInput {
    pub state: MovementState,
    pub dir_lat: f64,
    pub dir_lng: f64,
    pub factors: MovementFactors,
    pub is_transport: bool,
    pub is_at_sea: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementOutput {
    pub state: MovementState,
    pub applied_dir_lat: f64,
    pub applied_dir_lng: f64,
    pub move_distance: f64,
    pub coast_blocked: bool,
    pub coast_deflected: bool,
    pub coast_deflect_halved: bool,
    pub abandon_target: bool,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum MovementError {
    #[error("world grid is invalid")]
    InvalidWorld,
    #[error("movement input contains a non-finite numeric value")]
    NonFiniteInput,
}

pub fn integrate_unit_step(
    world: WorldGridView<'_>,
    input: MovementInput,
) -> Result<MovementOutput, MovementError> {
    validate_world(world)?;
    validate_input(input)?;

    let mut dir_lat = input.dir_lat;
    let mut dir_lng = input.dir_lng;
    let mut move_distance = input.factors.base_speed
        * input.factors.speed_mult
        * input.factors.plan_speed_mult
        * input.factors.neutral_penalty
        * input.factors.retreat_boost
        * input.factors.push_readiness
        * 0.8;
    if !move_distance.is_finite() {
        return Err(MovementError::NonFiniteInput);
    }

    let mut coast_blocked = false;
    let mut coast_deflected = false;
    let mut coast_deflect_halved = false;

    if !input.is_transport {
        let destination_lat = input.state.lat + dir_lat * move_distance;
        let destination_lng = input.state.lng + dir_lng * move_distance;
        if world.is_water(destination_lat, destination_lng) && !input.is_at_sea {
            coast_blocked = true;
            let look_distance = move_distance * 3.0;
            for angle in DEFLECTION_ANGLES {
                let radians = angle.to_radians();
                let candidate_lat = dir_lat * radians.cos() - dir_lng * radians.sin();
                let candidate_lng = dir_lat * radians.sin() + dir_lng * radians.cos();
                let land_count = (1..=3)
                    .filter(|sample| {
                        world.is_land(
                            input.state.lat + candidate_lat * look_distance * f64::from(*sample),
                            input.state.lng + candidate_lng * look_distance * f64::from(*sample),
                        )
                    })
                    .count();
                if land_count < 2 {
                    continue;
                }

                let magnitude =
                    (candidate_lat * candidate_lat + candidate_lng * candidate_lng).sqrt();
                if magnitude > 0.0 {
                    dir_lat = candidate_lat / magnitude;
                    dir_lng = candidate_lng / magnitude;
                }
                if world.is_water(
                    input.state.lat + dir_lat * move_distance,
                    input.state.lng + dir_lng * move_distance,
                ) {
                    move_distance *= 0.5;
                    coast_deflect_halved = true;
                }
                coast_deflected = true;
                break;
            }

            if !coast_deflected {
                dir_lat = 0.0;
                dir_lng = 0.0;
                move_distance = 0.0;
            }
        }
    }

    let (coast_stuck_ticks, abandon_target) = if coast_blocked {
        if input.state.coast_stuck_ticks >= COAST_STUCK_LIMIT {
            (0, true)
        } else {
            (input.state.coast_stuck_ticks + 1, false)
        }
    } else {
        (0, false)
    };

    let lat = (input.state.lat + dir_lat * move_distance).clamp(-LATITUDE_LIMIT, LATITUDE_LIMIT);
    let mut lng = input.state.lng + dir_lng * move_distance;
    if lng > 180.0 {
        lng -= 360.0;
    }
    if lng < -180.0 {
        lng += 360.0;
    }

    Ok(MovementOutput {
        state: MovementState {
            lat,
            lng,
            coast_stuck_ticks,
        },
        applied_dir_lat: dir_lat,
        applied_dir_lng: dir_lng,
        move_distance,
        coast_blocked,
        coast_deflected,
        coast_deflect_halved,
        abandon_target,
    })
}

fn validate_world(world: WorldGridView<'_>) -> Result<(), MovementError> {
    world.validate().map_err(|_| MovementError::InvalidWorld)
}

fn validate_input(input: MovementInput) -> Result<(), MovementError> {
    let values = [
        input.state.lat,
        input.state.lng,
        input.dir_lat,
        input.dir_lng,
        input.factors.base_speed,
        input.factors.speed_mult,
        input.factors.plan_speed_mult,
        input.factors.neutral_penalty,
        input.factors.retreat_boost,
        input.factors.push_readiness,
    ];
    if values.into_iter().any(|value| !value.is_finite()) {
        return Err(MovementError::NonFiniteInput);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_land() -> Vec<u8> {
        vec![1; 360 * 180]
    }

    fn world(mask: &[u8]) -> WorldGridView<'_> {
        WorldGridView::new(1.0, 360, 180, mask).unwrap()
    }

    fn input(lat: f64, lng: f64, dir_lat: f64, dir_lng: f64) -> MovementInput {
        MovementInput {
            state: MovementState {
                lat,
                lng,
                coast_stuck_ticks: 0,
            },
            dir_lat,
            dir_lng,
            factors: MovementFactors {
                base_speed: 1.25,
                speed_mult: 1.0,
                plan_speed_mult: 1.0,
                neutral_penalty: 1.0,
                retreat_boost: 1.0,
                push_readiness: 1.0,
            },
            is_transport: false,
            is_at_sea: false,
        }
    }

    fn set_water(mask: &mut [u8], lat: f64, lng: f64) {
        let index = world(mask).grid_index(lat, lng).unwrap();
        mask[index] = 0;
    }

    #[test]
    fn ordinary_land_step_preserves_unnormalized_heading() {
        let mask = all_land();
        let result = integrate_unit_step(world(&mask), input(10.0, 20.0, 0.5, 2.0)).unwrap();
        assert_eq!(result.state.lat, 10.5);
        assert_eq!(result.state.lng, 22.0);
        assert_eq!(result.applied_dir_lat, 0.5);
        assert_eq!(result.applied_dir_lng, 2.0);
        assert_eq!(result.move_distance, 1.0);
        assert!(!result.coast_blocked);
    }

    #[test]
    fn movement_distance_uses_the_complete_scalar_product() {
        let mask = all_land();
        let mut value = input(0.0, 0.0, 1.0, 0.0);
        value.factors = MovementFactors {
            base_speed: 2.0,
            speed_mult: 0.5,
            plan_speed_mult: 0.25,
            neutral_penalty: 0.8,
            retreat_boost: 1.5,
            push_readiness: 0.4,
        };
        let result = integrate_unit_step(world(&mask), value).unwrap();
        assert!((result.move_distance - 0.096).abs() < 1e-15);
    }

    #[test]
    fn longitude_wraps_once_across_antimeridian() {
        let mask = all_land();
        let result = integrate_unit_step(world(&mask), input(0.0, 179.8, 0.0, 1.0)).unwrap();
        assert!((result.state.lng + 179.2).abs() < 1e-12);
    }

    #[test]
    fn exact_positive_180_is_not_wrapped() {
        let mask = all_land();
        let result = integrate_unit_step(world(&mask), input(0.0, 179.0, 0.0, 1.0)).unwrap();
        assert_eq!(result.state.lng, 180.0);
    }

    #[test]
    fn latitude_is_clamped_to_renderer_safe_poles() {
        let mask = all_land();
        let result = integrate_unit_step(world(&mask), input(89.8, 0.0, 1.0, 0.0)).unwrap();
        assert_eq!(result.state.lat, 89.9);
    }

    #[test]
    fn transport_can_enter_water_without_coast_state() {
        let mask = vec![0; 360 * 180];
        let mut value = input(0.0, 0.0, 0.0, 1.0);
        value.is_transport = true;
        value.state.coast_stuck_ticks = 12;
        let result = integrate_unit_step(world(&mask), value).unwrap();
        assert_eq!(result.state.lng, 1.0);
        assert_eq!(result.state.coast_stuck_ticks, 0);
        assert!(!result.coast_blocked);
    }

    #[test]
    fn first_viable_deflection_is_negative_90_degrees() {
        let mut mask = all_land();
        set_water(&mut mask, 0.0, 1.0);
        let result = integrate_unit_step(world(&mask), input(0.0, 0.0, 0.0, 1.0)).unwrap();
        assert!(result.coast_blocked);
        assert!(result.coast_deflected);
        assert!(!result.coast_deflect_halved);
        assert!((result.applied_dir_lat - 1.0).abs() < 1e-12);
        assert!(result.applied_dir_lng.abs() < 1e-12);
        assert!((result.state.lat - 1.0).abs() < 1e-12);
    }

    #[test]
    fn viable_deflection_is_halved_when_its_endpoint_is_water() {
        let mut mask = all_land();
        set_water(&mut mask, 0.0, 1.0);
        set_water(&mut mask, 1.0, 0.0);
        let result = integrate_unit_step(world(&mask), input(0.0, 0.0, 0.0, 1.0)).unwrap();
        assert!(result.coast_deflected);
        assert!(result.coast_deflect_halved);
        assert_eq!(result.move_distance, 0.5);
        assert!((result.state.lat - 0.5).abs() < 1e-12);
    }

    #[test]
    fn no_viable_coast_route_stops_the_unit() {
        let mut mask = vec![0; 360 * 180];
        let current = world(&mask).grid_index(0.0, 0.0).unwrap();
        mask[current] = 1;
        let result = integrate_unit_step(world(&mask), input(0.0, 0.0, 0.0, 1.0)).unwrap();
        assert!(result.coast_blocked);
        assert!(!result.coast_deflected);
        assert_eq!(result.applied_dir_lat, 0.0);
        assert_eq!(result.applied_dir_lng, 0.0);
        assert_eq!(result.move_distance, 0.0);
    }

    #[test]
    fn at_sea_nontransport_keeps_moving_without_a_coast_block() {
        let mask = vec![0; 360 * 180];
        let mut value = input(0.0, 0.0, 0.0, 1.0);
        value.is_at_sea = true;
        value.state.coast_stuck_ticks = 12;
        let result = integrate_unit_step(world(&mask), value).unwrap();
        assert!(!result.coast_blocked);
        assert!(!result.coast_deflected);
        assert_eq!(result.state.lng, 1.0);
        assert_eq!(result.state.coast_stuck_ticks, 0);
    }

    #[test]
    fn coast_stuck_threshold_abandons_and_resets_after_60_ticks() {
        let mut mask = vec![0; 360 * 180];
        let current = world(&mask).grid_index(0.0, 0.0).unwrap();
        mask[current] = 1;
        let mut value = input(0.0, 0.0, 0.0, 1.0);
        value.state.coast_stuck_ticks = 60;
        let result = integrate_unit_step(world(&mask), value).unwrap();
        assert!(result.abandon_target);
        assert_eq!(result.state.coast_stuck_ticks, 0);

        value.state.coast_stuck_ticks = 59;
        let result = integrate_unit_step(world(&mask), value).unwrap();
        assert!(!result.abandon_target);
        assert_eq!(result.state.coast_stuck_ticks, 60);
    }

    #[test]
    fn zero_direction_remains_zero() {
        let mask = all_land();
        let result = integrate_unit_step(world(&mask), input(4.0, 5.0, 0.0, 0.0)).unwrap();
        assert_eq!(result.state.lat, 4.0);
        assert_eq!(result.state.lng, 5.0);
        assert_eq!(result.applied_dir_lat, 0.0);
        assert_eq!(result.applied_dir_lng, 0.0);
    }

    #[test]
    fn rejects_nonfinite_input_and_invalid_world() {
        let mask = all_land();
        let mut value = input(0.0, 0.0, 1.0, 0.0);
        value.factors.push_readiness = f64::NAN;
        assert_eq!(
            integrate_unit_step(world(&mask), value),
            Err(MovementError::NonFiniteInput)
        );

        let invalid = WorldGridView {
            grid_res: 1.0,
            width: 2,
            height: 2,
            land_mask: &[1],
        };
        assert_eq!(
            integrate_unit_step(invalid, input(0.0, 0.0, 1.0, 0.0)),
            Err(MovementError::InvalidWorld)
        );
    }
}
