//! What proves a branch's work already landed on the base (#7275).
//!
//! Why: a squash merge replaces the branch's commits with one new commit on the
//! base, so nothing the branch holds is ever an ancestor of `main`. Every
//! ahead-of-upstream reading therefore says "N commits unpushed" about a branch
//! whose content is fully merged, and cleanup refused seven such worktrees on
//! 2026-09-09 — the exact population it exists to reclaim. Landing has to be
//! judged by evidence a squash preserves.
//!
//! What: two independent facts, and a tree needs BOTH.
//!
//! | fact | what answers it | fails toward |
//! |---|---|---|
//! | a MERGED pull request carried this tree | [`Landing::merged_pr`] over #7267's `resolve_landing` — exact branch name, round stem, or head-commit ancestry | refuse |
//! | landing it again would change nothing | [`unlanded`] — `git merge-tree --write-tree origin/<base> <tip>` diffed against the base | refuse |
//!
//! Neither fact alone is enough. A merged pull request says the branch's NAME
//! landed, not that this checkout still holds only what landed — a tree whose
//! branch moved on after the merge matches by name and holds real work. The
//! merge-tree no-op says the content adds nothing, which for an unmerged branch
//! can also be true of an empty one. Requiring both is what keeps [`landed`]
//! from becoming the ahead-count it replaces.
//!
//! The merge-tree half reads `origin/<base>` as the LOCAL checkout holds it,
//! which the run itself only refreshes in step 5 — see [`Merge`] for why that
//! ordering refused a landed tree for a whole sweep interval (#7275 round 4).
//!
//! Test: the sibling `tests.rs` — `cleanup_removes_a_squash_merged_worktree`,
//! `cleanup_refuses_a_squash_merged_tree_that_moved_on`,
//! `cleanup_refuses_an_ahead_tree_with_no_merged_pr`,
//! `cleanup_refuses_when_the_merge_test_cannot_run`,
//! `cleanup_reclaims_a_tree_whose_base_ref_was_stale`,
//! `cleanup_refuses_when_the_base_ref_cannot_be_refreshed`.

use std::sync::OnceLock;

use super::driver::{Git, Landing};
use super::plan::{PrView, WorktreeEntry};
use super::{CleanupRequest, FALLBACK_BASE, owned};

/// The merge every landing test is measured against (#7275 round 4).
///
/// Why: [`unlanded`] asks whether landing a branch again would change anything
/// by comparing against `origin/<base>` AS THIS CHECKOUT LAST SAW IT, and the
/// only fetch in the run is step 5's `git fetch --prune origin` — which runs
/// AFTER the worktree step. So the first cleanup following a merge compares
/// against a base ref that predates that merge, finds the branch's content
/// missing from it, and refuses a tree that genuinely landed; only the NEXT
/// sweep, a fetch later, sees it. Refreshing the single ref the comparison
/// reads closes that window.
/// What: [`resolve`](Self::resolve) runs `git fetch origin <base>` ONCE per
/// cleanup run — the [`OnceLock`] is what keeps a sweep over five worktrees to
/// one round trip — and hands back the ref name to compare against.
/// FAIL-CLOSED: a fetch that errors or exits non-zero yields `Err`, and every
/// caller turns that into a refusal rather than comparing against the stale ref
/// (ADR-0045: an unanswerable question never authorises a delete).
/// Test: `cleanup_reclaims_a_tree_whose_base_ref_was_stale`,
/// `cleanup_refuses_when_the_base_ref_cannot_be_refreshed`.
pub(super) struct Merge {
    /// The base branch the pull request merged into.
    branch: String,
    /// `origin/<branch>` — what every merge test names.
    reference: String,
    /// The head commit GitHub reports the pull request merged from.
    head: String,
    /// The one refresh attempt, taken on first use and reused after.
    fetched: OnceLock<Result<(), String>>,
}

impl Merge {
    /// Read the base branch and merged head out of step 1's own `gh` payload.
    pub(super) fn new(view: &PrView) -> Self {
        let branch = match view.base_ref_name.trim() {
            "" => FALLBACK_BASE,
            b => b,
        };
        Self {
            branch: branch.to_string(),
            reference: format!("origin/{branch}"),
            head: view.head_ref_oid.trim().to_string(),
            fetched: OnceLock::new(),
        }
    }

