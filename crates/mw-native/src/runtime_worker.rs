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

use mw_core::{
    NativeRuntime, NativeRuntimeCheckpointState, RuntimeSnapshot, RuntimeState,
    TerritoryRenderUpdate,
};

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

#[derive(Debug, Clone)]
pub struct RuntimeWorkerCheckpointError(String);
impl std::fmt::Display for RuntimeWorkerCheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for RuntimeWorkerCheckpointError {}

enum RuntimeWorkerControl {
    Stop,
    Checkpoint(Sender<Result<NativeRuntimeCheckpointState, RuntimeWorkerCheckpointError>>),
    Pause,
    Resume,
    SetTickInterval(Duration),
}

/// Nonterminal acknowledgement emitted after a live control takes effect at a published runtime
/// boundary. These events are separate from [`RuntimeWorkerStatus`] so exact-step/headless callers
/// can continue treating every status as terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeWorkerControlEvent {
    Paused {
        tick: u64,
        frame: u64,
    },
    Resumed {
        tick: u64,
        frame: u64,
    },
    TickIntervalChanged {
        interval: Duration,
        tick: u64,
        frame: u64,
    },
    /// The worker has entered terminal/checkpoint-service mode and rejected a live control.
    Unavailable {
        tick: u64,
        frame: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeWorkerControlError {
    ZeroTickInterval,
    Stopped,
}

struct RuntimeWorkerEventSenders {
    status: Sender<RuntimeWorkerStatus>,
    control: Sender<RuntimeWorkerControlEvent>,
}

impl std::fmt::Display for RuntimeWorkerControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroTickInterval => {
                formatter.write_str("runtime worker tick interval must be nonzero")
            }
            Self::Stopped => formatter.write_str("runtime worker stopped"),
        }
    }
}

