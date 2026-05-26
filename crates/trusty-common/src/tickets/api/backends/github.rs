//! GitHub backend (REST v3 + GraphQL v4).
//!
//! Why: GitHub Issues is the de-facto open-source tracker; we need full
//! CRUD + labels + milestones + Projects V2.
//! What: PAT-authenticated `reqwest` client. REST for issues/comments/
//! labels/milestones; GraphQL for Projects V2.
//! Test: shape tests in this module; live tests gated by env vars.

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde_json::{Value, json};

use crate::tickets::api::backends::{
    Backend, CreateIssueParams, CreateMilestoneParams, ListIssuesParams, SearchIssuesParams,
    UpdateIssueParams,
};
use crate::tickets::api::config::GithubConfig;
use crate::tickets::api::models::*;

const REST_BASE: &str = "https://api.github.com";
const GRAPHQL_URL: &str = "https://api.github.com/graphql";
const USER_AGENT: &str = "trusty-tickets/0.1";

/// GitHub backend implementation.
///
/// Why: Holds the auth token + repo coordinates + HTTP client.
/// What: All requests carry `Authorization: Bearer ...` and the
/// recommended `X-GitHub-Api-Version` header.
/// Test: `tests::parse_issue_minimal` (shape only).
pub struct GitHubBackend {
    token: String,
    owner: String,
    repo: String,
    http: Client,
}

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

    fn rest_url(&self, path: &str) -> String {
        format!("{REST_BASE}{path}")
    }

    async fn rest_get(&self, path: &str) -> Result<Value> {
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

    async fn rest_post(&self, path: &str, body: Value) -> Result<Value> {
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

    async fn rest_patch(&self, path: &str, body: Value) -> Result<Value> {
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

    async fn rest_delete(&self, path: &str) -> Result<()> {
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

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
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

    fn issue_path(&self, number: &str) -> String {
        format!("/repos/{}/{}/issues/{number}", self.owner, self.repo)
    }
}

async fn ensure_ok(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let text = resp.text().await.context("read body")?;
    if !status.is_success() {
        bail!("github API failed: {status}: {text}");
    }
    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("parse json: {text}"))
}

/// Convert a GitHub issue JSON blob into the canonical `Issue`.
fn parse_issue(backend: &GitHubBackend, raw: &Value) -> Issue {
    let number = raw
        .get("number")
        .and_then(|v| v.as_i64())
        .map(|n| n.to_string())
        .unwrap_or_default();
    let state_str = raw.get("state").and_then(|v| v.as_str()).unwrap_or("open");
    let state = match state_str {
        "closed" => IssueState::Closed,
        _ => IssueState::Open,
    };
    let labels: Vec<String> = raw
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let assignee = raw
        .get("assignee")
        .and_then(|v| v.get("login"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let title = raw
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = raw.get("body").and_then(|v| v.as_str()).map(String::from);
    let url = raw
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(String::from);
    let (milestone_id, milestone_name) = raw
        .get("milestone")
        .map(|m| {
            let id = m
                .get("number")
                .and_then(|n| n.as_i64())
                .map(|n| n.to_string());
            let name = m.get("title").and_then(|n| n.as_str()).map(String::from);
            (id, name)
        })
        .unwrap_or((None, None));
    let created_at = raw
        .get("created_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    let updated_at = raw
        .get("updated_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));

    Issue {
        id: number,
        backend: backend.name().to_string(),
        url,
        title,
        description,
        state,
        issue_type: IssueType::Issue,
        priority: None,
        assignee,
        labels,
        milestone_id,
        milestone_name,
        project_id: None,
        project_name: Some(format!("{}/{}", backend.owner, backend.repo)),
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
            .and_then(|v| v.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_default(),
        issue_id: issue_id.to_string(),
        author: raw
            .get("user")
            .and_then(|u| u.get("login"))
            .and_then(|v| v.as_str())
            .map(String::from),
        body: raw
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        created_at: raw
            .get("created_at")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        updated_at: raw
            .get("updated_at")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
    }
}

fn parse_label(raw: &Value) -> Label {
    Label {
        id: raw
            .get("id")
            .and_then(|v| v.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_default(),
        name: raw
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        color: raw.get("color").and_then(|v| v.as_str()).map(String::from),
        description: raw
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

fn parse_milestone(raw: &Value) -> Milestone {
    let total = raw.get("open_issues").and_then(|v| v.as_u64()).unwrap_or(0)
        + raw
            .get("closed_issues")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
    let closed = raw
        .get("closed_issues")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let pct = if total > 0 {
        Some(closed as f64 / total as f64 * 100.0)
    } else {
        None
    };
    Milestone {
        id: raw
            .get("number")
            .and_then(|v| v.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_default(),
        name: raw
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: raw
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
        state: raw
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("open")
            .to_string(),
        due_date: raw
            .get("due_on")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        total_issues: Some(total as u32),
        closed_issues: Some(closed as u32),
        progress_pct: pct,
    }
}

#[async_trait]
impl Backend for GitHubBackend {
    fn name(&self) -> &'static str {
        "github"
    }

    async fn create_issue(&self, p: CreateIssueParams) -> Result<Issue> {
        let mut body = json!({ "title": p.title });
        if let Some(d) = &p.description {
            body["body"] = json!(d);
        }
        if !p.labels.is_empty() {
            body["labels"] = json!(p.labels);
        }
        if let Some(a) = &p.assignee {
            body["assignees"] = json!([a]);
        }
        if let Some(m) = &p.milestone_id
            && let Ok(n) = m.parse::<u64>()
        {
            body["milestone"] = json!(n);
        }
        let v = self
            .rest_post(&format!("/repos/{}/{}/issues", self.owner, self.repo), body)
            .await?;
        Ok(parse_issue(self, &v))
    }

    async fn get_issue(&self, id: &str) -> Result<Issue> {
        let v = self.rest_get(&self.issue_path(id)).await?;
        Ok(parse_issue(self, &v))
    }

    async fn update_issue(&self, id: &str, p: UpdateIssueParams) -> Result<Issue> {
        let mut body = json!({});
        if let Some(t) = p.title {
            body["title"] = json!(t);
        }
        if let Some(d) = p.description {
            body["body"] = json!(d);
        }
        if let Some(labels) = p.labels {
            body["labels"] = json!(labels);
        }
        if let Some(a) = p.assignee {
            body["assignees"] = json!([a]);
        }
        if let Some(m) = p.milestone_id
            && let Ok(n) = m.parse::<u64>()
        {
            body["milestone"] = json!(n);
        }
        if let Some(state) = p.state {
            let s = match state.as_str() {
                "open" | "reopened" => "open",
                _ => "closed",
            };
            body["state"] = json!(s);
        }
        let v = self.rest_patch(&self.issue_path(id), body).await?;
        Ok(parse_issue(self, &v))
    }

    async fn close_issue(&self, id: &str, comment: Option<&str>) -> Result<Issue> {
        if let Some(c) = comment {
            self.add_comment(id, c).await?;
        }
        let v = self
            .rest_patch(&self.issue_path(id), json!({ "state": "closed" }))
            .await?;
        Ok(parse_issue(self, &v))
    }

    async fn reopen_issue(&self, id: &str) -> Result<Issue> {
        let v = self
            .rest_patch(&self.issue_path(id), json!({ "state": "open" }))
            .await?;
        Ok(parse_issue(self, &v))
    }

    async fn list_issues(&self, p: ListIssuesParams) -> Result<Vec<Issue>> {
        let state = p.state.as_deref().unwrap_or("open");
        let state_q = match state {
            "closed" | "done" => "closed",
            "all" => "all",
            _ => "open",
        };
        let mut url = format!(
            "/repos/{}/{}/issues?state={state_q}&per_page={}&page={}",
            self.owner,
            self.repo,
            p.limit.max(1),
            (p.offset / p.limit.max(1)) + 1
        );
        if let Some(a) = &p.assignee {
            url.push_str(&format!("&assignee={a}"));
        }
        if !p.labels.is_empty() {
            url.push_str(&format!("&labels={}", p.labels.join(",")));
        }
        let v = self.rest_get(&url).await?;
        let arr = v.as_array().cloned().unwrap_or_default();
        Ok(arr.iter().map(|r| parse_issue(self, r)).collect())
    }

    async fn search_issues(&self, p: SearchIssuesParams) -> Result<Vec<Issue>> {
        let mut q = format!("repo:{}/{}", self.owner, self.repo);
        if let Some(text) = &p.query {
            q.push(' ');
            q.push_str(text);
        }
        if let Some(s) = &p.state {
            let st = match s.as_str() {
                "closed" | "done" => "closed",
                _ => "open",
            };
            q.push_str(&format!(" state:{st}"));
        }
        if let Some(a) = &p.assignee {
            q.push_str(&format!(" assignee:{a}"));
        }
        for l in &p.labels {
            q.push_str(&format!(" label:\"{l}\""));
        }
        let encoded = urlencode(&q);
        let url = format!(
            "/search/issues?q={encoded}&per_page={}&page={}",
            p.limit.max(1),
            (p.offset / p.limit.max(1)) + 1
        );
        let v = self.rest_get(&url).await?;
        let items = v
            .get("items")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(items.iter().map(|r| parse_issue(self, r)).collect())
    }

    async fn add_comment(&self, issue_id: &str, body: &str) -> Result<Comment> {
        let path = format!("{}/comments", self.issue_path(issue_id));
        let v = self.rest_post(&path, json!({ "body": body })).await?;
        Ok(parse_comment(issue_id, &v))
    }

    async fn list_comments(&self, issue_id: &str) -> Result<Vec<Comment>> {
        let path = format!("{}/comments", self.issue_path(issue_id));
        let v = self.rest_get(&path).await?;
        let arr = v.as_array().cloned().unwrap_or_default();
        Ok(arr.iter().map(|r| parse_comment(issue_id, r)).collect())
    }

    async fn update_comment(
        &self,
        issue_id: &str,
        comment_id: &str,
        body: &str,
    ) -> Result<Comment> {
        let path = format!(
            "/repos/{}/{}/issues/comments/{}",
            self.owner, self.repo, comment_id
        );
        let v = self.rest_patch(&path, json!({ "body": body })).await?;
        Ok(parse_comment(issue_id, &v))
    }

    async fn delete_comment(&self, _issue_id: &str, comment_id: &str) -> Result<()> {
        let path = format!(
            "/repos/{}/{}/issues/comments/{}",
            self.owner, self.repo, comment_id
        );
        self.rest_delete(&path).await
    }

    async fn list_labels(&self) -> Result<Vec<Label>> {
        let v = self
            .rest_get(&format!("/repos/{}/{}/labels", self.owner, self.repo))
            .await?;
        Ok(v.as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(parse_label)
            .collect())
    }

    async fn create_label(
        &self,
        name: &str,
        color: Option<&str>,
        description: Option<&str>,
    ) -> Result<Label> {
        let mut body = json!({ "name": name });
        if let Some(c) = color {
            body["color"] = json!(c);
        }
        if let Some(d) = description {
            body["description"] = json!(d);
        }
        let v = self
            .rest_post(&format!("/repos/{}/{}/labels", self.owner, self.repo), body)
            .await?;
        Ok(parse_label(&v))
    }

    async fn add_labels(&self, issue_id: &str, labels: &[String]) -> Result<()> {
        let path = format!("{}/labels", self.issue_path(issue_id));
        self.rest_post(&path, json!({ "labels": labels })).await?;
        Ok(())
    }

    async fn remove_labels(&self, issue_id: &str, labels: &[String]) -> Result<()> {
        for l in labels {
            let path = format!("{}/labels/{}", self.issue_path(issue_id), urlencode(l));
            // Per GitHub API spec, removing one label at a time.
            self.rest_delete(&path).await?;
        }
        Ok(())
    }

    async fn list_milestones(&self) -> Result<Vec<Milestone>> {
        let v = self
            .rest_get(&format!(
                "/repos/{}/{}/milestones?state=all",
                self.owner, self.repo
            ))
            .await?;
        Ok(v.as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(parse_milestone)
            .collect())
    }

    async fn create_milestone(&self, p: CreateMilestoneParams) -> Result<Milestone> {
        let mut body = json!({ "title": p.name });
        if let Some(d) = p.description {
            body["description"] = json!(d);
        }
        if let Some(due) = p.due_date {
            body["due_on"] = json!(due);
        }
        let v = self
            .rest_post(
                &format!("/repos/{}/{}/milestones", self.owner, self.repo),
                body,
            )
            .await?;
        Ok(parse_milestone(&v))
    }

    async fn close_milestone(&self, id: &str) -> Result<Milestone> {
        let v = self
            .rest_patch(
                &format!("/repos/{}/{}/milestones/{}", self.owner, self.repo, id),
                json!({ "state": "closed" }),
            )
            .await?;
        Ok(parse_milestone(&v))
    }

    async fn get_milestone_issues(&self, id: &str) -> Result<Vec<Issue>> {
        let url = format!(
            "/repos/{}/{}/issues?milestone={id}&state=all&per_page=100",
            self.owner, self.repo
        );
        let v = self.rest_get(&url).await?;
        Ok(v.as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| parse_issue(self, r))
            .collect())
    }

    async fn list_projects(&self) -> Result<Vec<Project>> {
        let q = r#"
            query($owner: String!) {
              repositoryOwner(login: $owner) {
                ... on ProjectV2Owner {
                  projectsV2(first: 50) {
                    nodes { id title number url closed }
                  }
                }
              }
            }
        "#;
        let v = self.graphql(q, json!({ "owner": self.owner })).await?;
        let nodes = v["data"]["repositoryOwner"]["projectsV2"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes
            .iter()
            .map(|n| Project {
                id: n["id"].as_str().unwrap_or("").to_string(),
                name: n["title"].as_str().unwrap_or("").to_string(),
                description: None,
                state: if n["closed"].as_bool().unwrap_or(false) {
                    "closed".into()
                } else {
                    "open".into()
                },
                url: n["url"].as_str().map(String::from),
                team_name: Some(self.owner.clone()),
            })
            .collect())
    }

    async fn get_project(&self, id: &str) -> Result<Project> {
        let q = r#"
            query($id: ID!) {
              node(id: $id) {
                ... on ProjectV2 { id title number url closed }
              }
            }
        "#;
        let v = self.graphql(q, json!({ "id": id })).await?;
        let n = &v["data"]["node"];
        Ok(Project {
            id: n["id"].as_str().unwrap_or("").to_string(),
            name: n["title"].as_str().unwrap_or("").to_string(),
            description: None,
            state: if n["closed"].as_bool().unwrap_or(false) {
                "closed".into()
            } else {
                "open".into()
            },
            url: n["url"].as_str().map(String::from),
            team_name: Some(self.owner.clone()),
        })
    }

    async fn list_epics(&self) -> Result<Vec<Issue>> {
        // GitHub: treat milestones as epics.
        let ms = self.list_milestones().await?;
        Ok(ms
            .into_iter()
            .map(|m| Issue {
                id: m.id,
                backend: self.name().to_string(),
                url: None,
                title: m.name,
                description: m.description,
                state: match m.state.as_str() {
                    "closed" => IssueState::Closed,
                    _ => IssueState::Open,
                },
                issue_type: IssueType::Epic,
                priority: None,
                assignee: None,
                labels: vec![],
                milestone_id: None,
                milestone_name: None,
                project_id: None,
                project_name: None,
                parent_id: None,
                children: vec![],
                created_at: None,
                updated_at: None,
                extra: json!({}),
            })
            .collect())
    }

    async fn get_epic_issues(&self, epic_id: &str) -> Result<Vec<Issue>> {
        self.get_milestone_issues(epic_id).await
    }

    async fn create_project_update(
        &self,
        _project_id: &str,
        _body: &str,
        _health: Option<&str>,
    ) -> Result<ProjectUpdate> {
        bail!("github: project updates are not supported by the GitHub Projects V2 API")
    }

    async fn list_project_updates(&self, _project_id: &str) -> Result<Vec<ProjectUpdate>> {
        bail!("github: project updates are not supported by the GitHub Projects V2 API")
    }

    async fn list_states(&self) -> Result<Vec<String>> {
        Ok(vec!["open".into(), "closed".into()])
    }

    async fn transition_issue(&self, id: &str, state: &str) -> Result<Issue> {
        let target = match state {
            "open" | "reopened" => "open",
            _ => "closed",
        };
        let v = self
            .rest_patch(&self.issue_path(id), json!({ "state": target }))
            .await?;
        Ok(parse_issue(self, &v))
    }

    async fn assign_issue(&self, id: &str, assignee: &str) -> Result<Issue> {
        let v = self
            .rest_patch(&self.issue_path(id), json!({ "assignees": [assignee] }))
            .await?;
        Ok(parse_issue(self, &v))
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make() -> GitHubBackend {
        GitHubBackend {
            token: "t".into(),
            owner: "o".into(),
            repo: "r".into(),
            http: Client::new(),
        }
    }

    #[test]
    fn parse_issue_minimal() {
        let raw = json!({
            "number": 7,
            "title": "fix",
            "state": "open",
            "labels": [{"name": "bug"}],
            "html_url": "https://github.com/o/r/issues/7"
        });
        let issue = parse_issue(&make(), &raw);
        assert_eq!(issue.id, "7");
        assert_eq!(issue.state, IssueState::Open);
        assert_eq!(issue.labels, vec!["bug".to_string()]);
    }

    #[test]
    fn urlencode_basic() {
        assert_eq!(urlencode("hello world"), "hello%20world");
        assert_eq!(urlencode("a/b"), "a%2Fb");
    }
}
