//! Linear GraphQL adapter.
//!
//! Why: Linear is GraphQL-only — unlike GitHub/JIRA, there's no REST surface
//! we can share. A small helper (`graphql_query`) encapsulates the
//! `POST https://api.linear.app/graphql` pattern.
//! What: `LinearClient` implements `TicketingClient` by composing GraphQL
//! mutations/queries; `team_id` is optional (used when creating issues).
//! Test: Construction test in `src/ticketing/mod.rs` checks for api_key.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::types::*;
use super::{TicketingClient, TicketingConfig};

const LINEAR_ENDPOINT: &str = "https://api.linear.app/graphql";

pub struct LinearClient {
    client: reqwest::Client,
    team_id: Option<String>,
}

impl LinearClient {
    /// Build a new Linear client.
    ///
    /// Why: Fail fast when api_key is missing.
    /// What: Sets `Authorization: <api_key>` (Linear wants the raw key, no
    /// Bearer prefix) and JSON content-type.
    /// Test: `linear_client_new_requires_api_key`.
    pub fn new(config: &TicketingConfig) -> Result<Self> {
        let api_key = config
            .linear_api_key
            .clone()
            .ok_or_else(|| anyhow!("linear_api_key required"))?;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&api_key).context("invalid chars in Linear API key")?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent("trusty-agents/0.1.0")
            .build()
            .context("failed to build Linear reqwest client")?;

        Ok(Self {
            client,
            team_id: config.linear_team_id.clone(),
        })
    }

    /// POST a GraphQL query/mutation and return the `data` object.
    ///
    /// Why: Avoids pulling in a full GraphQL crate for ~5 operations.
    /// What: Sends `{query, variables}`; errors if HTTP fails or response has
    /// a top-level `errors` array.
    /// Test: Covered indirectly by the TicketingClient impl tests.
    async fn graphql_query(&self, query: &str, variables: Value) -> Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        let resp = self
            .client
            .post(LINEAR_ENDPOINT)
            .json(&body)
            .send()
            .await
            .context("Linear: GraphQL request failed")?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .context("Linear: parse GraphQL response")?;
        if !status.is_success() {
            anyhow::bail!("Linear HTTP {status}: {v}");
        }
        if let Some(errs) = v.get("errors")
            && !errs.as_array().map(|a| a.is_empty()).unwrap_or(true)
        {
            anyhow::bail!("Linear GraphQL errors: {errs}");
        }
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    }
}

