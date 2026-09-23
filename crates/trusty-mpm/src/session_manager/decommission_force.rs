//! Decommission's in-project worktree removal, and its `--force` policy (#7660).
//!
//! Why: `tm sessions decommission` refused every freshly provisioned workspace,
//! because tm's own provisioning leaves ` M .gitignore`, `?? .claude/settings.json`,
//! `?? .claude/settings.json.bak` and `?? CLAUDE.md` behind, and the dirty-tree
//! guard counts all four as unsaved work. It also exited 0 while declining, so a
//! script saw success and the directory stayed on disk.
//! What: [`remove_in_project_worktree`] — the dirty-gated removal step that used
//! to sit inline in `decommission_with_root_checked` — now returns a
//! [`WorkspaceVerdict`] that carries WHY a workspace was kept, and honours
//! [`ProvisioningDirt::Discard`], under which the four provisioning paths are
//! excused only in the exact state provisioning leaves them
//! ([`is_provisioning_entry`]). Unpushed commits, any other modified or
//! untracked file, an edit to a tracked provisioning path, nested-repository
//! work, and every check that cannot complete still keep the workspace.
//! Test: `force_decommission_removes_a_provisioning_only_worktree`,
//! `force_decommission_keeps_an_edited_tracked_claude_md`,
//! `force_decommission_keeps_a_gitignore_with_a_non_provisioning_line`,
//! `force_decommission_still_refuses_user_work`,
//! `force_decommission_still_refuses_unpushed_commits`,
//! `force_decommission_removes_nothing_when_the_dirty_check_cannot_complete`,
//! `decommission_reports_why_it_kept_a_provisioned_worktree`.

use std::path::Path;

use tracing::warn;

use super::decommission::{
    GIT_WORKTREE_REMOVE_TIMEOUT, WorktreeRemoval, remove_session_worktree_guarded,
};
use super::record::{ManagedSessionId, SessionRecord};
use super::worktree_safety::{DirtyWorktree, inspect_dirt, inspect_dirt_excusing};

/// The paths tm's own provisioning writes into a workspace (#7660).
///
/// Why: these are the four entries the issue's `git status --porcelain` showed
/// on a workspace no user had touched. Nothing else is excused: a broader list
/// would let `--force` discard work.
pub(crate) const PROVISIONING_FILES: [&str; 4] = [
    ".gitignore",
    ".claude/settings.json",
    ".claude/settings.json.bak",
    "CLAUDE.md",
];

/// Whether decommission may discard tm's own provisioning dirt (#7660).
///
/// Why: a two-variant enum rather than a `force: bool`, for the reason
/// [`super::DirtyWorktreePolicy`] gives — a swapped positional bool next to the
/// existing `check_foreign_claim` flag would silently invert a data-safety gate.
/// Test: `force_decommission_removes_a_provisioning_only_worktree`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProvisioningDirt {
    /// Any dirt keeps the workspace (the default).
    #[default]
    Refuse,
    /// Dirt made only of [`PROVISIONING_FILES`] does not keep the workspace.
    Discard,
}

/// What a full decommission did with the workspace, and why (#7660).
///
/// Why: `(SessionRecord, bool)` could say THAT a workspace stayed but not WHY,
/// so the CLI could neither exit non-zero nor name what blocked the removal.
/// What: the tombstoned record, whether the directory was removed, and — only
/// when decommission had a removal candidate and kept it — the reason.
/// Test: `decommission_reports_why_it_kept_a_provisioned_worktree`.
#[derive(Debug, Clone)]
pub struct DecommissionReport {
    /// The tombstoned record.
    pub record: SessionRecord,
    /// `true` only when the directory was removed by this call.
    pub workspace_removed: bool,
    /// Why a removable workspace was kept; `None` when it was removed, absent,
    /// or never tm's to remove (a local-path or adopted session).
    pub workspace_kept_reason: Option<String>,
}

/// What [`remove_in_project_worktree`] did (#7660).
#[derive(Debug, Clone, Default)]
pub(super) struct WorkspaceVerdict {
    /// `true` only when the worktree was removed.
    pub removed: bool,
    /// Why the worktree was kept, when it was.
    pub kept_reason: Option<String>,
}

