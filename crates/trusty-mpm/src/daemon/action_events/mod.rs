//! Hook events onto the trusty-events bus as `ActionEvent`s (#6849).
//!
//! Why: DOC-73 §3.4 makes trusty-mpm a producer on the one event bus
//! (ADR-0065), and the dashboard tree needs tm's hooks as typed actions with
//! a `parent_id` chain. The bus must never block, fail or slow a hook (§4.1,
//! the non-blocking invariant).
//! What: [`ActionEvents`] is the daemon's optional producer, a field on
//! `DaemonState`; `push_hook_event`, the one point every hook record passes,
//! calls [`ActionEvents::publish`]. Publishing projects the record
//! ([`projection::Threader`]) and buffers the result in a `PushClient`
//! (bounded by count and bytes, overflow counted) — no I/O on the hook path.
//! #9167: a relay record also yields `message` events ([`message`]), which
//! ride the same buffer, flush and failure handling. #9167 PR2: each hook
//! record also marks its session's transcript due ([`received`]); the flush
//! task reads due transcripts off the hook path and buffers one `received`
//! `message` event per cross-session message that arrived.
//! #9167 AC5: [`inject`] records each text the session manager types into
//! a pane, on the same buffer.
//! [`start`] attaches a publisher and its flush task at serve start;
//! [`ActionEvents::shutdown`], called from both exit paths, ends the task,
//! reads every tracked transcript once more, and runs one final flush, all within
//! [`FINAL_FLUSH_BUDGET`].
//! Test: `tests.rs` (socket level), `projection_tests.rs` (pure).
//!
//! # Spec References
//! - [`SPEC-UNIDASH-04~draft`](docs/specs/DOC-73-unified-mpm-code-agents-dashboard.md#SPEC-UNIDASH-04~draft)

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use trusty_common::control_bus::{PushClient, PushFlushOutcome};

use crate::core::hook::HookEventRecord;
use crate::daemon::DaemonState;

// #9167 AC5: each delivered inject, on the same buffer.
pub mod inject;
// #9167: inter-session messages, projected beside the actions.
pub mod message;
pub mod projection;
// #9167 PR2: the receiving side of a cross-session message.
pub mod received;
pub mod tmux_send;

/// How often the flush task drains the buffer (as #9078's producer).
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// The most the exit path waits on the bus.
pub const FINAL_FLUSH_BUDGET: Duration = Duration::from_secs(1);

/// The most tm's SIGTERM handler waits on the bus before `process::exit`
/// (#6849 fix round). Kept small because that wait delays the exit that
/// races the serve drain's pane shutdown (#6288); see [`before_signal_exit`].
pub const SIGNAL_FLUSH_BUDGET: Duration = Duration::from_millis(200);

/// At most one "events dropped" warning per this window.
const DROP_WARN_EVERY: Duration = Duration::from_secs(60);

/// The daemon's optional producer: empty until [`start`] attaches one.
///
/// Why: a `DaemonState` built by a test, or by a daemon isolated from the
/// host, must push nowhere; only the serving daemon dials a bus. The flush
/// task lives here too, so every exit path — the serve drain and the
/// binary's signal handler, which exits first (#6288) — can end it.
/// What: a set-once publisher slot and the flush task's handle.
/// [`Self::publish`] is a no-op while the slot is empty.
/// Test: `an_unresolvable_bus_still_ingests_hooks_6849`,
/// `shutdown_runs_the_final_flush_once_6849`.
#[derive(Default)]
pub struct ActionEvents {
    publisher: OnceLock<Arc<ActionPublisher>>,
    flush_task: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for ActionEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionEvents")
            .field("attached", &self.publisher.get().is_some())
            .finish()
    }
}

impl ActionEvents {
    /// Project and buffer `record` when a publisher is attached.
    pub fn publish(&self, record: &HookEventRecord) {
        if let Some(publisher) = self.publisher.get() {
            publisher.publish(record);
        }
    }

