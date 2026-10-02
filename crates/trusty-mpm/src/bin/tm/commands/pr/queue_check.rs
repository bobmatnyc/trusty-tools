//! `tm pr queue-check` — the merge-queue stop-condition table as one command
//! (#6653).
//!
//! Why: `tm-workflow.md`'s "Merge-Queue Ownership — the Procedure" section is
//! a fixed decision table driven by `gh` reads, and the recorded failure mode
//! is a session under time pressure skipping one of them — a critic BLOCK
//! arrived four minutes after a merge. The stop conditions are booleans over
//! JSON; nothing here is a judgment.
//!
//! What: [`run`] evaluates, per PR, the stop conditions IN THE DOCUMENTED
//! ORDER and reports the FIRST that fires:
//!
//!   1. `isDraft: true`
//!   2. a hold label (`do-not-merge*`, `hold`)
//!   3. `reviewDecision: CHANGES_REQUESTED`
//!   4. an unresolved `code-critic` BLOCK in the PR comments
//!   5. `mergeable: CONFLICTING` or `mergeStateStatus: DIRTY` — a real
//!      conflict GitHub already detected; `UNKNOWN` on either field, or
//!      either field missing from the payload, is pending (GitHub has not
//!      finished computing it, or the response shape changed) and NEVER
//!      reads as mergeable (#8670)
//!   6. a required status context missing, pending, or not `SUCCESS` on the
//!      head SHA — pending while any run has no result, else judged on its
//!      latest run (#8638)
//!
//! Order matters: the required contexts are the LAST gate, not the first, so a
//! draft PR reports "draft" rather than "checks pending". Required contexts
//! are read LIVE from branch protection (root `CLAUDE.md`, "What CI actually
//! gates" — a hand-copied list already cost PR #5836 a merge); the same read
//! is available standalone as `scripts/required-checks.sh`.
//!
//! Test: the sibling `tests.rs` — `queue_*`.

use serde::{Deserialize, Serialize};

// #8638: one rollup entry shape and one latest-run rule, shared with `tm wait`.
use super::rollup::{RollupEntry, deciding_runs};
use super::{EXIT_BLOCKED, EXIT_OK, GhRunner, argv, repo_slug};
use crate::cli::PrQueueCheckArgs;

/// Label names that hold a PR out of the queue, lowercase.
const HOLD_LABELS: [&str; 2] = ["hold", "do-not-merge"];

/// One PR's verdict.
///
/// Why: `--json` and the human line must never disagree, so both render the
/// same value.
/// What: the PR number, whether it is mergeable, and the first stop reason.
/// Test: `queue_verdict_json_matches_the_line`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Verdict {
    /// PR number.
    pub(crate) number: u64,
    /// Whether every stop condition passed.
    pub(crate) mergeable: bool,
    /// The FIRST stop condition that fired, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

impl Verdict {
    /// The one-line human rendering.
    pub(crate) fn line(&self) -> String {
        match &self.reason {
            None => format!("#{} MERGEABLE", self.number),
            Some(r) => format!("#{} BLOCKED: {r}", self.number),
        }
    }
}

/// A PR as `gh pr list --json …` reports it.
#[derive(Debug, Deserialize)]
struct PrListRow {
    number: u64,
}

/// A GitHub label, as every `--json labels` payload shapes it.
#[derive(Debug, Deserialize)]
struct Label {
    name: String,
}

/// One PR comment.
#[derive(Debug, Deserialize)]
struct Comment {
    #[serde(default)]
    body: String,
}

/// The single-PR view the stop conditions are evaluated against.
#[derive(Debug, Deserialize)]
struct PrView {
    #[serde(default)]
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    /// `MergeableState`: `MERGEABLE`, `CONFLICTING`, `UNKNOWN`, or absent.
    ///
    /// Why (#8670): never requesting this field is how queue-check reported
    /// MERGEABLE for a PR GitHub itself already marked CONFLICTING.
    /// `CONFLICTING` lives HERE, never in `mergeStateStatus` — the two enums
    /// are disjoint, mirroring `tm pr merge`'s `conflict_field` (#6808).
    #[serde(default)]
    mergeable: Option<String>,
    /// `MergeStateStatus`: `DIRTY`, `UNKNOWN`, `BLOCKED`, `BEHIND`,
    /// `UNSTABLE`, `HAS_HOOKS`, `CLEAN`, or absent. `DIRTY` is the conflict.
    #[serde(default)]
    #[serde(rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
    #[serde(default)]
    #[serde(rename = "statusCheckRollup")]
    rollup: Vec<RollupEntry>,
    #[serde(default)]
    comments: Vec<Comment>,
}

