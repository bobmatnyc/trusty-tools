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
//! #8633: the comparison is [`content_on_base`], which also accepts an
//! earlier commit on the base's own history when later commits there edited
//! the same files — the squash-merged shape a tip-only merge misread. Ancestry
//! is sufficient there, never necessary: a `HEAD` that is not an ancestor of
//! the base needs a base commit, the tip included, that holds its content in
//! both directions, so a revert made after the squash is never admitted.
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

use crate::core::worktree_carried_by_pr::{CarriedByPr, carried_by_merged_pr};
use crate::core::worktree_landed_history::{ContentOnBase, content_on_base};
use crate::session_manager::worktree_landing_refresh::refresh_landing_refs_within;
use crate::session_manager::worktree_reclaim_pr_match::LandingProbe;
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
        /// #8633: the commit on `base`, possibly its tip, that holds the
        /// content in both directions; `None` when `HEAD` is an ancestor.
        landed_at: Option<String>,
    },
    /// Merging `HEAD` into `base` would still change files.
    Residual {
        /// The ref the comparison was made against.
        base: String,
        /// The first path the merge would change, in git's own ordering.
        first_path: String,
    },
    /// Merging `HEAD` into `base` conflicts, and no earlier commit on `base`
    /// holds the content either (#8633); or the merge is empty but `HEAD`
    /// undid part of what landed (`ContentOnBase::Undone`, #8633 round 3).
    Conflicted {
        /// The ref the comparison was made against.
        base: String,
        /// The probe's own account: conflicted paths and commits searched.
        detail: String,
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
            Self::Landed {
                base,
                base_sha,
                landed_at: None,
            } => format!(
                "ADR-0057's `{LANDED_CONTENT_CHECK}` admission applies: HEAD is an ancestor of \
                 {base} (`{base_sha}`), so this tree holds nothing the remote does not already \
                 have"
            ),
            Self::Landed {
                base,
                landed_at: Some(at),
                ..
            } => format!(
                "ADR-0057's `{LANDED_CONTENT_CHECK}` admission applies: {}, so this tree holds \
                 nothing the remote does not already have",
                ContentOnBase::Landed {
                    at: Some(at.clone())
                }
                .describe(base)
            ),
            Self::Conflicted { detail, .. } => format!(
                "ADR-0057's `{LANDED_CONTENT_CHECK}` admission does not apply either: {detail}, \
                 so this tree may hold work that is on no remote"
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

/// Both content routes of the #7889 admission, as one answer.
///
/// Why: owner ruling 2026-09-22 admits a clean tree on (b) landed content OR
/// (c) HEAD inside a merged pull request's history. Both ladders ask both, in
/// that order, through [`landing_admission`], when they ask at all.
/// What: `content` is route (b); `carried` is route (c), `None` when it was not
/// asked — because (b) already admitted, or because a test fake stated only
/// (b). [`admits`](Self::admits) is true when either route admits.
/// Test: `admission_admits_on_either_route_and_names_both_refusals`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandingAdmission {
    /// Route (b): is the content already on the landing base?
    pub content: LandedContent,
    /// Route (c): is HEAD inside a merged pull request's history?
    pub carried: Option<CarriedByPr>,
}

impl From<LandedContent> for LandingAdmission {
    fn from(content: LandedContent) -> Self {
        Self {
            content,
            carried: None,
        }
    }
}

impl LandingAdmission {
    /// True when route (b) or route (c) admits.
    pub fn admits(&self) -> bool {
        self.content.is_landed() || self.carried.as_ref().is_some_and(CarriedByPr::is_carried)
    }

    /// The merged pull request route (c) admitted on, if that is the grant.
    pub fn carried_pr(&self) -> Option<u64> {
        match &self.carried {
            Some(c @ CarriedByPr::Carried { pr, .. }) if c.is_carried() => Some(*pr),
            _ => None,
        }
    }

    /// The sentence a refusal or a grant quotes (#7889).
    ///
    /// What: the admitting route's own sentence on a grant; on a refusal, both
    /// routes' sentences, so the refusal names each predicate that failed and,
    /// for (b), the first residual path.
    /// Test: `admission_admits_on_either_route_and_names_both_refusals`.
    pub fn note(&self) -> String {
        match &self.carried {
            _ if self.content.is_landed() => self.content.note(),
            Some(c) if c.is_carried() => c.note(),
            Some(c) => format!("{}; {}", self.content.note(), c.note()),
            None => self.content.note(),
        }
    }
}

/// Route (b), then route (c) when (b) did not admit (#7889).
///
/// Why: the one entry point both ladders' production probes call. (c) costs a
/// `gh` search, so it runs only when the local comparison did not admit.
/// What: [`landed_content_verdict`] under `refresh_timeout`; on anything but
/// `Landed`, [`carried_by_merged_pr`] through `probe`.
/// Test: `admission_admits_on_either_route_and_names_both_refusals`;
/// `worktree_carried_by_pr_tests` for route (c).
pub(crate) fn landing_admission(
    dir: &Path,
    refresh_timeout: Duration,
    probe: &dyn LandingProbe,
) -> LandingAdmission {
    with_carried_fallback(dir, landed_content_verdict(dir, refresh_timeout), probe)
}

/// [`landing_admission`] against refs the CALLER refreshed moments ago (#7889).
///
/// Why: the removal guard's #7914 `local-only-commits` probe has already run a
/// bounded `git fetch --prune origin` in this same evaluation, and a second
/// fetch costs up to 3 s of the `PreToolUse` hook's 5 s. Only a caller that
/// KNOWS its refresh succeeded may use this; any other caller fetches.
/// What: as [`landing_admission`], minus the refresh.
/// Test: `worktree_7889_the_admission_reuses_the_local_only_fetch_only_when_it_succeeded`
/// in `bin/tm/commands/pm_guard_bash/worktree_remove`.
pub(crate) fn landing_admission_on_fetched_refs(
    dir: &Path,
    probe: &dyn LandingProbe,
) -> LandingAdmission {
    with_carried_fallback(dir, landed_content_on_fetched_refs(dir), probe)
}

/// Route (c) asked only when route (b)'s `content` did not admit.
fn with_carried_fallback(
    dir: &Path,
    content: LandedContent,
    probe: &dyn LandingProbe,
) -> LandingAdmission {
    if content.is_landed() {
        return content.into();
    }
    LandingAdmission {
        content,
        carried: Some(carried_by_merged_pr(dir, probe)),
    }
}

/// Is every byte this worktree holds already on its landing base (#7889)?
///
/// Why: the predicate BOTH reclaim ladders ask, so one implementation answers
/// for both wherever each asks it. See the
/// module doc for the shape it admits and the ruling behind it.
/// What: in order — refresh `origin` within `refresh_timeout` (a failure or an
/// expiry is [`LandedContent::Unavailable`], never a comparison against the
/// stale refs it left); resolve the landing base from [`BASE_CANDIDATES`];
/// then [`content_on_base`]. Content on the base's tip or on an earlier commit
/// of its history is [`Landed`](LandedContent::Landed); a clean merge that
/// still changes files is [`Residual`](LandedContent::Residual) naming its
/// first path; a conflict is [`Conflicted`](LandedContent::Conflicted).
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
    landed_content_on_fetched_refs(dir)
}

