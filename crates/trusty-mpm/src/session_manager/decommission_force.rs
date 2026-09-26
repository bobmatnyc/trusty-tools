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
//! [`ProvisioningDirt::Discard`], under which tm's provisioning writes are
//! excused only in the exact state provisioning leaves them
//! ([`is_provisioning_entry`]): `.gitignore`, `.claude/settings.json`,
//! `.claude/settings.json.bak`, `CLAUDE.md`, a timestamped
//! `.claude/settings.json.<timestamp>.bak` snapshot, and a `TASK.md` whose
//! bytes still equal the task tm wrote (#8688). Unpushed commits, any other
//! modified or untracked file, an edit to a tracked provisioning path,
//! nested-repository work, and every check that cannot complete still keep
//! the workspace.
//! When its excuse is needed, `--force` acts only on a linked worktree tm
//! provably created that nothing locks ([`force_blocker`]); on a clean tree it
//! is never stricter than no flag. A workspace tm never removes — the shared
//! main checkout a launch-on-main session runs in — is kept with a named
//! reason ([`unowned_kept_reason`]): a by-design note without `--force`, and a
//! refusal that makes the CLI exit non-zero only under `--force`.
//! Test: `force_decommission_removes_a_provisioning_only_worktree`,
//! `force_decommission_keeps_a_task_md_edited_after_spawn`,
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
use super::worktree_ownership::SentinelOwner;
use super::worktree_ownership_location::{
    OwnerReadError, admin_sentinel_path, legacy_sentinel_path, read_sentinel_owner_strict,
};
use super::worktree_safety::{DirtyWorktreePolicy, ExcuseEntry, inspect_dirt_excusing};
use crate::core::standalone::hooks::backup;

