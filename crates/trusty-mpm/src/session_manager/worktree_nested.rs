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
//! What: [`nested_dirt`] (the entry point), [`scan_ignored_subtrees`] (one
//! bounded walk that enumerates registered worktrees from git's own
//! bookkeeping, unregistered clones — bare and non-bare — from disk, and the
//! high-value gitignored files of #4166), and the loss-model discriminator
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

/// The entries that identify a BARE repository root on disk (#4166).
///
/// Why: [`scan_for_repos`] used to answer "is this a repository root" by
/// matching a directory entry named literally `.git`. A bare repository has no
/// `.git` entry — `git clone --bare` lays down `HEAD`, `objects/`, `refs/`,
/// `config`, `hooks/` and `info/` at its own root — so it was invisible to the
/// walk and a candidate holding one read CLEAN with `dirty_files = 0`. The
/// reviewer reproduced the loss end to end: `git status` on the candidate is
/// empty because the path is gitignored, `git worktree list` does not mention a
/// different repository, and the walk found nothing. A second-order effect came
/// with it — the walk DESCENDED into `objects/` and `refs/`, spending scan
/// budget, so a loose-object bare repo could fail-safe to DIRTY by accident
/// while a packed one (the normal state right after `clone --bare`) read clean.
/// What: all three must be present, with the types git actually creates, before
/// a directory is called a bare repository. A directory that merely happens to
/// hold three same-named entries is then classified as a nested repository and
/// fails toward DIRTY rather than toward deletion, so the conservative
/// direction survives a false positive.
/// Test: `inspect_dirt_reports_nested_bare_repo_holding_the_only_copy`,
/// `scan_does_not_descend_into_a_bare_repos_object_store`.
const BARE_REPO_MARKERS: &[(&str, bool)] = &[("HEAD", false), ("objects", true), ("refs", true)];

/// Gitignored basenames whose contents are unrecoverable if deleted (#4166).
///
/// Why: this module's residual-risk note used to excuse EVERY gitignored loose
/// file outside `.trusty-mpm/`, and the measurement behind that decision was
/// sound as far as it went — counting every non-disposable ignored entry as
/// dirt flagged `.claude/` in 30 of this repo's 31 session worktrees and would
/// have disabled reclamation outright. "Count none of them" overshoots in the
/// other direction: a `.env.local` holds credentials that exist nowhere else, a
/// `*.bak` or `*.orig` is the pre-edit copy someone kept deliberately, and all
/// three are cheap to name explicitly. Naming a short list buys back the
/// unrecoverable cases without reintroducing the 30-of-31 flag rate.
/// What: `.env*` by prefix, `*.bak` and `*.orig` by suffix, matched against a
/// basename at any depth of the gitignored walk. Anything under
/// [`DISPOSABLE_DIR_NAMES`] is never reached, so a `.env` written into
/// `target/` by a build stays excused.
/// Test: `inspect_dirt_reports_high_value_gitignored_env_file`,
/// `inspect_dirt_reports_high_value_gitignored_bak_in_a_subdirectory`,
/// `inspect_dirt_ignores_high_value_names_inside_disposable_build_dirs`.
fn is_high_value_ignored(name: &str) -> bool {
    name.starts_with(".env") || name.ends_with(".bak") || name.ends_with(".orig")
}

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
/// What: every nested root from [`scan_ignored_subtrees`] is inspected under
/// the loss model that actually applies to it (see [`object_store_dies_with`]).
/// A nested root that is dirty OR unassessable makes the PARENT dirty; one that
/// is provably safe does not, so a session that merely once spawned an agent
/// worktree stays reclaimable. The reported counts are the nested root's own.
/// The same walk also reports the high-value gitignored files of #4166, which
/// no git question asked of the candidate can see.
/// Test: `inspect_dirt_reports_nested_gitignored_worktree`,
/// `inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir`,
/// `inspect_dirt_reports_self_contained_clone_with_work_on_another_branch`,
/// `inspect_dirt_reports_nested_bare_repo_holding_the_only_copy`,
/// `inspect_dirt_reports_high_value_gitignored_env_file`,
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
    let scan = match scan_ignored_subtrees(candidate, &canonical) {
        Ok(s) => s,
        Err(e) => {
            return Some(DirtyWorktree::new(
                candidate,
                format!("nested-repository scan failed: {e}"),
                0,
                0,
            ));
        }
    };
    for root in scan.repos {
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
    high_value_dirt(candidate, &canonical, &scan.valuables)
}

