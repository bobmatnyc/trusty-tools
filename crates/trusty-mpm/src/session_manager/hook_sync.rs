//! Hook-correlation helpers that write Claude session ids to managed records.
//!
//! Why: `manager.rs` was at 500 SLOC (the production cap) after the
//! `claude_session_id` field was added; adding `set_claude_session_id` there
//! would breach the cap. Following the same pattern as `adopt.rs` (a sibling
//! `impl SessionManager` block), this file isolates the one write-path needed
//! by the hook relay — keeping all other files under their SLOC budgets.
//! What: two inherent methods — `SessionManager::set_claude_session_id` looks
//! up the record, writes `claude_session_id`, and persists atomically;
//! `SessionManager::clear_claude_session_id_if` (#4337) clears it again, but
//! only when the stored value still matches an expected id, so a
//! `SessionEnd` can safely un-correlate its OWN session without racing a
//! concurrent, fresher correlation.
//! Test: `claude_session_id_persists_on_session`,
//! `clear_claude_session_id_if_clears_on_exact_match`,
//! `clear_claude_session_id_if_leaves_a_different_id_untouched` in
//! `super::tests`.

use super::manager::{ManagedError, SessionManager};
use super::record::ManagedSessionId;

impl SessionManager {
    /// Store the Claude Code internal session UUID on a managed session record.
    ///
    /// Why (#1744): the `SessionStart` hook delivers Claude Code's own session
    /// UUID via `CLAUDE_SESSION_ID`. Persisting it lets `resume` pass
    /// `--resume <id>` to the new `claude` process, restoring the prior
    /// conversation even after an ungraceful exit (terminal kill, tmux pane
    /// closed without `/quit`). Without this id, resume falls back to
    /// `--continue` (most-recent conversation) or a fresh launch.
    /// What: looks up the record, sets `claude_session_id`, and persists.
    /// No tmux I/O — the hook handler calls this after correlating the
    /// `SessionStart` event to the right managed session.
    /// Test: `claude_session_id_persists_on_session` in `super::tests`.
    pub async fn set_claude_session_id(
        &self,
        id: &ManagedSessionId,
        claude_session_id: &str,
    ) -> Result<(), ManagedError> {
        let mut r = self.get(id).await?;
        r.claude_session_id = Some(claude_session_id.to_owned());
        self.store.write().await.upsert(r).await.map_err(Into::into)
    }

    /// Clear a managed record's `claude_session_id`, but ONLY if it still
    /// equals `expected` (#4337).
    ///
    /// Why: `SessionEnd` is Claude Code's own signal that `expected`'s
    /// conversation has ended, so the id is guaranteed stale from that point
    /// on — leaving it in place would let a later `SessionStart` from an
    /// unrelated process (or a stale resume) be silently compared against a
    /// dead id forever, permanently blocking re-correlation. Clearing it here
    /// lets the NEXT `SessionStart` for this record's cwd freely take the
    /// "no existing id" branch in `daemon::api::session_start_correlation`.
    /// The exact-match guard prevents clobbering a FRESHER id that a
    /// concurrent, later `SessionStart` may have already written between the
    /// caller's own match check and this call.
    /// What: no-op (`Ok(())`) when the record is missing, already `None`, or
    /// holds a DIFFERENT id than `expected`. Test:
    /// `clear_claude_session_id_if_clears_on_exact_match`,
    /// `clear_claude_session_id_if_leaves_a_different_id_untouched` in
    /// `super::tests`.
    pub async fn clear_claude_session_id_if(
        &self,
        id: &ManagedSessionId,
        expected: &str,
    ) -> Result<(), ManagedError> {
        let mut r = self.get(id).await?;
        if r.claude_session_id.as_deref() != Some(expected) {
            return Ok(());
        }
        r.claude_session_id = None;
        self.store.write().await.upsert(r).await.map_err(Into::into)
    }
}
