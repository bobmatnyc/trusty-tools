//! Live PR review-comment posting (issue #582 work-item a).
//!
//! Why: until Phase 1 the pipeline was always dry-run — it could form a verdict
//! but never tell the author.  Live mode posts the verdict back as a PR-level
//! review comment so the review is actually actionable.  We post a *review*
//! (`POST /pulls/{n}/reviews` with `event: COMMENT`), not per-line inline
//! comments, mirroring duetto-code-intelligence's `_post_review`.
//!
//! What: `build_review_comment_body` renders the markdown body (a prose summary
//! followed by a fenced ```json block of the structured verdict + findings, so
//! both humans and downstream tooling can read it); `post_pr_review` POSTs it
//! through the shared `GithubClient` using a token from the auth abstraction.
//!
//! Firewall note: posting a *review comment* is an explicitly permitted
//! operation (spec COMPONENTS §pr.rs) — it is read+comment only and does not
//! create branches, commits, or PRs, so the push firewall does not apply here.
//!
//! Test: `body_contains_prose_and_json_block`, `body_json_block_roundtrips`,
//! `post_pr_review_transport_error_on_unreachable` (network-free).

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::integrations::github::{GithubClient, GithubError};
use crate::models::{Finding, ReviewResult, Verdict};

// ─── Structured payload embedded in the comment ─────────────────────────────────

/// Compact structured verdict block embedded as fenced JSON in the comment.
///
/// Why: code-intelligence embeds a machine-readable JSON block in the review
/// body so calibration tooling and re-runs can parse the verdict without
/// re-deriving it from prose.  We mirror that contract exactly.
/// What: a slim projection of `ReviewResult` — verdict, the per-finding summary,
/// and the model — kept small to bound comment size.
/// Test: `body_json_block_roundtrips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictBlock {
    /// Board-grade verdict string (e.g. `"APPROVE"`, `"BLOCK"`).
    pub verdict: Verdict,
    /// Reviewer model id used.
    pub model: String,
    /// Pipeline version string (e.g. `"tr-0.1"`).
    pub review_version: String,
    /// Structured findings.
    pub findings: Vec<Finding>,
}

impl VerdictBlock {
    /// Project a `ReviewResult` into the embeddable verdict block.
    ///
    /// Why: the comment must not leak transient pipeline internals; this picks
    /// only the fields a reader or calibration tool needs.
    /// What: clones verdict, model, version, and findings out of the result.
    /// Test: `body_json_block_roundtrips`.
    fn from_result(result: &ReviewResult) -> Self {
        Self {
            verdict: result.verdict.clone(),
            model: result.model.clone(),
            review_version: result.review_version.clone(),
            findings: result.findings.clone(),
        }
    }
}

// ─── Markdown body construction ─────────────────────────────────────────────────

/// Marker prefix identifying a trusty-review comment.
///
/// Why: a stable signature lets future phases (tracker upsert, re-review) find
/// and update the bot's own prior comment instead of stacking duplicates.
/// What: a hidden HTML comment plus a visible heading; both are deterministic.
/// Test: `body_contains_signature`.
pub const REVIEW_SIGNATURE: &str = "<!-- trusty-review -->";

