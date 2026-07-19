//! Rename a managed session — update `tmux_name` + the live tmux entity.
//!
//! Why: operators need to give a managed session a meaningful name, both from
//! the master list (`tm sessions rename <id-or-name> <new-name>`) and from
//! within a session (`tm sessions rename <new-name>`, resolved from
//! `$TM_MANAGED_SESSION_ID`). A managed session's identity name IS its
//! `tmux_name` (there is no separate label field), so a rename updates that
//! field AND renames the live tmux session so `tmux attach`/`list-sessions`
//! stay consistent with the record. Extracted into its own file (mirroring
//! `delete.rs`) so `manager.rs` stays under the 500-SLOC production cap.
//! What: [`validate_session_name`] (a pure, unit-testable name check) plus an
//! inherent `impl SessionManager` block adding [`SessionManager::rename`], which
//! validates the name, guards against collisions (with another record OR a live
//! tmux session), renames the live tmux session when one exists, updates
//! `tmux_name`, and persists.
//! Test: `rename_*` in `super::rename_tests`.

use chrono::Utc;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// Validate a proposed session name, returning the trimmed value or a message.
///
/// Why: a tmux session name cannot be empty or contain whitespace, and tmux
/// treats `.`/`:` as reserved target separators — rejecting a bad name up front
/// with an actionable message beats a confusing downstream tmux failure.
/// What: trims `name`, then rejects an empty value, one over 64 chars, or any
/// character outside `[A-Za-z0-9_-]` (the alphabet the existing
/// `tm-<leaf>-NN`/`tm-<adjective>-<noun>` names already use). Returns the
/// trimmed, validated name on success.
/// Test: `validate_session_name_*` in `super::rename_tests`.
pub(crate) fn validate_session_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("session name must not be empty".to_string());
    }
    if trimmed.chars().count() > 64 {
        return Err(format!(
            "session name '{trimmed}' is too long (max 64 characters)"
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "session name '{trimmed}' has invalid characters — \
             use letters, digits, '-', and '_' only"
        ));
    }
    Ok(trimmed.to_string())
}

impl SessionManager {
    /// Rename a managed session — set its `tmux_name` and rename the live tmux
    /// session to match.
    ///
    /// Why: a managed session's user-facing identity IS its `tmux_name`;
    /// renaming must therefore update the record AND the live tmux entity in
    /// lock-step so a later `tmux attach`/`ls` never shows a stale name. A
    /// stopped session has no live tmux, so only its record changes.
    ///
    /// What: validates `new_name` via [`validate_session_name`] (an invalid
    /// name surfaces as [`ManagedError::InvalidState`]). Looks up the record (a
    /// missing id surfaces as [`ManagedError::SessionNotFound`]). A rename to
    /// the SAME name is a no-op that returns the record unchanged. A terminal
    /// record (`Decommissioned`/`Deleted`) is refused with `InvalidState`.
    /// Refuses — with [`ManagedError::NameCollision`] — a name already taken by
    /// ANOTHER managed record or by a live tmux session. Otherwise, when a live
    /// tmux session backs the record it is renamed via
    /// [`ManagedTmuxDriver::rename_session`](super::driver::ManagedTmuxDriver::rename_session)
    /// FIRST (so a tmux failure aborts before the store is touched — no drift),
    /// then the record's `tmux_name` is updated, `last_activity_at` is stamped,
    /// and the record is persisted. Returns the updated [`SessionRecord`].
    /// Test: `rename_updates_name_and_persists`,
    /// `rename_same_name_is_noop`, `rename_rejects_collision_with_record`,
    /// `rename_rejects_collision_with_live_tmux`,
    /// `rename_rejects_invalid_name`, `rename_rejects_terminal_record`,
    /// `rename_renames_live_tmux_session` in `super::rename_tests`.
    pub async fn rename(
        &self,
        id: &ManagedSessionId,
        new_name: &str,
    ) -> Result<SessionRecord, ManagedError> {
        let new_name = validate_session_name(new_name)
            .map_err(|msg| ManagedError::InvalidState(id.to_string(), msg))?;

        let record = self.get(id).await?;
        if record.tmux_name == new_name {
            // Renaming to the current name is a no-op — nothing to persist.
            return Ok(record);
        }
        if matches!(
            record.state,
            ManagedSessionState::Decommissioned | ManagedSessionState::Deleted
        ) {
            return Err(ManagedError::InvalidState(
                id.to_string(),
                format!(
                    "session is {} — a terminal record cannot be renamed",
                    record.state
                ),
            ));
        }

        // Collision guard: no OTHER managed record may already carry the name…
        if self
            .list()
            .await
            .into_iter()
            .any(|r| r.id != *id && r.tmux_name == new_name)
        {
            return Err(ManagedError::NameCollision(new_name));
        }
        // …and no live tmux session (managed or foreign) may hold it either.
        if self.tmux.session_exists(&new_name) {
            return Err(ManagedError::NameCollision(new_name));
        }

        // Rename the live tmux session FIRST when one backs the record: a tmux
        // failure aborts here, before the store is mutated, so the record's name
        // can never drift from the live session. A stopped session has no live
        // tmux to rename — only the record changes.
        if self.tmux.session_exists(&record.tmux_name) {
            self.tmux.rename_session(&record.tmux_name, &new_name)?;
        }

        let mut updated = record;
        updated.tmux_name = new_name;
        updated.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(updated.clone()).await?;
        Ok(updated)
    }
}
