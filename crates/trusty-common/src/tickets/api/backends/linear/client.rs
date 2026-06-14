//! Linear backend — HTTP client methods.
//!
//! Why: Separates the low-level GraphQL transport from the high-level Backend
//! trait impl so each file stays under the 500-SLOC cap.
//! What: Provides `LinearBackend` constructor, the `graphql` helper, and
//! `resolve_team_id` (lazy team key → ID resolution with caching).
//! Test: `tests::requires_api_key` (in `backend` submodule).

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Client;
use serde_json::{Value, json};

use crate::tickets::api::config::LinearConfig;

use super::types::{GRAPHQL_URL, LinearBackend, USER_AGENT};

impl LinearBackend {
    /// Why: Accept either `team_id` or `team_key`; key is resolved later.
    /// What: Construct + validate API key presence.
    /// Test: `tests::requires_api_key`.
    pub fn new(cfg: LinearConfig) -> Result<Self> {
        let api_key = cfg
            .api_key
            .ok_or_else(|| anyhow!("linear: missing api_key (set LINEAR_API_KEY)"))?;
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("build linear http client")?;
        Ok(Self {
            api_key,
            team_key: cfg.team_key,
            team_id: std::sync::Mutex::new(cfg.team_id),
            http,
        })
    }

    pub(super) async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let resp = self
            .http
            .post(GRAPHQL_URL)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .context("linear graphql")?;
        let status = resp.status();
        let text = resp.text().await.context("read body")?;
        if !status.is_success() {
            bail!("linear API failed: {status}: {text}");
        }
        let v: Value =
            serde_json::from_str(&text).with_context(|| format!("parse json: {text}"))?;
        if let Some(errors) = v.get("errors") {
            bail!("linear graphql errors: {errors}");
        }
        Ok(v)
    }

    pub(super) async fn resolve_team_id(&self) -> Result<String> {
        {
            let g = self.team_id.lock().unwrap();
            if let Some(t) = &*g {
                return Ok(t.clone());
            }
        }
        let key = self
            .team_key
            .clone()
            .ok_or_else(|| anyhow!("linear: missing team_key/team_id"))?;
        let q = "query($key: String!) { team(id: $key) { id name key } }";
        // Linear's `team(id:)` actually accepts the team key too; if that
        // fails we fall back to scanning teams.
        let mut id_opt = None;
        if let Ok(v) = self.graphql(q, json!({ "key": key })).await
            && let Some(id) = v["data"]["team"]["id"].as_str()
        {
            id_opt = Some(id.to_string());
        }
        if id_opt.is_none() {
            let q2 = "query { teams(first: 100) { nodes { id key name } } }";
            let v = self.graphql(q2, json!({})).await?;
            let nodes = v["data"]["teams"]["nodes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for n in nodes {
                if n.get("key").and_then(|k| k.as_str()) == Some(&key) {
                    id_opt = n.get("id").and_then(|v| v.as_str()).map(String::from);
                    break;
                }
            }
        }
        let id = id_opt.ok_or_else(|| anyhow!("linear: team '{key}' not found"))?;
        *self.team_id.lock().unwrap() = Some(id.clone());
        Ok(id)
    }
}
