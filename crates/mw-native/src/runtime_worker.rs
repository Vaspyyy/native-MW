//! Dedicated owner for a [`mw_core::NativeRuntime`].
//!
//! The renderer can always clone the newest immutable snapshot, while territory deltas travel
//! through a separate bounded FIFO.  Deltas are never replaced: a full FIFO pauses the producer
//! until the renderer catches up or an explicit stop request arrives.

use std::{
    any::Any,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use mw_core::{NativeRuntime, RuntimeSnapshot, RuntimeState, TerritoryRenderUpdate};

const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Terminal state reported by a runtime worker.  Successful ticks are communicated through
/// [`RuntimeWorker::latest_snapshot`] and [`RuntimeWorker::drain_render_state`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeWorkerStatus {
    /// The caller requested shutdown before the worker reached a runtime terminal state.
    Stopped,
    /// The runtime published a clean or gated terminal state and will not be stepped again.
    Terminal(RuntimeState),
    /// The optional deterministic step limit was reached exactly.
    Completed { steps: u64 },
    /// `NativeRuntime::step` failed.  The previously published snapshot remains current.
    Failed(String),
    /// The worker encountered a panic rather than silently disappearing.
    Panicked(String),
}

#[derive(Debug)]
pub enum RuntimeWorkerError {
    ZeroTickInterval,
    ZeroUpdateQueueCapacity,
    ZeroStepLimit,
    Spawn(std::io::Error),
}

impl std::fmt::Display for RuntimeWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroTickInterval => {
                formatter.write_str("runtime worker tick interval must be nonzero")
            }
            Self::ZeroUpdateQueueCapacity => {
                formatter.write_str("runtime worker update queue capacity must be nonzero")
            }
            Self::ZeroStepLimit => formatter.write_str("runtime worker step limit must be nonzero"),
            Self::Spawn(error) => write!(formatter, "failed to spawn runtime worker: {error}"),
        }
    }
}

impl std::error::Error for RuntimeWorkerError {}

enum RuntimeWorkerControl {
    Stop,
}

/// One atomic producer-to-renderer transaction. A receiver sees a tick's terrain deltas and its
/// corresponding snapshot together, or sees neither.
struct Publication<T, S> {
    territory_updates: Vec<T>,
    snapshot: S,
}

/// One nonblocking renderer drain.  `snapshot` is the newest marker encountered, and it is
/// therefore coherent with all returned territory updates which precede it in FIFO order.
#[derive(Default)]
pub struct RuntimeWorkerDrain {
    pub territory_updates: Vec<Arc<TerritoryRenderUpdate>>,
    pub snapshot: Option<Arc<RuntimeSnapshot>>,
}

/// Handle owned by the render thread.  It never exposes mutable runtime state.
pub struct RuntimeWorker {
    latest: Arc<Mutex<Arc<RuntimeSnapshot>>>,
    messages: Receiver<Publication<Arc<TerritoryRenderUpdate>, Arc<RuntimeSnapshot>>>,
    statuses: Receiver<RuntimeWorkerStatus>,
    control: Sender<RuntimeWorkerControl>,
    join: Option<JoinHandle<()>>,
}

impl RuntimeWorker {
    /// Starts a named thread which exclusively owns `runtime`.
    ///
    /// `update_queue_capacity` must be nonzero.  It is intentionally bounded: territory changes
    /// are lossless and ordered, so slowing simulation is safer than consuming unlimited memory.
    pub fn spawn(
        runtime: NativeRuntime,
        tick_interval: Duration,
        update_queue_capacity: usize,
    ) -> Result<Self, RuntimeWorkerError> {
        Self::spawn_with_limit(runtime, tick_interval, update_queue_capacity, None)
    }

