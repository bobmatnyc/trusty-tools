//! JIRA Cloud backend (REST API v3).
//!
//! Why: JIRA is the enterprise default; the v3 API uses ADF for prose.
//! What: Basic-auth (email + API token), JQL for queries, Versions for
//! milestones.
//! Test: shape tests in this module; live tests gated by env vars.

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde_json::{Value, json};

use crate::tickets::api::backends::{
    Backend, CreateIssueParams, CreateMilestoneParams, ListIssuesParams, SearchIssuesParams,
    UpdateIssueParams,
};
use crate::tickets::api::config::JiraConfig;
use crate::tickets::api::models::*;

const USER_AGENT: &str = "trusty-tickets/0.1";

/// JIRA Cloud backend.
///
/// Why: Stores server URL + creds + project key.
/// What: Builds basic-auth header once.
/// Test: `tests::parse_adf_text`.
pub struct JiraBackend {
    server: String,
    auth_header: String,
    project_key: String,
    http: Client,
}

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

    fn url(&self, path: &str) -> String {
        format!("{}/rest/api/3{path}", self.server)
    }

    async fn get(&self, path: &str) -> Result<Value> {
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

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
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

    async fn put(&self, path: &str, body: Value) -> Result<Value> {
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

    async fn delete(&self, path: &str) -> Result<()> {
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

async fn ensure_ok(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let text = resp.text().await.context("read body")?;
    if !status.is_success() {
        bail!("jira API failed: {status}: {text}");
    }
    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("parse json: {text}"))
}

/// Wrap a plain-text string in a minimal ADF (Atlassian Document Format)
/// paragraph.
fn adf_paragraph(text: &str) -> Value {
    json!({
        "type": "doc",
        "version": 1,
        "content": [{
            "type": "paragraph",
            "content": [{"type": "text", "text": text}]
        }]
    })
}

/// Best-effort flatten of an ADF body into a plain string.
fn flatten_adf(doc: &Value) -> Option<String> {
    if doc.is_null() {
        return None;
    }
    let mut out = String::new();
    fn walk(v: &Value, out: &mut String) {
        if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
            out.push_str(t);
        }
        if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
            for c in arr {
                walk(c, out);
            }
            if v.get("type")
                .and_then(|t| t.as_str())
                .map(|s| s == "paragraph")
                .unwrap_or(false)
            {
                out.push('\n');
            }
        }
    }
    walk(doc, &mut out);
    if out.is_empty() { None } else { Some(out) }
}

fn map_state(category: &str) -> IssueState {
    match category {
        "Done" | "done" => IssueState::Done,
        "In Progress" | "indeterminate" => IssueState::InProgress,
        _ => IssueState::Open,
    }
}

