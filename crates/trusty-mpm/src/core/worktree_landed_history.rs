//! Is a worktree's content on a base branch, even after the base moved on
//! (#8633, #8602)?
//!
//! Why: the ADR-0057 landed-content probe merged `HEAD` into the base's TIP
//! and read any `git merge-tree` failure as "not landed". A squash-merged
//! branch whose files `main` has edited since conflicts with that tip, so
//! every such worktree was refused — and because `merge-tree` reports a
//! conflict on stdout with exit 1, the refusal quoted an empty stderr and said
//! nothing. The content WAS on the remote: it landed at the squash commit,
//! which is an ancestor of the tip.
//!
//! What: [`content_on_base`] merges `HEAD` into the tip first. When that
//! merge conflicts or still changes files, it looks for an earlier commit on
//! the base's first-parent line, since `HEAD` forked, into which merging
//! `HEAD` changes nothing and whose own patch `HEAD` still holds in full.
//! Such a commit is on the remote, so the worktree
//! holds nothing the remote lacks. A conflict is told apart from a git error
//! by what `merge-tree` printed, never by its exit code alone.
//!
//! **Fail-open check.** An empty merge of `HEAD` into a history commit `M`
//! proves only that `HEAD`'s changes since the fork are all in `M`. It cannot
//! see a later branch commit that UNDOES part of `M` — a revert back to the
//! fork's version, or the deletion of a file `M` added — because relative to
//! the fork that commit changes nothing. So a history hit also has to pass the
//! reverse check: applying `M`'s own patch (against its first parent) onto
//! `HEAD` must change nothing. A branch that differs from every version the
//! base ever held, or that took back part of the landed change, fails one of
//! the two checks at every candidate and is reported not landed. Missing a
//! candidate (the walk is capped) under-reports "landed", which refuses.
//! Test: `worktree_landed_history_tests`.

use std::path::Path;

use crate::session_manager::worktree_safety::{git_command, git_stdout};

/// Most base commits the history walk merges `HEAD` into (#8633).
///
/// Why: each candidate costs two `git` subprocesses (`merge-tree`, then
/// `diff` on a clean merge), and a forward hit two more for the reverse check
/// plus one `rev-parse`; the removal guard must decide inside the `PreToolUse`
/// hook's budget. Missing the landing commit refuses, which is the safe
/// direction, and the refusal says the search was truncated.
/// What: the oldest this-many path-touching commits since the fork point.
const MAX_CANDIDATES: usize = 24;

/// Most changed paths passed to the candidate query as a pathspec (#8633).
///
/// Why: a squash commit touches every path the branch changed, so any subset
/// still selects it; the cap keeps the argument list bounded.
const MAX_PATHSPEC: usize = 256;

/// What merging a worktree's `HEAD` into a base established (#8633).
///
/// Why: "landed", "not landed" and "conflicted" lead to different refusal
/// text, and the refusal is only actionable if it says which one it was.
/// What: [`Landed`](Self::Landed) is the only variant that admits. `at` is
/// `None` when the base tip itself holds the content, or the earlier base
/// commit that does. `searched` counts the history commits tried, out of
/// `candidates` found; `searched < candidates` means the walk was capped.
/// Test: `a_squash_merged_branch_edited_over_on_main_is_landed`,
/// `a_different_version_of_the_change_on_main_is_not_landed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentOnBase {
    /// Merging `HEAD` into the tip changes no file; or merging it into `at`
    /// changes no file AND applying `at`'s own patch onto `HEAD` changes none.
    Landed {
        /// The earlier base commit that holds the content, if not the tip.
        at: Option<String>,
    },
    /// The tip merge is clean but changes `paths`; no earlier commit held it.
    Residual {
        /// What the merge into the tip would change, in git's order.
        paths: Vec<String>,
        /// History commits tried.
        searched: usize,
        /// History commits that touch `HEAD`'s changed paths, tried or not.
        candidates: usize,
    },
    /// The tip merge conflicts in `paths`; no earlier commit held it.
    Conflicted {
        /// The conflicted paths `merge-tree --name-only` listed.
        paths: Vec<String>,
        /// History commits tried.
        searched: usize,
        /// History commits that touch `HEAD`'s changed paths, tried or not.
        candidates: usize,
    },
}

