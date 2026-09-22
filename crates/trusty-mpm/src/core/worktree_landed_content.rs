//! Is a worktree's content already standing on its landing base (#7889)?
//!
//! Why: both reclaim ladders — the ADR-0057 removal guard and
//! `tm session prune-worktrees --merged-prs` — stop at "GitHub has no MERGED
//! pull request for this branch". The continuation rule this repository runs on
//! produces that state constantly: a parked agent's branch is fast-forwarded
//! onto a sibling `-r2`…`-r9` head and squash-merged under THAT name, so no
//! pull request ever carries the parked branch's own name. Fourteen such trees
//! were stuck on 2026-09-21 and five more on 2026-09-22, each holding a
//! 10–25 GB `target-*/` directory and each byte-identical to `origin/main`.
//! Owner ruling 2026-09-22: a clean, sole-owned tree whose content is landed is
//! admissible.
//!
//! What: [`landed_content_verdict`] refreshes `origin` under a caller-chosen
//! bound, resolves the landing base, and asks whether merging `HEAD` into that
//! base would change any file. An empty answer is the grant; a non-empty one
//! names the first residual path for the refusal.
//!
//! **Content, never ancestry.** Every merge here is a squash, so the branch tip
//! is structurally not an ancestor of the squash commit and
//! `git merge-base --is-ancestor` answers "not merged" for a tree that is safe
//! to reclaim. `git cherry`'s patch ids miss it for the mirror-image reason: a
//! squash of two or more commits matches none of them. Neither is used here.
//!
//! **Every failure arm refuses**, per
//! [ADR-0045](../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
//! — a failed or expired refresh, a base that will not resolve, a `merge-tree`
//! that errored or conflicted, and a `diff` that could not be read all return
//! [`LandedContent::Unavailable`], which grants nothing. A grant on stale refs
//! is the exact failure this admission would otherwise introduce.
//! Test: `worktree_landed_content_tests`.

use std::path::Path;
use std::time::Duration;

use crate::session_manager::worktree_landing_refresh::refresh_landing_refs_within;
use crate::session_manager::worktree_safety::git_stdout;

/// The admission's name, quoted by both ladders' refusals (#7889).
///
/// Why: the guard and the sweep must spell the check the same way, or an
/// operator grepping one refusal cannot find the other. Defined here because
/// this module is the only thing both ladders share.
/// What: the `landed-content` slug.
/// Test: `the_note_names_the_admission_in_every_arm`.
pub const LANDED_CONTENT_CHECK: &str = "landed-content";

/// The refs the landing base is looked for on, in order (#7889).
///
/// Why: `refs/remotes/origin/HEAD` is the authoritative answer and a real clone
/// always has it, but a checkout built by `git init` + `git remote add` does
/// not — so the two conventional default names are tried after it rather than
/// leaving such a repository with no admission at all. The order matters: the
/// repository's own declared default outranks a guess, and a repository with
/// none of the three resolves nothing and refuses.
/// What: tried left to right; the first that resolves to a commit wins.
/// Test: `a_worktree_with_no_resolvable_landing_base_is_unavailable`.
const BASE_CANDIDATES: &[&str] = &["origin/HEAD", "origin/main", "origin/master"];

/// Whether a worktree's content is already on its landing base (#7889).
///
/// Why: three outcomes, not two. "Not landed" and "could not be established"
/// lead to different refusal text and, more importantly, must never collapse
/// into each other on a path whose grant deletes a directory.
/// What: [`Landed`](Self::Landed) is the only variant that admits.
/// Test: `a_divergence_landed_by_another_route_reports_landed`,
/// `an_unlanded_commit_reports_its_residual_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandedContent {
    /// Merging `HEAD` into `base` would change nothing — the content is there.
    Landed {
        /// The ref the comparison was made against, e.g. `origin/main`.
        base: String,
        /// That ref's own commit, so a refusal or a grant is reproducible.
        base_sha: String,
    },
    /// Merging `HEAD` into `base` would still change files.
    Residual {
        /// The ref the comparison was made against.
        base: String,
        /// The first path the merge would change, in git's own ordering.
        first_path: String,
    },
    /// The question could not be answered, which never admits.
    Unavailable {
        /// Why, in the words of whatever failed.
        detail: String,
    },
}

