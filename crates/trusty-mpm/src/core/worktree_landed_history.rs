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
//! What: [`content_on_base`] admits a `HEAD` that is an ancestor of the base
//! outright — a plain merge or a fast-forward. Any other `HEAD` needs a
//! specific landing commit `M` on the base: the tip first, then the commits on
//! the base's first-parent line since `HEAD` forked, oldest first. `M` counts
//! only when four checks pass (the last three are [`judge`]): merging `HEAD`
//! into `M` changes nothing; `M`'s own patch (`M^1..M`) changes a path `HEAD` changed
//! since the fork; every path a branch commit touched is in that patch or is
//! identical in `HEAD` and `M`; and applying that patch onto `HEAD` changes no
//! branch-touched path. A conflict is told apart from a git error by what
//! `merge-tree` printed, never by its exit code alone.
//!
//! **Fail-open checks.** An empty merge of `HEAD` into `M` cannot see a later
//! branch commit that UNDOES part of a landing — a revert to the fork's
//! version, or the deletion of a file the squash added — because relative to
//! the fork that commit changes nothing. The reverse check sees the undo, but
//! only on the paths `M`'s patch changes. So a commit whose patch misses
//! `HEAD`'s files (an unrelated `u.txt` pushed after the squash) is never a
//! landing commit, and a path the branch touched that `M`'s patch does not
//! reach must be identical in `HEAD` and `M`. The tip gets no shortcut (#8633
//! round 3). The reverse check ignores residue on paths no branch commit
//! touched, the basis being the 2026-09-22 owner ruling that the #7889 donor
//! squash — which also carried a sibling's files — is admitted (ADR-0057).
//! **Known residual:** a later base commit whose patch `HEAD` holds, on the
//! same files, stands in as `M` — a second pull request from the same branch,
//! or a cherry-pick of a branch commit onto the base. An unpushed undo of an
//! EARLIER landing's lines on those files is then not seen. Missing a
//! candidate (the walk is capped) under-reports "landed", which refuses.
//! Test: `worktree_landed_history_tests`.

use std::collections::HashSet;
use std::path::Path;

use crate::session_manager::worktree_safety::{git_command, git_stdout};

