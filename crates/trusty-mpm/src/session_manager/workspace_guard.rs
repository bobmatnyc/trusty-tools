//! Path-containment and session-identity guards for workspace deletion
//! (#1511 containment, #3764 identity).
//!
//! Why: `SessionManager::decommission` previously `remove_dir_all`'d
//! `workspace_path` unconditionally, which deleted a live user repo when the
//! #1502 local-path spawn set `workspace_path` to a real on-disk directory.
//! This module provides the belt-and-suspenders containment guard that prevents
//! any path OUTSIDE the SM's managed workspace root from being deleted —
//! regardless of the `workspace_owned` flag.
//!
//! Containment alone is NOT enough (#3764). Every SM worktree is inside the
//! managed root, so [`is_safe_to_remove`] happily green-lights removing a
//! SIBLING session's live worktree — it only ever asked "is this path mine to
//! manage?", never "is this path SOMEONE ELSE's?". The observed precursor to
//! three separate worktree-destruction incidents was a #1744 cwd collision in
//! which THREE Active session records pointed at ONE worktree path; any
//! decommission of the two impostor records would have taken the real owner's
//! live tree with it, with containment passing cleanly. [`foreign_owner`]
//! adds the missing identity half.
//!
//! What: [`is_safe_to_remove`] canonicalizes both paths and verifies that the
//! workspace is strictly INSIDE the managed root, rejecting: path == root, path
//! outside root, paths with too few components, and `$HOME`.
//! [`foreign_owner`] is the pure decision half of the identity guard:
//! given the owner DECLARED BY THE WORKTREE ITSELF (its on-disk ownership
//! sentinel — not the possibly-corrupt session record that asked for the
//! removal) it reports whether that owner is a different, still-Active session.
//! Test: `is_safe_to_remove_*` and `foreign_owner_*` unit tests below.

use std::path::Path;

use tracing::warn;

use super::record::ManagedSessionId;

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