    /// `origin/<base>`, refreshed once, or why it could not be.
    pub(super) fn resolve<T: Git>(&self, git: &T, req: &CleanupRequest) -> Result<&str, String> {
        // #7275: a READ of the remote, so it runs under `--dry-run` too —
        // otherwise the dry run would report a verdict the real run reverses.
        let outcome = self.fetched.get_or_init(|| {
            match git.run(&req.repo_root, &owned(&["fetch", "origin", &self.branch])) {
                Ok(out) if out.success => Ok(()),
                Ok(out) => Err(format!(
                    "`git fetch origin {}` failed: {}",
                    self.branch,
                    out.stderr.trim()
                )),
                Err(e) => Err(format!(
                    "`git fetch origin {}` could not be run: {e:#}",
                    self.branch
                )),
            }
        });
        match outcome {
            Ok(()) => Ok(&self.reference),
            Err(why) => Err(why.clone()),
        }
    }
}

/// Why `tip` is NOT safely deletable as this PR's sibling, or `None` when it is.
///
/// Why (#7275, owner correction 2026-09-09): every merge here is a squash, so
/// `git cherry`'s per-commit patch-id comparison reports `+` for content that
/// IS on the base — it said so for #7258. Merging the tip into the base and
/// asking whether the result differs from the base answers the real question:
/// would landing this branch change anything? An empty diff means no, which is
/// the ownership proof a sibling needs and also what makes a stacked branch
/// whose base already merged a no-op.
/// What: accepts immediately when `tip` is an ancestor of the merged head —
/// a round-1 branch superseded by `-r2` usually is, and so is a tree sitting on
/// the merged head itself — and otherwise runs
/// `git merge-tree --write-tree origin/<base> <tip>` for the merged tree, then
/// `git diff --name-only origin/<base> <tree>`. An empty answer is the no-op; a
/// non-empty one NAMES the residue files, so a person can look at what the
/// merge did not carry instead of being told only that something remains.
/// FAIL-SAFE: a conflict, an unreadable tree, a base ref that cannot be
/// refreshed, and any error all REFUSE.
/// Test: `cleanup_removes_a_round_sibling_whose_content_landed`,
/// `cleanup_refuses_a_sibling_whose_content_is_not_on_the_base`,
/// `cleanup_removes_a_round_one_branch_that_never_had_its_own_pr`,
/// `cleanup_reclaims_a_tree_whose_base_ref_was_stale`.
pub(super) fn unlanded<T: Git>(
    git: &T,
    req: &CleanupRequest,
    merge: &Merge,
    tip: &str,
) -> Option<String> {
    let head_oid = merge.head.as_str();
    if !head_oid.is_empty()
        && let Ok(out) = git.run(
            &req.repo_root,
            &owned(&["merge-base", "--is-ancestor", tip, head_oid]),
        )
        && out.success
    {
        return None;
    }
    // #7275 round 4: the comparison below reads `origin/<base>` from the local
    // ref store, and the run's own fetch is step 5 — so refresh it here, before
    // the first comparison that depends on it. A fetch that fails refuses.
    let base_ref = match merge.resolve(git, req) {
        Ok(r) => r.to_string(),
        Err(why) => {
            return Some(format!(
                "cannot test whether the merge carried `{tip}`: {why} — refusing rather than \
                 comparing against a base ref that may predate the merge (#7275)"
            ));
        }
    };
    let merged = match git.run(
        &req.repo_root,
        &owned(&["merge-tree", "--write-tree", &base_ref, tip]),
    ) {
        Ok(out) if out.success => out.stdout,
        Ok(out) => {
            return Some(format!(
                "`{tip}` does not merge cleanly into {base_ref}, so the merge cannot have \
                 carried it: {}",
                out.stderr.trim()
            ));
        }
        Err(e) => {
            return Some(format!(
                "cannot merge-test `{tip}` against {base_ref}: {e:#}"
            ));
        }
    };
    let Some(tree) = merged
        .lines()
        .next()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        return Some(format!(
            "`git merge-tree --write-tree {base_ref} {tip}` named no tree"
        ));
    };
    match git.run(
        &req.repo_root,
        &owned(&["diff", "--name-only", &base_ref, tree]),
    ) {
        Ok(out) if out.success && out.stdout.trim().is_empty() => None,
        Ok(out) if out.success => Some(format!(
            "`{tip}` is this PR's round sibling, but merging it into {base_ref} would still \
             change {}: {} — it holds work the merge did not carry, so cleanup leaves it alone \
             (#7275)",
            file_count(&out.stdout),
            out.stdout.split_whitespace().collect::<Vec<_>>().join(", ")
        )),
        Ok(out) => Some(format!(
            "cannot compare the merge of `{tip}` against {base_ref}: {}",
            out.stderr.trim()
        )),
        Err(e) => Some(format!(
            "cannot compare the merge of `{tip}` against {base_ref}: {e:#}"
        )),
    }
}

