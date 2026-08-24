//! Browser-equivalent simulation-frame admission.
//!
//! Modern Wars owns two clocks: logical simulation ticks and presentation
//! frames. Foreground speed modes may admit several logical ticks while every
//! admitted tick observes the same pre-increment frame value.

use thiserror::Error;

pub const BROWSER_CLOCK_SCHEMA_VERSION: &str = "native-runtime-clock-v1";
pub const BROWSER_MIN_SPEED: u8 = 1;
pub const BROWSER_MAX_SPEED: u8 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowserClockMode {
    #[default]
    Foreground,
    Background,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrowserClockState {
    pub sim_speed: u8,
    pub frame_accumulator: f64,
    pub mode: BrowserClockMode,
    pub paused: bool,
}

impl Default for BrowserClockState {
    fn default() -> Self {
        Self {
            sim_speed: BROWSER_MIN_SPEED,
            frame_accumulator: 0.0,
            mode: BrowserClockMode::Foreground,
            paused: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum BrowserClockError {
    #[error("browser simulation speed must be in the 1x through 3x range")]
    InvalidSpeed,
    #[error("browser frame accumulator must be finite and in the [0, 1] range")]
    InvalidAccumulator,
}

impl BrowserClockState {
    pub fn new(
        sim_speed: u8,
        frame_accumulator: f64,
        mode: BrowserClockMode,
        paused: bool,
    ) -> Result<Self, BrowserClockError> {
        let state = Self {
            sim_speed,
            frame_accumulator,
            mode,
            paused,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(self) -> Result<(), BrowserClockError> {
        if !(BROWSER_MIN_SPEED..=BROWSER_MAX_SPEED).contains(&self.sim_speed) {
            return Err(BrowserClockError::InvalidSpeed);
        }
        if !self.frame_accumulator.is_finite() || !(0.0..=1.0).contains(&self.frame_accumulator) {
            return Err(BrowserClockError::InvalidAccumulator);
        }
        Ok(())
    }

    /// Apply Modern Wars' `setSpeed()`: update the speed and discard residual
    /// work from the prior speed mode.
    pub fn set_speed(&mut self, sim_speed: u8) -> Result<(), BrowserClockError> {
        if !(BROWSER_MIN_SPEED..=BROWSER_MAX_SPEED).contains(&sim_speed) {
            return Err(BrowserClockError::InvalidSpeed);
        }
        self.sim_speed = sim_speed;
        self.frame_accumulator = 0.0;
        Ok(())
    }

    /// Admit one browser frame's logical work and update the exact residual
    /// accumulator. Paused foreground frames still exist, but admit no ticks.
    pub fn admit_frame(&mut self) -> u8 {
        if self.paused {
            return 0;
        }

        self.frame_accumulator += f64::from(self.sim_speed);
        let requested = self.frame_accumulator.floor() as u8;
        let cap = if self.mode == BrowserClockMode::Foreground && self.sim_speed >= 3 {
            2
        } else {
            u8::MAX
        };
        let admitted = requested.min(cap);
        self.frame_accumulator -= f64::from(admitted);
        if self.mode == BrowserClockMode::Foreground
            && self.sim_speed >= 3
            && self.frame_accumulator > 1.0
        {
            self.frame_accumulator = 1.0;
        }
        admitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_speed_batches_match_browser() {
        let mut one = BrowserClockState::default();
        assert_eq!(one.admit_frame(), 1);
        assert_eq!(one.frame_accumulator, 0.0);

        let mut two = BrowserClockState::new(2, 0.0, BrowserClockMode::Foreground, false).unwrap();
        assert_eq!(two.admit_frame(), 2);
        assert_eq!(two.frame_accumulator, 0.0);

        let mut three =
            BrowserClockState::new(3, 0.0, BrowserClockMode::Foreground, false).unwrap();
        assert_eq!(three.admit_frame(), 2);
        assert_eq!(three.frame_accumulator, 1.0);
        assert_eq!(three.admit_frame(), 2);
        assert_eq!(three.frame_accumulator, 1.0);
    }

    #[test]
    fn background_drains_all_requested_subticks() {
        let mut clock =
            BrowserClockState::new(3, 0.5, BrowserClockMode::Background, false).unwrap();
        assert_eq!(clock.admit_frame(), 3);
        assert_eq!(clock.frame_accumulator, 0.5);
    }

    #[test]
    fn pause_and_speed_change_match_browser_state_rules() {
        let mut clock = BrowserClockState::new(3, 1.0, BrowserClockMode::Foreground, true).unwrap();
        assert_eq!(clock.admit_frame(), 0);
        assert_eq!(clock.frame_accumulator, 1.0);
        clock.set_speed(2).unwrap();
        assert_eq!(clock.frame_accumulator, 0.0);
        assert_eq!(clock.admit_frame(), 0);
        clock.paused = false;
        assert_eq!(clock.admit_frame(), 2);
    }

    #[test]
    fn invalid_wire_values_are_rejected() {
        assert_eq!(
            BrowserClockState::new(0, 0.0, BrowserClockMode::Foreground, false),
            Err(BrowserClockError::InvalidSpeed)
        );
        assert_eq!(
            BrowserClockState::new(1, f64::NAN, BrowserClockMode::Foreground, false),
            Err(BrowserClockError::InvalidAccumulator)
        );
        assert_eq!(
            BrowserClockState::new(1, 1.01, BrowserClockMode::Foreground, false),
            Err(BrowserClockError::InvalidAccumulator)
        );
    }
}
