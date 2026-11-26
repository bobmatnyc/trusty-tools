//! A time bound for a doctor phase, with a timeout row in place of a hang (#11885).
//!
//! Why: `tm doctor` printed nothing for over two minutes on a host with many
//! worktrees. The worktree inventory and the machine-wide settings walk scale
//! with the host and had no bound, so the operator could not tell a slow check
//! from a hung command.
//! What: [`within`] awaits a phase for at most a limit and, when the limit is
//! hit, answers the [`timeout_row`] that names the check. The row is `Unknown`:
//! a probe that ran out of time learned nothing (#4005), so it is never a pass.
//! Test: `core::doctor_bounded::tests`.

use std::future::Future;
use std::time::Duration;

use super::doctor::{CheckStatus, DoctorCheck};

/// Await `phase` for at most `limit`.
///
/// Why (#11885): one slow phase must cost its own row, not the whole report.
/// What: `Ok` with the phase's output when it finishes in time; otherwise
/// `Err` with the [`timeout_row`] for `check`. The phase future is dropped on
/// expiry; work it already handed to the blocking pool keeps running there.
/// Test: `a_phase_that_never_finishes_yields_a_timeout_row`,
/// `a_phase_that_finishes_in_time_yields_its_output`.
pub async fn within<T>(
    check: &str,
    limit: Duration,
    phase: impl Future<Output = T>,
) -> Result<T, DoctorCheck> {
    tokio::time::timeout(limit, phase)
        .await
        .map_err(|_| timeout_row(check, limit))
}

/// The row a phase that ran out of time reports.
///
/// What: `Unknown`, naming `check` and the limit in whole seconds.
/// Test: `a_phase_that_never_finishes_yields_a_timeout_row`.
pub fn timeout_row(check: &str, limit: Duration) -> DoctorCheck {
    DoctorCheck::new(
        check,
        CheckStatus::Unknown,
        format!(
            "timed out: `{check}` did not finish within {}s, so its state is not known \
             (#11885); the other checks still ran",
            limit.as_secs()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phase that never finishes ends at the limit with a row naming it.
    ///
    /// The outer guard is what turns a hang into a failure: with no bound in
    /// `within`, the pending phase holds the call until the guard fires.
    #[tokio::test]
    async fn a_phase_that_never_finishes_yields_a_timeout_row() {
        let limit = Duration::from_millis(50);
        let bounded = within("worktrees", limit, std::future::pending::<()>());
        let outcome = tokio::time::timeout(Duration::from_secs(10), bounded)
            .await
            .expect("`within` did not return at its limit: the phase is unbounded");

        let row = outcome.expect_err("a pending phase cannot finish");
        assert_eq!(row.name, "worktrees");
        assert_eq!(row.status, CheckStatus::Unknown);
        assert!(row.message.contains("timed out"), "{}", row.message);
        assert!(row.message.contains("`worktrees`"), "{}", row.message);
    }

    /// A phase that finishes in time passes its output through unchanged.
    #[tokio::test]
    async fn a_phase_that_finishes_in_time_yields_its_output() {
        let outcome = within("worktrees", Duration::from_secs(10), async { 7 }).await;
        assert_eq!(outcome.ok(), Some(7));
    }
}