/// The paths tm's own provisioning writes into a workspace (#7660), as the
/// kept reason names them.
///
/// Why: these are the entries `git status --porcelain` shows on a workspace no
/// user had touched. Nothing else is excused: a broader list would let
/// `--force` discard work. #8688: a task-bearing spawn also writes `TASK.md`
/// and a timestamped `.claude/settings.json` snapshot.
pub(crate) const PROVISIONING_FILES: [&str; 6] = [
    ".gitignore",
    ".claude/settings.json",
    ".claude/settings.json.bak",
    ".claude/settings.json.<timestamp>.bak",
    "CLAUDE.md",
    // #8688: excused only while it holds the task tm wrote.
    "unedited TASK.md",
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
/// `#[non_exhaustive]` (owner ruling 2026-09-24): a field added later is not
/// a semver break; only this crate constructs it.
/// Test: `decommission_reports_why_it_kept_a_provisioned_worktree`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DecommissionReport {
    /// The tombstoned record.
    pub record: SessionRecord,
    /// `true` only when the directory was removed by this call.
    pub workspace_removed: bool,
    /// Why the workspace was kept; `None` when it was removed or is absent.
    /// #7660: a workspace tm never removes (a main checkout, a local-path or
    /// adopted directory) is a refusal here only under `--force`.
    pub workspace_kept_reason: Option<String>,
    /// #7660: why a plain decommission kept a workspace tm never removes (a
    /// main checkout, a local-path or adopted directory). Not a failure.
    pub workspace_kept_by_design: Option<String>,
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
/// What: the [`UNTRACKED_PROVISIONING_FILES`] and a `.claude/settings.json`
/// snapshot ([`is_settings_snapshot`]) are excused only as `??`. `TASK.md` is
/// excused only as `??` and only when [`task_md_is_unedited`] against `task`,
/// the session record's task. `.gitignore` is excused as ` M` only when its
/// unstaged diff adds nothing but the lines provisioning writes and removes
/// nothing, and as `??` only when every line of it is such a line. Anything
/// else — staged, deleted, renamed, conflicted, or an unreadable diff — is not
/// excused.
/// Test: `provisioning_entry_matches_only_the_four_paths_in_provisioning_states`,
/// `provisioning_entry_excuses_task_md_and_settings_snapshots_only_when_untracked`,
/// `provisioning_entry_excuses_task_md_only_when_it_equals_the_session_task`,
/// `force_decommission_keeps_an_edited_tracked_claude_md`,
/// `force_decommission_keeps_a_gitignore_with_a_non_provisioning_line`,
/// `force_decommission_removes_a_tree_with_an_untracked_scaffold_gitignore`,
/// `force_decommission_keeps_an_untracked_gitignore_with_a_user_line`.
pub(crate) fn is_provisioning_entry(ws: &Path, task: Option<&str>, line: &str) -> bool {
    let (Some(status), Some(path)) = (line.get(..3), line.get(3..)) else {
        return false;
    };
    match (status, path.trim()) {
        // #7660: `.gitignore` is checked line by line, never by path alone.
        (" M ", ".gitignore") => gitignore_diff_is_provisioning(ws),
        ("?? ", ".gitignore") => std::fs::read_to_string(ws.join(".gitignore"))
            .is_ok_and(|body| body.lines().all(is_provisioning_gitignore_line)),
        // #8688: agents may write to `TASK.md`, so it is checked by content.
        ("?? ", TASK_MD) => task_md_is_unedited(ws, task),
        ("?? ", path) => UNTRACKED_PROVISIONING_FILES.contains(&path) || is_settings_snapshot(path),
        _ => false,
    }
}

/// The task brief the daemon writes at spawn (`write_task_md`, #1693).
const TASK_MD: &str = "TASK.md";

/// Whether `ws/TASK.md` still holds exactly the bytes tm wrote there (#8688).
///
/// Why: PMs and agents are told they may write to `TASK.md`, and `--force`
/// deletes whatever it excuses, so excusing it by path lost their notes.
/// What: `task` is the session record's task. `write_task_md` wrote the task
/// verbatim at spawn, so the file must equal `task` or, for a session errored
/// once, [`task_before_single_error_note`]. True only when a non-empty
/// candidate has the file's exact length and bytes, and `TASK.md` is a regular
/// file. The length is checked first and the read is bounded by it, so a large
/// file is never loaded. No record, an empty task, an unreadable file or any
/// byte difference is `false`, so the file keeps the worktree.
/// Test: `provisioning_entry_excuses_task_md_only_when_it_equals_the_session_task`,
/// `force_decommission_keeps_a_task_md_edited_after_spawn`,
/// `force_decommission_keeps_a_task_md_without_a_session_task`,
/// `force_decommission_removes_a_worktree_holding_task_md_and_a_settings_snapshot`,
/// `force_decommission_removes_a_once_errored_session_s_unedited_task_md`,
/// `force_decommission_keeps_a_repeatedly_errored_session_s_task_md`.
fn task_md_is_unedited(ws: &Path, task: Option<&str>) -> bool {
    use std::io::Read;
    let Some(task) = task else {
        return false;
    };
    let path = ws.join(TASK_MD);
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return false;
    };
    // #8688: the two candidates differ in length, so at most one can match.
    let Some(want) = [Some(task), task_before_single_error_note(task)]
        .into_iter()
        .flatten()
        .find(|c| !c.is_empty() && meta.is_file() && meta.len() == c.len() as u64)
    else {
        return false;
    };
    let mut bytes = Vec::with_capacity(want.len());
    std::fs::File::open(&path)
        .and_then(|file| file.take(meta.len() + 1).read_to_end(&mut bytes))
        .is_ok()
        && bytes == want.as_bytes()
}

/// The note `mark_errored` appends to a record's task: `" [error: {msg}]"`.
const ERROR_NOTE_OPEN: &str = " [error: ";

/// The task `write_task_md` wrote, recovered from a record `mark_errored`
/// changed exactly once (#8688).
///
/// Why: `mark_errored` appends a note to `record.task` after `TASK.md` was
/// written, so an errored session's unedited `TASK.md` never equals its task.
/// `strip_error_notes` is not an exact inverse: a task or message holding
/// note-shaped text strips to a string tm never wrote.
/// What: when `task` holds exactly one [`ERROR_NOTE_OPEN`] and ends with `]`,
/// the text before that marker. One marker means one `mark_errored` call, and
/// neither the original task nor the message held a marker, so the prefix is
/// exactly what was written. Markers are counted with overlaps: a task ending
/// in ` [error:` shares its space with the appended note, and non-overlapping
/// counting would see one marker and cut the task short. Two or more markers,
/// or none, give `None` and the tree is kept. A task `clear_error_note`
/// rewrote gets no candidate either.
/// Test: `force_decommission_removes_a_once_errored_session_s_unedited_task_md`,
/// `force_decommission_keeps_a_repeatedly_errored_session_s_task_md`.
fn task_before_single_error_note(task: &str) -> Option<&str> {
    // #8688: only the single-error case is exact; anything else fails closed.
    let mut markers = task.char_indices().map(|(pos, _)| pos).filter(|&pos| {
        task.get(pos..)
            .is_some_and(|s| s.starts_with(ERROR_NOTE_OPEN))
    });
    match (markers.next(), markers.next()) {
        (Some(pos), None) if task.ends_with(']') => task.get(..pos),
        _ => None,
    }
}

