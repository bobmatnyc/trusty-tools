//! The `tm doctor` `gh_account` check — which github.com identity `gh` will use.
//!
//! Why: `tm` shells to `gh` for PR merges, issue edits, and managed spawn, and
//! the identity it picks is invisible until something fails on permissions. This
//! check surfaces it, and flags the multi-account ambiguity that once made
//! `gh pr merge --admin` fail with "1 approving review required".
//! What: [`check_gh_account`] runs one bounded `gh auth status` off the async
//! executor; [`build_gh_account_check`] folds the outcome into a
//! [`DoctorCheck`].
//! Test: `doctor_gh_account_tests.rs` covers every branch of the fold.
//!
//! Split out of `doctor.rs` by #5032 — the tri-state probe pushed that file
//! past the 500-SLOC production cap.

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::gh_account::{GhAccountStatus, GhAuthProbe};

/// Probe the active `gh` github.com account and warn on ambiguity.
///
/// Why: see the module header — the active identity is otherwise invisible.
/// What: off-loads ONE bounded `gh auth status` to `spawn_blocking` (so the
/// async executor is not stalled), then folds the result via
/// [`build_gh_account_check`]. Advisory only: `Warn` when multiple accounts are
/// logged in or `gh` is unauthenticated, `Ok` for a single clear account — never
/// a hard `Fail`, since a missing/other `gh` identity is not a broken stack.
/// Test: `build_gh_account_check` covers every branch;
/// `gh_account_check_is_advisory_only` asserts it never returns `Fail`.
pub(super) async fn check_gh_account() -> DoctorCheck {
    // #5032: `gh auth status` is the only source that sees env-token auth, and
    // doctor is not latency-sensitive — so probe it under the doctor's own bound
    // instead of the statusline-era 250 ms one that timed out on every token.
    let probe = tokio::task::spawn_blocking(|| {
        crate::core::gh_account::probe_gh_auth(crate::core::gh_account::GH_DOCTOR_TIMEOUT)
    })
    .await
    .unwrap_or_else(|e| GhAuthProbe::Inconclusive(format!("gh probe task failed: {e}")));
    build_gh_account_check(probe)
}

/// Fold a [`GhAuthProbe`] into a [`DoctorCheck`] (pure).
///
/// Why: keeping the verdict logic pure makes every branch — single account,
/// multi-account ambiguity, active-unknown, unauthenticated, and inconclusive —
/// unit-testable without a live `gh`.
/// What: an [`GhAuthProbe::Inconclusive`] probe reports the state as UNKNOWN and
/// never as a negative (#5032 — a timed-out probe used to render as "not
/// authenticated"). For an answered probe: `Warn` when it holds more than one
/// login (naming them and pointing at `gh auth switch`), `Warn` when no account
/// is present, `Warn` when accounts exist but none is marked active, and `Ok`
/// for a single clear active account. Never `Fail` — this is advisory.
/// Test: `build_gh_account_check_single_ok`, `build_gh_account_check_multi_warn`,
/// `build_gh_account_check_unauthenticated_warn`,
/// `build_gh_account_check_inconclusive_is_not_unauthenticated`,
/// `gh_account_check_is_advisory_only`.
pub(super) fn build_gh_account_check(probe: GhAuthProbe) -> DoctorCheck {
    let GhAccountStatus {
        active,
        logged_in: accounts,
    } = match probe {
        GhAuthProbe::Answered(status) => status,
        // #5032: an unfinished probe is not a negative answer — say so.
        GhAuthProbe::Inconclusive(reason) => {
            return DoctorCheck::new(
                "gh_account",
                CheckStatus::Warn,
                format!(
                    "could not determine the gh account — auth state UNKNOWN, \
                     not necessarily unauthenticated ({reason}). Run `gh auth status` \
                     by hand to see the real state."
                ),
            );
        }
    };
    if accounts.len() > 1 {
        let list = accounts.join(", ");
        let active_str = active.as_deref().unwrap_or("unknown");
        return DoctorCheck::new(
            "gh_account",
            CheckStatus::Warn,
            format!(
                "{} github.com accounts logged in ({list}); active is `{active_str}`. \
                 `gh` (and `gh pr merge --admin`) uses the ACTIVE account — if merges \
                 fail on permissions, run `gh auth switch` to the repo owner.",
                accounts.len()
            ),
        );
    }
    match active {
        Some(login) => DoctorCheck::new(
            "gh_account",
            CheckStatus::Ok,
            format!("active gh account: `{login}`"),
        ),
        None if accounts.is_empty() => DoctorCheck::new(
            "gh_account",
            CheckStatus::Warn,
            "gh is not authenticated (no github.com account) — `gh` calls will fail; \
             run `gh auth login`"
                .to_string(),
        ),
        None => DoctorCheck::new(
            "gh_account",
            CheckStatus::Warn,
            format!(
                "gh authenticated ({}) but no active account is set — run `gh auth switch`",
                accounts.join(", ")
            ),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_gh_account_tests.rs"]
mod tests;
