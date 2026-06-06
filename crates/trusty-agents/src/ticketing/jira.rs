//! JIRA Cloud REST API v3 adapter.
//!
//! Why: JIRA is the dominant enterprise tracker; many projects mandate it.
//! What: `JiraClient` implements `TicketingClient` against
//! `https://<site>.atlassian.net/rest/api/3/...` with Basic(email:token)
//! base64 auth.
//! Test: Construction-time credential checks in `src/ticketing/mod.rs`.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::types::*;
use super::{TicketingClient, TicketingConfig};

/// JIRA Cloud client.
pub struct JiraClient {
    client: reqwest::Client,
    base_url: String,
    project: String,
}

impl JiraClient {
    /// Build a new JIRA client.
    ///
    /// Why: Fail early if any of url/email/token/project are missing.
    /// What: Builds Basic-auth header = `base64(email:token)`; strips any
    /// trailing slash from `base_url`.
    /// Test: `jira_client_new_requires_url` covers missing url.
    pub fn new(config: &TicketingConfig) -> Result<Self> {
        let base_url = config
            .jira_url
            .clone()
            .ok_or_else(|| anyhow!("jira_url required (e.g. https://company.atlassian.net)"))?;
        let email = config
            .jira_email
            .clone()
            .ok_or_else(|| anyhow!("jira_email required"))?;
        let token = config
            .jira_token
            .clone()
            .ok_or_else(|| anyhow!("jira_token required"))?;
        let project = config
            .jira_project
            .clone()
            .ok_or_else(|| anyhow!("jira_project required"))?;

        let base_url = base_url.trim_end_matches('/').to_string();
        let encoded = B64.encode(format!("{email}:{token}"));

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {encoded}"))
                .context("invalid chars in JIRA Basic auth header")?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent("trusty-agents/0.1.0")
            .build()
            .context("failed to build JIRA reqwest client")?;

        Ok(Self {
            client,
            base_url,
            project,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/rest/api/3{}", self.base_url, path)
    }
}

/// Map a JIRA issue JSON payload to our canonical `Ticket`.
fn issue_to_ticket(v: &Value) -> Result<Ticket> {
    let id = v
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("JIRA issue missing 'key'"))?
        .to_string();
    let fields = v.get("fields").cloned().unwrap_or(Value::Null);
    let title = fields
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // ADF body -> we just stringify for now.
    let body = fields
        .get("description")
        .map(|d| {
            d.as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| d.to_string())
        })
        .unwrap_or_default();
    let state_name = fields
        .get("status")
        .and_then(|s| s.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Open")
        .to_lowercase();
    let status = match state_name.as_str() {
        s if s.contains("done") || s.contains("closed") || s.contains("resolved") => {
            TicketStatus::Done
        }
        s if s.contains("progress") => TicketStatus::InProgress,
        _ => TicketStatus::Open,
    };
    let labels: Vec<String> = fields
        .get("labels")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let assignee = fields
        .get("assignee")
        .and_then(|a| {
            a.get("displayName")
                .or_else(|| a.get("emailAddress"))
                .and_then(Value::as_str)
        })
        .map(|s| s.to_string());

    Ok(Ticket {
        id,
        title,
        body,
        status,
        priority: None,
        labels,
        assignee,
        created_at: None,
        updated_at: None,
        url: None,
    })
}

#[async_trait]
impl TicketingClient for JiraClient {
    fn provider_name(&self) -> &str {
        "jira"
    }