    /// Attach `publisher`; `false` when one is already attached.
    pub fn attach(&self, publisher: Arc<ActionPublisher>) -> bool {
        self.publisher.set(publisher).is_ok()
    }

    /// The attached publisher, if any.
    pub fn publisher(&self) -> Option<&Arc<ActionPublisher>> {
        self.publisher.get()
    }

    /// End the flush task, then run the final flush within
    /// [`FINAL_FLUSH_BUDGET`]. Idempotent; the serve drain's call.
    ///
    /// Test: `shutdown_runs_the_final_flush_once_6849`.
    pub async fn shutdown(&self) {
        self.shutdown_within(FINAL_FLUSH_BUDGET).await;
    }

    /// End the flush task, then run the final flush within `budget`.
    ///
    /// Why: tm's SIGTERM handler exits the process before
    /// `serve_with_shutdown`'s drain runs (#6288), so both exit paths flush;
    /// whichever runs first delivers. The signal path passes the shorter
    /// [`SIGNAL_FLUSH_BUDGET`].
    /// What: aborts and joins the flush task (an aborted write re-queues its
    /// frame), runs one last transcript scan within half of `budget`, then
    /// [`ActionPublisher::final_flush_within`] in what is left. The whole
    /// call is cut off at `budget`. A second call finds no task, and its
    /// scan finds reading closed.
    /// Test: `shutdown_runs_the_final_flush_once_6849`,
    /// `the_signal_path_flush_returns_within_its_budget_6849`,
    /// `an_arrival_unread_at_shutdown_is_sent_exactly_once_9167`.
    pub async fn shutdown_within(&self, budget: Duration) {
        let deadline = tokio::time::Instant::now() + budget;
        let task = self.flush_task.lock().take();
        let publisher = self.publisher.get();
        let work = async {
            if let Some(task) = task {
                task.abort();
                let _ = task.await;
            }
            if let Some(publisher) = publisher {
                // #9167: arrivals pending at shutdown; the flush keeps half.
                publisher.last_scan_within(budget / 2).await;
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                publisher.final_flush_within(left).await;
            }
        };
        // `final_flush_within` logs its own cut-off; this bounds the join too.
        let _ = tokio::time::timeout(budget, work).await;
    }
}

/// One buffered `PushClient` plus the projection state.
///
/// Why: one per daemon, shared behind an `Arc` by the hook path and the flush
/// task, as the `PushClient` contract asks.
/// What: `publish` projects under a short lock and buffers; `seq` is this
/// process's counter (the bus restamps it, DOC-73 §4.3).
/// Test: `hook_events_reach_the_bus_with_a_parent_chain_6849`.
pub struct ActionPublisher {
    client: PushClient,
    threader: parking_lot::Mutex<projection::Threader>,
    seq: AtomicU64,
    flush_interval: Duration,
    /// Transcript read positions for received messages (#9167).
    received: parking_lot::Mutex<received::Tails>,
    /// Held across one transcript scan, so two scans never race an offset.
    scanning: parking_lot::Mutex<()>,
}

