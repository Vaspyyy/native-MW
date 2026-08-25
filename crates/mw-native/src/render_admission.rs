//! Browser-compatible presentation admission for native runtime publications.
//!
//! Admission only controls painting. It must never be used to skip, reorder, or
//! budget simulation work.

use std::fmt;

pub const BROWSER_RENDER_BUDGET_MS: f64 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPlaybackSpeed {
    OneX,
    TwoX,
    ThreeX,
}

impl BrowserPlaybackSpeed {
    pub const fn cadence_frames(self) -> u64 {
        match self {
            Self::OneX => 1,
            Self::TwoX => 2,
            Self::ThreeX => 4,
        }
    }

    pub const fn max_deferred_frames(self) -> u64 {
        let cadence = self.cadence_frames();
        let cadence_limit = cadence + 1;
        if cadence_limit > 2 { cadence_limit } else { 2 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderAdmissionInput {
    pub visual_dirty: bool,
    pub simulation_work_ms: f64,
    pub simulation_budget_ms: f64,
    pub commit_frame: bool,
    pub frames_since_render: u64,
    pub max_deferred_frames: u64,
    pub force: bool,
}

impl Default for RenderAdmissionInput {
    fn default() -> Self {
        Self {
            visual_dirty: true,
            simulation_work_ms: 0.0,
            simulation_budget_ms: BROWSER_RENDER_BUDGET_MS,
            commit_frame: false,
            frames_since_render: 0,
            max_deferred_frames: 2,
            force: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderAdmissionReason {
    Clean,
    Forced,
    MaxDeferral,
    CommitFrame,
    SimulationOverBudget,
    WithinBudget,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderAdmissionResult {
    pub admit: bool,
    pub reason: RenderAdmissionReason,
    pub visual_dirty: bool,
    pub simulation_work_ms: f64,
    pub simulation_budget_ms: f64,
    pub over_budget: bool,
    pub commit_frame: bool,
    pub frames_since_render: u64,
    pub max_deferred_frames: u64,
    pub forced_by_starvation: bool,
}

/// Port of the browser's `decideRenderAdmission` presentation policy.
pub fn decide_render_admission(input: RenderAdmissionInput) -> RenderAdmissionResult {
    // Match the browser's numeric normalization at the typed boundary.
    let simulation_work_ms = if input.simulation_work_ms.is_nan() {
        0.0
    } else {
        input.simulation_work_ms.max(0.0)
    };
    let simulation_budget_ms = if input.simulation_budget_ms.is_finite() {
        input.simulation_budget_ms.max(0.0)
    } else {
        BROWSER_RENDER_BUDGET_MS
    };
    let over_budget = simulation_work_ms > simulation_budget_ms;
    let forced_by_starvation = input.frames_since_render >= input.max_deferred_frames;

    let (admit, reason) = if !input.visual_dirty {
        (false, RenderAdmissionReason::Clean)
    } else if input.force {
        (true, RenderAdmissionReason::Forced)
    } else if forced_by_starvation {
        (true, RenderAdmissionReason::MaxDeferral)
    } else if input.commit_frame && over_budget {
        (false, RenderAdmissionReason::CommitFrame)
    } else if over_budget {
        (false, RenderAdmissionReason::SimulationOverBudget)
    } else {
        (true, RenderAdmissionReason::WithinBudget)
    };

    RenderAdmissionResult {
        admit,
        reason,
        visual_dirty: input.visual_dirty,
        simulation_work_ms,
        simulation_budget_ms,
        over_budget,
        commit_frame: input.commit_frame,
        frames_since_render: input.frames_since_render,
        max_deferred_frames: input.max_deferred_frames,
        forced_by_starvation,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrowserRenderFrame {
    pub runtime_frame: u64,
    pub speed: BrowserPlaybackSpeed,
    pub simulation_work_ms: f64,
    pub commit_frame: bool,
    pub force: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NonMonotonicPublication {
    pub previous: u64,
    pub next: u64,
}

impl fmt::Display for NonMonotonicPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "runtime frame publications cannot regress or repeat unless forced (previous {}, next {})",
            self.previous, self.next
        )
    }
}

impl std::error::Error for NonMonotonicPublication {}

/// Stateful browser-style admission over native runtime frame publications.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrowserRenderAdmission {
    last_publication: Option<u64>,
    frames_since_render: u64,
    force_next: bool,
}

impl BrowserRenderAdmission {
    pub const fn new() -> Self {
        Self {
            last_publication: None,
            frames_since_render: 0,
            force_next: false,
        }
    }

    #[cfg(test)]
    pub const fn last_publication(&self) -> Option<u64> {
        self.last_publication
    }

    #[cfg(test)]
    pub const fn frames_since_render(&self) -> u64 {
        self.frames_since_render
    }

    /// Force the next publication to be dirty and admitted.
    pub fn force(&mut self) {
        self.force_next = true;
    }

    /// Record that an admitted publication was actually presented.
    pub fn mark_rendered(&mut self) {
        self.frames_since_render = 0;
        self.force_next = false;
    }

    pub fn evaluate(
        &mut self,
        frame: BrowserRenderFrame,
    ) -> Result<RenderAdmissionResult, NonMonotonicPublication> {
        if let Some(previous) = self.last_publication {
            let duplicate_forced_terminal = frame.runtime_frame == previous && frame.force;
            if frame.runtime_frame < previous
                || (frame.runtime_frame == previous && !duplicate_forced_terminal)
            {
                return Err(NonMonotonicPublication {
                    previous,
                    next: frame.runtime_frame,
                });
            }
        }

        self.last_publication = Some(frame.runtime_frame);
        let cadence = frame.speed.cadence_frames();
        let max_deferred_frames = frame.speed.max_deferred_frames();
        let starvation_due = self.frames_since_render >= max_deferred_frames;
        let force = frame.force || self.force_next;
        self.force_next = false;

        // Browser force comes from a visual condition (for example zoom settle),
        // so it makes this publication dirty while remaining a separate reason.
        let visual_dirty = frame.runtime_frame.is_multiple_of(cadence) || starvation_due || force;
        let result = decide_render_admission(RenderAdmissionInput {
            visual_dirty,
            simulation_work_ms: frame.simulation_work_ms,
            simulation_budget_ms: BROWSER_RENDER_BUDGET_MS,
            commit_frame: frame.commit_frame,
            frames_since_render: self.frames_since_render,
            max_deferred_frames,
            force,
        });

        // Admission is only permission to paint. Keep aging until the caller
        // confirms successful presentation with `mark_rendered`.
        self.frames_since_render = self.frames_since_render.saturating_add(1);

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(
        visual_dirty: bool,
        work_ms: f64,
        commit_frame: bool,
        frames_since_render: u64,
        max_deferred_frames: u64,
        force: bool,
    ) -> RenderAdmissionResult {
        decide_render_admission(RenderAdmissionInput {
            visual_dirty,
            simulation_work_ms: work_ms,
            simulation_budget_ms: BROWSER_RENDER_BUDGET_MS,
            commit_frame,
            frames_since_render,
            max_deferred_frames,
            force,
        })
    }

    #[test]
    fn every_decision_reason_matches_browser_priority() {
        let cases = [
            (
                decision(false, 0.0, false, 0, 2, false),
                false,
                RenderAdmissionReason::Clean,
            ),
            (
                decision(true, 50.0, true, 2, 2, true),
                true,
                RenderAdmissionReason::Forced,
            ),
            (
                decision(true, 50.0, true, 2, 2, false),
                true,
                RenderAdmissionReason::MaxDeferral,
            ),
            (
                decision(true, 13.0, true, 0, 2, false),
                false,
                RenderAdmissionReason::CommitFrame,
            ),
            (
                decision(true, 13.0, false, 0, 2, false),
                false,
                RenderAdmissionReason::SimulationOverBudget,
            ),
            (
                decision(true, 12.0, true, 0, 2, false),
                true,
                RenderAdmissionReason::WithinBudget,
            ),
        ];

        for (result, admit, reason) in cases {
            assert_eq!(result.admit, admit);
            assert_eq!(result.reason, reason);
        }
    }

    #[test]
    fn dirty_gate_precedes_force_and_starvation_like_browser() {
        let result = decision(false, 50.0, true, 10, 2, true);
        assert_eq!(result.reason, RenderAdmissionReason::Clean);
        assert!(!result.admit);
        assert!(result.forced_by_starvation);
    }

    #[test]
    fn work_and_budget_normalization_matches_browser_defaults() {
        let negative = decide_render_admission(RenderAdmissionInput {
            simulation_work_ms: -5.0,
            simulation_budget_ms: -2.0,
            ..RenderAdmissionInput::default()
        });
        assert_eq!(negative.simulation_work_ms, 0.0);
        assert_eq!(negative.simulation_budget_ms, 0.0);
        assert!(!negative.over_budget);

        let invalid = decide_render_admission(RenderAdmissionInput {
            simulation_work_ms: f64::NAN,
            simulation_budget_ms: f64::INFINITY,
            ..RenderAdmissionInput::default()
        });
        assert_eq!(invalid.simulation_work_ms, 0.0);
        assert_eq!(invalid.simulation_budget_ms, BROWSER_RENDER_BUDGET_MS);
    }

    #[test]
    fn supported_speeds_have_browser_cadence_and_deferral_limits() {
        assert_eq!(BrowserPlaybackSpeed::OneX.cadence_frames(), 1);
        assert_eq!(BrowserPlaybackSpeed::TwoX.cadence_frames(), 2);
        assert_eq!(BrowserPlaybackSpeed::ThreeX.cadence_frames(), 4);
        assert_eq!(BrowserPlaybackSpeed::OneX.max_deferred_frames(), 2);
        assert_eq!(BrowserPlaybackSpeed::TwoX.max_deferred_frames(), 3);
        assert_eq!(BrowserPlaybackSpeed::ThreeX.max_deferred_frames(), 5);
    }

    fn frame(runtime_frame: u64, speed: BrowserPlaybackSpeed) -> BrowserRenderFrame {
        BrowserRenderFrame {
            runtime_frame,
            speed,
            simulation_work_ms: 0.0,
            commit_frame: false,
            force: false,
        }
    }

    #[test]
    fn controller_applies_each_supported_speed_cadence() {
        for (speed, due, not_due) in [
            (BrowserPlaybackSpeed::OneX, 1, None),
            (BrowserPlaybackSpeed::TwoX, 2, Some(1)),
            (BrowserPlaybackSpeed::ThreeX, 4, Some(1)),
        ] {
            let mut controller = BrowserRenderAdmission::new();
            if let Some(not_due) = not_due {
                let result = controller.evaluate(frame(not_due, speed)).unwrap();
                assert_eq!(result.reason, RenderAdmissionReason::Clean);
            }
            let result = controller.evaluate(frame(due, speed)).unwrap();
            assert_eq!(result.reason, RenderAdmissionReason::WithinBudget);
            assert!(result.admit);
        }
    }

    #[test]
    fn controller_defers_over_budget_commit_and_generic_frames() {
        let mut commit_controller = BrowserRenderAdmission::new();
        let mut commit = frame(1, BrowserPlaybackSpeed::OneX);
        commit.simulation_work_ms = 12.01;
        commit.commit_frame = true;
        assert_eq!(
            commit_controller.evaluate(commit).unwrap().reason,
            RenderAdmissionReason::CommitFrame
        );

        let mut generic_controller = BrowserRenderAdmission::new();
        let mut generic = frame(1, BrowserPlaybackSpeed::OneX);
        generic.simulation_work_ms = 12.01;
        assert_eq!(
            generic_controller.evaluate(generic).unwrap().reason,
            RenderAdmissionReason::SimulationOverBudget
        );
    }

    #[test]
    fn controller_starvation_bounds_deferral_even_on_heavy_commit() {
        let mut controller = BrowserRenderAdmission::new();
        for runtime_frame in 1..=2 {
            let mut publication = frame(runtime_frame, BrowserPlaybackSpeed::OneX);
            publication.simulation_work_ms = 50.0;
            publication.commit_frame = true;
            assert!(!controller.evaluate(publication).unwrap().admit);
        }

        let mut starving = frame(3, BrowserPlaybackSpeed::OneX);
        starving.simulation_work_ms = 50.0;
        starving.commit_frame = true;
        let result = controller.evaluate(starving).unwrap();
        assert_eq!(result.reason, RenderAdmissionReason::MaxDeferral);
        assert!(result.admit);
    }

    #[test]
    fn controller_force_inputs_and_latched_force_admit_off_cadence() {
        let mut direct = BrowserRenderAdmission::new();
        let mut forced = frame(1, BrowserPlaybackSpeed::ThreeX);
        forced.force = true;
        assert_eq!(
            direct.evaluate(forced).unwrap().reason,
            RenderAdmissionReason::Forced
        );

        let mut latched = BrowserRenderAdmission::new();
        latched.force();
        assert_eq!(
            latched
                .evaluate(frame(1, BrowserPlaybackSpeed::ThreeX))
                .unwrap()
                .reason,
            RenderAdmissionReason::Forced
        );
    }

    #[test]
    fn controller_mark_rendered_resets_render_state_but_keeps_publication_cursor() {
        let mut controller = BrowserRenderAdmission::new();
        controller
            .evaluate(frame(1, BrowserPlaybackSpeed::ThreeX))
            .unwrap();
        controller
            .evaluate(frame(2, BrowserPlaybackSpeed::ThreeX))
            .unwrap();
        assert_eq!(controller.frames_since_render(), 2);

        controller.force();
        controller.mark_rendered();
        assert_eq!(controller.frames_since_render(), 0);
        assert_eq!(controller.last_publication(), Some(2));
        assert_eq!(
            controller
                .evaluate(frame(3, BrowserPlaybackSpeed::ThreeX))
                .unwrap()
                .reason,
            RenderAdmissionReason::Clean
        );
    }

    #[test]
    fn controller_rejects_duplicate_and_regressing_publications_without_mutation() {
        let mut controller = BrowserRenderAdmission::new();
        controller
            .evaluate(frame(4, BrowserPlaybackSpeed::ThreeX))
            .unwrap();
        let before = controller.clone();

        assert_eq!(
            controller
                .evaluate(frame(4, BrowserPlaybackSpeed::ThreeX))
                .unwrap_err(),
            NonMonotonicPublication {
                previous: 4,
                next: 4
            }
        );
        assert_eq!(controller, before);
        assert!(
            controller
                .evaluate(frame(3, BrowserPlaybackSpeed::ThreeX))
                .is_err()
        );
        assert_eq!(controller, before);
    }

    #[test]
    fn controller_allows_one_forced_terminal_publication_on_the_same_browser_frame() {
        let mut controller = BrowserRenderAdmission::new();
        controller
            .evaluate(frame(4, BrowserPlaybackSpeed::OneX))
            .unwrap();
        controller.mark_rendered();

        let mut terminal = frame(4, BrowserPlaybackSpeed::OneX);
        terminal.force = true;
        let admitted = controller.evaluate(terminal).unwrap();

        assert!(admitted.admit);
        assert_eq!(admitted.reason, RenderAdmissionReason::Forced);
        assert_eq!(controller.last_publication(), Some(4));
        assert!(
            controller
                .evaluate(frame(4, BrowserPlaybackSpeed::OneX))
                .is_err()
        );
        let mut regressing = frame(3, BrowserPlaybackSpeed::OneX);
        regressing.force = true;
        assert!(controller.evaluate(regressing).is_err());
    }

    #[test]
    fn controller_ages_until_an_admitted_frame_is_confirmed_rendered() {
        let mut controller = BrowserRenderAdmission::new();
        assert!(
            controller
                .evaluate(frame(1, BrowserPlaybackSpeed::OneX))
                .unwrap()
                .admit
        );
        assert_eq!(controller.frames_since_render(), 1);

        controller.mark_rendered();
        assert_eq!(controller.frames_since_render(), 0);

        let mut heavy = frame(2, BrowserPlaybackSpeed::OneX);
        heavy.simulation_work_ms = 20.0;
        assert!(!controller.evaluate(heavy).unwrap().admit);
        assert_eq!(controller.frames_since_render(), 1);
    }
}
