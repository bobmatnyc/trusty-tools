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
//! `--force` acts only on a linked worktree tm provably created that nothing
//! locks ([`force_blocker`]); a workspace tm never removes — the shared main
//! checkout a launch-on-main session runs in — is kept with a named reason
//! ([`unowned_kept_reason`]) so the CLI exits non-zero (#7660 round 2).
//! Test: `force_decommission_removes_a_provisioning_only_worktree`,
//! `force_decommission_keeps_an_edited_tracked_claude_md`,
//! `force_decommission_keeps_a_gitignore_with_a_non_provisioning_line`,
//! `force_decommission_still_refuses_user_work`,
//! `force_decommission_still_refuses_unpushed_commits`,
//! `force_decommission_removes_nothing_when_the_dirty_check_cannot_complete`,
//! `decommission_reports_why_it_kept_a_provisioned_worktree`.

use std::path::{Path, PathBuf};

use tracing::warn;

use super::decommission::{
    GIT_WORKTREE_REMOVE_TIMEOUT, WORKTREE_SENTINEL_FILE, WorktreeRemoval,
    remove_session_worktree_guarded,
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
    /// Why the workspace was kept; `None` when it was removed or is absent.
    /// #7660: a workspace tm never removes (a main checkout, a local-path or
    /// adopted directory) carries its by-design reason too.
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
/// thread, after [`force_blocker`] under `Discard`. Any dirt, a blocker, a
/// panicked check or a failed check keeps the worktree and returns the reason;
/// a clean answer removes it through [`remove_session_worktree_guarded`]. Under
/// `Discard` its guard re-asks the same questions immediately before
/// `git worktree remove --force`. An absent path is neither removed nor kept.
/// Test: `force_decommission_removes_a_provisioning_only_worktree`,
/// `force_decommission_removes_nothing_when_the_dirty_check_cannot_complete`,
/// `decommission_reports_why_it_kept_a_provisioned_worktree`.
pub(super) async fn remove_in_project_worktree(
    id: &ManagedSessionId,
    ws: &Path,
    policy: ProvisioningDirt,
) -> WorkspaceVerdict {
    // #7660: nothing on disk is nothing kept — an absent tree is not a refusal.
    if std::fs::symlink_metadata(ws).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return WorkspaceVerdict::default();
    }
    let ws_for_check = ws.to_path_buf();
    let kept = tokio::task::spawn_blocking(move || keep_reason(&ws_for_check, policy))
        .await
        .unwrap_or_else(|e| {
            // Fail-safe: a panicked check is dirty, never a green light.
            Some(format!("the dirty-tree check panicked: {e}"))
        });
    if let Some(reason) = kept {
        warn!(
            id = %id, workspace = %ws.display(), reason = %reason,
            "decommission: refusing to remove worktree; leaving it on disk (the record \
             is still tombstoned)"
        );
        return WorkspaceVerdict {
            removed: false,
            kept_reason: Some(reason),
        };
    }
    // #1845 item 4: a hung git must not stall the executor.
    let ws_clone = ws.to_path_buf();
    let join = tokio::task::spawn_blocking(move || {
        // #7660: a forced removal re-asks every question — provenance, lock and
        // dirt — inside the audit window; the default path is unchanged.
        let guard = || match policy {
            ProvisioningDirt::Refuse => None,
            ProvisioningDirt::Discard => keep_reason(&ws_clone, policy),
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

/// Why `ws` must be kept under `policy`, or `None` when it may be removed
/// (#7660).
///
/// What: under [`ProvisioningDirt::Discard`], [`force_blocker`] runs first —
/// `--force` acts only on a worktree tm provably created and nothing locks.
/// Then the dirt `policy` does not excuse keeps it, named file by file.
fn keep_reason(ws: &Path, policy: ProvisioningDirt) -> Option<String> {
    if policy == ProvisioningDirt::Discard
        && let Some(blocker) = force_blocker(ws)
    {
        return Some(format!("--force declined: {blocker}; nothing was removed"));
    }
    dirt_under(ws, policy).map(|d| kept_for_dirt(ws, &d.reason, policy))
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
fn kept_for_dirt(ws: &Path, reason: &str, policy: ProvisioningDirt) -> String {
    let files = PROVISIONING_FILES.join(", ");
    // #7660: name what blocked the removal, not only how many entries did.
    let named = blocking_entries(ws, policy);
    match policy {
        ProvisioningDirt::Refuse => format!(
            "the dirty-tree guard kept it ({reason}{named}). If the only changes are tm's own \
             provisioning files ({files}), re-run with --force to remove it; --force never \
             discards other changes or unpushed commits"
        ),
        ProvisioningDirt::Discard => format!(
            "--force excused tm's provisioning files ({files}), but the dirty-tree guard \
             still kept it ({reason}{named}); --force never discards that"
        ),
    }
}

/// How many dirty entries [`blocking_entries`] names before it summarises.
const NAMED_ENTRIES_CAP: usize = 10;

/// `": <entry>, <entry>"` for the working-tree entries that keep `ws` under
/// `policy`, or `""` when there are none to name or status cannot be read
/// (#7660).
///
/// Why: "1 uncommitted/untracked file(s)" tells an operator THAT `--force`
/// refused, not what to look at.
/// What: display only — the keep/remove decision was already made by
/// [`dirt_under`], so a failed read here only drops the list. Lists the
/// per-file porcelain entries [`is_provisioning_entry`] does not excuse under
/// `Discard` (every entry under `Refuse`), skipping the ownership sentinel and
/// `.trusty-mpm/`, which the dirty-tree guard accounts for itself.
/// Test: `force_decommission_still_refuses_user_work`.
fn blocking_entries(ws: &Path, policy: ProvisioningDirt) -> String {
    let args = [
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ];
    let Ok(status) = super::worktree_safety::git_stdout(ws, &args) else {
        return String::new();
    };
    let entries: Vec<&str> = status
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| {
            let path = line.get(3..).unwrap_or("").trim();
            path != WORKTREE_SENTINEL_FILE
                && path != ".trusty-mpm"
                && !path.starts_with(".trusty-mpm/")
        })
        .filter(|line| policy == ProvisioningDirt::Refuse || !is_provisioning_entry(ws, line))
        .collect();
    if entries.is_empty() {
        return String::new();
    }
    let mut named = entries
        .iter()
        .take(NAMED_ENTRIES_CAP)
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(", ");
    if entries.len() > NAMED_ENTRIES_CAP {
        named.push_str(&format!(", and {} more", entries.len() - NAMED_ENTRIES_CAP));
    }
    format!(": {named}")
}

/// Why `--force` may not act on `ws`, or `None` when tm provably created it
/// as a linked worktree that nothing locks (#7660).
///
/// Why: `--force` is followed by `git worktree remove --force`, which destroys
/// the files it excused. It is only safe on a tree tm itself provisioned for a
/// session — never on a repository's main checkout, a tree tm cannot vouch for,
/// or one a `git worktree lock` protects.
/// What: three probes, each failing closed with a named reason: (1) the
/// ownership sentinel [`WORKTREE_SENTINEL_FILE`] that `create_session_worktree`
/// writes is present; (2) `git rev-parse` reports `ws` as its own worktree root
/// whose git dir differs from the common dir — a linked worktree, not a main
/// checkout; (3) that git dir holds no `locked` file ([`lock_blocker`]).
/// Test: `force_decommission_keeps_a_worktree_without_the_sentinel`,
/// `force_decommission_keeps_a_worktree_whose_sentinel_cannot_be_read`,
/// `force_decommission_keeps_a_main_checkout_under_the_worktrees_dir`,
/// `force_decommission_keeps_a_worktree_git_cannot_resolve`,
/// `force_decommission_keeps_a_locked_worktree`,
/// `lock_blocker_fails_closed_when_the_lock_state_cannot_be_read`.
pub(crate) fn force_blocker(ws: &Path) -> Option<String> {
    let sentinel = ws.join(WORKTREE_SENTINEL_FILE);
    match std::fs::symlink_metadata(&sentinel) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return Some(format!(
                "cannot prove tm created it: {WORKTREE_SENTINEL_FILE} is not a regular file"
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Some(format!(
                "cannot prove tm created it: it carries no ownership sentinel \
                 ({WORKTREE_SENTINEL_FILE})"
            ));
        }
        Err(e) => {
            return Some(format!(
                "cannot prove tm created it: its ownership sentinel could not be read: {e}"
            ));
        }
    }
    match linked_worktree_git_dir(ws) {
        Ok(git_dir) => lock_blocker(&git_dir),
        Err(reason) => Some(reason),
    }
}

/// The git dir of `ws` when `ws` is the root of a LINKED worktree (#7660).
///
/// What: one `git rev-parse --path-format=absolute --show-toplevel --git-dir
/// --git-common-dir` through the hardened [`super::worktree_safety::git_stdout`].
/// A failed or malformed answer, a toplevel other than `ws`, and a git dir
/// equal to the common dir (a main checkout) are each an `Err` naming why.
/// Test: `force_decommission_keeps_a_main_checkout_under_the_worktrees_dir`,
/// `force_decommission_keeps_a_worktree_git_cannot_resolve`.
fn linked_worktree_git_dir(ws: &Path) -> Result<PathBuf, String> {
    let args = [
        "rev-parse",
        "--path-format=absolute",
        "--show-toplevel",
        "--git-dir",
        "--git-common-dir",
    ];
    let out = super::worktree_safety::git_stdout(ws, &args)
        .map_err(|e| format!("cannot prove it is a tm-created linked worktree: {e}"))?;
    let lines: Vec<&str> = out.lines().collect();
    let [top, git_dir, common] = lines.as_slice() else {
        return Err(format!(
            "cannot prove it is a tm-created linked worktree: unexpected `git rev-parse` \
             output {out:?}"
        ));
    };
    let resolve = |p: &Path| {
        std::fs::canonicalize(p).map_err(|e| {
            format!(
                "cannot prove it is a tm-created linked worktree: {} could not be resolved: {e}",
                p.display()
            )
        })
    };
    if resolve(Path::new(top))? != resolve(ws)? {
        return Err(format!(
            "it is not a worktree root (git reports the root as {top})"
        ));
    }
    let git_dir = resolve(Path::new(git_dir))?;
    if git_dir == resolve(Path::new(common))? {
        return Err(
            "it is a repository's main checkout, not a linked worktree tm created; a main \
             checkout is never removed"
                .to_string(),
        );
    }
    Ok(git_dir)
}

/// Why a worktree whose git dir is `git_dir` is locked, or `None` when it is
/// not (#7660).
///
/// What: `git worktree lock` writes `<git-dir>/locked`. Present — locked, with
/// its reason when readable; absent — unlocked; any other stat error fails
/// closed, since an unreadable lock state is never proof of no lock.
/// Test: `force_decommission_keeps_a_locked_worktree`,
/// `lock_blocker_fails_closed_when_the_lock_state_cannot_be_read`.
pub(crate) fn lock_blocker(git_dir: &Path) -> Option<String> {
    let lock = git_dir.join("locked");
    match std::fs::symlink_metadata(&lock) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Ok(_) => {
            let why = std::fs::read_to_string(&lock).unwrap_or_default();
            let why = why.trim();
            Some(if why.is_empty() {
                "it is locked (`git worktree lock`)".to_string()
            } else {
                format!("it is locked (`git worktree lock`: {why})")
            })
        }
        Err(e) => Some(format!(
            "its lock state could not be read ({}): {e}",
            lock.display()
        )),
    }
}

/// Why decommission keeps a workspace tm never removes, or `None` when nothing
/// is on disk (#7660).
///
/// Why: a launch-on-main session's workspace is the project's shared managed
/// checkout (`launch_on_main.rs`, `workspace_owned: false`), and a local-path
/// or adopted session's is the user's own directory. Decommission keeps both by
/// design, but said nothing, so the CLI exited 0 and `--force` was silently
/// ignored.
/// What: one line naming what the workspace is — a main checkout (`.git` is a
/// directory), a worktree tm did not create (`.git` is a file), or another
/// directory — and, under `--force`, that the flag does not apply.
/// Test: `unowned_kept_reason_names_a_main_checkout`,
/// `session_decommission_routed_keeps_the_shared_main_checkout_under_force`,
/// `session_decommission_routed_says_why_it_kept_the_main_checkout`.
pub(crate) fn unowned_kept_reason(ws: &Path, policy: ProvisioningDirt) -> Option<String> {
    if std::fs::symlink_metadata(ws).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return None;
    }
    let kind = match std::fs::symlink_metadata(ws.join(".git")) {
        Ok(meta) if meta.is_dir() => {
            "a repository's main checkout, shared by the project's sessions"
        }
        Ok(_) => "a git worktree tm did not create for this session",
        Err(_) => "a local-path or adopted directory tm did not create",
    };
    let force = match policy {
        ProvisioningDirt::Refuse => "",
        ProvisioningDirt::Discard => "; --force does not apply to it",
    };
    Some(format!(
        "kept by design: {} is {kind}, and decommission never deletes it{force}",
        ws.display()
    ))
}

#[cfg(test)]
#[path = "decommission_force_tests.rs"]
mod decommission_force_tests;
