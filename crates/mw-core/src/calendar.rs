//! Browser-sandbox campaign calendar and elapsed-time accumulation.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const GAME_CALENDAR_SCHEMA_VERSION: &str = "native-game-calendar-v1";
pub const DEFAULT_DAY_DURATION_MS: f64 = 500.0;
pub const GAME_CALENDAR_MIN_SPEED: u8 = 1;
pub const GAME_CALENDAR_MAX_SPEED: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameDate {
    pub year: u32,
    pub month: u8,
    pub day: u8,
}

impl GameDate {
    pub fn new(year: u32, month: u8, day: u8) -> Result<Self, GameCalendarError> {
        let date = Self { year, month, day };
        date.validate()?;
        Ok(date)
    }

    pub fn validate(&self) -> Result<(), GameCalendarError> {
        if self.year == 0 {
            return Err(GameCalendarError::InvalidYear);
        }
        if !(1..=12).contains(&self.month) {
            return Err(GameCalendarError::InvalidMonth);
        }
        if self.day == 0 || self.day > days_in_month(self.year, self.month) {
            return Err(GameCalendarError::InvalidDay);
        }
        Ok(())
    }

    pub const fn is_leap_year(year: u32) -> bool {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    }

    pub fn advance_one_day(&mut self) -> Result<(), GameCalendarError> {
        self.validate()?;
        if self.day < days_in_month(self.year, self.month) {
            self.day += 1;
        } else if self.month < 12 {
            self.month += 1;
            self.day = 1;
        } else {
            self.year = self
                .year
                .checked_add(1)
                .ok_or(GameCalendarError::DateOverflow)?;
            self.month = 1;
            self.day = 1;
        }
        Ok(())
    }
}

impl fmt::Display for GameDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}/{:02}/{:02}",
            self.year, self.month, self.day
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameCalendarState {
    pub schema: String,
    pub date: GameDate,
    pub accumulator_ms: f64,
    pub day_duration_ms: f64,
}