/// Evaluate the stop-condition table against one PR view.
///
/// Why: keeping this a pure function of (view, required contexts) is what
/// makes the documented ORDER testable — a PR that is simultaneously draft and
/// missing a check must report "draft", and only a test can pin that.
/// What: returns the first stop reason, or `None` when the PR is mergeable.
/// Test: `queue_stop_order_prefers_draft`, `queue_stop_order_prefers_hold`,
/// `queue_stop_order_prefers_changes_requested`,
/// `queue_stop_order_prefers_critic_block`,
/// `queue_required_context_missing`, `queue_required_context_not_success`,
/// `queue_duplicate_cancelled_then_success_is_mergeable`,
/// `queue_duplicate_success_then_failure_is_blocked`,
/// `queue_duplicate_success_then_running_is_pending`,
/// `queue_duplicate_success_then_queued_is_pending`,
/// `queue_check_and_status_same_name_both_required`,
/// `queue_mergeable_conflicting_is_blocked`, `queue_merge_state_dirty_is_blocked`,
/// `queue_mergeable_unknown_is_pending`, `queue_merge_state_unknown_is_pending`,
/// `queue_mergeable_field_missing_is_pending`,
/// `queue_merge_state_field_missing_is_pending`,
/// `queue_mergeable_clean_happy_path_is_admitted`.
fn stop_reason(view: &PrView, required: &[String]) -> Option<String> {
    if view.is_draft {
        return Some("draft".to_string());
    }
    if let Some(l) = view.labels.iter().find(|l| is_hold_label(&l.name)) {
        return Some(format!("hold label `{}`", l.name));
    }
    if view
        .review_decision
        .as_deref()
        .is_some_and(|d| d.eq_ignore_ascii_case("CHANGES_REQUESTED"))
    {
        return Some("review decision CHANGES_REQUESTED".to_string());
    }
    if latest_critic_verdict(&view.comments) == Some(CriticVerdict::Block) {
        return Some("unresolved code-critic BLOCK in the PR comments".to_string());
    }
    if let Some(reason) = mergeability_reason(view) {
        return Some(reason);
    }
    for context in required {
        // #8638: the latest run decides, never the first listed, and a
        // CheckRun and a StatusContext sharing the name must BOTH pass.
        let runs = deciding_runs(&view.rollup, context);
        if runs.is_empty() {
            return Some(format!(
                "required context `{context}` is missing on the head SHA"
            ));
        }
        if runs.iter().any(|e| e.settled() && !e.is_success()) {
            return Some(format!("required context `{context}` is not SUCCESS"));
        }
        if let Some(e) = runs.iter().find(|e| e.is_unfinished()) {
            return Some(format!(
                "required context `{context}` is pending: a run has no result yet {}",
                e.run_summary()
            ));
        }
    }
    None
}

/// Is `name` a hold label?
fn is_hold_label(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    HOLD_LABELS.iter().any(|h| {
        lower == *h || lower.starts_with(&format!("{h}/")) || lower.starts_with(&format!("{h}:"))
    })
}