/// Most base commits the history walk merges `HEAD` into (#8633).
///
/// Why: each candidate costs two `git` subprocesses (`merge-tree`, then
/// `diff` on a clean merge), and a forward hit two more for the reverse check
/// plus one `rev-parse`; the removal guard must decide inside the `PreToolUse`
/// hook's budget. Missing the landing commit refuses, which is the safe
/// direction, and the refusal says the search was truncated.
/// What: the oldest this-many path-touching commits since the fork point;
/// the tip is tried in addition to them.
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
/// `None` when `HEAD` is an ancestor of the base, else the base commit —
/// possibly the tip — that holds the content in both directions. `searched`
/// counts the history commits tried, out of `candidates` found;
/// `searched < candidates` means the walk was capped. The tip is tried in
/// addition to them.
/// Test: `a_squash_merged_branch_edited_over_on_main_is_landed`,
/// `a_different_version_of_the_change_on_main_is_not_landed`,
/// `a_partial_revert_while_main_is_unmoved_is_not_landed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentOnBase {
    /// `HEAD` is an ancestor of the base (`at: None`); or merging `HEAD` into
    /// `at` changes no file AND applying `at`'s own patch onto `HEAD` changes
    /// none.
    Landed {
        /// The base commit that holds the content; `None` for an ancestor.
        at: Option<String>,
    },
    /// The tip merge changes no file, but `HEAD` is not an ancestor of the
    /// base and no base commit holds its content in both directions: `HEAD`
    /// undid part of what landed at `at` (#8633 round 3).
    Undone {
        /// The landing commit `HEAD` no longer holds in full.
        at: String,
        /// What applying `at`'s own patch onto `HEAD` would change.
        paths: Vec<String>,
        /// History commits tried.
        searched: usize,
        /// History commits that touch `HEAD`'s changed paths, tried or not.
        candidates: usize,
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
            Self::Landed { at: None } => format!("HEAD is an ancestor of {base}"),
            Self::Landed { at: Some(at) } => format!(
                "HEAD's content landed on {base} at `{at}`: merging HEAD into that commit \
                 changes no file, and HEAD still holds all of that commit's own change"
            ),
            Self::Undone {
                at,
                paths,
                searched,
                candidates,
            } => format!(
                "merging HEAD into {base} changes no file, but HEAD is not on {base} and no \
                 longer holds all of what landed at `{at}` — {} file(s) HEAD's own commits \
                 touched ({}) either lack part of that commit's own change, or differ from that \
                 commit where its change never reached them: a revert or deletion made after a \
                 landing, or a change on those files HEAD never had; {}",
                paths.len(),
                name_paths(paths),
                history_clause(base, *searched, *candidates)
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
/// short (#8633) — a truncated search must not read as an exhaustive one. The
/// tip is always tried as well (#8633 round 3).
fn history_clause(base: &str, searched: usize, candidates: usize) -> String {
    if candidates > searched {
        format!(
            "none of the oldest {searched} of {candidates} commit(s) on {base} since HEAD forked \
             holds HEAD's content in both directions, nor does its tip; the newer {} were not \
             searched",
            candidates - searched
        )
    } else {
        format!(
            "none of the {searched} commit(s) on {base} since HEAD forked holds HEAD's content \
             in both directions, nor does its tip"
        )
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
/// What: [`merge_probe`] against `base`. An empty merge whose `HEAD` is an
/// ancestor of `base` is `Landed { at: None }`. Every other `HEAD` goes to
/// [`landed_in_history`], which tries the tip and then the base's commits
/// since the fork point, each in both directions; a hit is
/// `Landed { at: Some(sha) }`. A miss is `Residual` or `Conflicted` naming
/// the tip merge's paths, or `Undone` when the tip merge was empty. `Err`
/// when git could not answer — a bad ref, an unrelated history, a failed
/// `diff`, an ancestry check that errored — with git's own stderr quoted,
/// which refuses on every caller.
/// Test: `a_squash_merged_branch_edited_over_on_main_is_landed`,
/// `a_different_version_of_the_change_on_main_is_not_landed`,
/// `a_merge_tree_git_error_is_undeterminable_and_quotes_stderr`,
/// `a_partial_revert_while_main_is_unmoved_is_not_landed`,
/// `a_head_that_is_an_ancestor_of_main_is_landed`.
pub fn content_on_base(dir: &Path, base: &str) -> Result<ContentOnBase, String> {
    let base = base.trim();
    if base.is_empty() {
        return Err("no base ref was supplied to judge this worktree's content against".into());
    }
    let tip = merge_probe(dir, base)?;
    // #8633 round 3: an empty tip merge admits on its own only for an
    // ancestor; anything else needs a two-way landing commit.
    if tip.is_noop() && head_is_ancestor_of(dir, base)? {
        return Ok(ContentOnBase::Landed { at: None });
    }
    let walk = landed_in_history(dir, base, tip.is_noop())?;
    if let Some(at) = walk.at {
        return Ok(ContentOnBase::Landed { at: Some(at) });
    }
    let (searched, candidates) = (walk.searched, walk.candidates);
    match tip {
        MergeProbe::Clean(paths) if paths.is_empty() => match walk.undone {
            Some((at, paths)) => Ok(ContentOnBase::Undone {
                at,
                paths,
                searched,
                candidates,
            }),
            // #8633 final round: every forward hit was ineligible — its own
            // change touches no file HEAD changed — so nothing proves the
            // content landed. Refuse.
            None => Err(format!(
                "merging HEAD into {base} changes no file and HEAD is not an ancestor of it, \
                 but no commit on {base} whose own change touches a file HEAD changed holds \
                 HEAD's content, so nothing proves it landed"
            )),
        },
        MergeProbe::Clean(paths) => Ok(ContentOnBase::Residual {
            paths,
            searched,
            candidates,
        }),
        MergeProbe::Conflicted(paths) => Ok(ContentOnBase::Conflicted {
            paths,
            searched,
            candidates,
        }),
    }
}

/// Is `HEAD` an ancestor of `base` — a plain merge or a fast-forward (#8633
/// round 3)?
///
/// What: `git merge-base --is-ancestor HEAD <base>`; exit 0 is `true`, exit 1
/// is `false`, anything else is `Err` quoting git's stderr, which refuses.
/// Test: `a_head_that_is_an_ancestor_of_main_is_landed`,
/// `an_ancestry_check_git_cannot_answer_refuses`.
fn head_is_ancestor_of(dir: &Path, base: &str) -> Result<bool, String> {
    let out = git_command(dir, &["merge-base", "--is-ancestor", "HEAD", base])
        .output()
        .map_err(|e| format!("`git merge-base --is-ancestor HEAD {base}` could not be run: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "`git merge-base --is-ancestor HEAD {base}` could not answer ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
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
/// What: exit 0 — `git diff --name-only --no-renames --ignore-submodules=none <ours>
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
                    // #8633 round 3: both sides of a rename, so a
                    // branch-scoped reverse check sees the path it touched.
                    "--no-renames",
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

/// What the history walk found: the landing commit, if any, how many of the
/// candidate commits it tried, and the first forward hit that failed the
/// reverse check, with what `HEAD` lacks of it.
struct HistoryWalk {
    at: Option<String>,
    searched: usize,
    candidates: usize,
    undone: Option<(String, Vec<String>)>,
}

/// The branch's paths, computed once per walk (#8633 final round).
struct BranchPaths {
    /// Paths `git diff <fork> HEAD` names: what `HEAD` changed, net.
    changed: HashSet<String>,
    /// Paths any commit in `<fork>..HEAD` touched ([`branch_touched_paths`]).
    touched: HashSet<String>,
}

/// What one candidate landing commit proved (#8633 final round).
enum Judged {
    /// Its own change touches no file `HEAD` changed, so it proves nothing.
    Ineligible,
    /// `HEAD` holds nothing the commit lacks and lacks nothing it carried.
    Lands,
    /// These branch-touched paths are not proven on the remote.
    Lacks(Vec<String>),
}

/// The base commit — the tip first, then the earliest on `base`'s
/// first-parent line since `HEAD` forked — whose content equals `HEAD`'s in
/// both directions (#8633).
///
/// Why: a squash commit carries the branch's content, and later commits may
/// edit the same lines. The squash is on the remote, so finding it proves the
/// content is too — provided `HEAD` has not since taken part of it back. The
/// tip is a candidate like any other (#8633 round 3): when `main` has not
/// moved since the squash, the tip IS the squash, and it gets no shortcut.
/// What: `git merge-base <base> HEAD` is the fork; the branch's changed and
/// touched paths are listed once. The tip's forward check is the caller's tip
/// merge (`tip_forward_noop`); on a forward hit it runs [`judge`]. Then the
/// first-parent commits in `<fork>..<base>` that touch a path `HEAD` changed,
/// of which the oldest [`MAX_CANDIDATES`] are tried, oldest first, the tip
/// skipped as already tried. A candidate wins when merging `HEAD` into it
/// changes nothing AND [`judge`] says it lands. Pathspecs are literal, so a
/// file name is never read as magic. `undone` prefers the oldest history
/// forward hit over the tip, since that is where the content landed.
/// Test: `a_squash_merged_branch_edited_over_on_main_is_landed`,
/// `a_different_version_of_the_change_on_main_is_not_landed`,
/// `a_post_squash_partial_revert_is_not_landed`,
/// `a_post_squash_deletion_of_a_squashed_file_is_not_landed`,
/// `a_partial_revert_while_main_is_unmoved_is_not_landed`,
/// `a_rebase_merged_branch_is_landed_at_its_last_replayed_commit`,
/// `an_unpushed_revert_with_an_unrelated_main_commit_after_the_squash_is_not_landed`.
fn landed_in_history(
    dir: &Path,
    base: &str,
    tip_forward_noop: bool,
) -> Result<HistoryWalk, String> {
    let spec = format!("{base}^{{commit}}");
    let tip = git_stdout(dir, &["rev-parse", "--verify", "--quiet", &spec])
        .map_err(|e| format!("the tip commit of {base} could not be resolved: {e}"))?;
    let tip = tip.trim().to_string();
    if tip.is_empty() {
        return Err(format!("{base} does not name a commit"));
    }
    let mut walk = HistoryWalk {
        at: None,
        searched: 0,
        candidates: 0,
        undone: None,
    };
    let fork = git_stdout(dir, &["merge-base", base, "HEAD"])
        .map_err(|e| format!("the fork point of HEAD and {base} could not be found: {e}"))?;
    let fork = fork.trim();
    let branch = BranchPaths {
        changed: diff_names(dir, fork, "HEAD")?,
        touched: branch_touched_paths(dir, fork)?,
    };
    let mut tip_undone = None;
    if tip_forward_noop {
        match judge(dir, &tip, &branch)? {
            Judged::Ineligible => {}
            Judged::Lands => {
                walk.at = Some(tip);
                return Ok(walk);
            }
            Judged::Lacks(lacked) => tip_undone = Some((tip.clone(), lacked)),
        }
    }
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
        walk.undone = tip_undone;
        return Ok(walk);
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
    walk.candidates = listed.len();
    for candidate in listed.into_iter().take(MAX_CANDIDATES) {
        walk.searched += 1;
        if candidate == tip {
            continue; // #8633 round 3: tried first, above.
        }
        // #8633 critic round: the forward merge alone cannot see a branch
        // commit that undid part of `candidate`; the reverse check can.
        if !merge_probe(dir, &candidate)?.is_noop() {
            continue;
        }
        match judge(dir, &candidate, &branch)? {
            Judged::Ineligible => {}
            Judged::Lands => {
                walk.at = Some(candidate);
                return Ok(walk);
            }
            Judged::Lacks(lacked) if walk.undone.is_none() => {
                walk.undone = Some((candidate, lacked));
            }
            Judged::Lacks(_) => {}
        }
    }
    walk.undone = walk.undone.take().or(tip_undone);
    Ok(walk)
}

/// Can `commit` be the commit `HEAD`'s content landed at (#8633)?
///
/// Why: the forward merge (`HEAD` into `commit`, run by the caller) proves
/// `HEAD`'s net change since the fork is in `commit`. It cannot see a branch
/// commit that UNDID part of a landing — a revert to the fork's version, or
/// the deletion of a file the squash added — because relative to the fork
/// that commit changes nothing. Three further checks close that:
/// 1. Eligibility. `commit`'s own patch (`commit^1..commit`) must change a
///    path `HEAD` changed since the fork. A commit that only added an
///    unrelated `u.txt` proves nothing about `HEAD`'s files; before the #8633
///    final round the tip in that shape was admitted with every reverse
///    residue filtered away, and an unpushed revert was lost.
/// 2. Coverage. The reverse check sees only paths `commit`'s patch changes,
///    so every path a branch commit touched must be in that patch or be
///    byte-identical in `HEAD` and `commit`. Otherwise an undo on a path
///    `commit` never touched — `g.txt`, undone after an earlier squash, while
///    `commit` is a later landing of `f.txt` — would pass unseen.
/// 3. Reverse. `commit`'s own patch applied onto `HEAD` must change nothing
///    on a branch-touched path ([`lacked_patch`]).
///
/// What: `Ineligible` fails check 1 and is skipped, never admitted; `Lacks`
/// names the uncovered paths (check 2) or what `HEAD` lacks (check 3);
/// `Lands` passes all three. A `commit` with no first parent, or any git
/// error, is `Err` and refuses.
/// Test: `an_unpushed_revert_with_an_unrelated_main_commit_after_the_squash_is_not_landed`,
/// `an_unpushed_deletion_with_an_unrelated_main_commit_after_the_squash_is_not_landed`,
/// `a_later_landing_that_leaves_an_undone_file_uncovered_is_not_landed`,
/// `a_rebase_merged_branch_is_landed_at_its_last_replayed_commit`.
fn judge(dir: &Path, commit: &str, branch: &BranchPaths) -> Result<Judged, String> {
    let spec = format!("{commit}^1^{{commit}}");
    let parent = git_stdout(dir, &["rev-parse", "--verify", "--quiet", &spec])
        .map_err(|e| format!("the first parent of `{commit}` could not be resolved: {e}"))?;
    let parent = parent.trim();
    if parent.is_empty() {
        return Err(format!("`{commit}` has no first parent to diff it against"));
    }
    let patch = diff_names(dir, parent, commit)?;
    if patch.is_disjoint(&branch.changed) {
        return Ok(Judged::Ineligible);
    }
    let differs = diff_names(dir, commit, "HEAD")?;
    let mut uncovered: Vec<String> = branch
        .touched
        .iter()
        .filter(|p| differs.contains(*p) && !patch.contains(*p))
        .cloned()
        .collect();
    if !uncovered.is_empty() {
        uncovered.sort();
        return Ok(Judged::Lacks(uncovered));
    }
    Ok(match lacked_patch(dir, parent, commit, &branch.touched)? {
        None => Judged::Lands,
        Some(lacked) => Judged::Lacks(lacked),
    })
}

/// What `HEAD` lacks of `commit`'s own patch on paths the branch worked on,
/// or `None` when it lacks nothing there (#8633 round 3).
///
/// Why: a landing commit may carry MORE than this branch — the #7889 donor
/// shape squashes a parked branch together with its `-r2` sibling's work, and
/// the 2026-09-22 owner ruling (ADR-0057) says that tree is admitted. A path
/// no branch commit touched is the fork's version in `HEAD`, which is on the
/// remote, so `HEAD` lacking a sibling's change there loses nothing. A branch
/// that undid part of the landing did so in a commit of its own, which
/// touched the path. [`judge`] only calls this once `commit`'s patch covers
/// every branch-touched path `HEAD` and `commit` disagree on.
/// What: `git merge-tree --write-tree --merge-base <parent> HEAD <commit>`.
/// A line `commit` changed that `HEAD` lacks is either taken from `commit`
/// (a residue) or conflicts. A conflict lacks all its paths; a clean residue
/// lacks only the paths in `touched`. A merge commit's patch is everything it
/// brought onto its first parent, which is stricter, never looser.
/// Test: `a_partial_revert_while_main_is_unmoved_is_not_landed`,
/// `a_post_squash_deletion_at_the_unmoved_tip_is_not_landed`,
/// `content_landed_by_a_merge_commit_edited_over_on_main_is_landed`,
/// `worktree_7889_the_sweep_admits_a_real_donor_branch`,
/// `extra_content_on_a_touched_path_is_not_landed`.
fn lacked_patch(
    dir: &Path,
    parent: &str,
    commit: &str,
    touched: &HashSet<String>,
) -> Result<Option<Vec<String>>, String> {
    let residue = match merge_in_memory(dir, Some(parent), "HEAD", commit)? {
        MergeProbe::Conflicted(paths) => return Ok(Some(paths)),
        MergeProbe::Clean(paths) => paths,
    };
    let lacked: Vec<String> = residue
        .into_iter()
        .filter(|p| touched.contains(p))
        .collect();
    Ok((!lacked.is_empty()).then_some(lacked))
}

/// The paths that differ between `from` and `to`, quoted as every other
/// path set in this module is (#8633 final round).
///
/// What: `git diff --name-only --no-renames --ignore-submodules=none`, so a
/// rename lists both sides and a gitlink bump is never hidden by config.
fn diff_names(dir: &Path, from: &str, to: &str) -> Result<HashSet<String>, String> {
    let out = git_stdout(
        dir,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=none",
            from,
            to,
        ],
    )
    .map_err(|e| {
        format!("the paths that differ between {from} and {to} could not be listed: {e}")
    })?;
    Ok(non_empty_lines(&out).into_iter().collect())
}

/// Every path any commit in `<fork>..HEAD` touched, merges included (#8633
/// round 3).
///
/// What: `git log --format= --name-only --no-renames --ignore-submodules=none
/// -m <fork>..HEAD`. `-m` lists a merge commit's paths against each parent,
/// `--no-renames` lists both sides of a rename, and `--ignore-submodules=none`
/// keeps config from hiding a gitlink, so the set only ever errs larger, which
/// refuses more. A failed `git log` is `Err`, never an empty set. Paths are
/// quoted exactly as `git diff --name-only` quotes them.
/// Test: `a_failed_git_log_of_the_branch_paths_refuses`,
/// `a_partial_revert_while_main_is_unmoved_is_not_landed`.
fn branch_touched_paths(dir: &Path, fork: &str) -> Result<HashSet<String>, String> {
    let range = format!("{fork}..HEAD");
    let out = git_stdout(
        dir,
        &[
            "log",
            "--format=",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=none",
            "-m",
            &range,
        ],
    )
    .map_err(|e| format!("the paths the commits in {range} touched could not be listed: {e}"))?;
    Ok(non_empty_lines(&out).into_iter().collect())
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
