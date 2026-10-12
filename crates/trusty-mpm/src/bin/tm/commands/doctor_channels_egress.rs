//! The `egress_policy_ready` row of `tm doctor` (#9741, #8454 G3 S4).
//!
//! Why: `tm hook --egress-guard` refuses a channel-tainted session's path tool
//! call as `egress-policy-not-ready` when the route policy is not ready. The
//! operator needs to see that state, and its branch, before a session hits it.
//! What: [`egress_ready_row`] judges the daemon view the `channels_routes` row
//! already loaded with the hook's own `classify`; it runs no load of its own.
//! Test: `egress_policy_ready_row_is_ok_only_when_ready`, and the
//! `channels_*` row tests that iterate every row name.

use trusty_channels::policy::LoadReport;
use trusty_mpm::channels::readiness::classify;
use trusty_mpm::core::channels_readiness::{HostBranch, NOT_READY_CHECK, NotReady};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

use crate::commands::doctor_channels::Outcome;

const EGRESS_READY: &str = "egress_policy_ready";

/// The `egress_policy_ready` row over the daemon view's load.
///
/// Why (Fail-Open Check): only a ready policy, or no channel configured at
/// all (the deny state by design), reads Ok.
/// What: Unknown for an unfinished load, no load, or no home; Ok for a ready
/// policy or a missing `config.yaml` / `channels:` section; Warn naming the
/// branch for any other not-ready state.
/// Test: `egress_policy_ready_row_is_ok_only_when_ready`,
/// `not_configured_reads_as_the_deny_state`, `home_unknown_is_unknown`,
/// `a_load_that_hangs_is_unknown_and_doctor_finishes`, `a_load_that_panics_is_unknown`.
pub(crate) fn egress_ready_row(daemon: &Outcome<Vec<LoadReport>>) -> DoctorCheck {
    let row = |status, message: String| DoctorCheck::new(EGRESS_READY, status, message);
    let report = match daemon {
        Err(u) => return row(CheckStatus::Unknown, format!("the daemon view load {u}")),
        Ok(reports) => match reports.first() {
            Some(report) => report,
            None => return row(CheckStatus::Unknown, "the daemon view ran no load".into()),
        },
    };
    match classify(report) {
        Ok(()) => row(
            CheckStatus::Ok,
            "ready: a channel-tainted session's path tool calls get the taint deny".into(),
        ),
        Err(not_ready) if not_ready.is_not_configured() => row(
            CheckStatus::Ok,
            format!(
                "not configured ({not_ready}); a channel-tainted session's path tool calls are \
                 denied as {NOT_READY_CHECK}"
            ),
        ),
        Err(
            not_ready @ NotReady::Host {
                branch: HostBranch::HomeUnknown,
                ..
            },
        ) => row(CheckStatus::Unknown, not_ready.to_string()),
        Err(not_ready) => row(
            CheckStatus::Warn,
            format!(
                "not ready: {not_ready}; a channel-tainted session's path tool calls are denied \
                 as {NOT_READY_CHECK} (#9741)"
            ),
        ),
    }
}