impl std::error::Error for RuntimeWorkerControlError {}

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
    control_events: Receiver<RuntimeWorkerControlEvent>,
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
        let (control_event_tx, control_events) = mpsc::channel();
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
                        RuntimeWorkerEventSenders {
                            status: status_tx.clone(),
                            control: control_event_tx,
                        },
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
            control_events,
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

    /// Requests a pause without blocking the caller. [`RuntimeWorkerControlEvent::Paused`] is
    /// emitted only after any already-completed tick has been published and made current.
    pub fn request_pause(&self) -> Result<(), RuntimeWorkerControlError> {
        self.send_control(RuntimeWorkerControl::Pause)
    }

    /// Requests a resume without blocking the caller. Resuming schedules one immediately due tick;
    /// time spent paused is never replayed as a catch-up burst.
    pub fn request_resume(&self) -> Result<(), RuntimeWorkerControlError> {
        self.send_control(RuntimeWorkerControl::Resume)
    }

    /// Changes the live tick interval without blocking the caller. The new interval starts when
    /// the worker applies the command at a published boundary.
    pub fn request_tick_interval(
        &self,
        interval: Duration,
    ) -> Result<(), RuntimeWorkerControlError> {
        if interval.is_zero() {
            return Err(RuntimeWorkerControlError::ZeroTickInterval);
        }
        self.send_control(RuntimeWorkerControl::SetTickInterval(interval))
    }

    /// Receives one nonterminal live-control acknowledgement without blocking.
    pub fn poll_control_event(&self) -> Option<RuntimeWorkerControlEvent> {
        self.control_events.try_recv().ok()
    }

    fn send_control(&self, control: RuntimeWorkerControl) -> Result<(), RuntimeWorkerControlError> {
        self.control
            .send(control)
            .map_err(|_| RuntimeWorkerControlError::Stopped)
    }

    /// Makes the worker stop promptly, including while its bounded update FIFO is full.
    pub fn stop(&self) {
        let _ = self.control.send(RuntimeWorkerControl::Stop);
    }

    pub fn checkpoint_state(
        &self,
    ) -> Result<NativeRuntimeCheckpointState, RuntimeWorkerCheckpointError> {
        let (reply, result) = mpsc::channel();
        self.control
            .send(RuntimeWorkerControl::Checkpoint(reply))
            .map_err(|_| RuntimeWorkerCheckpointError("runtime worker stopped".to_owned()))?;
        result
            .recv()
            .map_err(|_| RuntimeWorkerCheckpointError("runtime worker stopped".to_owned()))?
    }

    /// Requests shutdown and waits for the worker. Call during application teardown, not a hot
    /// render callback.
    pub fn stop_and_join(&mut self) -> thread::Result<()> {
        self.join()
    }

    /// Stops checkpoint-service mode, then waits for the worker thread.
    pub fn join(&mut self) -> thread::Result<()> {
        self.stop();
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
    events: RuntimeWorkerEventSenders,
    control: Receiver<RuntimeWorkerControl>,
    max_steps: Option<u64>,
) {
    let initial_controls = match forward_initial_state(&mut runtime, &messages, &latest, &control) {
        Some(controls) => controls,
        None => {
            let _ = events.status.send(RuntimeWorkerStatus::Stopped);
            return;
        }
    };
    if is_terminal_state(runtime.state()) {
        let _ = events
            .status
            .send(RuntimeWorkerStatus::Terminal(runtime.state()));
        serve_terminal(&mut runtime, &control, &events.control);
        return;
    }

    let mut tick_interval = tick_interval;
    let mut due_at = Instant::now();
    let mut paused = false;
    for request in initial_controls {
        if apply_live_control(
            request,
            &mut runtime,
            &mut due_at,
            &mut tick_interval,
            &mut paused,
            &events.control,
        ) {
            let _ = events.status.send(RuntimeWorkerStatus::Stopped);
            return;
        }
    }
    let mut completed_steps = 0_u64;
    loop {
        if wait_until_due(
            &mut runtime,
            &mut due_at,
            &mut tick_interval,
            &mut paused,
            &events.control,
            &control,
        ) {
            let _ = events.status.send(RuntimeWorkerStatus::Stopped);
            return;
        }
        let snapshot = match runtime.step() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = events
                    .status
                    .send(RuntimeWorkerStatus::Failed(error.to_string()));
                return;
            }
        };
        let deferred =
            match forward_tick(&mut runtime, snapshot.clone(), &messages, &latest, &control) {
                Some(deferred) => deferred,
                None => {
                    let _ = events.status.send(RuntimeWorkerStatus::Stopped);
                    return;
                }
            };
        // Establish the ordinary next deadline first. Deferred controls were received while this
        // tick's publication was backpressured and may intentionally replace this schedule.
        due_at = next_due(due_at, tick_interval, Instant::now());
        for request in deferred {
            if apply_live_control(
                request,
                &mut runtime,
                &mut due_at,
                &mut tick_interval,
                &mut paused,
                &events.control,
            ) {
                let _ = events.status.send(RuntimeWorkerStatus::Stopped);
                return;
            }
        }
        completed_steps += 1;
        if reached_limit(max_steps, completed_steps) {
            let _ = events.status.send(RuntimeWorkerStatus::Completed {
                steps: completed_steps,
            });
            serve_terminal(&mut runtime, &control, &events.control);
            return;
        }
        if is_terminal_state(snapshot.state) {
            let _ = events
                .status
                .send(RuntimeWorkerStatus::Terminal(snapshot.state));
            serve_terminal(&mut runtime, &control, &events.control);
            return;
        }
    }
}

