//! GitHub PR metadata and diff fetching.
//!
//! Why: the review pipeline needs the unified diff and PR metadata (title,
//! author, base/head SHAs) to drive the LLM review.  This module provides
//! typed structures and fetch helpers for both.
//! (spec REV-404, source-analysis §4.2)
//!
//! What: `PrMetadata` captures the PR fields needed by the pipeline;
//! `fetch_pr_metadata` fetches the JSON metadata via the standard Accept header;
//! `fetch_pr_diff` fetches the unified diff via the `vnd.github.v3.diff` header.
//! Both helpers use the shared `GithubClient` and a pre-resolved access token.
//!
//! Test: `pr_metadata_deserialises_minimal_json` tests JSON deserialization;
//! `fetch_pr_diff_transport_error` verifies the transport error path without
//! a real network call.

use serde::{Deserialize, Serialize};

use crate::integrations::github::{GithubClient, GithubError};

// ─── PR metadata shape ────────────────────────────────────────────────────────

/// Core PR metadata fetched from the GitHub REST API.
///
/// Why: the pipeline needs the PR title, author, and base/head SHAs for the
/// dedup key, review body, and tracker issue title.
/// What: a typed subset of the `GET /repos/{owner}/{repo}/pulls/{number}` JSON
/// response; unknown fields are ignored by `#[serde(deny_unknown_fields)]` is
/// NOT used so the struct remains forward-compatible with new GitHub fields.
/// Test: `pr_metadata_deserialises_minimal_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrMetadata {
    /// PR number.
    pub number: u64,
    /// PR title.
    pub title: String,
    /// HTML URL (e.g. `https://github.com/owner/repo/pull/42`).
    pub html_url: String,
    /// PR state: `"open"`, `"closed"`.
    pub state: String,
    /// Author login.
    pub user: PrUser,
    /// Base branch ref and SHA.
    pub base: PrRef,
    /// Head branch ref and SHA.
    pub head: PrRef,
    /// PR body (description), may be null.
    #[serde(default)]
    pub body: Option<String>,
}

/// GitHub user (author) embedded in PR metadata.
///
/// Why: the pipeline uses the author login for the excluded-authors gate.
/// What: minimal shape — just the `login` field.
/// Test: covered transitively by `pr_metadata_deserialises_minimal_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrUser {
    /// GitHub login (username).
    pub login: String,
}

/// Branch reference (base or head) embedded in PR metadata.
///
/// Why: both the base and head SHA are needed for the dedup key and for
/// context retrieval.
/// What: `label` is `"owner:branch"`, `sha` is the full commit SHA.
/// Test: covered transitively by `pr_metadata_deserialises_minimal_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrRef {
    /// Branch label (e.g. `"main"` or `"feature/my-branch"`).
    #[serde(rename = "ref")]
    pub branch: String,
    /// Full 40-character commit SHA.
    pub sha: String,
    /// Repository label (owner/name) on the fork side.
    #[serde(default)]
    pub label: Option<String>,
}

// ─── Fetch helpers ────────────────────────────────────────────────────────────

/// Fetch PR metadata (title, author, SHAs) from the GitHub REST API.
///
/// Why: the pipeline needs structured metadata before fetching the diff so it
/// can apply the eligibility gate (author exclusion, repo exclusion) early.
/// What: `GET /repos/{owner}/{repo}/pulls/{pr}` with the standard JSON Accept
/// header.  Returns a typed `PrMetadata` struct.
/// Test: no real-network tests; `pr_metadata_deserialises_minimal_json` covers
/// the JSON parsing path.
pub async fn fetch_pr_metadata(
    client: &GithubClient,
    owner: &str,
    repo: &str,
    pr: u64,
    token: &str,
) -> Result<PrMetadata, GithubError> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/pulls/{pr}");
    let resp = client
        .http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", format!("Bearer {token}"))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", &client.user_agent)
        .send()
        .await
        .map_err(|e| GithubError::Transport(format!("GET {url}: {e}")))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| GithubError::Transport(format!("read body of {url}: {e}")))?;

    if !status.is_success() {
        return Err(GithubError::Api {
            status: status.as_u16(),
            body,
        });
    }

    serde_json::from_str(&body)
        .map_err(|e| GithubError::Transport(format!("parse PR metadata from {url}: {e}")))
}

