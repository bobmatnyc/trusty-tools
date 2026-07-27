//! Pre-deletion safety checks for an orphaned-worktree reclaim candidate
//! (#3649 `git worktree list` cross-check, extended #4091 dirty-tree guard).
//!
//! Why: before #4091 the reclaim path's ENTIRE safety model was "who owned
//! this directory" — the #3649 ownership sentinel plus the
//! [`git_worktree_list_agrees`] cross-check. Neither asks the one question an
//! operator actually cares about: *is there unsaved work in here*. A worktree
//! with a KNOWN owner that passed the owner-terminal + grace-window gates was
//! force-deleted (`git worktree remove --force`, falling back to
//! `fs::remove_dir_all`) with uncommitted edits still in it, silently. Worse,
//! `decommission::remove_session_worktree` also runs
//! `git branch -D session/<leaf>` on success, so commits made on the
//! worktree's own branch and never pushed lose their last reachable ref too —
//! committed work is genuinely destroyed by a reclaim, not merely detached.
//! Ownership answers "may I delete this"; it must not also have to answer "is
//! this safe to delete".
//! What: [`DirtyWorktreePolicy`] (skip vs. explicit force-discard),
//! [`DirtyWorktree`] (a reportable skip record), [`inspect_dirt`] (the check
//! itself), and [`git_worktree_list_agrees`] (moved here verbatim from
//! `prune.rs`, which is at its 500-SLOC cap — these are the same category of
//! check and belong together).
//!
//! FAIL-SAFE DIRECTION: every error path in this module resolves to DIRTY.
//! A git subprocess that cannot be spawned, exits non-zero, or emits an
//! unparsable count, and a directory that cannot be read, all mean "this
//! check could not prove the directory is safe to delete" — which must never
//! become a green light to delete. That inverts the whole point.
//! Test: `worktree_safety_tests`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// What the reclaim path does when a candidate holds unsaved work (#4091).
///
/// Why: the default must be impossible to trigger accidentally — an ordinary
/// `/tm-session-pause` (which prunes by DEFAULT, `prune_worktrees` unwrapping
/// to `true`) must never be able to discard uncommitted work. Modelling the
/// override as its own two-variant enum rather than a `force: bool` keeps it
/// from being positionally confused with the `dry_run: bool` that already sits
/// next to it in [`super::prune::SessionManager::prune_orphaned_worktrees`]'s
/// signature, where a swapped argument would mean "really delete, and discard
/// dirty work" instead of "preview".
/// What: [`Skip`](Self::Skip) (the [`Default`]) refuses to remove a dirty
/// candidate and reports it; [`ForceDiscard`](Self::ForceDiscard) removes it
/// anyway, and is reachable ONLY from the explicit `discard_dirty` flag on the
/// `prune-worktrees` HTTP route / `tm session prune-worktrees --discard-dirty`.
/// Test: `dirty_policy_defaults_to_skip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirtyWorktreePolicy {
    /// Refuse to remove a candidate holding unsaved work; report it instead.
    #[default]
    Skip,
    /// Explicit opt-in: remove the candidate even though work will be lost.
    ForceDiscard,
}

/// A reclaim candidate that was skipped because it holds unsaved work (#4091).
///
/// Why: "never silently" is half the fix. A skipped worktree that only shows
/// up as a log line is invisible to the MCP caller, the HTTP client, and the
/// `/tm-session-pause` skill — so the operator sweeping ~95 worktrees would
/// have no idea which ones still hold work. This is the structured, wire-
/// serializable form that every surface returns.
/// What: the candidate `path`, a human-readable `reason`, and the two counts
/// behind that reason — `dirty_files` (working-tree entries reported by
/// `git status --porcelain`, i.e. modified/staged tracked files PLUS
/// untracked-but-not-ignored files) and `unpushed_commits`.
/// Test: `inspect_dirt_reports_modified_tracked_file`,
/// `inspect_dirt_reports_untracked_file`, `inspect_dirt_reports_unpushed_commit`.
#[derive(Debug, Clone, Serialize)]
pub struct DirtyWorktree {
    /// The candidate directory that was NOT removed.
    pub path: PathBuf,
    /// Human-readable explanation of what was found (or why it is unknown).
    pub reason: String,
    /// Working-tree entries from `git status --porcelain` (0 when unknown).
    pub dirty_files: usize,
    /// Commits on `HEAD` not present on the upstream / any remote (0 when unknown).
    pub unpushed_commits: usize,
}

