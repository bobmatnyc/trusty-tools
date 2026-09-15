//! `tm pr merge` — squash-merge with the validated PR body as the landing
//! commit message (#6808).
//!
//! Why: `tm pr open` validates the nine-field body and the exact attribution
//! footer, but the documented merge path `gh pr merge --squash --delete-branch
//! --auto` lets GitHub assemble the squash commit from the branch's raw commit
//! messages, so the body that was validated never becomes the landing commit.
//! The squash for PR #6607 landed as a concatenation of five raw messages,
//! harness trailers included. Passing the body through `--body-file` closes
//! that gap, and the same read that supplies the body also answers the four
//! hold signals, so one command replaces a read-then-judge-then-merge sequence.
//!
//! What: [`run`] reads the PR once
//! (`gh pr view <n> --json
//! number,title,body,isDraft,labels,reviewDecision,mergeStateStatus,mergeable,headRefName`),
//! re-validates the body with [`body::validate`] — reporting the nine-field
//! gaps and refusing only on a missing attribution footer, which is the half
//! that belongs to the commit message this command writes (#7868) — and either
//! refuses with one line and no `gh pr merge` call, or merges
//! with `--squash --delete-branch --subject "<title> (#<n>)" --body-file <tmp>`
//! where the temp file holds that validated body. The refusal itself is
//! [`decide`], a pure function of the viewed fields plus the validation result.
//!
//! Test: the sibling `tests.rs` — `merge_*`.

use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize;

use super::body;
use super::{EXIT_BLOCKED, EXIT_OK, GhRunner, argv};
use crate::cli::PrMergeArgs;

/// The label that holds a PR out of a merge, lowercase.
const HOLD_LABEL: &str = "do-not-merge";

/// A GitHub label, as every `--json labels` payload shapes it.
#[derive(Debug, Deserialize)]
pub(crate) struct Label {
    /// The label's display name.
    pub(crate) name: String,
}

/// The PR fields the merge decision is made from.
///
/// Why: every field here is either an input to the squash commit message
/// (`title`, `body`) or one of the documented hold signals, so a single
/// `gh pr view` answers the whole decision — no second round trip can observe
/// a different PR state than the one that was judged.
/// What: the `gh pr view --json` payload, every field defaulted so a payload
/// missing one (an unreviewed PR reports `reviewDecision: null`) still parses.
/// Test: `merge_valid_body_merges`, `merge_refuses_draft`.
#[derive(Debug, Deserialize)]
pub(crate) struct MergeView {
    /// The PR number GitHub reports, echoed back in the merge output.
    #[serde(default)]
    pub(crate) number: u64,
    /// PR title — the first line of the squash commit subject.
    #[serde(default)]
    pub(crate) title: String,
    /// PR body — becomes the squash commit message verbatim.
    #[serde(default)]
    pub(crate) body: String,
    /// Whether the PR is still a draft.
    #[serde(default, rename = "isDraft")]
    pub(crate) is_draft: bool,
    /// Every label on the PR.
    #[serde(default)]
    pub(crate) labels: Vec<Label>,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or absent.
    #[serde(default, rename = "reviewDecision")]
    pub(crate) review_decision: Option<String>,
    /// `MergeStateStatus`: `DIRTY`, `UNKNOWN`, `BLOCKED`, `BEHIND`,
    /// `UNSTABLE`, `HAS_HOOKS`, `CLEAN`, or absent. `DIRTY` is the conflict.
    #[serde(default, rename = "mergeStateStatus")]
    pub(crate) merge_state_status: Option<String>,
    /// `MergeableState`: `MERGEABLE`, `CONFLICTING`, `UNKNOWN`, or absent.
    ///
    /// Why (#6808): `CONFLICTING` lives HERE, never in `mergeStateStatus` —
    /// the two enums are disjoint, and reading a conflict off the wrong one
    /// let every conflicted PR through to the raw `gh` error.
    #[serde(default)]
    pub(crate) mergeable: Option<String>,
    /// The head branch, named in the merge output.
    #[serde(default, rename = "headRefName")]
    pub(crate) head_ref_name: String,
    /// `OPEN`, `CLOSED` or `MERGED` at the START of this invocation (#7945).
    ///
    /// Why: the post-failure re-read cannot tell "this command's merge landed"
    /// from "someone merged it an hour ago" unless the opening read already
    /// established that the PR was OPEN. Defaulted, so a payload without the
    /// field (every pre-#7945 fixture) still parses and refuses nothing.
    #[serde(default)]
    pub(crate) state: String,
}