impl LandedContent {
    /// An undeterminable answer carrying `detail`.
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::Unavailable {
            detail: detail.into(),
        }
    }

    /// True only for [`Landed`](Self::Landed) — the one variant that admits.
    pub fn is_landed(&self) -> bool {
        matches!(self, Self::Landed { .. })
    }

    /// One sentence naming what this admission decided, for either ladder's
    /// operator-facing message (#7889).
    ///
    /// Why: the guard appends it to a deny and the sweep records it as a spare
    /// reason. Rendering it here rather than at each call site is what keeps
    /// the two ladders from describing the same verdict differently.
    /// What: the grant names the base and its commit; the residual arm names
    /// the base and the FIRST path the merge would still change; the
    /// undeterminable arm quotes what failed.
    /// Test: `the_note_names_the_admission_in_every_arm`,
    /// `worktree_7889_a_residual_path_denies_and_names_it`.
    pub fn note(&self) -> String {
        match self {
            Self::Landed { base, base_sha } => format!(
                "ADR-0057's `{LANDED_CONTENT_CHECK}` admission applies: merging HEAD into \
                 {base} (`{base_sha}`) would change no file, so this tree holds nothing the \
                 remote does not already have"
            ),
            Self::Residual { base, first_path } => format!(
                "ADR-0057's `{LANDED_CONTENT_CHECK}` admission does not apply either: merging \
                 HEAD into {base} would still change `{first_path}`, so this tree holds work \
                 that is on no remote"
            ),
            Self::Unavailable { detail } => format!(
                "ADR-0057's `{LANDED_CONTENT_CHECK}` admission could not be established \
                 either — {detail} — and a fact that cannot be established never grants"
            ),
        }
    }
}

/// Is every byte this worktree holds already on its landing base (#7889)?
///
/// Why: the predicate BOTH reclaim ladders ask, so one implementation answers
/// for both and they cannot give one worktree opposite verdicts. See the
/// module doc for the shape it admits and the ruling behind it.
/// What: in order — refresh `origin` within `refresh_timeout` (a failure or an
/// expiry is [`LandedContent::Unavailable`], never a comparison against the
/// stale refs it left); resolve the landing base from [`BASE_CANDIDATES`];
/// then [`merge_residue`]. An empty residue is
/// [`Landed`](LandedContent::Landed), a non-empty one is
/// [`Residual`](LandedContent::Residual) naming its first path.
///
/// This decides nothing on its own: both callers reach it only after their own
/// clean-tree and ownership gates have passed, and neither treats anything but
/// [`LandedContent::is_landed`] as a grant.
/// Test: `a_divergence_landed_by_another_route_reports_landed`,
/// `an_unlanded_commit_reports_its_residual_path`,
/// `a_worktree_with_no_resolvable_landing_base_is_unavailable`,
/// `a_refresh_that_fails_never_reports_landed`.
pub fn landed_content_verdict(dir: &Path, refresh_timeout: Duration) -> LandedContent {
    // The refs this comparison rests on are a LOCAL cache. A squash that landed
    // on GitHub is invisible here until a fetch brings it in, and judging
    // against the pre-merge ref would report residue for a tree holding none —
    // the refusing direction, but also the one that keeps 14 stuck trees stuck.
    // A refresh that fails is undeterminable, never "compare anyway".
    if let Err(e) = refresh_landing_refs_within(dir, refresh_timeout) {
        return LandedContent::unavailable(format!(
            "`origin` could not be refreshed, so the remote-tracking refs cannot be trusted to \
             show what has landed: {e}"
        ));
    }
    let Some((base, base_sha)) = landing_base(dir) else {
        return LandedContent::unavailable(format!(
            "no landing base resolved in this worktree — none of {} names a commit",
            BASE_CANDIDATES.join(", ")
        ));
    };
    match merge_residue(dir, &base) {
        Err(e) => LandedContent::unavailable(e),
        Ok(residue) => match residue.first() {
            None => LandedContent::Landed { base, base_sha },
            Some(first_path) => LandedContent::Residual {
                base,
                first_path: first_path.clone(),
            },
        },
    }
}

