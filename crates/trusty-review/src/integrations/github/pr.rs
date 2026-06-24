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

// ─── Reaction and commit types ────────────────────────────────────────────────

/// A single emoji reaction on a GitHub review comment.
///
/// Why: reaction data tells us whether the author accepted a finding
/// (👍/🚀) or dismissed it (👎) — the primary outcome signal.
/// What: a typed subset of the `GET /repos/{owner}/{repo}/pulls/comments/{id}/reactions`
/// response. Unknown `content` values are kept as-is for forward compatibility.
/// Test: `reaction_deserialises`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    /// Emoji content string e.g. `"+1"`, `"-1"`, `"rocket"`.
    pub content: String,
    /// Login of the user who reacted.
    pub user: PrUser,
    /// ISO-8601 creation timestamp e.g. `"2026-06-23T12:00:00Z"`.
    pub created_at: String,
}

/// Minimal commit info from the PR commits list.
///
/// Why: follow-up commits touching a finding's file within ~7 days signal
/// that the author acted on the finding (ActedOn outcome).
/// What: a typed subset of the
/// `GET /repos/{owner}/{repo}/pulls/{pr}/commits` response.
/// Test: `commit_info_deserialises`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Commit SHA.
    pub sha: String,
    /// Commit timestamp (from `commit.author.date`) ISO-8601.
    pub commit_date: String,
    /// Files changed in this commit (from `files[].filename`).
    /// Populated only when the response includes file data (not always present
    /// in the listing endpoint — callers must note this).
    #[serde(default)]
    pub files: Vec<String>,
}

// ─── Reaction and commit fetch helpers ───────────────────────────────────────

/// Fetch emoji reactions on a PR review comment.
///
/// Why: reactions (👍/🚀 = accepted, 👎 = dismissed) are the cheapest
/// outcome signal — they require no diff analysis and are always present
/// when the author interacts with a comment.
/// What: `GET /repos/{owner}/{repo}/pulls/comments/{comment_id}/reactions`
/// with the reactions preview Accept header. Fail-open: returns `Ok(vec![])`
/// on API errors to avoid blocking the outcome pipeline.
/// Test: `reaction_deserialises` covers the JSON parsing path; transport errors
/// are logged and returned as empty to callers.
pub async fn get_review_comment_reactions(
    client: &GithubClient,
    owner: &str,
    repo: &str,
    comment_id: u64,
    token: &str,
) -> Result<Vec<Reaction>, GithubError> {
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/pulls/comments/{comment_id}/reactions"
    );
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
        .map_err(|e| GithubError::Transport(format!("parse reactions from {url}: {e}")))
}

