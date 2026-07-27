//! Nested-repository discovery and loss-model selection for the #4091
//! dirty-worktree guard (split out of `worktree_safety.rs` in the #4118 round 3
//! review, which took that file to its 500-SLOC cap).
//!
//! Why: `git status` on a candidate says NOTHING about a checkout nested inside
//! it, and deleting the candidate deletes that checkout too. Two shapes of
//! nested repository exist beneath a session worktree, they lose completely
//! different things when the candidate is removed, and **assessing one with the
//! other's model manufactures false confidence** — which is worse than not
//! looking at all, because the sweep then deletes with a clean bill of health:
//!
//! - A **registered worktree** of the enclosing repository (the
//!   `.claude/worktrees/<name>` agent shape). Its object store and ref
//!   namespace live in the SHARED `.base/.git`, *outside* the candidate, and
//!   survive its removal. Only its working tree and the branch
//!   `decommission` force-deletes are at risk — the same model that is correct
//!   for the candidate itself.
//! - A **self-contained clone** — someone ran `git clone` into a gitignored
//!   scratch directory. Its `.git` lives INSIDE the candidate, so **every**
//!   local branch, tag, stash entry and reflog dies with it. Asking only about
//!   `HEAD` is close to asking nothing: a clone whose HEAD is on a pushed
//!   `main` while a `feature` branch holds the only copy of a commit reads
//!   perfectly clean.
//!
//! What: [`nested_dirt`] (the entry point), [`nested_repo_roots`] (exact
//! enumeration — registered worktrees from git's own bookkeeping, unregistered
//! clones from a bounded on-disk walk), and the loss-model discriminator
//! [`object_store_dies_with`].
//!
//! The discriminator is `git rev-parse --path-format=absolute --git-common-dir`,
//! and it is a direct test of the question that matters — *does this
//! repository's object store live inside the directory we are about to
//! delete?* — rather than a proxy for how the repository was created. Measured
//! on both shapes under one candidate:
//!
//! ```text
//! candidate           : …/base/.worktrees/candidate
//! registered worktree : …/base/.git                              <- OUTSIDE, survives
//! self-contained clone: …/base/.worktrees/candidate/scratch/work/.git  <- INSIDE, dies
//! ```
//!
//! FAIL-SAFE DIRECTION, unchanged from `worktree_safety`: every error here —
//! an unreadable directory, a git subprocess that will not run, an exhausted
//! scan budget, a path this code cannot spell — resolves to DIRTY.
//! Test: `worktree_nested_tests`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::worktree_safety::{DirtyWorktree, count_dirty_files, git_stdout, inspect_dirt_at};

/// Directory names whose contents are regenerable build/cache output (#4118).
///
/// Why: the nested-repository scan has to walk gitignored subtrees, and those
/// are dominated by exactly two things — enormous disposable build output, and
/// the occasional nested checkout that holds real work. Walking `target/` on 95
/// candidates would cost minutes and find nothing; skipping it by NAME is the
/// difference between a scan that runs and one that gets disabled. These names
/// are regenerable by definition: no tool writes work-of-record into them, and
/// losing them costs only CPU.
/// What: matched against a directory's own file name at any depth. A nested
/// repository underneath one of these is NOT seen — see the residual-risk note
/// in `worktree_safety`.
/// Test: `inspect_dirt_does_not_scan_disposable_build_dirs`.
const DISPOSABLE_DIR_NAMES: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".cache",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".venv",
    "venv",
    ".gradle",
    "coverage",
    ".terraform",
];

/// Directory entries the gitignored-subtree scan may visit per candidate.
///
/// Why: an unbounded walk of an arbitrary ignored tree is a denial-of-service
/// against the sweep. Measured against this repo's own 31 session worktrees, a
/// FULL-tree walk with [`DISPOSABLE_DIR_NAMES`] pruning visited 5.6k entries per
/// worktree on average; the ignored-only subset is far smaller, so 50k is two
/// orders of magnitude of headroom.
/// What: exceeding the budget is an ERROR, which the caller turns into DIRTY —
/// "I could not finish looking" is never "there is nothing there".
const IGNORED_SCAN_ENTRY_BUDGET: usize = 50_000;

/// Collapsed listing of gitignored entries, used to bound the nested-repo scan.
const IGNORED_STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain",
    "--untracked-files=normal",
    "--ignored",
    "--ignore-submodules=none",
];

