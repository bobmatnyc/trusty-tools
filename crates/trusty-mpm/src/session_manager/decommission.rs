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

use chrono::Utc;

use tracing::{info, warn};

use crate::core::trusty_tools_config::{TrustyToolsConfig, workspace_root};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::search_gc;
use super::workspace_guard::is_safe_to_remove;
use super::worktree_protection;
use super::worktree_registry;
use super::worktree_safety::{DirtyWorktree, inspect_dirt, worktree_remove_command};

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
/// Test: `sentinel_gates_worktree_removal` in the sibling `decommission_tests`.
pub(crate) const WORKTREE_SENTINEL_FILE: &str = ".trusty-mpm-worktree";

/// Directory name (relative to a base git checkout) under which NEW SM-created
/// per-session git worktrees are created.
///
/// Why: both the in-project spawn path (`daemon::managed_routes::inproject`)
/// and the clone-based shared-base-checkout path (#1935,
/// `provisioner::workspace`) nest per-session worktrees one level under a
/// shared base checkout; naming the segment once (rather than repeating the
/// `".worktrees"` string literal at each call site) keeps the convention
/// singular and greppable. #5204 turned that constant into a resolver so the
/// name is configurable — and so `trusty-search`'s indexing exclusion and
/// `trusty-memory`'s workstream attribution resolve the SAME value.
/// What: [`crate::core::trusty_tools_config::worktrees_dirname`] over the
/// loaded host config — **env > config > `.worktrees`**. Use this ONLY where a
/// worktree is being created or a path is being built; every "is this a
/// worktree base?" question goes to [`worktree_dir_names`] instead.
/// Test: `is_session_worktree_detects_dot_worktrees_component`,
/// `worktrees_dirname_delegates_to_the_shared_resolver`.
pub(crate) fn worktrees_dirname() -> String {
    crate::core::trusty_tools_config::worktrees_dirname(
        &crate::core::trusty_tools_config::TrustyToolsConfig::load(),
    )
}

/// The resolved worktree-base names for DETECTION — configured plus built-in.
///
/// Why: detection is deliberately a superset of creation (#5204). An operator
/// who retargets the base still has worktrees on disk under `.worktrees`; if
/// detection narrowed to the configured name alone, those would stop being
/// recognised as session worktrees — decommission would refuse to remove them,
/// prune would skip them, and trusty-search would start indexing every one.
/// What: a [`trusty_common::workspace_layout::WorktreeDirNames`] whose `matches`
/// accepts the configured name OR `.worktrees`.
/// Test: `is_session_worktree_detects_dot_worktrees_component`.
pub(crate) fn worktree_dir_names() -> trusty_common::workspace_layout::WorktreeDirNames {
    trusty_common::workspace_layout::WorktreeDirNames::from_configured(
        crate::core::trusty_tools_config::TrustyToolsConfig::load()
            .worktrees_dirname
            .as_deref(),
    )
}

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
///
/// Visibility (#2033, widened #2508): `pub(crate)` — reused by
/// `session_manager::search_gc` (same "disposable workspace?" rule for the
/// search-index lifecycle) and by `core::agent_reset_workspace` (the same
/// rule gates which session workspaces `--reset-agents-workspaces` is
/// allowed to force-recompose files into — a local-path/adopted session's
/// real, long-lived checkout must never be mistaken for a disposable
/// SM-provisioned one, the #1511 incident class). Keeping this in exactly one
/// place means the filesystem-removal guard, the search-index GC guard, and
/// the agent-reset guard can never diverge.
pub(crate) fn is_session_worktree(path: &Path) -> bool {
    // #5204: detection matches the configured base OR the built-in `.worktrees`,
    // so retargeting never orphans worktrees already on disk.
    is_session_worktree_with(path, &worktree_dir_names())
}

/// [`is_session_worktree`] against ALREADY-RESOLVED base names.
///
/// Why: [`worktree_dir_names`] calls `TrustyToolsConfig::load()`, which re-reads
/// and re-parses config files on every call and re-emits its unknown-key warning
/// (#5207). That is fine once, but a caller asking the question per item in a
/// loop pays it per item — and on a repeating timer it also multiplies that
/// one-shot warning into a per-item, per-tick log flood. `WorktreeDirNames`'s own
/// docs prescribe the remedy: resolve once at the top of a scan and pass the
/// value down (#5204). #5327's retention sweep is the first such loop caller.
/// What: the pure path-shape predicate — is the immediate parent a base name.
/// [`is_session_worktree`] is this function plus a per-call resolve.
/// Test: `is_session_worktree_detects_dot_worktrees_component` (through the
/// delegating wrapper), `workspace_needs_protection_covers_a_session_worktree`.
pub(crate) fn is_session_worktree_with(
    path: &Path,
    names: &trusty_common::workspace_layout::WorktreeDirNames,
) -> bool {
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(|n| names.matches(n))
        .unwrap_or(false)
}