impl ActionPublisher {
    /// A publisher dialling `socket`, showing paths relative to `$HOME`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            client: PushClient::new(socket),
            threader: parking_lot::Mutex::new(projection::Threader::new(dirs::home_dir())),
            seq: AtomicU64::new(0),
            flush_interval: DEFAULT_FLUSH_INTERVAL,
            received: parking_lot::Mutex::default(),
            scanning: parking_lot::Mutex::new(()),
        }
    }

    /// Show paths outside a session's `cwd` relative to `home`.
    pub fn with_home(self, home: Option<PathBuf>) -> Self {
        let mut threader = self.threader.lock();
        let bodies = threader.message_bodies();
        *threader = projection::Threader::new(home);
        threader.set_message_bodies(bodies);
        drop(threader);
        self
    }

    /// Carry the credential-scanned body on `message` events (#9167).
    pub fn with_message_bodies(self, on: bool) -> Self {
        self.threader.lock().set_message_bodies(on);
        self
    }

    /// Replace the flush task's interval.
    pub fn with_flush_interval(mut self, every: Duration) -> Self {
        self.flush_interval = every;
        self
    }

    /// Project `record` and buffer its events; never does I/O.
    ///
    /// Why: this runs on the hook path, which must not wait on the bus.
    /// What: projection and `PushClient::send` under one lock, so `seq`
    /// follows emission order and a parent is buffered before its child.
    /// Test: `ten_thousand_hooks_with_no_bus_are_fast_and_bounded_6849`,
    /// `a_stalled_bus_never_slows_hook_ingest_6849`.
    pub fn publish(&self, record: &HookEventRecord) {
        let mut threader = self.threader.lock();
        for mut event in threader.project(record) {
            event.seq = self.seq.fetch_add(1, Ordering::Relaxed);
            self.client.send(event);
        }
        drop(threader);
        // #9167: no I/O here; the flush task reads the transcript.
        self.received.lock().note(record);
    }

    /// Buffer a `received` event per cross-session message that reached a
    /// due transcript; returns how many (#9167 PR2).
    ///
    /// Why: the arrival is visible only in the receiving transcript, and
    /// reading it is file I/O, which the hook path must never do.
    /// What: runs `scan_received` on the blocking pool. A failed
    /// read is logged at debug and retried on the session's next hook.
    /// Test: `a_received_message_is_one_received_event_with_no_body_9167`,
    /// `n_messages_between_two_sessions_are_n_sent_and_n_received_9167`.
    pub async fn collect_received(self: &Arc<Self>) -> usize {
        let me = Arc::clone(self);
        match tokio::task::spawn_blocking(move || me.scan_received()).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(error = %e, "action events: transcript scan did not finish");
                0
            }
        }
    }

    /// [`Self::collect_received`]'s blocking body.
    fn scan_received(&self) -> usize {
        let _one_scan = self.scanning.lock();
        let jobs = self.received.lock().take_due();
        self.scan_jobs(jobs)
    }

    /// Read `jobs` and buffer their arrivals; returns how many.
    ///
    /// Why: a scan the shutdown abort left running on the blocking pool can
    /// finish after the final flush began; its events would then be lost
    /// while its offsets moved past them (#9167).
    /// What: each job's offset advance and event buffering happen under one
    /// `received` lock, and stop for good once [`received::Tails::close`]
    /// has run, so no offset passes an event that missed the final flush.
    /// Test: `a_scan_that_outlives_shutdown_buffers_nothing_9167`.
    fn scan_jobs(&self, jobs: Vec<received::Job>) -> usize {
        let mut found = 0;
        for job in jobs {
            let mut scan = match received::read(&job) {
                Ok(scan) => scan,
                Err(e) => {
                    tracing::debug!(error = %e, path = %job.path.display(), "action events: transcript unreadable");
                    continue;
                }
            };
            let (bodies, root) = {
                let threader = self.threader.lock();
                (threader.message_bodies(), threader.root_of(&job.session))
            };
            let events = received::events(
                job.session,
                std::mem::take(&mut scan.arrivals),
                bodies,
                root,
            );
            let mut tails = self.received.lock();
            if !tails.finish(&job, &scan) {
                break;
            }
            for mut event in events {
                event.seq = self.seq.fetch_add(1, Ordering::Relaxed);
                self.client.send(event);
                found += 1;
            }
        }
        found
    }

    /// Shutdown's last transcript read, then close reading (#9167).
    ///
    /// Why: an arrival still unread at shutdown would otherwise be lost, and
    /// a restart never counts it, because `since` excludes it.
    /// What: marks every tracked transcript due and runs one
    /// [`Self::collect_received`] cut off at `budget`; it waits on any scan
    /// already running, so that scan's events are buffered first. Then
    /// [`received::Tails::close`], so nothing is buffered after this returns.
    /// Test: `an_arrival_unread_at_shutdown_is_sent_exactly_once_9167`.
    async fn last_scan_within(self: &Arc<Self>, budget: Duration) {
        self.received.lock().mark_all_due();
        if tokio::time::timeout(budget, self.collect_received())
            .await
            .is_err()
        {
            tracing::warn!("action events: shutdown transcript scan outlived its budget");
        }
        self.received.lock().close();
    }

    /// Drain the buffer to the bus once (the only call that does I/O).
    pub async fn flush(&self) -> PushFlushOutcome {
        self.client.flush().await
    }

    /// Events waiting for a flush.
    pub fn buffered_len(&self) -> usize {
        self.client.buffered_len()
    }

    /// Events lost to buffer overflow since start.
    pub fn dropped(&self) -> u64 {
        self.client.dropped()
    }

    /// Drain once per interval until aborted. A slow flush delays the next
    /// tick rather than stacking; no connection is held between flushes.
    pub fn spawn_flush_task(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let every = me.flush_interval;
            let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut health = FlushHealth::default();
            loop {
                tick.tick().await;
                // #9167: received messages ride the same flush.
                me.collect_received().await;
                let outcome = me.client.flush().await;
                health.report(&outcome, me.client.dropped(), Instant::now());
            }
        })
    }

    /// The exit-path flush, bounded by [`FINAL_FLUSH_BUDGET`].
    ///
    /// Test: `final_flush_is_bounded_when_the_bus_stalls_6849`,
    /// `shutdown_runs_the_final_flush_once_6849`.
    pub async fn final_flush(&self) {
        self.final_flush_within(FINAL_FLUSH_BUDGET).await;
    }

    /// The exit-path flush, bounded by `budget`; logs what it left behind.
    pub async fn final_flush_within(&self, budget: Duration) {
        match tokio::time::timeout(budget, self.client.flush()).await {
            Ok(PushFlushOutcome { error: None, sent }) => {
                tracing::debug!(sent, "action events: final flush delivered");
            }
            Ok(PushFlushOutcome {
                error: Some(e),
                sent,
            }) => tracing::warn!(
                sent,
                undelivered = self.client.buffered_len(),
                error = %e,
                "action events: final flush stopped; the rest are lost"
            ),
            // The cut-off write re-queued its frame, so `undelivered` counts it.
            Err(_) => tracing::warn!(
                undelivered = self.client.buffered_len(),
                "action events: final flush outlived its budget; the rest are lost"
            ),
        }
    }
}