/// Identify a worktree that belongs to a DIFFERENT session which is NOT provably
/// ownerless (#3764 item 1) — the pure decision half of the cross-session
/// deletion guard.
///
/// Why: [`is_safe_to_remove`] answers "is this path inside the managed root?"
/// and nothing else, so every cross-session deletion it is asked about passes.
/// The #3649 owner gate closes part of the hole but only fires when a SESSION
/// identifies itself as `caller`; every daemon-routed remove path in the tree
/// (`daemon/mcp_session.rs`, `daemon/sm_stdio/control.rs`, the HTTP routes, the
/// idle reaper, `dedup`) passes `caller: None` and therefore skips it entirely.
/// That leaves the exact incident shape unguarded: a corrupt/colliding record
/// whose `workspace_path` points at a PEER's worktree gets decommissioned, and
/// the peer's tree is destroyed under it. This guard is deliberately
/// independent of `caller`, so it holds even for an operator-authority
/// (`caller: None`) removal.
///
/// **Reclaimability is decided by [`SessionManager::resolve_ownerless`], NOT by
/// `state == Active` (code-critic HIGH-1 on the #3764 PR).** The first draft of
/// this guard refused only for an `Active` owner, which made it strictly WEAKER
/// than the #3649 gate sitting twenty lines earlier in the same function — that
/// gate refuses whenever `!resolve_ownerless(owner)`, which is `false` for
/// `Stopped`/`Errored`/`Provisioning`. The result was backwards: `caller: Some`
/// protected a Stopped peer's worktree while `caller: None` — every daemon path
/// this guard exists to cover — happily deleted it. The reachable sequence:
/// `idle_reaper` stops peer P, later reaps colliding record I via
/// `decommission(&I, None)`, the sentinel names P, and P's RESUMABLE tree is
/// destroyed. A Stopped session is precisely the one whose worktree must
/// survive — it is going to be resumed. `prune.rs`'s adjacent orphan sweep
/// already uses `resolve_ownerless_with_grace` for this same question, so
/// anything else would be a third, disagreeing notion of "reclaimable".
///
/// What: returns `Some(owner)` ONLY when all three hold — (a) an owner is known
/// at all, (b) that owner is not `target` (the session whose teardown is
/// running), and (c) that owner is NOT provably ownerless. Every other case
/// returns `None` (removal proceeds), deliberately:
///   * owner unknown (`None` — legacy pre-#3649 worktree, or an unparsable
///     sentinel) → no evidence of a peer; preserves backward compatibility.
///     The store-side companion check in
///     [`SessionManager::foreign_active_worktree_owner`] covers this case
///     without needing any on-disk file.
///   * owner == target → the session is removing its OWN worktree; the normal case.
///   * owner provably ownerless (terminal `Decommissioned`/`Deleted`, or absent
///     from the store entirely) → #3649's orphan-GC and every existing reclaim
///     path keep working exactly as before. This guard can only ever REFUSE a
///     deletion the tree previously allowed; it never permits a new one.
///
/// Test: `foreign_owner_blocks_live_peer`, `foreign_owner_blocks_stopped_peer`,
/// `foreign_owner_allows_self`, `foreign_owner_allows_unknown_owner`,
/// `foreign_owner_allows_ownerless_peer` in this module; the wired-in behaviour
/// is covered by `decommission_refuses_to_delete_live_peer_worktree` and
/// `decommission_refuses_to_delete_stopped_peer_worktree` in
/// `super::worktree_identity_guard_tests`.
pub(crate) fn foreign_owner(
    declared_owner: Option<ManagedSessionId>,
    target: ManagedSessionId,
    owner_is_ownerless: bool,
) -> Option<ManagedSessionId> {
    let owner = declared_owner?;
    if owner == target {
        return None;
    }
    if owner_is_ownerless {
        return None;
    }
    Some(owner)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ManagedSessionId, foreign_owner, is_safe_to_remove};

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

    // ── foreign_owner identity guard (#3764 item 1) ─────────────────────────
    //
    // Reclaimability here is `resolve_ownerless`'s answer, passed in as a bool
    // (see the function doc for why `state == Active` was wrong). The mapping
    // these tests encode, matching `resolve_ownerless`:
    //   Active / Stopped / Errored / Provisioning -> NOT ownerless -> REFUSE
    //   Decommissioned / Deleted / absent record  -> ownerless     -> ALLOW

    /// A worktree owned by a DIFFERENT, still-live session is refused.
    ///
    /// Why: this IS the incident shape — a corrupt/colliding record whose
    /// `workspace_path` points at a live peer's worktree. Before #3764 the
    /// containment guard passed and the peer's tree was destroyed.
    /// Test: this function IS the test.
    #[test]
    fn foreign_owner_blocks_live_peer() {
        let peer = ManagedSessionId::new();
        let target = ManagedSessionId::new();
        assert_eq!(
            foreign_owner(Some(peer), target, false),
            Some(peer),
            "a live peer's worktree must be refused"
        );
    }

    /// A STOPPED peer's worktree is refused too (code-critic HIGH-1).
    ///
    /// Why: a Stopped session is resumable — its worktree is exactly the one
    /// that must survive, because it is going to be resumed. The first draft
    /// refused only for `Active`, which made this guard weaker than the #3649
    /// gate beside it and let `idle_reaper`'s `caller: None` reap destroy a
    /// stopped peer's tree. `resolve_ownerless` returns `false` for every
    /// live/resumable state, so all of them land here.
    /// Test: this function IS the test.
    #[test]
    fn foreign_owner_blocks_stopped_peer() {
        let peer = ManagedSessionId::new();
        let target = ManagedSessionId::new();
        // `resolve_ownerless` == false for Stopped/Errored/Provisioning alike.
        assert_eq!(
            foreign_owner(Some(peer), target, false),
            Some(peer),
            "a resumable (Stopped/Errored/Provisioning) peer's worktree must be refused"
        );
    }

    /// A session removing its OWN worktree is allowed — the normal case.
    ///
    /// Why: the guard must not break every legitimate decommission. Checked
    /// even when the owner is NOT ownerless, since a live session tearing down
    /// its own workspace is the ordinary path.
    /// Test: this function IS the test.
    #[test]
    fn foreign_owner_allows_self() {
        let id = ManagedSessionId::new();
        assert_eq!(
            foreign_owner(Some(id), id, false),
            None,
            "a session must always be able to remove its own worktree"
        );
    }

    /// An owner-unknown worktree is allowed by THIS predicate.
    ///
    /// Why: pre-#3649 worktrees carry a zero-byte sentinel with no owner, so
    /// the sentinel half must stay backward-compatible and never block on
    /// absent evidence. The store-side companion check in
    /// `foreign_active_worktree_owner` is what covers this case — see
    /// `decommission_refuses_when_store_shows_live_peer_without_sentinel`.
    /// Test: this function IS the test.
    #[test]
    fn foreign_owner_allows_unknown_owner() {
        assert_eq!(
            foreign_owner(None, ManagedSessionId::new(), false),
            None,
            "an owner-unknown worktree must not be blocked by the sentinel half"
        );
    }

    /// A provably-ownerless peer does NOT block removal.
    ///
    /// Why: this is what keeps #3649's orphan-GC and every existing reclaim
    /// path working unchanged — a terminal (`Decommissioned`/`Deleted`) or
    /// absent owner is reclaimable, and `tm sessions delete <stale-id>` is
    /// therefore the operator's escape hatch when a sentinel goes stale.
    /// Test: this function IS the test.
    #[test]
    fn foreign_owner_allows_ownerless_peer() {
        assert_eq!(
            foreign_owner(Some(ManagedSessionId::new()), ManagedSessionId::new(), true),
            None,
            "a provably-ownerless peer must not block removal"
        );
    }
}
