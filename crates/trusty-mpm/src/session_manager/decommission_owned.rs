//! Content gates on decommission's two `remove_dir_all` routes (#8663).
//!
//! Why: every `git worktree remove` route refuses on dirty files, unpushed
//! commits and kept gitignored output (#4091, #8534). Two routes delete with
//! `remove_dir_all` instead and asked none of it: decommission of an SM-owned
//! workspace (which can be a git worktree, #5949), and the shared remover's
//! fallback for a directory git has disowned. One decommission call could
//! delete an agent's `results/` and its unpushed commits.
//! What: [`remove_owned_workspace`] runs [`owned_workspace_keep_reason`] and the
//! delete inside one audited blocking task; [`unclaimed_directory_blocks_removal`]
//! gates the disowned-directory fallback. Every failed check keeps the
//! directory.
//! Test: `decommission_owned_tests`.

use std::path::Path;

use tracing::{info, warn};

use super::decommission::WorktreeRemoval;
use super::decommission_force::{
    ProvisioningDirt, WorkspaceVerdict, WorktreeKind, dirty_entries, force_blocker,
    is_provisioning_entry, kept_for_dirt, lock_blocker, worktree_kind,
};
use super::manager::ManagedError;
use super::provisioning_ledger;
use super::record::ManagedSessionId;
use super::worktree_ignored_output::{
    ignored_output_refusal, kept_unversioned_content, unversioned_content_refusal,
};
use super::worktree_safety::{DirtyWorktreePolicy, inspect_dirt_excusing, is_worktree_root};

/// Why an SM-owned workspace must be kept, or `None` when it may be deleted
/// (#8663).
///
/// Why: `workspace_owned` says tm created the directory, not that nothing in
/// it is worth keeping.
/// What: a directory that is its own git worktree root gets the worktree
/// guard. [`worktree_kind`] must prove it a linked worktree or a main
/// checkout; a probe error keeps it under either policy. A linked worktree
/// that `git worktree lock` protects is kept under either policy
/// ([`lock_blocker`]). Then [`inspect_dirt_excusing`] (dirty files, unpushed
/// commits, nested repositories), excusing only entries that match the
/// [`provisioning_ledger`] byte for byte; no ledger excuses nothing. Under
/// [`ProvisioningDirt::Discard`] (`--force`), when that check still finds
/// dirt, a linked worktree must pass [`force_blocker`] for `id`, and the dirt
/// check then also excuses tm's provisioning files as the in-project route
/// does: `TASK.md` only while it holds what tm wrote from `task`, the
/// record's task (#8688). Last, [`ignored_output_refusal`]. A `.git` entry
/// git cannot resolve keeps it. Any other directory must hold only harness
/// files and regenerable output ([`unversioned_content_refusal`]). Every
/// reason names the path.
/// Test: `owned_worktree_with_an_unpushed_commit_is_kept`,
/// `owned_worktree_with_untracked_results_is_kept`,
/// `owned_non_git_workspace_with_user_files_is_kept`,
/// `owned_workspace_is_kept_when_the_content_check_fails`,
/// `ledger_excuses_only_provisioning_dirt_on_a_clone`,
/// `locked_owned_worktree_is_kept_even_with_force`,
/// `owned_worktree_whose_probe_fails_is_kept`,
/// `force_on_owned_worktree_of_another_session_is_kept`.
pub(super) fn owned_workspace_keep_reason(
    ws: &Path,
    id: &ManagedSessionId,
    task: Option<&str>,
    policy: ProvisioningDirt,
) -> Option<String> {
    if is_worktree_root(ws).unwrap_or(false) {
        let named = |reason: String| Some(format!("{}: {reason}", ws.display()));
        // #8663 critic round 2: only a proven main checkout skips the lock and
        // `--force` gates; a probe that cannot answer keeps the tree.
        let linked = match worktree_kind(ws) {
            Ok(WorktreeKind::Linked(git_dir)) => Some(git_dir),
            Ok(WorktreeKind::MainCheckout) => None,
            Err(e) => return named(format!("{e}; nothing was removed")),
        };
        // #8663 critic round 1: a lock keeps it under both policies.
        if let Some(blocker) = linked.as_deref().and_then(lock_blocker) {
            return named(format!("{blocker}; nothing was removed"));
        }
        // #8663 critic round 1: tm's provisioning writes, proven by the ledger.
        let ledger = provisioning_ledger::load(ws);
        // #8688: one excuse for the count and the list; no ledger excuses nothing.
        let ledgered = |line: &str| ledger.as_ref().is_some_and(|l| l.excuses(ws, line));
        let Some(dirt) = inspect_dirt_excusing(ws, &ledgered) else {
            return ignored_output_refusal(ws);
        };
        if policy == ProvisioningDirt::Refuse {
            return named(kept_for_dirt(ws, &dirt.reason, policy, &ledgered));
        }
        if linked.is_some()
            && let Some(blocker) = force_blocker(ws, id)
        {
            return named(format!("--force declined: {blocker}; nothing was removed"));
        }
        // #8688: `task` decides whether `TASK.md` is tm's write.
        let excuse = |line: &str| is_provisioning_entry(ws, task, line) || ledgered(line);
        if let Some(dirt) = inspect_dirt_excusing(ws, &excuse) {
            return named(kept_for_dirt(ws, &dirt.reason, policy, &excuse));
        }
        return ignored_output_refusal(ws);
    }
    match std::fs::symlink_metadata(ws.join(".git")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => unversioned_content_refusal(ws),
        // #8663: a `.git` git cannot resolve hides commits no check can count.
        _ => Some(format!(
            "{} has a `.git` entry git does not resolve to this directory, so its commits \
             and changes cannot be checked; keeping it (#8663)",
            ws.display()
        )),
    }
}

