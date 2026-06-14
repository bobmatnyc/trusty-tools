//! GitHub backend — HTTP client methods.
//!
//! Why: Separates the low-level HTTP transport from the high-level Backend
//! trait impl so each file stays under the 500-SLOC cap.
//! What: Provides `GitHubBackend` constructor and the private REST/GraphQL
//! helper methods used by the Backend impl.
//! Test: exercised transitively by all Backend impl methods.

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Client;
use serde_json::{Value, json};

use crate::tickets::api::config::GithubConfig;

use super::types::{GitHubBackend, GRAPHQL_URL, REST_BASE, USER_AGENT, ensure_ok};

impl GitHubBackend {
    /// Why: Caller has validated config; we just wire up the client.
    /// What: Constructs from `GithubConfig` after env-var fallback applied.
    /// Test: covered by `client.rs` construction tests.
    pub fn new(cfg: GithubConfig) -> Result<Self> {
        let token = cfg
            .token
            .ok_or_else(|| anyhow!("github: missing token (set GITHUB_TOKEN)"))?;
        let owner = cfg
            .owner
            .ok_or_else(|| anyhow!("github: missing owner (set GITHUB_OWNER)"))?;
        let repo = cfg
            .repo
            .ok_or_else(|| anyhow!("github: missing repo (set GITHUB_REPO)"))?;
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("build github http client")?;
        Ok(Self {
            token,
            owner,
            repo,
            http,
        })
    }

    pub(super) fn rest_url(&self, path: &str) -> String {
        format!("{REST_BASE}{path}")
    }

    pub(super) async fn rest_get(&self, path: &str) -> Result<Value> {
        let resp = self
            .http
            .get(self.rest_url(path))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        ensure_ok(resp).await
    }

    pub(super) async fn rest_post(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self
            .http
            .post(self.rest_url(path))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        ensure_ok(resp).await
    }

    pub(super) async fn rest_patch(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self
            .http
            .patch(self.rest_url(path))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("PATCH {path}"))?;
        ensure_ok(resp).await
    }

    pub(super) async fn rest_delete(&self, path: &str) -> Result<()> {
        let resp = self
            .http
            .delete(self.rest_url(path))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("DELETE {path}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("github DELETE failed: {status}: {text}");
        }
        Ok(())
    }

    pub(super) async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let resp = self
            .http
            .post(GRAPHQL_URL)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .context("github graphql")?;
        let v = ensure_ok(resp).await?;
        if let Some(errors) = v.get("errors") {
            bail!("github graphql errors: {errors}");
        }
        Ok(v)
    }

    pub(super) fn issue_path(&self, number: &str) -> String {
        format!("/repos/{}/{}/issues/{number}", self.owner, self.repo)
    }
}