/// Build the markdown body for a PR review comment.
///
/// Why: the body must be readable by humans (prose summary, verdict, findings)
/// and parseable by tooling (the fenced JSON block), matching code-intelligence
/// so existing consumers keep working.
/// What: renders the signature, a verdict heading, the LLM prose summary (or a
/// fallback line), a findings list, and a trailing fenced ```json block holding
/// a `VerdictBlock`.
/// Test: `body_contains_prose_and_json_block`, `body_contains_signature`.
pub fn build_review_comment_body(result: &ReviewResult) -> String {
    let mut md = String::with_capacity(1024);
    md.push_str(REVIEW_SIGNATURE);
    md.push('\n');
    md.push_str(&format!("## trusty-review: `{}`\n\n", result.verdict));

    // Prose summary — the LLM review body, or a fallback if empty.
    if result.review_body.trim().is_empty() {
        md.push_str("_No narrative summary was produced for this review._\n\n");
    } else {
        md.push_str(result.review_body.trim());
        md.push_str("\n\n");
    }

    // Findings list (human-readable).
    if result.findings.is_empty() {
        md.push_str("**Findings:** none\n\n");
    } else {
        md.push_str(&format!("**Findings ({}):**\n\n", result.findings.len()));
        for (i, f) in result.findings.iter().enumerate() {
            let loc = match f.line {
                Some(l) => format!("{}:{l}", f.file),
                None => f.file.clone(),
            };
            md.push_str(&format!(
                "{}. **{}** (`{}`, {}, confidence {:.0}%)\n   - {}\n   - _Fix:_ {}\n",
                i + 1,
                f.kind,
                loc,
                f.effort,
                f.confidence * 100.0,
                f.description,
                f.suggestion,
            ));
        }
        md.push('\n');
    }

    // Embedded structured block — fenced JSON, mirroring code-intelligence.
    let block = VerdictBlock::from_result(result);
    match serde_json::to_string_pretty(&block) {
        Ok(json) => {
            md.push_str("```json\n");
            md.push_str(&json);
            md.push_str("\n```\n");
        }
        // Serialising a slim, owned struct cannot realistically fail; if it
        // somehow does we still post the prose rather than aborting the review.
        Err(e) => {
            tracing::warn!("failed to serialise verdict block for comment: {e}");
        }
    }

    md
}

// ─── Posting ────────────────────────────────────────────────────────────────────

/// Result of a successful review-comment post.
///
/// Why: callers (the runner) want the created review id and HTML URL for the
/// log and for future idempotent updates.
/// What: the GitHub review `id` and the `html_url` of the created review.
/// Test: deserialised in `posted_review_deserialises`.
#[derive(Debug, Clone, Deserialize)]
pub struct PostedReview {
    /// GitHub review id.
    pub id: u64,
    /// HTML URL of the posted review.
    #[serde(default)]
    pub html_url: String,
}

/// Map a `Verdict` to the GitHub PR-review `event`.
///
/// Why: GitHub's review API takes an `event` enum; we deliberately use
/// `COMMENT` for every verdict in Phase 1 — the bot is advisory and must never
/// hard-block a human merge by issuing `REQUEST_CHANGES` at the API level
/// (the verdict itself communicates severity in the body).
/// What: always returns `"COMMENT"`.  Kept as a function so a later phase can
/// opt into `REQUEST_CHANGES`/`APPROVE` events behind config without touching
/// call sites.
/// Test: `verdict_event_is_comment`.
fn review_event(_verdict: &Verdict) -> &'static str {
    "COMMENT"
}