/// Whether GitHub's own `mergeable`/`mergeStateStatus` fields stop this PR.
///
/// Why (#8670): queue-check never requested either field, so a PR GitHub
/// already marked CONFLICTING or DIRTY still reported MERGEABLE — a caller
/// trusting queue-check alone could attempt, and waste, a doomed merge. `tm
/// pr merge`'s own `conflict_field` (#6808) is the model for reading the
/// conflict: `CONFLICTING` lives in `mergeable`, `DIRTY` in
/// `mergeStateStatus`, and the two enums are disjoint.
/// What: a real conflict returns a reason naming the field. `UNKNOWN` on
/// either field, or either field absent from the payload (an unparseable
/// shape or a truncated response), also returns a reason — GitHub has not
/// finished computing mergeability, or the field never arrived — so both
/// fail CLOSED rather than defaulting to mergeable. Only `mergeable:
/// MERGEABLE` together with `mergeStateStatus` outside `{DIRTY, UNKNOWN}`
/// (e.g. `CLEAN`) returns `None`.
/// Test: `queue_mergeable_conflicting_is_blocked`,
/// `queue_merge_state_dirty_is_blocked`, `queue_mergeable_unknown_is_pending`,
/// `queue_merge_state_unknown_is_pending`,
/// `queue_mergeable_field_missing_is_pending`,
/// `queue_merge_state_field_missing_is_pending`,
/// `queue_mergeable_clean_happy_path_is_admitted`.
fn mergeability_reason(view: &PrView) -> Option<String> {
    let mergeable = view.mergeable.as_deref();
    let merge_state = view.merge_state_status.as_deref();

    if mergeable.is_some_and(|m| m.eq_ignore_ascii_case("CONFLICTING")) {
        return Some("mergeable CONFLICTING — resolve with `gh pr update-branch`".to_string());
    }
    if merge_state.is_some_and(|s| s.eq_ignore_ascii_case("DIRTY")) {
        return Some("mergeStateStatus DIRTY — resolve with `gh pr update-branch`".to_string());
    }

    match mergeable {
        None => {
            return Some("mergeable field is missing on the head SHA".to_string());
        }
        Some(m) if m.eq_ignore_ascii_case("UNKNOWN") => {
            return Some("mergeable is UNKNOWN — GitHub is still computing it; retry".to_string());
        }
        _ => {}
    }
    match merge_state {
        None => {
            return Some("mergeStateStatus field is missing on the head SHA".to_string());
        }
        Some(s) if s.eq_ignore_ascii_case("UNKNOWN") => {
            return Some(
                "mergeStateStatus is UNKNOWN — GitHub is still computing it; retry".to_string(),
            );
        }
        _ => {}
    }
    None
}

/// A `code-critic` verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CriticVerdict {
    /// The critic blocked the change.
    Block,
    /// The critic approved, or warned without blocking.
    Cleared,
}

/// The most recent `code-critic` verdict across a PR's comments.
///
/// Why: a BLOCK is unresolved only when no LATER comment cleared it, so the
/// answer is the last verdict in comment order, not "does a BLOCK exist".
/// What: considers only comments that name `code-critic` (case-insensitive),
/// and reads the last standalone `BLOCK` / `APPROVE` / `WARN` token in each.
/// Comments are returned by `gh` oldest-first, so the final match wins.
/// Test: `queue_critic_block_then_approve_is_clear`,
/// `queue_critic_ignores_unrelated_comments`.
fn latest_critic_verdict(comments: &[Comment]) -> Option<CriticVerdict> {
    let mut latest = None;
    for c in comments {
        if !c.body.to_ascii_lowercase().contains("code-critic") {
            continue;
        }
        for token in c.body.split(|ch: char| !ch.is_ascii_alphabetic()) {
            match token {
                "BLOCK" => latest = Some(CriticVerdict::Block),
                "APPROVE" | "WARN" => latest = Some(CriticVerdict::Cleared),
                _ => {}
            }
        }
    }
    latest
}

/// Read the live required status-check contexts for `base`.
///
/// Why: root `CLAUDE.md` requires this list be read live, never hand-copied —
/// a stale copy cost PR #5836 a merge. `scripts/required-checks.sh` is the
/// same read, standalone, for callers outside `tm`.
/// What: `gh api repos/<slug>/branches/<base>/protection --jq
/// '.required_status_checks.contexts[]'`, one context per line. An empty list
/// is an error: it means either no protection or a changed payload shape, and
/// treating "no required contexts" as "everything passed" would silently
/// remove the last gate.
/// Test: `queue_required_contexts_parse`, `queue_empty_required_list_errors`.
pub(crate) fn required_contexts<R: GhRunner>(
    gh: &R,
    slug: &str,
    base: &str,
) -> anyhow::Result<Vec<String>> {
    let path = format!("repos/{slug}/branches/{base}/protection");
    let a = argv(&["api", &path, "--jq", ".required_status_checks.contexts[]"]);
    let stdout = gh.run(&a)?.stdout_ok(&a)?;
    let contexts: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().trim_matches('"').to_string())
        .filter(|l| !l.is_empty())
        .collect();
    anyhow::ensure!(
        !contexts.is_empty(),
        "branch protection for `{base}` lists no required status checks; \
         refusing to report every PR mergeable off an empty gate list"
    );
    Ok(contexts)
}

