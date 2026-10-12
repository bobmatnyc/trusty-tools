//! The daemon's route-policy snapshot (#8433 P1, #8454).
//!
//! Why: the daemon enforces chat-channel routes on every send and every
//! inbound answer. Reloading the policy per call runs git per project, and a
//! policy the daemon cannot re-verify must not stay in force for ever (#8454
//! S2c Q4: 5 s refresh, 60 s max age). This is a Fail-Open Check site: no
//! failure here may serve an old policy past max age or allow by default.
//! What: [`PolicySnapshot`] holds a [`PolicySource`] (in the daemon, a
//! trusty-channels `PolicyLoader` in [`LoaderSource`]). A check starts one
//! refresh on its own thread when [`REFRESH_INTERVAL`] has passed since the
//! last one finished, and never waits on it (#9777). It serves the published
//! report only while the last successful refresh started at most
//! [`MAX_AGE`] ago and the report is not `denied`.
//! Test: `snapshot_tests.rs`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use trusty_channels::policy::{
    Channel, Clock, DenyReason, DropReason, EgressDecision, FileState, Finding, GateError,
    InboundDecision, LoadReport, LoadRequest, MessageKind, MonotonicClock, PolicyLoader,
    ProjectFileError, QuestionId, QuestionState, Route,
};

/// The least time between two refresh attempts (#8454 S2c Q4).
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// The oldest a successful refresh may be before everything denies
/// (#8454 S2c Q4).
pub const MAX_AGE: Duration = Duration::from_secs(60);

/// The channels whose routes the daemon serves; gchat-mcp serves gchat.
const DAEMON_CHANNELS: [Channel; 2] = [Channel::Slack, Channel::Telegram];

/// The snapshot type the daemon holds.
pub type DaemonPolicySnapshot = PolicySnapshot<LoaderSource, MonotonicClock>;

/// Why the snapshot refused before any route was consulted.
///
/// Why: a caller audits which rule refused; a snapshot refusal is never a
/// route verdict and must not read as one.
/// What: one variant per fail-closed arm of [`PolicySnapshot::current`].
/// Test: `snapshot_older_than_max_age_denies`,
/// `denied_report_denies_egress_and_inbound`,
/// `refresh_error_keeps_last_good_only_until_max_age`,
/// `poisoned_snapshot_lock_denies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotDeny {
    /// No refresh has completed yet.
    #[error("no channel policy load has completed")]
    NeverLoaded,
    /// The last successful refresh is older than `max`.
    #[error("channel policy is {age:?} old, past the {max:?} max age")]
    Stale {
        /// Time since the last successful refresh.
        age: Duration,
        /// [`MAX_AGE`].
        max: Duration,
    },
    /// The load was denied: a host-ceiling fault or a cross-file overlap.
    #[error("channel policy load was denied")]
    Denied,
    /// The snapshot lock was poisoned; nothing is served again.
    #[error("channel policy snapshot lock is poisoned")]
    Poisoned,
}

/// A refused check: by the snapshot, or by the policy's own rule.
///
/// Why: one type for both, so a caller cannot read a snapshot deny as a
/// route deny or skip it.
/// What: `Snapshot` from [`SnapshotDeny`]; `Policy` carries the
/// trusty-channels reason ([`DenyReason`] or [`DropReason`]).
/// Test: `denied_report_denies_egress_and_inbound`,
/// `empty_policy_with_enabled_channel_denies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal<R> {
    /// The snapshot refused before any route was read.
    Snapshot(SnapshotDeny),
    /// The policy refused.
    Policy(R),
}

/// Where a snapshot gets a fresh report.
///
/// Why: the snapshot's timing and fail-closed rules are tested without git;
/// the daemon plugs in [`LoaderSource`].
/// What: `refresh` reloads as needed and returns the report now in effect.
/// It may panic; the snapshot catches the panic and counts it as a failed
/// refresh, so `refresh` must leave `self` usable after a panic.
/// Test: `refresh_within_interval_spawns_no_git`.
pub trait PolicySource: Send {
    /// Reload as needed and return the report now in effect.
    fn refresh(&mut self) -> LoadReport;
}