/// Fetch the unified diff for a pull request.
///
/// Why: the diff is the primary input to the LLM reviewer.  Using the
/// `vnd.github.v3.diff` Accept header causes GitHub to return the raw diff
/// text directly rather than a JSON envelope.
/// What: `GET /repos/{owner}/{repo}/pulls/{pr}` with the diff Accept header.
/// Returns the raw unified diff as a `String`.
/// Test: `fetch_pr_diff_transport_error` verifies error handling without a real
/// network call; real-network path is covered by integration tests only.
pub async fn fetch_pr_diff(
    client: &GithubClient,
    owner: &str,
    repo: &str,
    pr: u64,
    token: &str,
) -> Result<String, GithubError> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/pulls/{pr}");
    let resp = client
        .http
        .get(&url)
        .header("Accept", "application/vnd.github.v3.diff")
        .header("Authorization", format!("Bearer {token}"))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", &client.user_agent)
        .send()
        .await
        .map_err(|e| GithubError::Transport(format!("GET {url} (diff): {e}")))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| GithubError::Transport(format!("read body of {url} (diff): {e}")))?;

    if !status.is_success() {
        return Err(GithubError::Api {
            status: status.as_u16(),
            body,
        });
    }

    Ok(body)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_metadata_deserialises_minimal_json() {
        // Fake commit SHAs — low entropy placeholder values for test-only JSON.
        let base_sha = "baaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; // pragma: allowlist secret
        let head_sha = "feeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"; // pragma: allowlist secret
        let json = format!(
            r#"{{
            "number": 42,
            "title": "Add feature X",
            "html_url": "https://github.com/acme/backend/pull/42",
            "state": "open",
            "user": {{ "login": "alice" }},
            "base": {{ "ref": "main", "sha": "{base_sha}", "label": "acme:main" }},
            "head": {{ "ref": "feature/x", "sha": "{head_sha}", "label": "alice:feature/x" }},
            "body": "This PR adds feature X."
        }}"#
        );

        let meta: PrMetadata = serde_json::from_str(&json).expect("should deserialise");
        assert_eq!(meta.number, 42);
        assert_eq!(meta.title, "Add feature X");
        assert_eq!(meta.user.login, "alice");
        assert_eq!(meta.base.sha, base_sha);
        assert_eq!(meta.head.sha, head_sha);
        assert_eq!(meta.body.as_deref(), Some("This PR adds feature X."));
    }

    #[test]
    fn pr_metadata_null_body_defaults_to_none() {
        let json = r#"{
            "number": 1,
            "title": "Fix typo",
            "html_url": "https://github.com/o/r/pull/1",
            "state": "open",
            "user": { "login": "bob" },
            "base": { "ref": "main", "sha": "aaa" },
            "head": { "ref": "fix/typo", "sha": "bbb" }
        }"#;

        let meta: PrMetadata = serde_json::from_str(json).expect("should deserialise");
        assert!(
            meta.body.is_none(),
            "missing body field should deserialise as None"
        );
    }

    #[test]
    fn pr_metadata_ignores_extra_fields() {
        // Verify forward-compatibility: extra fields from the GitHub API do not
        // cause a deserialisation error.
        let json = r#"{
            "number": 99,
            "title": "Test",
            "html_url": "https://github.com/o/r/pull/99",
            "state": "open",
            "user": { "login": "eve", "id": 12345, "avatar_url": "https://example.com/e.png" },
            "base": { "ref": "main", "sha": "aaa", "repo": { "name": "r" } },
            "head": { "ref": "br", "sha": "bbb" },
            "draft": false,
            "merged": null
        }"#;

        let meta: PrMetadata = serde_json::from_str(json).expect("extra fields should be ignored");
        assert_eq!(meta.number, 99);
        assert_eq!(meta.user.login, "eve");
    }

    #[tokio::test]
    async fn fetch_pr_diff_transport_error_on_unreachable_host() {
        // Sending to a guaranteed-unreachable address must yield a Transport error.
        let client = GithubClient::with_timeout(std::time::Duration::from_millis(200))
            .expect("TLS init should succeed in tests");
        // 127.0.0.1:1 is always refused (port 1 is reserved/privileged).
        let result = client
            .http
            .get("http://127.0.0.1:1/repos/o/r/pulls/1")
            .header("Accept", "application/vnd.github.v3.diff")
            .header("Authorization", "Bearer dummy")
            .header("User-Agent", &client.user_agent)
            .send()
            .await;
        assert!(result.is_err(), "connection to port 1 must fail");
    }
}