/// Run `tm pr queue-check`.
///
/// Why: this replaces four hand-typed `gh` reads and the cross-check between
/// them with one exit code, so a batch merge cannot proceed on a queue nobody
/// fully read.
/// What: resolves `owner/repo`, reads the live required contexts, lists the
/// open PRs on `--base` (or takes the single `<pr>` argument), views each, and
/// prints one verdict line — or the whole set as a JSON array under `--json`.
/// Exits 0 when every listed PR is mergeable and 1 otherwise; an empty queue
/// is 0.
/// Test: `queue_exits_1_when_any_pr_blocked`, `queue_reports_mergeable`,
/// `queue_verdict_json_matches_the_line`, `queue_empty_queue_is_ok`.
pub(crate) fn run<R: GhRunner>(gh: &R, args: &PrQueueCheckArgs) -> anyhow::Result<i32> {
    let slug = repo_slug(gh, args.repo.as_deref())?;
    let required = required_contexts(gh, &slug, &args.base)?;

    let numbers = match args.pr {
        Some(n) => vec![n],
        None => list_open_prs(gh, &slug, &args.base)?,
    };

    let verdicts = verdicts(gh, &slug, &required, &numbers)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&verdicts)?);
    } else {
        for v in &verdicts {
            println!("{}", v.line());
        }
    }

    Ok(if verdicts.iter().all(|v| v.mergeable) {
        EXIT_OK
    } else {
        EXIT_BLOCKED
    })
}

/// Evaluate every PR in `numbers` against the stop-condition table.
///
/// Why: separating the per-PR evaluation from the printing lets the tests
/// assert the verdict values themselves rather than scraping stdout.
/// What: one `gh pr view` per PR, then [`stop_reason`].
/// Test: every `queue_stop_order_*` and `queue_required_context_*` case.
pub(crate) fn verdicts<R: GhRunner>(
    gh: &R,
    slug: &str,
    required: &[String],
    numbers: &[u64],
) -> anyhow::Result<Vec<Verdict>> {
    let mut out = Vec::with_capacity(numbers.len());
    for n in numbers {
        let view = pr_view(gh, slug, *n)?;
        let reason = stop_reason(&view, required);
        out.push(Verdict {
            number: *n,
            mergeable: reason.is_none(),
            reason,
        });
    }
    Ok(out)
}

/// The open PR numbers on `base`, oldest first.
///
/// Why: the merge-queue procedure's own ownership read is
/// `gh pr list --json number,author,assignees,isDraft,labels,headRefName`;
/// the extra fields are requested so the one call answers both the ownership
/// question and this one without a second round trip.
/// What: parses the `number` field out of that payload.
/// Test: `queue_lists_open_prs`.
fn list_open_prs<R: GhRunner>(gh: &R, slug: &str, base: &str) -> anyhow::Result<Vec<u64>> {
    let a = argv(&[
        "pr",
        "list",
        "--repo",
        slug,
        "--base",
        base,
        "--state",
        "open",
        "--limit",
        "100",
        "--json",
        "number,author,assignees,isDraft,labels,headRefName",
    ]);
    let stdout = gh.run(&a)?.stdout_ok(&a)?;
    let rows: Vec<PrListRow> = serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("cannot parse `gh pr list` JSON: {e}"))?;
    let mut numbers: Vec<u64> = rows.into_iter().map(|r| r.number).collect();
    numbers.sort_unstable();
    Ok(numbers)
}

/// One PR's stop-condition inputs, in a single `gh pr view` call.
///
/// #8670: `mergeable,mergeStateStatus` join the requested fields so
/// [`mergeability_reason`] has something to read; a prior version of this
/// call omitted them entirely.
fn pr_view<R: GhRunner>(gh: &R, slug: &str, pr: u64) -> anyhow::Result<PrView> {
    let n = pr.to_string();
    let a = argv(&[
        "pr",
        "view",
        &n,
        "--repo",
        slug,
        "--json",
        "isDraft,labels,reviewDecision,mergeable,mergeStateStatus,statusCheckRollup,comments",
    ]);
    let stdout = gh.run(&a)?.stdout_ok(&a)?;
    serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("cannot parse `gh pr view {pr}` JSON: {e}"))
}