impl DirtyWorktree {
    /// Build a skip record for `path` with an explicit reason and counts.
    fn new(path: &Path, reason: impl Into<String>, dirty_files: usize, unpushed: usize) -> Self {
        Self {
            path: path.to_path_buf(),
            reason: reason.into(),
            dirty_files,
            unpushed_commits: unpushed,
        }
    }
}

/// Does this reclaim candidate hold work that removal would destroy (#4091)?
///
/// Why: this is the check the reclaim path never had. It is deliberately the
/// LAST gate — it runs only on candidates the #3649 ownership gate has already
/// approved, so it is additive and cannot weaken that guard.
/// What: returns `None` only when the directory is PROVABLY free of unsaved
/// work; `Some(DirtyWorktree)` in every other case, including every error.
/// "Dirty" means any of:
/// 1. `git status --porcelain` reports at least one entry — modified or staged
///    tracked files AND untracked-but-not-ignored files, in one command
///    (ignored files are excluded by design: build artefacts are not work, and
///    so are trusty-mpm's own untracked artefacts — see [`is_tool_bookkeeping`]);
/// 2. at least one commit on `HEAD` is not on the branch's upstream (when one
///    is configured) or, with no upstream, is not on ANY remote-tracking ref.
///    This second leg matters because `remove_session_worktree` deletes the
///    worktree's `session/<leaf>` branch, so unpushed commits lose their last
///    reachable ref;
/// 3. the directory is not a git worktree ROOT at all and is not empty — see
///    [`non_git_dirt`];
/// 4. any check errored — see the module-level FAIL-SAFE note.
///
/// The worktree-root identity check is not incidental: running
/// `git status` inside a plain directory that happens to sit INSIDE a checkout
/// would report the ENCLOSING repository's status, which is both wrong and
/// misleading, so the toplevel must canonicalize to the candidate itself.
/// Test: `inspect_dirt_clean_pushed_worktree_is_none`,
/// `inspect_dirt_reports_modified_tracked_file`,
/// `inspect_dirt_reports_untracked_file`,
/// `inspect_dirt_reports_unpushed_commit`,
/// `inspect_dirt_treats_missing_path_as_dirty`,
/// `inspect_dirt_treats_non_worktree_with_files_as_dirty`,
/// `inspect_dirt_allows_empty_non_git_leftover`.
pub(crate) fn inspect_dirt(path: &Path) -> Option<DirtyWorktree> {
    match is_worktree_root(path) {
        Ok(true) => {}
        // Not a git worktree root (or git cannot tell us) — fall back to the
        // "is it even empty" question, the only one answerable without git.
        Ok(false) | Err(_) => return non_git_dirt(path),
    }

    let status = match git_stdout(path, &["status", "--porcelain"]) {
        Ok(s) => s,
        Err(e) => {
            return Some(DirtyWorktree::new(
                path,
                format!("dirty-check failed: {e}"),
                0,
                0,
            ));
        }
    };
    let dirty_files = status
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !is_tool_bookkeeping(l))
        .count();

    let unpushed = match count_unpushed(path) {
        Ok(n) => n,
        Err(e) => {
            return Some(DirtyWorktree::new(
                path,
                format!("dirty-check failed: {e}"),
                dirty_files,
                0,
            ));
        }
    };

    if dirty_files == 0 && unpushed == 0 {
        return None;
    }
    let reason =
        format!("{dirty_files} uncommitted/untracked file(s), {unpushed} unpushed commit(s)");
    Some(DirtyWorktree::new(path, reason, dirty_files, unpushed))
}

