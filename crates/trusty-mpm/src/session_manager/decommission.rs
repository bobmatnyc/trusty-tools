//! Decommission and workspace-ownership methods for the session manager (#1511).
//!
//! Why: `decommission` and the companion `set_workspace_owned` are extracted
//! from `manager.rs` to keep that file under the 500-SLOC production cap,
//! mirroring the pattern used by `adopt.rs` and `prune.rs`. The decommission
//! logic is also a natural home for the ownership-tracking primitive.
//! What: public [`SessionManager::decommission`] (full teardown with the #1511
//! dual guard), internal [`SessionManager::decommission_with_root`] (injectable
//! managed-root for test isolation without env mutation), and
//! [`SessionManager::set_workspace_owned`] (marks a workspace as SM-provisioned).
//! Test: `manager_decommission_removes_workspace`,
//! `manager_decommission_unowned_skips_deletion`,
//! `workspace_owned_flag_round_trips_via_set` in `super::tests`.

use std::path::Path;

use tracing::{info, warn};

use crate::core::trusty_tools_config::{TrustyToolsConfig, workspace_root};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::workspace_guard::is_safe_to_remove;

/// True when `path` is an SM-created per-session git worktree (#1840).
///
/// Why: in-project sessions create their workspace at
/// `<base>/.worktrees/<session-id>/` with `workspace_owned = false` — they do
/// NOT own the base clone, but they DO own their worktree slice. The standard
/// `workspace_owned` guard therefore skips removal entirely, leaving orphaned
/// worktree directories and stale git worktree refs. This predicate identifies
/// the SM-worktree pattern so decommission can take targeted worktree-removal
/// action for `workspace_owned = false` sessions.
/// What: returns `true` when the path's immediate parent directory is named
/// `.worktrees` — i.e. the path is `<base>/.worktrees/<session-id>`. Checking
/// only the immediate parent (not any ancestor) prevents false positives for
/// paths like `<base>/.worktrees/deep/nested` where `.worktrees` is a grandparent.
/// Test: `is_session_worktree_detects_dot_worktrees_component`.
fn is_session_worktree(path: &Path) -> bool {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n == ".worktrees")
        .unwrap_or(false)
}

/// Remove an in-project per-session git worktree via `git worktree remove --force`.
///
/// Why (#1840): `remove_dir_all` alone leaves the git ref (`session/<id>`) and
/// the git worktree entry (`.git/worktrees/<id>`) in the base clone, polluting
/// `git worktree list` and `git branch` output. `git worktree remove --force`
/// prunes both the directory AND the ref atomically, restoring a clean state.
/// What: runs `git -C <repo-root> worktree remove --force <path>` where
/// `<repo-root>` is the grandparent of the worktree directory
/// (`path` → `.worktrees` → `<repo-root>`). Also runs
/// `git -C <repo-root> worktree prune` on success to clear any stale git refs.
/// Idempotent: if `path` is already absent, returns `false` immediately. On git
/// failure, best-effort falls back to `remove_dir_all` and logs WARN.
/// Returns `true` when the workspace was removed (either via git or fallback),
/// `false` when it was already absent or all removal attempts failed.
/// Test: `is_session_worktree_absent_path_is_noop`; integration coverage via
/// the decommission round-trip tests that set up real git worktrees.
pub(super) fn remove_session_worktree(path: &Path) -> bool {
    if !path.exists() {
        return false; // Already gone, idempotent.
    }
    // The repo root is the grandparent of the worktree dir:
    // <repo-root>/.worktrees/<session-id>/ → grandparent = <repo-root>
    let repo_root = match path.parent().and_then(|p| p.parent()) {
        Some(r) => r,
        None => {
            warn!(
                path = %path.display(),
                "decommission: cannot determine repo root from worktree path — skipping git removal"
            );
            return false;
        }
    };
    // Step 1: git worktree remove --force <path> (run from repo root)
    let out = std::process::Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "worktree",
            "remove",
            "--force",
        ])
        .arg(path)
        .output();
    let git_success = match out {
        Ok(o) if o.status.success() => {
            info!(path = %path.display(), "decommission: git worktree removed (incl. ref)");
            true
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(
                path = %path.display(),
                "decommission: git worktree remove --force failed ({}): {stderr}; \
                 falling back to remove_dir_all",
                o.status
            );
            if let Err(e) = std::fs::remove_dir_all(path) {
                warn!(path = %path.display(), "decommission: fallback remove_dir_all failed: {e}");
                return false;
            }
            false // git remove failed, but dir removal succeeded
        }
        Err(e) => {
            warn!(
                path = %path.display(),
                "decommission: failed to spawn git for worktree removal: {e}; \
                 falling back to remove_dir_all"
            );
            if let Err(e2) = std::fs::remove_dir_all(path) {
                warn!(path = %path.display(), "decommission: fallback remove_dir_all also failed: {e2}");
                return false;
            }
            false
        }
    };
    // Step 2: git worktree prune to clear any stale git refs.
    // Best-effort; a failure here is a minor annoyance (stale ref in git output)
    // not a correctness failure.
    if git_success {
        let prune_out = std::process::Command::new("git")
            .args(["-C", &repo_root.to_string_lossy(), "worktree", "prune"])
            .output();
        if let Err(e) = prune_out {
            warn!(root = %repo_root.display(), "decommission: git worktree prune failed: {e}");
        }
    }
    true // workspace was removed (either via git or fallback)
}