    /// Starts a worker with an optional exact successful-step limit. `Some(0)` is rejected; a
    /// limited worker publishes precisely `N` completed ticks before reporting `Completed`.
    pub fn spawn_with_limit(
        runtime: NativeRuntime,
        tick_interval: Duration,
        update_queue_capacity: usize,
        max_steps: Option<u64>,
    ) -> Result<Self, RuntimeWorkerError> {
        if tick_interval.is_zero() {
            return Err(RuntimeWorkerError::ZeroTickInterval);
        }
        if update_queue_capacity == 0 {
            return Err(RuntimeWorkerError::ZeroUpdateQueueCapacity);
        }
        if max_steps == Some(0) {
            return Err(RuntimeWorkerError::ZeroStepLimit);
        }
        let latest = Arc::new(Mutex::new(runtime.latest_snapshot()));
        let worker_latest = Arc::clone(&latest);
        let (message_tx, messages) = mpsc::sync_channel(update_queue_capacity);
        let (status_tx, statuses) = mpsc::channel();
        let (control, control_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("mw-native-runtime".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_worker(
                        runtime,
                        tick_interval,
                        worker_latest,
                        message_tx,
                        status_tx.clone(),
                        control_rx,
                        max_steps,
                    );
                }));
                if let Err(payload) = result {
                    let _ = status_tx.send(RuntimeWorkerStatus::Panicked(panic_message(payload)));
                }
            })
            .map_err(RuntimeWorkerError::Spawn)?;
        Ok(Self {
            latest,
            messages,
            statuses,
            control,
            join: Some(join),
        })
    }

    /// Clones the current immutable publication.  This is a newest-wins mailbox, not a FIFO.
    pub fn latest_snapshot(&self) -> Arc<RuntimeSnapshot> {
        self.latest
            .lock()
            .expect("runtime worker latest snapshot mutex poisoned")
            .clone()
    }

    /// Drains complete runtime publications without blocking. Apply `territory_updates` in order,
    /// then upload `snapshot` once (when present). Intermediate snapshots are intentionally
    /// collapsed, preserving newest-wins rendering while retaining every territory delta.
    pub fn drain_render_state(&self) -> RuntimeWorkerDrain {
        let (territory_updates, snapshot) = drain_publications(&self.messages);
        RuntimeWorkerDrain {
            territory_updates,
            snapshot,
        }
    }

    /// Receives one terminal status without blocking.
    pub fn poll_status(&self) -> Option<RuntimeWorkerStatus> {
        self.statuses.try_recv().ok()
    }

    /// Makes the worker stop promptly, including while its bounded update FIFO is full.
    pub fn stop(&self) {
        let _ = self.control.send(RuntimeWorkerControl::Stop);
    }

    /// Requests shutdown and waits for the worker. Call during application teardown, not a hot
    /// render callback.
    pub fn stop_and_join(&mut self) -> thread::Result<()> {
        self.stop();
        self.join()
    }

    /// Waits for a worker that has already been stopped or reached a terminal state.
    pub fn join(&mut self) -> thread::Result<()> {
        self.join.take().map_or(Ok(()), JoinHandle::join)
    }
}

impl Drop for RuntimeWorker {
    fn drop(&mut self) {
        self.stop();
        // Dropping `messages` next disconnects a worker blocked in `try_send` retries.  Joining in
        // Drop would risk blocking an event-loop callback, so explicit `stop_and_join` is the
        // clean shutdown path.
    }
}