/// Does a NESTED repository under this candidate hold unsaved work (#4091)?
///
/// Why: this is the hole that made the whole guard bypassable. Deleting the
/// candidate deletes everything inside it, but `git status` on the candidate
/// says nothing about a nested checkout — `.claude/worktrees/` is gitignored on
/// this repo's `main` (`.gitignore:40`), so seven registered agent worktrees
/// inside `.base/.worktrees/2eb72dca-…` produced `dirty_files = 0`. Worse, the
/// #3649 ownership gate deliberately REFUSES to delete those nested worktrees
/// directly (no sentinel ⇒ owner-unknown), so the sweep would have honoured
/// that refusal and then deleted the directory they live in — bypassing its own
/// guard by removing the parent.
/// What: every nested root from [`nested_repo_roots`] is inspected under the
/// loss model that actually applies to it (see [`object_store_dies_with`]). A
/// nested root that is dirty OR unassessable makes the PARENT dirty; one that
/// is provably safe does not, so a session that merely once spawned an agent
/// worktree stays reclaimable. The reported counts are the nested root's own.
/// Test: `inspect_dirt_reports_nested_gitignored_worktree`,
/// `inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir`,
/// `inspect_dirt_reports_self_contained_clone_with_work_on_another_branch`,
/// `inspect_dirt_allows_clean_nested_worktree`.
pub(super) fn nested_dirt(candidate: &Path) -> Option<DirtyWorktree> {
    let canonical = match std::fs::canonicalize(candidate) {
        Ok(c) => c,
        Err(e) => {
            return Some(DirtyWorktree::new(
                candidate,
                format!("candidate is unreadable: {e}"),
                0,
                0,
            ));
        }
    };
    let roots = match nested_repo_roots(candidate, &canonical) {
        Ok(r) => r,
        Err(e) => {
            return Some(DirtyWorktree::new(
                candidate,
                format!("nested-repository scan failed: {e}"),
                0,
                0,
            ));
        }
    };
    for root in roots {
        // A nested root that is provably safe does not pin the parent.
        let Some(inner) = inspect_nested_root(&root, &canonical) else {
            continue;
        };
        let shown = root
            .strip_prefix(&canonical)
            .unwrap_or(&root)
            .display()
            .to_string();
        return Some(DirtyWorktree::new(
            candidate,
            format!(
                "nested git worktree/repository `{shown}` holds unsaved work \
                 that `git status` on this directory cannot see: {}",
                inner.reason
            ),
            inner.dirty_files,
            inner.unpushed_commits,
        ));
    }
    None
}

/// Inspect one nested root under the loss model that applies to it (#4118).
///
/// Why: see the module header. Using the shared-store model on a self-contained
/// clone is a demonstrated false CLEAN with unrecoverable loss.
/// What: dispatches on [`object_store_dies_with`]. A discriminator that cannot
/// answer is DIRTY — not knowing which model applies means not knowing what
/// removal costs.
/// Test: `inspect_dirt_reports_self_contained_clone_with_work_on_another_branch`,
/// `inspect_dirt_allows_clean_nested_worktree`.
fn inspect_nested_root(root: &Path, candidate: &Path) -> Option<DirtyWorktree> {
    match object_store_dies_with(root, candidate) {
        // Shared store outside the candidate: only the working tree and the
        // force-deleted branch are at risk — the top-level model.
        Ok(false) => inspect_dirt_at(root, false),
        Ok(true) => self_contained_dirt(root),
        Err(e) => Some(DirtyWorktree::new(
            root,
            format!("nested-repository loss-model probe failed: {e}"),
            0,
            0,
        )),
    }
}