/// The daemon's source: a trusty-channels `PolicyLoader`.
///
/// Why: `PolicyLoader` writes its settled fingerprint before it applies the
/// new report, so a panic while applying would leave a loader whose
/// fingerprint does not describe its report, and that change would never
/// reload. #9808: on a normal return the fingerprint describes the bytes
/// the load read, or is unset so the next refresh reloads; keeping the
/// clone then keeps no stale report.
/// What: loads lazily on the first refresh, then refreshes a clone and
/// keeps it only once the refresh returned.
/// Test: `routes_edit_outside_config_yaml_has_no_effect_in_daemon`,
/// `overlapping_routes_deny_daemon_snapshot`.
#[derive(Debug)]
pub struct LoaderSource {
    req: LoadRequest,
    loader: Option<PolicyLoader>,
}

impl LoaderSource {
    /// A source for `req`; nothing is read until the first refresh.
    pub fn new(req: LoadRequest) -> Self {
        Self { req, loader: None }
    }
}

impl PolicySource for LoaderSource {
    fn refresh(&mut self) -> LoadReport {
        // #8454: work on a copy; a panic leaves `self.loader` as it was.
        let next = match &self.loader {
            Some(loader) => {
                let mut next = loader.clone();
                next.refresh();
                next
            }
            None => PolicyLoader::new(self.req.clone()),
        };
        let report = next.report().clone();
        self.loader = Some(next);
        report
    }
}

/// The daemon's load request: Slack and Telegram, every listed project,
/// under `home` (`None` denies all, `HomeUnknown`).
pub fn daemon_request(home: Option<PathBuf>) -> LoadRequest {
    let host_path = home
        .as_deref()
        .map(|h| trusty_common::crate_config::crate_config_path_at(h, "trusty-mpm"))
        .unwrap_or_default();
    LoadRequest {
        host_path,
        home,
        project: None,
        channels: DAEMON_CHANNELS.to_vec(),
    }
}

/// What a check reads. #9777: locked for a few instructions at a time,
/// never across a policy load or git.
#[derive(Debug, Default)]
struct Published {
    /// Clock time the last refresh attempt finished, good or failed.
    attempted: Option<Duration>,
    /// True from the check that starts a refresh until that refresh ends.
    in_flight: bool,
    /// The report of the last refresh that returned.
    report: Option<Arc<LoadReport>>,
    /// Clock time the last refresh that returned with every refusal a
    /// content verdict started.
    fresh_at: Option<Duration>,
}

impl Published {
    /// True when a check at `now` must start a refresh: none is in flight,
    /// and none has finished in the last [`REFRESH_INTERVAL`].
    fn due(&self, now: Duration) -> bool {
        !self.in_flight
            && self
                .attempted
                .is_none_or(|at| now.saturating_sub(at) >= REFRESH_INTERVAL)
    }

    /// The report a check at `now` may serve, or why none may be served.
    fn judge(&self, now: Duration) -> Result<Arc<LoadReport>, SnapshotDeny> {
        let (Some(report), Some(fresh_at)) = (&self.report, self.fresh_at) else {
            return Err(SnapshotDeny::NeverLoaded);
        };
        let age = now.saturating_sub(fresh_at);
        if age > MAX_AGE {
            return Err(SnapshotDeny::Stale { age, max: MAX_AGE });
        }
        if report.denied {
            return Err(SnapshotDeny::Denied);
        }
        Ok(Arc::clone(report))
    }
}

/// The state a snapshot shares with its refresh thread.
#[derive(Debug)]
struct Inner<S, C> {
    clock: C,
    /// Locked only by the one refresh in flight, never by a check.
    source: Mutex<S>,
    published: Mutex<Published>,
    /// Notified each time a refresh ends.
    idle: Condvar,
}