/// Whether the repo-relative `path` is a `.claude/settings.json` snapshot the
/// hooks writers take before a rewrite (#8688).
///
/// What: `.claude/<name>` where `name` passes the snapshot pruner's own exact
/// name rule, [`backup::is_snapshot_of`] — no other file is matched.
/// Test: `provisioning_entry_excuses_task_md_and_settings_snapshots_only_when_untracked`.
fn is_settings_snapshot(path: &str) -> bool {
    // #8688: the pruner's rule, so the excuse and the writer cannot drift.
    path.strip_prefix(".claude/")
        .is_some_and(|name| backup::is_snapshot_of("settings.json", name))
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
    gitignore_diff_only_adds(ws, &is_provisioning_gitignore_line)
}

/// Whether `ws`'s unstaged `.gitignore` diff adds at least one line, removes
/// none, and every added line passes `accept` (#7660, #8663).
///
/// What: the parser behind [`gitignore_diff_is_provisioning`], shared with the
/// provisioning ledger, which accepts only the lines tm recorded appending.
/// Test: `gitignore_body_line_shaped_like_a_header_is_not_excused`,
/// `ledger_keeps_a_clone_whose_gitignore_gained_a_user_line`.
pub(super) fn gitignore_diff_only_adds(ws: &Path, accept: &dyn Fn(&str) -> bool) -> bool {
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
            Some(body) if accept(body) => added += 1,
            _ => return false,
        }
    }
    added > 0
}

