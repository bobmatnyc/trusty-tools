//! `POST /api/v1/channels/egress-verdict`: the egress hook's policy
//! readiness verdict, from the daemon's policy snapshot (#8454 VC, #8433).
//!
//! Why: `tm hook --egress-guard` checks policy readiness for every gated
//! call of a channel-tainted session, and its inline load costs ~1.5 s and
//! several git spawns per project. The daemon already holds the policy in
//! its snapshot (P1). This is a Fail-Open Check site: no arm here may answer
//! `ready` for a report the inline check would refuse.
//! What: [`router`] registers [`VERDICT_ROUTE`]; [`answer`] is the whole
//! decision. A verdict is served only for the daemon's own home, from a
//! snapshot within max age, for a loader-stamped fingerprint (never 0), and
//! is always judged on the report the snapshot publishes at that query. Any
//! other case is `Unserved`, and the hook runs its inline load.
//! Test: `egress_verdict_tests.rs`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::{Json, Router, extract::State, routing::post};
use trusty_channels::policy::LoadReport;

use crate::channels::readiness::classify;
use crate::channels::snapshot::{MAX_AGE, SnapshotDeny};
use crate::core::channels_readiness::{NOT_READY_CHECK, NotReady};
use crate::core::egress_verdict::{Unserved, VERDICT_ROUTE, VerdictReply, VerdictRequest};
use crate::daemon::state::DaemonState;

/// The egress verdict sub-router, merged into the daemon router by `api.rs`.
///
/// What: each query reads the daemon's policy snapshot and [`answer`]s it.
/// Test: `route_answers_a_denied_daemon_policy_as_not_ready`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new().route(
        VERDICT_ROUTE,
        post(
            |State(state): State<Arc<DaemonState>>, Json(req): Json<VerdictRequest>| async move {
                let home = state.user_home();
                let read = || state.channel_policy().current_aged();
                Json(answer(read, home.as_deref(), &req.home))
            },
        ),
    )
}

/// The verdict for a hook that asked under `asked`.
///
/// Why: see the module doc. The inline check is the reference: a served
/// verdict is the inline check's verdict on the same report. The snapshot
/// is the cache; a verdict stored apart from it could outlive the report it
/// was judged on (#8454: a report stamped with a fingerprint read before its
/// files, and sources that return to that fingerprint).
/// What: in order, `asked` not the daemon's home, a snapshot refusal
/// (`read` runs only after the home matched), an age past [`MAX_AGE`] and a
/// fingerprint of 0 are each `Unserved`. Otherwise the published report is
/// [`classify`]d at this query and served. A denied report is never ready.
/// Test: `cached_verdict_reused_while_fingerprint_unchanged`,
/// `fingerprint_change_invalidates_cached_verdict`,
/// `verdict_is_judged_on_the_published_report_only`,
/// `snapshot_older_than_max_age_is_not_served`,
/// `denied_policy_report_is_cached_as_deny_not_allow`,
/// `zero_fingerprint_is_never_served`, `home_mismatch_is_not_served`.
pub fn answer(
    read: impl FnOnce() -> Result<(Arc<LoadReport>, Duration), SnapshotDeny>,
    daemon_home: Option<&Path>,
    asked: &Path,
) -> VerdictReply {
    let unserved = |why| VerdictReply::Unserved { why };
    if daemon_home != Some(asked) {
        return unserved(Unserved::HomeMismatch);
    }
    let (report, age) = match read() {
        Ok(read) => read,
        Err(SnapshotDeny::NeverLoaded) => return unserved(Unserved::NeverLoaded),
        Err(SnapshotDeny::Stale { .. }) => return unserved(Unserved::Stale),
        Err(SnapshotDeny::Poisoned) => return unserved(Unserved::Poisoned),
        // `current_aged` returns a denied report; any other refusal serves nothing.
        Err(SnapshotDeny::Denied) => return unserved(Unserved::Refused),
    };
    // #8454 VC: the snapshot judged the age already; checked again here.
    if age > MAX_AGE {
        return unserved(Unserved::Stale);
    }
    if report.fingerprint == 0 {
        return unserved(Unserved::NoFingerprint);
    }
    // #8454: judged on the published report at every query, never stored.
    served(&judge(&report), report.fingerprint, age)
}

/// [`classify`], with a denied report never read as ready.
fn judge(report: &LoadReport) -> Result<(), NotReady> {
    match classify(report) {
        // `classify` refuses a denied report first; this keeps it so here.
        Ok(()) if report.denied => Err(NotReady::Denied {
            finding: "denied report".into(),
        }),
        verdict => verdict,
    }
}

/// The reply serving `verdict` for `fingerprint` at `age`.
fn served(verdict: &Result<(), NotReady>, fingerprint: u64, age: Duration) -> VerdictReply {
    let age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
    match verdict {
        Ok(()) => VerdictReply::Ready {
            fingerprint,
            age_ms,
        },
        Err(not_ready) => VerdictReply::NotReady {
            slug: NOT_READY_CHECK.to_owned(),
            not_ready: not_ready.clone(),
            fingerprint,
            age_ms,
        },
    }
}

#[cfg(test)]
#[path = "egress_verdict_tests.rs"]
mod tests;
