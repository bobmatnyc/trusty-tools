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
    /// session, via [`SessionManager::taken_name_set`] +
    /// [`SessionManager::dedupe_name_against`] (a terminal tombstone's name is
    /// reusable as-is, unsuffixed). The record lookup, collision check, dedupe,
    /// live-tmux rename, and persist ALL run under ONE held store write guard
    /// (#3692 review HIGH-3): for a STOPPED record nothing else serializes two
    /// concurrent renames, so a lock-free dedupe snapshot would let both compute
    /// the same free ordinal and both persist — recreating the very
    /// two-records-one-name defect this fix exists for. Holding the guard also
    /// subsumes the old pre-tmux-rename "did someone rename me concurrently?"
    /// re-verify: no other in-process rename can interleave at all. When a live
    /// tmux session backs the record it is renamed via
    /// [`ManagedTmuxDriver::rename_session`](super::driver::ManagedTmuxDriver::rename_session)
    /// FIRST (a tmux failure aborts before the store is mutated), then the
    /// record's `tmux_name` is updated, `last_activity_at` is stamped, and the
    /// record is persisted through the same guard. If the store write fails
    /// AFTER the tmux rename, the tmux rename is rolled back so the live
    /// session and the record never desync; a failed rollback surfaces an
    /// explicit manual-recovery error. Returns the updated [`SessionRecord`]
    /// (its `tmux_name` may differ from the requested `new_name` if it was
    /// auto-suffixed).
    /// Test: `rename_updates_name_and_persists`,
    /// `rename_same_name_is_noop`, `rename_suffixes_collision_with_record`,
    /// `rename_suffixes_collision_with_live_tmux`,
    /// `rename_suffix_skips_to_next_free_ordinal`,
    /// `rename_concurrent_stopped_renames_to_same_target_never_collide`,
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

        // ── Everything below runs under ONE held store write guard (#3692
        // review HIGH-3): lookup, collision-check/dedupe, tmux rename, and
        // upsert. Two concurrent renames — including of two STOPPED records,
        // where no live tmux accidentally serializes them — are therefore
        // fully ordered: the second recomputes its ordinal AFTER the first
        // has persisted, so they can never both claim the same name. NOTE:
        // do not call `self.get`/`self.list`/`self.dedupe_session_name` in
        // here — they take this same lock (deadlock); read through the guard.
        let mut store = self.store.write().await;
        let records = store.all().await?;
        let record = records
            .iter()
            .find(|r| r.id == *id)
            .cloned()
            .ok_or_else(|| ManagedError::SessionNotFound(id.to_string()))?;
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
        let taken = self.taken_name_set(&records, Some(id))?;
        let new_name = Self::dedupe_name_against(&new_name, &taken);

        let old_name = record.tmux_name.clone();
        let tmux_live = self.tmux.session_exists(&old_name);

        // Rename the live tmux session FIRST when one backs the record: a tmux
        // failure aborts here, before the store is mutated. A stopped session
        // has no live tmux to rename — only the record changes. (The held
        // guard above already excludes a concurrent in-process rename of this
        // record, so no pre-rename re-verify is needed anymore.)
        if tmux_live {
            self.tmux.rename_session(&old_name, &new_name)?;
        }

        let mut updated = record;
        updated.tmux_name = new_name.clone();
        updated.last_activity_at = Some(Utc::now());
        if let Err(e) = store.upsert(updated.clone()).await {
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
