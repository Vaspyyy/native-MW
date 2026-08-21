//! Deterministic side momentum, war phase, and AI posture state.

use std::collections::{BTreeMap, VecDeque};

pub const MOMENTUM_SAMPLE_INTERVAL: u64 = 200;
pub const MOMENTUM_SAMPLE_OFFSET: u64 = 37;
pub const MOMENTUM_WINDOW: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarPhase {
    Advancing,
    Stalemate,
    Retreating,
    Collapsing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarPosture {
    Offensive,
    Balanced,
    Defensive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MomentumSample {
    pub frame: u64,
    pub controlled: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SideDynamics {
    pub side_index: usize,
    pub initial_personnel: f64,
    pub current_personnel: f64,
    pub momentum_samples: VecDeque<MomentumSample>,
    pub phase: WarPhase,
    pub posture: WarPosture,
    /// Captured browser strategic/reaction override. Native refreshes strength-based posture,
    /// while this frozen input preserves the last known higher-priority browser decision until
    /// the country-desperation and defender-reaction planners are ported.
    pub posture_override: Option<WarPosture>,
}

impl SideDynamics {
    pub fn bootstrap(side_index: usize, personnel: f64) -> Self {
        let personnel = if personnel.is_finite() {
            personnel.max(0.0)
        } else {
            0.0
        };
        Self {
            side_index,
            initial_personnel: personnel,
            current_personnel: personnel,
            momentum_samples: VecDeque::new(),
            phase: WarPhase::Stalemate,
            posture: WarPosture::Balanced,
            posture_override: None,
        }
    }

    pub fn validate(&self, checkpoint_frame: u64, controlled_cell_limit: u64) -> bool {
        self.initial_personnel.is_finite()
            && self.initial_personnel >= 0.0
            && self.current_personnel.is_finite()
            && self.current_personnel >= 0.0
            && self.posture_override != Some(WarPosture::Balanced)
            && self.momentum_samples.len() <= MOMENTUM_WINDOW
            && self.momentum_samples.iter().all(|sample| {
                sample.frame <= checkpoint_frame && sample.controlled <= controlled_cell_limit
            })
            && self
                .momentum_samples
                .iter()
                .zip(self.momentum_samples.iter().skip(1))
                .all(|(left, right)| left.frame <= right.frame)
    }

    pub fn apply_casualties(&mut self, casualties: f64) {
        if casualties.is_finite() && casualties > 0.0 {
            self.current_personnel = (self.current_personnel - casualties).max(0.0);
        }
    }

    pub const fn sample_due(tick: u64) -> bool {
        tick >= MOMENTUM_SAMPLE_OFFSET
            && (tick - MOMENTUM_SAMPLE_OFFSET).is_multiple_of(MOMENTUM_SAMPLE_INTERVAL)
    }

    /// Append one committed territory sample and reproduce the browser's phase resolver.
    ///
    /// The browser's "last three" loop also compares the first entry in that suffix to the
    /// immediately preceding entry when the history contains four or more samples. Preserve that
    /// edge comparison rather than replacing it with a simple three-value monotonic test.
    pub fn sample(&mut self, frame: u64, controlled: u64) {
        self.momentum_samples
            .push_back(MomentumSample { frame, controlled });
        while self.momentum_samples.len() > MOMENTUM_WINDOW {
            self.momentum_samples.pop_front();
        }
        if self.momentum_samples.len() < 3 {
            self.phase = WarPhase::Stalemate;
            return;
        }

        let first = self
            .momentum_samples
            .front()
            .expect("history has at least three samples")
            .controlled as f64;
        let last = self
            .momentum_samples
            .back()
            .expect("history has at least three samples")
            .controlled as f64;
        let delta_ratio = if first > 0.0 {
            (last - first) / first
        } else {
            0.0
        };

        let start = self.momentum_samples.len().saturating_sub(3);
        let mut trend_up = 0;
        let mut trend_down = 0;
        for index in start..self.momentum_samples.len() {
            if index == 0 {
                continue;
            }
            let previous = self.momentum_samples[index - 1].controlled;
            let current = self.momentum_samples[index].controlled;
            if current > previous {
                trend_up += 1;
            } else if current < previous {
                trend_down += 1;
            }
        }

        let manpower_ratio = if self.initial_personnel > 0.0 {
            self.current_personnel / self.initial_personnel
        } else {
            1.0
        };
        self.phase = if delta_ratio < -0.05 || manpower_ratio < 0.10 {
            WarPhase::Collapsing
        } else if delta_ratio < -0.005 || trend_down >= 2 {
            WarPhase::Retreating
        } else if delta_ratio > 0.005 || trend_up >= 2 {
            WarPhase::Advancing
        } else {
            WarPhase::Stalemate
        };
    }

    /// Recompute continuation posture from currently visible deployed strength.
    ///
    /// Browser CONQUEST posture uses observer-scoped operational intel. Native does not own that
    /// task-force/intel layer yet, so continuation deliberately uses authoritative live hostile
    /// strength while retaining the browser thresholds and manpower override.
    pub fn refresh_posture(
        &mut self,
        has_deployed_units: bool,
        own_strength: f64,
        has_hostile_units: bool,
        hostile_strength: f64,
    ) {
        if !has_deployed_units {
            self.posture = WarPosture::Balanced;
            return;
        }

        let mut posture = self.posture_override.unwrap_or(WarPosture::Balanced);
        if self.posture_override.is_none() && has_hostile_units {
            let ratio = own_strength.max(0.0) / hostile_strength.max(1.0);
            if ratio > 1.5 {
                posture = WarPosture::Offensive;
            } else if ratio < 0.7 {
                posture = WarPosture::Defensive;
            }
        }
        if self.initial_personnel > 0.0 && self.current_personnel / self.initial_personnel < 0.15 {
            posture = WarPosture::Defensive;
        }
        self.posture = posture;
    }

    /// Recompute continuation posture from observer-scoped operational intel.
    ///
    /// The supplied estimate may include the observer's persisted prewar baseline, but this path
    /// never substitutes authoritative live all-map strength. A genuinely missing estimate stays
    /// missing. Legacy checkpoints continue through [`Self::refresh_posture`].
    pub fn refresh_posture_from_intel(
        &mut self,
        has_deployed_units: bool,
        own_strength: f64,
        known_hostile_strength: Option<f64>,
    ) {
        if !has_deployed_units {
            self.posture = WarPosture::Balanced;
            return;
        }

        let mut posture = self.posture_override.unwrap_or(WarPosture::Balanced);
        if self.posture_override.is_none()
            && let Some(hostile_strength) = known_hostile_strength
        {
            let ratio = own_strength.max(0.0) / hostile_strength.max(1.0);
            if ratio > 1.5 {
                posture = WarPosture::Offensive;
            } else if ratio < 0.7 {
                posture = WarPosture::Defensive;
            }
        }
        if self.initial_personnel > 0.0 && self.current_personnel / self.initial_personnel < 0.15 {
            posture = WarPosture::Defensive;
        }
        self.posture = posture;
    }
}

pub fn bootstrap_sides<I>(side_count: usize, units: I) -> BTreeMap<usize, SideDynamics>
where
    I: IntoIterator<Item = (usize, f64)>,
{
    let mut totals = vec![0.0; side_count];
    for (side, personnel) in units {
        if side < side_count && personnel.is_finite() {
            totals[side] += personnel.max(0.0);
        }
    }
    totals
        .into_iter()
        .enumerate()
        .map(|(side, personnel)| (side, SideDynamics::bootstrap(side, personnel)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sampled(controlled: &[u64]) -> SideDynamics {
        let mut side = SideDynamics::bootstrap(0, 100.0);
        for (frame, controlled) in controlled.iter().copied().enumerate() {
            side.sample(frame as u64, controlled);
        }
        side
    }

    #[test]
    fn cadence_and_window_match_browser() {
        assert!(!SideDynamics::sample_due(36));
        assert!(SideDynamics::sample_due(37));
        assert!(SideDynamics::sample_due(237));
        assert!(!SideDynamics::sample_due(238));

        let side = sampled(&(100..112).collect::<Vec<_>>());
        assert_eq!(side.momentum_samples.len(), MOMENTUM_WINDOW);
        assert_eq!(side.momentum_samples.front().unwrap().controlled, 102);
    }

    #[test]
    fn phase_uses_exact_gate_thresholds_and_zero_baseline() {
        assert_eq!(sampled(&[100, 120]).phase, WarPhase::Stalemate);
        assert_eq!(sampled(&[100, 100, 95]).phase, WarPhase::Retreating);
        assert_eq!(sampled(&[100, 100, 94]).phase, WarPhase::Collapsing);
        assert_eq!(sampled(&[100, 100, 101]).phase, WarPhase::Advancing);
        assert_eq!(sampled(&[0, 1, 2]).phase, WarPhase::Advancing);

        let mut no_initial_pool = SideDynamics::bootstrap(0, 0.0);
        no_initial_pool.sample(0, 10);
        no_initial_pool.sample(1, 10);
        no_initial_pool.sample(2, 10);
        assert_eq!(no_initial_pool.phase, WarPhase::Stalemate);
    }

    #[test]
    fn trend_includes_the_browser_suffix_edge() {
        assert_eq!(sampled(&[100, 101, 102, 102]).phase, WarPhase::Advancing);
        assert_eq!(sampled(&[100, 99, 98, 98]).phase, WarPhase::Retreating);
    }

    #[test]
    fn posture_and_personnel_match_browser_thresholds() {
        let mut side = SideDynamics::bootstrap(0, 100.0);
        side.refresh_posture(true, 151.0, true, 100.0);
        assert_eq!(side.posture, WarPosture::Offensive);
        side.refresh_posture(true, 150.0, true, 100.0);
        assert_eq!(side.posture, WarPosture::Balanced);
        side.refresh_posture(true, 69.0, true, 100.0);
        assert_eq!(side.posture, WarPosture::Defensive);
        side.refresh_posture(true, 70.0, true, 100.0);
        assert_eq!(side.posture, WarPosture::Balanced);

        side.apply_casualties(85.01);
        side.refresh_posture(true, 200.0, true, 100.0);
        assert_eq!(side.posture, WarPosture::Defensive);
        side.apply_casualties(1_000.0);
        assert_eq!(side.current_personnel, 0.0);
    }

    #[test]
    fn captured_override_precedes_strength_but_not_manpower_collapse() {
        let mut side = SideDynamics::bootstrap(0, 100.0);
        side.posture_override = Some(WarPosture::Defensive);
        side.refresh_posture(true, 1_000.0, true, 1.0);
        assert_eq!(side.posture, WarPosture::Defensive);

        side.posture_override = Some(WarPosture::Offensive);
        side.refresh_posture(true, 1.0, true, 1_000.0);
        assert_eq!(side.posture, WarPosture::Offensive);
        side.apply_casualties(86.0);
        side.refresh_posture(true, 1_000.0, true, 1.0);
        assert_eq!(side.posture, WarPosture::Defensive);

        side.refresh_posture(false, 0.0, true, 1.0);
        assert_eq!(side.posture, WarPosture::Balanced);
    }

    #[test]
    fn observer_intel_path_does_not_substitute_missing_hostile_strength() {
        let mut side = SideDynamics::bootstrap(0, 100.0);
        side.refresh_posture_from_intel(true, 100.0, None);
        assert_eq!(side.posture, WarPosture::Balanced);

        side.refresh_posture_from_intel(true, 100.0, Some(50.0));
        assert_eq!(side.posture, WarPosture::Offensive);
        side.refresh_posture_from_intel(true, 10.0, Some(100.0));
        assert_eq!(side.posture, WarPosture::Defensive);

        side.posture_override = Some(WarPosture::Offensive);
        side.refresh_posture_from_intel(true, 1.0, Some(100.0));
        assert_eq!(side.posture, WarPosture::Offensive);
        side.current_personnel = 14.0;
        side.refresh_posture_from_intel(true, 100.0, Some(1.0));
        assert_eq!(side.posture, WarPosture::Defensive);
    }

    #[test]
    fn bootstrap_covers_empty_sides_and_validation_checks_history() {
        let sides = bootstrap_sides(3, [(0, 10.5), (2, 4.25)]);
        assert_eq!(sides.len(), 3);
        assert_eq!(sides[&0].current_personnel, 10.5);
        assert_eq!(sides[&1].current_personnel, 0.0);
        assert_eq!(sides[&2].current_personnel, 4.25);
        assert!(sides[&2].validate(0, 8));
    }
}