/// Would removing `candidate` destroy `root`'s entire object store (#4118)?
///
/// Why: this is the whole question, asked directly instead of inferred from how
/// the repository was created. `git rev-parse --git-common-dir` resolves to the
/// directory holding the objects, refs and reflogs that a worktree shares — the
/// shared `.base/.git` for a registered worktree, and its own `.git` for a
/// standalone clone. Whether that path is inside the directory about to be
/// deleted IS the loss model.
/// What: `Ok(true)` when the common dir is a strict descendant of `candidate`;
/// `Ok(false)` otherwise; `Err` when git cannot answer. `--path-format=absolute`
/// is required — the default is relative to the queried worktree, which would
/// make the containment test meaningless.
/// Test: `object_store_dies_with_is_false_for_a_registered_worktree`,
/// `object_store_dies_with_is_true_for_a_self_contained_clone`.
pub(super) fn object_store_dies_with(root: &Path, candidate: &Path) -> Result<bool, String> {
    let raw = git_stdout(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = PathBuf::from(raw.trim());
    if common.as_os_str().is_empty() {
        return Err("`rev-parse --git-common-dir` returned nothing".into());
    }
    let common = std::fs::canonicalize(&common).unwrap_or(common);
    Ok(common.starts_with(candidate) && common != candidate)
}

/// Dirt check for a nested repository whose ENTIRE store dies with the
/// candidate (#4118).
///
/// Why: the top-level model asks about `HEAD` and one branch because every
/// other ref survives in the shared store. Here nothing survives, so the
/// question widens to *everything this repository holds that exists nowhere
/// else*. The reviewer's repro: a clone in a gitignored `scratch/`, HEAD on a
/// pushed `main`, one commit on a `feature` branch that exists nowhere else —
/// `status` empty, `origin/main..HEAD` = 0, no `session/<leaf>` branch, so all
/// three top-level questions answered clean while the commit was destroyed.
/// `--all --not --remotes` returns 3 for that same repository.
/// What: working-tree entries, PLUS commits reachable from any local ref but
/// from no remote-tracking ref, PLUS stash entries. A clone with no remotes at
/// all makes `--remotes` expand to nothing, so every commit counts — the
/// correct, conservative answer for a scratch clone nobody ever pushed.
/// Test: `inspect_dirt_reports_self_contained_clone_with_work_on_another_branch`,
/// `inspect_dirt_reports_self_contained_clone_holding_only_a_stash`,
/// `inspect_dirt_allows_fully_pushed_self_contained_clone`.
fn self_contained_dirt(root: &Path) -> Option<DirtyWorktree> {
    let files = match count_dirty_files(root) {
        Ok(n) => n,
        Err(e) => {
            return Some(DirtyWorktree::new(
                root,
                format!("dirty-check failed: {e}"),
                0,
                0,
            ));
        }
    };
    let local_only = match count_local_only_commits(root) {
        Ok(n) => n,
        Err(e) => {
            return Some(DirtyWorktree::new(
                root,
                format!("dirty-check failed: {e}"),
                files,
                0,
            ));
        }
    };
    let stashed = match count_stash_entries(root) {
        Ok(n) => n,
        Err(e) => {
            return Some(DirtyWorktree::new(
                root,
                format!("dirty-check failed: {e}"),
                files,
                local_only,
            ));
        }
    };
    if files == 0 && local_only == 0 && stashed == 0 {
        return None;
    }
    Some(DirtyWorktree::new(
        root,
        format!(
            "self-contained clone (its object store dies with the candidate): \
             {files} uncommitted/untracked file(s), {local_only} commit(s) on no remote \
             across ALL local refs, {stashed} stash entr(y/ies)"
        ),
        files,
        local_only,
    ))
}

/// Count commits reachable from any local ref but from no remote-tracking ref.
///
/// Why: `--all` covers every branch, tag and `refs/stash`, not just `HEAD`.
/// That breadth is the point — a self-contained clone loses all of them.
/// What: `rev-list --count --all --not --remotes`.
/// Test: `inspect_dirt_reports_self_contained_clone_with_work_on_another_branch`.
fn count_local_only_commits(root: &Path) -> Result<usize, String> {
    let raw = git_stdout(
        root,
        &["rev-list", "--count", "--all", "--not", "--remotes"],
    )?;
    raw.trim()
        .parse::<usize>()
        .map_err(|e| format!("unparsable rev-list count `{}`: {e}", raw.trim()))
}

/// Count stash entries, which die with a self-contained clone.
///
/// Why: `worktree_safety`'s residual-risk note says the stash is "repo-level,
/// not per-worktree". True for the top-level candidate — its stash lives in the
/// shared `.base/.git` and survives. FALSE for a self-contained clone, whose
/// `refs/stash` and its reflog are inside the directory being deleted.
/// What: `for-each-ref` first, because it exits 0 with EMPTY output when the ref
/// is absent — so "no stash" is distinguishable from "git failed", which stays
/// an `Err`. Only then is the reflog walked for a count.
/// Test: `inspect_dirt_reports_self_contained_clone_holding_only_a_stash`.
fn count_stash_entries(root: &Path) -> Result<usize, String> {
    let listed = git_stdout(root, &["for-each-ref", "--format=%(refname)", "refs/stash"])?;
    if listed.trim().is_empty() {
        return Ok(0);
    }
    let log = git_stdout(root, &["reflog", "show", "--format=%H", "refs/stash"])?;
    Ok(log.lines().filter(|l| !l.trim().is_empty()).count())
}

/// Enumerate every nested repository root strictly beneath `candidate`.
///
/// Why: "does this directory contain a checkout" must be answered EXACTLY, not
/// heuristically, because the answer decides whether gigabytes get deleted. Two
/// disjoint populations exist and neither subsumes the other: worktrees
/// REGISTERED with the enclosing repository (the `.claude/worktrees/…` shape,
/// free to enumerate — git already knows) and independent clones that git has
/// never heard of (only findable on disk).
/// What: the union of (a) `worktree list --porcelain` entries that are strict
/// descendants of `candidate` — exact, at any depth, and immune to gitignore
/// because git's own bookkeeping is the source — and (b) a bounded walk of the
/// gitignored subtrees `git status --ignored` reports, looking for a `.git`
/// entry. Untracked-but-not-ignored nested repos need no walk at all: they
/// already surface as `??` lines in `count_dirty_files`.
/// Test: `inspect_dirt_reports_nested_gitignored_worktree`,
/// `inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir`.
fn nested_repo_roots(candidate: &Path, canonical: &Path) -> Result<Vec<PathBuf>, String> {
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();

    for line in git_stdout(candidate, &["worktree", "list", "--porcelain"])?.lines() {
        let Some(raw) = line.strip_prefix("worktree ") else {
            continue;
        };
        let listed = PathBuf::from(raw.trim());
        let listed = std::fs::canonicalize(&listed).unwrap_or(listed);
        if listed != canonical && listed.starts_with(canonical) {
            roots.insert(listed);
        }
    }

    let mut budget = IGNORED_SCAN_ENTRY_BUDGET;
    for line in git_stdout(candidate, IGNORED_STATUS_ARGS)?.lines() {
        let Some(raw) = line.strip_prefix("!! ") else {
            continue;
        };
        let entry = raw.trim();
        if entry.starts_with('"') {
            return Err(format!(
                "ignored entry `{entry}` has a quoted path this scan cannot interpret"
            ));
        }
        scan_for_repos(
            &canonical.join(entry.trim_end_matches('/')),
            &mut roots,
            &mut budget,
        )?;
    }
    Ok(roots.into_iter().collect())
}

/// Walk `root`, collecting directories that contain a `.git` entry.
///
/// Why: an unregistered clone inside a gitignored directory is invisible to
/// every git question asked of the CANDIDATE, so the only way to find it is to
/// look. Iterative rather than recursive so a pathologically deep tree cannot
/// overflow the stack, and symlinks are never followed — `remove_dir_all`
/// deletes a link, not its target, so nothing beyond one is at risk.
/// What: pushes each directory holding a `.git` entry into `out` and stops
/// descending there. Skips [`DISPOSABLE_DIR_NAMES`] by name at every level.
/// Exhausting `budget`, or any unreadable entry, is an `Err` the caller turns
/// into DIRTY.
/// Test: `inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir`,
/// `inspect_dirt_does_not_scan_disposable_build_dirs`.
fn scan_for_repos(
    root: &Path,
    out: &mut BTreeSet<PathBuf>,
    budget: &mut usize,
) -> Result<(), String> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        match std::fs::symlink_metadata(&dir) {
            // A plain file, or a symlink of any kind: nothing to descend into.
            Ok(meta) if !meta.is_dir() => continue,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("`{}` is unreadable: {e}", dir.display())),
        }
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if DISPOSABLE_DIR_NAMES.contains(&name) {
            continue;
        }
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("`{}` is unreadable: {e}", dir.display()))?;
        let mut children = Vec::new();
        let mut is_repo = false;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("an entry of `{}` is unreadable: {e}", dir.display()))?;
            if *budget == 0 {
                return Err(format!(
                    "gitignored-subtree scan exceeded its {IGNORED_SCAN_ENTRY_BUDGET}-entry \
                     budget at `{}`",
                    dir.display()
                ));
            }
            *budget -= 1;
            if entry.file_name() == ".git" {
                is_repo = true;
                break;
            }
            children.push(entry.path());
        }
        if is_repo {
            out.insert(dir);
            continue;
        }
        stack.extend(children);
    }
    Ok(())
}

#[cfg(test)]
#[path = "worktree_nested_tests.rs"]
mod worktree_nested_tests;
