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

impl RecallLimits {
    /// Read both limits from the environment (see [`crate::engine::timeouts`]).
    pub fn from_env() -> Self {
        Self {
            open_wait: crate::engine::timeouts::recall_open_wait(),
            all_deadline: crate::engine::timeouts::recall_all_deadline(),
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
