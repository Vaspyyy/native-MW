//! Deterministic, window-free validation of the production runtime worker.

use std::{
    collections::BTreeMap,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use mw_checkpoint::native_runtime::{
    load_runtime_checkpoint, write_runtime_checkpoint_state_v2, write_runtime_checkpoint_state_v3,
    write_runtime_checkpoint_state_v4, write_runtime_checkpoint_state_v5,
};
use mw_core::{
    CombatEvent, CombatLayer, ConflictResolutionKind, GridSpec, NativeWarBootstrapConfig,
    ProductionConfig, ProductionCountry, RuntimeSnapshot, RuntimeState, TerritoryRenderUpdate,
    UnitKind, bootstrap_native_war, decode_mwsc_gzip_file, derive_scenario_production,
};
use serde_json::json;

use crate::{
    options::AppOptions,
    runtime_worker::{RuntimeWorker, RuntimeWorkerStatus},
};

const HEADLESS_SCHEMA: &str = "mw-native-headless-v2";
const RESUMABLE_CHECKPOINT_BOUNDARIES: [&str; 2] = ["postStartWar", "midWar"];
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const MIN_WATCHDOG: Duration = Duration::from_secs(30);
const MAX_WATCHDOG: Duration = Duration::from_secs(24 * 60 * 60);
const FNV64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;
const TERMINAL_SAVE_SKIP_REASON: &str =
    "conflictResolved is terminal and cannot be resumed as a mid-war checkpoint";

fn resolve_sides(
    selectors: &[Vec<String>],
    countries: &[ProductionCountry],
) -> Result<Vec<Vec<u16>>> {
    let mut resolved = Vec::with_capacity(selectors.len());
    let mut seen = std::collections::BTreeSet::new();
    for side in selectors {
        let mut ids = Vec::with_capacity(side.len());
        for selector in side {
            let id = if let Ok(id) = selector.parse::<u16>() {
                ensure!(
                    countries.iter().any(|country| country.country_id == id),
                    "unknown country selector {selector:?}"
                );
                id
            } else {
                let matches = countries
                    .iter()
                    .filter(|country| country.name.eq_ignore_ascii_case(selector))
                    .collect::<Vec<_>>();
                ensure!(
                    matches.len() == 1,
                    "{} country matches for selector {:?}",
                    if matches.is_empty() { "no" } else { "multiple" },
                    selector
                );
                matches[0].country_id
            };
            ensure!(
                seen.insert(id),
                "country selector {selector:?} appears more than once"
            );
            ids.push(id);
        }
        resolved.push(ids);
    }
    Ok(resolved)
}

/// Load a strict production checkpoint, execute up to `steps` worker ticks, and print a
/// deterministic report. A resolved conflict ends cleanly before that cap. This path deliberately
/// constructs no window or GPU state.
pub fn run_headless(options: &AppOptions, steps: u64) -> Result<()> {
    ensure!(steps > 0, "headless worker steps must be greater than zero");
    let (baseline, runtime, boundary, unit_count) = if let Some(checkpoint_path) =
        options.runtime_checkpoint_path.as_ref()
    {
        let loaded = load_runtime_checkpoint(&options.scenario_path, checkpoint_path)
            .with_context(|| {
                format!(
                    "failed to load native runtime checkpoint {}",
                    checkpoint_path.display()
                )
            })?;
        ensure!(
            loaded.resumable,
            "checkpoint boundary {} is not resumable",
            loaded.checkpoint_boundary
        );
        ensure!(
            RESUMABLE_CHECKPOINT_BOUNDARIES.contains(&loaded.checkpoint_boundary),
            "headless runtime requires a postStartWar or midWar checkpoint, got {}",
            loaded.checkpoint_boundary
        );
        ensure!(
            loaded.exact_geography_supplied,
            "headless runtime requires checkpoint-supplied exact geography"
        );
        (
            loaded.baseline,
            loaded.runtime,
            loaded.checkpoint_boundary,
            loaded.unit_count,
        )
    } else {
        ensure!(
            options.native_war_sides.len() >= 2,
            "headless mode requires a checkpoint or at least two --side arguments"
        );
        let baseline = decode_mwsc_gzip_file(&options.scenario_path, Some(GridSpec::world(0.15)?))
            .with_context(|| format!("failed to decode {}", options.scenario_path.display()))?;
        let production = derive_scenario_production(&baseline, &ProductionConfig::default())?;
        let sides = resolve_sides(&options.native_war_sides, &production.countries)?;
        let runtime = bootstrap_native_war(
            &baseline,
            &NativeWarBootstrapConfig {
                sides,
                hostility: None,
                production: ProductionConfig::default(),
                war_grace_end: 600,
            },
        )?;
        let units = runtime.latest_snapshot().frame_snapshot.units.len();
        (baseline, runtime, "nativeNewWar", units)
    };
    let initial = runtime.latest_snapshot();
    ensure!(
        initial.frame_snapshot.units.len() == unit_count,
        "loaded unit count does not match the runtime's initial publication"
    );
    let initial_tick = initial.tick;
    let initial_frame = initial.frame;
    initial_tick
        .checked_add(steps)
        .context("requested headless steps overflow the runtime tick")?;
    initial_frame
        .checked_add(steps)
        .context("requested headless steps overflow the runtime frame")?;

    let mut worker = RuntimeWorker::spawn_with_limit(
        runtime,
        options.runtime_tick_interval,
        options.runtime_queue_capacity,
        Some(steps),
    )
    .context("failed to start the native runtime worker")?;

    let watchdog = watchdog_duration(options.runtime_tick_interval, steps);
    let outcome = monitor_worker(&worker, steps, initial_tick, initial_frame, watchdog);
    let completion = match outcome {
        Ok(completion) => completion,
        Err(error) => {
            if worker.stop_and_join().is_err() {
                bail!("{error:#}; native runtime worker also panicked while joining");
            }
            return Err(error);
        }
    };
    let save_skip_reason =
        checkpoint_save_skip_reason(options.save_checkpoint_path.is_some(), completion.reason);
    let save_skipped = save_skip_reason.is_some();
    let save_result = (|| -> Result<_> {
        let Some(path) = options.save_checkpoint_path.as_ref() else {
            return Ok(None);
        };
        if save_skipped {
            return Ok(None);
        }
        let state = worker.checkpoint_state().map_err(|error| {
            anyhow::anyhow!("failed to capture native runtime checkpoint state: {error}")
        })?;
        let writer = if state.operations.is_some() {
            write_runtime_checkpoint_state_v5
        } else if state.side_dynamics.is_some() {
            write_runtime_checkpoint_state_v4
        } else if state.influence_runtime.is_some() {
            write_runtime_checkpoint_state_v3
        } else {
            write_runtime_checkpoint_state_v2
        };
        Ok(Some(writer(
            &options.scenario_path,
            &baseline,
            &state,
            path,
            usize::try_from(completion.completed_steps)
                .context("completed steps exceed save format")?,
        )?))
    })();
    let joined = worker.stop_and_join();
    if joined.is_err() {
        bail!("native runtime worker panicked while joining");
    }
    let save_report = save_result?;
    ensure!(
        completion.completed_steps <= steps,
        "headless worker completed {} steps, exceeding its requested limit of {steps}",
        completion.completed_steps
    );
    let expected_final_tick = initial_tick
        .checked_add(completion.completed_steps)
        .context("completed headless steps overflow the runtime tick")?;
    let expected_final_frame = initial_frame
        .checked_add(completion.completed_steps)
        .context("completed headless steps overflow the runtime frame")?;

    ensure!(
        completion.final_snapshot.tick == expected_final_tick,
        "headless worker ended at tick {}, expected {expected_final_tick}",
        completion.final_snapshot.tick
    );
    ensure!(
        completion.final_snapshot.frame == expected_final_frame,
        "headless worker ended at frame {}, expected {expected_final_frame}",
        completion.final_snapshot.frame
    );

    let units = completion.final_snapshot.frame_snapshot.units.len();
    let checksum = semantic_checksum(ChecksumInput {
        boundary,
        requested_steps: steps,
        completed_steps: completion.completed_steps,
        initial_tick,
        initial_frame,
        final_snapshot: &completion.final_snapshot,
        update_checksum: completion.update_checksum,
        territory_updates: completion.territory_updates,
    })?;

    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema": HEADLESS_SCHEMA,
                "checkpointBoundary": boundary,
                "requestedSteps": steps,
                "completedSteps": completion.completed_steps,
                "initialTick": initial_tick,
                "finalTick": completion.final_snapshot.tick,
                "initialFrame": initial_frame,
                "finalFrame": completion.final_snapshot.frame,
                "units": units,
                "territoryUpdates": completion.territory_updates,
                "joined": true,
                "termination": completion.reason.as_str(),
                "checksum": checksum,
                "save": save_report,
                "saveSkipped": save_skip_reason,
            }))?
        );
    } else {
        println!(
            "{HEADLESS_SCHEMA} {boundary}: {}/{} steps, tick {}->{}, frame {}->{}, {} units, {} territory updates, {}, joined, checksum {}{}",
            completion.completed_steps,
            steps,
            initial_tick,
            completion.final_snapshot.tick,
            initial_frame,
            completion.final_snapshot.frame,
            units,
            completion.territory_updates,
            completion.reason.as_str(),
            checksum,
            save_report
                .as_ref()
                .map(|report| format!(", saved {}", report.path))
                .or_else(|| save_skip_reason.map(|reason| format!(", save skipped: {reason}")))
                .unwrap_or_default(),
        );
    }
    Ok(())
}

