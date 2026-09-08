//! Path-containment AND session-identity guards for workspace deletion (#1511,
//! #3764).
//!
//! Why: `SessionManager::decommission` previously `remove_dir_all`'d
//! `workspace_path` unconditionally, which deleted a live user repo when the
//! #1502 local-path spawn set `workspace_path` to a real on-disk directory.
//! This module provides the belt-and-suspenders containment guard that prevents
//! any path OUTSIDE the SM's managed workspace root from being deleted —
//! regardless of the `workspace_owned` flag. #3764 widens the module's
//! question from "is this path safe to touch at all" to "does anyone ELSE
//! also claim it": the #3715 incident's precursor was a 3-way cwd collision
//! (#1744) — three `Active` session records canonicalizing to one worktree
//! path — hours before that path was destroyed. Path containment alone never
//! sees that; it only ever asks about ONE path and ONE managed root.
//! What: [`is_safe_to_remove`] canonicalizes both paths and verifies that the
//! workspace is strictly INSIDE the managed root, rejecting: path == root, path
//! outside root, paths with too few components, and `$HOME`.
//! [`foreign_active_claim`] answers the session-identity question: does any
//! `Active` record OTHER than the one being acted on also canonicalize to the
//! same directory?
//! Test: `is_safe_to_remove_*` and `foreign_active_claim_*` unit tests below.

use std::path::Path;

use tracing::warn;