/// What happened to a worktree the removal path was asked to delete (#4732).
///
/// Why: the caller has to be able to tell "gone" from "still on disk, and here
/// is why" — and it has to be told the reason, not left to find it in a `warn!`
/// nobody reads. The previous `bool` could express neither: `false` conflated a
/// deliberate refusal, an I/O error, and a timeout, and the reason existed only
/// as a log line inside the function.
/// What: [`Removed`](Self::Removed) — the directory is gone;
/// [`Kept`](Self::Kept) — it is still on disk, carrying an operator-facing
/// reason.
/// Test: `remove_session_worktree_refuses_a_git_locked_worktree`,
/// `remove_cleans_up_a_directory_no_repository_claims`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorktreeRemoval {
    /// The directory is gone — git removed it, it was already absent, or an
    /// unclaimed trusty-mpm-owned directory was removed directly.
    Removed,
    /// The directory is still on disk. The string says why, for the operator.
    Kept(String),
}

impl WorktreeRemoval {
    /// `true` only when the directory is gone.
    pub(super) fn removed(&self) -> bool {
        matches!(self, Self::Removed)
    }

    /// The operator-facing reason the directory was kept, if it was.
    pub(super) fn reason(&self) -> Option<&str> {
        match self {
            Self::Removed => None,
            Self::Kept(reason) => Some(reason),
        }
    }
}