impl ContentOnBase {
    /// True only for [`Landed`](Self::Landed).
    pub fn is_landed(&self) -> bool {
        matches!(self, Self::Landed { .. })
    }

    /// One clause naming the probe result against `base`, for a refusal or a
    /// grant (#8633).
    ///
    /// Test: `the_description_names_the_probe_result`.
    pub fn describe(&self, base: &str) -> String {
        match self {
            Self::Landed { at: None } => format!("merging HEAD into {base} changes no file"),
            Self::Landed { at: Some(at) } => format!(
                "HEAD's content landed on {base} at `{at}`; later commits on {base} edited the \
                 same files, so a merge into the tip no longer comes out empty"
            ),
            Self::Residual {
                paths,
                searched,
                candidates,
            } => format!(
                "merging HEAD into {base} would still change {} file(s) ({}), and {}",
                paths.len(),
                name_paths(paths),
                history_clause(base, *searched, *candidates)
            ),
            Self::Conflicted {
                paths,
                searched,
                candidates,
            } => format!(
                "merging HEAD into {base} conflicts in {} file(s) ({}), and {}",
                paths.len(),
                name_paths(paths),
                history_clause(base, *searched, *candidates)
            ),
        }
    }
}

/// What the history walk tried, saying so when [`MAX_CANDIDATES`] cut it
/// short (#8633) — a truncated search must not read as an exhaustive one.
fn history_clause(base: &str, searched: usize, candidates: usize) -> String {
    if candidates > searched {
        format!(
            "none of the oldest {searched} of {candidates} commit(s) on {base} since HEAD forked \
             holds HEAD's content; the newer {} were not searched",
            candidates - searched
        )
    } else {
        format!("none of the {searched} commit(s) on {base} since HEAD forked holds HEAD's content")
    }
}

/// Up to five backtick-quoted paths, then a count of the rest.
fn name_paths(paths: &[String]) -> String {
    let mut named: Vec<String> = paths.iter().take(5).map(|p| format!("`{p}`")).collect();
    if paths.len() > 5 {
        named.push(format!("and {} more", paths.len() - 5));
    }
    named.join(", ")
}

/// Is every change `HEAD` makes already on `base`, at its tip or at an
/// earlier commit on its first-parent line (#8633)?
///
/// Why: the one predicate both the landed-content admission and the guard's
/// merged-pull-request residue check ask. See the module doc.
/// What: [`merge_probe`] against `base`; an empty clean merge is
/// `Landed { at: None }`. Otherwise [`landed_in_history`] walks the base's
/// commits since the fork point, checking each in both directions; a hit is
/// `Landed { at: Some(sha) }`, a miss
/// is `Residual` or `Conflicted` naming the tip merge's paths. `Err` when git
/// could not answer — a bad ref, an unrelated history, a failed `diff` — with
/// git's own stderr quoted, which refuses on every caller.
/// Test: `a_squash_merged_branch_edited_over_on_main_is_landed`,
/// `a_different_version_of_the_change_on_main_is_not_landed`,
/// `a_merge_tree_git_error_is_undeterminable_and_quotes_stderr`.
pub fn content_on_base(dir: &Path, base: &str) -> Result<ContentOnBase, String> {
    let base = base.trim();
    if base.is_empty() {
        return Err("no base ref was supplied to judge this worktree's content against".into());
    }
    let (conflicted, paths) = match merge_probe(dir, base)? {
        MergeProbe::Clean(paths) if paths.is_empty() => {
            return Ok(ContentOnBase::Landed { at: None });
        }
        MergeProbe::Clean(paths) => (false, paths),
        MergeProbe::Conflicted(paths) => (true, paths),
    };
    let walk = landed_in_history(dir, base)?;
    Ok(match (walk.at, conflicted) {
        (Some(at), _) => ContentOnBase::Landed { at: Some(at) },
        (None, false) => ContentOnBase::Residual {
            paths,
            searched: walk.searched,
            candidates: walk.candidates,
        },
        (None, true) => ContentOnBase::Conflicted {
            paths,
            searched: walk.searched,
            candidates: walk.candidates,
        },
    })
}

