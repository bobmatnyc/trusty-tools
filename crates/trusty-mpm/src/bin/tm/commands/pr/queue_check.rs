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
//!   5. a required status context missing, or not `SUCCESS`, on the head SHA
//!
//! Order matters: the required contexts are the LAST gate, not the first, so a
//! draft PR reports "draft" rather than "checks pending". Required contexts
//! are read LIVE from branch protection (root `CLAUDE.md`, "What CI actually
//! gates" — a hand-copied list already cost PR #5836 a merge); the same read
//! is available standalone as `scripts/required-checks.sh`.
//!
//! Test: the sibling `tests.rs` — `queue_*`.

use serde::{Deserialize, Serialize};

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

/// One entry of `statusCheckRollup`.
///
/// Why: the rollup mixes two GraphQL types. A `CheckRun` carries `name` +
/// `status` + `conclusion`; a `StatusContext` carries `context` + `state`.
/// Deserializing both permissively into one struct keeps the matcher single.
/// What: every field optional; [`RollupEntry::label`] and
/// [`RollupEntry::is_success`] normalize across the two shapes.
/// Test: `queue_required_context_not_success`, `queue_accepts_status_context`.
#[derive(Debug, Deserialize)]
struct RollupEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

impl RollupEntry {
    /// The context name this entry reports under.
    fn label(&self) -> Option<&str> {
        self.name
            .as_deref()
            .or(self.context.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Did this check pass?
    ///
    /// Why: `SUCCESS` only. `NEUTRAL` and `SKIPPED` are not success, and a
    /// required context that skipped has not proven anything.
    /// What: `conclusion == SUCCESS` (CheckRun) or `state == SUCCESS`
    /// (StatusContext).
    /// Test: `queue_required_context_not_success`.
    fn is_success(&self) -> bool {
        let v = self.conclusion.as_deref().or(self.state.as_deref());
        v.is_some_and(|s| s.eq_ignore_ascii_case("SUCCESS"))
    }
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
/// `queue_required_context_missing`, `queue_required_context_not_success`.
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
    for context in required {
        match view
            .rollup
            .iter()
            .find(|e| e.label() == Some(context.as_str()))
        {
            None => {
                return Some(format!(
                    "required context `{context}` is missing on the head SHA"
                ));
            }
            Some(e) if !e.is_success() => {
                return Some(format!("required context `{context}` is not SUCCESS"));
            }
            Some(_) => {}
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
/// Test: `queue_exits_1_when_any_pr_blocked`, `queue_json_shape`,
/// `queue_empty_queue_is_ok`.
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
fn pr_view<R: GhRunner>(gh: &R, slug: &str, pr: u64) -> anyhow::Result<PrView> {
    let n = pr.to_string();
    let a = argv(&[
        "pr",
        "view",
        &n,
        "--repo",
        slug,
        "--json",
        "isDraft,labels,reviewDecision,statusCheckRollup,comments",
    ]);
    let stdout = gh.run(&a)?.stdout_ok(&a)?;
    serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("cannot parse `gh pr view {pr}` JSON: {e}"))
}