fn run_worker(
    mut runtime: NativeRuntime,
    tick_interval: Duration,
    latest: Arc<Mutex<Arc<RuntimeSnapshot>>>,
    messages: SyncSender<Publication<Arc<TerritoryRenderUpdate>, Arc<RuntimeSnapshot>>>,
    statuses: Sender<RuntimeWorkerStatus>,
    control: Receiver<RuntimeWorkerControl>,
    max_steps: Option<u64>,
) {
    if !forward_initial_state(&mut runtime, &messages, &latest, &control) {
        let _ = statuses.send(RuntimeWorkerStatus::Stopped);
        return;
    }
    if is_terminal_state(runtime.state()) {
        let _ = statuses.send(RuntimeWorkerStatus::Terminal(runtime.state()));
        return;
    }

    let mut due_at = Instant::now();
    let mut completed_steps = 0_u64;
    loop {
        if wait_until_due(due_at, &control) {
            let _ = statuses.send(RuntimeWorkerStatus::Stopped);
            return;
        }
        let snapshot = match runtime.step() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = statuses.send(RuntimeWorkerStatus::Failed(error.to_string()));
                return;
            }
        };
        if !forward_tick(&mut runtime, snapshot.clone(), &messages, &latest, &control) {
            let _ = statuses.send(RuntimeWorkerStatus::Stopped);
            return;
        }
        completed_steps += 1;
        if reached_limit(max_steps, completed_steps) {
            let _ = statuses.send(RuntimeWorkerStatus::Completed {
                steps: completed_steps,
            });
            return;
        }
        if is_terminal_state(snapshot.state) {
            let _ = statuses.send(RuntimeWorkerStatus::Terminal(snapshot.state));
            return;
        }

        due_at = next_due(due_at, tick_interval, Instant::now());
    }
}

fn is_terminal_state(state: RuntimeState) -> bool {
    match state {
        RuntimeState::Running => false,
        RuntimeState::AwaitingStrategicEffects { .. }
        | RuntimeState::ConflictResolved { .. }
        | RuntimeState::Poisoned => true,
    }
}

fn reached_limit(max_steps: Option<u64>, completed_steps: u64) -> bool {
    max_steps == Some(completed_steps)
}

/// Advances exactly one deadline.  When a tick ran long, the following tick is due immediately;
/// the worker still performs only that one tick per loop iteration.
fn next_due(previous_due: Instant, tick_interval: Duration, now: Instant) -> Instant {
    let scheduled = previous_due + tick_interval;
    if scheduled < now { now } else { scheduled }
}

/// Returns true when stop was requested.
fn wait_until_due(due_at: Instant, control: &Receiver<RuntimeWorkerControl>) -> bool {
    let remaining = due_at.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return matches!(
            control.try_recv(),
            Ok(RuntimeWorkerControl::Stop) | Err(TryRecvError::Disconnected)
        );
    }
    match control.recv_timeout(remaining) {
        Ok(RuntimeWorkerControl::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
        Err(mpsc::RecvTimeoutError::Timeout) => false,
    }
}

/// Returns false if shutdown was requested or the receiver has gone away.
fn forward_initial_state(
    runtime: &mut NativeRuntime,
    messages: &SyncSender<Publication<Arc<TerritoryRenderUpdate>, Arc<RuntimeSnapshot>>>,
    latest: &Arc<Mutex<Arc<RuntimeSnapshot>>>,
    control: &Receiver<RuntimeWorkerControl>,
) -> bool {
    let snapshot = runtime.latest_snapshot();
    forward_tick(runtime, snapshot, messages, latest, control)
}

fn forward_tick(
    runtime: &mut NativeRuntime,
    snapshot: Arc<RuntimeSnapshot>,
    messages: &SyncSender<Publication<Arc<TerritoryRenderUpdate>, Arc<RuntimeSnapshot>>>,
    latest: &Arc<Mutex<Arc<RuntimeSnapshot>>>,
    control: &Receiver<RuntimeWorkerControl>,
) -> bool {
    let mut territory_updates = Vec::new();
    while let Some(update) = runtime.pop_render_update() {
        territory_updates.push(update);
    }
    if !send_lossless(
        messages,
        control,
        Publication {
            territory_updates,
            snapshot: snapshot.clone(),
        },
    ) {
        return false;
    }
    *latest
        .lock()
        .expect("runtime worker latest snapshot mutex poisoned") = snapshot;
    true
}

