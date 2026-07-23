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
use super::record::{ManagedSessionId, SessionRecord};

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
    /// Auto-suffixes — NEVER rejects (owner decision, issue #3692) — a name
    /// already taken by another NON-terminal managed record or by a live tmux
    /// session, via [`SessionManager::dedupe_session_name`] (a terminal
    /// tombstone's name is reusable as-is, unsuffixed). Otherwise, when a live
    /// tmux session backs the record it is renamed via
    /// [`ManagedTmuxDriver::rename_session`](super::driver::ManagedTmuxDriver::rename_session)
    /// FIRST (re-verifying `tmux_name` immediately beforehand to catch a
    /// concurrent rename), then the record's `tmux_name` is updated,
    /// `last_activity_at` is stamped, and the record is persisted. If the store
    /// write fails AFTER the tmux rename, the tmux rename is rolled back so the
    /// live session and the record never desync; a failed rollback surfaces an
    /// explicit manual-recovery error. Returns the updated [`SessionRecord`]
    /// (its `tmux_name` may differ from the requested `new_name` if it was
    /// auto-suffixed).
    /// Test: `rename_updates_name_and_persists`,
    /// `rename_same_name_is_noop`, `rename_suffixes_collision_with_record`,
    /// `rename_suffixes_collision_with_live_tmux`,
    /// `rename_suffix_skips_to_next_free_ordinal`,
    /// `rename_rejects_invalid_name`, `rename_rejects_terminal_record`,
    /// `rename_renames_live_tmux_session`,
    /// `rename_reuses_name_freed_by_a_deleted_record` in `super::rename_tests`.
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
        if record.state.is_terminal() {
            return Err(ManagedError::InvalidState(
                id.to_string(),
                format!(
                    "session is {} — a terminal (decommissioned/deleted) record cannot be renamed",
                    record.state
                ),
            ));
        }

        // Auto-suffix on collision — NEVER reject (owner decision, issue
        // #3692): a name already held by another non-terminal managed record
        // or a live tmux session (managed or foreign) is disambiguated with
        // the smallest free `-N` ordinal rather than refusing the rename. A
        // TERMINAL tombstone's `tmux_name` does NOT count as taken (it is gone
        // for good) — a name freed by delete/decommission is reused as-is,
        // unsuffixed; only `prune` still holds the on-disk tombstone.
        let new_name = self.dedupe_session_name(&new_name, Some(id)).await?;

        let old_name = record.tmux_name.clone();
        let tmux_live = self.tmux.session_exists(&old_name);

        // Rename the live tmux session FIRST when one backs the record: a tmux
        // failure aborts here, before the store is mutated. A stopped session
        // has no live tmux to rename — only the record changes.
        //
        // Concurrency (get→check→act is not atomic): another rename could have
        // changed this record's `tmux_name` between our read and now. Re-verify
        // right before the destructive tmux rename and abort on mismatch, so we
        // never rename a tmux session that no longer belongs to this record.
        if tmux_live {
            let current = self.get(id).await?;
            if current.tmux_name != old_name {
                return Err(ManagedError::InvalidState(
                    id.to_string(),
                    format!(
                        "session was renamed concurrently (now '{}', expected '{old_name}') — \
                         retry the rename",
                        current.tmux_name
                    ),
                ));
            }
            self.tmux.rename_session(&old_name, &new_name)?;
        }

        let mut updated = record;
        updated.tmux_name = new_name.clone();
        updated.last_activity_at = Some(Utc::now());
        if let Err(e) = self.store.write().await.upsert(updated.clone()).await {
            // Compensation: the tmux session was already renamed but the store
            // write failed, so the live tmux name and the (unchanged) record
            // would desync. Roll the tmux rename back; if THAT also fails, surface
            // an explicit manual-recovery error rather than leaving a silent split.
            if tmux_live && let Err(rollback) = self.tmux.rename_session(&new_name, &old_name) {
                return Err(ManagedError::InvalidState(
                    id.to_string(),
                    format!(
                        "rename half-applied: tmux is now '{new_name}' but the store still \
                         records '{old_name}', and the rollback failed ({rollback}) — manually \
                         run `tmux rename-session -t {new_name} {old_name}` (store error: {e})"
                    ),
                ));
            }
            return Err(e.into());
        }
        Ok(updated)
    }
}