impl GameCalendarState {
    pub fn new(date: GameDate) -> Result<Self, GameCalendarError> {
        let state = Self {
            schema: GAME_CALENDAR_SCHEMA_VERSION.to_owned(),
            date,
            accumulator_ms: 0.0,
            day_duration_ms: DEFAULT_DAY_DURATION_MS,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), GameCalendarError> {
        if self.schema != GAME_CALENDAR_SCHEMA_VERSION {
            return Err(GameCalendarError::InvalidSchema);
        }
        self.date.validate()?;
        if self.day_duration_ms.to_bits() != DEFAULT_DAY_DURATION_MS.to_bits() {
            return Err(GameCalendarError::InvalidDayDuration);
        }
        if !self.accumulator_ms.is_finite()
            || self.accumulator_ms < 0.0
            || self.accumulator_ms >= self.day_duration_ms
        {
            return Err(GameCalendarError::InvalidAccumulator);
        }
        Ok(())
    }

    /// Add elapsed wall time at the active browser speed and return the number
    /// of whole campaign days consumed.
    pub fn advance_elapsed(
        &mut self,
        elapsed_ms: f64,
        speed: u8,
    ) -> Result<u64, GameCalendarError> {
        self.validate()?;
        if !elapsed_ms.is_finite() || elapsed_ms < 0.0 {
            return Err(GameCalendarError::InvalidElapsed);
        }
        if !(GAME_CALENDAR_MIN_SPEED..=GAME_CALENDAR_MAX_SPEED).contains(&speed) {
            return Err(GameCalendarError::InvalidSpeed);
        }

        let scaled_elapsed = elapsed_ms * f64::from(speed);
        if !scaled_elapsed.is_finite() || !((self.accumulator_ms + scaled_elapsed).is_finite()) {
            return Err(GameCalendarError::InvalidElapsed);
        }
        self.accumulator_ms += scaled_elapsed;

        let mut days_advanced = 0_u64;
        while self.accumulator_ms >= self.day_duration_ms {
            self.date.advance_one_day()?;
            self.accumulator_ms -= self.day_duration_ms;
            days_advanced = days_advanced
                .checked_add(1)
                .ok_or(GameCalendarError::DateOverflow)?;
        }
        Ok(days_advanced)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum GameCalendarError {
    #[error("game calendar schema is invalid")]
    InvalidSchema,
    #[error("game date year must be positive")]
    InvalidYear,
    #[error("game date month must be in the 1 through 12 range")]
    InvalidMonth,
    #[error("game date day is invalid for its month")]
    InvalidDay,
    #[error("game calendar accumulator must be finite, nonnegative, and less than one day")]
    InvalidAccumulator,
    #[error("game calendar day duration must match the 500 ms browser sandbox clock")]
    InvalidDayDuration,
    #[error("elapsed milliseconds must be finite and nonnegative")]
    InvalidElapsed,
    #[error("game calendar speed must be in the 1x through 3x range")]
    InvalidSpeed,
    #[error("game date exceeded the supported year range")]
    DateOverflow,
}

const fn days_in_month(year: u32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if GameDate::is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_and_displays_strict_dates() {
        let date = GameDate::new(7, 1, 9).unwrap();
        assert_eq!(date.to_string(), "0007/01/09");
        assert_eq!(GameDate::new(0, 1, 1), Err(GameCalendarError::InvalidYear));
        assert_eq!(
            GameDate::new(2024, 0, 1),
            Err(GameCalendarError::InvalidMonth)
        );
        assert_eq!(
            GameDate::new(2024, 13, 1),
            Err(GameCalendarError::InvalidMonth)
        );
        assert_eq!(
            GameDate::new(2024, 1, 0),
            Err(GameCalendarError::InvalidDay)
        );
        assert_eq!(
            GameDate::new(2023, 2, 29),
            Err(GameCalendarError::InvalidDay)
        );
    }

    #[test]
    fn gregorian_leap_and_century_rollovers_are_exact() {
        let mut leap = GameDate::new(2024, 2, 28).unwrap();
        leap.advance_one_day().unwrap();
        assert_eq!(leap, GameDate::new(2024, 2, 29).unwrap());
        leap.advance_one_day().unwrap();
        assert_eq!(leap, GameDate::new(2024, 3, 1).unwrap());

        let mut common_century = GameDate::new(1900, 2, 28).unwrap();
        common_century.advance_one_day().unwrap();
        assert_eq!(common_century, GameDate::new(1900, 3, 1).unwrap());

        let mut leap_century = GameDate::new(2000, 2, 28).unwrap();
        leap_century.advance_one_day().unwrap();
        assert_eq!(leap_century, GameDate::new(2000, 2, 29).unwrap());
    }

    #[test]
    fn rolls_month_and_year_boundaries() {
        let mut date = GameDate::new(2023, 4, 30).unwrap();
        date.advance_one_day().unwrap();
        assert_eq!(date, GameDate::new(2023, 5, 1).unwrap());

        let mut year_end = GameDate::new(2023, 12, 31).unwrap();
        year_end.advance_one_day().unwrap();
        assert_eq!(year_end, GameDate::new(2024, 1, 1).unwrap());
    }

    #[test]
    fn elapsed_time_advances_multiple_days_and_preserves_residual() {
        let mut calendar = GameCalendarState::new(GameDate::new(2023, 12, 30).unwrap()).unwrap();
        assert_eq!(calendar.advance_elapsed(625.0, 2).unwrap(), 2);
        assert_eq!(calendar.date, GameDate::new(2024, 1, 1).unwrap());
        assert_eq!(calendar.accumulator_ms, 250.0);

        assert_eq!(calendar.advance_elapsed(250.0, 1).unwrap(), 1);
        assert_eq!(calendar.date, GameDate::new(2024, 1, 2).unwrap());
        assert_eq!(calendar.accumulator_ms, 0.0);
    }

    #[test]
    fn constructor_and_serde_shape_are_canonical_and_strict() {
        let calendar = GameCalendarState::new(GameDate::new(2024, 6, 15).unwrap()).unwrap();
        assert_eq!(calendar.schema, GAME_CALENDAR_SCHEMA_VERSION);
        assert_eq!(calendar.day_duration_ms, DEFAULT_DAY_DURATION_MS);
        assert_eq!(
            serde_json::to_value(&calendar).unwrap(),
            json!({
                "schema": "native-game-calendar-v1",
                "date": { "year": 2024, "month": 6, "day": 15 },
                "accumulatorMs": 0.0,
                "dayDurationMs": 500.0
            })
        );

        let mut unknown = serde_json::to_value(&calendar).unwrap();
        unknown["extra"] = json!(true);
        assert!(serde_json::from_value::<GameCalendarState>(unknown).is_err());

        let mut unknown_date = serde_json::to_value(&calendar).unwrap();
        unknown_date["date"]["extra"] = json!(true);
        assert!(serde_json::from_value::<GameCalendarState>(unknown_date).is_err());
    }

    #[test]
    fn rejects_invalid_state_elapsed_and_speed_without_mutating() {
        let valid = GameCalendarState::new(GameDate::new(2024, 1, 1).unwrap()).unwrap();
        for elapsed in [f64::NAN, f64::INFINITY, -1.0] {
            let mut calendar = valid.clone();
            assert_eq!(
                calendar.advance_elapsed(elapsed, 1),
                Err(GameCalendarError::InvalidElapsed)
            );
            assert_eq!(calendar, valid);
        }
        for speed in [0, 4] {
            let mut calendar = valid.clone();
            assert_eq!(
                calendar.advance_elapsed(100.0, speed),
                Err(GameCalendarError::InvalidSpeed)
            );
            assert_eq!(calendar, valid);
        }

        let mut bad_schema = valid.clone();
        bad_schema.schema = "other".to_owned();
        assert_eq!(bad_schema.validate(), Err(GameCalendarError::InvalidSchema));

        let mut bad_accumulator = valid.clone();
        bad_accumulator.accumulator_ms = DEFAULT_DAY_DURATION_MS;
        assert_eq!(
            bad_accumulator.validate(),
            Err(GameCalendarError::InvalidAccumulator)
        );

        let mut bad_duration = valid.clone();
        bad_duration.day_duration_ms = 0.0;
        assert_eq!(
            bad_duration.validate(),
            Err(GameCalendarError::InvalidDayDuration)
        );

        let mut noncanonical_duration = valid;
        noncanonical_duration.day_duration_ms = 250.0;
        assert_eq!(
            noncanonical_duration.validate(),
            Err(GameCalendarError::InvalidDayDuration)
        );
    }
}
