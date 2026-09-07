//! `SessionRegistry::set_task_result` (#4351), split out of `registry.rs` for
//! the 500-SLOC cap — a child module of `registry` (declared via
//! `#[path = ...] mod task_result_ops;`), so it keeps full access to
//! `SessionRegistry`'s private `lock` helper and `SessionEntry`'s private
//! fields exactly as if the method were still defined there.
//!
//! Why: `registry.rs` sits within a few lines of its cap; the same reason
//! `registry_transcript.rs` and `registry_events.rs` exist.
//! What: [`SessionRegistry::set_task_result`] and the `session.cancel`
//! convenience [`SessionRegistry::set_cancelled_result`].
//! Test: `registry_tests::set_task_result_stores_the_result`,
//! `registry_tests::set_task_result_on_unknown_session_is_a_no_op`,
//! `registry_tests::cancel_records_a_refless_cancelled_result`.

use super::*;
use crate::session::task_result::{TaskResult, TaskResultStatus};

impl SessionRegistry {
    /// Record this session's actionable run result (#4351).
    ///
    /// Why: `session.status` must be able to hand a caller the branch/ref/
    /// pass-fail story, not just an opaque id — see
    /// `crate::session::task_result` for why. `crate::task::executor` is the
    /// only production caller; it fires exactly once, immediately before
    /// `finish`, so the result and the terminal status become visible in the
    /// same snapshot.
    /// What: overwrites `Session::result` wholesale. A session that has
    /// vanished (never created, or already pruned) is a silent no-op — the
    /// same contract [`Self::set_run_outcome`] uses, and for the same reason:
    /// this is called from a detached task that cannot meaningfully handle
    /// "the session went away while I was running".
    /// Test: `registry_tests::set_task_result_stores_the_result`,
    /// `registry_tests::set_task_result_on_unknown_session_is_a_no_op`.
    pub fn set_task_result(&self, id: &str, result: TaskResult) {
        let mut sessions = self.lock();
        if let Some(entry) = sessions.get_mut(id) {
            entry.session.result = Some(result);
        }
    }

    /// Record the result for a user-initiated `session.cancel` (#4351).
    ///
    /// Why: [`Self::cancel`] transitions a session straight to `Cancelled`
    /// without going through `crate::task::executor`, so without this a
    /// cancelled session was the ONE terminal state answering `session.status`
    /// with no `result` at all — a caller that reads the field to tell what
    /// happened would see nothing for the one outcome it most likely caused.
    /// A session with a LIVE run also gets a result from the executor when its
    /// loop unwinds; that one arrives later and knows more, so it correctly
    /// overwrites this one. A session with no live run gets only this.
    /// What: a refless `Cancelled` result. There is nothing on disk to point
    /// at — cancellation is a status transition, not a run outcome (vision
    /// spec §12 11.6) — so the refs stay `None` rather than being guessed.
    /// Test: `registry_tests::cancel_records_a_refless_cancelled_result`.
    pub(super) fn set_cancelled_result(&self, id: &str) {
        self.set_task_result(
            id,
            TaskResult::new(TaskResultStatus::Cancelled)
                .with_summary(Some("cancelled: no changes recorded".to_string())),
        );
    }
}