fn drain_publications<T, S>(receiver: &Receiver<Publication<T, S>>) -> (Vec<T>, Option<S>) {
    let mut territory_updates = Vec::new();
    let mut snapshot = None;
    while let Ok(publication) = receiver.try_recv() {
        territory_updates.extend(publication.territory_updates);
        snapshot = Some(publication.snapshot);
    }
    (territory_updates, snapshot)
}

/// Sends one item without losing it to a full FIFO.  The control wait is what makes a bounded
/// channel cancellable; `SyncSender::send` alone could otherwise block teardown indefinitely.
fn send_lossless<T>(
    updates: &SyncSender<T>,
    control: &Receiver<RuntimeWorkerControl>,
    mut value: T,
) -> bool {
    loop {
        match updates.try_send(value) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                value = returned;
                match control.recv_timeout(CONTROL_POLL_INTERVAL) {
                    Ok(RuntimeWorkerControl::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return false;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "runtime worker panicked with a non-string payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mw_core::{ConflictResolutionKind, ConflictResolutionPlan};

    #[test]
    fn native_runtime_is_send_for_dedicated_worker_ownership() {
        fn assert_send<T: Send>() {}
        assert_send::<NativeRuntime>();
    }

    #[test]
    fn stop_interrupts_a_full_fifo_wait() {
        let (updates, _receiver) = mpsc::sync_channel(1);
        updates.send(1_u8).unwrap();
        let (control_tx, control_rx) = mpsc::channel();
        let started = Instant::now();
        let handle = thread::spawn(move || send_lossless(&updates, &control_rx, 2_u8));
        control_tx.send(RuntimeWorkerControl::Stop).unwrap();
        assert!(!handle.join().unwrap());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn lossless_sender_preserves_fifo_after_backpressure() {
        let (updates, receiver) = mpsc::sync_channel(1);
        let (_control_tx, control_rx) = mpsc::channel();
        updates.send(1_u8).unwrap();
        let sender = thread::spawn(move || send_lossless(&updates, &control_rx, 2_u8));
        assert_eq!(receiver.recv().unwrap(), 1);
        assert!(sender.join().unwrap());
        assert_eq!(receiver.recv().unwrap(), 2);
    }

    #[test]
    fn a_slow_tick_has_no_extra_interval_cooldown() {
        let start = Instant::now();
        let now = start + Duration::from_millis(40);
        assert_eq!(
            next_due(start, Duration::from_millis(33), now),
            now,
            "the next tick is immediately due when its prior deadline is already behind"
        );
    }

    #[test]
    fn publication_is_atomic_and_drain_collapses_only_snapshots() {
        let (sender, receiver) = mpsc::sync_channel(2);
        sender
            .send(Publication {
                territory_updates: vec![1_u8, 2],
                snapshot: 10_u8,
            })
            .unwrap();
        sender
            .send(Publication {
                territory_updates: vec![3_u8],
                snapshot: 11_u8,
            })
            .unwrap();

        let (updates, snapshot) = drain_publications(&receiver);
        assert_eq!(updates, vec![1, 2, 3]);
        assert_eq!(snapshot, Some(11));
    }

    #[test]
    fn completion_limit_is_exact_and_zero_is_reserved_for_rejection() {
        assert!(!reached_limit(Some(3), 2));
        assert!(reached_limit(Some(3), 3));
        assert!(!reached_limit(None, 3));
        assert_eq!(
            RuntimeWorkerError::ZeroStepLimit.to_string(),
            "runtime worker step limit must be nonzero"
        );
    }

    #[test]
    fn resolved_conflict_is_a_clean_worker_terminal() {
        let state = RuntimeState::ConflictResolved {
            cycle: 4,
            tick: 2_400,
            resolution: ConflictResolutionPlan {
                kind: ConflictResolutionKind::FullCapitulation,
                winner_side: Some(1),
                stop_simulation: true,
            },
        };

        assert!(is_terminal_state(state));
        assert!(!is_terminal_state(RuntimeState::Running));
    }
}