use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// Canonicalize `path`, falling back to the raw form on failure.
///
/// Why: shared by both directions of the comparison
/// [`foreign_active_claim`] makes — the candidate path AND every other
/// record's `workspace_path`/`cwd` — so a canonicalize failure on either side
/// degrades to a raw-string comparison instead of silently excusing the
/// comparison entirely. A path that no longer exists on disk (already
/// destroyed) must still be comparABLE by its last-known spelling.
fn canon_or_raw(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Does any `Active` session record OTHER than `self_id` also claim
/// `workspace_path` (#3764)?
///
/// Why: `is_safe_to_remove` answers "is this path inside the managed root",
/// never "does a SIBLING session also believe this is theirs". The #3715
/// incident's own precursor state (#1744: three `Active` records sharing one
/// cwd) is invisible to path containment by construction — it is a
/// same-path, cross-RECORD question, not a path-vs-root one. This check
/// closes that hole at the one place every daemon-routed removal already
/// passes through: before `SessionManager::decommission_with_root` mutates
/// disk, it now asks this question first, unconditionally — unlike
/// [`super::manager::ManagedError::WorktreeOwnerMismatch`]'s gate, which only
/// fires when a caller identifies itself, this runs regardless of caller,
/// because the hazard is a store inconsistency (two `Active` records naming
/// one directory), not a caller impersonating an owner.
///
/// What: canonicalizes `workspace_path` (falling back to the raw path on a
/// canonicalize failure — a destroyed directory must still compare) and scans
/// `records` for the first `Active` record whose id is not `self_id` and
/// whose `workspace_path` OR `cwd` canonicalizes (or, on failure, compares
/// raw) to the same path. Returns that record's id.
///
/// A record in any state other than `Active` is never a conflict — a
/// `Stopped`/`Errored`/terminal record's directory is exactly what the
/// caller's own worktree-reclaim machinery ([`super::worktree_reclaim`]'s
/// gate 2) already expects to reclaim, and treating it as a live claim here
/// would refuse ordinary, safe cleanup.
/// Test: `foreign_active_claim_finds_a_colliding_workspace_path`,
/// `foreign_active_claim_finds_a_colliding_cwd`,
/// `foreign_active_claim_ignores_a_non_active_record`,
/// `foreign_active_claim_ignores_the_records_own_id`,
/// `foreign_active_claim_returns_none_when_unclaimed`.
pub(crate) fn foreign_active_claim(
    workspace_path: &Path,
    self_id: &ManagedSessionId,
    records: &[SessionRecord],
) -> Option<ManagedSessionId> {
    let canon_target = canon_or_raw(workspace_path);
    records
        .iter()
        .find(|r| {
            if r.id == *self_id || r.state != ManagedSessionState::Active {
                return false;
            }
            let ws_matches = r
                .workspace_path
                .as_deref()
                .is_some_and(|p| canon_or_raw(p) == canon_target || p == workspace_path);
            let cwd_matches = canon_or_raw(&r.cwd) == canon_target || r.cwd == workspace_path;
            ws_matches || cwd_matches
        })
        .map(|r| r.id)
}

/// Decide whether `workspace_path` is safe to `remove_dir_all` (#1511).
///
/// Why: even an `workspace_owned = true` record should only be deleted when the
/// path is strictly INSIDE the SM's managed workspaces root. This prevents
/// decommission from deleting a directory if `workspace_owned` were stale or if a
/// bug let a real path slip through as "owned". The guard rejects: path == root,
/// path outside root, path with too few components (filesystem / volume root), and
/// the user's home directory.
///
/// What: canonicalizes both paths and checks that `workspace_path` starts with
/// `managed_root` AND is not equal to it (strictly INSIDE). Also rejects any path
/// that is `$HOME` or has fewer than 3 components (e.g. `/`, `/tmp`) as an extra
/// safety net against catastrophic deletion. Returns `false` on any I/O error
/// during canonicalization (e.g. path does not exist) — never errors out.
///
/// Test: `is_safe_to_remove_rejects_outside_root`, `is_safe_to_remove_rejects_root_itself`,
/// `is_safe_to_remove_rejects_home`, `is_safe_to_remove_rejects_shallow_path`,
/// `is_safe_to_remove_accepts_valid_child` in this module.
pub(crate) fn is_safe_to_remove(workspace_path: &Path, managed_root: &Path) -> bool {
    // Reject suspiciously shallow ABSOLUTE paths before canonicalizing.
    // This counts components from the filesystem root (e.g. `/` counts 1,
    // `/tmp` counts 2) as a coarse guard against catastrophic paths like `/`
    // or `/tmp` — NOT as a measure of depth relative to the managed root.
    // The real containment check (canonicalize + starts_with) follows below.
    let component_count = workspace_path.components().count();
    if component_count < 3 {
        warn!(
            path = %workspace_path.display(),
            components = component_count,
            "is_safe_to_remove: rejecting — too few path components"
        );
        return false;
    }

    // Reject $HOME outright — no managed session should ever BE the home dir.
    if dirs::home_dir().is_some_and(|home| workspace_path == home) {
        warn!(
            path = %workspace_path.display(),
            "is_safe_to_remove: rejecting — path is $HOME"
        );
        return false;
    }

    // Canonicalize both paths to resolve symlinks and `..` so a symlink into
    // the managed root (or vice-versa) cannot trick the prefix check.
    let canon_ws = match workspace_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            warn!(
                path = %workspace_path.display(),
                "is_safe_to_remove: cannot canonicalize workspace path ({e}); skipping deletion"
            );
            return false;
        }
    };
    let canon_root = match managed_root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            warn!(
                root = %managed_root.display(),
                "is_safe_to_remove: cannot canonicalize managed root ({e}); skipping deletion"
            );
            return false;
        }
    };

    // Must be strictly INSIDE the root (starts_with AND not equal to root).
    if canon_ws == canon_root {
        warn!(
            path = %workspace_path.display(),
            "is_safe_to_remove: rejecting — path IS the managed root"
        );
        return false;
    }
    if !canon_ws.starts_with(&canon_root) {
        warn!(
            path = %workspace_path.display(),
            root = %managed_root.display(),
            "is_safe_to_remove: rejecting — path is outside the managed root"
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        ManagedSessionId, ManagedSessionState, SessionRecord, foreign_active_claim,
        is_safe_to_remove,
    };

    /// Build a bare [`SessionRecord`] naming `workspace_path` and `state`, for
    /// the [`foreign_active_claim`] tests below. Mirrors the field list
    /// `decommission_tests::owned_record` uses for the same purpose.
    fn record_at(
        id: ManagedSessionId,
        state: ManagedSessionState,
        workspace_path: PathBuf,
    ) -> SessionRecord {
        SessionRecord {
            id,
            tmux_name: format!("tm-foreign-claim-{id}"),
            cwd: PathBuf::from("/tmp/unrelated-cwd"),
            task: "task".into(),
            state,
            created_at: chrono::Utc::now(),
            last_activity_at: None,
            workspace_path: Some(workspace_path),
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
            worktree_owner: None,
            terminal_at: None,
            stop_cause: None,
        }
    }

    // ── foreign_active_claim unit tests (#3764) ─────────────────────────────

    /// Another `Active` record whose `workspace_path` matches the candidate is
    /// a conflict.
    ///
    /// Why: this is the #1744 precursor shape — two `Active` records naming
    /// one directory.
    /// Test: this function IS the test.
    #[test]
    fn foreign_active_claim_finds_a_colliding_workspace_path() {
        let root = crate::test_support::hermetic_temp_dir();
        let shared = root.path().join("shared-worktree");
        std::fs::create_dir_all(&shared).unwrap();

        let self_id = ManagedSessionId::new();
        let other_id = ManagedSessionId::new();
        let records = vec![record_at(
            other_id,
            ManagedSessionState::Active,
            shared.clone(),
        )];

        assert_eq!(
            foreign_active_claim(&shared, &self_id, &records),
            Some(other_id),
            "an Active sibling record naming the same path must be reported"
        );
    }

    /// A collision on `cwd` (not `workspace_path`) is also caught.
    ///
    /// Why: the #1744 collision this guards against was keyed on `cwd`, not
    /// `workspace_path` — a record can claim a directory as its `cwd` before a
    /// `workspace_path` is ever recorded for it.
    /// Test: this function IS the test.
    #[test]
    fn foreign_active_claim_finds_a_colliding_cwd() {
        let root = crate::test_support::hermetic_temp_dir();
        let shared = root.path().join("shared-cwd");
        std::fs::create_dir_all(&shared).unwrap();

        let self_id = ManagedSessionId::new();
        let other_id = ManagedSessionId::new();
        let mut other = record_at(
            other_id,
            ManagedSessionState::Active,
            root.path().join("elsewhere"),
        );
        other.cwd = shared.clone();
        let records = vec![other];

        assert_eq!(
            foreign_active_claim(&shared, &self_id, &records),
            Some(other_id),
            "an Active sibling record whose cwd matches must be reported"
        );
    }

    /// A record in any non-`Active` state is never a conflict.
    ///
    /// Why: a `Stopped`/`Decommissioned` record's directory is exactly what
    /// ordinary reclaim is FOR — treating a terminal record as a live claim
    /// would refuse safe cleanup, not just unsafe cleanup.
    /// Test: this function IS the test.
    #[test]
    fn foreign_active_claim_ignores_a_non_active_record() {
        let root = crate::test_support::hermetic_temp_dir();
        let shared = root.path().join("shared-terminal");
        std::fs::create_dir_all(&shared).unwrap();

        let self_id = ManagedSessionId::new();
        let other_id = ManagedSessionId::new();
        let records = vec![record_at(
            other_id,
            ManagedSessionState::Decommissioned,
            shared.clone(),
        )];

        assert_eq!(
            foreign_active_claim(&shared, &self_id, &records),
            None,
            "a terminal-state record must never block removal"
        );
    }

    /// The record's own id is never reported as a conflict with itself.
    ///
    /// Why: `records` passed to this function typically includes the record
    /// being decommissioned itself; excluding `self_id` is what makes an
    /// ordinary, uncontested decommission possible at all.
    /// Test: this function IS the test.
    #[test]
    fn foreign_active_claim_ignores_the_records_own_id() {
        let root = crate::test_support::hermetic_temp_dir();
        let shared = root.path().join("own-workspace");
        std::fs::create_dir_all(&shared).unwrap();

        let self_id = ManagedSessionId::new();
        let records = vec![record_at(
            self_id,
            ManagedSessionState::Active,
            shared.clone(),
        )];

        assert_eq!(
            foreign_active_claim(&shared, &self_id, &records),
            None,
            "a record must never conflict with itself"
        );
    }

    /// No record at all claiming the path returns `None`.
    ///
    /// Why: this is the ordinary, uncontested case that must stay fast and
    /// silent — most decommissions have no sibling collision.
    /// Test: this function IS the test.
    #[test]
    fn foreign_active_claim_returns_none_when_unclaimed() {
        let root = crate::test_support::hermetic_temp_dir();
        let unclaimed = root.path().join("nobody-here");
        std::fs::create_dir_all(&unclaimed).unwrap();

        let self_id = ManagedSessionId::new();
        assert_eq!(foreign_active_claim(&unclaimed, &self_id, &[]), None);
    }

    // ── is_safe_to_remove unit tests (#1511) ────────────────────────────────

    /// A valid child of the managed root passes the guard.
    ///
    /// Why: this is the happy path — an SM-provisioned workspace is always
    /// nested under the managed root.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_accepts_valid_child() {
        let root = crate::test_support::hermetic_temp_dir();
        let child = root.path().join("owner").join("repo").join("session-abc");
        std::fs::create_dir_all(&child).unwrap();
        assert!(
            is_safe_to_remove(&child, root.path()),
            "a path strictly inside the managed root must pass"
        );
    }

    /// A path equal to the managed root is rejected.
    ///
    /// Why: deleting the root itself would wipe all managed workspaces at once.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_rejects_root_itself() {
        let root = crate::test_support::hermetic_temp_dir();
        assert!(
            !is_safe_to_remove(root.path(), root.path()),
            "the managed root itself must be rejected"
        );
    }

    /// A path outside the managed root is rejected even if it exists.
    ///
    /// Why: a stale or stale `workspace_owned` flag must not cause out-of-root
    /// deletion.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_rejects_outside_root() {
        let root = crate::test_support::hermetic_temp_dir();
        let outside = crate::test_support::hermetic_temp_dir(); // different temp dir
        let outside_child = outside.path().join("some").join("path");
        std::fs::create_dir_all(&outside_child).unwrap();
        assert!(
            !is_safe_to_remove(&outside_child, root.path()),
            "a path outside the managed root must be rejected"
        );
    }

    /// A path with fewer than 3 components is rejected.
    ///
    /// Why: `/`, `/tmp`, or a single-segment path is never a valid SM workspace.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_rejects_shallow_path() {
        let root = crate::test_support::hermetic_temp_dir();
        // A 1-component path like "/" or a 2-component path like "/tmp" must be
        // rejected before even reaching canonicalization.
        let shallow = PathBuf::from("/tmp");
        assert!(
            !is_safe_to_remove(&shallow, root.path()),
            "a shallow path (/tmp) must be rejected by the component-count guard"
        );
    }

    /// `$HOME` is rejected outright.
    ///
    /// Why: deleting the user's home directory is catastrophic and must be
    /// impossible regardless of what the managed root is set to.
    /// Test: this function IS the test.
    ///
    /// Why serial (issue #2461 sweep): this test reads `dirs::home_dir()`
    /// itself AND `is_safe_to_remove` reads it again internally — two
    /// separate reads of the process-wide `HOME` env var that must observe
    /// the same value. Serialized against other `HOME`-redirecting tests in
    /// this binary for the same reason as the `core::paths` sweep.
    #[serial_test::serial]
    #[test]
    fn is_safe_to_remove_rejects_home() {
        if let Some(home) = dirs::home_dir() {
            // Use home as both path and root — even if "home is inside home" the
            // home-directory check fires first and rejects.
            assert!(
                !is_safe_to_remove(&home, &home),
                "$HOME must always be rejected"
            );
        }
    }

    /// A non-existent path returns false (canonicalize fails).
    ///
    /// Why: a workspace that has already been deleted must not cause a panic;
    /// the guard should simply return false so decommission logs and moves on.
    /// Test: this function IS the test.
    #[test]
    fn is_safe_to_remove_returns_false_for_nonexistent_path() {
        let root = crate::test_support::hermetic_temp_dir();
        let nonexistent = root.path().join("ghost").join("nope").join("absent");
        // path does not exist → canonicalize fails → returns false
        assert!(
            !is_safe_to_remove(&nonexistent, root.path()),
            "a non-existent workspace path must return false (canonicalize fails)"
        );
    }
}