/// Delete an SM-owned workspace unless [`owned_workspace_keep_reason`] keeps
/// it (decommission effect 3, #8663).
///
/// Why: the check and the delete run in one blocking task, inside the
/// removal audit, so nothing written between them is lost unchecked and the
/// daemon executor never waits on `git status`.
/// What: `Ok` with the verdict — removed, or kept with the reason, logged
/// with the session id. A kept workspace leaves the record's pointer in place
/// (the tombstone step clears it only when nothing is on disk). A failed
/// `remove_dir_all` is `Err(ManagedError::Io)`, as before. A panicked task is
/// a keep. Under `--force` a `warn!` names the provisioning files deleted with
/// the tree.
/// Test: `owned_worktree_with_an_unpushed_commit_is_kept`,
/// `clean_owned_workspace_is_still_removed`,
/// `decommission_prunes_the_base_repo_worktree_registry`.
pub(super) async fn remove_owned_workspace(
    id: &ManagedSessionId,
    task: Option<&str>,
    ws: &Path,
    policy: ProvisioningDirt,
) -> Result<WorkspaceVerdict, ManagedError> {
    let path = ws.to_path_buf();
    let owner = *id;
    // #8688: `TASK.md` is excused only while it equals the record's task.
    let task = task.map(str::to_owned);
    let join = tokio::task::spawn_blocking(move || {
        let mut failure: Option<std::io::Error> = None;
        // #7885 critic round: audited like every other removal route.
        let outcome = super::worktree_removal_audit::audited_removal(
            &path,
            "session decommission: owned workspace, containment guard passed",
            || {
                let task = task.as_deref();
                if let Some(reason) = owned_workspace_keep_reason(&path, &owner, task, policy) {
                    return WorktreeRemoval::Kept(reason);
                }
                if policy == ProvisioningDirt::Discard {
                    warn_force_losses(&path);
                }
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => WorktreeRemoval::Removed,
                    Err(e) => {
                        let kept =
                            WorktreeRemoval::Kept(format!("removing the workspace failed: {e}"));
                        failure = Some(e);
                        kept
                    }
                }
            },
        );
        (outcome, failure)
    });
    let (outcome, failure) = join.await.unwrap_or_else(|e| {
        let reason = format!("the owned-workspace removal task panicked: {e}");
        (WorktreeRemoval::Kept(reason), None)
    });
    if let Some(e) = failure {
        return Err(ManagedError::Io(std::io::Error::new(
            e.kind(),
            format!("remove workspace {ws:?}: {e}"),
        )));
    }
    match outcome.reason() {
        Some(reason) => warn!(
            id = %id, workspace = %ws.display(), reason = %reason,
            "decommission: refusing to remove the owned workspace; leaving it on disk (the \
             record is still tombstoned)"
        ),
        None => info!(
            id = %id, workspace = %ws.display(),
            "decommission: owned workspace removed from disk"
        ),
    }
    Ok(WorkspaceVerdict {
        removed: outcome.removed(),
        kept_reason: outcome.reason().map(str::to_string),
    })
}

