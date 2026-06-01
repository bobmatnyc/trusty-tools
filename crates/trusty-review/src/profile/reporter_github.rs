//! GitHub issue upsert for the contributor-profile reporter (#567).
//!
//! Why: extracted from `reporter.rs` to keep that file under the 500-line cap;
//! all GitHub API types and the upsert function live here.
//! What: provides `github_upsert_issue`, `IssueBody`, and `issue_title`.
//! These are all `pub(super)` so they are visible only within the `reporter`
//! module and its test file.
//! Test: covered by `reporter_tests::reporter_github_issue_request_construction`
//! (mock only — no real network calls).

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::integrations::github::GithubClient;
use crate::profile::types::ContributorProfile;

use super::GithubIssueConfig;

// ─── Wire types ───────────────────────────────────────────────────────────────

/// Issue search result from GitHub search API.
#[derive(Debug, Deserialize)]
pub(super) struct IssueSearchResult {
    #[serde(default)]
    pub items: Vec<IssueItem>,
}

/// Minimal issue item returned by the search API.
#[derive(Debug, Deserialize)]
pub(super) struct IssueItem {
    pub number: u64,
    pub html_url: String,
    pub title: String,
}

/// Create or update issue request body.
#[derive(Debug, Serialize)]
pub(super) struct IssueBody {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

/// Comment body for issue updates.
#[derive(Debug, Serialize)]
pub(super) struct CommentBody {
    pub body: String,
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Expected title prefix for dev-profile issues.
///
/// Why: the title is used both for issue creation and for searching existing
/// issues by prefix so each contributor maps to exactly one issue thread.
/// What: formats as `"[dev-profile] <name> <email>"`.
/// Test: `reporter_tests::reporter_github_issue_request_construction`.
pub(super) fn issue_title(profile: &ContributorProfile) -> String {
    format!(
        "[dev-profile] {} <{}>",
        profile.canonical_name, profile.canonical_email
    )
}

/// Upsert a GitHub issue: search by label + title prefix, then create or comment.
///
/// Why: each contributor maps to exactly one issue thread so profiles
/// accumulate over time as new comments rather than creating duplicate issues.
/// What: POSTs to `GET /search/issues?q=...` to find an existing issue, then
/// POSTs a comment if found, or creates a new issue if not.  Returns the URL.
/// Test: `reporter_tests::reporter_github_issue_request_construction` (mock only).
pub(super) async fn github_upsert_issue(
    client: &GithubClient,
    config: &GithubIssueConfig,
    profile: &ContributorProfile,
    markdown: &str,
) -> Result<String, crate::integrations::github::GithubError> {
    use crate::integrations::github::GithubError;

    let title = issue_title(profile);

    let search_url = format!(
        "https://api.github.com/search/issues?q=repo:{owner}/{repo}+label:{label}+in:title+{email}&type=issue",
        owner = config.owner,
        repo = config.repo,
        label = config.label,
        email = urlencoding_simple(&config.label),
    );
    debug!(url = %search_url, "searching for existing dev-profile issue");

    let search_resp = client
        .http
        .get(&search_url)
        .header("Authorization", format!("Bearer {}", config.token))
        .header("User-Agent", &client.user_agent)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| GithubError::Transport(e.to_string()))?;

    if !search_resp.status().is_success() {
        let status = search_resp.status().as_u16();
        let body = search_resp
            .text()
            .await
            .unwrap_or_else(|_| String::from("(no body)"));
        return Err(GithubError::Api { status, body });
    }

    let search_result: IssueSearchResult = search_resp
        .json()
        .await
        .map_err(|e| GithubError::Transport(e.to_string()))?;

    let existing = search_result.items.iter().find(|i| {
        i.title.starts_with("[dev-profile]") && i.title.contains(&profile.canonical_email)
    });

    if let Some(issue) = existing {
        let comment_url = format!(
            "https://api.github.com/repos/{owner}/{repo}/issues/{number}/comments",
            owner = config.owner,
            repo = config.repo,
            number = issue.number,
        );
        let comment = CommentBody {
            body: markdown.to_string(),
        };
        let resp = client
            .http
            .post(&comment_url)
            .header("Authorization", format!("Bearer {}", config.token))
            .header("User-Agent", &client.user_agent)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&comment)
            .send()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| String::from("(no body)"));
            return Err(GithubError::Api {
                status,
                body: body_text,
            });
        }
        Ok(issue.html_url.clone())
    } else {
        let create_url = format!(
            "https://api.github.com/repos/{owner}/{repo}/issues",
            owner = config.owner,
            repo = config.repo,
        );
        let issue_body = IssueBody {
            title,
            body: markdown.to_string(),
            labels: vec![config.label.clone()],
        };
        let resp = client
            .http
            .post(&create_url)
            .header("Authorization", format!("Bearer {}", config.token))
            .header("User-Agent", &client.user_agent)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&issue_body)
            .send()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| String::from("(no body)"));
            return Err(GithubError::Api {
                status,
                body: body_text,
            });
        }

        #[derive(Deserialize)]
        struct CreatedIssue {
            html_url: String,
        }
        let created: CreatedIssue = resp
            .json()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;
        Ok(created.html_url)
    }
}

/// Minimal URL-component encoding (spaces → `+`, `@` → `%40`).
pub(super) fn urlencoding_simple(s: &str) -> String {
    s.replace(' ', "+").replace('@', "%40")
}