    async fn create_ticket(&self, req: CreateTicketReq) -> Result<Ticket> {
        // Build an ADF description: JIRA v3 requires doc-shaped description.
        let description = json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [{"type": "text", "text": req.body}]
            }]
        });
        let mut fields = json!({
            "project": {"key": self.project},
            "summary": req.title,
            "description": description,
            "issuetype": {"name": "Task"},
        });
        if !req.labels.is_empty() {
            fields["labels"] = json!(req.labels);
        }
        let body = json!({ "fields": fields });
        let resp = self
            .client
            .post(self.url("/issue"))
            .json(&body)
            .send()
            .await
            .context("JIRA create_ticket: request failed")?;
        let status = resp.status();
        let v: Value = resp.json().await.context("JIRA create_ticket: parse")?;
        if !status.is_success() {
            anyhow::bail!("JIRA create_ticket HTTP {status}: {v}");
        }
        let key = v
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("JIRA create_ticket: no key in response"))?
            .to_string();
        // Fetch full issue for canonical Ticket.
        self.get_ticket(&key).await
    }

    async fn get_ticket(&self, id: &str) -> Result<Ticket> {
        let resp = self
            .client
            .get(self.url(&format!("/issue/{id}")))
            .send()
            .await
            .context("JIRA get_ticket: request failed")?;
        let status = resp.status();
        let v: Value = resp.json().await.context("JIRA get_ticket: parse")?;
        if !status.is_success() {
            anyhow::bail!("JIRA get_ticket HTTP {status}: {v}");
        }
        issue_to_ticket(&v)
    }

    async fn update_ticket(&self, id: &str, req: UpdateTicketReq) -> Result<Ticket> {
        let mut fields = serde_json::Map::new();
        if let Some(t) = req.title {
            fields.insert("summary".into(), json!(t));
        }
        if let Some(b) = req.body {
            fields.insert(
                "description".into(),
                json!({
                    "type": "doc",
                    "version": 1,
                    "content": [{
                        "type": "paragraph",
                        "content": [{"type": "text", "text": b}]
                    }]
                }),
            );
        }
        if let Some(ls) = req.labels {
            fields.insert("labels".into(), json!(ls));
        }
        let payload = json!({ "fields": fields });
        let resp = self
            .client
            .put(self.url(&format!("/issue/{id}")))
            .json(&payload)
            .send()
            .await
            .context("JIRA update_ticket: request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("JIRA update_ticket HTTP {status}: {text}");
        }
        // Status is handled via transitions; if requested, do it now.
        if let Some(_s) = req.status {
            // Caller can use close_ticket() for final state; other transitions
            // require a transition id we'd need to look up.
        }
        self.get_ticket(id).await
    }

    async fn close_ticket(&self, id: &str) -> Result<()> {
        // Find a "Done"-like transition.
        let resp = self
            .client
            .get(self.url(&format!("/issue/{id}/transitions")))
            .send()
            .await
            .context("JIRA close_ticket: list transitions failed")?;
        let status = resp.status();
        let v: Value = resp.json().await.context("JIRA close_ticket: parse")?;
        if !status.is_success() {
            anyhow::bail!("JIRA close_ticket HTTP {status}: {v}");
        }
        let tid = v
            .get("transitions")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find_map(|t| {
                    let name = t.get("name").and_then(Value::as_str).unwrap_or("");
                    if name.eq_ignore_ascii_case("done")
                        || name.eq_ignore_ascii_case("close")
                        || name.to_lowercase().contains("done")
                        || name.to_lowercase().contains("close")
                    {
                        t.get("id").and_then(Value::as_str).map(String::from)
                    } else {
                        None
                    }
                })
            })
            .ok_or_else(|| anyhow!("JIRA close_ticket: no Done/Close transition available"))?;

        let resp = self
            .client
            .post(self.url(&format!("/issue/{id}/transitions")))
            .json(&json!({ "transition": { "id": tid } }))
            .send()
            .await
            .context("JIRA close_ticket: POST transition failed")?;
        let s = resp.status();
        if !s.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("JIRA close_ticket HTTP {s}: {text}");
        }
        Ok(())
    }

    async fn list_tickets(&self, filter: TicketFilter) -> Result<Vec<Ticket>> {
        let mut jql = format!("project = {}", self.project);
        if let Some(s) = filter.status {
            let jira_state = match s {
                TicketStatus::Open => "Open".to_string(),
                TicketStatus::InProgress => "\"In Progress\"".to_string(),
                TicketStatus::InReview => "\"In Review\"".to_string(),
                TicketStatus::Done => "Done".to_string(),
                TicketStatus::Closed => "Closed".to_string(),
                TicketStatus::Blocked => "Blocked".to_string(),
                TicketStatus::Cancelled => "Cancelled".to_string(),
                TicketStatus::Custom(name) => format!("\"{name}\""),
            };
            jql.push_str(&format!(" AND status = {jira_state}"));
        }
        if let Some(a) = filter.assignee {
            jql.push_str(&format!(" AND assignee = \"{a}\""));
        }
        if !filter.labels.is_empty() {
            for l in &filter.labels {
                jql.push_str(&format!(" AND labels = \"{l}\""));
            }
        }
        let max = filter.limit.unwrap_or(50);
        let resp = self
            .client
            .get(self.url("/search"))
            .query(&[("jql", jql.as_str()), ("maxResults", &max.to_string())])
            .send()
            .await
            .context("JIRA list_tickets: request failed")?;
        let status = resp.status();
        let v: Value = resp.json().await.context("JIRA list_tickets: parse")?;
        if !status.is_success() {
            anyhow::bail!("JIRA list_tickets HTTP {status}: {v}");
        }
        let arr = v
            .get("issues")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("JIRA list_tickets: missing 'issues' array"))?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            out.push(issue_to_ticket(item)?);
        }
        Ok(out)
    }

    async fn add_comment(&self, id: &str, body: &str) -> Result<()> {
        let payload = json!({
            "body": {
                "type": "doc",
                "version": 1,
                "content": [{
                    "type": "paragraph",
                    "content": [{"type": "text", "text": body}]
                }]
            }
        });
        let resp = self
            .client
            .post(self.url(&format!("/issue/{id}/comment")))
            .json(&payload)
            .send()
            .await
            .context("JIRA add_comment: request failed")?;
        let s = resp.status();
        if !s.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("JIRA add_comment HTTP {s}: {text}");
        }
        Ok(())
    }
}