/// Remove an in-project per-session git worktree via `git worktree remove --force`.
///
/// Why (#1840): `remove_dir_all` alone leaves the git ref (`session/<id>`) and
/// the git worktree entry (`.git/worktrees/<id>`) in the base clone, polluting
/// `git worktree list` and `git branch` output. `git worktree remove --force`
/// prunes both the directory AND the ref atomically, restoring a clean state.
/// What: runs `git -C <repo-root> worktree remove --force <path>` where
/// `<repo-root>` is the checkout git itself reports as owning `path`'s
/// worktree registry ([`super::worktree_registry::registry_root_for`], #4207 —
/// previously guessed as the grandparent directory). Also runs
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
/// Idempotent: if `path` is already absent, returns
/// [`Removed`](WorktreeRemoval::Removed).
///
/// # 🔴 Git declining is a REFUSAL, not a failure (#4732)
///
/// This function used to fall through to `std::fs::remove_dir_all` on ANY
/// non-zero exit, and on any failure to resolve the owning checkout. Git exits
/// 128 for every fatal condition, so that fall-through deleted the exact things
/// git was protecting — most sharply, `git worktree lock`, whose only mechanism
/// for saying "leave this alone" is a 128 exit. Locking a worktree to protect
/// it was what got it deleted. A stale worktree pointer and an unreadable
/// `.git` produced the same outcome while their working trees, uncommitted work
/// included, were entirely intact.
///
/// Every git failure is now classified by
/// [`super::worktree_protection`], which answers three states and refuses on
/// two of them. The `remove_dir_all` fallback survives for EXACTLY ONE case:
///
/// > `path` has passed the trusty-mpm ownership gate above, and git has
/// > POSITIVELY established that it holds no state there — the path carries no
/// > `.git` entry, and either no repository exists above it at all, or the
/// > owning repository's `git worktree list` does not name it.
///
/// That is a leftover directory: a worktree git already pruned, or one whose
/// creation never completed registration. There is no ref to prune and nothing
/// for git to protect, so a direct removal is the only way to clean it up.
/// Every other outcome — including "git could not be asked" — keeps the
/// directory and returns [`Kept`](WorktreeRemoval::Kept) with the reason.
///
/// Test: `is_session_worktree_absent_path_is_noop`,
/// `remove_session_worktree_refuses_a_git_locked_worktree`,
/// `remove_refuses_a_stale_worktree_pointer`,
/// `remove_refuses_an_unreadable_git_entry`,
/// `remove_refuses_a_worktree_with_a_broken_git_file`,
/// `remove_cleans_up_a_directory_no_repository_claims`,
/// `remove_cleans_up_an_unregistered_leftover_inside_a_repo`,
/// `remove_still_removes_a_healthy_worktree`; integration coverage via the
/// decommission round-trip tests that set up real git worktrees.
pub(super) fn remove_session_worktree(path: &Path) -> WorktreeRemoval {
    if !path.exists() {
        // Already gone — either removed by a concurrent decommission or by a
        // previous partial run. Treat as success (idempotent removal).
        return WorktreeRemoval::Removed;
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
            // #5204: name the configured base in the message the operator reads.
            return WorktreeRemoval::Kept(format!(
                "no trusty-mpm ownership sentinel ({WORKTREE_SENTINEL_FILE}) and the path \
                 is not under {}/",
                worktrees_dirname()
            ));
        }
        warn!(
            path = %path.display(),
            sentinel = WORKTREE_SENTINEL_FILE,
            "decommission: sentinel absent; falling back to convention check \
             (backward-compat with pre-sentinel worktrees)"
        );
    }

    // #4207: ask git which checkout owns this worktree's registry instead of
    // guessing that it is the grandparent directory. The grandparent rule held
    // only for the two shapes it was written against; a worktree registered to
    // the parent repo but living under `.base/.worktrees/` resolved to `.base`,
    // which disowns it, so `git worktree remove` there could never succeed.
    let repo_root = match super::worktree_registry::registry_root_for(path) {
        Some(r) => r,
        None => {
            // #4732: a `None` here is NOT "no git repository owns this path".
            // `registry_root_for` also returns `None` when git could not
            // resolve one — which is exactly what a worktree with a stale or
            // unreadable `.git` pointer answers, while its working tree and
            // uncommitted work are entirely intact. Classify before deleting.
            let verdict = worktree_protection::protection_without_registry_root(path);
            if let Some(reason) = verdict.refusal() {
                warn!(
                    path = %path.display(),
                    "decommission: refusing worktree removal — {reason} (#4732)"
                );
                return WorktreeRemoval::Kept(reason.to_string());
            }
            // The one surviving fallback case — see this function's doc.
            warn!(
                path = %path.display(),
                "decommission: no git repository claims this path — removing the \
                 directory directly (no worktree ref to prune)"
            );
            return remove_unclaimed_directory(path);
        }
    };
    let repo_root = repo_root.as_path();
    // Step 1: git worktree remove --force <path> (run from repo root).
    // #6391: through the hardened builder, which keeps the #1840 OsStr-safe
    // Path args and additionally strips the env vars that would point
    // `git worktree` at a different repository than `-C` names.
    let out = worktree_remove_command(repo_root, path).output();
    match out {
        Ok(o) if o.status.success() => {
            info!(path = %path.display(), "decommission: git worktree removed (incl. ref)");
            prune_refs_after_removal(repo_root, path);
            WorktreeRemoval::Removed
        }
        Ok(o) => {
            // #4732: git exits 128 for every fatal condition, and `git worktree
            // lock` uses that exit to REFUSE. Classify the reason instead of
            // reading every non-zero exit as permission to delete by hand.
            let stderr = String::from_utf8_lossy(&o.stderr);
            let verdict =
                worktree_protection::protection_after_failed_removal(path, repo_root, &stderr);
            if let Some(reason) = verdict.refusal() {
                warn!(
                    path = %path.display(),
                    "decommission: git worktree remove --force declined ({}) and the \
                     directory must be preserved — {reason} (#4732)",
                    o.status
                );
                return WorktreeRemoval::Kept(reason.to_string());
            }
            // The one surviving fallback case — see this function's doc.
            warn!(
                path = %path.display(),
                "decommission: git holds no worktree state at this path ({}: {stderr}); \
                 removing the leftover directory directly",
                o.status
            );
            remove_unclaimed_directory(path)
        }
        Err(e) => {
            // #4732: git could not be run at all, so nothing is known about
            // what it may be protecting. An unanswerable probe is never a
            // licence to delete.
            let reason = format!("git could not be run to remove the worktree: {e}");
            warn!(
                path = %path.display(),
                "decommission: refusing worktree removal — {reason} (#4732)"
            );
            WorktreeRemoval::Kept(reason)
        }
    }
}

