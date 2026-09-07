//! `SessionRegistry::set_task_result` (#4351), split out of `registry.rs` for
//! the 500-SLOC cap — a child module of `registry` (declared via
//! `#[path = ...] mod task_result_ops;`), so it keeps full access to
//! `SessionRegistry`'s private `lock` helper and `SessionEntry`'s private
//! fields exactly as if the method were still defined there.
//!
//! Why: `registry.rs` sits within a few lines of its cap; the same reason
//! `registry_transcript.rs` and `registry_events.rs` exist.
//! What: [`SessionRegistry::set_task_result`].
//! Test: `registry_tests::set_task_result_stores_the_result`,
//! `registry_tests::set_task_result_on_unknown_session_is_a_no_op`.

use super::*;
use crate::session::task_result::TaskResult;

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
}
