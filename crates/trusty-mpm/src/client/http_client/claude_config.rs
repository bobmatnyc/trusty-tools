//! Claude Code config-analyzer methods for [`DaemonClient`].
//!
//! Why: the `/config` command family (analyze, apply a recommendation, list
//! checkpoints, deploy a built-in profile) is one cohesive concern against the
//! daemon's `/claude-config*` routes. It lives in a sibling file (not
//! `mod.rs`) so the client `mod.rs` stays under the 500-SLOC production cap
//! (issue #2471) — the same reason [`super::managed`] and [`super::projects`]
//! already live in their own files; this file can reach the `base`/`http`
//! fields because they are `pub(in crate::client::http_client)`.
//! What: one async method per config-analyzer endpoint, each building the URL
//! from [`DaemonClient::base`], sending via the shared `reqwest::Client`, and
//! deserializing the response.
//! Test: covered by the executor's config test and the daemon's claude-config
//! tests (live HTTP); no wire-shape unit test lives here today.

use super::DaemonClient;
use super::types::ConfigRecommendation;

impl DaemonClient {
    /// Analyze a project's Claude Code config via `GET /claude-config`.
    ///
    /// Why: the `/config` command surfaces analyzer recommendations.
    /// What: `GET /claude-config?project=<path>`, returns one
    /// [`ConfigRecommendation`] per recommendation.
    /// Test: covered by the executor's config test.
    pub async fn analyze_config(&self, project: &str) -> anyhow::Result<Vec<ConfigRecommendation>> {
        let url = format!("{}/claude-config", self.base);
        let body: serde_json::Value = self
            .http
            .get(&url)
            .query(&[("project", project)])
            .send()
            .await?
            .json()
            .await?;
        let recs = body["recommendations"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(recs
            .iter()
            .map(|r| ConfigRecommendation {
                id: r
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                message: r
                    .get("message")
                    .and_then(|v| v.as_str())
                    .or_else(|| r.as_str())
                    .unwrap_or("?")
                    .to_string(),
            })
            .collect())
    }

    /// Apply a config recommendation via `POST /claude-config/apply`.
    ///
    /// Why: lets a UI act on a recommendation without hand-editing JSON.
    /// What: POSTs the project path and recommendation id; returns the
    /// checkpoint id on success.
    /// Test: covered by the daemon's claude-config tests.
    pub async fn apply_recommendation(
        &self,
        project: &str,
        recommendation_id: &str,
    ) -> anyhow::Result<String> {
        let url = format!("{}/claude-config/apply", self.base);
        let body: serde_json::Value = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "project": project,
                "recommendation_id": recommendation_id,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body
            .get("checkpoint_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// List a project's config checkpoints via `GET /claude-config/checkpoints`.
    ///
    /// Why: a UI offers a restore picker fed by this list.
    /// What: returns the raw checkpoint JSON array.
    /// Test: covered by the daemon's claude-config tests.
    pub async fn list_checkpoints(&self, project: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let url = format!("{}/claude-config/checkpoints", self.base);
        let body: serde_json::Value = self
            .http
            .get(&url)
            .query(&[("project", project)])
            .send()
            .await?
            .json()
            .await?;
        Ok(body["checkpoints"].as_array().cloned().unwrap_or_default())
    }

    /// Deploy a built-in profile via `POST /claude-config/deploy`.
    ///
    /// Why: lets a UI apply a configuration preset in one call.
    /// What: POSTs the project path and profile name; returns the checkpoint id.
    /// Test: covered by the daemon's claude-config tests.
    pub async fn deploy_profile(
        &self,
        project: &str,
        profile_name: &str,
    ) -> anyhow::Result<String> {
        let url = format!("{}/claude-config/deploy", self.base);
        let body: serde_json::Value = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "project": project,
                "profile_name": profile_name,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body
            .get("checkpoint_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }
}
