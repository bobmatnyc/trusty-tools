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
use tracing::warn;

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
    /// reusable as-is, unsuffixed).
    ///
    /// Locking design (#3692 review HIGH-3, revised by #3698 round-2 HIGH-A):
    /// tmux subprocess calls are NEVER made while the store write guard is
    /// held — `store.write()` gates every session-manager mutation daemon-wide,
    /// and `rename_session`/`list_sessions` are blocking fork/exec/waits (a
    /// hung tmux under the guard would stall the whole daemon; every other
    /// write path in this module keeps tmux outside the lock). Instead:
    /// the live-tmux snapshot is fetched FIRST (unlocked); then lookup +
    /// collision-check/dedupe run under the guard. A record with NO live tmux
    /// (stopped) persists under that SAME guard — this is the HIGH-3
    /// guarantee: two concurrent stopped-session renames to one target are
    /// fully serialized and can never both claim the same name. A record WITH
    /// a live tmux session releases the guard, renames tmux (unlocked), then
    /// RE-ACQUIRES the guard and RE-VERIFIES before persisting that (a) the
    /// record still exists, non-terminal, still under its old name, and (b)
    /// nobody claimed the deduped name while the lock was released; on
    /// re-verify failure the tmux rename is rolled back and a retryable
    /// `InvalidState` error is returned. If the store write itself fails
    /// after the tmux rename, the tmux rename is rolled back so the live
    /// session and the record never desync; a failed rollback surfaces an
    /// explicit manual-recovery error. Returns the updated [`SessionRecord`]
    /// (its `tmux_name` may differ from the requested `new_name` if it was
    /// auto-suffixed).
    ///
    /// Tmux-identity guard (#3714 remediation): "does a live tmux entity need
    /// renaming" is decided by this record's OWN recorded `pane_id` — verified
    /// pane-scoped via `ManagedTmuxDriver::pane_exists` — never by a bare
    /// name-string membership check against `list_sessions`. A duplicate-name
    /// record (the #3692 condition) whose own pane is gone must never have its
    /// rename physically retarget an UNRELATED live session that happens to
    /// hold the same name; when that mismatch is detected, only the DB record
    /// is renamed (with a `warn!` notice) and the live tmux entity is left
    /// untouched.
    /// Test: `rename_updates_name_and_persists`,
    /// `rename_same_name_is_noop`, `rename_suffixes_collision_with_record`,
    /// `rename_suffixes_collision_with_live_tmux`,
    /// `rename_suffix_skips_to_next_free_ordinal`,
    /// `rename_concurrent_stopped_renames_to_same_target_never_collide`,
    /// `rename_rejects_invalid_name`, `rename_rejects_terminal_record`,
    /// `rename_renames_live_tmux_session`,
    /// `rename_never_renames_unrelated_live_session_sharing_a_stale_name`,
    /// `rename_renames_live_session_when_pane_confirmed_alive`,
    /// `rename_reuses_name_freed_by_a_deleted_record` in `super::rename_tests`.
    pub async fn rename(
        &self,
        id: &ManagedSessionId,
        new_name: &str,
    ) -> Result<SessionRecord, ManagedError> {
        let new_name = validate_session_name(new_name)
            .map_err(|msg| ManagedError::InvalidState(id.to_string(), msg))?;

        // Live-tmux snapshot BEFORE the guard (#3698 round-2 HIGH-A):
        // `list_sessions` is a blocking tmux subprocess and must never run
        // under the store write lock that gates every other mutation.
        let live_names = self
            .tmux
            .list_sessions()
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;

        // ── Guard 1: lookup + collision-check/dedupe (no tmux calls held).
        // NOTE: do not call `self.get`/`self.list`/`self.dedupe_session_name`
        // in here — they take this same lock (deadlock); read through the
        // guard.
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
        let taken = Self::taken_name_set(&live_names, &records, Some(id));
        let new_name = Self::dedupe_name_against(&new_name, &taken);

        let old_name = record.tmux_name.clone();
        // #3714 (remediation addendum): a bare `live_names.contains(old_name)`
        // proves only that SOME tmux session is currently using this NAME — a
        // duplicate-name record (the #3692 condition) can have its OWN pane
        // long gone while an UNRELATED, genuinely live session (owned by a
        // DIFFERENT record) still happens to hold that exact name. Physically
        // renaming "whatever tmux session currently holds `old_name`" in that
        // case hijacks the unrelated live session out from under its own
        // operator — the exact remediation-comment incident (`tm sessions
        // rename` retitled the attached `tm-tagents` session while trying to
        // rename an unrelated stale duplicate). When this record has a
        // captured `pane_id`, liveness is only trusted once the driver
        // confirms THAT SPECIFIC pane still lives inside a session named
        // `old_name` (`pane_exists` is pane-scoped, never a name-string
        // match) — tying the check to this record's OWN tmux identity. A
        // legacy record with no captured `pane_id` (pre-#2453) falls back to
        // the prior name-only check; no stronger signal exists for it.
        let name_live = live_names.iter().any(|n| n == &old_name);
        let tmux_live = match record.pane_id.as_deref() {
            Some(pane_id) => name_live && self.tmux.pane_exists(&old_name, pane_id),
            None => name_live,
        };
        if name_live && !tmux_live {
            warn!(
                id = %id,
                name = %old_name,
                "rename: a tmux session named '{old_name}' is live but does not contain \
                 this record's own recorded pane — it belongs to a DIFFERENT session \
                 (duplicate-name collision, #3714); renaming the DB record only and \
                 leaving the live tmux session untouched"
            );
        }

        let mut updated = record;
        updated.tmux_name = new_name.clone();
        updated.last_activity_at = Some(Utc::now());

        if !tmux_live {
            // Stopped path: nothing external to mutate — persist under the
            // SAME guard (#3692 HIGH-3): two concurrent stopped-session
            // renames are fully serialized here, so the second recomputes its
            // ordinal AFTER the first persisted and they can never collide.
            store.upsert(updated.clone()).await?;
            return Ok(updated);
        }
        drop(store);

        // Live path: rename tmux OUTSIDE any lock (#3698 round-2 HIGH-A) —
        // a slow/hung tmux must never stall the daemon-wide store guard.
        self.tmux.rename_session(&old_name, &new_name)?;

        // ── Guard 2: re-verify, then persist. The lock was released across
        // the tmux call, so both sides of the check-then-act must be
        // re-established before the write.
        let mut store = self.store.write().await;
        let records = match store.all().await {
            Ok(r) => r,
            Err(e) => {
                drop(store);
                return Err(self.rollback_rename(&new_name, &old_name, id, &e.to_string()));
            }
        };
        let self_unchanged = records
            .iter()
            .any(|r| r.id == *id && r.tmux_name == old_name && !r.state.is_terminal());
        let name_still_free = !records
            .iter()
            .any(|r| r.id != *id && r.tmux_name == new_name && !r.state.is_terminal());
        if !(self_unchanged && name_still_free) {
            drop(store);
            return Err(self.rollback_rename(
                &new_name,
                &old_name,
                id,
                "lost a concurrent rename race while tmux was being renamed — retry the rename",
            ));
        }
        if let Err(e) = store.upsert(updated.clone()).await {
            drop(store);
            return Err(self.rollback_rename(&new_name, &old_name, id, &e.to_string()));
        }
        Ok(updated)
    }

    /// Roll a half-applied tmux rename back (`new` → `old`) and build the
    /// error the caller should surface — shared by `rename`'s re-verify and
    /// store-write failure paths.
    ///
    /// Why: once tmux has been renamed OUTSIDE the store guard (#3698 round-2
    /// HIGH-A), any later failure — reload error, lost re-verify race, failed
    /// upsert — leaves the live session and the record desynced unless the
    /// tmux rename is compensated. Centralising the rollback keeps all three
    /// failure paths byte-identical.
    /// What: renames tmux back; on success returns a retryable
    /// [`ManagedError::InvalidState`] carrying `cause`; on rollback failure
    /// returns the explicit manual-recovery error naming the exact
    /// `tmux rename-session` command to run.
    /// Test: `rename_rolls_back_tmux_when_store_write_fails` in
    /// `super::rename_tests`.
    fn rollback_rename(
        &self,
        new_name: &str,
        old_name: &str,
        id: &ManagedSessionId,
        cause: &str,
    ) -> ManagedError {
        match self.tmux.rename_session(new_name, old_name) {
            Ok(()) => ManagedError::InvalidState(
                id.to_string(),
                format!("rename aborted and rolled back ({cause}) — retry the rename"),
            ),
            Err(rollback) => ManagedError::InvalidState(
                id.to_string(),
                format!(
                    "rename half-applied: tmux is now '{new_name}' but the store still \
                     records '{old_name}', and the rollback failed ({rollback}) — manually \
                     run `tmux rename-session -t {new_name} {old_name}` (cause: {cause})"
                ),
            ),
        }
    }
}