/// Route (b) against the remote-tracking refs as they stand (#7889).
///
/// Why: split from [`landed_content_verdict`] so a caller that has just
/// refreshed `origin` itself does not pay for a second fetch.
/// What: resolve the landing base, then [`content_on_base`].
/// Test: `a_divergence_landed_by_another_route_reports_landed`,
/// `an_unlanded_commit_reports_its_residual_path`.
fn landed_content_on_fetched_refs(dir: &Path) -> LandedContent {
    let Some((base, base_sha)) = landing_base(dir) else {
        return LandedContent::unavailable(format!(
            "no landing base resolved in this worktree — none of {} names a commit",
            BASE_CANDIDATES.join(", ")
        ));
    };
    // #8633: a conflict is a verdict, not a failure; only a git error is
    // `Unavailable`.
    match content_on_base(dir, &base) {
        Err(e) => LandedContent::unavailable(e),
        Ok(ContentOnBase::Landed { at }) => LandedContent::Landed {
            base,
            base_sha,
            landed_at: at,
        },
        // #8633 round 3: `Undone` carries no merge residue to name, so it
        // reports through the probe's own description, as a conflict does.
        Ok(c @ (ContentOnBase::Conflicted { .. } | ContentOnBase::Undone { .. })) => {
            LandedContent::Conflicted {
                detail: c.describe(&base),
                base,
            }
        }
        Ok(ContentOnBase::Residual { paths, .. }) => LandedContent::Residual {
            base,
            first_path: paths.into_iter().next().unwrap_or_default(),
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

#[cfg(test)]
#[path = "worktree_landed_content_tests.rs"]
mod worktree_landed_content_tests;