/// The provisioning paths `--force` excuses only while git does not track them
/// (#7660). A tracked one showing ` M` is an edit someone made, not a write
/// provisioning did, so it is never excused.
const UNTRACKED_PROVISIONING_FILES: [&str; 3] = [
    ".claude/settings.json",
    ".claude/settings.json.bak",
    "CLAUDE.md",
];

/// Is this `git status --porcelain` line in `ws` one of tm's provisioning
/// files, in exactly the state provisioning leaves it (#7660)?
///
/// Why: `--force` is followed by `git worktree remove --force`, which destroys
/// whatever it excused. A repository that tracks `CLAUDE.md` shows an agent's
/// edit to it as ` M CLAUDE.md`, and excusing that by path alone discarded it.
/// What: the three [`UNTRACKED_PROVISIONING_FILES`] are excused only as `??`.
/// `.gitignore` is excused as ` M` only when its unstaged diff adds nothing
/// but the lines provisioning writes and removes nothing, and as `??` only
/// when every line of it is such a line. Anything else — staged, deleted,
/// renamed, conflicted, or an unreadable diff — is not excused.
/// Test: `provisioning_entry_matches_only_the_four_paths_in_provisioning_states`,
/// `force_decommission_keeps_an_edited_tracked_claude_md`,
/// `force_decommission_keeps_a_gitignore_with_a_non_provisioning_line`,
/// `force_decommission_removes_a_tree_with_an_untracked_scaffold_gitignore`,
/// `force_decommission_keeps_an_untracked_gitignore_with_a_user_line`.
pub(crate) fn is_provisioning_entry(ws: &Path, line: &str) -> bool {
    let (Some(status), Some(path)) = (line.get(..3), line.get(3..)) else {
        return false;
    };
    match (status, path.trim()) {
        // #7660: `.gitignore` is checked line by line, never by path alone.
        (" M ", ".gitignore") => gitignore_diff_is_provisioning(ws),
        ("?? ", ".gitignore") => std::fs::read_to_string(ws.join(".gitignore"))
            .is_ok_and(|body| body.lines().all(is_provisioning_gitignore_line)),
        ("?? ", path) => UNTRACKED_PROVISIONING_FILES.contains(&path),
        _ => false,
    }
}

/// Whether `line` is one provisioning writes into `.gitignore` (#7660): a
/// blank line, a managed-block marker, or a managed path.
fn is_provisioning_gitignore_line(line: &str) -> bool {
    use crate::core::scaffold_gitignore::{
        SCAFFOLD_GITIGNORE_BEGIN, SCAFFOLD_GITIGNORE_END, SCAFFOLD_IGNORED_PATHS,
    };
    line.trim().is_empty()
        || line == SCAFFOLD_GITIGNORE_BEGIN
        || line == SCAFFOLD_GITIGNORE_END
        || SCAFFOLD_IGNORED_PATHS.contains(&line)
}

/// Whether `ws`'s unstaged `.gitignore` diff only ADDS provisioning lines
/// (#7660).
///
/// What: `git diff -U0` of the working tree against the index. Every `+` line
/// must pass [`is_provisioning_gitignore_line`]; any `-` line, any line that is
/// not a diff header, or a diff that cannot be read answers `false`. Headers
/// are recognised only before the first `@@` hunk.
/// Test: `gitignore_body_line_shaped_like_a_header_is_not_excused`,
/// `force_decommission_keeps_the_tree_when_the_gitignore_diff_cannot_be_read`.
fn gitignore_diff_is_provisioning(ws: &Path) -> bool {
    let args = [
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "-U0",
        "--",
        ".gitignore",
    ];
    let Ok(diff) = super::worktree_safety::git_stdout(ws, &args) else {
        return false;
    };
    let mut added = 0usize;
    let mut in_hunk = false;
    for line in diff.lines() {
        // #7660: `---`/`+++` are headers only before the first hunk; inside
        // one, `+++ x` is the added line `++ x`.
        if !in_hunk {
            if line.starts_with("@@ ") {
                in_hunk = true;
                continue;
            }
            if line.starts_with("diff --git ")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
            {
                continue;
            }
            return false;
        }
        if line.starts_with("@@ ") || line.starts_with("\\ ") {
            continue;
        }
        match line.strip_prefix('+') {
            Some(body) if is_provisioning_gitignore_line(body) => added += 1,
            _ => return false,
        }
    }
    added > 0
}