/// What [`decide`] concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Nothing holds the PR back; merge it.
    Merge,
    /// Do not call `gh pr merge`; this is the one-line reason.
    Refuse(String),
}

/// Decide whether this PR may be squash-merged.
///
/// Why: keeping the decision a pure function of (viewed fields, validation
/// result) is what makes both the refusals and the ORDER testable without a
/// live `gh`. A PR that is simultaneously draft and mislabelled must report one
/// stable reason, and only a test can pin which.
///
/// What: refuses, in this order, on `body_failures`, a draft, a
/// `do-not-merge` label in any case, a `CHANGES_REQUESTED` review decision, and
/// a conflict — the last naming `gh pr update-branch`, which is the fix.
///
/// `body_failures` is [`body::BodyReport::merge_failures`] — the attribution
/// footer alone (#7868). The footer IS part of the landing commit message this
/// command writes, so a body missing it would put an unattributed commit on
/// `main`; the nine-field contract is not, and is reported by [`run`] instead.
///
/// A conflict is `mergeable == CONFLICTING` or `mergeStateStatus == DIRTY`;
/// the two are separate GraphQL enums and `CONFLICTING` never appears in
/// `mergeStateStatus`, so reading only the latter let every conflicted PR
/// through (#6808). `BEHIND` is deliberately NOT a refusal: this repo's rule is
/// that a behind branch merges fine, and updating it for BEHIND alone restarts
/// CI and can fail to converge (root `CLAUDE.md`, "What CI actually gates").
/// Every OTHER `mergeStateStatus` — `BLOCKED`, `UNSTABLE`, `HAS_HOOKS`,
/// `UNKNOWN` — and every `reviewDecision` other than `CHANGES_REQUESTED`
/// (`REVIEW_REQUIRED` and absent included) are left to `gh pr merge` to accept
/// or reject, which is what makes `--auto` on a still-checking PR the intended
/// path rather than a refusal.
///
/// Test: `pr_7945_a_pr_already_merged_at_start_is_refused`,
/// `merge_valid_body_merges`, `merge_refuses_missing_footer`,
/// `merge_refuses_draft`, `merge_refuses_do_not_merge_label_any_case`,
/// `merge_refuses_changes_requested`, `merge_behind_is_not_a_refusal`,
/// `merge_refuses_conflicting_with_update_branch_hint`,
/// `merge_refuses_dirty_merge_state_with_update_branch_hint`,
/// `merge_other_merge_states_fall_through_to_gh`,
/// `pr_7868_a_sparse_body_is_not_a_merge_refusal`.
pub(crate) fn decide(view: &MergeView, body_failures: &[String]) -> Decision {
    // #7945: first, because a PR that is not OPEN cannot be merged by this
    // invocation at all — and because the post-failure re-read below treats
    // "MERGED" as evidence that THIS run's merge landed, which is only sound
    // when the run started against an open PR.
    if !view.state.is_empty() && !view.state.eq_ignore_ascii_case("OPEN") {
        return Decision::Refuse(format!(
            "the PR is already {} — nothing to merge",
            view.state.trim().to_uppercase()
        ));
    }
    if !body_failures.is_empty() {
        return Decision::Refuse(format!(
            "PR body fails the same check `tm pr open` runs: {}",
            body_failures.join("; ")
        ));
    }
    if view.is_draft {
        return Decision::Refuse("the PR is a draft".to_string());
    }
    if let Some(l) = view
        .labels
        .iter()
        .find(|l| l.name.trim().eq_ignore_ascii_case(HOLD_LABEL))
    {
        return Decision::Refuse(format!("the PR carries the `{}` label", l.name.trim()));
    }
    if view
        .review_decision
        .as_deref()
        .is_some_and(|d| d.eq_ignore_ascii_case("CHANGES_REQUESTED"))
    {
        return Decision::Refuse("the review decision is CHANGES_REQUESTED".to_string());
    }
    // #6808: BEHIND merges fine here; only a real conflict stops the merge.
    if let Some(field) = conflict_field(view) {
        return Decision::Refuse(format!(
            "the PR has merge conflicts ({field}) — resolve them with `gh pr update-branch {}`",
            view.number
        ));
    }
    Decision::Merge
}