/// Is this `git status --porcelain` line trusty-mpm's OWN bookkeeping rather
/// than somebody's work (#4091)?
///
/// Why: this exclusion is what keeps the guard from degenerating into "never
/// reclaim anything". trusty-mpm writes two artefacts into EVERY managed
/// worktree it creates — the `.trusty-mpm-worktree` ownership sentinel
/// (`create_session_worktree`) and the `.trusty-mpm/` scrollback-snapshot
/// directory (`snapshot::write_scrollback`) — and neither is typically
/// gitignored by the host project. Counting them would mark every single
/// managed worktree permanently dirty, which is worse than useless: it stops
/// all reclamation AND buries the genuinely-dirty worktrees in noise, which is
/// how a safety report gets ignored.
/// What: excuses a line ONLY when it is UNTRACKED (`??`) and its path is the
/// sentinel file or lives under `.trusty-mpm/`. A TRACKED modification is
/// never excused, no matter its path — if a project tracks
/// `.trusty-mpm/INSTRUCTIONS.md` and someone edited it, that is real work.
/// Test: `inspect_dirt_excludes_own_sentinel`,
/// `inspect_dirt_excludes_untracked_trusty_mpm_dir`,
/// `inspect_dirt_counts_tracked_trusty_mpm_edit`.
fn is_tool_bookkeeping(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("?? ") else {
        return false;
    };
    // Git quotes paths containing unusual characters; neither artefact's name
    // ever needs quoting, so a quoted path is by definition something else.
    let path = rest.trim();
    path == super::decommission::WORKTREE_SENTINEL_FILE
        || path == ".trusty-mpm/"
        || path.starts_with(".trusty-mpm/")
}

/// Classify a candidate that is NOT a git worktree root (#4091).
///
/// Why: the orphan sweep's original purpose (#1838) is reclaiming the leftover
/// `.worktrees/<id>` SHELLS that accumulate when `git worktree remove` never
/// ran or half-ran — one project grew 94 of them. Those shells carry no git
/// metadata, so no git-based dirty check can speak to them, and treating every
/// one as dirty would neuter reclamation entirely (the guard must not become a
/// permanent leak). But a non-git directory that still holds FILES is exactly
/// the case we cannot assess, and the fail-safe direction is to keep it.
/// What: `None` when the directory holds nothing but the trusty-mpm ownership
/// sentinel (an empty shell — provably no work); `Some` otherwise, including
/// when the directory cannot be read at all.
/// Test: `inspect_dirt_allows_empty_non_git_leftover`,
/// `inspect_dirt_treats_non_worktree_with_files_as_dirty`,
/// `inspect_dirt_treats_missing_path_as_dirty`.
fn non_git_dirt(path: &Path) -> Option<DirtyWorktree> {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            return Some(DirtyWorktree::new(
                path,
                format!("not a git worktree and the directory is unreadable: {e}"),
                0,
                0,
            ));
        }
    };
    let mut count = 0usize;
    for entry in entries {
        let Ok(entry) = entry else {
            return Some(DirtyWorktree::new(
                path,
                "not a git worktree and a directory entry is unreadable",
                count,
                0,
            ));
        };
        if entry.file_name() == super::decommission::WORKTREE_SENTINEL_FILE {
            continue;
        }
        count += 1;
    }
    if count == 0 {
        return None;
    }
    Some(DirtyWorktree::new(
        path,
        format!(
            "not a git worktree root, but holds {count} entr(y/ies) whose contents \
             cannot be verified as saved"
        ),
        count,
        0,
    ))
}

/// Does `path` canonicalize to its own git worktree toplevel?
///
/// Why: `git -C <dir> status` happily answers for an ENCLOSING repository when
/// `dir` is merely nested inside one, so an identity check is required before
/// any status output can be attributed to the candidate itself.
/// What: `Ok(true)` when `git rev-parse --show-toplevel` canonicalizes to the
/// same path as `path`; `Ok(false)` when it resolves elsewhere; `Err` when git
/// cannot answer (not a repository, git missing, spawn failure).
/// Test: `inspect_dirt_treats_non_worktree_with_files_as_dirty` (the nested
/// plain-directory case), `inspect_dirt_clean_pushed_worktree_is_none`.
fn is_worktree_root(path: &Path) -> Result<bool, String> {
    let top = git_stdout(path, &["rev-parse", "--show-toplevel"])?;
    let top = PathBuf::from(top.trim());
    if top.as_os_str().is_empty() {
        return Ok(false);
    }
    let resolved_top = std::fs::canonicalize(&top).unwrap_or(top);
    let resolved_self =
        std::fs::canonicalize(path).map_err(|e| format!("candidate is unreadable: {e}"))?;
    Ok(resolved_top == resolved_self)
}

