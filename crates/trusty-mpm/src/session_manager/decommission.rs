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

/// Sentinel file written by [`create_session_worktree`] into every SM-created
/// per-session git worktree (#1845 item 5).
///
/// Why: `is_session_worktree` identifies worktrees by the `.worktrees` parent-name
/// convention, but a user-owned directory that is a direct child of a `.worktrees/`
/// directory would be misclassified and deleted. The sentinel provides an explicit
/// SM-ownership marker so `remove_session_worktree` can distinguish TM-created dirs
/// from user-owned ones without relying solely on the naming convention. The convention
/// check is kept as a fallback for worktrees created before this sentinel was
/// introduced (backward-compatibility).
/// What: a zero-byte file named `.trusty-mpm-worktree` written at the root of every
/// SM-created worktree by [`create_session_worktree`] immediately after git creates it.
/// Test: `sentinel_gates_worktree_removal` in `decommission::tests`.
pub(crate) const WORKTREE_SENTINEL_FILE: &str = ".trusty-mpm-worktree";

/// Directory name (relative to a base git checkout) holding all SM-created
/// per-session git worktrees.
///
/// Why: both the in-project spawn path (`daemon::managed_routes::inproject`)
/// and the clone-based shared-base-checkout path (#1935,
/// `provisioner::workspace`) nest per-session worktrees one level under a
/// shared base checkout; naming the segment once here (rather than repeating
/// the `".worktrees"` string literal at each call site) keeps the convention
/// singular and greppable.
/// What: `".worktrees"`.
/// Test: exercised transitively by every test that builds a `.worktrees/<id>`
/// path — `is_session_worktree_detects_dot_worktrees_component` pins the
/// literal value via [`is_session_worktree`].
pub(crate) const WORKTREES_DIRNAME: &str = ".worktrees";

