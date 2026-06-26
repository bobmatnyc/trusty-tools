//! Hook-correlation helpers that write Claude session ids to managed records.
//!
//! Why: `manager.rs` was at 500 SLOC (the production cap) after the
//! `claude_session_id` field was added; adding `set_claude_session_id` there
//! would breach the cap. Following the same pattern as `adopt.rs` (a sibling
//! `impl SessionManager` block), this file isolates the one write-path needed
//! by the hook relay — keeping all other files under their SLOC budgets.
//! What: one inherent method `SessionManager::set_claude_session_id` — looks
//! up the record, writes `claude_session_id`, and persists atomically.
//! Test: `claude_session_id_persists_on_session` in `super::tests`.

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
}
