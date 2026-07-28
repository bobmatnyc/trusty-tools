//! Git-native worktree enumeration and registry-root resolution (#4207 slice 1).
//!
//! Why: every worktree question this crate asks used to be answered by
//! GUESSING at the filesystem. `prune::find_orphaned_worktrees` walked five
//! hard-coded location shapes (`.worktrees/`, `.base/.worktrees/`,
//! `.claude/worktrees/`, `.base/.claude/worktrees/`, and
//! `.base/.worktrees/<id>/.claude/worktrees/`), and both
//! `worktree_safety::git_worktree_list_agrees` and
//! `decommission::remove_session_worktree` derived "which checkout owns this
//! worktree?" from the candidate's GRANDPARENT directory. Neither is a fact;
//! both are inferences that git can answer exactly, and both were wrong on
//! this machine on 2026-07-27: fourteen worktrees physically located inside
//! `<repo>/.base/.worktrees/` are registered to the PARENT repo `<repo>`, so
//! the grandparent rule asked `.base` about them, `.base` correctly said "not
//! mine", and the sweep skipped them conservatively — forever. They were not
//! merely un-reclaimed, they were STRUCTURALLY unreclaimable, and no amount of
//! adding a sixth location shape would have fixed it.
//!
//! Git already maintains the answer. `git worktree list --porcelain` is the
//! registry, and `git rev-parse --git-common-dir` names the checkout that owns
//! it. Deriving from those two commands makes location irrelevant: a worktree
//! is found because git registered it, not because it happened to be parked
//! under a directory name someone remembered to enumerate.
//!
//! What: [`RegisteredWorktree`] (one porcelain record),
//! [`parse_worktree_list`] (the pure parser), [`registry_root_for`] (replaces
//! grandparent-as-repo-root inference), [`list_registered_worktrees`] (what
//! git says about the repository owning a path), and
//! [`enumerate_registered_worktrees`] (every registered worktree under a
//! managed repos root).
//!
//! # An unanswerable probe is never a verdict
//!
//! Every function here returns `Option`/`None` — never a `bool` — for the
//! "could not determine" case, so no caller can silently read a missing `git`
//! binary, a permissions error, or a stalled network mount as a fact about the
//! worktree. This is the same invariant PR #4202's round-1 fix broke by
//! collapsing every `canonicalize()` error kind into "destroyed"; on
//! 2026-07-21 a rule that loose would have told an operator to recreate ~70
//! fully intact worktrees. Callers decide what a `None` means in their own
//! direction of safety — [`enumerate_registered_worktrees`] treats it as "no
//! candidates from this anchor" (nothing is proposed for deletion), while
//! `git_worktree_list_agrees` keeps its long-standing best-effort `true`.
//!
//! Test: `worktree_registry_tests`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::worktree_safety::git_command;

/// The bare-clone checkout trusty-mpm provisions alongside a managed project.
///
/// Why: a managed project has at most TWO git checkouts that can own a worktree
/// registry — the project checkout itself and the `.base` clone
/// `provisioner::workspace::WorkspaceProvisioner` creates. Naming the segment
/// once keeps [`enumerate_registered_worktrees`] from repeating the literal.
/// This is NOT a worktree-location guess: it names a CHECKOUT to interrogate,
/// and git then reports where that checkout's worktrees actually live.
/// What: `".base"`.
/// Test: `enumerate_finds_worktree_registered_to_base_clone`.
const BASE_CLONE_DIRNAME: &str = ".base";

/// One worktree exactly as `git worktree list --porcelain` reports it (#4207).
///
/// Why: the porcelain format carries the four facts the reclaim path needs —
/// where the worktree is, whether it is the main checkout (never a reclaim
/// candidate), whether it is bare (no working tree to reclaim), and whether
/// git already considers it prunable (its directory is gone, so there is
/// nothing to delete). Modelling them as fields rather than re-grepping the
/// raw text at each call site keeps the parse in one place.
/// What: `path` is git's own absolute path for the worktree; `branch` is the
/// short branch name when the worktree is not detached; `is_main` marks the
/// FIRST record, which git always emits for the main worktree.
/// Test: `parse_worktree_list_reads_porcelain_records`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegisteredWorktree {
    /// Absolute path git reports for this worktree.
    pub path: PathBuf,
    /// Short branch name (`refs/heads/` stripped), or `None` when detached.
    pub branch: Option<String>,
    /// `true` for a bare repository record (no working tree).
    pub bare: bool,
    /// `true` when git reports the worktree's directory is missing.
    pub prunable: bool,
    /// `true` for the first record — git always lists the main worktree first.
    pub is_main: bool,
}