/// Log the provisioning files `--force` is about to delete with `ws` (#8663).
fn warn_force_losses(ws: &Path) {
    let lost = dirty_entries(ws);
    if !lost.is_empty() {
        warn!(
            workspace = %ws.display(),
            "decommission --force: deleting tm's provisioning files with the workspace, \
             including any edits made to them: {}",
            lost.join(", ")
        );
    }
}

/// Why the shared remover's disowned-directory fallback must keep `path`, or
/// `None` to delete it (#8663).
///
/// Why: git holds no state there, so no dirt check ran; an agent that deleted
/// `.git` in its worktree lost its results to the orphan prune.
/// What: [`unversioned_content_refusal`], logged. Under
/// [`DirtyWorktreePolicy::ForceDiscard`] (`prune-worktrees --discard-dirty`)
/// found content is deleted after a `warn!` naming every kept entry and its
/// file count ([`discarded_entries`]); a failed check keeps the directory under
/// either policy.
/// Test: `unclaimed_directory_with_results_is_kept`,
/// `unclaimed_directory_holding_only_harness_files_is_removed`,
/// `unclaimed_directory_is_kept_when_the_content_check_fails`,
/// `force_discard_removes_an_unclaimed_directory_with_results`.
pub(super) fn unclaimed_directory_blocks_removal(
    path: &Path,
    policy: DirtyWorktreePolicy,
) -> Option<String> {
    if policy == DirtyWorktreePolicy::ForceDiscard {
        match kept_unversioned_content(path) {
            Ok(None) => return None,
            Ok(Some(out)) => {
                warn!(
                    path = %path.display(),
                    "worktree removal: DISCARDING {} file(s) git holds no record of: {} — \
                     explicit force-discard opt-in was supplied (#8663)",
                    out.files,
                    discarded_entries(&out.entries)
                );
                return None;
            }
            // #8663: nothing can be named, so nothing is discarded.
            Err(_) => {}
        }
    }
    let refusal = unversioned_content_refusal(path)?;
    warn!(path = %path.display(), "worktree removal refused — {refusal}");
    Some(refusal)
}

/// How many entries [`discarded_entries`] names before it summarises.
const DISCARD_LOG_CAP: usize = 50;

/// `` `a` (3 files), `b` (1 file)`` for every kept top-level entry, capped at
/// [`DISCARD_LOG_CAP`] with `+N more` (#8663 critic round 1).
///
/// Why: `--discard-dirty` deletes all of them; a log naming only the first
/// leaves the operator unable to say what was lost.
/// Test: `discarded_entries_names_every_entry_up_to_the_cap`.
pub(super) fn discarded_entries(entries: &[(String, usize)]) -> String {
    let mut named: Vec<String> = entries
        .iter()
        .take(DISCARD_LOG_CAP)
        .map(|(entry, files)| {
            let noun = if *files == 1 { "file" } else { "files" };
            format!("`{entry}` ({files} {noun})")
        })
        .collect();
    if entries.len() > DISCARD_LOG_CAP {
        named.push(format!("+{} more", entries.len() - DISCARD_LOG_CAP));
    }
    named.join(", ")
}
