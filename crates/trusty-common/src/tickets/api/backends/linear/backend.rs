//! Linear backend — `Backend` trait implementation.
//!
//! Why: Isolates the Backend trait methods from transport and parse helpers
//! so the file stays under the 500-SLOC cap.
//! What: Implements the full `Backend` trait for `LinearBackend`, using
//! the GraphQL helper and parse helpers from the sibling submodules.
//! Test: `tests::priority_to_int_mapping`, `tests::int_to_priority_mapping`,
//! `tests::state_mapping`, `tests::requires_api_key`.

use anyhow::{Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;

use crate::tickets::api::backends::{
    Backend, CreateIssueParams, CreateMilestoneParams, ListIssuesParams, SearchIssuesParams,
    UpdateIssueParams,
};
use crate::tickets::api::models::*;

use super::types::{ISSUE_FIELDS, LinearBackend, parse_issue, priority_to_int};

#[async_trait]
impl Backend for LinearBackend {
    fn name(&self) -> &'static str {
        "linear"
    }

    async fn create_issue(&self, p: CreateIssueParams) -> Result<Issue> {
        let team_id = self.resolve_team_id().await?;
        let mut input = json!({
            "teamId": team_id,
            "title": p.title,
        });
        if let Some(d) = p.description {
            input["description"] = json!(d);
        }
        if let Some(pri) = p.priority {
            input["priority"] = json!(priority_to_int(&pri));
        }
        if let Some(a) = p.assignee {
            input["assigneeId"] = json!(a);
        }
        if let Some(parent) = p.parent_id {
            input["parentId"] = json!(parent);
        }
        if let Some(proj) = p.project_id {
            input["projectId"] = json!(proj);
        }
        let q = format!(
            "mutation($input: IssueCreateInput!) {{ issueCreate(input: $input) {{ success issue {{ {ISSUE_FIELDS} }} }} }}"
        );
        let v = self.graphql(&q, json!({ "input": input })).await?;
        Ok(parse_issue(&v["data"]["issueCreate"]["issue"]))
    }

    async fn get_issue(&self, id: &str) -> Result<Issue> {
        let q = format!("query($id: String!) {{ issue(id: $id) {{ {ISSUE_FIELDS} }} }}");
        let v = self.graphql(&q, json!({ "id": id })).await?;
        Ok(parse_issue(&v["data"]["issue"]))
    }

    async fn update_issue(&self, id: &str, p: UpdateIssueParams) -> Result<Issue> {
        let mut input = json!({});
        if let Some(t) = p.title {
            input["title"] = json!(t);
        }
        if let Some(d) = p.description {
            input["description"] = json!(d);
        }
        if let Some(a) = p.assignee {
            input["assigneeId"] = json!(a);
        }
        if let Some(pri) = p.priority {
            input["priority"] = json!(priority_to_int(&pri));
        }
        if let Some(state) = p.state {
            // For state, resolve state id by name.
            let team_id = self.resolve_team_id().await?;
            let sq = "query($t: String!){ team(id:$t){ states { nodes { id name } } } }";
            let v = self.graphql(sq, json!({ "t": team_id })).await?;
            let nodes = v["data"]["team"]["states"]["nodes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let want = state.to_lowercase();
            if let Some(sid) = nodes.iter().find_map(|n| {
                let name = n.get("name").and_then(|v| v.as_str())?;
                if name.to_lowercase() == want {
                    n.get("id").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            }) {
                input["stateId"] = json!(sid);
            }
        }
        let q = format!(
            "mutation($id: String!, $input: IssueUpdateInput!) {{ issueUpdate(id: $id, input: $input) {{ success issue {{ {ISSUE_FIELDS} }} }} }}"
        );
        let v = self
            .graphql(&q, json!({ "id": id, "input": input }))
            .await?;
        Ok(parse_issue(&v["data"]["issueUpdate"]["issue"]))
    }

    async fn close_issue(&self, id: &str, comment: Option<&str>) -> Result<Issue> {
        if let Some(c) = comment {
            self.add_comment(id, c).await?;
        }
        self.transition_issue(id, "Done").await
    }

    async fn reopen_issue(&self, id: &str) -> Result<Issue> {
        self.transition_issue(id, "Todo").await
    }

    async fn list_issues(&self, p: ListIssuesParams) -> Result<Vec<Issue>> {
        let team_id = self.resolve_team_id().await?;
        let q = format!(
            "query($t: String!, $first: Int!) {{ team(id: $t) {{ issues(first: $first) {{ nodes {{ {ISSUE_FIELDS} }} }} }} }}"
        );
        let v = self
            .graphql(&q, json!({ "t": team_id, "first": p.limit.clamp(1, 250) }))
            .await?;
        let nodes = v["data"]["team"]["issues"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes.iter().map(parse_issue).collect())
    }

    async fn search_issues(&self, p: SearchIssuesParams) -> Result<Vec<Issue>> {
        let query = p.query.clone().unwrap_or_default();
        let q = format!(
            "query($q: String!, $first: Int!) {{ issueSearch(query: $q, first: $first) {{ nodes {{ {ISSUE_FIELDS} }} }} }}"
        );
        let v = self
            .graphql(&q, json!({ "q": query, "first": p.limit.clamp(1, 250) }))
            .await?;
        let nodes = v["data"]["issueSearch"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes.iter().map(parse_issue).collect())
    }

    async fn add_comment(&self, issue_id: &str, body: &str) -> Result<Comment> {
        let q = "mutation($input: CommentCreateInput!) { commentCreate(input: $input) { success comment { id body user { name } createdAt updatedAt } } }";
        let v = self
            .graphql(q, json!({ "input": { "issueId": issue_id, "body": body } }))
            .await?;
        let c = &v["data"]["commentCreate"]["comment"];
        Ok(Comment {
            id: c["id"].as_str().unwrap_or("").to_string(),
            issue_id: issue_id.to_string(),
            author: c["user"]["name"].as_str().map(String::from),
            body: c["body"].as_str().unwrap_or("").to_string(),
            created_at: c["createdAt"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc)),
            updated_at: c["updatedAt"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc)),
        })
    }

    async fn list_comments(&self, issue_id: &str) -> Result<Vec<Comment>> {
        let q = "query($id: String!) { issue(id: $id) { comments { nodes { id body user { name } createdAt updatedAt } } } }";
        let v = self.graphql(q, json!({ "id": issue_id })).await?;
        let nodes = v["data"]["issue"]["comments"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes
            .iter()
            .map(|c| Comment {
                id: c["id"].as_str().unwrap_or("").to_string(),
                issue_id: issue_id.to_string(),
                author: c["user"]["name"].as_str().map(String::from),
                body: c["body"].as_str().unwrap_or("").to_string(),
                created_at: c["createdAt"]
                    .as_str()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                updated_at: c["updatedAt"]
                    .as_str()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc)),
            })
            .collect())
    }

    async fn update_comment(
        &self,
        issue_id: &str,
        comment_id: &str,
        body: &str,
    ) -> Result<Comment> {
        let q = "mutation($id: String!, $input: CommentUpdateInput!) { commentUpdate(id: $id, input: $input) { success comment { id body updatedAt } } }";
        let v = self
            .graphql(q, json!({ "id": comment_id, "input": { "body": body } }))
            .await?;
        let c = &v["data"]["commentUpdate"]["comment"];
        Ok(Comment {
            id: comment_id.to_string(),
            issue_id: issue_id.to_string(),
            author: None,
            body: c["body"].as_str().unwrap_or("").to_string(),
            created_at: None,
            updated_at: c["updatedAt"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc)),
        })
    }

    async fn delete_comment(&self, _issue_id: &str, comment_id: &str) -> Result<()> {
        let q = "mutation($id: String!) { commentDelete(id: $id) { success } }";
        self.graphql(q, json!({ "id": comment_id })).await?;
        Ok(())
    }

    async fn list_labels(&self) -> Result<Vec<Label>> {
        let team_id = self.resolve_team_id().await?;
        let q = "query($t: String!) { team(id: $t) { labels { nodes { id name color } } } }";
        let v = self.graphql(q, json!({ "t": team_id })).await?;
        let nodes = v["data"]["team"]["labels"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes
            .iter()
            .map(|n| Label {
                id: n["id"].as_str().unwrap_or("").to_string(),
                name: n["name"].as_str().unwrap_or("").to_string(),
                color: n["color"].as_str().map(String::from),
                description: None,
            })
            .collect())
    }

    async fn create_label(
        &self,
        name: &str,
        color: Option<&str>,
        _description: Option<&str>,
    ) -> Result<Label> {
        let team_id = self.resolve_team_id().await?;
        let q = "mutation($input: IssueLabelCreateInput!) { issueLabelCreate(input: $input) { success issueLabel { id name color } } }";
        let mut input = json!({ "teamId": team_id, "name": name });
        if let Some(c) = color {
            input["color"] = json!(c);
        }
        let v = self.graphql(q, json!({ "input": input })).await?;
        let l = &v["data"]["issueLabelCreate"]["issueLabel"];
        Ok(Label {
            id: l["id"].as_str().unwrap_or("").to_string(),
            name: l["name"].as_str().unwrap_or("").to_string(),
            color: l["color"].as_str().map(String::from),
            description: None,
        })
    }

    async fn add_labels(&self, issue_id: &str, labels: &[String]) -> Result<()> {
        // Linear takes label IDs in `issueUpdate(labelIds: [...])`.
        // We pass through the IDs as-is (caller responsibility to pass
        // IDs rather than names).
        let q = format!(
            "mutation($id: String!, $input: IssueUpdateInput!) {{ issueUpdate(id: $id, input: $input) {{ success issue {{ {ISSUE_FIELDS} }} }} }}"
        );
        self.graphql(
            &q,
            json!({ "id": issue_id, "input": { "labelIds": labels } }),
        )
        .await?;
        Ok(())
    }

    async fn remove_labels(&self, issue_id: &str, _labels: &[String]) -> Result<()> {
        // Linear sets the full label set; to remove, callers should set
        // labels via `add_labels` with the desired set.
        bail!(
            "linear: remove_labels not directly supported — use add_labels with the new full label set (issue {issue_id})"
        )
    }

    async fn list_milestones(&self) -> Result<Vec<Milestone>> {
        let team_id = self.resolve_team_id().await?;
        let q = "query($t: String!) { team(id: $t) { cycles { nodes { id name number startsAt endsAt completedAt } } } }";
        let v = self.graphql(q, json!({ "t": team_id })).await?;
        let nodes = v["data"]["team"]["cycles"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes
            .iter()
            .map(|n| Milestone {
                id: n["id"].as_str().unwrap_or("").to_string(),
                name: n["name"].as_str().map(String::from).unwrap_or_else(|| {
                    n["number"]
                        .as_i64()
                        .map(|i| format!("Cycle {i}"))
                        .unwrap_or_default()
                }),
                description: None,
                state: if n["completedAt"].is_null() {
                    "active".into()
                } else {
                    "completed".into()
                },
                due_date: n["endsAt"]
                    .as_str()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                total_issues: None,
                closed_issues: None,
                progress_pct: None,
            })
            .collect())
    }

    async fn create_milestone(&self, p: CreateMilestoneParams) -> Result<Milestone> {
        let team_id = self.resolve_team_id().await?;
        let q = "mutation($input: CycleCreateInput!) { cycleCreate(input: $input) { success cycle { id name number startsAt endsAt } } }";
        let mut input = json!({ "teamId": team_id, "name": p.name });
        if let Some(due) = p.due_date {
            input["endsAt"] = json!(due);
        }
        let v = self.graphql(q, json!({ "input": input })).await?;
        let c = &v["data"]["cycleCreate"]["cycle"];
        Ok(Milestone {
            id: c["id"].as_str().unwrap_or("").to_string(),
            name: c["name"].as_str().unwrap_or("").to_string(),
            description: None,
            state: "active".into(),
            due_date: c["endsAt"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc)),
            total_issues: None,
            closed_issues: None,
            progress_pct: None,
        })
    }

    async fn close_milestone(&self, _id: &str) -> Result<Milestone> {
        bail!("linear: cycles complete automatically based on endsAt; manual close not supported")
    }

    async fn get_milestone_issues(&self, id: &str) -> Result<Vec<Issue>> {
        let q = format!(
            "query($id: String!) {{ cycle(id: $id) {{ issues {{ nodes {{ {ISSUE_FIELDS} }} }} }} }}"
        );
        let v = self.graphql(&q, json!({ "id": id })).await?;
        let nodes = v["data"]["cycle"]["issues"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes.iter().map(parse_issue).collect())
    }

    async fn list_projects(&self) -> Result<Vec<Project>> {
        let team_id = self.resolve_team_id().await?;
        let q = "query($t: String!) { team(id: $t) { projects { nodes { id name description state url } } } }";
        let v = self.graphql(q, json!({ "t": team_id })).await?;
        let nodes = v["data"]["team"]["projects"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes
            .iter()
            .map(|n| Project {
                id: n["id"].as_str().unwrap_or("").to_string(),
                name: n["name"].as_str().unwrap_or("").to_string(),
                description: n["description"].as_str().map(String::from),
                state: n["state"].as_str().unwrap_or("planned").to_string(),
                url: n["url"].as_str().map(String::from),
                team_name: self.team_key.clone(),
            })
            .collect())
    }

    async fn get_project(&self, id: &str) -> Result<Project> {
        let q = "query($id: String!) { project(id: $id) { id name description state url } }";
        let v = self.graphql(q, json!({ "id": id })).await?;
        let n = &v["data"]["project"];
        Ok(Project {
            id: n["id"].as_str().unwrap_or("").to_string(),
            name: n["name"].as_str().unwrap_or("").to_string(),
            description: n["description"].as_str().map(String::from),
            state: n["state"].as_str().unwrap_or("planned").to_string(),
            url: n["url"].as_str().map(String::from),
            team_name: self.team_key.clone(),
        })
    }

    async fn list_epics(&self) -> Result<Vec<Issue>> {
        // Linear treats parent issues as epics; we expose top-level issues
        // that have children.
        let team_id = self.resolve_team_id().await?;
        let q = format!(
            "query($t: String!) {{ team(id: $t) {{ issues(first: 100, filter: {{ children: {{ length: {{ gt: 0 }} }} }}) {{ nodes {{ {ISSUE_FIELDS} }} }} }} }}"
        );
        let v = self.graphql(&q, json!({ "t": team_id })).await?;
        let nodes = v["data"]["team"]["issues"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes
            .iter()
            .map(|n| {
                let mut i = parse_issue(n);
                i.issue_type = IssueType::Epic;
                i
            })
            .collect())
    }

    async fn get_epic_issues(&self, epic_id: &str) -> Result<Vec<Issue>> {
        let q = format!(
            "query($id: String!) {{ issue(id: $id) {{ children {{ nodes {{ {ISSUE_FIELDS} }} }} }} }}"
        );
        let v = self.graphql(&q, json!({ "id": epic_id })).await?;
        let nodes = v["data"]["issue"]["children"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes.iter().map(parse_issue).collect())
    }

    async fn create_project_update(
        &self,
        project_id: &str,
        body: &str,
        health: Option<&str>,
    ) -> Result<ProjectUpdate> {
        let q = "mutation($input: ProjectUpdateCreateInput!) { projectUpdateCreate(input: $input) { success projectUpdate { id body health user { name } createdAt } } }";
        let mut input = json!({ "projectId": project_id, "body": body });
        if let Some(h) = health {
            let enum_val = match h {
                "on_track" => "onTrack",
                "at_risk" => "atRisk",
                "off_track" => "offTrack",
                "complete" => "complete",
                "inactive" => "inactive",
                _ => h,
            };
            input["health"] = json!(enum_val);
        }
        let v = self.graphql(q, json!({ "input": input })).await?;
        let u = &v["data"]["projectUpdateCreate"]["projectUpdate"];
        Ok(ProjectUpdate {
            id: u["id"].as_str().unwrap_or("").to_string(),
            project_id: project_id.to_string(),
            body: u["body"].as_str().unwrap_or("").to_string(),
            health: u["health"].as_str().map(String::from),
            author_name: u["user"]["name"].as_str().map(String::from),
            created_at: u["createdAt"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(Utc::now),
        })
    }

    async fn list_project_updates(&self, project_id: &str) -> Result<Vec<ProjectUpdate>> {
        let q = "query($id: String!) { project(id: $id) { projectUpdates { nodes { id body health user { name } createdAt updatedAt } } } }";
        let v = self.graphql(q, json!({ "id": project_id })).await?;
        let nodes = v["data"]["project"]["projectUpdates"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes
            .iter()
            .map(|u| ProjectUpdate {
                id: u["id"].as_str().unwrap_or("").to_string(),
                project_id: project_id.to_string(),
                body: u["body"].as_str().unwrap_or("").to_string(),
                health: u["health"].as_str().map(String::from),
                author_name: u["user"]["name"].as_str().map(String::from),
                created_at: u["createdAt"]
                    .as_str()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now),
            })
            .collect())
    }

    async fn list_states(&self) -> Result<Vec<String>> {
        let team_id = self.resolve_team_id().await?;
        let q = "query($t: String!) { team(id: $t) { states { nodes { name } } } }";
        let v = self.graphql(q, json!({ "t": team_id })).await?;
        let nodes = v["data"]["team"]["states"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes
            .iter()
            .filter_map(|n| n["name"].as_str().map(String::from))
            .collect())
    }

    async fn transition_issue(&self, id: &str, state: &str) -> Result<Issue> {
        self.update_issue(
            id,
            UpdateIssueParams {
                state: Some(state.to_string()),
                ..Default::default()
            },
        )
        .await
    }

    async fn assign_issue(&self, id: &str, assignee: &str) -> Result<Issue> {
        self.update_issue(
            id,
            UpdateIssueParams {
                assignee: Some(assignee.to_string()),
                ..Default::default()
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests;
