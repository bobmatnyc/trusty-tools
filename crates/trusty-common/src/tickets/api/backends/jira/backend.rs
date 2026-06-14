//! JIRA backend — `Backend` trait implementation.
//!
//! Why: Isolates the Backend trait methods from transport and parse helpers
//! so the file stays under the 500-SLOC cap.
//! What: Implements the full `Backend` trait for `JiraBackend`, wiring
//! REST helpers and JQL queries.
//! Test: `tests::parse_adf_text`, `tests::issue_state_mapping`.

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::tickets::api::backends::{
    Backend, CreateIssueParams, CreateMilestoneParams, ListIssuesParams, SearchIssuesParams,
    UpdateIssueParams,
};
use crate::tickets::api::models::*;

use super::types::{JiraBackend, adf_paragraph, parse_comment, parse_issue};

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
    use serde_json::json;

    use super::super::types::{flatten_adf, map_state};
    use crate::tickets::api::models::IssueState;

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
