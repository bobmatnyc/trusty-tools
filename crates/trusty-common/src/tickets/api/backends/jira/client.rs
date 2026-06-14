//! JIRA backend — HTTP client methods.
//!
//! Why: Separates the low-level HTTP transport from the high-level Backend
//! trait impl so each file stays under the 500-SLOC cap.
//! What: Provides `JiraBackend` constructor and the private REST helper
//! methods (get/post/put/delete) used by the Backend impl.
//! Test: exercised transitively by all Backend impl methods.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use reqwest::Client;
use serde_json::Value;

use crate::tickets::api::config::JiraConfig;

use super::types::{JiraBackend, USER_AGENT, ensure_ok};

impl JiraBackend {
    /// Why: Validate required fields up-front so the dispatcher can fail fast.
    /// What: Constructs from `JiraConfig`. Requires server, email, token,
    ///   and project_key.
    /// Test: covered by `client.rs` tests.
    pub fn new(cfg: JiraConfig) -> Result<Self> {
        let server = cfg
            .server
            .ok_or_else(|| anyhow!("jira: missing server (set JIRA_SERVER)"))?;
        let email = cfg
            .email
            .ok_or_else(|| anyhow!("jira: missing email (set JIRA_EMAIL)"))?;
        let token = cfg
            .api_token
            .ok_or_else(|| anyhow!("jira: missing api_token (set JIRA_API_TOKEN)"))?;
        let project_key = cfg
            .project_key
            .ok_or_else(|| anyhow!("jira: missing project_key (set JIRA_PROJECT_KEY)"))?;
        let creds = format!("{email}:{token}");
        let encoded = base64::engine::general_purpose::STANDARD.encode(creds.as_bytes());
        let auth_header = format!("Basic {encoded}");
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("build jira http client")?;
        Ok(Self {
            server: server.trim_end_matches('/').to_string(),
            auth_header,
            project_key,
            http,
        })
    }

    pub(super) fn url(&self, path: &str) -> String {
        format!("{}/rest/api/3{path}", self.server)
    }

    pub(super) async fn get(&self, path: &str) -> Result<Value> {
        let resp = self
            .http
            .get(self.url(path))
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        ensure_ok(resp).await
    }

    pub(super) async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self
            .http
            .post(self.url(path))
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        ensure_ok(resp).await
    }

    pub(super) async fn put(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self
            .http
            .put(self.url(path))
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("PUT {path}"))?;
        ensure_ok(resp).await
    }

    pub(super) async fn delete(&self, path: &str) -> Result<()> {
        let resp = self
            .http
            .delete(self.url(path))
            .header("Authorization", &self.auth_header)
            .send()
            .await
            .with_context(|| format!("DELETE {path}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("jira DELETE failed: {status}: {text}");
        }
        Ok(())
    }
}