/// Count commits on `HEAD` that are not yet safely on a remote (#4091).
///
/// Why: `decommission::remove_session_worktree` deletes the worktree's
/// `session/<leaf>` branch after a successful removal, so a commit that exists
/// only on that branch loses its last reachable ref and becomes garbage-
/// collectable. "Committed" is therefore NOT the same as "safe".
/// What: when the checked-out branch has an upstream, counts
/// `<upstream>..HEAD`. With NO upstream configured, counts commits reachable
/// from `HEAD` but from no remote-tracking ref at all
/// (`rev-list --count HEAD --not --remotes`) — which subsumes the
/// "not on `origin/main`" rule (a commit absent from `origin/main` AND every
/// other remote ref is counted) while not falsely flagging a branch that WAS
/// pushed under its own name but never had upstream tracking configured. In a
/// repository with no remotes at all, `--remotes` expands to nothing and every
/// commit counts as unpushed — the correct, conservative answer.
/// Test: `inspect_dirt_reports_unpushed_commit`,
/// `inspect_dirt_clean_pushed_worktree_is_none`.
fn count_unpushed(path: &Path) -> Result<usize, String> {
    let upstream = git_stdout(
        path,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());

    let raw = match upstream {
        Some(up) => git_stdout(path, &["rev-list", "--count", &format!("{up}..HEAD")])?,
        None => git_stdout(path, &["rev-list", "--count", "HEAD", "--not", "--remotes"])?,
    };
    raw.trim()
        .parse::<usize>()
        .map_err(|e| format!("unparsable rev-list count `{}`: {e}", raw.trim()))
}

/// Run `git -C <dir> <args>` and return stdout, or an error string.
///
/// Why: every check in this module needs the same "ran, exited zero, gave me
/// stdout" contract, and every deviation from it must surface as an `Err` the
/// caller turns into DIRTY rather than being swallowed.
/// What: non-zero exit and spawn failure both become `Err` carrying the
/// command and git's own stderr; stdout is lossily decoded.
/// Test: `inspect_dirt_treats_missing_path_as_dirty`.
fn git_stdout(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("`git {}` could not be run: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "`git {}` failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Best-effort cross-check: does `git worktree list` on the checkout owning
/// `candidate` agree that `candidate` is a real, currently-registered git
/// worktree (#3649)?
///
/// Why: the sentinel + store-ownerless checks establish WHO owned this
/// directory and whether that owner is provably gone, but neither confirms
/// git's OWN bookkeeping still recognises the path as a worktree at all — a
/// belt-and-suspenders safety net against deleting a directory that merely
/// LOOKS like a worktree (e.g. its git worktree entry was already pruned by
/// something else, or the shape matched by coincidence). A disagreement is
/// treated conservatively: skip rather than delete.
/// What: runs `git -C <repo_root> worktree list --porcelain`, where
/// `repo_root` is `candidate`'s grandparent directory — the SAME derivation
/// `decommission::remove_session_worktree` uses, which works identically for
/// both worktree-store shapes (`<repo>/.worktrees/<name>` and
/// `<repo>/.base/.worktrees/<id>`, since either way the grandparent of the
/// worktree leaf is the git checkout root). Returns `true` (agree — deletion
/// may proceed, subject to the caller's other checks) when the git command
/// cannot be run or fails outright — this check is an ADDITIONAL safety net
/// on top of the sentinel/store checks, not a replacement for them, so a
/// missing `git` binary or a transient failure never blocks a deletion those
/// checks already approved. It is emphatically NOT the fail-safe gate; that
/// role belongs to [`inspect_dirt`], which fails toward DIRTY. Returns `true`
/// only when `candidate`'s canonicalized path appears among the porcelain
/// output's `worktree <path>` lines.
/// Test: `git_worktree_list_agrees_true_for_real_worktree`,
///       `git_worktree_list_agrees_false_for_untracked_dir`.
pub(crate) fn git_worktree_list_agrees(candidate: &Path) -> bool {
    let Some(repo_root) = candidate.parent().and_then(|p| p.parent()) else {
        return true;
    };
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output();
    let Ok(out) = out else {
        return true; // best-effort: git unavailable must never block a delete
    };
    if !out.status.success() {
        return true;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let canonical_candidate =
        std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| {
            let pb = PathBuf::from(p);
            std::fs::canonicalize(&pb).unwrap_or(pb) == canonical_candidate
        })
}

#[cfg(test)]
#[path = "worktree_safety_tests.rs"]
mod worktree_safety_tests;