impl SessionManager {
    /// Decommission a session: stop the runtime, remove the workspace from disk
    /// (ONLY if the SM provisioned it), and mark the record `Decommissioned`.
    ///
    /// Why: the only full teardown operation. Unlike `stop`, this removes the
    /// workspace directory from disk so no future `resume` is possible. A
    /// tombstone record is kept in the store so `ls` can show history.
    ///
    /// Safety (#1511): `remove_dir_all` is executed ONLY when BOTH conditions hold:
    /// (a) `record.workspace_owned == true` — the SM provisioned (cloned) the
    ///     directory and is the rightful owner; local-path spawn (#1502) and
    ///     `adopt_existing` (#1433) leave `workspace_owned = false` so they are
    ///     NEVER deleted by this path.
    /// (b) `is_safe_to_remove(workspace_path, managed_root)` — the canonicalized
    ///     path is strictly INSIDE the SM's managed workspace root, rejecting any
    ///     path outside it (including `$HOME`, volume roots, and paths with too
    ///     few components). This belt-and-suspenders guard catches stale/incorrect
    ///     `workspace_owned` flags before disk mutation occurs.
    ///
    /// #1840 worktree extension: even when `workspace_owned = false`, if the
    /// workspace path is under a `.worktrees/` directory (an in-project per-session
    /// worktree), the worktree IS removed via `git worktree remove --force`. The
    /// base clone (`<base>/`) is NEVER touched — only the per-session leaf dir.
    ///
    /// When deletion is skipped (unowned non-worktree or unsafe path), decommission
    /// still transitions the record to `Decommissioned` and returns successfully.
    ///
    /// What: delegates to [`decommission_with_root`](Self::decommission_with_root)
    /// with the config-derived managed root so callers remain env-agnostic.
    /// Test: `manager_decommission_removes_workspace` — asserts the workspace dir
    /// is gone from disk and the record state is `Decommissioned`.
    /// `manager_decommission_unowned_skips_deletion` — asserts that decommissioning
    /// a local-path/adopt record does NOT delete the directory.
    pub async fn decommission(
        &self,
        id: &ManagedSessionId,
    ) -> Result<(SessionRecord, bool), ManagedError> {
        let config = TrustyToolsConfig::load();
        let managed_root = workspace_root(&config);
        self.decommission_with_root(id, &managed_root).await
    }

