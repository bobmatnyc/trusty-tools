//! The inline policy load behind the egress guard's readiness gate (#9741,
//! #8454 G3 S4).
//!
//! Why: the hook runs once per tool call and holds no snapshot, so it loads
//! the daemon's view of the policy itself, under a deadline, and fails closed
//! on anything but a clean, non-empty load. This is a Fail-Open Check site:
//! every arm below that is not `Ok(())` must stay a refusal.
//! What: [`classify`] judges a `LoadReport`; [`check_with`] runs a load on a
//! detached thread and waits for it; [`check`] is the production load over
//! [`daemon_request`].
//! Test: `bin/tm/commands/egress_guard_policy_tests.rs`.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use trusty_channels::policy::{
    FileState, Finding, FindingScope, HostError, LoadReport, LoadRequest, load_effective_until,
};

use super::snapshot::daemon_request;
use crate::core::channels_readiness::{HostBranch, NotReady};

/// How long before the wait ends the load's git is killed, so no git process
/// outlives a not-ready verdict (#8454); halved for a shorter wait.
pub const KILL_MARGIN: Duration = Duration::from_millis(500);

/// Is `report` a ready policy?
///
/// Why (S4 plan decision 3): any doubt about the policy is not ready.
/// What: in order, a denied load (its host fault when it has one), any
/// finding, any project file not `Effective`, and an empty policy are each
/// [`NotReady`]; anything else is ready.
/// Test: `config_missing_denies_a_gated_call`, `denied_report_denies`,
/// `finding_denies_a_gated_call`, `refused_project_file_denies`,
/// `file_not_in_effect_denies_a_gated_call`,
/// `empty_policy_with_enabled_channel_denies`,
/// `ready_policy_falls_through_to_taint_deny`.
pub fn classify(report: &LoadReport) -> Result<(), NotReady> {
    if report.denied {
        let host = report.findings.iter().find_map(|f| match f {
            Finding::HostRefused { error } => Some(error),
            _ => None,
        });
        if let Some(error) = host {
            return Err(NotReady::Host {
                branch: host_branch(error),
                error: variant_name(error),
            });
        }
        let finding = report
            .findings
            .iter()
            .find(|f| f.scope() == FindingScope::DenyAll)
            .map_or_else(|| "no finding names it".into(), variant_name);
        return Err(NotReady::Denied { finding });
    }
    if let Some(first) = report.findings.first() {
        return Err(NotReady::Findings {
            count: report.findings.len(),
            first: variant_name(first),
        });
    }
    // #9741: only `Effective` counts; `Stale` cannot come from a fresh load.
    if let Some(file) = report
        .per_file
        .iter()
        .find(|s| !matches!(s.state, FileState::Effective { .. }))
    {
        return Err(NotReady::File {
            state: variant_name(&file.state),
        });
    }
    if report.policy.routes().is_empty() {
        return Err(NotReady::NoRoutes);
    }
    Ok(())
}

/// The S4 plan branch a host fault belongs to.
fn host_branch(error: &HostError) -> HostBranch {
    match error {
        HostError::HomeUnknown => HostBranch::HomeUnknown,
        HostError::Missing => HostBranch::Missing,
        HostError::NoChannelsSection => HostBranch::NoChannels,
        HostError::NotRegular { .. }
        | HostError::NotUnderHome { .. }
        | HostError::Read { .. }
        | HostError::TooLarge { .. }
        | HostError::NotUtf8 => HostBranch::Unreadable,
        HostError::Malformed { .. } | HostError::Invalid { .. } | HostError::Version { .. } => {
            HostBranch::Invalid
        }
        // `HostError` is non_exhaustive: an unknown fault is a refusal.
        _ => HostBranch::Refused,
    }
}

/// The variant name of `value`'s `Debug` text, without any field value.
fn variant_name(value: &impl std::fmt::Debug) -> String {
    let text = format!("{value:?}");
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Run `loader` on a detached thread and judge its report.
///
/// Why (B7): a git step can block, and the hook must answer inside Claude
/// Code's hook timeout. A detached std thread is never joined, so a stuck
/// load cannot hold the hook's exit; the deadline it gets ends its git first.
/// What: hands `loader` a deadline [`KILL_MARGIN`] before the wait ends (or
/// halfway through a shorter wait) and waits up to `limit`. Past the wait is
/// [`NotReady::TimedOut`], a load that panicked is [`NotReady::Stopped`], a
/// report is [`classify`]d. A thread that cannot start panics here, and the
/// egress guard's `deny_on_panic` denies it.
/// Test: `load_past_deadline_denies`, `load_panic_denies`,
/// `denied_report_denies`.
pub fn check_with<L>(loader: L, req: LoadRequest, limit: Duration) -> Result<(), NotReady>
where
    L: FnOnce(&LoadRequest, Instant) -> LoadReport + Send + 'static,
{
    let deadline = Instant::now() + limit - KILL_MARGIN.min(limit / 2);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(loader(&req, deadline));
    });
    match rx.recv_timeout(limit) {
        Ok(report) => classify(&report),
        Err(RecvTimeoutError::Timeout) => Err(NotReady::TimedOut(limit)),
        // The sender dropped without a report: the load panicked.
        Err(RecvTimeoutError::Disconnected) => Err(NotReady::Stopped),
    }
}

/// The production check: the daemon's request (Slack and Telegram, every
/// listed project) under `home`, loaded with git bounded by the deadline.
///
/// Test: `config_missing_denies_a_gated_call`, `home_unknown_denies_a_gated_call`,
/// `ready_policy_falls_through_to_taint_deny`.
pub fn check(home: Option<&Path>, limit: Duration) -> Result<(), NotReady> {
    let req = daemon_request(home.map(Path::to_path_buf));
    check_with(load_effective_until, req, limit)
}