/// The ref this worktree's content is judged against, and its commit (#7889).
///
/// Why: with no pull request there is no `baseRefName` to take the base from,
/// so it has to be resolved locally — and resolved to something that actually
/// exists, because `git merge-tree` against a ref that does not resolve errors
/// out and would otherwise read as "undeterminable" for a reason the operator
/// cannot act on.
/// What: the first of [`BASE_CANDIDATES`] that `rev-parse --verify` resolves to
/// a commit, paired with that commit. `origin/HEAD` is reported under the name
/// it points at, so the message an operator reads says `origin/main` rather
/// than a symbolic ref. `None` when none of them resolve.
/// Test: `a_worktree_with_no_resolvable_landing_base_is_unavailable`.
fn landing_base(dir: &Path) -> Option<(String, String)> {
    for candidate in BASE_CANDIDATES {
        let Ok(sha) = git_stdout(
            dir,
            &["rev-parse", "--verify", &format!("{candidate}^{{commit}}")],
        ) else {
            continue;
        };
        let sha = sha.trim().to_string();
        if sha.is_empty() {
            continue;
        }
        // `origin/HEAD` is symbolic; name the branch it points at instead, so a
        // refusal an operator reads names a ref they can type.
        let name = git_stdout(dir, &["rev-parse", "--abbrev-ref", candidate])
            .map(|n| n.trim().to_string())
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| (*candidate).to_string());
        return Some((name, sha));
    }
    None
}

/// The paths merging `HEAD` into `base` would still change (#7275, #7889).
///
/// Why: the one question that answers "would deleting this directory destroy
/// anything". `git cherry`'s per-commit patch ids report `+` for content that
/// IS on the base, because every merge here is a squash — observed on #7258 —
/// and ancestry is wrong for the same reason. #7889 moved the implementation
/// here from `worktree_removal_facts` so the guard's merged-PR residue check
/// and both ladders' landed-content admission run the same two commands.
/// What: `git merge-tree --write-tree <base> HEAD` writes the merged tree;
/// `git diff --name-only <base> <tree>` names what it changed. `Err` when
/// either command failed — which includes a merge CONFLICT, since `merge-tree`
/// exits non-zero on one — or when no tree was named.
/// Test: `a_divergence_landed_by_another_route_reports_landed`,
/// `an_unlanded_commit_reports_its_residual_path`,
/// `merge_residue_against_an_unresolvable_base_is_an_error`.
pub fn merge_residue(dir: &Path, base: &str) -> Result<Vec<String>, String> {
    let base = base.trim();
    if base.is_empty() {
        return Err("no base ref was supplied to judge this worktree's content against".into());
    }
    let tree = git_stdout(dir, &["merge-tree", "--write-tree", base, "HEAD"])
        .map_err(|e| format!("`git merge-tree --write-tree {base} HEAD` failed: {e}"))?;
    let Some(tree) = tree.lines().next().map(str::trim).filter(|t| !t.is_empty()) else {
        return Err(format!(
            "`git merge-tree --write-tree {base} HEAD` named no tree"
        ));
    };
    // `--name-only` rather than `--quiet`: an empty answer is the no-op, and a
    // non-empty one names the residue the refusal has to quote.
    let residue = git_stdout(dir, &["diff", "--name-only", base, tree])
        .map_err(|e| format!("`git diff --name-only {base} <merged tree>` failed: {e}"))?;
    Ok(residue
        .lines()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
#[path = "worktree_landed_content_tests.rs"]
mod worktree_landed_content_tests;