/// The outcome of one in-memory `git merge-tree --write-tree`.
enum MergeProbe {
    /// The merge is clean; these paths differ between `ours` and its result.
    Clean(Vec<String>),
    /// The merge conflicts in these paths.
    Conflicted(Vec<String>),
}

impl MergeProbe {
    /// True for a clean merge that changes no file.
    fn is_noop(&self) -> bool {
        matches!(self, Self::Clean(paths) if paths.is_empty())
    }
}

/// Merge `HEAD` into `base` in memory and report what that would change.
fn merge_probe(dir: &Path, base: &str) -> Result<MergeProbe, String> {
    merge_in_memory(dir, None, base, "HEAD")
}

/// Merge `theirs` into `ours` in memory — over `merge_base` when given, else
/// over their merge base — and report what that would change (#7275, #7889,
/// #8633).
///
/// Why: `merge-tree` exits 1 BOTH for a conflict and for a ref it cannot
/// merge, and prints a conflict on stdout, not stderr. Reading every non-zero
/// exit as a failure is what produced the empty "failed (exit status: 1): "
/// refusal. A conflict writes a tree id as its first stdout line; an error
/// does not, so that line is the discriminator.
/// What: exit 0 — `git diff --name-only --ignore-submodules=none <ours>
/// <tree>` names the residue (`--ignore-submodules=none` keeps a config
/// setting from hiding a gitlink bump, #7889). Exit 1 with a tree id — the
/// conflicted paths `--name-only` listed. Anything else is `Err` quoting
/// git's stderr.
/// Test: `a_merge_tree_git_error_is_undeterminable_and_quotes_stderr`,
/// `a_gitlink_bump_is_residue_even_when_submodule_diffs_are_ignored`,
/// `a_post_squash_partial_revert_is_not_landed`.
fn merge_in_memory(
    dir: &Path,
    merge_base: Option<&str>,
    ours: &str,
    theirs: &str,
) -> Result<MergeProbe, String> {
    let mut args = vec!["merge-tree", "--write-tree", "--name-only"];
    if let Some(merge_base) = merge_base {
        args.extend(["--merge-base", merge_base]);
    }
    args.extend([ours, theirs]);
    let shown = format!("git {}", args.join(" "));
    let out = git_command(dir, &args)
        .output()
        .map_err(|e| format!("`{shown}` could not be run: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let tree = lines
        .next()
        .map(str::trim)
        .filter(|t| t.len() >= 40 && t.bytes().all(|b| b.is_ascii_hexdigit()));
    match (out.status.code(), tree) {
        (Some(0), Some(tree)) => {
            let residue = git_stdout(
                dir,
                &[
                    "diff",
                    "--name-only",
                    "--ignore-submodules=none",
                    ours,
                    tree,
                ],
            )
            .map_err(|e| format!("`git diff --name-only {ours} <merged tree>` failed: {e}"))?;
            Ok(MergeProbe::Clean(non_empty_lines(&residue)))
        }
        // Conflicted-file info runs up to the first blank line; the
        // informational messages after it are not paths.
        (Some(1), Some(_)) => Ok(MergeProbe::Conflicted(
            lines
                .take_while(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .collect(),
        )),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            Err(format!(
                "`{shown}` could not answer ({}, not a merge conflict): {}",
                out.status,
                if stderr.is_empty() {
                    "git wrote nothing to stderr"
                } else {
                    stderr
                }
            ))
        }
    }
}

/// What the history walk found: the landing commit, if any, and how many of
/// the candidate commits it tried.
struct HistoryWalk {
    at: Option<String>,
    searched: usize,
    candidates: usize,
}

/// The earliest commit on `base`'s first-parent line, since `HEAD` forked,
/// whose content equals `HEAD`'s in both directions (#8633).
///
/// Why: a squash commit carries the branch's content, and later commits may
/// edit the same lines. The squash is on the remote, so finding it proves the
/// content is too — provided `HEAD` has not since taken part of it back.
/// What: `git merge-base <base> HEAD`; the paths `HEAD` changed since then;
/// the first-parent commits in `<fork>..<base>` that touch any of them, of
/// which the oldest [`MAX_CANDIDATES`] are tried, oldest first. A candidate
/// wins when merging `HEAD` into it changes nothing AND
/// [`head_keeps_patch_of`] holds. Pathspecs are literal, so a file name is
/// never read as magic.
/// Test: `a_squash_merged_branch_edited_over_on_main_is_landed`,
/// `a_different_version_of_the_change_on_main_is_not_landed`,
/// `a_post_squash_partial_revert_is_not_landed`,
/// `a_post_squash_deletion_of_a_squashed_file_is_not_landed`.
fn landed_in_history(dir: &Path, base: &str) -> Result<HistoryWalk, String> {
    let fork = git_stdout(dir, &["merge-base", base, "HEAD"])
        .map_err(|e| format!("the fork point of HEAD and {base} could not be found: {e}"))?;
    let fork = fork.trim();
    let changed = git_stdout(
        dir,
        &[
            "diff",
            "--name-only",
            "-z",
            "--ignore-submodules=none",
            fork,
            "HEAD",
        ],
    )
    .map_err(|e| format!("the paths HEAD changed since {fork} could not be listed: {e}"))?;
    let paths: Vec<&str> = changed
        .split('\0')
        .filter(|p| !p.is_empty())
        .take(MAX_PATHSPEC)
        .collect();
    if paths.is_empty() {
        return Ok(HistoryWalk {
            at: None,
            searched: 0,
            candidates: 0,
        });
    }
    let range = format!("{fork}..{base}");
    let mut args = vec!["rev-list", "--first-parent", "--reverse", &range, "--"];
    args.extend(paths);
    let out = git_command(dir, &args)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .output()
        .map_err(|e| format!("`git rev-list {range}` could not be run: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git rev-list {range}` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let listed = non_empty_lines(&String::from_utf8_lossy(&out.stdout));
    let candidates = listed.len();
    let mut searched = 0;
    for candidate in listed.into_iter().take(MAX_CANDIDATES) {
        searched += 1;
        // #8633 critic round: the forward merge alone cannot see a branch
        // commit that undid part of `candidate`; the reverse check can.
        if merge_probe(dir, &candidate)?.is_noop() && head_keeps_patch_of(dir, &candidate)? {
            return Ok(HistoryWalk {
                at: Some(candidate),
                searched,
                candidates,
            });
        }
    }
    Ok(HistoryWalk {
        at: None,
        searched,
        candidates,
    })
}

/// Does applying `commit`'s own patch — against its first parent — onto
/// `HEAD` change nothing (#8633)?
///
/// Why: an empty merge of `HEAD` into `commit` proves `HEAD`'s changes since
/// the fork are all in `commit`, not that `HEAD` still holds all of
/// `commit`'s. A branch commit after the squash that reverts a line to the
/// fork's version, or deletes a file the squash added, is invisible to that
/// merge, and admitting the removal would destroy it.
/// What: `git merge-tree --write-tree --merge-base <commit>^1 HEAD <commit>`,
/// then the result must equal `HEAD`. A line `commit` changed that `HEAD`
/// lacks is either taken from `commit` (a residue) or conflicts; either one
/// is `false`. A merge commit's patch is everything it brought onto its first
/// parent, which is stricter, never looser. A `commit` with no first parent,
/// or any git error, is `Err` and refuses.
/// Test: `a_post_squash_partial_revert_is_not_landed`,
/// `a_post_squash_deletion_of_a_squashed_file_is_not_landed`,
/// `content_landed_by_a_merge_commit_edited_over_on_main_is_landed`.
fn head_keeps_patch_of(dir: &Path, commit: &str) -> Result<bool, String> {
    let spec = format!("{commit}^1^{{commit}}");
    let parent = git_stdout(dir, &["rev-parse", "--verify", "--quiet", &spec])
        .map_err(|e| format!("the first parent of `{commit}` could not be resolved: {e}"))?;
    let parent = parent.trim();
    if parent.is_empty() {
        return Err(format!("`{commit}` has no first parent to diff it against"));
    }
    Ok(merge_in_memory(dir, Some(parent), "HEAD", commit)?.is_noop())
}

/// Trimmed, non-empty lines of `text`.
fn non_empty_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
#[path = "worktree_landed_history_tests.rs"]
mod worktree_landed_history_tests;