/// `"1 file"` / `"3 files"` for a newline-separated listing.
pub(super) fn file_count(listing: &str) -> String {
    let n = listing.split_whitespace().count();
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// Did the merge already carry everything this worktree holds (#7275 round 3)?
///
/// Why: this replaces the ahead-of-upstream count as cleanup's answer to "is
/// this branch landed". The count cannot answer it — a squash merge guarantees
/// a nonzero count for landed work — so the tree that most needs reclaiming is
/// the one it refuses hardest. The module doc has the two facts and why one
/// alone would be fail-open.
/// What: `Ok(pr)` naming the merged pull request when a merged pull request
/// matches this tree AND [`unlanded`] finds the merge a no-op; `Err(reason)`
/// otherwise, with the reason naming which fact failed. A tree sitting ON the
/// merged head commit seeds the matcher's first rung from step 1's own
/// `gh pr view`, so the common path costs no extra `gh` call; every other tree
/// pays the full three-rung lookup.
/// FAIL-CLOSED: a `gh` that cannot answer, a `git merge-tree` that cannot run,
/// and a tree naming neither a branch nor a HEAD all return `Err`.
/// Test: `cleanup_removes_a_squash_merged_worktree`,
/// `cleanup_refuses_a_squash_merged_tree_that_moved_on`,
/// `cleanup_refuses_an_ahead_tree_with_no_merged_pr`,
/// `cleanup_refuses_when_the_merge_test_cannot_run`.
pub(super) fn landed<T: Git>(
    git: &T,
    landing: &dyn Landing,
    req: &CleanupRequest,
    merge: &Merge,
    entry: &WorktreeEntry,
) -> Result<u64, String> {
    let branch = entry
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    let head = entry.head.trim();
    let oid = merge.head.as_str();
    // Fact 1. Only a tree sitting ON the merged head commit is settled without
    // a lookup — no matcher rung can improve on that identity. A branch that
    // merely SHARES this pull request's round stem is asked in full: the name
    // relation is how the tree became a candidate, so accepting it as its own
    // landing evidence would make this gate circular.
    let exact = (!oid.is_empty() && head.eq_ignore_ascii_case(oid)).then_some(req.pr);
    let Some(pr) = landing.merged_pr(&req.repo_root, &entry.path, branch, exact) else {
        return Err(format!(
            "no MERGED pull request matches {} by branch name, round stem or head commit, so \
             the commits it is ahead by are not provably landed (#7275)",
            branch.unwrap_or("its detached HEAD")
        ));
    };
    // Fact 2. A name match alone would let a tree whose branch moved on after
    // the merge be removed with the commits it gained.
    let Some(tip) = branch.or(if head.is_empty() { None } else { Some(head) }) else {
        return Err(
            "this worktree names neither a branch nor a HEAD commit, so whether the merge \
             carried its content cannot be tested (#7275)"
                .to_string(),
        );
    };
    match unlanded(git, req, merge, tip) {
        None => Ok(pr),
        Some(reason) => Err(reason),
    }
}

/// A step failure reading for a tree the ahead-count gate could not clear.
///
/// Why: an operator reading `2 unpushed commit(s)` needs to know cleanup did
/// not stop at the count — it asked whether a merge carried them and got an
/// answer — otherwise the fix looks like "push the branch" when it is not.
/// Test: `cleanup_refuses_an_ahead_tree_with_no_merged_pr`.
pub(super) fn ahead_refusal(shown: &str, dirt_reason: &str, why: &str) -> String {
    format!(
        "{shown} is ahead of its upstream ({dirt_reason}) and the merge does not account for \
         it — refusing to remove it: {why}"
    )
}

/// Does this dirt reading say only "ahead of the upstream", and nothing else?
///
/// Why: an uncommitted or untracked file is unsaved work no merge can vouch
/// for, and a reading with BOTH counts zero is `inspect_dirt`'s error arm —
/// which fails toward dirty on purpose. Only the committed-but-not-on-a-remote
/// case is the squash-merge artefact this gate exists for.
/// Test: `cleanup_refuses_a_dirty_worktree` (files present),
/// `cleanup_refuses_an_unreadable_worktree` (both counts zero).
pub(super) fn ahead_only(dirt: &crate::session_manager::DirtyWorktree) -> bool {
    dirt.dirty_files == 0 && dirt.unpushed_commits > 0
}
