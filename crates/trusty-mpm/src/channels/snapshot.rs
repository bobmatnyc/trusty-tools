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
use std::path::{Path, PathBuf};
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
    /// Clock time of the last refresh that returned with every refusal a
    /// content verdict.
    fresh_at: Option<Duration>,
}

/// A route policy that refreshes at most every [`REFRESH_INTERVAL`] and
/// denies everything once it is older than [`MAX_AGE`].
///
/// Why: see the module doc. Held by the daemon in `DaemonState`.
/// What: every check calls [`Self::current`]. A refresh that panics, or
/// whose report keeps a project's last-good routes for any reason but a
/// content verdict, does not advance the fresh time; the last report is
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
        // #8454: a poisoned lock denies; nothing vouches for its state.
        let mut state = self.state.lock().map_err(|_| SnapshotDeny::Poisoned)?;
        let started = self.clock.now();
        let due = state
            .attempted
            .is_none_or(|at| started.saturating_sub(at) >= REFRESH_INTERVAL);
        if due {
            state.refresh(started);
        }
        // #8454 r-078 H1: judge age after the lock wait and the refresh.
        let now = self.clock.now();
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
        if unverified(&report) {
            // #8454 r-078 H2: last-good routes kept over a fault count toward max age.
            tracing::error!("channel policy refresh kept last-good routes over a fault; not fresh");
        } else {
            self.fresh_at = Some(now);
        }
        self.report = Some(Arc::new(report));
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