/// Remove a directory git has POSITIVELY disclaimed (#4732).
///
/// Why: named so the one legitimate `remove_dir_all` in this module has exactly
/// one call shape and is greppable. Reaching it requires a
/// [`worktree_protection`] verdict of "no git state at or claiming this path" —
/// never a bare non-zero exit, and never an unanswerable probe. See
/// [`remove_session_worktree`]'s doc for the full statement of that case.
/// What: `std::fs::remove_dir_all`, mapped onto [`WorktreeRemoval`].
/// Test: `remove_cleans_up_a_directory_no_repository_claims`,
/// `remove_cleans_up_an_unregistered_leftover_inside_a_repo`.
fn remove_unclaimed_directory(path: &Path) -> WorktreeRemoval {
    match std::fs::remove_dir_all(path) {
        Ok(()) => WorktreeRemoval::Removed,
        Err(e) => {
            warn!(path = %path.display(), "decommission: remove_dir_all failed: {e}");
            WorktreeRemoval::Kept(format!("removing the directory failed: {e}"))
        }
    }
}

/// Clear the git refs a successful `git worktree remove` leaves behind (#1840,
/// #2032).
///
/// Why: split out of [`remove_session_worktree`] when #4732 turned that
/// function's tail into a three-way classification — the ref cleanup is
/// best-effort bookkeeping that runs only after git itself removed the
/// worktree, and interleaving it with the safety classification made both
/// harder to read.
/// What: `git worktree prune`, then `git branch -D session/<leaf>`. Both are
/// best-effort; a failure leaves a stale ref in git's output, never data loss.
/// Test: `manager_decommission_removes_real_git_worktree` asserts both effects.
fn prune_refs_after_removal(repo_root: &Path, path: &Path) {
    {
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
}

/// The base checkout whose worktree registry a decommission will strand (#5949).
///
/// Why: an SM-OWNED workspace is removed with `remove_dir_all`, which git never
/// learns about — the entry survives in the base checkout's worktree list until
/// something prunes it. The client layer already repaired this for the routed
/// HTTP paths, so the MCP tool and the idle reaper — both of which call
/// `decommission` in-process, below that layer — accumulated stale entries
/// instead, the reaper unattended. Answering the question here means every
/// caller inherits the repair regardless of transport.
/// What: `None` unless the record names an owned workspace that git reports a
/// DIFFERENT checkout as owning. A workspace that is its own checkout root has
/// no external registry to repair, and an unowned one is removed by
/// [`remove_session_worktree`], which prunes for itself. Must be called BEFORE
/// the removal: git can only answer from a directory that still exists.
/// Test: `decommission_prunes_the_base_repo_worktree_registry`,
/// `registry_root_to_repair_ignores_a_standalone_owned_clone`.
pub(super) fn registry_root_to_repair(record: &SessionRecord) -> Option<std::path::PathBuf> {
    if !record.workspace_owned {
        return None;
    }
    let ws = record.workspace_path.as_deref()?;
    let root = worktree_registry::registry_root_for(ws)?;
    // git reports a resolved path; the record's may still carry a symlinked
    // spelling of the same directory (every macOS temp path does), so compare
    // both in canonical form or the two would never look equal.
    let ws_resolved = std::fs::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf());
    (root != ws_resolved).then_some(root)
}