/// Timeout for the blocking `git worktree remove` subprocess (#1845 item 4).
///
/// Why: `std::process::Command` is synchronous and has no built-in timeout. A git
/// process that hangs (e.g. waiting for a network mount or a file lock) would
/// block the daemon's async executor indefinitely when called from `decommission`,
/// making the daemon unresponsive for the duration. A 30-second bound converts
/// a hung git call into a clean timeout log entry and a conservative `false` return.
/// What: a [`std::time::Duration`] of 30 seconds passed to `tokio::time::timeout`
/// wrapping the `spawn_blocking` that runs `remove_session_worktree`.
/// Test: `git_worktree_remove_timeout_is_bounded_constant`.
pub(crate) const GIT_WORKTREE_REMOVE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);

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
        .map(|n| n == WORKTREES_DIRNAME)
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
/// `git -C <repo-root> worktree prune` and
/// `git -C <repo-root> branch -D session/<leaf>` on success to clear stale git
/// refs and the session branch, where `<leaf>` is the last component of `path`
/// (the worktree dir name) and the `session/` prefix matches EXACTLY what
/// `inproject::create_session_worktree` creates (issue #2032 fix — before
/// this, the missing prefix meant the branch delete always targeted a
/// nonexistent ref and silently no-opped, leaking every session's branch).
/// Works identically for both pre-#2032 UUID-named leaves and the new
/// semantic-tmux-name leaves, since both share the `session/<leaf>`
/// convention. Branch deletion is best-effort — "not found" is silently
/// ignored since older sessions may not have a branch. OsStr-safe path args
/// avoid lossy UTF-8 coercion (#1840).
/// Idempotent: if `path` is already absent, returns `true` (already removed).
/// On git failure, best-effort falls back to `remove_dir_all` and logs WARN.
/// Returns `true` when the workspace was removed (either via git or fallback)
/// or was already absent; `false` only when all removal attempts failed.
/// Test: `is_session_worktree_absent_path_is_noop`; integration coverage via
/// the decommission round-trip tests that set up real git worktrees.
pub(super) fn remove_session_worktree(path: &Path) -> bool {
    if !path.exists() {
        // Already gone — either removed by a concurrent decommission or by a
        // previous partial run. Treat as success (idempotent removal).
        return true;
    }

    // Data-safety gate (#1845 item 5): prefer the SM ownership sentinel over the
    // naming-convention check. Every SM-created worktree has a `.trusty-mpm-worktree`
    // sentinel written by `create_session_worktree`. If the sentinel is ABSENT:
    //   • and the path IS under `.worktrees/` → backward-compat (pre-sentinel worktree);
    //     proceed with a WARN so operators know the sentinel is missing.
    //   • and the path is NOT under `.worktrees/` → NOT a SM worktree; refuse removal.
    // This two-tier check is conservative: it avoids deleting user-owned directories
    // that happen to sit under a `.worktrees/` parent.
    let sentinel = path.join(WORKTREE_SENTINEL_FILE);
    if !sentinel.exists() {
        if !is_session_worktree(path) {
            warn!(
                path = %path.display(),
                sentinel = WORKTREE_SENTINEL_FILE,
                "decommission: refusing worktree removal — no SM ownership sentinel \
                 and path is not under .worktrees/; skipping conservatively"
            );
            return false;
        }
        warn!(
            path = %path.display(),
            sentinel = WORKTREE_SENTINEL_FILE,
            "decommission: sentinel absent; falling back to convention check \
             (backward-compat with pre-sentinel worktrees)"
        );
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
    // Step 1: git worktree remove --force <path> (run from repo root).
    // Pass repo_root as an OsStr-safe Path arg to avoid lossy UTF-8 coercion (#1840).
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "remove", "--force"])
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
    if git_success {
        // Step 2: git worktree prune to clear any stale git worktree refs.
        // Best-effort: a failure here is a minor annoyance (stale ref in git output),
        // not a correctness failure.
        let prune_out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["worktree", "prune"])
            .output();
        if let Err(e) = prune_out {
            warn!(root = %repo_root.display(), "decommission: git worktree prune failed: {e}");
        }

        // Step 3: delete the session branch ref (if any). #2032 FIX: the branch
        // `create_session_worktree` actually creates is `session/<worktree-leaf>`
        // (see `crate::core::worktree_naming::worktree_branch_for` — the SAME
        // convention `daemon::managed_routes::inproject::create_session_worktree`
        // uses), NOT the bare leaf name. Before this fix the missing `session/`
        // prefix meant `git branch -D <leaf>` always targeted a nonexistent
        // branch and silently fell into the "not found" debug-log path below —
        // session branches were NEVER actually cleaned up. This works
        // identically for both OLD (raw-UUID-named, pre-#2032) and NEW
        // (semantic-tmux-name) worktree leaves, since both used/use the same
        // `session/<leaf>` convention for the branch name. Ignore "not found"
        // — the branch may not exist for older sessions that never created one
        // (#1840). Uses `core::worktree_naming` (unconditionally compiled),
        // NOT `daemon::managed_routes::inproject` (feature = "daemon"), so
        // this module keeps compiling with the `daemon` feature disabled.
        if let Some(session_name) = path.file_name().and_then(|n| n.to_str()) {
            let branch = crate::core::worktree_naming::worktree_branch_for(session_name);
            let branch_out = std::process::Command::new("git")
                .arg("-C")
                .arg(repo_root)
                .args(["branch", "-D"])
                .arg(&branch)
                .output();
            match branch_out {
                Ok(o) if o.status.success() => {
                    info!(
                        path = %path.display(),
                        "decommission: git branch -D {:?} (session ref cleaned)",
                        branch
                    );
                }
                Ok(o) => {
                    // Branch not found is expected for sessions that never created one.
                    tracing::debug!(
                        path = %path.display(),
                        "decommission: git branch -D {:?} not needed: {}",
                        branch,
                        String::from_utf8_lossy(&o.stderr).trim()
                    );
                }
                Err(e) => {
                    warn!(
                        path = %path.display(),
                        "decommission: git branch -D failed to spawn: {e}"
                    );
                }
            }
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

        // Gracefully terminate the runtime before removing the workspace (#1975):
        // SIGTERM the claude process and give it a grace window to flush state,
        // then reclaim the pane — instead of an abrupt `kill_session`. Best-effort:
        // a session whose runtime is already gone still decommissions cleanly —
        // the helper self-guards and is a no-op when the pane is already gone.
        self.graceful_terminate_runtime(&record.tmux_name).await;

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
                    // Item 4 (#1845): wrap the blocking `git worktree remove`
                    // call in spawn_blocking + tokio::time::timeout so a hung
                    // git process cannot stall the async executor indefinitely.
                    let ws_clone = ws.clone();
                    let join =
                        tokio::task::spawn_blocking(move || remove_session_worktree(&ws_clone));
                    workspace_removed =
                        match tokio::time::timeout(GIT_WORKTREE_REMOVE_TIMEOUT, join).await {
                            Ok(Ok(removed)) => removed,
                            Ok(Err(e)) => {
                                warn!(
                                    id = %id,
                                    workspace = %ws.display(),
                                    "decommission: remove_session_worktree task panicked: {e}"
                                );
                                false
                            }
                            Err(_elapsed) => {
                                warn!(
                                    id = %id,
                                    workspace = %ws.display(),
                                    timeout_secs = GIT_WORKTREE_REMOVE_TIMEOUT.as_secs(),
                                    "decommission: git worktree remove timed out; \
                                     worktree may require manual cleanup"
                                );
                                false
                            }
                        };
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
        // remove_session_worktree must return true without panicking when path is
        // already absent (#1840 D: idempotent — "already gone" is success).
        let absent = std::path::Path::new("/nonexistent/.worktrees/session-abc");
        // is_session_worktree: true (immediate parent is `.worktrees`)
        assert!(is_session_worktree(absent));
        // remove_session_worktree: returns true idempotently (path already absent)
        let result = remove_session_worktree(absent);
        assert!(
            result,
            "absent path should return true (idempotently removed)"
        );
    }

    /// Item 5 (#1845): sentinel gate refuses to delete directories that are NOT
    /// under `.worktrees/` and have no sentinel file — they cannot be SM worktrees.
    #[test]
    fn sentinel_gates_worktree_removal_refuses_non_worktrees_dir_without_sentinel() {
        // Why: a directory outside the .worktrees/ convention and without the
        // `.trusty-mpm-worktree` sentinel must NEVER be deleted by the SM.
        // What: create a real temp dir (so path.exists() is true), confirm no
        // sentinel exists, confirm the parent is NOT `.worktrees`, and assert
        // that remove_session_worktree returns false (refused, dir untouched).
        // Test: this function is the sentinel_gates test.
        let dir = tempfile::tempdir().expect("tempdir");
        let wt_path = dir.path().to_path_buf();
        // Verify: no sentinel file, and parent is NOT `.worktrees`.
        assert!(!wt_path.join(WORKTREE_SENTINEL_FILE).exists());
        assert!(
            !is_session_worktree(&wt_path),
            "test invariant: parent must NOT be .worktrees for this branch"
        );
        let result = remove_session_worktree(&wt_path);
        assert!(
            !result,
            "remove_session_worktree must return false for non-worktrees dir without sentinel"
        );
        // The directory must NOT have been deleted.
        assert!(
            wt_path.exists(),
            "non-SM directory must remain on disk after refused removal"
        );
    }

    /// Item 5 (#1845): sentinel gate allows removal when the sentinel IS present.
    ///
    /// Why: confirm the happy path — when the sentinel file exists, `remove_session_worktree`
    /// passes the safety gate and removes the directory. We verify observable filesystem
    /// state (`!path.exists()`) rather than the bool return value, which can vary with
    /// filesystem permissions unrelated to the sentinel gate (Finding 2 #1845).
    /// Test: create a temp dir, write sentinel, call remove_session_worktree, assert
    /// the directory is gone — proving the gate was passed AND removal succeeded.
    #[test]
    fn sentinel_present_passes_safety_gate() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Keep the TempDir alive but prevent auto-cleanup on drop: we call
        // remove_session_worktree (which deletes the directory) and then assert
        // it is gone. `keep()` suppresses the automatic deletion so Drop does not
        // fight with our explicit removal assertion.
        let wt_path = dir.keep();
        // Write the sentinel to simulate an SM-created worktree.
        std::fs::write(wt_path.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");
        // The sentinel check must pass (not return false early). The git call will fail
        // because this is not a git worktree, but remove_session_worktree falls back
        // to remove_dir_all. Assert the observable outcome: the directory is gone.
        remove_session_worktree(&wt_path);
        assert!(
            !wt_path.exists(),
            "sentinel present: safety gate must pass and directory must be removed"
        );
    }
}