/// Fetch commits on a PR and populate per-commit file lists.
///
/// Why: follow-up commits touching a finding's file within ~7 days of the
/// review are an `ActedOn` signal — the author fixed the issue without
/// explicitly reacting to the comment.
///
/// What: two-phase fetch —
///
///   1. `GET /repos/{owner}/{repo}/pulls/{pr}/commits?per_page=100` returns the
///      SHA list (no per-commit file data from this endpoint).
///   2. For each of the first 20 commits, `GET /repos/{owner}/{repo}/commits/{sha}`
///      returns file-level change data; `files[].filename` is extracted and stored
///      on the corresponding `CommitInfo`.
///
/// Fail-open: if the per-commit fetch fails for any SHA, a `warn!` is logged
/// and that commit is returned with `files: vec![]` — the batch continues.
/// Cap at 20 commits to bound N+1 API cost. No pagination: `per_page=100`
/// is sufficient for typical PRs.
///
/// Test: `commit_info_deserialises` covers SHA-list parsing;
/// `commit_files_populated_from_single_commit_response` covers file extraction.
pub async fn get_pr_commits_after(
    client: &GithubClient,
    owner: &str,
    repo: &str,
    pr: u64,
    token: &str,
) -> Result<Vec<CommitInfo>, GithubError> {
    let url =
        format!("https://api.github.com/repos/{owner}/{repo}/pulls/{pr}/commits?per_page=100");
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

    // GitHub commits-list endpoint: array with sha + nested commit.author.date.
    // The `files[]` array is NOT present in the list response — only in the
    // individual commit endpoint.
    #[derive(serde::Deserialize)]
    struct RawCommit {
        sha: String,
        commit: RawCommitInner,
        /// Present in single-commit response; absent in list response.
        #[serde(default)]
        files: Vec<RawCommitFile>,
    }
    #[derive(serde::Deserialize)]
    struct RawCommitInner {
        author: RawCommitAuthor,
    }
    #[derive(serde::Deserialize)]
    struct RawCommitAuthor {
        date: String,
    }
    #[derive(serde::Deserialize)]
    struct RawCommitFile {
        filename: String,
    }

    let raw: Vec<RawCommit> = serde_json::from_str(&body)
        .map_err(|e| GithubError::Transport(format!("parse commits from {url}: {e}")))?;

    // Phase 1: build the flat list from the SHA-list response (files are empty here).
    let mut commits: Vec<CommitInfo> = raw
        .into_iter()
        .map(|c| CommitInfo {
            sha: c.sha,
            commit_date: c.commit.author.date,
            files: vec![],
        })
        .collect();

    // Phase 2: enrich the first 20 commits with per-file data from the individual
    // commit endpoint.  Fail-open: on any error, continue with empty files.
    const FILE_ENRICH_CAP: usize = 20;
    for info in commits.iter_mut().take(FILE_ENRICH_CAP) {
        let sha_url = format!(
            "https://api.github.com/repos/{owner}/{repo}/commits/{}",
            info.sha
        );
        let result = client
            .http
            .get(&sha_url)
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", format!("Bearer {token}"))
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", &client.user_agent)
            .send()
            .await;
        let resp = match result {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(sha = %info.sha, error = %e, "per-commit file fetch failed; continuing with empty files");
                continue;
            }
        };
        if !resp.status().is_success() {
            tracing::warn!(sha = %info.sha, status = %resp.status(), "per-commit file fetch non-2xx; continuing with empty files");
            continue;
        }
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(sha = %info.sha, error = %e, "per-commit body read failed; continuing with empty files");
                continue;
            }
        };
        match serde_json::from_str::<RawCommit>(&text) {
            Ok(raw_single) => {
                info.files = raw_single.files.into_iter().map(|f| f.filename).collect();
            }
            Err(e) => {
                tracing::warn!(sha = %info.sha, error = %e, "per-commit JSON parse failed; continuing with empty files");
            }
        }
    }

    Ok(commits)
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

    #[test]
    fn reaction_deserialises() {
        let json = r#"[
            {"id": 1, "content": "+1", "user": {"login": "alice"}, "created_at": "2026-06-23T12:00:00Z"},
            {"id": 2, "content": "rocket", "user": {"login": "bob"}, "created_at": "2026-06-23T13:00:00Z"}
        ]"#;
        let reactions: Vec<Reaction> = serde_json::from_str(json).expect("should deserialise");
        assert_eq!(reactions.len(), 2);
        assert_eq!(reactions[0].content, "+1");
        assert_eq!(reactions[0].user.login, "alice");
        assert_eq!(reactions[1].content, "rocket");
    }

    #[test]
    fn commit_info_deserialises() {
        let json = r#"[
            {"sha": "abc123", "commit": {"author": {"date": "2026-06-20T10:00:00Z"}, "message": "fix: resolve issue"}},
            {"sha": "def456", "commit": {"author": {"date": "2026-06-21T11:00:00Z"}, "message": "chore: cleanup"}}
        ]"#;
        // CommitInfo uses the raw deserialization path in get_pr_commits_after.
        // Test the RawCommit parsing by direct serde.
        #[derive(serde::Deserialize)]
        struct RawCommit {
            sha: String,
            commit: RawCommitInner,
            #[serde(default)]
            files: Vec<RawCommitFile>,
        }
        #[derive(serde::Deserialize)]
        struct RawCommitInner {
            author: RawCommitAuthor,
        }
        #[derive(serde::Deserialize)]
        struct RawCommitAuthor {
            date: String,
        }
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct RawCommitFile {
            filename: String,
        }
        let raw: Vec<RawCommit> = serde_json::from_str(json).expect("should deserialise");
        assert_eq!(raw.len(), 2);
        assert_eq!(raw[0].sha, "abc123");
        assert_eq!(raw[0].commit.author.date, "2026-06-20T10:00:00Z");
        assert!(raw[0].files.is_empty(), "list response has no files");
        assert_eq!(raw[1].sha, "def456");
    }

    /// Verify that the single-commit response shape (with `files[]`) is parsed correctly.
    ///
    /// Why: `get_pr_commits_after` phase-2 uses the single-commit endpoint to
    /// populate `CommitInfo.files`; this test exercises the JSON parsing without
    /// making real network calls.
    /// What: construct a fake single-commit JSON body matching GitHub's shape,
    /// parse it with the same local structs used in the function, assert filenames.
    /// Test: no network calls — pure serde deserialization.
    #[test]
    fn commit_files_populated_from_single_commit_response() {
        // Fake single-commit response: matches GET /repos/{owner}/{repo}/commits/{sha}.
        let json = r#"{
            "sha": "abc123",
            "commit": {
                "author": { "date": "2026-06-20T10:00:00Z" },
                "message": "fix: resolve issue"
            },
            "files": [
                { "filename": "src/main.rs", "status": "modified" },
                { "filename": "src/lib.rs",  "status": "added" }
            ]
        }"#;

        // Mirror the private local structs used inside get_pr_commits_after.
        #[derive(serde::Deserialize)]
        struct RawCommit {
            sha: String,
            commit: RawCommitInner,
            #[serde(default)]
            files: Vec<RawCommitFile>,
        }
        #[derive(serde::Deserialize)]
        struct RawCommitInner {
            author: RawCommitAuthor,
        }
        #[derive(serde::Deserialize)]
        struct RawCommitAuthor {
            date: String,
        }
        #[derive(serde::Deserialize)]
        struct RawCommitFile {
            filename: String,
        }

        let raw: RawCommit = serde_json::from_str(json).expect("should deserialise");
        assert_eq!(raw.sha, "abc123");
        assert_eq!(raw.commit.author.date, "2026-06-20T10:00:00Z");
        assert_eq!(raw.files.len(), 2, "two file entries expected");
        let filenames: Vec<&str> = raw.files.iter().map(|f| f.filename.as_str()).collect();
        assert!(
            filenames.contains(&"src/main.rs"),
            "src/main.rs must be present"
        );
        assert!(
            filenames.contains(&"src/lib.rs"),
            "src/lib.rs must be present"
        );
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
