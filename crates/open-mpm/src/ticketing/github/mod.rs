//! GitHub Issues adapter.
//!
//! Why: GitHub is the most common issue tracker for OSS projects. REST v3
//! is stable and well-documented; bearer auth is simple.
//! What: `GitHubClient` implements `TicketingClient` against
//! `https://api.github.com/repos/{owner}/{repo}/issues...`.
//! Test: Construction tests in `src/ticketing/mod.rs` cover missing
//! credentials; live calls are not exercised in unit tests.
//!
//! Module layout (see #366 split): struct + inherent helpers + parsing here;
//! the `impl TicketingClient` block in `client_impl.rs`.

mod client_impl;

use anyhow::{Context, Result, anyhow};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::Value;

use super::TicketingConfig;
use super::types::*;

pub(super) const GH_API: &str = "https://api.github.com";

/// GitHub Issues client.
pub struct GitHubClient {
    client: reqwest::Client,
    owner: String,
    repo: String,
}

impl GitHubClient {
    /// Build a new GitHub client.
    ///
    /// Why: Fail fast on missing credentials so the agent gets a clear error
    /// rather than a cryptic 401 later.
    /// What: Resolves token from config or `GITHUB_TOKEN` env; splits
    /// `owner/repo`; builds a reqwest client with default auth headers.
    /// Test: `github_client_new_requires_token`, `github_client_new_requires_repo`.
    pub fn new(config: &TicketingConfig) -> Result<Self> {
        let token = config
            .github_token
            .clone()
            .or_else(|| std::env::var("GITHUB_TOKEN").ok())
            .ok_or_else(|| {
                anyhow!("GitHub token required (set github_token or GITHUB_TOKEN env)")
            })?;
        let repo_full = config
            .github_repo
            .clone()
            .ok_or_else(|| anyhow!("github_repo required (owner/repo)"))?;
        let (owner, repo) = repo_full.split_once('/').ok_or_else(|| {
            anyhow!("github_repo must be in 'owner/repo' format, got '{repo_full}'")
        })?;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .context("invalid characters in GitHub token")?,
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("open-mpm/0.1.0"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .context("failed to build reqwest client for GitHub")?;

        Ok(Self {
            client,
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    fn issues_url(&self) -> String {
        format!("{GH_API}/repos/{}/{}/issues", self.owner, self.repo)
    }

    fn issue_url(&self, id: &str) -> String {
        format!("{GH_API}/repos/{}/{}/issues/{}", self.owner, self.repo, id)
    }
}

/// Map a GitHub issue JSON payload to our canonical `Ticket`.
pub(super) fn issue_to_ticket(v: &Value) -> Result<Ticket> {
    let id = v
        .get("number")
        .and_then(Value::as_i64)
        .map(|n| n.to_string())
        .ok_or_else(|| anyhow!("issue missing 'number'"))?;
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let body = v
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let state = v.get("state").and_then(Value::as_str).unwrap_or("open");
    let status = match state {
        "closed" => TicketStatus::Closed,
        _ => TicketStatus::Open,
    };
    let labels: Vec<String> = v
        .get("labels")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let assignee = v
        .get("assignee")
        .and_then(|a| a.get("login").and_then(Value::as_str))
        .map(|s| s.to_string());
    let url = v
        .get("html_url")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let created_at = v
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let updated_at = v
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    Ok(Ticket {
        id,
        title,
        body,
        status,
        priority: None,
        labels,
        assignee,
        created_at,
        updated_at,
        url,
    })
}

pub(super) fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}