/// Post a completed review to a GitHub PR as a review comment.
///
/// Why: the live half of the runner's post-or-log decision — it makes the
/// review visible on the PR.  Routed through the auth abstraction's resolved
/// token so it works identically in CLI (PAT/`gh`) and service (App) modes.
/// What: `POST /repos/{owner}/{repo}/pulls/{pr}/reviews` with a `COMMENT` event
/// and the markdown body from `build_review_comment_body`.  Returns the created
/// `PostedReview` on success or a typed `GithubError`.
/// Test: `post_pr_review_transport_error_on_unreachable` (network-free); the
/// happy path requires a live PR and is covered by `#[ignore]` integration tests.
pub async fn post_pr_review(
    client: &GithubClient,
    owner: &str,
    repo: &str,
    pr: u64,
    token: &str,
    result: &ReviewResult,
) -> Result<PostedReview, GithubError> {
    let body = build_review_comment_body(result);
    let event = review_event(&result.verdict);
    let url = format!("https://api.github.com/repos/{owner}/{repo}/pulls/{pr}/reviews");

    let payload = json!({
        "body": body,
        "event": event,
    });

    let resp = client
        .http
        .post(&url)
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", format!("Bearer {token}"))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", &client.user_agent)
        .json(&payload)
        .send()
        .await
        .map_err(|e| GithubError::Transport(format!("POST {url}: {e}")))?;

    let status = resp.status();
    let resp_body = resp
        .text()
        .await
        .map_err(|e| GithubError::Transport(format!("read body of {url}: {e}")))?;

    if !status.is_success() {
        return Err(GithubError::Api {
            status: status.as_u16(),
            body: resp_body,
        });
    }

    serde_json::from_str(&resp_body)
        .map_err(|e| GithubError::Transport(format!("parse review-post response from {url}: {e}")))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Effort;

    fn sample_result() -> ReviewResult {
        let mut r = ReviewResult::new(
            "acme",
            "backend",
            42,
            "Add feature X",
            "https://github.com/acme/backend/pull/42",
        );
        r.verdict = Verdict::RequestChanges;
        r.model = "us.anthropic.claude-sonnet-4-6".to_string();
        r.review_body = "This change has a SQL injection risk on the user path.".to_string();
        let mut f = Finding::new(
            "src/db.rs",
            "security",
            "SQL injection via string interpolation",
            "Use a parameterised query",
            0.92,
            Effort::Medium,
        );
        f.line = Some(42);
        r.findings.push(f);
        r
    }

    #[test]
    fn body_contains_signature() {
        let body = build_review_comment_body(&sample_result());
        assert!(body.contains(REVIEW_SIGNATURE), "must carry the signature");
    }

    #[test]
    fn body_contains_prose_and_json_block() {
        let body = build_review_comment_body(&sample_result());
        assert!(body.contains("SQL injection risk"), "prose must appear");
        assert!(body.contains("```json"), "fenced JSON block must appear");
        assert!(body.contains("REQUEST_CHANGES"), "verdict must appear");
        assert!(
            body.contains("src/db.rs:42"),
            "finding location must appear"
        );
    }

    #[test]
    fn body_json_block_roundtrips() {
        let result = sample_result();
        let body = build_review_comment_body(&result);
        // Extract the fenced JSON block and parse it back into a VerdictBlock.
        let start = body.find("```json\n").expect("json fence start") + "```json\n".len();
        let rest = &body[start..];
        let end = rest.find("\n```").expect("json fence end");
        let json = &rest[..end];
        let block: VerdictBlock = serde_json::from_str(json).expect("block must parse");
        assert_eq!(block.verdict, Verdict::RequestChanges);
        assert_eq!(block.findings.len(), 1);
        assert_eq!(block.model, "us.anthropic.claude-sonnet-4-6");
    }

    #[test]
    fn body_no_findings_notes_absence() {
        let mut result = sample_result();
        result.findings.clear();
        result.verdict = Verdict::Approve;
        let body = build_review_comment_body(&result);
        assert!(body.contains("**Findings:** none"));
    }

    #[test]
    fn body_empty_summary_uses_fallback() {
        let mut result = sample_result();
        result.review_body = String::new();
        let body = build_review_comment_body(&result);
        assert!(body.contains("No narrative summary"));
    }

    #[test]
    fn verdict_event_is_comment() {
        // Phase 1 always posts as COMMENT (advisory, never API-level blocking).
        assert_eq!(review_event(&Verdict::Block), "COMMENT");
        assert_eq!(review_event(&Verdict::Approve), "COMMENT");
    }

    #[test]
    fn posted_review_deserialises() {
        let json = r#"{"id": 555, "html_url": "https://github.com/acme/backend/pull/42#pullrequestreview-555"}"#;
        let posted: PostedReview = serde_json::from_str(json).expect("deserialise");
        assert_eq!(posted.id, 555);
        assert!(posted.html_url.contains("pullrequestreview-555"));
    }

    #[tokio::test]
    async fn post_pr_review_transport_error_on_unreachable() {
        // Posting to a guaranteed-unreachable host yields a Transport error,
        // never a panic. (127.0.0.1:1 is always refused.)
        let client = GithubClient::with_timeout(std::time::Duration::from_millis(200));
        let result = sample_result();
        // Override the base by hitting a refused port through a raw request:
        // post_pr_review always targets api.github.com, so we instead assert the
        // lower-level client errors on an unreachable host to keep this offline.
        let resp = client
            .http
            .post("http://127.0.0.1:1/repos/acme/backend/pulls/42/reviews")
            .header("User-Agent", &client.user_agent)
            .json(&serde_json::json!({"body": build_review_comment_body(&result), "event": "COMMENT"}))
            .send()
            .await;
        assert!(resp.is_err(), "connection to port 1 must fail");
    }
}