/// Remove an in-project session worktree unless it holds work (#4344, #7660).
///
/// Why: see the module doc. Moved out of `decommission_with_root_checked`
/// unchanged except for the `policy` excuse and the returned reason.
/// What: runs [`keep_reason`] on a blocking thread — a plain dirt check, and
/// only when that finds dirt under [`ProvisioningDirt::Discard`],
/// [`force_blocker`] then [`inspect_dirt_excusing`] with
/// [`is_provisioning_entry`], which checks `TASK.md` against `task`, the
/// session record's task (`None` without a record). Any dirt, a blocker, a
/// panicked check or a failed check keeps the worktree and returns the reason;
/// a clean answer removes it through [`remove_session_worktree_guarded`]. Under
/// `Discard` its guard re-asks the same questions immediately before
/// `git worktree remove --force`. An absent path is neither removed nor kept.
/// Test: `force_decommission_removes_a_provisioning_only_worktree`,
/// `force_decommission_removes_nothing_when_the_dirty_check_cannot_complete`,
/// `decommission_reports_why_it_kept_a_provisioned_worktree`,
/// `force_decommission_keeps_a_task_md_edited_after_spawn`.
pub(super) async fn remove_in_project_worktree(
    id: &ManagedSessionId,
    task: Option<&str>,
    ws: &Path,
    policy: ProvisioningDirt,
) -> WorkspaceVerdict {
    // #7660: nothing on disk is nothing kept — an absent tree is not a refusal.
    if std::fs::symlink_metadata(ws).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return WorkspaceVerdict::default();
    }
    let ws_for_check = ws.to_path_buf();
    let owner = *id;
    // #8688: both checks compare `TASK.md` with the task tm wrote.
    let task_for_check = task.map(str::to_owned);
    let task_for_guard = task_for_check.clone();
    let kept = tokio::task::spawn_blocking(move || {
        keep_reason(&ws_for_check, &owner, task_for_check.as_deref(), policy)
    })
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
            ProvisioningDirt::Discard => {
                keep_reason(&ws_clone, &owner, task_for_guard.as_deref(), policy)
            }
        };
        // #7885: name the route in the audit line.
        remove_session_worktree_guarded(
            &ws_clone,
            "session decommission: the session ended and its tree is clean",
            &guard,
            // #8534: `--force` excuses provisioning files only, never run output.
            DirtyWorktreePolicy::Skip,
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
/// What: a tree the plain dirt check passes may be removed under either
/// policy — `--force` is never stricter than no flag (#7660 critic round 3).
/// Only when `--force`'s excuse is needed does [`force_blocker`] run: it acts
/// only on a worktree tm provably created and nothing locks. Then the dirt
/// `policy` does not excuse keeps it, named file by file. `id` is the session
/// being decommissioned and `task` its record's task, which `TASK.md` must
/// still equal to be excused (#8688).
/// Test: `force_decommission_is_never_stricter_than_plain_on_a_clean_tree`,
/// `force_decommission_keeps_a_worktree_without_the_sentinel`,
/// `decommission_refusal_count_matches_the_entries_it_lists`,
/// `force_decommission_keeps_a_task_md_edited_after_spawn`.
fn keep_reason(
    ws: &Path,
    id: &ManagedSessionId,
    task: Option<&str>,
    policy: ProvisioningDirt,
) -> Option<String> {
    // #8688: counted per file, as `kept_for_dirt` lists them — a collapsed
    // `?? .claude/` counted one entry where the list named three.
    let nothing = |_: &str| false;
    let plain = inspect_dirt_excusing(ws, &nothing)?;
    if policy == ProvisioningDirt::Refuse {
        return Some(kept_for_dirt(ws, &plain.reason, policy, &nothing));
    }
    if let Some(blocker) = force_blocker(ws, id) {
        return Some(format!("--force declined: {blocker}; nothing was removed"));
    }
    let excuse = |line: &str| is_provisioning_entry(ws, task, line);
    inspect_dirt_excusing(ws, &excuse).map(|d| kept_for_dirt(ws, &d.reason, policy, &excuse))
}

/// The untracked provisioning files `--force` excuses by path alone, never by
/// content, so an edit to one is lost with the worktree. See #8540.
// #8688: `TASK.md` is content-checked (`task_md_is_unedited`), so not listed.
const CONTENT_UNCHECKED_FILES: [&str; 2] = [".claude/settings.json", "CLAUDE.md"];

/// The operator-facing reason a dirty worktree was kept (#7660).
///
/// What: `reason` is the dirt check's own summary; the entries it names are
/// the ones `excuse` — the same excuse that check counted with — does not
/// accept, so the count and the list agree (#8688).
/// Test: `decommission_refusal_warns_force_discards_untracked_claude_md_edits`,
/// `decommission_refusal_warning_names_a_single_untracked_file`,
/// `decommission_refusal_omits_the_warning_without_untracked_claude_files`,
/// `decommission_refusal_count_matches_the_entries_it_lists`,
/// `ledger_refusal_lists_only_the_entries_it_counts`.
pub(super) fn kept_for_dirt(
    ws: &Path,
    reason: &str,
    policy: ProvisioningDirt,
    excuse: ExcuseEntry,
) -> String {
    let files = PROVISIONING_FILES.join(", ");
    let entries = dirty_entries(ws);
    // #7660: name what blocked the removal, not only how many entries did.
    let named = blocking_entries(&entries, excuse);
    match policy {
        ProvisioningDirt::Refuse => format!(
            "the dirty-tree guard kept it ({reason}{named}). If the only changes are tm's own \
             provisioning files ({files}), re-run with --force to remove it; --force never \
             discards other changes or unpushed commits{}",
            force_discard_warning(&entries)
        ),
        ProvisioningDirt::Discard => format!(
            "--force excused tm's provisioning files ({files}), but the dirty-tree guard \
             still kept it ({reason}{named}); --force never discards that"
        ),
    }
}

/// How many dirty entries [`blocking_entries`] names before it summarises.
const NAMED_ENTRIES_CAP: usize = 10;

/// `": <entry>, <entry>"` for the [`dirty_entries`] `excuse` does not accept,
/// or `""` when there are none to name (#7660).
///
/// Why: "1 uncommitted/untracked file(s)" tells an operator THAT `--force`
/// refused, not what to look at.
/// What: display only — the keep/remove decision was already made by
/// [`keep_reason`], so a failed status read only drops the list.
/// Test: `force_decommission_still_refuses_user_work`,
/// `force_decommission_keeps_user_work_beside_task_md`.
fn blocking_entries(entries: &[String], excuse: ExcuseEntry) -> String {
    let entries: Vec<&str> = entries
        .iter()
        .map(String::as_str)
        .filter(|line| !excuse(line))
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

/// `ws`'s per-file `git status --porcelain` lines, without the ownership
/// sentinel and `.trusty-mpm/`, which the dirty-tree guard accounts for
/// itself; empty when status cannot be read (display only, #7660).
pub(super) fn dirty_entries(ws: &Path) -> Vec<String> {
    let args = [
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ];
    let Ok(status) = super::worktree_safety::git_stdout(ws, &args) else {
        return Vec::new();
    };
    status
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| {
            let path = line.get(3..).unwrap_or("").trim();
            path != WORKTREE_SENTINEL_FILE
                && path != ".trusty-mpm"
                && !path.starts_with(".trusty-mpm/")
        })
        .map(str::to_string)
        .collect()
}

/// The warning a plain refusal appends when `entries` hold an untracked file
/// `--force` would discard unread, or `""` when they hold none (#7660).
///
/// Why: the owner's interim ruling for 1.7.3 — `--force` excuses these files
/// by path, so notes a user appended to an untracked `CLAUDE.md` are lost.
// See #8540
fn force_discard_warning(entries: &[String]) -> String {
    let named: Vec<&str> = CONTENT_UNCHECKED_FILES
        .into_iter()
        .filter(|file| entries.iter().any(|line| line == &format!("?? {file}")))
        .collect();
    // #7660: an English list — "A" or "A and B" — with a matching pronoun.
    let (list, pronoun) = match named.as_slice() {
        [] => return String::new(),
        [one] => ((*one).to_string(), "it"),
        [init @ .., last] => (format!("{} and {last}", init.join(", ")), "them"),
    };
    format!(
        ". WARNING: --force deletes the untracked {list} with the worktree, including any edits \
         made to {pronoun}; copy out anything you added there first"
    )
}

/// Why `--force` may not act on `ws` for session `id`, or `None` when tm
/// provably created it for `id` as a linked worktree that nothing locks
/// (#7660).
///
/// Why: `--force` is followed by `git worktree remove --force`, which destroys
/// the files it excused. It is only safe on a tree tm itself provisioned for a
/// session — never on a repository's main checkout, a tree tm cannot vouch for,
/// or one a `git worktree lock` protects.
/// What: three probes, each failing closed with a named reason: (1) the
/// ownership marker is present and names no other owner
/// ([`ownership_blocker`]); (2) `git rev-parse`
/// reports `ws` as its own worktree root whose git dir differs from the common
/// dir — a linked worktree, not a main checkout; (3) that git dir holds no
/// `locked` file ([`lock_blocker`]).
/// Test: `force_decommission_keeps_a_worktree_without_the_sentinel`,
/// `force_decommission_keeps_a_main_checkout_under_the_worktrees_dir`,
/// `force_decommission_keeps_a_worktree_git_cannot_resolve`,
/// `force_decommission_keeps_a_locked_worktree`,
/// `lock_blocker_fails_closed_when_the_lock_state_cannot_be_read`,
/// `force_decommission_keeps_a_worktree_another_session_owns`.
pub(crate) fn force_blocker(ws: &Path, id: &ManagedSessionId) -> Option<String> {
    if let Some(reason) = ownership_blocker(ws, id) {
        return Some(reason);
    }
    match linked_worktree_git_dir(ws) {
        Ok(git_dir) => lock_blocker(&git_dir),
        Err(reason) => Some(reason),
    }
}

/// Why `ws`'s ownership marker gives `--force` no licence, or `None` when it
/// proves tm created the tree for session `id` (#7660 critic round 3, #8511).
///
/// Why: since #8511 provisioning writes the marker into the git admin dir
/// only, so a check of the legacy in-tree path alone refused nearly every
/// tm-created worktree.
/// What: any marker location — legacy or admin — that exists as anything but
/// a regular file blocks, because [`read_sentinel_owner_strict`] follows
/// symlinks. Then that strict reader decides: a marker naming `id`, or an
/// owner-less (pre-#3649) one, passes; a marker naming another session or an
/// agent blocks and names that owner; no marker, an unreadable one, a corrupt
/// one, or an unresolvable `.git` blocks.
/// Test: `force_decommission_honours_an_admin_dir_marker`,
/// `force_decommission_keeps_a_worktree_another_session_owns`,
/// `force_decommission_keeps_a_worktree_an_agent_owns`,
/// `force_decommission_removes_a_worktree_its_own_session_owns`,
/// `force_decommission_honours_a_legacy_in_tree_marker`,
/// `force_decommission_keeps_a_worktree_whose_marker_is_a_symlink`,
/// `force_decommission_keeps_a_worktree_whose_marker_is_corrupt`,
/// `force_decommission_keeps_a_worktree_whose_sentinel_cannot_be_read`.
fn ownership_blocker(ws: &Path, id: &ManagedSessionId) -> Option<String> {
    let unproven = |reason: String| Some(format!("cannot prove tm created it: {reason}"));
    for marker in std::iter::once(legacy_sentinel_path(ws)).chain(admin_sentinel_path(ws)) {
        if std::fs::symlink_metadata(&marker).is_ok_and(|meta| !meta.is_file()) {
            return unproven(format!(
                "its ownership sentinel {} is not a regular file",
                marker.display()
            ));
        }
    }
    match read_sentinel_owner_strict(ws) {
        // #7660 critic round 3: the marker must name this session, or no one.
        Ok(Some(SentinelOwner::Unknown)) => None,
        Ok(Some(SentinelOwner::Known(owner, _))) if owner == *id => None,
        Ok(Some(SentinelOwner::Known(owner, _))) => Some(format!(
            "its ownership sentinel names session {owner}, not {id}; --force never removes \
             another session's worktree"
        )),
        Ok(Some(SentinelOwner::Agent(agent, _))) => Some(format!(
            "its ownership sentinel names agent {}, not session {id}; --force never removes \
             an agent's worktree",
            agent.agent_id
        )),
        Ok(None) => unproven(format!(
            "it carries no ownership sentinel (neither in its git admin dir nor as \
             {WORKTREE_SENTINEL_FILE})"
        )),
        Err(OwnerReadError::Unreadable { path, reason }) => unproven(format!(
            "its ownership sentinel could not be read ({}): {reason}",
            path.display()
        )),
        Err(OwnerReadError::Corrupt { path, reason }) => unproven(format!(
            "its ownership sentinel {} is corrupt: {reason}",
            path.display()
        )),
    }
}

/// What `git rev-parse` positively established a worktree root to be (#8663).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorktreeKind {
    /// A linked worktree, with its git dir.
    Linked(PathBuf),
    /// A repository's main checkout: its git dir is the common dir.
    MainCheckout,
}

