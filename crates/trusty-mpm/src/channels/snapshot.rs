//! The daemon's route-policy snapshot (#8433 P1, #8454).
//!
//! Why: the daemon enforces chat-channel routes on every send and every
//! inbound answer. Reloading the policy per call runs git per project, and a
//! policy the daemon cannot re-verify must not stay in force for ever (#8454
//! S2c Q4: 5 s refresh, 60 s max age). This is a Fail-Open Check site: no
//! failure here may serve an old policy past max age or allow by default.
//! What: [`PolicySnapshot`] holds a [`PolicySource`] (in the daemon, a
//! trusty-channels `PolicyLoader` in [`LoaderSource`]) behind a lock. A check
//! refreshes the source when [`REFRESH_INTERVAL`] has passed since the last
//! attempt, then serves the report only while the last successful refresh is
//! at most [`MAX_AGE`] old and the report is not `denied`.
//! Test: `snapshot_tests.rs`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
/// Why: `PolicyLoader::refresh` records its new fingerprint before it
/// reloads, so a panic mid-reload would leave a loader that never reloads
/// that change again.
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

#[derive(Debug)]
struct State<S> {
    source: S,
    /// Clock time of the last refresh attempt, good or failed.
    attempted: Option<Duration>,
    /// The report of the last refresh that returned.
    report: Option<Arc<LoadReport>>,
    /// Clock time of the last refresh that returned with no I/O fault.
    fresh_at: Option<Duration>,
}

/// A route policy that refreshes at most every [`REFRESH_INTERVAL`] and
/// denies everything once it is older than [`MAX_AGE`].
///
/// Why: see the module doc. Held by the daemon in `DaemonState`.
/// What: every check calls [`Self::current`]. A refresh that panics, or
/// whose report keeps a project's last-good routes because the file or git
/// could not be read, does not advance the fresh time; the last report is
/// served until it is older than [`MAX_AGE`], then every check denies. The
/// lock is held across a refresh, so a check may block on git; call it from
/// a blocking context.
/// Test: `refresh_within_interval_spawns_no_git`,
/// `snapshot_older_than_max_age_denies`,
/// `denied_report_denies_egress_and_inbound`,
/// `refresh_error_keeps_last_good_only_until_max_age`,
/// `poisoned_snapshot_lock_denies`.
#[derive(Debug)]
pub struct PolicySnapshot<S, C = MonotonicClock> {
    clock: C,
    state: Mutex<State<S>>,
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

impl<S: PolicySource, C: Clock> PolicySnapshot<S, C> {
    /// A snapshot over `source`; nothing loads until the first check.
    pub fn new(source: S, clock: C) -> Self {
        Self {
            clock,
            state: Mutex::new(State {
                source,
                attempted: None,
                report: None,
                fresh_at: None,
            }),
        }
    }

    /// The report in effect, or why nothing may be served.
    ///
    /// Why: the one gate every check passes; a caller that reads the report
    /// directly (doctor, a future route) gets the same fail-closed answer.
    /// What: refreshes when [`REFRESH_INTERVAL`] has passed since the last
    /// attempt, then returns the report unless the lock is poisoned, no
    /// refresh has returned, the last fresh refresh is older than
    /// [`MAX_AGE`], or the report is denied.
    /// Test: `snapshot_older_than_max_age_denies`,
    /// `denied_report_denies_egress_and_inbound`,
    /// `refresh_error_keeps_last_good_only_until_max_age`,
    /// `poisoned_snapshot_lock_denies`.
    pub fn current(&self) -> Result<Arc<LoadReport>, SnapshotDeny> {
        let now = self.clock.now();
        // #8454: a poisoned lock denies; nothing vouches for its state.
        let mut state = self.state.lock().map_err(|_| SnapshotDeny::Poisoned)?;
        let due = state
            .attempted
            .is_none_or(|at| now.saturating_sub(at) >= REFRESH_INTERVAL);
        if due {
            state.refresh(now);
        }
        let (Some(report), Some(fresh_at)) = (&state.report, state.fresh_at) else {
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

impl<S: PolicySource> State<S> {
    /// One refresh attempt at `now`; only a clean one moves `fresh_at`.
    fn refresh(&mut self, now: Duration) {
        self.attempted = Some(now);
        let report = match catch_unwind(AssertUnwindSafe(|| self.source.refresh())) {
            Ok(report) => report,
            Err(_) => {
                // #8454: a panic counts toward max age; the last report stays.
                tracing::error!("channel policy refresh panicked; last report kept until max age");
                return;
            }
        };
        if io_faulted(&report) {
            // #8454: last-good routes kept over an I/O fault count toward max age.
            tracing::error!("channel policy refresh hit an I/O fault; not fresh");
        } else {
            self.fresh_at = Some(now);
        }
        self.report = Some(Arc::new(report));
    }
}

/// True when a project file keeps its last-good routes (`Stale`) because it
/// or its git state could not be read.
///
/// Why: trusty-channels keeps last-good routes over any reload fault with no
/// time limit (Architect Q3). An unreviewed edit is a verdict and stays
/// fresh; a read or git fault is not a verdict, so it ages (#8433 P1).
/// What: a `Stale` file whose `FileRefused` finding is a read error or a
/// git step that could not run, failed or timed out.
/// Test: `refresh_error_keeps_last_good_only_until_max_age`,
/// `routes_edit_outside_config_yaml_has_no_effect_in_daemon`.
fn io_faulted(report: &LoadReport) -> bool {
    let stale = |file: &PathBuf| {
        report
            .per_file
            .iter()
            .any(|s| s.state == FileState::Stale && s.file == *file)
    };
    report.findings.iter().any(|f| match f {
        Finding::FileRefused { file, error } => stale(file) && is_io(error),
        _ => false,
    })
}

fn is_io(error: &ProjectFileError) -> bool {
    matches!(
        error,
        ProjectFileError::Read { .. }
            | ProjectFileError::Gate {
                error: GateError::GitUnavailable { .. }
                    | GateError::GitFailed { .. }
                    | GateError::GitTimedOut { .. }
            }
    )
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
