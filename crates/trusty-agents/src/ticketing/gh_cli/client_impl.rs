//! The `impl TicketingClient for GhCliClient` block.
//!
//! Why: The trait implementation (provider metadata + every CRUD operation
//! shelling out to `gh issue ...`) is the bulk of the adapter; isolating it
//! from the struct + parsing helpers keeps both files under the 500-line cap.
//! What: Every `TicketingClient` method for `GhCliClient`, dispatching through
//! the inherent `run_with_repo`/`get_ticket` helpers and the free
//! `gh_issue_to_ticket` / `plan_gh_issue_edit_calls` parsers in `mod.rs`.
//! Test: Exercised indirectly; the parsers are unit-tested in `gh_cli::tests`.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use super::{
    GhCliClient, LIST_JSON_FIELDS, TICKET_JSON_FIELDS, gh_issue_to_ticket, plan_gh_issue_edit_calls,
};
use crate::ticketing::TicketingClient;
use crate::ticketing::types::{
    CreateTicketReq, Priority, Tag, Ticket, TicketFilter, TicketStatus, TicketingCapabilities,
    UpdateTicketReq,
};

#[async_trait]
impl TicketingClient for GhCliClient {
    fn provider_name(&self) -> &str {
        "github-gh-cli"
    }

    async fn create_ticket(&self, req: CreateTicketReq) -> Result<Ticket> {
        // `gh issue create --json` is not supported on all gh versions, so
        // we create then look up by URL. `gh issue create` prints the URL on
        // stdout; the issue number is the last path segment.
        let labels_str;
        let mut args: Vec<&str> = vec![
            "issue", "create", "--title", &req.title, "--body", &req.body,
        ];
        if !req.labels.is_empty() {
            labels_str = req.labels.join(",");
            args.push("--label");
            args.push(&labels_str);
        }
        if let Some(a) = req.assignee.as_deref() {
            args.push("--assignee");
            args.push(a);
        }
        let stdout = self.run_with_repo(&args).await?;
        let url = stdout.trim();
        let num = url
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| anyhow!("could not parse issue number from gh output: '{url}'"))?;
        let _ = req.priority; // priority not settable via gh issue create
        self.get_ticket(&num.to_string()).await
    }

    async fn get_ticket(&self, id: &str) -> Result<Ticket> {
        let stdout = self
            .run_with_repo(&["issue", "view", id, "--json", TICKET_JSON_FIELDS])
            .await?;
        let v: Value =
            serde_json::from_str(&stdout).context("failed to parse `gh issue view` JSON")?;
        gh_issue_to_ticket(&v)
    }

    async fn update_ticket(&self, id: &str, req: UpdateTicketReq) -> Result<Ticket> {
        // Plan all `gh issue edit` invocations (pure, see `plan_gh_issue_edit_calls`).
        // #248 C2: this now includes dedicated --add-label / --remove-label
        // calls for `req.add_labels` / `req.remove_labels`, which were
        // silently ignored before.
        for call in plan_gh_issue_edit_calls(id, &req) {
            let arg_refs: Vec<&str> = call.iter().map(|s| s.as_str()).collect();
            self.run_with_repo(&arg_refs).await?;
        }

        // Apply state transition separately (`gh issue close` / `reopen`).
        if let Some(s) = req.status {
            match s {
                TicketStatus::Closed | TicketStatus::Done | TicketStatus::Cancelled => {
                    self.run_with_repo(&["issue", "close", id]).await?;
                }
                TicketStatus::Open
                | TicketStatus::InProgress
                | TicketStatus::InReview
                | TicketStatus::Blocked => {
                    self.run_with_repo(&["issue", "reopen", id]).await?;
                }
                // Custom statuses can't be applied to a GitHub issue's
                // open/closed flag — silently ignore.
                TicketStatus::Custom(_) => {}
            }
        }

        self.get_ticket(id).await
    }

    async fn close_ticket(&self, id: &str) -> Result<()> {
        self.run_with_repo(&["issue", "close", id]).await?;
        Ok(())
    }

    async fn list_tickets(&self, filter: TicketFilter) -> Result<Vec<Ticket>> {
        let state = match filter.status {
            Some(TicketStatus::Closed)
            | Some(TicketStatus::Done)
            | Some(TicketStatus::Cancelled) => "closed",
            Some(_) => "open",
            None => "all",
        };
        let limit = filter.limit.unwrap_or(50).to_string();
        let mut args: Vec<String> = vec![
            "issue".into(),
            "list".into(),
            "--state".into(),
            state.into(),
            "--limit".into(),
            limit,
            "--json".into(),
            LIST_JSON_FIELDS.into(),
        ];
        if !filter.labels.is_empty() {
            args.push("--label".into());
            args.push(filter.labels.join(","));
        }
        if let Some(a) = filter.assignee.as_deref() {
            args.push("--assignee".into());
            args.push(a.to_string());
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let stdout = self.run_with_repo(&arg_refs).await?;
        let arr: Vec<Value> =
            serde_json::from_str(&stdout).context("failed to parse `gh issue list` JSON")?;
        let mut out = Vec::with_capacity(arr.len());
        for v in &arr {
            out.push(gh_issue_to_ticket(v).map(|mut t| {
                // list output omits body; leave empty string.
                if t.priority.is_none() {
                    t.priority = None::<Priority>;
                }
                t
            })?);
        }
        Ok(out)
    }

    async fn add_comment(&self, id: &str, body: &str) -> Result<()> {
        self.run_with_repo(&["issue", "comment", id, "--body", body])
            .await?;
        Ok(())
    }

    // ----- #246: capabilities, tagging, ownership, search, transitions -----

    fn capabilities(&self) -> TicketingCapabilities {
        TicketingCapabilities {
            tagging: true,
            transitions: true,
            ownership: true,
            search: true,
            milestones: false,
        }
    }

    async fn add_tags(&self, id: &str, tags: &[String]) -> Result<Ticket> {
        if !tags.is_empty() {
            let joined = tags.join(",");
            self.run_with_repo(&["issue", "edit", id, "--add-label", &joined])
                .await?;
        }
        self.get_ticket(id).await
    }

    async fn remove_tags(&self, id: &str, tags: &[String]) -> Result<Ticket> {
        if !tags.is_empty() {
            let joined = tags.join(",");
            self.run_with_repo(&["issue", "edit", id, "--remove-label", &joined])
                .await?;
        }
        self.get_ticket(id).await
    }

    async fn list_available_tags(&self) -> Result<Vec<Tag>> {
        let stdout = self
            .run_with_repo(&["label", "list", "--json", "name,color,description"])
            .await?;
        let arr: Vec<Value> =
            serde_json::from_str(&stdout).context("failed to parse `gh label list` JSON")?;
        let out = arr
            .iter()
            .filter_map(|l| {
                let name = l.get("name").and_then(Value::as_str)?.to_string();
                let color = l.get("color").and_then(Value::as_str).map(String::from);
                let description = l
                    .get("description")
                    .and_then(Value::as_str)
                    .map(String::from);
                Some(Tag {
                    name,
                    color,
                    description,
                })
            })
            .collect();
        Ok(out)
    }

    async fn assign(&self, id: &str, assignee: &str) -> Result<Ticket> {
        self.run_with_repo(&["issue", "edit", id, "--add-assignee", assignee])
            .await?;
        self.get_ticket(id).await
    }

    async fn unassign(&self, id: &str) -> Result<Ticket> {
        // gh requires knowing the assignee to remove. Look up current,
        // then remove all of them.
        let t = self.get_ticket(id).await?;
        if let Some(a) = t.assignee.as_deref() {
            self.run_with_repo(&["issue", "edit", id, "--remove-assignee", a])
                .await?;
        }
        self.get_ticket(id).await
    }

    async fn search(&self, query: &str, filter: TicketFilter) -> Result<Vec<Ticket>> {
        let state = match filter.status {
            Some(TicketStatus::Closed)
            | Some(TicketStatus::Done)
            | Some(TicketStatus::Cancelled) => "closed",
            Some(_) => "open",
            None => "all",
        };
        let limit = filter.limit.unwrap_or(30).to_string();
        let args: Vec<String> = vec![
            "issue".into(),
            "list".into(),
            "--search".into(),
            query.to_string(),
            "--state".into(),
            state.into(),
            "--limit".into(),
            limit,
            "--json".into(),
            LIST_JSON_FIELDS.into(),
        ];
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let stdout = self.run_with_repo(&arg_refs).await?;
        let arr: Vec<Value> =
            serde_json::from_str(&stdout).context("failed to parse `gh issue list` JSON")?;
        let mut out = Vec::with_capacity(arr.len());
        for v in &arr {
            let mut t = gh_issue_to_ticket(v)?;
            if t.priority.is_none() {
                t.priority = None::<Priority>;
            }
            out.push(t);
        }
        Ok(out)
    }

    async fn count_open_issues(&self, repo: &str) -> Result<u32> {
        // #342: cheap "how many issues" signal for project-discovery UIs.
        // Why subprocess-and-tolerant: `gh` may not be authed for `repo` (or
        // installed at all). Failing here would surface a noisy error in
        // every `/projects` render; instead we degrade gracefully to 0.
        let output = Command::new("gh")
            .args([
                "issue", "list", "--repo", repo, "--state", "open", "--json", "number",
            ])
            .output()
            .await?;
        if !output.status.success() {
            return Ok(0);
        }
        let json: Value = serde_json::from_slice(&output.stdout)?;
        Ok(json.as_array().map(|a| a.len() as u32).unwrap_or(0))
    }

    async fn count_open_prs(&self, repo: &str) -> Result<u32> {
        // #342: companion to `count_open_issues` — same graceful-degrade
        // semantics on auth/install failure.
        let output = Command::new("gh")
            .args([
                "pr", "list", "--repo", repo, "--state", "open", "--json", "number",
            ])
            .output()
            .await?;
        if !output.status.success() {
            return Ok(0);
        }
        let json: Value = serde_json::from_slice(&output.stdout)?;
        Ok(json.as_array().map(|a| a.len() as u32).unwrap_or(0))
    }

    async fn available_transitions(&self, _id: &str) -> Result<Vec<TicketStatus>> {
        Ok(vec![
            TicketStatus::Open,
            TicketStatus::InProgress,
            TicketStatus::InReview,
            TicketStatus::Closed,
            TicketStatus::Cancelled,
        ])
    }

    async fn transition(&self, id: &str, to: TicketStatus) -> Result<Ticket> {
        match &to {
            TicketStatus::Closed | TicketStatus::Done => {
                self.run_with_repo(&["issue", "close", id]).await?;
            }
            TicketStatus::Cancelled => {
                self.run_with_repo(&["issue", "edit", id, "--add-label", "status:cancelled"])
                    .await?;
                self.run_with_repo(&["issue", "close", id]).await?;
            }
            TicketStatus::Open => {
                self.run_with_repo(&["issue", "reopen", id]).await?;
            }
            TicketStatus::InProgress => {
                self.run_with_repo(&["issue", "edit", id, "--add-label", "status:in-progress"])
                    .await?;
            }
            TicketStatus::InReview => {
                self.run_with_repo(&["issue", "edit", id, "--add-label", "status:in-review"])
                    .await?;
            }
            TicketStatus::Blocked => {
                self.run_with_repo(&["issue", "edit", id, "--add-label", "status:blocked"])
                    .await?;
            }
            TicketStatus::Custom(name) => {
                let label = format!("status:{}", name);
                self.run_with_repo(&["issue", "edit", id, "--add-label", &label])
                    .await?;
            }
        }
        self.get_ticket(id).await
    }
}
