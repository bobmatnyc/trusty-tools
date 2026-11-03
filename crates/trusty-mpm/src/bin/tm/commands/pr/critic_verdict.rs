//! `tm pr merge`'s code-critic verdict gate (#10724).
//!
//! Why: the critic posts APPROVE/WARN/BLOCK as a COMMENT-type review, never as
//! `CHANGES_REQUESTED` (GitHub refuses that event on a self-authored PR), so
//! `reviewDecision` never reflects a BLOCK and an unresolved BLOCK merged.
//!
//! What: [`read`] fetches the head sha with every comment and review in one
//! `gh pr view`; [`assess`] finds each canonical verdict line, pins it to a
//! commit, and refuses while the latest verdict pinned to the head is a BLOCK,
//! or while a BLOCK that names no commit is not followed by an APPROVE or WARN
//! pinned to the head. A verdict set that covers only older heads is a warning.
//!
//! The verdict line is the `code-review-standards` Output Format heading,
//! `## Verdict: <APPROVE|WARN|BLOCK>`, matched case-sensitively at the start of
//! a line outside a code fence. A review is pinned to its own `commit.oid`; a
//! `pinned to <sha>` clause on the verdict line overrides that, and is the only
//! pin a plain comment can carry.
//!
//! Test: `critic_verdict_tests.rs`, and `merge_critic_tests.rs` end to end.

use serde::Deserialize;

use super::GhRunner;

/// The canonical verdict heading (`code-review-standards`, Output Format).
const VERDICT_HEADING: &str = "## Verdict:";

/// The clause that pins a verdict line to a commit.
const PIN_CLAUSE: &str = "pinned to ";

/// One code-critic verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Approve,
    Warn,
    Block,
}

impl Verdict {
    /// The verdict word as the critic writes it.
    fn label(self) -> &'static str {
        match self {
            Verdict::Approve => "APPROVE",
            Verdict::Warn => "WARN",
            Verdict::Block => "BLOCK",
        }
    }
}

/// The `gh pr view --json headRefOid,comments,reviews` payload.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct VerdictView {
    /// The head commit the merge would land.
    #[serde(default, rename = "headRefOid")]
    pub(crate) head: String,
    /// Every PR (issue) comment.
    #[serde(default)]
    pub(crate) comments: Vec<Comment>,
    /// Every review, COMMENT-type critic verdicts included.
    #[serde(default)]
    pub(crate) reviews: Vec<Review>,
}

/// A comment or review author.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Author {
    #[serde(default)]
    pub(crate) login: String,
}

/// A PR comment.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Comment {
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default, rename = "createdAt")]
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) url: String,
}

/// The commit a review was submitted against.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CommitRef {
    #[serde(default)]
    pub(crate) oid: String,
}

/// A PR review.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Review {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) author: Option<Author>,
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default, rename = "submittedAt")]
    pub(crate) submitted_at: String,
    #[serde(default)]
    pub(crate) commit: Option<CommitRef>,
}

/// A verdict found on the PR, with the commit it covers.
#[derive(Debug, Clone)]
struct Posted {
    verdict: Verdict,
    /// Lowercase hex, 7 to 40 chars; `None` when nothing pins it.
    sha: Option<String>,
    /// RFC 3339 UTC timestamp; orders the verdicts.
    at: String,
    /// Where it was posted, for the refusal text.
    source: String,
}

/// What [`assess`] concluded.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Assessment {
    /// Refuse the merge with this reason.
    pub(crate) refusal: Option<String>,
    /// Merge, but print this.
    pub(crate) warning: Option<String>,
}

/// Fetch the head sha, comments and reviews in one read.
///
/// What: runs `base` (a `gh pr view <n> [--repo]` argv) plus
/// `--json headRefOid,comments,reviews`; a failed run or unparseable JSON is
/// an error, which the caller turns into a refusal.
/// Test: `pr_10724_a_verdict_read_failure_refuses`,
/// `pr_10724_unparseable_verdict_json_is_an_error`.
pub(crate) fn read<R: GhRunner>(gh: &R, mut base: Vec<String>) -> anyhow::Result<VerdictView> {
    base.push("--json".to_string());
    base.push("headRefOid,comments,reviews".to_string());
    let stdout = gh.run(&base)?.stdout_ok(&base)?;
    serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("cannot parse `gh {}` JSON: {e}", base.join(" ")))
}

/// The first canonical verdict line in `body`, with its `pinned to` sha.
///
/// Why: a strict match on the Output Format heading keeps prose such as
/// "verdict BLOCK" or a quoted line inside a code fence from holding a merge.
/// What: skips fenced lines; requires the trimmed line to start with
/// `## Verdict:`, then exactly `APPROVE`, `WARN` or `BLOCK` ending at a
/// non-letter. The sha is the 7-40 hex token after `pinned to `, if any.
/// Test: `pr_10724_prose_and_fenced_lines_are_not_verdicts`,
/// `pr_10724_a_pin_clause_is_parsed_from_the_verdict_line`.
pub(crate) fn parse_verdict_line(body: &str) -> Option<(Verdict, Option<String>)> {
    let mut fenced = false;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let Some(rest) = line.strip_prefix(VERDICT_HEADING) else {
            continue;
        };
        let rest = rest.trim_start();
        let end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let verdict = match &rest[..end] {
            "APPROVE" => Verdict::Approve,
            "WARN" => Verdict::Warn,
            "BLOCK" => Verdict::Block,
            _ => continue,
        };
        return Some((verdict, pin(&rest[end..])));
    }
    None
}