    /// Internal: decommission with an explicit managed root (test seam).
    ///
    /// Why: tests need to inject a temp directory as the managed root to keep the
    /// containment guard working without mutating process-global env vars
    /// (`TRUSTY_MPM_WORKSPACE_ROOT`). Env mutation is thread-unsafe and pollutes
    /// parallel tests; injecting the root avoids that entirely.
    /// What: identical teardown logic as the public `decommission` but resolves the
    /// managed root from the caller-supplied `managed_root` instead of the config.
    /// Returns `(SessionRecord, workspace_removed)` where `workspace_removed` is
    /// `true` ONLY when `remove_dir_all` actually ran — callers must not infer this
    /// from a post-call filesystem check (TOCTOU: owned workspace already absent
    /// before decommission would give a false-positive filesystem result).
    /// Test: called by `manager_decommission_removes_workspace` (which passes a
    /// TempDir as the managed root, removing the need for `set_var`).
    pub(crate) async fn decommission_with_root(
        &self,
        id: &ManagedSessionId,
        managed_root: &Path,
    ) -> Result<(SessionRecord, bool), ManagedError> {
        let mut record = self.get(id).await?;

        // Kill the runtime (best-effort).
        if self.tmux.session_exists(&record.tmux_name)
            && let Err(e) = self.tmux.kill_session(&record.tmux_name)
        {
            warn!(name = %record.tmux_name, "decommission: kill_session failed: {e}");
        }

        // Guard: only remove the workspace directory if the SM provisioned it.
        // Track whether remove_dir_all ACTUALLY RAN (not inferred from filesystem).
        let mut workspace_removed = false;
        if let Some(ref ws) = record.workspace_path {
            if !record.workspace_owned {
                // Unowned workspace (local-path spawn or adopt): never bulk-delete.
                // #1840: EXCEPTION — in-project per-session worktrees live under
                // .worktrees/ and must be cleaned up via `git worktree remove` so
                // the git ref is also pruned.  The base clone directory is NEVER
                // touched — only the leaf worktree path.
                if is_session_worktree(ws) {
                    workspace_removed = remove_session_worktree(ws);
                } else {
                    warn!(
                        id = %id,
                        workspace = %ws.display(),
                        "decommission: skipping workspace removal — not SM-owned \
                         (local-path or adopted session); the directory was NOT \
                         created by the session manager"
                    );
                }
            } else {
                // Owned workspace: check existence first so a path that is
                // already gone is not misreported as a containment failure.
                if !ws.exists() {
                    // Benign: the workspace was removed before decommission ran
                    // (e.g. a prior partial teardown). The tombstone is still
                    // written below; no further disk action is needed.
                    // workspace_removed stays false — we did NOT remove it.
                    tracing::debug!(
                        id = %id,
                        workspace = %ws.display(),
                        "decommission: owned workspace already absent — skipping removal"
                    );
                } else {
                    // Workspace exists: apply the belt-and-suspenders
                    // path-containment guard before touching the filesystem.
                    // Only paths that exist but are OUTSIDE the managed root
                    // (or are otherwise unsafe) reach this warning.
                    if !is_safe_to_remove(ws, managed_root) {
                        warn!(
                            id = %id,
                            workspace = %ws.display(),
                            root = %managed_root.display(),
                            "decommission: skipping workspace removal — path fails \
                             containment guard (outside managed root or unsafe path)"
                        );
                    } else {
                        std::fs::remove_dir_all(ws).map_err(|e| {
                            ManagedError::Io(std::io::Error::new(
                                e.kind(),
                                format!("remove workspace {:?}: {e}", ws),
                            ))
                        })?;
                        workspace_removed = true;
                        info!(
                            id = %id,
                            workspace = %ws.display(),
                            "decommission: owned workspace removed from disk"
                        );
                    }
                }
            }
        }

        // Tombstone: clear workspace_path, mark Decommissioned, persist.
        record.workspace_path = None;
        record.workspace_owned = false;
        record.state = ManagedSessionState::Decommissioned;
        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %record.tmux_name, "managed session decommissioned");
        Ok((record, workspace_removed))
    }

    /// Mark a session's workspace as SM-owned (provisioned by clone) or unowned.
    ///
    /// Why (#1511): the decommission path must know whether the SM provisioned the
    /// `workspace_path` (and therefore may `remove_dir_all` it) or whether the path
    /// is a real, pre-existing user directory (local-path spawn, adopt) that must
    /// NEVER be deleted. Setting `workspace_owned = true` is the explicit assertion
    /// that this SM cloned the workspace; `false` (the serde default) means "do not
    /// touch this directory on decommission." Callers that use the local-path spawn
    /// or `adopt_existing` path MUST NOT call this method (or call it with `false`).
    /// What: looks up the record, sets `workspace_owned`, and persists.
    /// Test: `workspace_owned_flag_round_trips_via_set` + the decommission guard
    /// tests in `session_manager/tests.rs`.
    pub async fn set_workspace_owned(
        &self,
        id: &ManagedSessionId,
        owned: bool,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.workspace_owned = owned;
        self.store.write().await.upsert(record).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_session_worktree_detects_dot_worktrees_component() {
        // Why (#1840): decommission must detect the .worktrees/ pattern to know
        // when to call git worktree remove even for workspace_owned=false sessions.
        // Checks the IMMEDIATE parent — not any ancestor — to avoid false positives.
        // parent is `.worktrees` → true
        assert!(is_session_worktree(std::path::Path::new(
            "/home/user/repo/.worktrees/session-abc"
        )));
        assert!(is_session_worktree(std::path::Path::new(
            "/some/base/.worktrees/session-id"
        )));
        // parent is `repo` (not `.worktrees`) → false
        assert!(!is_session_worktree(std::path::Path::new(
            "/home/user/repo/session-id"
        )));
        // parent is `deep` (not `.worktrees`), even though `.worktrees` is an ancestor → false
        assert!(!is_session_worktree(std::path::Path::new(
            "/base/.worktrees/deep/path"
        )));
        // parent is `worktrees` (no dot-prefix) → false
        assert!(!is_session_worktree(std::path::Path::new(
            "/base/worktrees/session"
        )));
    }

    #[test]
    fn is_session_worktree_absent_path_is_noop() {
        // remove_session_worktree must return false without panicking when path is absent.
        let absent = std::path::Path::new("/nonexistent/.worktrees/session-abc");
        // is_session_worktree: true (immediate parent is `.worktrees`)
        assert!(is_session_worktree(absent));
        // remove_session_worktree: returns false idempotently (path.exists() == false)
        let result = remove_session_worktree(absent);
        assert!(!result, "absent path must return false (already gone)");
    }
}