/// Parse `git worktree list --porcelain` output into records (#4207).
///
/// Why: the parse is the one piece of this module that can be tested without
/// spawning git, and the porcelain format has enough shape (blank-line-
/// separated stanzas, valueless `bare`/`detached`/`prunable` attribute lines)
/// that an ad-hoc `strip_prefix("worktree ")` over all lines — what
/// `git_worktree_list_agrees` did before this change — silently ignores every
/// other fact in the record.
/// What: splits on blank lines; each stanza starts with `worktree <path>` and
/// may carry `HEAD <sha>`, `branch <ref>`, `bare`, `detached`, `locked`, or
/// `prunable` (the last two optionally with a reason). The first stanza is
/// flagged [`RegisteredWorktree::is_main`]. Unknown attribute lines are
/// ignored so a future git version cannot break the parse.
/// Test: `parse_worktree_list_reads_porcelain_records`,
/// `parse_worktree_list_marks_only_the_first_record_main`.
pub(crate) fn parse_worktree_list(stdout: &str) -> Vec<RegisteredWorktree> {
    let mut out: Vec<RegisteredWorktree> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end();
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(RegisteredWorktree {
                path: PathBuf::from(path),
                branch: None,
                bare: false,
                prunable: false,
                is_main: out.is_empty(),
            });
            continue;
        }
        let Some(current) = out.last_mut() else {
            continue;
        };
        if let Some(branch) = line.strip_prefix("branch ") {
            current.branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
            );
        } else if line == "bare" {
            current.bare = true;
        } else if line == "prunable" || line.starts_with("prunable ") {
            current.prunable = true;
        }
    }
    out
}

