//! `tm pr merge` — squash-merge with the validated PR body as the landing
//! commit message (#6808).
//!
//! Why: `tm pr open` validates the seven-field body and the exact attribution
//! footer, but the documented merge path `gh pr merge --squash --delete-branch
//! --auto` lets GitHub assemble the squash commit from the branch's raw commit
//! messages, so the body that was validated never becomes the landing commit.
//! The squash for PR #6607 landed as a concatenation of five raw messages,
//! harness trailers included. Passing the body through `--body-file` closes
//! that gap, and the same read that supplies the body also answers the four
//! hold signals, so one command replaces a read-then-judge-then-merge sequence.
//!
//! What: [`run`] reads the PR once
//! (`gh pr view <n> --json number,title,body,isDraft,labels,reviewDecision,mergeStateStatus,headRefName`),
//! re-validates the body with [`body::validate`] — the same check `tm pr open`
//! runs — and either refuses with one line and no `gh pr merge` call, or merges
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
    /// `CLEAN`, `BEHIND`, `BLOCKED`, `CONFLICTING`, … or absent.
    #[serde(default, rename = "mergeStateStatus")]
    pub(crate) merge_state_status: Option<String>,
    /// The head branch, named in the merge output.
    #[serde(default, rename = "headRefName")]
    pub(crate) head_ref_name: String,
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
/// What: refuses, in this order, on a failed body validation, a draft, a
/// `do-not-merge` label in any case, a `CHANGES_REQUESTED` review decision, and
/// a `CONFLICTING` merge state — the last naming `gh pr update-branch`, which
/// is the fix for a genuine conflict. `BEHIND` is deliberately NOT a refusal:
/// this repo's rule is that a behind branch merges fine, and updating it for
/// BEHIND alone restarts CI and can fail to converge (root `CLAUDE.md`, "What
/// CI actually gates").
///
/// Test: `merge_valid_body_merges`, `merge_refuses_missing_footer`,
/// `merge_refuses_draft`, `merge_refuses_do_not_merge_label_any_case`,
/// `merge_refuses_changes_requested`, `merge_behind_is_not_a_refusal`,
/// `merge_refuses_conflicting_with_update_branch_hint`.
pub(crate) fn decide(view: &MergeView, body_failures: &[String]) -> Decision {
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
    // #6808: BEHIND merges fine here, so only CONFLICTING stops the merge.
    if view
        .merge_state_status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("CONFLICTING"))
    {
        return Decision::Refuse(format!(
            "mergeStateStatus is CONFLICTING — resolve it with `gh pr update-branch {}`",
            view.number
        ));
    }
    Decision::Merge
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
    a.push(
        "number,title,body,isDraft,labels,reviewDecision,mergeStateStatus,headRefName".to_string(),
    );
    let stdout = gh.run(&a)?.stdout_ok(&a)?;
    serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("cannot parse `gh pr view {}` JSON: {e}", args.pr))
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
/// Test: `merge_refuses_without_calling_gh_merge`,
/// `merge_argv_carries_squash_delete_and_body_file`.
pub(crate) fn run<R: GhRunner>(gh: &R, args: &PrMergeArgs) -> anyhow::Result<i32> {
    let view = pr_view(gh, args)?;
    let failures = body::validate(&view.body).failures();

    match decide(&view, &failures) {
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
                anyhow::bail!("`gh pr merge {}` failed: {}", args.pr, out.stderr.trim());
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
