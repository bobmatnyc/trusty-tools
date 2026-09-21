//! Whether a worktree's divergence holds CONTENT its landing branch lacks
//! (#7889).
//!
//! Why: the two equivalences that already run — `git cherry`'s per-commit patch
//! id (#6528) and the aggregate-divergence patch id (#6507) — both compare
//! DIFFS, and a diff only matches when the landing branch grew the same content
//! in the same shape. The continuation rule this repository runs on produces the
//! other shape constantly: a killed agent's branch is continued on `-r2`…`-r9`,
//! the rounds land as several pull requests, and the parked original's
//! divergence then equals no single commit's patch anywhere on `main`. Fourteen
//! worktrees were stuck that way on 2026-09-21, each refused with "2–42 unpushed
//! commits" while holding no content `origin/main` did not already have, and
//! each holding a 10–25 GB `target-*/` directory.
//!
//! What: [`content_superseded_commits`] asks the question a removal actually
//! turns on — would deleting this tree destroy anything? It compares END STATES
//! rather than diffs: every path the divergence `<merge-base>..<tip>` touched
//! must be byte-identical between `tip` and the landing base. When they all are,
//! the divergence's commits are returned as landed, because no file they wrote
//! differs from what the remote already holds.
//!
//! **The rule is a superset of the patch-id ones, not a loosening of them.** A
//! patch-id match means the landing branch grew the same diff; this means it
//! HOLDS the same bytes, which is the property that makes the directory
//! disposable. It is also narrower in one direction that matters: a path the
//! branch changed and the landing branch has since changed further reports a
//! difference and refuses, so a divergence is cleared only against the content
//! standing on the remote right now.
//!
//! **Every failure arm refuses**, per ADR-0045 and ADR-0057 decision 6 — no
//! merge base, an unreadable diff, a path this process cannot spell back to git,
//! more paths than [`SUPERSEDED_PATH_MAX`], or a divergence that touched no file
//! at all all return nothing and leave the caller's raw count standing.
//!
//! Test: `worktree_landed_content_tests`.

use std::path::Path;

use super::worktree_safety::git_stdout;

/// How many changed paths one containment check will compare (#7889).
///
/// Why: the check spells every path back to git as a pathspec, and an
/// unbounded list would build an argv from a branch that rewrote the whole
/// tree. Exceeding the bound returns nothing, which keeps the raw count — the
/// refusing direction.
/// What: 500, above any real branch's file count here.
/// Test: `a_divergence_wider_than_the_path_bound_is_not_cleared`.
const SUPERSEDED_PATH_MAX: usize = 500;

/// The commits in `<merge-base>..<tip>` whose CONTENT `base` already holds
/// (#7889).
///
/// Why: see the module doc — the shape the per-commit and aggregate patch-id
/// comparisons both miss.
/// What: `git diff --name-only -z <merge-base> <tip>` names the paths the
/// divergence touched; `git diff --name-only -z <base> <tip> -- <paths>` names
/// the ones that still differ. An empty second answer means the landing base
/// holds every byte this divergence wrote, so every SHA in the range is
/// returned as landed. Anything else — including a path git spelled back in a
/// form this process cannot hand it verbatim — returns an empty vector.
/// Test: `two_commits_whose_content_landed_by_another_route_are_superseded`,
/// `a_commit_the_landing_base_does_not_hold_is_not_superseded`,
/// `a_base_git_cannot_resolve_supersedes_nothing`,
/// `a_divergence_that_touched_no_file_is_not_cleared`.
pub(crate) fn content_superseded_commits(path: &Path, base: &str, tip: &str) -> Vec<String> {
    let nothing = Vec::new();
    let Ok(merge_base) = git_stdout(path, &["merge-base", base, tip]) else {
        return nothing;
    };
    let merge_base = merge_base.trim().to_string();
    if merge_base.is_empty() {
        return nothing;
    }
    let Some(paths) = changed_paths(path, &merge_base, tip) else {
        return nothing;
    };
    let mut args: Vec<&str> = vec!["diff", "--name-only", "-z", base, tip, "--"];
    args.extend(paths.iter().map(String::as_str));
    let Ok(still_differs) = git_stdout(path, &args) else {
        return nothing;
    };
    if still_differs.split('\0').any(|p| !p.trim().is_empty()) {
        return nothing;
    }
    let range = format!("{merge_base}..{tip}");
    let Ok(listing) = git_stdout(path, &["rev-list", &range]) else {
        return nothing;
    };
    listing
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>()
}

/// The paths `<merge-base>..<tip>` touched, or `None` when they cannot be used.
///
/// Why: a path that comes back with a Unicode replacement character cannot be
/// handed to git as a pathspec — it would match nothing, and a pathspec that
/// matches nothing makes the containment diff come back EMPTY, which reads as
/// "everything landed". That is the one way this check could fail open, so a
/// path this process cannot spell back refuses the whole comparison.
/// What: `-z` so names arrive raw rather than C-quoted. `None` for a git
/// failure, for a divergence that touched no file (an empty or reverting
/// commit proves nothing about content), for a lossily-decoded name, and for
/// more than [`SUPERSEDED_PATH_MAX`] paths.
/// Test: `a_divergence_that_touched_no_file_is_not_cleared`,
/// `a_divergence_wider_than_the_path_bound_is_not_cleared`.
fn changed_paths(path: &Path, merge_base: &str, tip: &str) -> Option<Vec<String>> {
    let out = git_stdout(path, &["diff", "--name-only", "-z", merge_base, tip]).ok()?;
    let paths: Vec<String> = out
        .split('\0')
        .map(|p| p.trim_end_matches('\n'))
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    if paths.is_empty() || paths.len() > SUPERSEDED_PATH_MAX {
        return None;
    }
    if paths.iter().any(|p| p.contains('\u{FFFD}')) {
        return None;
    }
    Some(paths)
}

/// Is this worktree's whole divergence already held, byte for byte, by a
/// landing branch (#7889)?
///
/// Why: the ADR-0057 removal guard asks the same question the reclaim sweep's
/// gate 6 asks, and it must get the same answer — the fourteen trees of
/// 2026-09-21 were refused by both paths for the same reason.
/// What: true when [`content_superseded_commits`] clears `tip` against any of
/// [`landing_bases`](super::worktree_safety::landing_bases). False when no base
/// clears it, which is every failure arm as well.
/// Test: `a_superseded_divergence_is_landed_on_some_base`,
/// `an_unlanded_commit_is_landed_on_no_base`.
pub(crate) fn divergence_is_superseded(path: &Path, tip: &str) -> bool {
    super::worktree_safety::landing_bases(path)
        .iter()
        .any(|base| !content_superseded_commits(path, base, tip).is_empty())
}

#[cfg(test)]
#[path = "worktree_landed_content_tests.rs"]
mod worktree_landed_content_tests;