/// A route policy that refreshes at most every [`REFRESH_INTERVAL`] and
/// denies everything once it is older than [`MAX_AGE`].
///
/// Why: see the module doc. Held by the daemon in `DaemonState`.
/// What: every check calls [`Self::current`], which never waits on a
/// refresh (#9777): a due check starts one refresh on its own thread and is
/// judged on the report already published. A refresh that panics, or whose
/// report keeps a project's last-good routes for any reason but a content
/// verdict, does not advance the fresh time; the last report is served
/// until it is older than [`MAX_AGE`], then every check denies.
/// Test: `refresh_within_interval_spawns_no_git`,
/// `snapshot_older_than_max_age_denies`,
/// `denied_report_denies_egress_and_inbound`,
/// `refresh_error_keeps_last_good_only_until_max_age`,
/// `poisoned_snapshot_lock_denies`, `reader_not_blocked_by_slow_refresh`.
#[derive(Debug)]
pub struct PolicySnapshot<S, C = MonotonicClock> {
    inner: Arc<Inner<S, C>>,
}

impl DaemonPolicySnapshot {
    /// The daemon's snapshot over `config.yaml` under `home`.
    pub fn for_home(home: Option<PathBuf>) -> Self {
        Self::new(
            LoaderSource::new(daemon_request(home)),
            MonotonicClock::new(),
        )
    }
}