/// Which field reports this PR as conflicted, if either does.
///
/// Why (#6808): GitHub splits the answer across two disjoint enums —
/// `MergeableState` carries `CONFLICTING`, `MergeStateStatus` carries `DIRTY`.
/// Reading both means neither spelling of the same conflict slips through.
/// What: the human-readable field name, or `None` when neither reports one.
/// Test: `merge_refuses_conflicting_with_update_branch_hint`,
/// `merge_refuses_dirty_merge_state_with_update_branch_hint`.
fn conflict_field(view: &MergeView) -> Option<&'static str> {
    if view
        .mergeable
        .as_deref()
        .is_some_and(|m| m.eq_ignore_ascii_case("CONFLICTING"))
    {
        return Some("mergeable CONFLICTING");
    }
    if view
        .merge_state_status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("DIRTY"))
    {
        return Some("mergeStateStatus DIRTY");
    }
    None
}

/// The `gh pr merge` argv for an approved merge.
///
/// Why: `--subject` and `--body-file` together are the whole point of this
/// command — without them GitHub concatenates the branch's raw commit messages
/// into the squash commit.
/// What: `pr merge <n> [--repo <slug>] --squash [--delete-branch] [--auto]
/// --subject "<title> (#<n>)" --body-file <path>`.
/// Test: `merge_argv_carries_squash_delete_and_body_file`,
/// `merge_argv_honours_auto_and_no_delete_branch`.
pub(crate) fn plan(args: &PrMergeArgs, view: &MergeView, body_file: &Path) -> Vec<String> {
    let n = args.pr.to_string();
    let mut a = argv(&["pr", "merge", &n]);
    push_repo(&mut a, args);
    a.push("--squash".to_string());
    if !args.no_delete_branch {
        a.push("--delete-branch".to_string());
    }
    if args.auto {
        a.push("--auto".to_string());
    }
    a.push("--subject".to_string());
    a.push(format!("{} (#{})", view.title.trim(), args.pr));
    a.push("--body-file".to_string());
    a.push(body_file.display().to_string());
    a
}

/// Append `--repo <slug>` when one was passed explicitly.
fn push_repo(a: &mut Vec<String>, args: &PrMergeArgs) {
    if let Some(repo) = args
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        a.push("--repo".to_string());
        a.push(repo.to_string());
    }
}

/// Read the PR fields the decision and the commit message need.
fn pr_view<R: GhRunner>(gh: &R, args: &PrMergeArgs) -> anyhow::Result<MergeView> {
    let n = args.pr.to_string();
    let mut a = argv(&["pr", "view", &n]);
    push_repo(&mut a, args);
    a.push("--json".to_string());
    // #7945: `state` joins the set so `decide` can refuse a PR that is not OPEN.
    a.push(
        "number,title,body,isDraft,labels,reviewDecision,mergeStateStatus,mergeable,headRefName,\
         state"
            .to_string(),
    );
    let stdout = gh.run(&a)?.stdout_ok(&a)?;
    serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("cannot parse `gh pr view {}` JSON: {e}", args.pr))
}

/// The merge state a post-failure confirmation read (#7945).
///
/// Why: the only question that matters after `gh pr merge` exits non-zero is
/// whether GitHub merged the pull request anyway. `state` answers it; the
/// merge commit is what makes the answer checkable in the output.
/// What: the `state,mergeCommit` half of a `gh pr view --json` payload.
/// Test: `pr_7945_a_worktree_held_branch_does_not_fail_a_landed_merge`.
#[derive(Debug, Deserialize)]
struct MergedState {
    /// `OPEN`, `CLOSED` or `MERGED`.
    #[serde(default)]
    state: String,
    /// The squash commit, when GitHub reports one.
    #[serde(default, rename = "mergeCommit")]
    merge_commit: Option<MergeCommit>,
}