fn serve_terminal(
    runtime: &mut NativeRuntime,
    control: &Receiver<RuntimeWorkerControl>,
    control_events: &Sender<RuntimeWorkerControlEvent>,
) {
    while let Ok(request) = control.recv() {
        match request {
            RuntimeWorkerControl::Stop => break,
            RuntimeWorkerControl::Checkpoint(reply) => {
                let _ = reply.send(
                    runtime
                        .checkpoint_state()
                        .map_err(|error| RuntimeWorkerCheckpointError(error.to_string())),
                );
            }
            RuntimeWorkerControl::Pause
            | RuntimeWorkerControl::Resume
            | RuntimeWorkerControl::SetTickInterval(_) => {
                let snapshot = runtime.latest_snapshot();
                let _ = control_events.send(RuntimeWorkerControlEvent::Unavailable {
                    tick: snapshot.tick,
                    frame: snapshot.frame,
                });
            }
        }
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
fn wait_until_due(
    runtime: &mut NativeRuntime,
    due_at: &mut Instant,
    tick_interval: &mut Duration,
    paused: &mut bool,
    control_events: &Sender<RuntimeWorkerControlEvent>,
    control: &Receiver<RuntimeWorkerControl>,
) -> bool {
    loop {
        if *paused {
            match control.recv() {
                Ok(request) => {
                    if apply_live_control(
                        request,
                        runtime,
                        due_at,
                        tick_interval,
                        paused,
                        control_events,
                    ) {
                        return true;
                    }
                    continue;
                }
                Err(_) => return true,
            }
        }
        let remaining = due_at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            match control.try_recv() {
                Ok(request) => {
                    if apply_live_control(
                        request,
                        runtime,
                        due_at,
                        tick_interval,
                        paused,
                        control_events,
                    ) {
                        return true;
                    }
                    continue;
                }
                Err(TryRecvError::Disconnected) => return true,
                Err(TryRecvError::Empty) => return false,
            }
        }
        match control.recv_timeout(remaining) {
            Ok(request) => {
                if apply_live_control(
                    request,
                    runtime,
                    due_at,
                    tick_interval,
                    paused,
                    control_events,
                ) {
                    return true;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return true,
            Err(mpsc::RecvTimeoutError::Timeout) => return false,
        }
    }
}

fn apply_live_control(
    request: RuntimeWorkerControl,
    runtime: &mut NativeRuntime,
    due_at: &mut Instant,
    tick_interval: &mut Duration,
    paused: &mut bool,
    events: &Sender<RuntimeWorkerControlEvent>,
) -> bool {
    let snapshot = runtime.latest_snapshot();
    match request {
        RuntimeWorkerControl::Stop => true,
        RuntimeWorkerControl::Checkpoint(reply) => {
            let _ = reply.send(
                runtime
                    .checkpoint_state()
                    .map_err(|error| RuntimeWorkerCheckpointError(error.to_string())),
            );
            false
        }
        RuntimeWorkerControl::Pause => {
            *paused = true;
            let _ = events.send(RuntimeWorkerControlEvent::Paused {
                tick: snapshot.tick,
                frame: snapshot.frame,
            });
            false
        }
        RuntimeWorkerControl::Resume => {
            *paused = false;
            *due_at = Instant::now();
            let _ = events.send(RuntimeWorkerControlEvent::Resumed {
                tick: snapshot.tick,
                frame: snapshot.frame,
            });
            false
        }
        RuntimeWorkerControl::SetTickInterval(interval) => {
            debug_assert!(!interval.is_zero());
            *tick_interval = interval;
            if !*paused {
                *due_at = Instant::now() + interval;
            }
            let _ = events.send(RuntimeWorkerControlEvent::TickIntervalChanged {
                interval,
                tick: snapshot.tick,
                frame: snapshot.frame,
            });
            false
        }
    }
}

/// Returns false if shutdown was requested or the receiver has gone away.
fn forward_initial_state(
    runtime: &mut NativeRuntime,
    messages: &SyncSender<Publication<Arc<TerritoryRenderUpdate>, Arc<RuntimeSnapshot>>>,
    latest: &Arc<Mutex<Arc<RuntimeSnapshot>>>,
    control: &Receiver<RuntimeWorkerControl>,
) -> Option<Vec<RuntimeWorkerControl>> {
    let snapshot = runtime.latest_snapshot();
    forward_tick(runtime, snapshot, messages, latest, control)
}

fn forward_tick(
    runtime: &mut NativeRuntime,
    snapshot: Arc<RuntimeSnapshot>,
    messages: &SyncSender<Publication<Arc<TerritoryRenderUpdate>, Arc<RuntimeSnapshot>>>,
    latest: &Arc<Mutex<Arc<RuntimeSnapshot>>>,
    control: &Receiver<RuntimeWorkerControl>,
) -> Option<Vec<RuntimeWorkerControl>> {
    let mut territory_updates = Vec::new();
    while let Some(update) = runtime.pop_render_update() {
        territory_updates.push(update);
    }
    let deferred = send_lossless(
        Some(runtime),
        messages,
        control,
        Publication {
            territory_updates,
            snapshot: snapshot.clone(),
        },
    )?;
    *latest
        .lock()
        .expect("runtime worker latest snapshot mutex poisoned") = snapshot;
    Some(deferred)
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
    mut runtime: Option<&mut NativeRuntime>,
    updates: &SyncSender<T>,
    control: &Receiver<RuntimeWorkerControl>,
    mut value: T,
) -> Option<Vec<RuntimeWorkerControl>> {
    let mut deferred = Vec::new();
    loop {
        match updates.try_send(value) {
            Ok(()) => return Some(deferred),
            Err(TrySendError::Full(returned)) => {
                value = returned;
                match control.recv_timeout(CONTROL_POLL_INTERVAL) {
                    Ok(RuntimeWorkerControl::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return None;
                    }
                    Ok(RuntimeWorkerControl::Checkpoint(reply)) => {
                        let result = runtime.as_deref_mut().map_or_else(
                            || {
                                Err(RuntimeWorkerCheckpointError(
                                    "runtime unavailable".to_owned(),
                                ))
                            },
                            |runtime| {
                                runtime
                                    .checkpoint_state()
                                    .map_err(|e| RuntimeWorkerCheckpointError(e.to_string()))
                            },
                        );
                        let _ = reply.send(result);
                    }
                    Ok(request @ RuntimeWorkerControl::Pause)
                    | Ok(request @ RuntimeWorkerControl::Resume)
                    | Ok(request @ RuntimeWorkerControl::SetTickInterval(_)) => {
                        deferred.push(request);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            Err(TrySendError::Disconnected(_)) => return None,
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
    use mw_core::{
        ConflictResolutionKind, ConflictResolutionPlan, DecodedScenario, GridSpec,
        NativeWarBootstrapConfig, ProductionConfig, bootstrap_native_war,
    };

    fn test_runtime() -> NativeRuntime {
        let grid = GridSpec {
            grid_res: 90.0,
            width: 4,
            height: 2,
        };
        let decoded = DecodedScenario {
            metadata: serde_json::json!({
                "metadata": [
                    {"id": 7, "name": "Seven", "gdp": 100, "population": 1_000_000},
                    {"id": 11, "name": "Eleven", "gdp": 80, "population": 800_000}
                ]
            }),
            source: grid,
            target: grid,
            entry_count: 6,
            world_control: vec![7, 11, 0, 0, 7, 11, 0, 0],
            de_jure: vec![7, 11, 0, 0, 7, 11, 0, 0],
            land: vec![1, 1, 0, 0, 1, 1, 0, 0],
            biome: vec![0; 8],
            province: vec![0; 8],
        };
        bootstrap_native_war(
            &decoded,
            &NativeWarBootstrapConfig {
                sides: vec![vec![7], vec![11]],
                hostility: None,
                production: ProductionConfig::default(),
                war_grace_end: 600,
            },
        )
        .unwrap()
    }

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
        let handle = thread::spawn(move || send_lossless(None, &updates, &control_rx, 2_u8));
        control_tx.send(RuntimeWorkerControl::Stop).unwrap();
        assert!(handle.join().unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn lossless_sender_preserves_fifo_after_backpressure() {
        let (updates, receiver) = mpsc::sync_channel(1);
        let (_control_tx, control_rx) = mpsc::channel();
        updates.send(1_u8).unwrap();
        let sender = thread::spawn(move || send_lossless(None, &updates, &control_rx, 2_u8));
        assert_eq!(receiver.recv().unwrap(), 1);
        assert!(sender.join().unwrap().is_some());
        assert_eq!(receiver.recv().unwrap(), 2);
    }

    #[test]
    fn checkpoint_request_is_served_while_fifo_is_full_without_reordering() {
        let mut runtime = test_runtime();
        let (updates, receiver) = mpsc::sync_channel(1);
        updates.send(1_u8).unwrap();
        let (control_tx, control_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let worker =
            thread::spawn(move || send_lossless(Some(&mut runtime), &updates, &control_rx, 2_u8));
        control_tx
            .send(RuntimeWorkerControl::Checkpoint(reply_tx))
            .unwrap();
        let state = reply_rx.recv().unwrap().unwrap();
        assert_eq!(state.tick, 0);
        assert_eq!(state.frame, 0);
        assert_eq!(receiver.recv().unwrap(), 1);
        assert!(worker.join().unwrap().is_some());
        assert_eq!(receiver.recv().unwrap(), 2);
    }

    #[test]
    fn checkpoint_request_does_not_advance_the_tick_deadline() {
        let mut runtime = test_runtime();
        let (control_tx, control_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let due_at = Instant::now() + Duration::from_millis(250);
        let waiter = thread::spawn(move || {
            let mut due_at = due_at;
            let mut tick_interval = Duration::from_millis(250);
            let mut paused = false;
            let (event_tx, _event_rx) = mpsc::channel();
            let stopped = wait_until_due(
                &mut runtime,
                &mut due_at,
                &mut tick_interval,
                &mut paused,
                &event_tx,
                &control_rx,
            );
            finished_tx.send(stopped).unwrap();
        });

        let (reply_tx, reply_rx) = mpsc::channel();
        control_tx
            .send(RuntimeWorkerControl::Checkpoint(reply_tx))
            .unwrap();
        let state = reply_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(state.tick, 0);
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        control_tx.send(RuntimeWorkerControl::Stop).unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        waiter.join().unwrap();
    }

    #[test]
    fn pause_is_deferred_until_a_backpressured_publication_is_enqueued() {
        let mut runtime = test_runtime();
        let (updates, receiver) = mpsc::sync_channel(1);
        updates.send(1_u8).unwrap();
        let (control_tx, control_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let sender = thread::spawn(move || {
            let result = send_lossless(Some(&mut runtime), &updates, &control_rx, 2_u8);
            finished_tx.send(result).unwrap();
        });

        control_tx.send(RuntimeWorkerControl::Pause).unwrap();
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(receiver.recv().unwrap(), 1);
        let deferred = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(deferred.len(), 1);
        assert!(matches!(deferred[0], RuntimeWorkerControl::Pause));
        assert_eq!(receiver.recv().unwrap(), 2);
        sender.join().unwrap();
    }

    #[test]
    fn asynchronous_pause_holds_an_exact_tick_until_resume() {
        let mut worker = RuntimeWorker::spawn(test_runtime(), Duration::from_millis(5), 4).unwrap();
        worker.request_pause().unwrap();

        let started = Instant::now();
        let paused_tick = loop {
            let _ = worker.drain_render_state();
            if let Some(RuntimeWorkerControlEvent::Paused { tick, frame }) =
                worker.poll_control_event()
            {
                assert_eq!(tick, frame);
                break tick;
            }
            assert!(started.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(1));
        };
        thread::sleep(Duration::from_millis(30));
        let _ = worker.drain_render_state();
        assert_eq!(worker.latest_snapshot().tick, paused_tick);
        let paused_interval = Duration::from_millis(2);
        worker.request_tick_interval(paused_interval).unwrap();
        let interval_started = Instant::now();
        loop {
            if matches!(
                worker.poll_control_event(),
                Some(RuntimeWorkerControlEvent::TickIntervalChanged { interval, .. })
                    if interval == paused_interval
            ) {
                break;
            }
            assert!(interval_started.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(1));
        }
        let checkpoint = worker.checkpoint_state().unwrap();
        assert_eq!(checkpoint.tick, paused_tick);
        thread::sleep(Duration::from_millis(15));
        let _ = worker.drain_render_state();
        assert_eq!(worker.latest_snapshot().tick, paused_tick);

        worker.request_resume().unwrap();
        let resumed_started = Instant::now();
        loop {
            let _ = worker.drain_render_state();
            if worker.latest_snapshot().tick > paused_tick {
                break;
            }
            assert!(resumed_started.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(1));
        }
        worker.stop_and_join().unwrap();
    }

    #[test]
    fn live_interval_rejects_zero_and_acknowledges_at_a_boundary() {
        let mut worker =
            RuntimeWorker::spawn(test_runtime(), Duration::from_millis(100), 4).unwrap();
        assert_eq!(
            worker.request_tick_interval(Duration::ZERO),
            Err(RuntimeWorkerControlError::ZeroTickInterval)
        );
        let interval = Duration::from_millis(7);
        worker.request_tick_interval(interval).unwrap();
        let started = Instant::now();
        loop {
            let _ = worker.drain_render_state();
            if let Some(RuntimeWorkerControlEvent::TickIntervalChanged {
                interval: acknowledged,
                tick,
                frame,
            }) = worker.poll_control_event()
            {
                assert_eq!(acknowledged, interval);
                assert_eq!(tick, frame);
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(1));
        }
        worker.stop_and_join().unwrap();
    }

    #[test]
    fn terminal_worker_explicitly_rejects_live_controls() {
        let mut worker =
            RuntimeWorker::spawn_with_limit(test_runtime(), Duration::from_millis(1), 4, Some(1))
                .unwrap();
        let started = Instant::now();
        loop {
            let _ = worker.drain_render_state();
            if matches!(
                worker.poll_status(),
                Some(RuntimeWorkerStatus::Completed { steps: 1 })
            ) {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(1));
        }

        worker.request_pause().unwrap();
        let rejected_started = Instant::now();
        loop {
            if let Some(RuntimeWorkerControlEvent::Unavailable { tick, frame }) =
                worker.poll_control_event()
            {
                assert_eq!(tick, 1);
                assert_eq!(frame, 1);
                break;
            }
            assert!(rejected_started.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(1));
        }
        worker.stop_and_join().unwrap();
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