fn parse_issue(raw: &Value) -> Issue {
    let key = raw
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let fields = raw.get("fields").cloned().unwrap_or(json!({}));
    let title = fields
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = fields.get("description").and_then(flatten_adf);
    let state_cat = fields
        .get("status")
        .and_then(|s| s.get("statusCategory"))
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("To Do");
    let state = map_state(state_cat);
    let assignee = fields
        .get("assignee")
        .and_then(|a| a.get("displayName"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let priority_name = fields
        .get("priority")
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());
    let priority = priority_name.and_then(|s| match s.as_str() {
        "lowest" | "low" => Some(Priority::Low),
        "medium" => Some(Priority::Medium),
        "high" => Some(Priority::High),
        "highest" | "critical" => Some(Priority::Critical),
        _ => None,
    });
    let labels = fields
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let issuetype = fields
        .get("issuetype")
        .and_then(|t| t.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Task")
        .to_lowercase();
    let issue_type = match issuetype.as_str() {
        "epic" => IssueType::Epic,
        "sub-task" | "subtask" => IssueType::Subtask,
        "task" => IssueType::Task,
        _ => IssueType::Issue,
    };
    let project_id = fields
        .get("project")
        .and_then(|p| p.get("key"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let created_at = fields
        .get("created")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    let updated_at = fields
        .get("updated")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));

    Issue {
        id: key,
        backend: "jira".into(),
        url: None,
        title,
        description,
        state,
        issue_type,
        priority,
        assignee,
        labels,
        milestone_id: None,
        milestone_name: None,
        project_id,
        project_name: None,
        parent_id: None,
        children: vec![],
        created_at,
        updated_at,
        extra: raw.clone(),
    }
}

fn parse_comment(issue_id: &str, raw: &Value) -> Comment {
    Comment {
        id: raw
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        issue_id: issue_id.to_string(),
        author: raw
            .get("author")
            .and_then(|a| a.get("displayName"))
            .and_then(|v| v.as_str())
            .map(String::from),
        body: raw.get("body").and_then(flatten_adf).unwrap_or_default(),
        created_at: raw
            .get("created")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        updated_at: raw
            .get("updated")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
    }
}

#[async_trait]
impl Backend for JiraBackend {
    fn name(&self) -> &'static str {
        "jira"
    }

    async fn create_issue(&self, p: CreateIssueParams) -> Result<Issue> {
        let issuetype = match p.issue_type.as_deref() {
            Some("epic") => "Epic",
            Some("task") => "Task",
            Some("subtask") => "Sub-task",
            _ => "Story",
        };
        let mut fields = json!({
            "project": { "key": self.project_key },
            "summary": p.title,
            "issuetype": { "name": issuetype },
        });
        if let Some(d) = p.description {
            fields["description"] = adf_paragraph(&d);
        }
        if let Some(a) = p.assignee {
            fields["assignee"] = json!({ "name": a });
        }
        if let Some(pri) = p.priority {
            let name = match pri.as_str() {
                "low" => "Low",
                "high" => "High",
                "critical" => "Highest",
                _ => "Medium",
            };
            fields["priority"] = json!({ "name": name });
        }
        if !p.labels.is_empty() {
            fields["labels"] = json!(p.labels);
        }
        let v = self.post("/issue", json!({ "fields": fields })).await?;
        // Returned body has key but no fields; re-fetch.
        let key = v
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("jira: create_issue response missing key"))?
            .to_string();
        self.get_issue(&key).await
    }

    async fn get_issue(&self, id: &str) -> Result<Issue> {
        let v = self.get(&format!("/issue/{id}")).await?;
        Ok(parse_issue(&v))
    }

    async fn update_issue(&self, id: &str, p: UpdateIssueParams) -> Result<Issue> {
        let mut fields = json!({});
        if let Some(t) = p.title {
            fields["summary"] = json!(t);
        }
        if let Some(d) = p.description {
            fields["description"] = adf_paragraph(&d);
        }
        if let Some(a) = p.assignee {
            fields["assignee"] = json!({ "name": a });
        }
        if let Some(labels) = p.labels {
            fields["labels"] = json!(labels);
        }
        if let Some(pri) = p.priority {
            let name = match pri.as_str() {
                "low" => "Low",
                "high" => "High",
                "critical" => "Highest",
                _ => "Medium",
            };
            fields["priority"] = json!({ "name": name });
        }
        if fields.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            self.put(&format!("/issue/{id}"), json!({ "fields": fields }))
                .await?;
        }
        if let Some(state) = p.state {
            self.transition_issue(id, &state).await?;
        }
        self.get_issue(id).await
    }

    async fn close_issue(&self, id: &str, comment: Option<&str>) -> Result<Issue> {
        if let Some(c) = comment {
            self.add_comment(id, c).await?;
        }
        self.transition_issue(id, "Done").await
    }

    async fn reopen_issue(&self, id: &str) -> Result<Issue> {
        self.transition_issue(id, "To Do").await
    }

    async fn list_issues(&self, p: ListIssuesParams) -> Result<Vec<Issue>> {
        let mut jql = format!("project = \"{}\"", self.project_key);
        if let Some(s) = &p.state {
            let cat = match s.as_str() {
                "done" | "closed" => "Done",
                "in_progress" => "In Progress",
                _ => "To Do",
            };
            jql.push_str(&format!(" AND statusCategory = \"{cat}\""));
        }
        if let Some(a) = &p.assignee {
            jql.push_str(&format!(" AND assignee = \"{a}\""));
        }
        for l in &p.labels {
            jql.push_str(&format!(" AND labels = \"{l}\""));
        }
        jql.push_str(" ORDER BY created DESC");
        let body = json!({
            "jql": jql,
            "maxResults": p.limit.max(1),
            "startAt": p.offset,
        });
        let v = self.post("/search/jql", body).await?;
        let issues = v
            .get("issues")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(issues.iter().map(parse_issue).collect())
    }

    async fn search_issues(&self, p: SearchIssuesParams) -> Result<Vec<Issue>> {
        let mut jql = format!("project = \"{}\"", self.project_key);
        if let Some(q) = &p.query {
            jql.push_str(&format!(" AND text ~ \"{q}\""));
        }
        if let Some(s) = &p.state {
            let cat = match s.as_str() {
                "done" | "closed" => "Done",
                "in_progress" => "In Progress",
                _ => "To Do",
            };
            jql.push_str(&format!(" AND statusCategory = \"{cat}\""));
        }
        if let Some(a) = &p.assignee {
            jql.push_str(&format!(" AND assignee = \"{a}\""));
        }
        for l in &p.labels {
            jql.push_str(&format!(" AND labels = \"{l}\""));
        }
        if let Some(pri) = &p.priority {
            jql.push_str(&format!(" AND priority = \"{pri}\""));
        }
        let body = json!({
            "jql": jql,
            "maxResults": p.limit.max(1),
            "startAt": p.offset,
        });
        let v = self.post("/search/jql", body).await?;
        let issues = v
            .get("issues")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(issues.iter().map(parse_issue).collect())
    }

    async fn add_comment(&self, issue_id: &str, body: &str) -> Result<Comment> {
        let v = self
            .post(
                &format!("/issue/{issue_id}/comment"),
                json!({ "body": adf_paragraph(body) }),
            )
            .await?;
        Ok(parse_comment(issue_id, &v))
    }

    async fn list_comments(&self, issue_id: &str) -> Result<Vec<Comment>> {
        let v = self.get(&format!("/issue/{issue_id}/comment")).await?;
        let arr = v
            .get("comments")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(arr.iter().map(|r| parse_comment(issue_id, r)).collect())
    }

    async fn update_comment(
        &self,
        issue_id: &str,
        comment_id: &str,
        body: &str,
    ) -> Result<Comment> {
        let v = self
            .put(
                &format!("/issue/{issue_id}/comment/{comment_id}"),
                json!({ "body": adf_paragraph(body) }),
            )
            .await?;
        Ok(parse_comment(issue_id, &v))
    }

    async fn delete_comment(&self, issue_id: &str, comment_id: &str) -> Result<()> {
        self.delete(&format!("/issue/{issue_id}/comment/{comment_id}"))
            .await
    }

    async fn list_labels(&self) -> Result<Vec<Label>> {
        let v = self.get("/label").await?;
        let arr = v
            .get("values")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .iter()
            .filter_map(|x| x.as_str())
            .map(|s| Label {
                id: s.to_string(),
                name: s.to_string(),
                color: None,
                description: None,
            })
            .collect())
    }

    async fn create_label(
        &self,
        _name: &str,
        _color: Option<&str>,
        _description: Option<&str>,
    ) -> Result<Label> {
        bail!("jira: labels are created implicitly when assigned to an issue")
    }

    async fn add_labels(&self, issue_id: &str, labels: &[String]) -> Result<()> {
        let updates: Vec<Value> = labels.iter().map(|l| json!({ "add": l })).collect();
        self.put(
            &format!("/issue/{issue_id}"),
            json!({ "update": { "labels": updates } }),
        )
        .await?;
        Ok(())
    }

    async fn remove_labels(&self, issue_id: &str, labels: &[String]) -> Result<()> {
        let updates: Vec<Value> = labels.iter().map(|l| json!({ "remove": l })).collect();
        self.put(
            &format!("/issue/{issue_id}"),
            json!({ "update": { "labels": updates } }),
        )
        .await?;
        Ok(())
    }

    async fn list_milestones(&self) -> Result<Vec<Milestone>> {
        let v = self
            .get(&format!("/project/{}/versions", self.project_key))
            .await?;
        let arr = v.as_array().cloned().unwrap_or_default();
        Ok(arr
            .iter()
            .map(|r| Milestone {
                id: r
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                name: r
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: r
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                state: if r.get("released").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "released".into()
                } else {
                    "open".into()
                },
                due_date: r
                    .get("releaseDate")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                total_issues: None,
                closed_issues: None,
                progress_pct: None,
            })
            .collect())
    }

    async fn create_milestone(&self, p: CreateMilestoneParams) -> Result<Milestone> {
        let mut body = json!({
            "name": p.name,
            "project": self.project_key,
        });
        if let Some(d) = p.description {
            body["description"] = json!(d);
        }
        if let Some(due) = p.due_date {
            body["releaseDate"] = json!(due);
        }
        let v = self.post("/version", body).await?;
        Ok(Milestone {
            id: v
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            name: v
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            description: v
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
            state: "open".into(),
            due_date: None,
            total_issues: None,
            closed_issues: None,
            progress_pct: None,
        })
    }

    async fn close_milestone(&self, id: &str) -> Result<Milestone> {
        let v = self
            .put(&format!("/version/{id}"), json!({ "released": true }))
            .await?;
        Ok(Milestone {
            id: id.to_string(),
            name: v
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            description: None,
            state: "released".into(),
            due_date: None,
            total_issues: None,
            closed_issues: None,
            progress_pct: None,
        })
    }

    async fn get_milestone_issues(&self, id: &str) -> Result<Vec<Issue>> {
        let jql = format!("fixVersion = {id}");
        let body = json!({ "jql": jql, "maxResults": 100 });
        let v = self.post("/search/jql", body).await?;
        let issues = v
            .get("issues")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(issues.iter().map(parse_issue).collect())
    }

    async fn list_projects(&self) -> Result<Vec<Project>> {
        let v = self.get("/project").await?;
        let arr = v.as_array().cloned().unwrap_or_default();
        Ok(arr
            .iter()
            .map(|r| Project {
                id: r
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                name: r
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: None,
                state: "active".into(),
                url: r.get("self").and_then(|v| v.as_str()).map(String::from),
                team_name: None,
            })
            .collect())
    }

    async fn get_project(&self, id: &str) -> Result<Project> {
        let v = self.get(&format!("/project/{id}")).await?;
        Ok(Project {
            id: v
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            name: v
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            description: v
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
            state: "active".into(),
            url: v.get("self").and_then(|v| v.as_str()).map(String::from),
            team_name: None,
        })
    }

    async fn list_epics(&self) -> Result<Vec<Issue>> {
        let jql = format!("project = \"{}\" AND issuetype = Epic", self.project_key);
        let v = self
            .post("/search/jql", json!({ "jql": jql, "maxResults": 100 }))
            .await?;
        let issues = v
            .get("issues")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(issues.iter().map(parse_issue).collect())
    }

    async fn get_epic_issues(&self, epic_id: &str) -> Result<Vec<Issue>> {
        let jql = format!("parent = {epic_id}");
        let v = self
            .post("/search/jql", json!({ "jql": jql, "maxResults": 100 }))
            .await?;
        let issues = v
            .get("issues")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(issues.iter().map(parse_issue).collect())
    }

    async fn create_project_update(
        &self,
        _project_id: &str,
        _body: &str,
        _health: Option<&str>,
    ) -> Result<ProjectUpdate> {
        bail!("jira: project updates are not supported by the JIRA REST API v3")
    }

    async fn list_project_updates(&self, _project_id: &str) -> Result<Vec<ProjectUpdate>> {
        bail!("jira: project updates are not supported by the JIRA REST API v3")
    }

    async fn list_states(&self) -> Result<Vec<String>> {
        let v = self.get("/status").await?;
        let arr = v.as_array().cloned().unwrap_or_default();
        Ok(arr
            .iter()
            .filter_map(|r| r.get("name").and_then(|v| v.as_str()).map(String::from))
            .collect())
    }

    async fn transition_issue(&self, id: &str, state: &str) -> Result<Issue> {
        // Look up transitions to find one matching the target state name.
        let transitions = self.get(&format!("/issue/{id}/transitions")).await?;
        let arr = transitions
            .get("transitions")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        let want = state.to_lowercase();
        let matched = arr.iter().find(|t| {
            t.get("to")
                .and_then(|to| to.get("name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase() == want)
                .unwrap_or(false)
                || t.get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_lowercase() == want)
                    .unwrap_or(false)
        });
        let tid = matched
            .and_then(|t| t.get("id").and_then(|v| v.as_str()))
            .ok_or_else(|| anyhow!("jira: no transition matches '{state}'"))?;
        self.post(
            &format!("/issue/{id}/transitions"),
            json!({ "transition": { "id": tid } }),
        )
        .await?;
        self.get_issue(id).await
    }

    async fn assign_issue(&self, id: &str, assignee: &str) -> Result<Issue> {
        self.put(
            &format!("/issue/{id}/assignee"),
            json!({ "name": assignee }),
        )
        .await?;
        self.get_issue(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_adf_text() {
        let doc = json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [{"type": "text", "text": "hello"}]
            }]
        });
        assert_eq!(flatten_adf(&doc).as_deref(), Some("hello\n"));
    }

    #[test]
    fn issue_state_mapping() {
        assert_eq!(map_state("Done"), IssueState::Done);
        assert_eq!(map_state("In Progress"), IssueState::InProgress);
        assert_eq!(map_state("To Do"), IssueState::Open);
    }
}