/// Clear the stale registry entry a removed worktree leaves behind (#5949).
///
/// Why: git keeps reporting a worktree whose directory was deleted out from
/// under it, and every consumer of that listing — this crate's own reclaim
/// sweep included — then reasons about a directory that is not there.
/// What: runs the prune subcommand in `root`. Best-effort: a failure leaves a
/// stale entry in git's output, never data loss, so it is logged and the
/// teardown that already succeeded continues.
/// Test: `decommission_prunes_the_base_repo_worktree_registry`.
fn prune_worktree_registry(root: &Path) {
    match std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["worktree", "prune"])
        .output()
    {
        Ok(out) if out.status.success() => {
            info!(root = %root.display(), "decommission: worktree registry pruned");
        }
        Ok(out) => warn!(
            root = %root.display(),
            "decommission: worktree prune exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => warn!(
            root = %root.display(),
            "decommission: worktree prune failed to spawn: {e}"
        ),
    }
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
    ///
    /// `caller` (#3649, Option B): `None` means an operator/daemon-internal
    /// caller (CLI, HTTP route, the age-based reaper, bulk prune) — current
    /// authority is preserved unconditionally, matching every pre-#3649
    /// call site. `Some(id)` means a SESSION is asking to decommission
    /// `id`'s target on its own behalf; if the target's worktree has a KNOWN
    /// owner that disagrees with `caller` AND that owner is not provably
    /// ownerless, the call is refused with
    /// [`ManagedError::WorktreeOwnerMismatch`] instead of tearing down a peer
    /// session's worktree out from under it.
    /// Test: `manager_decommission_removes_workspace` — asserts the workspace dir
    /// is gone from disk and the record state is `Decommissioned`.
    /// `manager_decommission_unowned_skips_deletion` — asserts that decommissioning
    /// a local-path/adopt record does NOT delete the directory.
    /// `decommission_owner_gate_refuses_foreign_caller`,
    /// `decommission_owner_gate_allows_terminal_owner` (#3649).
    pub async fn decommission(
        &self,
        id: &ManagedSessionId,
        caller: Option<ManagedSessionId>,
    ) -> Result<(SessionRecord, bool), ManagedError> {
        let config = TrustyToolsConfig::load();
        let managed_root = workspace_root(&config);
        self.decommission_with_root(id, &managed_root, caller).await
    }

    /// Tombstone a record and do NOTHING else — a dedicated, single-effect path,
    /// not a flag threaded through [`decommission_with_root`](Self::decommission_with_root)
    /// (owner request 2026-07-29; rebuilt as a separate function per PR #4725
    /// review round 2).
    ///
    /// Why: this exists for the `tm ls` auto-prune sweep, which runs unattended
    /// on every listing and knows only that a workspace was ABSENT at listing
    /// time. It must therefore be incapable of destroying anything, including on
    /// a remount race. It was originally built as `decommission_with_root(…,
    /// record_only: true)`, and that shape failed twice in review — each time
    /// because a destructive effect in that function was not behind the flag:
    ///
    ///   * round 1 (#4728): `graceful_terminate_runtime` ran above the guard,
    ///     SIGTERMing and `kill_session`ing any live pane whose NAME matched the
    ///     record's;
    ///   * round 2: `delete_search_index_best_effort` ran below every guard,
    ///     issuing a cross-daemon `DELETE /indexes/{id}?delete_data=true`. Worse,
    ///     its target id comes from `disposable_workspace_index_id`, which walks
    ///     UP from the workspace path for a `.git` marker and is documented as
    ///     requiring the workspace to still exist. Auto-prune fires only once the
    ///     workspace is GONE, so that contract is violated by construction and
    ///     the walk resolves to the PARENT PROJECT's index.
    ///
    /// Two rounds, two ungated effects, each found only by executing the path.
    /// A boolean parameter cannot make a multi-effect function safe — it can
    /// only be audited by exercising every branch, and it silently acquires new
    /// effects whenever someone adds one to the shared function. This function
    /// is auditable by reading: its entire body is record mutation plus one
    /// store write. There is no filesystem call, no subprocess, no network
    /// request, and no tmux interaction to gate, so none can be forgotten.
    ///
    /// What: applies the #3649 owner gate (via
    /// [`check_worktree_owner`](Self::check_worktree_owner)), clears
    /// `workspace_path`/`workspace_owned` only when nothing is left on disk
    /// (#4344 — a retained directory must keep its pointer), clears the #4400
    /// pending-decision fields, sets `Decommissioned`, and persists. Returns
    /// `(record, false)` — the `false` is a type-level constant, not a computed
    /// result, because nothing here can remove anything.
    /// Test: `decommission_record_only_never_removes_existing_workspace`,
    /// `decommission_record_only_never_touches_the_runtime`,
    /// `decommission_record_only_has_no_side_effects_beyond_the_store`;
    /// `disposable_index_id_for_a_removed_worktree_resolves_to_the_parent_project`
    /// pins why the shared teardown's index cleanup must not be reused here.
    pub async fn decommission_record_only(
        &self,
        id: &ManagedSessionId,
    ) -> Result<(SessionRecord, bool), ManagedError> {
        let mut record = self.get(id).await?;
        // Daemon-internal sweep: `caller = None` preserves full pre-#3649
        // authority, exactly as before. Kept so this path can never be a wider
        // authority than the teardown it replaces.
        self.check_worktree_owner(&record, None, id).await?;

        // #4344: only blank the pointer when the directory really is gone —
        // otherwise a retained workspace is stranded with no trail back to it.
        let workspace_still_on_disk = record.workspace_path.as_deref().is_some_and(|p| p.exists());
        if !workspace_still_on_disk {
            record.workspace_path = None;
            record.workspace_owned = false;
        }
        // #4400: a decommissioned session is terminal — a pending decision on it
        // would sit in the human-confirmation queue forever.
        record.pending_decision = None;
        record.proposed_default = None;
        // Stamps `terminal_at` as well as `state` — the retention sweep's clock
        // starts here, not at `created_at`.
        record.set_lifecycle_state(ManagedSessionState::Decommissioned, Utc::now());
        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %record.tmux_name, "managed session record tombstoned (record-only)");
        Ok((record, false))
    }

    /// The #3649 worktree-owner gate, shared by the full teardown and the
    /// record-only tombstone.
    ///
    /// Why: extracted so [`decommission_record_only`](Self::decommission_record_only)
    /// can apply the identical authority check without depending on
    /// `decommission_with_root` — the whole point of that function is that it
    /// shares no code path with the destructive one.
    /// What: `None` (operator/daemon-internal) always passes. `Some(id)` is
    /// refused with [`ManagedError::WorktreeOwnerMismatch`] when the target's
    /// worktree has a known owner that disagrees and is not provably ownerless.
    /// Test: `decommission_owner_gate_refuses_foreign_caller`,
    /// `decommission_owner_gate_allows_terminal_owner`.
    async fn check_worktree_owner(
        &self,
        record: &SessionRecord,
        caller: Option<ManagedSessionId>,
        id: &ManagedSessionId,
    ) -> Result<(), ManagedError> {
        if let Some(caller_id) = caller
            && let Some(owner) = self.known_owner_of(record)
            && owner != caller_id
            && !self.resolve_ownerless(owner).await
        {
            return Err(ManagedError::WorktreeOwnerMismatch(caller_id, owner, *id));
        }
        Ok(())
    }

    /// Internal: decommission with an explicit managed root (test seam).
    ///
    /// Why: tests need to inject a temp directory as the managed root to keep the
    /// containment guard working without mutating process-global env vars
    /// (`TRUSTY_MPM_WORKSPACE_ROOT`). Env mutation is thread-unsafe and pollutes
    /// parallel tests; injecting the root avoids that entirely.
    /// What: identical teardown logic as the public `decommission` but resolves the
    /// managed root from the caller-supplied `managed_root` instead of the config.
    ///
    /// 🔴 THIS FUNCTION IS UNCONDITIONALLY DESTRUCTIVE and has no "safe mode".
    /// The `record_only` flag it used to carry is GONE (PR #4725 review round 2);
    /// [`decommission_record_only`](Self::decommission_record_only) is now a
    /// separate function that shares no code path with this one. Read that
    /// function's doc for why. Its side effects, in order, are:
    ///
    /// | # | Effect | Reversible? |
    /// |---|---|---|
    /// | 1 | `graceful_terminate_runtime` — SIGTERM + `kill_session` the pane | no |
    /// | 2 | `remove_session_worktree` — `git worktree remove --force`, `fs::remove_dir_all` fallback, `git worktree prune`, `git branch -D` (dirty-gated, #4344) | no |
    /// | 3 | `fs::remove_dir_all` on an SM-owned workspace (containment-gated) | no |
    /// | 4 | `delete_search_index_best_effort` — cross-daemon `DELETE /indexes/{id}` (never from a test process, #4743) | no |
    /// | 5 | clears `workspace_path`/`workspace_owned` when nothing is on disk | store-only |
    /// | 6 | clears `pending_decision`/`proposed_default` (#4400) | store-only |
    /// | 7 | writes state `Decommissioned` | store-only |
    ///
    /// Effects 1–4 destroy state outside the record store. Any new effect added
    /// here belongs in that table. If a caller needs a subset, give it its own
    /// function rather than a flag — that is the lesson of this PR's two review
    /// rounds, each of which found a different ungated effect.
    ///
    /// Returns `(SessionRecord, workspace_removed)` where `workspace_removed` is
    /// `true` ONLY when `remove_dir_all` actually ran — callers must not infer this
    /// from a post-call filesystem check (TOCTOU: owned workspace already absent
    /// before decommission would give a false-positive filesystem result).
    /// Test: called by `manager_decommission_removes_workspace` (which passes a
    /// TempDir as the managed root, removing the need for `set_var`);
    /// `decommission_full_still_terminates_the_runtime`.
    pub(crate) async fn decommission_with_root(
        &self,
        id: &ManagedSessionId,
        managed_root: &Path,
        caller: Option<ManagedSessionId>,
    ) -> Result<(SessionRecord, bool), ManagedError> {
        let mut record = self.get(id).await?;

        // #3649 owner gate: only applies when a SESSION identifies itself as
        // the caller. `None` (operator/daemon-internal) preserves full
        // pre-#3649 authority unconditionally — see the doc above.
        self.check_worktree_owner(&record, caller, id).await?;

        // #2033: derive the trusty-search index id for a disposable workspace
        // (SM-owned clone or in-project worktree — see
        // `search_gc::disposable_workspace_index_id`) BEFORE any removal
        // happens below. This must run first because
        // `trusty_common::resolve_project_root` walks UP from the workspace
        // path looking for a `.git` marker — if the directory is already gone
        // by the time we derive the id, the walk would find the wrong (e.g.
        // shared base clone's) ancestor `.git` and target the WRONG index for
        // deletion. Local-path/adopted sessions (`workspace_owned == false`
        // and not an in-project worktree) return `None` here — their real,
        // long-lived directory keeps its search index, exactly mirroring the
        // filesystem-removal guard below.
        let search_index_id = search_gc::disposable_workspace_index_id(
            record.workspace_path.as_deref(),
            record.workspace_owned,
        );

        // #5949: resolve the checkout that owns this workspace's worktree
        // registry BEFORE anything is removed. `registry_root_for` asks git
        // from the workspace itself, so it can only be answered while the
        // directory still exists — the same ordering contract `search_index_id`
        // above is bound by, for the same reason.
        let registry_root = registry_root_to_repair(&record);

        // Gracefully terminate the runtime before removing the workspace (#1975):
        // SIGTERM the claude process and give it a grace window to flush state,
        // then reclaim the pane — instead of an abrupt `kill_session`. Best-effort:
        // a session whose runtime is already gone still decommissions cleanly —
        // the helper self-guards and is a no-op when the pane is already gone.
        //
        // Effect 1 of the table above. `graceful_terminate_runtime` self-guards
        // on `session_exists(name)` — LIVE-TMUX NAME MEMBERSHIP, not the record's
        // captured `pane_id` (#4728) — so it kills whatever live session carries
        // this name. Acceptable here, where teardown is the caller's stated
        // intent; never acceptable on a listing sweep, which is why the
        // record-only path is a separate function rather than a flag.
        self.graceful_terminate_runtime(&record.tmux_name).await;

        // Effects 2–3. Guard: only remove the workspace directory if the SM
        // provisioned it. Track whether remove_dir_all ACTUALLY RAN (not
        // inferred from filesystem).
        let mut workspace_removed = false;
        if let Some(ref ws) = record.workspace_path {
            if !record.workspace_owned {
                // Unowned workspace (local-path spawn or adopt): never bulk-delete.
                // #1840: EXCEPTION — in-project per-session worktrees live under
                // .worktrees/ and must be cleaned up via `git worktree remove` so
                // the git ref is also pruned.  The base clone directory is NEVER
                // touched — only the leaf worktree path.
                if is_session_worktree(ws) {
                    // Data-safety gate (#4091-style, decommission-side): before
                    // this, `remove_session_worktree` ran `git worktree remove
                    // --force` (falling back to `fs::remove_dir_all`)
                    // unconditionally, with NO dirty check at all — a live
                    // data-loss path for `tm sessions prune --state stopped`
                    // (which calls `decommission` per matching record). Reuse
                    // the same `worktree_safety::inspect_dirt` check the
                    // orphan-worktree sweep (`prune_orphaned_worktrees`)
                    // already uses: refuse (and report) a candidate holding
                    // uncommitted/untracked work or unpushed commits. Runs on
                    // spawn_blocking since it shells out to git, same as the
                    // removal itself; a panicked check is treated as dirty
                    // (fail-safe) rather than a green light to delete.
                    let ws_for_check = ws.clone();
                    let dirt = tokio::task::spawn_blocking(move || inspect_dirt(&ws_for_check))
                        .await
                        .unwrap_or_else(|e| {
                            Some(DirtyWorktree::new(
                                ws,
                                format!("dirty-check task panicked: {e}"),
                                0,
                                0,
                            ))
                        });

                    if let Some(dirt) = dirt {
                        warn!(
                            id = %id,
                            workspace = %ws.display(),
                            reason = %dirt.reason,
                            "decommission: refusing to remove worktree — it holds \
                             unsaved work; leaving it on disk (the record below is \
                             still tombstoned)"
                        );
                        // workspace_removed stays false — nothing was deleted.
                    } else {
                        // Item 4 (#1845): wrap the blocking `git worktree remove`
                        // call in spawn_blocking + tokio::time::timeout so a hung
                        // git process cannot stall the async executor indefinitely.
                        let ws_clone = ws.clone();
                        let join =
                            tokio::task::spawn_blocking(move || remove_session_worktree(&ws_clone));
                        let outcome =
                            match tokio::time::timeout(GIT_WORKTREE_REMOVE_TIMEOUT, join).await {
                                Ok(Ok(outcome)) => outcome,
                                Ok(Err(e)) => {
                                    WorktreeRemoval::Kept(format!("the removal task panicked: {e}"))
                                }
                                Err(_elapsed) => WorktreeRemoval::Kept(format!(
                                    "git worktree remove did not finish within {}s; the \
                                     worktree may require manual cleanup",
                                    GIT_WORKTREE_REMOVE_TIMEOUT.as_secs()
                                )),
                            };
                        // #4732: the reason now comes back FROM the remover
                        // rather than being buried in a log line inside it.
                        workspace_removed = outcome.removed();
                        if let Some(reason) = outcome.reason() {
                            warn!(
                                id = %id,
                                workspace = %ws.display(),
                                "decommission: worktree left on disk — {reason}"
                            );
                        }
                    }
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
                        // #5949: `remove_dir_all` is invisible to git, so the
                        // base checkout still lists this worktree. Repair it
                        // here — below every caller, in-process ones included.
                        if let Some(ref root) = registry_root {
                            prune_worktree_registry(root);
                        }
                    }
                }
            }
        }

        // Effect 4. #2033: best-effort remove the trusty-search index for a
        // disposed workspace, alongside the worktree/clone directory. Fail-soft:
        // an unreachable/erroring search daemon must never block or fail session
        // teardown — `delete_search_index_best_effort` logs and swallows every
        // failure mode itself.
        //
        // Note the ordering contract that makes this correct ONLY here:
        // `search_index_id` was derived at the top of this function, BEFORE any
        // removal, because `resolve_project_root` walks up for a `.git` marker
        // and would otherwise resolve to the parent project's index. A caller
        // that runs against an already-absent workspace cannot satisfy that
        // contract, which is precisely why the record-only path does not reuse
        // this function (PR #4725 review round 2).
        if let Some(index_id) = search_index_id {
            search_gc::delete_search_index_best_effort(&index_id).await;
        }

        // Tombstone: mark Decommissioned, persist. `workspace_path`/
        // `workspace_owned` are cleared ONLY when nothing is left on disk —
        // every "skip removal" branch above (the dirty-worktree refusal,
        // unowned/local-path, the containment guard) deliberately leaves real
        // content in place, and blanking the pointer here would strand that
        // retained directory with nothing but the transient warn! log line
        // above as a trail back to it (#4344 review). A record whose
        // workspace really was removed (or was already absent) still nulls
        // both fields exactly as before.
        let workspace_still_on_disk = record.workspace_path.as_deref().is_some_and(|p| p.exists());
        if !workspace_still_on_disk {
            record.workspace_path = None;
            record.workspace_owned = false;
        }
        // #4400: a decommissioned session is terminal — no human will ever act
        // on a `pending_decision` raised before teardown, so leaving it set
        // means `supervisor_status`'s human-confirmation queue accumulates
        // phantom gates forever (19-day-old dead entries indistinguishable
        // from a real, live T4 gate). Clear both fields unconditionally on
        // every decommission path (fresh teardown, record-only tombstone,
        // dirty-worktree-refused teardown — all reach this line).
        record.pending_decision = None;
        record.proposed_default = None;
        // Stamps `terminal_at` as well as `state` — the retention sweep's clock
        // starts here, not at `created_at`.
        record.set_lifecycle_state(ManagedSessionState::Decommissioned, Utc::now());
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