/// Map a Linear issue JSON node to our canonical `Ticket`.
fn issue_to_ticket(v: &Value) -> Result<Ticket> {
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Linear issue missing 'id'"))?
        .to_string();
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let body = v
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let state_name = v
        .get("state")
        .and_then(|s| s.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Backlog")
        .to_lowercase();
    let status = match state_name.as_str() {
        s if s.contains("done") || s.contains("completed") => TicketStatus::Done,
        s if s.contains("cancel") || s.contains("closed") => TicketStatus::Closed,
        s if s.contains("progress") || s.contains("started") => TicketStatus::InProgress,
        _ => TicketStatus::Open,
    };
    let labels: Vec<String> = v
        .get("labels")
        .and_then(|l| l.get("nodes"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n.get("name").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let assignee = v
        .get("assignee")
        .and_then(|a| a.get("name").and_then(Value::as_str))
        .map(String::from);
    let url = v.get("url").and_then(Value::as_str).map(String::from);

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
        url,
    })
}

#[async_trait]
impl TicketingClient for LinearClient {
    fn provider_name(&self) -> &str {
        "linear"
    }

    async fn create_ticket(&self, req: CreateTicketReq) -> Result<Ticket> {
        let team_id = self
            .team_id
            .clone()
            .ok_or_else(|| anyhow!("linear_team_id required to create issues"))?;
        let q = r#"
            mutation IssueCreate($input: IssueCreateInput!) {
                issueCreate(input: $input) {
                    success
                    issue {
                        id title description url
                        state { name }
                        labels { nodes { name } }
                        assignee { name }
                    }
                }
            }
        "#;
        let mut input = json!({
            "teamId": team_id,
            "title": req.title,
            "description": req.body,
        });
        if !req.labels.is_empty() {
            // Linear requires label *ids*, not names; callers must pass ids.
            input["labelIds"] = json!(req.labels);
        }
        let data = self.graphql_query(q, json!({ "input": input })).await?;
        let issue = data
            .get("issueCreate")
            .and_then(|c| c.get("issue"))
            .ok_or_else(|| anyhow!("Linear create_ticket: missing issue in response"))?;
        issue_to_ticket(issue)
    }

    async fn get_ticket(&self, id: &str) -> Result<Ticket> {
        let q = r#"
            query Issue($id: String!) {
                issue(id: $id) {
                    id title description url
                    state { name }
                    labels { nodes { name } }
                    assignee { name }
                }
            }
        "#;
        let data = self.graphql_query(q, json!({ "id": id })).await?;
        let issue = data
            .get("issue")
            .ok_or_else(|| anyhow!("Linear get_ticket: missing issue"))?;
        issue_to_ticket(issue)
    }

    async fn update_ticket(&self, id: &str, req: UpdateTicketReq) -> Result<Ticket> {
        let q = r#"
            mutation IssueUpdate($id: String!, $input: IssueUpdateInput!) {
                issueUpdate(id: $id, input: $input) {
                    success
                    issue {
                        id title description url
                        state { name }
                        labels { nodes { name } }
                        assignee { name }
                    }
                }
            }
        "#;
        let mut input = serde_json::Map::new();
        if let Some(t) = req.title {
            input.insert("title".into(), json!(t));
        }
        if let Some(b) = req.body {
            input.insert("description".into(), json!(b));
        }
        if let Some(ls) = req.labels {
            input.insert("labelIds".into(), json!(ls));
        }
        let data = self
            .graphql_query(q, json!({ "id": id, "input": Value::Object(input) }))
            .await?;
        let issue = data
            .get("issueUpdate")
            .and_then(|c| c.get("issue"))
            .ok_or_else(|| anyhow!("Linear update_ticket: missing issue"))?;
        issue_to_ticket(issue)
    }

    async fn close_ticket(&self, id: &str) -> Result<()> {
        // Linear closes issues by moving to a "completed"/"canceled" state,
        // which requires the target state id. As a pragmatic default, we
        // archive the issue (marks it closed in the UI).
        let q = r#"
            mutation IssueArchive($id: String!) {
                issueArchive(id: $id) { success }
            }
        "#;
        let data = self.graphql_query(q, json!({ "id": id })).await?;
        let ok = data
            .get("issueArchive")
            .and_then(|c| c.get("success"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !ok {
            anyhow::bail!("Linear close_ticket: archive returned success=false");
        }
        Ok(())
    }

    async fn list_tickets(&self, filter: TicketFilter) -> Result<Vec<Ticket>> {
        let q = r#"
            query Issues($filter: IssueFilter, $first: Int) {
                issues(filter: $filter, first: $first) {
                    nodes {
                        id title description url
                        state { name }
                        labels { nodes { name } }
                        assignee { name }
                    }
                }
            }
        "#;
        let mut f = serde_json::Map::new();
        if let Some(s) = filter.status {
            let name = match s {
                TicketStatus::Open => "Backlog".to_string(),
                TicketStatus::InProgress => "In Progress".to_string(),
                TicketStatus::InReview => "In Review".to_string(),
                TicketStatus::Done => "Done".to_string(),
                TicketStatus::Closed => "Canceled".to_string(),
                TicketStatus::Blocked => "Blocked".to_string(),
                TicketStatus::Cancelled => "Canceled".to_string(),
                TicketStatus::Custom(n) => n,
            };
            f.insert("state".into(), json!({ "name": { "eq": name } }));
        }
        let variables = json!({
            "filter": Value::Object(f),
            "first": filter.limit.unwrap_or(50) as i64,
        });
        let data = self.graphql_query(q, variables).await?;
        let nodes = data
            .get("issues")
            .and_then(|i| i.get("nodes"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(nodes.len());
        for n in &nodes {
            out.push(issue_to_ticket(n)?);
        }
        Ok(out)
    }

    async fn add_comment(&self, id: &str, body: &str) -> Result<()> {
        let q = r#"
            mutation CommentCreate($input: CommentCreateInput!) {
                commentCreate(input: $input) { success }
            }
        "#;
        let data = self
            .graphql_query(q, json!({ "input": { "issueId": id, "body": body } }))
            .await?;
        let ok = data
            .get("commentCreate")
            .and_then(|c| c.get("success"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !ok {
            anyhow::bail!("Linear add_comment: returned success=false");
        }
        Ok(())
    }
}