/// Report the high-value gitignored files the walk found, if any (#4166).
///
/// Why: `git status` cannot see these by construction — they are gitignored —
/// and the sweep deletes them with the candidate. A `.env.local` is credentials
/// that exist nowhere else; a `*.bak` or `*.orig` is a copy somebody kept on
/// purpose.
/// What: `None` when the walk found none. Otherwise a skip record naming up to
/// [`REPORTED_VALUABLE_LIMIT`] of them relative to the candidate, with the full
/// count carried in `dirty_files` so an operator sees the size of what a
/// force-discard would destroy.
/// Test: `inspect_dirt_reports_high_value_gitignored_env_file`,
/// `inspect_dirt_reports_high_value_gitignored_bak_in_a_subdirectory`.
fn high_value_dirt(
    candidate: &Path,
    canonical: &Path,
    valuables: &BTreeSet<PathBuf>,
) -> Option<DirtyWorktree> {
    if valuables.is_empty() {
        return None;
    }
    let shown: Vec<String> = valuables
        .iter()
        .take(REPORTED_VALUABLE_LIMIT)
        .map(|p| p.strip_prefix(canonical).unwrap_or(p).display().to_string())
        .collect();
    let more = valuables.len().saturating_sub(shown.len());
    let tail = if more > 0 {
        format!(" (and {more} more)")
    } else {
        String::new()
    };
    Some(DirtyWorktree::new(
        candidate,
        format!(
            "{} gitignored file(s) that removal would destroy and no `git status` \
             can show: {}{tail}",
            valuables.len(),
            shown.join(", ")
        ),
        valuables.len(),
        0,
    ))
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
///
/// A BARE repository takes the same path with one leg removed (#4166): it has
/// no working tree, so `git status` inside it exits 128 rather than reporting
/// nothing. Asking anyway would make every bare repo DIRTY through the error
/// arm, including a fully-pushed one — safe, but a permanent leak of the kind
/// this module is careful to avoid. The commit and stash legs need no
/// adjustment; both answer correctly on a bare repository.
/// Test: `inspect_dirt_reports_self_contained_clone_with_work_on_another_branch`,
/// `inspect_dirt_reports_self_contained_clone_holding_only_a_stash`,
/// `inspect_dirt_reports_nested_bare_repo_holding_the_only_copy`,
/// `inspect_dirt_allows_an_empty_nested_bare_repo`,
/// `inspect_dirt_allows_fully_pushed_self_contained_clone`.
fn self_contained_dirt(root: &Path) -> Option<DirtyWorktree> {
    let bare = match is_bare_repository(root) {
        Ok(b) => b,
        Err(e) => {
            return Some(DirtyWorktree::new(
                root,
                format!("bare-repository probe failed: {e}"),
                0,
                0,
            ));
        }
    };
    let files = match if bare { Ok(0) } else { count_dirty_files(root) } {
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
    let shape = if bare {
        "bare repository"
    } else {
        "self-contained clone"
    };
    let tree = if bare {
        String::new()
    } else {
        format!("{files} uncommitted/untracked file(s), ")
    };
    Some(DirtyWorktree::new(
        root,
        format!(
            "{shape} (its object store dies with the candidate): \
             {tree}{local_only} commit(s) on no remote across ALL local refs, \
             {stashed} stash entr(y/ies)"
        ),
        files,
        local_only,
    ))
}

/// Is `root` a bare repository — one with no working tree (#4166)?
///
/// Why: [`self_contained_dirt`]'s working-tree leg is not merely useless on a
/// bare repository, it fails: `git status` there exits 128 with "this operation
/// must be run in a work tree", which the error arm would turn into a
/// permanent DIRTY.
/// What: `git rev-parse --is-bare-repository`, which prints `true` or `false`.
/// Anything else is an `Err` the caller turns into DIRTY.
/// Test: `inspect_dirt_allows_an_empty_nested_bare_repo`.
fn is_bare_repository(root: &Path) -> Result<bool, String> {
    let raw = git_stdout(root, &["rev-parse", "--is-bare-repository"])?;
    match raw.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!(
            "`rev-parse --is-bare-repository` answered `{other}`, which this guard \
             cannot interpret"
        )),
    }
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

/// Everything one bounded walk of the candidate's gitignored subtrees found.
///
/// Why: the walk is the expensive part, and it visits every entry exactly once.
/// Both questions the walk can answer — "is there a nested repository here" and
/// "is there a high-value gitignored file here" — are decided from the same
/// directory listing, so asking them together costs no extra I/O. Asking them
/// in two passes would double a cost the module already bounds with a budget.
/// Test: `worktree_nested_tests`.
#[derive(Default)]
struct IgnoredScan {
    /// Nested repository roots, from git's bookkeeping and from disk.
    repos: BTreeSet<PathBuf>,
    /// Absolute paths of gitignored files matching [`is_high_value_ignored`].
    valuables: BTreeSet<PathBuf>,
}

/// High-value gitignored paths named individually in a skip record (#4166).
///
/// Why: an operator deciding whether to force-discard needs to see WHAT is at
/// risk, but a candidate holding hundreds of them would produce a log line
/// nobody reads. The full count is always reported; only the enumeration is
/// capped.
const REPORTED_VALUABLE_LIMIT: usize = 5;

/// Enumerate everything strictly beneath `candidate` that removal would destroy
/// and no git question asked of `candidate` can see.
///
/// Why: two disjoint populations of nested repository exist and neither
/// subsumes the other — worktrees REGISTERED with the enclosing repository (the
/// `.claude/worktrees/…` shape, free to enumerate, git already knows) and
/// independent repositories git has never heard of (only findable on disk).
/// High-value gitignored files (#4166) are invisible to both.
/// What: (a) `worktree list --porcelain` entries that are strict descendants of
/// `candidate` — at any depth, and immune to gitignore because git's own
/// bookkeeping is the source; (b) a bounded walk of the gitignored subtrees
/// `git status --ignored` reports, collecting repository roots (bare and
/// non-bare) and high-value filenames as it goes. Untracked-but-not-ignored
/// nested repos need no walk at all: they already surface as `??` lines in
/// `count_dirty_files`.
///
/// Leg (a) is exact. Leg (b) is a disk walk with two documented blind spots —
/// it does not descend into [`DISPOSABLE_DIR_NAMES`], and it stops at the first
/// repository root it finds — so this function is not the exhaustive
/// enumeration an earlier version of this doc claimed it was. The residual-risk
/// list in `worktree_safety` states both limits; keep the two in step.
/// Test: `inspect_dirt_reports_nested_gitignored_worktree`,
/// `inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir`,
/// `inspect_dirt_reports_nested_bare_repo_holding_the_only_copy`.
fn scan_ignored_subtrees(candidate: &Path, canonical: &Path) -> Result<IgnoredScan, String> {
    let mut scan = IgnoredScan::default();

    for line in git_stdout(candidate, &["worktree", "list", "--porcelain"])?.lines() {
        let Some(raw) = line.strip_prefix("worktree ") else {
            continue;
        };
        let listed = PathBuf::from(raw.trim());
        let listed = std::fs::canonicalize(&listed).unwrap_or(listed);
        if listed != canonical && listed.starts_with(canonical) {
            scan.repos.insert(listed);
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
        // `git status --ignored` collapses a wholly-ignored directory to one
        // line, so an ignored FILE arrives here as a leaf the walk below never
        // descends into. #4166: test its own name before descending.
        let path = canonical.join(entry.trim_end_matches('/'));
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && is_high_value_ignored(name)
        {
            scan.valuables.insert(path.clone());
        }
        scan_for_repos(&path, &mut scan, &mut budget)?;
    }
    Ok(scan)
}

/// Walk `root`, collecting nested repository roots and high-value ignored files.
///
/// Why: an unregistered repository inside a gitignored directory is invisible
/// to every git question asked of the CANDIDATE, so the only way to find it is
/// to look — and the same is true of a gitignored `.env.local` (#4166).
/// Iterative rather than recursive so a pathologically deep tree cannot
/// overflow the stack, and symlinks are never followed — `remove_dir_all`
/// deletes a link, not its target, so nothing beyond one is at risk.
/// What: pushes each directory that is a repository root into `scan.repos` and
/// stops descending there; pushes each file matching [`is_high_value_ignored`]
/// into `scan.valuables`. A root is recognised by a `.git` entry OR by the
/// [`BARE_REPO_MARKERS`] triple, which is what stops the walk both from missing
/// a bare repository and from descending into its `objects/`. Skips
/// [`DISPOSABLE_DIR_NAMES`] by name at every level. Exhausting `budget`, or any
/// unreadable entry, is an `Err` the caller turns into DIRTY.
/// Test: `inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir`,
/// `inspect_dirt_reports_nested_bare_repo_holding_the_only_copy`,
/// `scan_does_not_descend_into_a_bare_repos_object_store`,
/// `inspect_dirt_does_not_scan_disposable_build_dirs`.
fn scan_for_repos(root: &Path, scan: &mut IgnoredScan, budget: &mut usize) -> Result<(), String> {
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
        let mut valuables = Vec::new();
        let mut markers = 0usize;
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
            if let Some(name) = entry.file_name().to_str() {
                if is_bare_marker(&entry, name) {
                    markers += 1;
                }
                if is_high_value_ignored(name) {
                    valuables.push(entry.path());
                }
            }
            children.push(entry.path());
        }
        // #4166: a bare repository has no `.git` entry, only this triple.
        if is_repo || markers == BARE_REPO_MARKERS.len() {
            scan.repos.insert(dir);
            continue;
        }
        scan.valuables.extend(valuables);
        stack.extend(children);
    }
    Ok(())
}

/// Does this directory entry satisfy one of the [`BARE_REPO_MARKERS`]?
///
/// Why: matching on the name alone would call any directory holding files
/// coincidentally named `HEAD`, `objects` and `refs` a bare repository. Checking
/// the type as well costs nothing — `read_dir` carries it — and the wrong answer
/// is only reachable when the type cannot be read at all.
/// What: name equality plus the directory-ness git actually creates. An
/// unreadable type counts as a match, so an entry this code cannot classify
/// pushes toward "treat as a repository", which fails toward DIRTY.
fn is_bare_marker(entry: &std::fs::DirEntry, name: &str) -> bool {
    BARE_REPO_MARKERS.iter().any(|(marker, want_dir)| {
        *marker == name
            && entry
                .file_type()
                .map(|t| t.is_dir() == *want_dir)
                .unwrap_or(true)
    })
}

#[cfg(test)]
#[path = "worktree_nested_tests.rs"]
mod worktree_nested_tests;