/// The sha after `pinned to ` in `tail`, lowercased, when it is 7-40 hex.
fn pin(tail: &str) -> Option<String> {
    let after = &tail[tail.find(PIN_CLAUSE)? + PIN_CLAUSE.len()..];
    let token: String = after
        .trim_start_matches(['`', ' '])
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    (7..=40)
        .contains(&token.len())
        .then(|| token.to_ascii_lowercase())
}

/// Every verdict on the PR, oldest first.
fn collect(view: &VerdictView) -> Vec<Posted> {
    let mut out: Vec<Posted> = Vec::new();
    for c in &view.comments {
        if let Some((verdict, sha)) = parse_verdict_line(&c.body) {
            out.push(Posted {
                verdict,
                sha,
                at: c.created_at.clone(),
                source: format!("comment {}", c.url),
            });
        }
    }
    for r in &view.reviews {
        if let Some((verdict, text_pin)) = parse_verdict_line(&r.body) {
            let oid = r.commit.as_ref().and_then(|c| pin_oid(&c.oid));
            let by = r.author.as_ref().map_or("", |a| a.login.as_str());
            out.push(Posted {
                verdict,
                sha: text_pin.or(oid),
                at: r.submitted_at.clone(),
                source: format!("review {} by {by}", r.id),
            });
        }
    }
    out.sort_by(|a, b| a.at.cmp(&b.at));
    out
}

/// A review's `commit.oid`, lowercased, when it is a full or short sha.
fn pin_oid(oid: &str) -> Option<String> {
    let oid = oid.trim();
    ((7..=40).contains(&oid.len()) && oid.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| oid.to_ascii_lowercase())
}

/// Judge the PR's code-critic verdicts against its head.
///
/// Why: a BLOCK holds the commit it reviewed; a later push makes it stale but
/// not resolved, so it warns instead of refusing. A BLOCK that names no commit
/// cannot be shown stale, so it fails closed.
/// What: refuses when the latest verdict pinned to the head is a BLOCK, or
/// when an unpinned BLOCK has no later APPROVE/WARN pinned to the head, or
/// when verdicts exist and `headRefOid` is empty. Warns when verdicts exist
/// and none is pinned to the head. No verdict at all: neither.
///
/// Test: `pr_10724_the_head_block_refusal_names_source_and_sha`,
/// `pr_10724_an_old_head_block_warns_that_no_verdict_covers_the_head`,
/// `pr_10724_a_later_warn_resolves_a_head_block`,
/// `pr_10724_a_head_block_after_an_approve_refuses`,
/// `pr_10724_an_unpinned_block_names_the_fix`,
/// `pr_10724_an_unpinned_block_is_resolved_by_a_later_head_approve`,
/// `pr_10724_no_verdict_is_neither_refusal_nor_warning`,
/// `pr_10724_verdicts_without_a_head_sha_refuse`.
pub(crate) fn assess(view: &VerdictView) -> Assessment {
    let posted = collect(view);
    if posted.is_empty() {
        return Assessment::default();
    }
    let head = view.head.trim().to_ascii_lowercase();
    if pin_oid(&head).is_none() {
        return refuse(format!(
            "`gh pr view` reported no head sha, so the {} code-critic verdict(s) on the PR \
             cannot be matched to a commit",
            posted.len()
        ));
    }
    let covers = |sha: &str| head.starts_with(sha);
    let mut last_on_head: Option<&Posted> = None;
    let mut open_unpinned: Option<&Posted> = None;
    for p in &posted {
        match (&p.sha, p.verdict) {
            (Some(sha), v) if covers(sha) => {
                last_on_head = Some(p);
                if v != Verdict::Block {
                    open_unpinned = None;
                }
            }
            (None, Verdict::Block) => open_unpinned = Some(p),
            _ => {}
        }
    }
    if let Some(p) = last_on_head.filter(|p| p.verdict == Verdict::Block) {
        return refuse(format!(
            "an unresolved code-critic BLOCK is pinned to the head {head} ({}) — fix the \
             findings and get a later APPROVE or WARN verdict pinned to {head}",
            p.source
        ));
    }
    if let Some(p) = open_unpinned {
        return refuse(format!(
            "a code-critic BLOCK names no commit sha ({}), so it holds the merge — post a later \
             APPROVE or WARN pinned to the head {head}: a COMMENT review on that commit, or a \
             comment whose verdict line reads `## Verdict: APPROVE, pinned to {head}`",
            p.source
        ));
    }
    if last_on_head.is_some() {
        return Assessment::default();
    }
    let latest = posted.last().map_or(String::new(), |p| {
        format!(
            "; the latest is {} pinned to {} ({})",
            p.verdict.label(),
            p.sha.as_deref().unwrap_or("no commit"),
            p.source
        )
    });
    Assessment {
        refusal: None,
        warning: Some(format!(
            "no code-critic verdict covers the head {head}{latest}"
        )),
    }
}

fn refuse(reason: String) -> Assessment {
    Assessment {
        refusal: Some(reason),
        warning: None,
    }
}

#[cfg(test)]
#[path = "critic_verdict_tests.rs"]
mod tests;