/// The squash commit in a `mergeCommit` payload.
#[derive(Debug, Deserialize)]
struct MergeCommit {
    /// The commit sha.
    #[serde(default)]
    oid: String,
}

impl MergedState {
    /// ` as <sha>`, or the empty string when GitHub named no merge commit.
    fn commit_suffix(&self) -> String {
        match self.merge_commit.as_ref().map(|c| c.oid.trim()) {
            Some(oid) if !oid.is_empty() => format!(" as {oid}"),
            _ => String::new(),
        }
    }
}

/// Did the squash-merge land even though `gh pr merge` exited non-zero? (#7945)
///
/// Why: `--delete-branch` deletes the local branch AFTER the API merge, so a
/// branch a worktree holds makes `gh` fail with the merge already on `main` —
/// reported twice (PR #7943, PR #8007) as `tm pr merge` exiting 2 on a merge
/// that had landed. Only a fresh read of the PR can tell that case from a merge
/// that never happened.
/// What: `Some(state)` only when the re-read succeeds, parses, and reports
/// `MERGED`. Every other outcome — a failed read, unparseable JSON, any other
/// state — is `None`, so an unanswerable question keeps the original failure
/// rather than upgrading it to success.
/// Test: `pr_7945_a_worktree_held_branch_does_not_fail_a_landed_merge`,
/// `pr_7945_a_merge_that_did_not_land_still_fails`,
/// `pr_7945_an_unreadable_confirmation_still_fails`.
/// Is this `gh pr merge` failure the local branch-delete step failing? (#7945)
///
/// Why: "the PR reads MERGED" is not enough on its own — a PR someone else
/// merged an hour ago reads MERGED too, and so does one whose merge landed
/// before a failure that has nothing to do with cleanup. The downgrade to exit
/// 0 is only defensible for the ONE failure that happens strictly after the API
/// merge: deleting the local branch.
/// What: matches the two spellings `gh` and `git` produce, case-insensitively.
/// Test: `pr_7945_a_non_cleanup_failure_on_a_merged_pr_still_fails`,
/// `pr_7945_a_worktree_held_branch_does_not_fail_a_landed_merge`.
fn is_branch_delete_failure(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("cannot delete branch") || s.contains("failed to delete local branch")
}

/// The report a landed merge with deferred cleanup prints (#7945).
///
/// Why: the issue's second closure condition is that the output DISTINGUISHES
/// "merge succeeded" from "local branch cleanup deferred". A `String` is what
/// lets a test hold that text to it; printed straight to stderr it was
/// unpinnable.
/// What: two lines — what landed, and what did not, naming the `gh` stderr
/// (which carries the worktree path) and the command that finishes the job.
/// Test: `pr_7945_the_report_separates_the_landed_merge_from_the_deferred_cleanup`.
pub(crate) fn cleanup_deferred_report(
    pr: u64,
    head: &str,
    commit_suffix: &str,
    stderr: &str,
) -> String {
    format!(
        "MERGE LANDED: #{pr} ({head}) was squash-merged{commit_suffix}.\n\
         CLEANUP DEFERRED: the local branch was not deleted — {}. Run `tm pr cleanup {pr}` to \
         remove the worktree holding `{head}` and then the branch.",
        stderr.trim()
    )
}

fn merged_after_failure<R: GhRunner>(gh: &R, args: &PrMergeArgs) -> Option<MergedState> {
    let n = args.pr.to_string();
    let mut a = argv(&["pr", "view", &n]);
    push_repo(&mut a, args);
    a.push("--json".to_string());
    a.push("state,mergeCommit".to_string());
    let run = gh.run(&a).ok()?;
    if !run.success {
        return None;
    }
    let state: MergedState = serde_json::from_str(&run.stdout).ok()?;
    (state.state.eq_ignore_ascii_case("MERGED")).then_some(state)
}