/// The git dir of `ws` when `ws` is the root of a LINKED worktree (#7660).
///
/// What: [`worktree_kind`], with a main checkout as an `Err` naming why.
/// Test: `force_decommission_keeps_a_main_checkout_under_the_worktrees_dir`,
/// `force_decommission_keeps_a_worktree_git_cannot_resolve`,
/// `force_blocker_refuses_a_directory_that_is_not_a_worktree_root`.
pub(super) fn linked_worktree_git_dir(ws: &Path) -> Result<PathBuf, String> {
    match worktree_kind(ws)? {
        WorktreeKind::Linked(git_dir) => Ok(git_dir),
        WorktreeKind::MainCheckout => Err(
            "it is a repository's main checkout, not a linked worktree tm created; a main \
             checkout is never removed"
                .to_string(),
        ),
    }
}

/// Whether the worktree root `ws` is a linked worktree or a main checkout
/// (#7660, #8663 critic round 2).
///
/// Why: a caller that treats every probe failure as "not linked" skips the
/// lock and `--force` gates on a linked worktree it merely failed to read.
/// Only a positive answer may select a branch.
/// What: one `git rev-parse --path-format=absolute --show-toplevel --git-dir
/// --git-common-dir` through the hardened [`super::worktree_safety::git_stdout`].
/// A failed or malformed answer, a path that cannot be canonicalized, and a
/// toplevel other than `ws` are each an `Err` naming why.
/// Test: `owned_worktree_whose_probe_fails_is_kept`,
/// `force_decommission_keeps_a_worktree_git_cannot_resolve`.
pub(super) fn worktree_kind(ws: &Path) -> Result<WorktreeKind, String> {
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
        return Ok(WorktreeKind::MainCheckout);
    }
    Ok(WorktreeKind::Linked(git_dir))
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
