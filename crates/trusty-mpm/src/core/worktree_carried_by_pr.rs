//! Is this worktree's HEAD inside a MERGED pull request (#7889, route (c))?
//!
//! Why: owner ruling 2026-09-22 names three routes to landing evidence for a
//! clean tree — (a) its own branch's merged pull request, (b) landed content,
//! (c) "HEAD is an ancestor of the head commit of a merged PR". Route (c) is
//! the donor-branch shape itself: a parked branch fast-forwarded onto a
//! sibling's head, which then merged. It still admits where (b) cannot — a
//! donor whose content was later edited on `main` so the merge conflicts, or
//! whose change the pull request itself superseded.
//!
//! What: [`carried_by_merged_pr`] asks GitHub which MERGED pull requests
//! contain HEAD, then asks git whether HEAD is an ancestor of — or equal to —
//! one of their head commits.
//!
//! **One direction only.** HEAD ⊑ the pull request's head means every commit
//! here was inside what merged. The reverse — the pull request's head is an
//! ancestor of HEAD — leaves commits here the merge never saw, so it is never
//! a match. That is why this does not reuse `worktree_reclaim_pr_match`'s
//! either-way-round `vouches_for_head`, whose gate 6 catches the leftover.
//!
//! **Every failure arm refuses** (ADR-0045): an unreadable HEAD, a commit
//! search that did not answer, and an ancestry probe that errored — which is
//! what a head commit this checkout does not hold looks like — are all
//! [`CarriedByPr::Unavailable`]. A fork's pull request is never a match.
//! Test: `worktree_carried_by_pr_tests`.

use std::path::Path;

use crate::session_manager::worktree_reclaim_pr_match::{LandingProbe, MergedPrHead};

/// The admission's name, quoted by both ladders' refusals (#7889).
pub const MERGED_PR_ANCESTRY_CHECK: &str = "merged-pr-ancestry";

/// Whether a MERGED pull request carried this worktree's HEAD (#7889).
///
/// Why: "no pull request carries it" and "the question could not be asked"
/// lead to different refusal text, and must never collapse into each other on
/// a path whose grant deletes a directory.
/// What: [`Carried`](Self::Carried) is the only variant that admits.
/// Test: `a_head_inside_a_merged_prs_history_is_carried`,
/// `a_merged_pr_head_behind_head_is_not_a_match`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarriedByPr {
    /// HEAD is `pr_head` or one of its ancestors, and pull request `pr` merged.
    Carried {
        /// The merged pull request.
        pr: u64,
        /// That pull request's own head commit.
        pr_head: String,
    },
    /// GitHub answered, and no merged pull request's head descends from HEAD.
    NotCarried {
        /// The HEAD commit that was searched for.
        head: String,
    },
    /// The question could not be answered, which never admits.
    Unavailable {
        /// Why, in the words of whatever failed.
        detail: String,
    },
}

impl CarriedByPr {
    /// True only for a [`Carried`](Self::Carried) answer naming a head commit.
    pub fn is_carried(&self) -> bool {
        matches!(self, Self::Carried { pr_head, .. } if !pr_head.trim().is_empty())
    }

    /// One sentence naming what this admission decided (#7889).
    ///
    /// Why: the guard and the sweep append it to a refusal, and one rendering
    /// keeps the two ladders from describing one verdict differently.
    /// What: names the pull request on a grant, the HEAD commit when none
    /// carried it, and what failed when the question could not be asked.
    /// Test: `every_arm_names_the_admission`.
    pub fn note(&self) -> String {
        match self {
            Self::Carried { pr, pr_head } => format!(
                "ADR-0057's `{MERGED_PR_ANCESTRY_CHECK}` admission applies: HEAD is inside the \
                 history of PR #{pr}'s head `{pr_head}`, which merged"
            ),
            Self::NotCarried { head } => format!(
                "ADR-0057's `{MERGED_PR_ANCESTRY_CHECK}` admission does not apply either: no \
                 MERGED pull request's head commit descends from HEAD `{head}`"
            ),
            Self::Unavailable { detail } => format!(
                "ADR-0057's `{MERGED_PR_ANCESTRY_CHECK}` admission could not be established \
                 either — {detail} — and a fact that cannot be established never grants"
            ),
        }
    }
}

/// Is `dir`'s HEAD inside the history of a MERGED pull request's head (#7889)?
///
/// Why: route (c) of the 2026-09-22 ruling. See the module doc for the one
/// direction it accepts and why.
/// What: HEAD from `probe.head_commit`; the rows GitHub returns for it from
/// `probe.merged_prs_containing`; then [`carrying_row`]. `dir` is both the
/// worktree and the root the repository is resolved from, which is what the
/// removal guard and the sweep both have in hand.
/// Test: `a_head_inside_a_merged_prs_history_is_carried`,
/// `an_unreadable_head_is_unavailable`, `a_failed_search_is_unavailable`.
pub(crate) fn carried_by_merged_pr(dir: &Path, probe: &dyn LandingProbe) -> CarriedByPr {
    let head = match probe.head_commit(dir) {
        Ok(h) if !h.trim().is_empty() => h.trim().to_string(),
        Ok(_) => return unavailable("`git rev-parse HEAD` named no commit"),
        Err(e) => return unavailable(format!("HEAD could not be resolved: {e}")),
    };
    match probe.merged_prs_containing(dir, &head) {
        Ok(rows) => carrying_row(&rows, &head, &|a, d| probe.is_ancestor(dir, a, d)),
        Err(e) => unavailable(format!(
            "the MERGED pull request search for `{head}` did not answer: {e}"
        )),
    }
}

/// The first non-fork row whose head commit is `head` or descends from it.
///
/// Why: the decision, split from the subprocesses so every arm is unit-tested.
/// What: equal ids match without a subprocess. Otherwise `is_ancestor(head,
/// oid)` decides; a row whose probe errors is skipped, and when no row matched
/// the first such error is the answer — an ancestry question that could not be
/// asked is never "no pull request carries it".
/// Test: `a_head_inside_a_merged_prs_history_is_carried`,
/// `a_merged_pr_head_behind_head_is_not_a_match`,
/// `a_fork_row_is_never_a_match`, `an_ancestry_probe_error_is_unavailable`.
fn carrying_row(
    rows: &[MergedPrHead],
    head: &str,
    is_ancestor: &dyn Fn(&str, &str) -> Result<bool, String>,
) -> CarriedByPr {
    let mut first_error = None;
    for row in rows {
        let oid = row.head_ref_oid.trim();
        if row.is_cross_repository || oid.is_empty() {
            continue;
        }
        let carried = oid.eq_ignore_ascii_case(head) || {
            match is_ancestor(head, oid) {
                Ok(yes) => yes,
                Err(e) => {
                    first_error.get_or_insert(format!(
                        "whether HEAD is an ancestor of PR #{}'s head `{oid}` could not be \
                         established: {e}",
                        row.number
                    ));
                    false
                }
            }
        };
        if carried {
            return CarriedByPr::Carried {
                pr: row.number,
                pr_head: oid.to_string(),
            };
        }
    }
    match first_error {
        Some(detail) => unavailable(detail),
        None => CarriedByPr::NotCarried {
            head: head.to_string(),
        },
    }
}

fn unavailable(detail: impl Into<String>) -> CarriedByPr {
    CarriedByPr::Unavailable {
        detail: detail.into(),
    }
}

#[cfg(test)]
#[path = "worktree_carried_by_pr_tests.rs"]
mod worktree_carried_by_pr_tests;