/// Rate-limits what the flush task says about the bus.
///
/// Why: a bus down for an hour must cost one warning, not 3,600, and
/// overflow must be visible without flooding the log (DOC-73 §4.2).
/// What: warns once when the bus becomes unreachable, logs once when it
/// recovers, and warns about a grown drop count at most once per
/// [`DROP_WARN_EVERY`]. `report` returns what it logged.
/// Test: `flush_health_warns_once_per_outage_6849`.
#[derive(Default)]
pub(crate) struct FlushHealth {
    down: bool,
    dropped_seen: u64,
    last_drop_warn: Option<Instant>,
}

/// What [`FlushHealth::report`] logged.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    Down,
    Recovered,
    Dropped(u64),
}

impl FlushHealth {
    pub(crate) fn report(
        &mut self,
        outcome: &PushFlushOutcome,
        dropped: u64,
        now: Instant,
    ) -> Vec<Notice> {
        let mut said = Vec::new();
        match (&outcome.error, self.down) {
            (Some(e), false) => {
                tracing::warn!(error = %e, "action events: trusty-events unreachable; buffering (bounded)");
                self.down = true;
                said.push(Notice::Down);
            }
            (None, true) if outcome.sent > 0 => {
                tracing::info!("action events: trusty-events reachable again");
                self.down = false;
                said.push(Notice::Recovered);
            }
            _ => {}
        }
        let due = self
            .last_drop_warn
            .is_none_or(|t| now.duration_since(t) >= DROP_WARN_EVERY);
        if dropped > self.dropped_seen && due {
            tracing::warn!(
                dropped,
                "action events: buffer overflowed; oldest events dropped"
            );
            self.dropped_seen = dropped;
            self.last_drop_warn = Some(now);
            said.push(Notice::Dropped(dropped));
        }
        said
    }
}