impl<S, C> PolicySnapshot<S, C>
where
    S: PolicySource + 'static,
    C: Clock + Send + Sync + 'static,
{
    /// A snapshot over `source`; nothing loads until the first check.
    pub fn new(source: S, clock: C) -> Self {
        let inner = Inner {
            clock,
            source: Mutex::new(source),
            published: Mutex::new(Published::default()),
            idle: Condvar::new(),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// The report in effect, or why nothing may be served.
    ///
    /// Why: the one gate every check passes; a caller that reads the report
    /// directly (doctor, a future route) gets the same fail-closed answer.
    /// It must never wait on git (#9777).
    /// What: when no refresh is in flight and none finished in the last
    /// [`REFRESH_INTERVAL`], starts one on its own thread. Returns the
    /// published report unless the lock is poisoned, no refresh has
    /// returned, the last fresh refresh started more than [`MAX_AGE`] ago,
    /// or the report is denied. A cold snapshot denies `NeverLoaded` while
    /// its first load runs.
    /// Test: `reader_not_blocked_by_slow_refresh`,
    /// `slow_refresh_does_not_start_a_second_refresh`,
    /// `hung_refresh_past_max_age_denies_without_blocking`,
    /// `never_loaded_denies_while_first_refresh_in_flight`,
    /// `snapshot_older_than_max_age_denies`,
    /// `denied_report_denies_egress_and_inbound`,
    /// `poisoned_snapshot_lock_denies`.
    pub fn current(&self) -> Result<Arc<LoadReport>, SnapshotDeny> {
        // #8454: a poisoned lock denies; nothing vouches for its state.
        let mut published = self
            .inner
            .published
            .lock()
            .map_err(|_| SnapshotDeny::Poisoned)?;
        // #8454 r-078 H1: judge age at read time, after the lock wait.
        let now = self.inner.clock.now();
        // #9777: the due check and the in-flight claim share one lock hold,
        // so at most one refresh runs.
        let start = published.due(now);
        if start {
            published.in_flight = true;
        }
        let verdict = published.judge(now);
        drop(published);
        if start {
            self.spawn_refresh(now);
        }
        verdict
    }

    /// Run one refresh started at `started` on its own thread.
    ///
    /// Why: #9777, a check must not wait on the policy load or git.
    /// What: the [`Flight`] publishes the outcome and clears the in-flight
    /// flag when dropped: after the refresh, on any panic, and when the
    /// spawn fails, because a failed spawn drops the closure that owns it.
    /// Test: `refresh_panic_clears_in_flight_and_keeps_report`,
    /// `concurrent_due_checks_start_one_refresh`.
    fn spawn_refresh(&self, started: Duration) {
        let mut flight = Flight {
            inner: Arc::clone(&self.inner),
            started,
            outcome: None,
        };
        let spawned = std::thread::Builder::new()
            .name("channel-policy-refresh".into())
            .spawn(move || flight.run());
        if let Err(error) = spawned {
            // #9777 Q4: a failed attempt; `fresh_at` does not move.
            tracing::error!(%error, "channel policy refresh thread did not start");
        }
    }

    /// May a `kind` message go to `recipient` on `channel`?
    ///
    /// Why: every daemon send passes this before any network call.
    /// What: [`Self::current`], then `ChannelPolicy::check_egress`; the
    /// allowing route is returned owned.
    /// Test: `denied_report_denies_egress_and_inbound`,
    /// `empty_policy_with_enabled_channel_denies`.
    pub fn check_egress(
        &self,
        channel: Channel,
        recipient: &str,
        kind: MessageKind,
    ) -> Result<Route, Refusal<DenyReason>> {
        let report = self.current().map_err(Refusal::Snapshot)?;
        match report.policy.check_egress(channel, recipient, kind) {
            EgressDecision::Allow(route) => Ok(route.clone()),
            EgressDecision::Deny(reason) => Err(Refusal::Policy(reason)),
        }
    }

    /// May this inbound message answer `binding`'s question?
    ///
    /// Why: every inbound answer passes this before it binds to a question.
    /// What: [`Self::current`], then `ChannelPolicy::check_inbound`.
    /// Test: `denied_report_denies_egress_and_inbound`,
    /// `empty_policy_with_enabled_channel_denies`.
    pub fn check_inbound(
        &self,
        channel: Channel,
        sender: &str,
        binding: &[QuestionId],
        questions: &[QuestionState],
    ) -> Result<(Route, QuestionId), Refusal<DropReason>> {
        let report = self.current().map_err(Refusal::Snapshot)?;
        match report
            .policy
            .check_inbound(channel, sender, binding, questions)
        {
            InboundDecision::Answer { route, question } => Ok((route.clone(), question)),
            InboundDecision::Drop(reason) => Err(Refusal::Policy(reason)),
        }
    }
}

impl<S: PolicySource, C> Inner<S, C> {
    /// One source refresh, outside every lock a check takes: the report and
    /// whether it is fresh, or `None` when the refresh panicked.
    fn load(&self) -> Option<(Arc<LoadReport>, bool)> {
        // #9777: the guard lives only inside the catch, so a refresh panic
        // never poisons the lock; a poisoned one fails the refresh.
        let caught = {
            let Ok(mut source) = self.source.lock() else {
                tracing::error!("channel policy source lock is poisoned; refresh failed");
                return None;
            };
            catch_unwind(AssertUnwindSafe(|| source.refresh()))
        };
        let report = match caught {
            Ok(report) => report,
            Err(_) => {
                // #8454: a panic counts toward max age; the last report stays.
                tracing::error!("channel policy refresh panicked; last report kept until max age");
                return None;
            }
        };
        let fresh = !unverified(&report);
        if !fresh {
            // #8454 r-078 H2: last-good routes kept over a fault count toward max age.
            tracing::error!("channel policy refresh kept last-good routes over a fault; not fresh");
        }
        Some((Arc::new(report), fresh))
    }
}

/// One refresh in flight. Dropping it ends the flight.
///
/// Why: #9777, the in-flight flag must clear on every path, or no refresh
/// ever starts again and the snapshot ages to deny for good.
/// What: on drop, publishes `outcome` (only a fresh one moves `fresh_at`,
/// to the start time), stamps `attempted` with the finish time (#9777 Q2),
/// clears the flag and notifies `idle`.
/// Test: `refresh_panic_clears_in_flight_and_keeps_report`,
/// `slow_refresh_does_not_start_a_second_refresh`,
/// `hung_refresh_past_max_age_denies_without_blocking`.
struct Flight<S, C: Clock> {
    inner: Arc<Inner<S, C>>,
    started: Duration,
    outcome: Option<(Arc<LoadReport>, bool)>,
}

impl<S: PolicySource, C: Clock> Flight<S, C> {
    /// Run the refresh; the outcome is published when `self` drops.
    fn run(&mut self) {
        self.outcome = self.inner.load();
    }
}

impl<S, C: Clock> Drop for Flight<S, C> {
    fn drop(&mut self) {
        let finished = self.inner.clock.now();
        // A poisoned lock already denies every check (`Poisoned`).
        if let Ok(mut published) = self.inner.published.lock() {
            if let Some((report, fresh)) = self.outcome.take() {
                if fresh {
                    published.fresh_at = Some(self.started);
                }
                published.report = Some(report);
            }
            published.attempted = Some(finished);
            published.in_flight = false;
        }
        self.inner.idle.notify_all();
    }
}

/// True when a project file keeps its last-good routes (`Stale`) for any
/// reason but a known content verdict.
///
/// Why: trusty-channels keeps last-good routes over any reload refusal with
/// no time limit (Architect Q3). An unreviewed edit or a bad file is a
/// verdict and stays fresh. Any other refusal ages (#8433 P1): a read or git
/// fault, including the git faults the gate names `NotARepository`,
/// `NotTopLevel`, `NoDefaultCommit` or `NotCommitted`, and any variant added
/// later (r-078 H2).
/// What: a `Stale` file with no refusal finding, or with one refusal that
/// [`is_verdict`] does not accept.
/// Test: `refresh_error_keeps_last_good_only_until_max_age`,
/// `git_fault_reported_as_a_verdict_ages_to_deny`,
/// `routes_edit_outside_config_yaml_has_no_effect_in_daemon`.
fn unverified(report: &LoadReport) -> bool {
    report
        .per_file
        .iter()
        .filter(|s| s.state == FileState::Stale)
        .any(|s| {
            let refusals: Vec<bool> = report
                .findings
                .iter()
                .filter_map(|f| refusal_is_verdict(f, &s.file))
                .collect();
            refusals.is_empty() || refusals.contains(&false)
        })
}

/// `Some(true)` when `f` refuses `file` with a content verdict,
/// `Some(false)` when it refuses `file` for another reason, `None` when it
/// does not refuse `file`. The route-level findings are rules over the
/// parsed file and the host ceiling, so they are verdicts.
fn refusal_is_verdict(f: &Finding, file: &Path) -> Option<bool> {
    match f {
        Finding::FileRefused { file: of, error } => (of == file).then(|| is_verdict(error)),
        Finding::RouteRejected { origin, .. } | Finding::Widening { origin, .. } => {
            (origin.file == file).then_some(true)
        }
        Finding::Overlap { first, .. } => (first.file == file).then_some(true),
        Finding::NoGchatConnection { file: of } => (of == file).then_some(true),
        _ => None,
    }
}

/// True for a refusal that is a verdict on reviewed content or repo state.
///
/// `NotCommitted` is not one: `rev-parse --verify` fails the same way for a
/// missing path and for a missing tree object.
fn is_verdict(error: &ProjectFileError) -> bool {
    match error {
        ProjectFileError::Parse { .. }
        | ProjectFileError::Version { .. }
        | ProjectFileError::Invalid { .. }
        | ProjectFileError::NotUtf8
        | ProjectFileError::TooLarge { .. }
        | ProjectFileError::NotRegular { .. } => true,
        ProjectFileError::Gate { error } => matches!(
            error,
            GateError::ContentDiffers { .. }
                | GateError::NotOnDefaultBranch { .. }
                | GateError::DetachedHead
        ),
        // #8454 r-078 H2: every other refusal, and any later variant, ages.
        _ => false,
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
