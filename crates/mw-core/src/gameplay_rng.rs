//! Replay-safe gameplay randomness matching the browser's seeded stream.
//!
//! The state transition and output conversion intentionally mirror
//! `createSeededRng` in the browser runtime. Keep this stream separate from
//! stateless identity hashes and cadence jitter.

use serde::{Deserialize, Serialize};

pub const GAMEPLAY_RNG_SCHEMA_VERSION: &str = "native-gameplay-rng-v1";
pub const GAMEPLAY_RNG_ALGORITHM: &str = "mulberry32";
pub const DEFAULT_GAMEPLAY_RNG_SEED: u32 = 0x4d57_5031;
const STEP: u32 = 0x6d2b_79f5;
const UINT32_RANGE: f64 = 4_294_967_296.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameplayRngState {
    pub state: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GameplayRng {
    state: u32,
}

impl GameplayRng {
    pub const fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    pub const fn restore(state: GameplayRngState) -> Self {
        Self { state: state.state }
    }

    pub const fn state(self) -> GameplayRngState {
        GameplayRngState { state: self.state }
    }

    pub fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_add(STEP);
        let mut value = self.state;
        value = (value ^ (value >> 15)).wrapping_mul(value | 1);
        value ^= value.wrapping_add((value ^ (value >> 7)).wrapping_mul(value | 61));
        value ^ (value >> 14)
    }

    pub fn next_f64(&mut self) -> f64 {
        f64::from(self.next_u32()) / UINT32_RANGE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_browser_mulberry32_known_answers() {
        let mut rng = GameplayRng::new(0);
        assert_eq!(rng.next_u32(), 1_144_304_738);
        assert_eq!(rng.next_u32(), 1_416_247);
        assert_eq!(rng.next_u32(), 958_946_056);
        assert_eq!(rng.state().state, STEP.wrapping_mul(3));
    }

    #[test]
    fn restoring_cursor_continues_exactly() {
        let mut uninterrupted = GameplayRng::new(0x4d57_5031);
        let _ = uninterrupted.next_u32();
        let state = uninterrupted.state();
        let expected = uninterrupted.next_u32();

        let mut restored = GameplayRng::restore(state);
        assert_eq!(restored.next_u32(), expected);
        assert_eq!(restored.state(), uninterrupted.state());
    }

    #[test]
    fn floating_draw_uses_the_browser_uint32_divisor() {
        let mut integer = GameplayRng::new(17);
        let expected = f64::from(integer.next_u32()) / UINT32_RANGE;
        let mut floating = GameplayRng::new(17);
        assert_eq!(floating.next_f64().to_bits(), expected.to_bits());
        assert!((0.0..1.0).contains(&expected));
    }
}