/// Run `tm pr merge`.
///
/// Why: the validated body only reaches `main` if the same process that
/// validates it also performs the merge — a human or agent re-typing
/// `gh pr merge --squash` drops it silently, which is exactly what happened to
/// PR #6607.
/// What: view the PR, validate its body, [`decide`], then either print the
/// refusal on stderr and exit [`EXIT_BLOCKED`] without calling `gh pr merge`,
/// or write the body to a temp file and merge from it. Under `--auto` the
/// merge is queued and GitHub applies the supplied subject and body when
/// auto-merge fires.
///
/// #7945: a non-zero `gh pr merge` exits [`EXIT_OK`] only when a branch delete
/// was requested, the failure is [`is_branch_delete_failure`]-shaped, and
/// [`merged_after_failure`] confirms the PR — OPEN when [`decide`] judged it —
/// now reads MERGED. That exit code is what gates the post-merge cleanup
/// (`tm pr cleanup`), which is precisely what reclaims the worktree that
/// blocked the delete. Every other failure keeps the error.
/// Test: `merge_refuses_without_calling_gh_merge`,
/// `merge_argv_carries_squash_delete_and_body_file`,
/// `pr_7945_a_worktree_held_branch_does_not_fail_a_landed_merge`,
/// `pr_7945_a_merge_that_did_not_land_still_fails`,
/// `pr_7945_a_non_cleanup_failure_on_a_merged_pr_still_fails`,
/// `pr_7945_no_delete_branch_never_downgrades_a_failure`.
pub(crate) fn run<R: GhRunner>(gh: &R, args: &PrMergeArgs) -> anyhow::Result<i32> {
    let view = pr_view(gh, args)?;
    let report = body::validate(&view.body);

    // #7868: the nine-field contract is the OPEN gate. Re-running it here made
    // a body written to the sparse prose rules unmergeable by the one command
    // that passes the reviewed body through `--body-file`, so the operator fell
    // back to raw `gh pr merge` and lost that guarantee. The gaps are reported;
    // only the footer still refuses.
    let gaps = report.contract_gaps();
    if !gaps.is_empty() {
        eprintln!(
            "tm pr merge: #{}: the body does not fill every field of the nine-field contract — \
             reported, not a refusal (#7868):",
            args.pr
        );
        for gap in &gaps {
            eprintln!("  - {gap}");
        }
    }

    match decide(&view, &report.merge_failures()) {
        Decision::Refuse(reason) => {
            eprintln!(
                "tm pr merge: refusing to merge #{} — {reason}; `gh pr merge` was not called",
                args.pr
            );
            Ok(EXIT_BLOCKED)
        }
        Decision::Merge => {
            // #6808: the temp file must outlive the `gh` call, so bind it.
            let mut tmp = tempfile::NamedTempFile::new()
                .context("cannot create the temp file holding the squash commit body")?;
            tmp.write_all(view.body.as_bytes())
                .and_then(|()| tmp.flush())
                .context("cannot write the squash commit body to its temp file")?;

            let a = plan(args, &view, tmp.path());
            let out = gh.run(&a)?;
            if !out.success {
                // #7945: `gh pr merge --delete-branch` deletes the LOCAL branch
                // AFTER the API merge lands, and that delete fails when a
                // worktree holds it. Three conditions, all required: a delete
                // was asked for, the failure is that delete, and the PR — OPEN
                // when this run started — now reads MERGED.
                let cleanup_shaped =
                    !args.no_delete_branch && is_branch_delete_failure(&out.stderr);
                let landed = cleanup_shaped
                    .then(|| merged_after_failure(gh, args))
                    .flatten();
                let Some(landed) = landed else {
                    anyhow::bail!("`gh pr merge {}` failed: {}", args.pr, out.stderr.trim());
                };
                let report = cleanup_deferred_report(
                    args.pr,
                    &view.head_ref_name,
                    &landed.commit_suffix(),
                    &out.stderr,
                );
                eprintln!("tm pr merge: warning — {report}");
                println!("squash-merged #{} ({})", args.pr, view.head_ref_name);
                return Ok(EXIT_OK);
            }
            if args.auto {
                println!(
                    "auto-merge armed on #{} ({}) — GitHub applies the supplied subject and body when it fires",
                    args.pr, view.head_ref_name
                );
            } else {
                println!("squash-merged #{} ({})", args.pr, view.head_ref_name);
            }
            Ok(EXIT_OK)
        }
    }
}