struct WorkerCompletion {
    completed_steps: u64,
    final_snapshot: Arc<RuntimeSnapshot>,
    territory_updates: usize,
    update_checksum: Fnv64,
    reason: CompletionReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletionReason {
    StepLimit,
    ConflictResolved,
}

impl CompletionReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::StepLimit => "stepLimit",
            Self::ConflictResolved => "conflictResolved",
        }
    }
}

fn checkpoint_save_skip_reason(
    save_requested: bool,
    reason: CompletionReason,
) -> Option<&'static str> {
    (save_requested && reason == CompletionReason::ConflictResolved)
        .then_some(TERMINAL_SAVE_SKIP_REASON)
}

fn monitor_worker(
    worker: &RuntimeWorker,
    requested_steps: u64,
    initial_tick: u64,
    initial_frame: u64,
    watchdog: Duration,
) -> Result<WorkerCompletion> {
    let started = Instant::now();
    let mut latest_snapshot = None;
    let mut territory_updates = 0_usize;
    let mut update_checksum = Fnv64::new();

    loop {
        drain_worker(
            worker,
            &mut latest_snapshot,
            &mut territory_updates,
            &mut update_checksum,
        );

        if let Some(status) = worker.poll_status() {
            match status {
                RuntimeWorkerStatus::Completed { steps } => {
                    ensure!(
                        steps == requested_steps,
                        "native runtime worker completed {steps} steps, expected {requested_steps}"
                    );
                    // The worker sends the final atomic publication before its completion status.
                    // Drain once more so the final snapshot and every territory delta are observed.
                    drain_worker(
                        worker,
                        &mut latest_snapshot,
                        &mut territory_updates,
                        &mut update_checksum,
                    );
                    let final_snapshot = latest_snapshot
                        .context("worker completed without publishing a runtime snapshot")?;
                    let reason = completion_reason(final_snapshot.state)?;
                    return Ok(WorkerCompletion {
                        completed_steps: steps,
                        final_snapshot,
                        territory_updates,
                        update_checksum,
                        reason,
                    });
                }
                RuntimeWorkerStatus::Stopped => {
                    bail!("native runtime worker stopped before its exact step limit")
                }
                RuntimeWorkerStatus::Terminal(state) => {
                    drain_worker(
                        worker,
                        &mut latest_snapshot,
                        &mut territory_updates,
                        &mut update_checksum,
                    );
                    let final_snapshot = latest_snapshot
                        .context("terminal worker stopped without publishing its final snapshot")?;
                    ensure!(
                        final_snapshot.state == state,
                        "terminal worker status does not match its final snapshot"
                    );
                    let reason = completion_reason(state)?;
                    let completed_ticks = final_snapshot.tick.checked_sub(initial_tick).context(
                        "terminal worker final tick precedes its initial checkpoint tick",
                    )?;
                    let completed_frames =
                        final_snapshot.frame.checked_sub(initial_frame).context(
                            "terminal worker final frame precedes its initial checkpoint frame",
                        )?;
                    ensure!(
                        completed_ticks == completed_frames,
                        "terminal worker advanced {completed_ticks} ticks but {completed_frames} frames"
                    );
                    ensure!(
                        completed_ticks < requested_steps,
                        "terminal worker reported early completion after {completed_ticks} steps, but its limit was {requested_steps}"
                    );
                    return Ok(WorkerCompletion {
                        completed_steps: completed_ticks,
                        final_snapshot,
                        territory_updates,
                        update_checksum,
                        reason,
                    });
                }
                RuntimeWorkerStatus::Failed(error) => {
                    bail!("native runtime worker failed before its exact step limit: {error}")
                }
                RuntimeWorkerStatus::Panicked(error) => {
                    bail!("native runtime worker panicked: {error}")
                }
            }
        }

        if started.elapsed() >= watchdog {
            bail!(
                "native runtime worker exceeded its {:?} headless watchdog after requesting {requested_steps} steps",
                watchdog
            );
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn completion_reason(state: RuntimeState) -> Result<CompletionReason> {
    match state {
        RuntimeState::Running => Ok(CompletionReason::StepLimit),
        RuntimeState::ConflictResolved { .. } => Ok(CompletionReason::ConflictResolved),
        RuntimeState::AwaitingStrategicEffects { .. } => {
            bail!("native runtime stopped while awaiting unapplied strategic effects")
        }
        RuntimeState::Poisoned => bail!("native runtime stopped in a poisoned state"),
    }
}

fn drain_worker(
    worker: &RuntimeWorker,
    latest_snapshot: &mut Option<Arc<RuntimeSnapshot>>,
    territory_updates: &mut usize,
    update_checksum: &mut Fnv64,
) {
    let drained = worker.drain_render_state();
    for update in drained.territory_updates {
        checksum_territory_update(update_checksum, &update);
        *territory_updates = territory_updates.saturating_add(1);
    }
    if let Some(snapshot) = drained.snapshot {
        *latest_snapshot = Some(snapshot);
    }
}

fn watchdog_duration(tick_interval: Duration, steps: u64) -> Duration {
    // Allow twenty nominal tick intervals per requested step, plus startup/load scheduling
    // latitude. Integer nanosecond arithmetic avoids overflow for adversarial CLI values.
    let scaled_nanos = tick_interval
        .as_nanos()
        .saturating_mul(u128::from(steps))
        .saturating_mul(20)
        .saturating_add(Duration::from_secs(5).as_nanos());
    let minimum = MIN_WATCHDOG.as_nanos();
    let maximum = MAX_WATCHDOG.as_nanos();
    let bounded = scaled_nanos.clamp(minimum, maximum);
    Duration::new(
        (bounded / 1_000_000_000) as u64,
        (bounded % 1_000_000_000) as u32,
    )
}

struct ChecksumInput<'a> {
    boundary: &'a str,
    requested_steps: u64,
    completed_steps: u64,
    initial_tick: u64,
    initial_frame: u64,
    final_snapshot: &'a RuntimeSnapshot,
    update_checksum: Fnv64,
    territory_updates: usize,
}

fn semantic_checksum(input: ChecksumInput<'_>) -> Result<String> {
    let mut checksum = Fnv64::new();
    checksum.write_bytes(HEADLESS_SCHEMA.as_bytes());
    checksum.write_bytes(input.boundary.as_bytes());
    checksum.write_u64(input.requested_steps);
    checksum.write_u64(input.completed_steps);
    checksum.write_u64(input.initial_tick);
    checksum.write_u64(input.initial_frame);
    checksum.write_u64(input.territory_updates as u64);
    checksum.write_u64(input.update_checksum.value());
    checksum_runtime_snapshot(&mut checksum, input.final_snapshot)?;
    Ok(checksum.finish())
}

fn checksum_runtime_snapshot(checksum: &mut Fnv64, snapshot: &RuntimeSnapshot) -> Result<()> {
    checksum.write_u64(snapshot.tick);
    checksum.write_u64(snapshot.frame);
    match snapshot.state {
        RuntimeState::Running => checksum.write_u64(0),
        RuntimeState::AwaitingStrategicEffects {
            cycle,
            tick,
            desertion_commands,
            surrender_commands,
            conflict_resolution,
        } => {
            checksum.write_u64(1);
            checksum.write_u64(cycle);
            checksum.write_u64(tick);
            checksum.write_u64(desertion_commands as u64);
            checksum.write_u64(surrender_commands as u64);
            checksum.write_bool(conflict_resolution);
        }
        RuntimeState::Poisoned => checksum.write_u64(2),
        RuntimeState::ConflictResolved {
            cycle,
            tick,
            resolution,
        } => {
            checksum.write_u64(3);
            checksum.write_u64(cycle);
            checksum.write_u64(tick);
            checksum.write_u64(match resolution.kind {
                ConflictResolutionKind::WhitePeace => 0,
                ConflictResolutionKind::FullCapitulation => 1,
            });
            checksum.write_bool(resolution.winner_side.is_some());
            if let Some(winner_side) = resolution.winner_side {
                checksum.write_u16(winner_side);
            }
            checksum.write_bool(resolution.stop_simulation);
        }
    }

    let frame = &snapshot.frame_snapshot;
    checksum.write_u64(frame.units.len() as u64);
    for unit in frame.units.iter() {
        checksum.write_u64(unit.id);
        checksum.write_u16(unit.side);
        checksum.write_u64(unit.sovereign);
        checksum.write_u64(match unit.kind {
            UnitKind::Army => 0,
            UnitKind::Armor => 1,
        });
        for value in [
            unit.lat,
            unit.lng,
            unit.health,
            unit.max_health,
            f64::from(unit.health_fraction),
            unit.dir_lat,
            unit.dir_lng,
        ] {
            checksum.write_f64(value);
        }
        for value in [
            unit.personnel,
            unit.personnel_capacity,
            unit.equipment,
            unit.max_equipment,
            unit.last_combat_tick,
            unit.victory_boost_ticks,
        ] {
            checksum.write_u64(value);
        }
        checksum.write_u64(u64::from(unit.coast_stuck_ticks));
        checksum.write_bool(unit.landing_penalty_active);
        checksum.write_bool(unit.transport);
        checksum.write_bool(unit.at_sea);
    }

    checksum.write_u64(frame.events.len() as u64);
    for event in frame.events.iter() {
        checksum_combat_event(checksum, event);
    }
    for ids in [frame.removed_ids.as_ref(), frame.abandoned_ids.as_ref()] {
        checksum.write_u64(ids.len() as u64);
        for &id in ids {
            checksum.write_u64(id);
        }
    }

    checksum.write_bytes(&serde_json::to_vec(snapshot.territory_snapshot.as_ref())?);
    if let Some(strategic) = &snapshot.strategic_snapshot {
        checksum.write_bool(true);
        checksum.write_u64(strategic.cycle);
        checksum.write_u64(strategic.tick);
        checksum.write_u64(strategic.territory_generation);
        checksum.write_u64(strategic.territory_commit_sequence);
        checksum.write_bytes(&serde_json::to_vec(strategic.countries.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(strategic.occupations.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(
            strategic.occupation_assessments.as_ref(),
        )?);
        checksum.write_bytes(&serde_json::to_vec(strategic.desertions.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(strategic.surrenders.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(strategic.events.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(&strategic.conflict_resolution)?);
    } else {
        checksum.write_bool(false);
    }
    checksum.write_bytes(&serde_json::to_vec(snapshot.casualty_totals.as_ref())?);
    checksum_casualties_by_victim(checksum, snapshot.casualties_by_victim.as_ref());
    Ok(())
}

fn checksum_casualties_by_victim(
    checksum: &mut Fnv64,
    casualties_by_victim: &BTreeMap<u16, BTreeMap<u16, f64>>,
) {
    checksum.write_u64(casualties_by_victim.len() as u64);
    for (&victim_id, attackers) in casualties_by_victim {
        checksum.write_u16(victim_id);
        checksum.write_u64(attackers.len() as u64);
        for (&attacker_id, &casualties) in attackers {
            checksum.write_u16(attacker_id);
            checksum.write_f64(casualties);
        }
    }
}

fn checksum_combat_event(checksum: &mut Fnv64, event: &CombatEvent) {
    checksum.write_u64(match event.layer {
        CombatLayer::Proximity => 0,
        CombatLayer::Direct => 1,
    });
    checksum.write_u64(event.attacker_id);
    checksum.write_u64(event.target_id);
    for value in [
        event.target_damage,
        event.attacker_damage,
        event.transport_self_damage,
        event.target_resulting_health,
        event.attacker_resulting_health,
    ] {
        checksum.write_f64(value);
    }
    for value in [
        event.target_personnel_loss,
        event.attacker_personnel_loss,
        event.target_equipment_loss,
        event.attacker_equipment_loss,
    ] {
        checksum.write_u64(value);
    }
    checksum.write_bool(event.target_knockback_blocked);
    checksum.write_bool(event.attacker_knockback_blocked);
}

fn checksum_territory_update(checksum: &mut Fnv64, update: &TerritoryRenderUpdate) {
    checksum.write_bool(update.full_update);
    checksum.write_u64(update.tiles.len() as u64);
    for tile in &update.tiles {
        checksum.write_u64(tile.bounds.tile as u64);
        checksum.write_u64(tile.bounds.min_x as u64);
        checksum.write_u64(tile.bounds.min_y as u64);
        checksum.write_u64(tile.bounds.max_x as u64);
        checksum.write_u64(tile.bounds.max_y as u64);
        checksum.write_u64(tile.pixels.len() as u64);
        for &pixel in &tile.pixels {
            checksum.write_u16(pixel);
        }
    }
}

#[derive(Clone, Copy)]
struct Fnv64(u64);

impl Fnv64 {
    const fn new() -> Self {
        Self(FNV64_OFFSET)
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(FNV64_PRIME);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_u16(&mut self, value: u16) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_f64(&mut self, value: f64) {
        self.write_u64(value.to_bits());
    }

    fn write_bool(&mut self, value: bool) {
        self.write_bytes(&[u8::from(value)]);
    }

    const fn value(self) -> u64 {
        self.0
    }

    fn finish(self) -> String {
        format!("{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mw_core::ConflictResolutionPlan;

    #[test]
    fn watchdog_has_a_floor_and_scales_without_overflow() {
        assert_eq!(watchdog_duration(Duration::from_millis(1), 1), MIN_WATCHDOG);
        assert_eq!(watchdog_duration(Duration::MAX, u64::MAX), MAX_WATCHDOG);
    }

    #[test]
    fn fnv_checksum_is_stable_and_order_sensitive() {
        let mut left = Fnv64::new();
        left.write_bytes(b"territory");
        left.write_u64(7);
        let mut right = Fnv64::new();
        right.write_bytes(b"territory");
        right.write_u64(7);
        let mut reordered = Fnv64::new();
        reordered.write_u64(7);
        reordered.write_bytes(b"territory");

        assert_eq!(left.finish(), right.finish());
        assert_ne!(left.finish(), reordered.finish());
    }

    #[test]
    fn resolved_conflict_is_a_clean_headless_completion() {
        let state = RuntimeState::ConflictResolved {
            cycle: 4,
            tick: 2_400,
            resolution: ConflictResolutionPlan {
                kind: ConflictResolutionKind::FullCapitulation,
                winner_side: Some(1),
                stop_simulation: true,
            },
        };

        assert_eq!(
            completion_reason(state).unwrap(),
            CompletionReason::ConflictResolved
        );
        assert!(
            completion_reason(RuntimeState::AwaitingStrategicEffects {
                cycle: 4,
                tick: 2_400,
                desertion_commands: 0,
                surrender_commands: 1,
                conflict_resolution: false,
            })
            .is_err()
        );
        assert_eq!(
            checkpoint_save_skip_reason(true, CompletionReason::ConflictResolved),
            Some(TERMINAL_SAVE_SKIP_REASON)
        );
        assert_eq!(
            checkpoint_save_skip_reason(true, CompletionReason::StepLimit),
            None
        );
        assert_eq!(
            checkpoint_save_skip_reason(false, CompletionReason::ConflictResolved),
            None
        );
    }

    #[test]
    fn nested_casualty_checksum_is_sorted_and_value_sensitive() {
        let mut left = BTreeMap::new();
        left.insert(7, BTreeMap::from([(3, 12.5), (5, 4.0)]));
        left.insert(2, BTreeMap::from([(7, 8.0)]));
        let mut same = BTreeMap::new();
        same.insert(2, BTreeMap::from([(7, 8.0)]));
        same.insert(7, BTreeMap::from([(5, 4.0), (3, 12.5)]));
        let mut changed = same.clone();
        changed.get_mut(&7).unwrap().insert(3, 12.75);

        let mut left_checksum = Fnv64::new();
        checksum_casualties_by_victim(&mut left_checksum, &left);
        let mut same_checksum = Fnv64::new();
        checksum_casualties_by_victim(&mut same_checksum, &same);
        let mut changed_checksum = Fnv64::new();
        checksum_casualties_by_victim(&mut changed_checksum, &changed);

        assert_eq!(left_checksum.finish(), same_checksum.finish());
        assert_ne!(left_checksum.finish(), changed_checksum.finish());
    }
}
