//! Per-instance operation limits on [`AppState`] and their builders.
//!
//! Why: each limit is read from the environment once, at construction, and
//! stored on the state, so a test injects a short bound with a builder instead
//! of setting a process-wide env var that would race parallel tests. #11255
//! moved the two write-budget builders here from `lib.rs`, which sits at the
//! 500-SLOC cap, beside the new recall limits.
//! What: [`RecallLimits`], and the `with_write_op_budget`,
//! `with_write_pipeline_budget` and `with_recall_limits` builders.
//! Test: `tools::tests::write_budget_tests`; `recall_limit_defaults`;
//! `a_held_palace_open_answers_palace_opening_and_the_open_continues`;
//! `scope_all_returns_partial_results_at_its_deadline`.

use std::time::Duration;

use crate::AppState;
use crate::worker_liveness;

/// The time limits of the recall surfaces (#11255).
///
/// Why: a single-palace recall waited on a cold palace open with no bound of
/// its own, and a `scope: "all"` walk had no time budget at all; both ran past
/// the MCP bridge's 60 s.
/// What: `open_wait` bounds how long `memory_recall` / `memory_recall_deep`
/// wait for their palace to open (`TRUSTY_RECALL_OPEN_WAIT_SECS`, 15 s);
/// `all_deadline` bounds one `scope: "all"` walk
/// (`TRUSTY_RECALL_ALL_DEADLINE_SECS`, 20 s).
/// Test: `recall_limit_defaults`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallLimits {
    /// Longest wait on a single-palace recall's palace open.
    pub open_wait: Duration,
    /// Wall-clock budget of one `scope: "all"` recall_all walk.
    pub all_deadline: Duration,
}

/// The longest `scope: "all"` deadline honoured; a longer one is capped.
///
/// Why (#11255 review): `Instant::now() + deadline` panics on overflow, so an
/// absurd `TRUSTY_RECALL_ALL_DEADLINE_SECS` could crash the daemon.
pub const MAX_RECALL_ALL_DEADLINE: Duration = Duration::from_secs(3600);

impl RecallLimits {
    /// Read both limits from the environment (see [`crate::engine::timeouts`]).
    pub fn from_env() -> Self {
        use crate::engine::timeouts;
        Self::resolve(
            timeouts::recall_open_wait(),
            timeouts::recall_all_deadline(),
            timeouts::open_queue_timeout(),
        )
    }

    /// The effective limits for raw `open_wait` and `all_deadline` values.
    ///
    /// Why (#11255 review): a configured open wait at or past the open-queue
    /// bound answered the queue's generic timeout instead of `PalaceOpening`;
    /// the compile-time check covered only the defaults.
    /// What: `open_wait` is capped at three quarters of `open_queue`, so it
    /// is always below a non-zero queue bound (a zero bound gives a zero
    /// wait: a cold recall answers `PalaceOpening` at once). `all_deadline` is
    /// capped at [`MAX_RECALL_ALL_DEADLINE`].
    /// Test: `recall_limits_resolve_below_the_queue_bound_and_under_the_cap`.
    pub fn resolve(open_wait: Duration, all_deadline: Duration, open_queue: Duration) -> Self {
        Self {
            open_wait: open_wait.min(open_queue - open_queue / 4),
            all_deadline: all_deadline.min(MAX_RECALL_ALL_DEADLINE),
        }
    }
}

impl AppState {
    /// Override the per-operation write budget (issue #4002).
    ///
    /// Why: tests must prove the two write legs draw on ONE budget without
    /// setting `TRUSTY_WRITE_OP_BUDGET_SECS`, which is process-wide and would
    /// race any test running in parallel.
    /// What: consuming builder that overwrites [`AppState::write_op_budget`]
    /// and re-derives [`AppState::wedge_threshold`] from it.
    /// Test: `tools::tests::write_budget_tests`,
    /// `the_wedge_line_follows_the_configured_write_budgets`.
    #[must_use]
    pub fn with_write_op_budget(mut self, budget: Duration) -> Self {
        self.write_op_budget = budget;
        // #9783: the wedge line follows the write bounds this state enforces.
        self.wedge_threshold =
            worker_liveness::wedge_threshold_for(self.write_op_budget, self.write_pipeline_budget);
        self
    }

    /// Override the write-pipeline ceiling (issue #6366).
    ///
    /// Why: same reason as [`AppState::with_write_op_budget`] — a test proving
    /// the ceiling fires must not set `TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS`,
    /// which is process-wide and would race any parallel test.
    /// What: consuming builder that overwrites
    /// [`AppState::write_pipeline_budget`] and re-derives
    /// [`AppState::wedge_threshold`] from it.
    /// Test: `tools::tests::write_budget_tests::memory_note_surfaces_the_pipeline_ceiling`,
    /// `the_wedge_line_follows_the_configured_write_budgets`.
    #[must_use]
    pub fn with_write_pipeline_budget(mut self, budget: Duration) -> Self {
        self.write_pipeline_budget = budget;
        // #9783: the wedge line follows the write bounds this state enforces.
        self.wedge_threshold =
            worker_liveness::wedge_threshold_for(self.write_op_budget, self.write_pipeline_budget);
        self
    }

    /// Override the recall time limits (#11255).
    ///
    /// Why: the open-wait and deadline tests need bounds of milliseconds
    /// without setting the process-wide env vars.
    /// What: consuming builder that overwrites [`AppState::recall_limits`].
    /// Test: `a_held_palace_open_answers_palace_opening_and_the_open_continues`,
    /// `scope_all_returns_partial_results_at_its_deadline`.
    #[must_use]
    pub fn with_recall_limits(mut self, limits: RecallLimits) -> Self {
        self.recall_limits = limits;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#11255 review): no configured value can make the open wait reach
    /// the open-queue bound, or make the deadline overflow an `Instant`.
    /// What: a table of raw `(open_wait, all_deadline, open_queue)` values,
    /// the defaults included, and the limits `resolve` makes of each; then an
    /// `Instant::now() + all_deadline` that must not panic.
    /// Test: itself.
    #[test]
    fn recall_limits_resolve_below_the_queue_bound_and_under_the_cap() {
        let s = Duration::from_secs;
        let huge = Duration::from_secs(u64::MAX);
        for ((open, all, queue), (want_open, want_all)) in [
            ((s(15), s(20), s(60)), (s(15), s(20))),
            ((s(60), s(20), s(60)), (s(45), s(20))),
            ((s(90), s(20), s(60)), (s(45), s(20))),
            ((huge, huge, s(60)), (s(45), MAX_RECALL_ALL_DEADLINE)),
            ((s(15), s(20), s(4)), (s(3), s(20))),
            ((s(15), s(20), Duration::ZERO), (Duration::ZERO, s(20))),
            ((s(15), s(20), huge), (s(15), s(20))),
        ] {
            let got = RecallLimits::resolve(open, all, queue);
            assert_eq!(
                (got.open_wait, got.all_deadline),
                (want_open, want_all),
                "{open:?} {all:?} {queue:?}"
            );
            assert!(
                queue.is_zero() || got.open_wait < queue,
                "{got:?} vs {queue:?}"
            );
            let _ = tokio::time::Instant::now() + got.all_deadline;
        }
    }
}
