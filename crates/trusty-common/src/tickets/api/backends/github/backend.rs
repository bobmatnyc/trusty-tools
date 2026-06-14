//! GitHub backend — `Backend` trait implementation.
//!
//! Why: Isolates the Backend trait methods from transport and parse helpers
//! so the file stays under the 500-SLOC cap.
//! What: Implements the full `Backend` trait for `GitHubBackend`, wiring
//! REST helpers and GraphQL for Projects V2 support.
//! Test: `tests::parse_issue_minimal`, `tests::urlencode_basic`.

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::json;

use crate::tickets::api::backends::{
    Backend, CreateIssueParams, CreateMilestoneParams, ListIssuesParams, SearchIssuesParams,
    UpdateIssueParams,
};
use crate::tickets::api::models::*;

use super::types::{
    GitHubBackend, parse_comment, parse_issue, parse_label, parse_milestone, urlencode,
};

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

#[cfg(test)]
mod tests {
    use reqwest::Client;
    use serde_json::json;

    use super::super::types::{GitHubBackend, parse_issue, urlencode};
    use crate::tickets::api::models::IssueState;

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