/// tm's SIGTERM handler steps before `process::exit`, in order.
///
/// Why: the handler's exit races the serve drain, whose `mgr.shutdown()`
/// stops owned panes (#6288); every millisecond spent here widens that race,
/// so the flush gets [`SIGNAL_FLUSH_BUDGET`], not [`FINAL_FLUSH_BUDGET`]. The
/// lock file names a live daemon until the flush ends, so a spawner never
/// sees "no daemon" while this one still serves its socket.
/// What: [`ActionEvents::shutdown_within`] with [`SIGNAL_FLUSH_BUDGET`], then
/// `remove_lock`.
/// Test: `the_signal_path_flush_returns_within_its_budget_6849`,
/// `the_signal_path_flushes_before_removing_the_lock_6849`.
pub async fn before_signal_exit(state: &DaemonState, remove_lock: impl FnOnce()) {
    state
        .action_events()
        .shutdown_within(SIGNAL_FLUSH_BUDGET)
        .await;
    remove_lock();
}

/// Whether this daemon may dial a bus at all.
///
/// Why: `events_socket_path` follows the data dir, not `$HOME` or the
/// framework root. A daemon isolated by either of those (a test, a scratch
/// root) but sharing the host's data dir would push into the operator's bus.
/// What: `true` when the data dir is overridden (the bus is the sandbox's
/// own) or the daemon runs in the operator's real environment.
/// Test: `a_scratch_daemon_dials_no_host_bus_6849`.
pub fn bus_allowed(data_dir_overridden: bool, real_environment: bool) -> bool {
    data_dir_overridden || real_environment
}

/// [`start_with`] on this process's environment and `events_socket_path`.
pub fn start(state: &DaemonState) -> bool {
    let overridden =
        std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).is_some_and(|v| !v.is_empty());
    let real = crate::core::host_state_gate::host_state_access_for_root(state.framework_root())
        .is_allowed();
    start_with(
        state,
        bus_allowed(overridden, real),
        DEFAULT_FLUSH_INTERVAL,
        trusty_common::control_bus::events_socket_path,
    )
}

/// Attach a publisher on `resolve`'s socket when `allowed`, and start its
/// flush task, draining every `flush_every`.
///
/// Why: a bus that is disallowed or cannot be resolved is a log line, never
/// a failed start (DOC-73 §4.1).
/// What: attaches nothing and returns `false` when `allowed` is false
/// (without calling `resolve`), when `resolve` fails, or when a publisher is
/// already attached; otherwise attaches one, keeps its flush task on
/// [`ActionEvents`] for [`ActionEvents::shutdown`], and returns `true`.
/// Test: `an_unresolvable_bus_still_ingests_hooks_6849`,
/// `a_scratch_daemon_dials_no_host_bus_6849`,
/// `shutdown_runs_the_final_flush_once_6849`.
pub fn start_with(
    state: &DaemonState,
    allowed: bool,
    flush_every: Duration,
    resolve: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> bool {
    if !allowed {
        tracing::debug!("action events: isolated daemon on the host data dir; no bus");
        return false;
    }
    match resolve() {
        Ok(socket) => {
            // #9167: message bodies only when the operator turned them on.
            let publisher = Arc::new(
                ActionPublisher::new(socket)
                    .with_flush_interval(flush_every)
                    .with_message_bodies(message::bodies_from_env()),
            );
            let events = state.action_events();
            if !events.attach(Arc::clone(&publisher)) {
                return false;
            }
            *events.flush_task.lock() = Some(publisher.spawn_flush_task());
            true
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "action events: no trusty-events socket; events are off for this run");
            false
        }
    }
}

#[cfg(test)]
mod message_tests;
#[cfg(test)]
mod received_tests;
#[cfg(test)]
mod tests;