/// The checkout that owns the worktree registry `anchor` belongs to (#4207).
///
/// Why: this REPLACES grandparent-as-repo-root inference
/// (`path.parent().and_then(|p| p.parent())`), which was correct only for the
/// two shapes it was written against and wrong for every other one —
/// `<repo>/.claude/worktrees/<name>` resolved to `<repo>/.claude`, which is
/// not a repository at all, and the fourteen `.base`-resident worktrees
/// registered to the parent repo resolved to `.base`, which disowns them.
/// `git rev-parse --git-common-dir` answers the same question as a fact: it
/// names the SHARED git directory every worktree of a repository points at,
/// which is exactly the registry `git worktree list` reads.
/// What: runs `git -C <anchor> rev-parse --path-format=absolute
/// --git-common-dir`. A normal checkout answers `<root>/.git`, so the root is
/// its parent; a bare clone answers with the bare directory itself, which IS
/// the root. Returns `None` when git cannot be run, exits non-zero (including
/// the ordinary "not a git repository" case), or prints nothing usable — never
/// a guessed path.
/// Test: `registry_root_for_linked_worktree_is_the_owning_checkout`,
/// `registry_root_for_non_repo_is_none`.
pub(crate) fn registry_root_for(anchor: &Path) -> Option<PathBuf> {
    let out = git_command(
        anchor,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .output()
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let common_dir = PathBuf::from(raw.trim());
    if common_dir.as_os_str().is_empty() {
        return None;
    }
    // A non-bare repository's common dir is `<root>/.git`; a bare one IS the root.
    if common_dir.file_name().is_some_and(|n| n == ".git") {
        common_dir.parent().map(Path::to_path_buf)
    } else {
        Some(common_dir)
    }
}

/// Every worktree git registers for the repository that owns `anchor` (#4207).
///
/// Why: `git worktree list` is the registry. Asking it directly is what makes
/// worktree discovery independent of where a worktree is parked, and it is the
/// only source that can report a worktree registered to one checkout while
/// living inside another's directory tree — the exact state that made fourteen
/// worktrees on this machine unreclaimable.
/// What: runs `git -C <anchor> worktree list --porcelain` and parses the
/// result via [`parse_worktree_list`]. Git resolves the repository from
/// `anchor` itself, so `anchor` may be the checkout root, a linked worktree, or
/// any directory inside one. Returns `None` on a spawn failure or non-zero
/// exit — an unanswerable probe, never an empty list.
/// Test: `list_registered_worktrees_reports_a_real_worktree`,
/// `list_registered_worktrees_none_outside_a_repo`.
pub(crate) fn list_registered_worktrees(anchor: &Path) -> Option<Vec<RegisteredWorktree>> {
    let out = git_command(anchor, &["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_worktree_list(&String::from_utf8_lossy(&out.stdout)))
}

/// Every git-registered worktree living under `repos_root` (#4207 slice 1).
///
/// Why: this is the derive-not-walk replacement for
/// `prune::find_orphaned_worktrees`' five hard-coded location shapes. Those
/// shapes could only ever find worktrees someone had already thought to
/// enumerate, and each new agent runtime added a shape (#3649, #3971, and the
/// #3971 follow-up were all "we missed a location" fixes). Asking git removes
/// the category: a worktree is discovered because it is REGISTERED, wherever
/// it sits.
/// What: for each `<repos_root>/<owner>/<repo>/`, interrogates the two
/// checkouts trusty-mpm actually provisions — `<repo>` and
/// `<repo>/.base` — but only after confirming (via [`registry_root_for`]) that
/// the anchor IS that repository's root. That confirmation is load-bearing:
/// without it, `git -C <non-repo>` walks UP the filesystem and would report an
/// unrelated enclosing repository's worktrees. Records that are bare, main, or
/// already prunable are dropped (there is no working tree to reclaim), as is
/// any worktree resolving outside `repos_root` — preserving the containment
/// boundary the old walk had structurally, since it could only ever produce
/// paths beneath the root it was handed. Results are canonicalized (so they
/// compare cleanly against the canonicalized active-session set) and returned
/// sorted and de-duplicated, since both anchors may report the same worktree.
/// Test: `enumerate_finds_worktree_at_an_unwalked_location`,
/// `enumerate_finds_worktree_registered_to_base_clone`,
/// `enumerate_ignores_plain_directory_that_is_not_a_worktree`,
/// `enumerate_excludes_the_main_checkout`,
/// `enumerate_excludes_worktrees_outside_the_repos_root`.
pub(crate) fn enumerate_registered_worktrees(repos_root: &Path) -> Vec<PathBuf> {
    let canonical_root = std::fs::canonicalize(repos_root).unwrap_or_else(|_| repos_root.into());
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();
    let Ok(owner_entries) = std::fs::read_dir(repos_root) else {
        return Vec::new();
    };
    for owner_entry in owner_entries.flatten() {
        let owner_path = owner_entry.path();
        if !owner_path.is_dir() {
            continue;
        }
        let Ok(repo_entries) = std::fs::read_dir(&owner_path) else {
            continue;
        };
        for repo_entry in repo_entries.flatten() {
            let repo_path = repo_entry.path();
            for anchor in [repo_path.clone(), repo_path.join(BASE_CLONE_DIRNAME)] {
                collect_from_anchor(&anchor, &canonical_root, &mut found);
            }
        }
    }
    found.into_iter().collect()
}

/// Add every reclaimable-shaped worktree registered at `anchor` to `found`.
///
/// Why: extracted so [`enumerate_registered_worktrees`]' two anchors share one
/// rule rather than duplicating the root-confirmation, filtering, and
/// containment logic — the duplication that let the old walk's five shapes
/// drift apart from each other.
/// What: no-ops unless `anchor` is a directory that is ITSELF the root of the
/// repository owning its registry (see [`registry_root_for`]); then inserts
/// every non-bare, non-main, non-prunable worktree whose canonicalized path
/// lies under `canonical_root`.
/// Test: covered by every `enumerate_*` test.
fn collect_from_anchor(anchor: &Path, canonical_root: &Path, found: &mut BTreeSet<PathBuf>) {
    if !anchor.is_dir() {
        return;
    }
    // Confirm the anchor is a repository ROOT, not merely a directory inside
    // one — otherwise git walks up and reports a foreign repository's registry.
    let Some(root) = registry_root_for(anchor) else {
        return;
    };
    let canonical_anchor = std::fs::canonicalize(anchor).unwrap_or_else(|_| anchor.into());
    let canonical_repo_root = std::fs::canonicalize(&root).unwrap_or(root);
    if canonical_anchor != canonical_repo_root {
        return;
    }
    let Some(worktrees) = list_registered_worktrees(anchor) else {
        return;
    };
    for wt in worktrees {
        if wt.bare || wt.is_main || wt.prunable {
            continue;
        }
        // #1845 item 8, preserved: a path that cannot be canonicalized is
        // SKIPPED, never proposed. A dangling symlink, a deletion race, an
        // unreadable parent, or a stalled mount all make the path unresolvable,
        // and an unresolvable path cannot be compared against the active-session
        // set — so proposing it would risk offering a LIVE worktree for
        // deletion. The asymmetry is deliberate: a failed observation reduces
        // what this function proposes, and can never enlarge it.
        let Ok(canonical) = std::fs::canonicalize(&wt.path) else {
            continue;
        };
        if canonical.starts_with(canonical_root) {
            found.insert(canonical);
        }
    }
}

#[cfg(test)]
#[path = "worktree_registry_tests.rs"]
mod worktree_registry_tests;