/// Remove an in-project session worktree unless it holds work (#4344, #7660).
///
/// Why: see the module doc. Moved out of `decommission_with_root_checked`
/// unchanged except for the `policy` excuse and the returned reason.
/// What: runs [`inspect_dirt`] — or, under [`ProvisioningDirt::Discard`],
/// [`inspect_dirt_excusing`] with [`is_provisioning_entry`] — on a blocking
/// thread. Any dirt, a panicked check or a failed check keeps the worktree and
/// returns the reason; a clean answer removes it through
/// [`remove_session_worktree_guarded`]. Under `Discard` its guard re-asks the
/// same question immediately before `git worktree remove --force`.
/// Test: `force_decommission_removes_a_provisioning_only_worktree`,
/// `force_decommission_removes_nothing_when_the_dirty_check_cannot_complete`,
/// `decommission_reports_why_it_kept_a_provisioned_worktree`.
pub(super) async fn remove_in_project_worktree(
    id: &ManagedSessionId,
    ws: &Path,
    policy: ProvisioningDirt,
) -> WorkspaceVerdict {
    let ws_for_check = ws.to_path_buf();
    let dirt = tokio::task::spawn_blocking(move || dirt_under(&ws_for_check, policy))
        .await
        .unwrap_or_else(|e| {
            // Fail-safe: a panicked check is dirty, never a green light.
            Some(DirtyWorktree::new(
                ws,
                format!("dirty-check task panicked: {e}"),
                0,
                0,
            ))
        });
    if let Some(dirt) = dirt {
        warn!(
            id = %id, workspace = %ws.display(), reason = %dirt.reason,
            "decommission: refusing to remove worktree — it holds unsaved work; leaving \
             it on disk (the record is still tombstoned)"
        );
        return WorkspaceVerdict {
            removed: false,
            kept_reason: Some(kept_for_dirt(&dirt.reason, policy)),
        };
    }
    // #1845 item 4: a hung git must not stall the executor.
    let ws_clone = ws.to_path_buf();
    let join = tokio::task::spawn_blocking(move || {
        // #7660: a forced removal re-asks its question inside the audit window;
        // the default path is unchanged.
        let guard = || match policy {
            ProvisioningDirt::Refuse => None,
            ProvisioningDirt::Discard => dirt_under(&ws_clone, policy).map(|d| d.reason),
        };
        // #7885: name the route in the audit line.
        remove_session_worktree_guarded(
            &ws_clone,
            "session decommission: the session ended and its tree is clean",
            &guard,
        )
    });
    let outcome = match tokio::time::timeout(GIT_WORKTREE_REMOVE_TIMEOUT, join).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => WorktreeRemoval::Kept(format!("the removal task panicked: {e}")),
        Err(_elapsed) => WorktreeRemoval::Kept(format!(
            "git worktree remove did not finish within {}s; the worktree may require manual \
             cleanup",
            GIT_WORKTREE_REMOVE_TIMEOUT.as_secs()
        )),
    };
    // #4732: the reason comes back FROM the remover.
    if let Some(reason) = outcome.reason() {
        warn!(id = %id, workspace = %ws.display(), "decommission: worktree left on disk — {reason}");
    }
    WorkspaceVerdict {
        removed: outcome.removed(),
        kept_reason: outcome.reason().map(str::to_string),
    }
}

/// The dirt `policy` does not excuse, or `None` when `ws` may be removed.
fn dirt_under(ws: &Path, policy: ProvisioningDirt) -> Option<DirtyWorktree> {
    match policy {
        ProvisioningDirt::Refuse => inspect_dirt(ws),
        ProvisioningDirt::Discard => {
            inspect_dirt_excusing(ws, &|line| is_provisioning_entry(ws, line))
        }
    }
}

/// The operator-facing reason a dirty worktree was kept (#7660).
fn kept_for_dirt(reason: &str, policy: ProvisioningDirt) -> String {
    let files = PROVISIONING_FILES.join(", ");
    match policy {
        ProvisioningDirt::Refuse => format!(
            "the dirty-tree guard kept it ({reason}). If the only changes are tm's own \
             provisioning files ({files}), re-run with --force to remove it; --force never \
             discards other changes or unpushed commits"
        ),
        ProvisioningDirt::Discard => format!(
            "--force excused tm's provisioning files ({files}), but the dirty-tree guard \
             still kept it ({reason}); --force never discards that"
        ),
    }
}

#[cfg(test)]
#[path = "decommission_force_tests.rs"]
mod decommission_force_tests;
