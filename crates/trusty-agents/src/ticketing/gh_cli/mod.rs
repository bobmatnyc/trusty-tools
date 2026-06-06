//! `gh` CLI ticketing backend (#245).
//!
//! Why: The token-based REST client (`github::GitHubClient`) requires the user
//! to mint and manage a `GITHUB_TOKEN` PAT. Many users already have the
//! official `gh` CLI installed and authenticated — we should be able to drive
//! GitHub Issues through it without a second credential. This adapter is a
//! drop-in `TicketingClient` that shells out to `gh issue ...` and parses the
//! `--json` output into our canonical `Ticket` shape.
//! What: `GhCliClient` implements `TicketingClient` by running `gh` subprocess
//! commands. `gh_available()` probes whether `gh` is installed AND
//! authenticated so the factory in `build_client()` can decide between the
//! REST and CLI backends.
//! Test: `tests::*` cover construction, label extraction, and status mapping.
//! Network/CLI calls are not exercised in unit tests (env-dependent).
//!
//! Module layout (see #366 split): struct + parsing helpers here; the
//! `impl TicketingClient` block in `client_impl.rs`; tests in `tests.rs`.

mod client_impl;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use tokio::process::Command;

use super::types::{Ticket, TicketStatus, UpdateTicketReq};

/// Check if `gh` is on PATH and authenticated.
///
/// Why: We only want to fall back to the CLI when it's actually usable —
/// otherwise users get confusing "command not found" or "not authenticated"
/// errors deep inside a tool call.
/// What: Runs `gh auth status` and returns `true` if the process exits zero.
/// Any error (binary missing, auth missing, IO) yields `false`.
/// Test: `gh_available_returns_bool` — only asserts no panic; the actual
/// boolean depends on the test environment.
pub async fn gh_available() -> bool {
    match Command::new("gh").args(["auth", "status"]).output().await {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// `gh` CLI-backed `TicketingClient`.
///
/// Why: Mirrors `GitHubClient` but uses the user's existing `gh` auth,
/// removing the need for a separate `GITHUB_TOKEN`.
/// What: Holds an optional `repo` ("owner/repo"); when `None`, defers to
/// `gh`'s current-directory remote resolution.
/// Test: `gh_cli_client_new_with_repo`, `gh_cli_client_new_without_repo`.
pub struct GhCliClient {
    /// "owner/repo" — if `None`, `gh` uses the current directory's remote.
    repo: Option<String>,
}

impl GhCliClient {
    pub fn new(repo: Option<String>) -> Self {
        Self { repo }
    }

    /// Run a `gh` command and return stdout as `String`.
    ///
    /// Why: Centralizes subprocess spawning + error handling so each tool
    /// method doesn't have to repeat the success-check / stderr-capture
    /// boilerplate.
    /// What: Spawns `gh <args...>`, returns stdout on success or an error
    /// containing stderr on non-zero exit.
    async fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("gh")
            .args(args)
            .output()
            .await
            .with_context(|| format!("failed to spawn 'gh {}'", args.join(" ")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "gh {} failed (exit {}): {}",
                args.join(" "),
                output.status,
                stderr.trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Run a `gh` command, prepending `--repo <repo>` if configured.
    ///
    /// Why: `gh` defaults to the current directory's git remote, but when the
    /// caller has explicitly set a repo we want every command to target it.
    /// What: If `self.repo` is `Some`, runs `gh --repo <repo> <args...>`;
    /// otherwise runs `gh <args...>`.
    async fn run_with_repo(&self, args: &[&str]) -> Result<String> {
        if let Some(repo) = &self.repo {
            let mut combined: Vec<&str> = vec!["--repo", repo.as_str()];
            combined.extend_from_slice(args);
            self.run(&combined).await
        } else {
            self.run(args).await
        }
    }
}

/// Map a `gh issue` JSON object to our canonical `Ticket`.
///
/// Why: `gh --json` output uses different field names from the REST API
/// (e.g. `number` instead of `id`, `OPEN`/`CLOSED` uppercase state, label
/// objects with `.name`). Centralizing the mapping keeps each tool method
/// focused on argv construction.
/// What: Reads `number`, `title`, `body`, `state`, `labels[].name`, `url`,
/// `createdAt`, `updatedAt` and produces a `Ticket`.
/// Test: `ticket_state_mapping`, `label_extraction_from_gh_json`.
fn gh_issue_to_ticket(v: &Value) -> Result<Ticket> {
    let id = v
        .get("number")
        .and_then(Value::as_i64)
        .map(|n| n.to_string())
        .ok_or_else(|| anyhow!("gh issue JSON missing 'number'"))?;
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let body = v
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let status = parse_gh_state(v.get("state").and_then(Value::as_str).unwrap_or("OPEN"));
    let labels = extract_labels(v.get("labels"));
    let url = v.get("url").and_then(Value::as_str).map(|s| s.to_string());
    let created_at = v
        .get("createdAt")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let updated_at = v
        .get("updatedAt")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let assignee = v
        .get("assignees")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|a| a.get("login").and_then(Value::as_str))
        .map(|s| s.to_string());

    Ok(Ticket {
        id,
        title,
        body,
        status,
        priority: None,
        labels,
        assignee,
        created_at,
        updated_at,
        url,
    })
}

/// Map gh's uppercase `state` string to `TicketStatus`.
///
/// Why: `gh --json state` returns `"OPEN"` / `"CLOSED"` (uppercase) whereas
/// the REST API uses lowercase. Anything else (defensive) maps to `Open`.
/// Test: `ticket_state_mapping`.
fn parse_gh_state(state: &str) -> TicketStatus {
    match state {
        "CLOSED" | "closed" => TicketStatus::Closed,
        _ => TicketStatus::Open,
    }
}

/// Extract a list of label names from `gh --json labels` output.
///
/// Why: `gh` returns labels as an array of objects (`[{"id":..,
/// "name":"bug","color":".."}]`); we only care about the names.
/// Test: `label_extraction_from_gh_json`.
fn extract_labels(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) const TICKET_JSON_FIELDS: &str =
    "number,title,body,state,labels,url,createdAt,updatedAt,assignees";
pub(super) const LIST_JSON_FIELDS: &str =
    "number,title,state,labels,url,createdAt,updatedAt,assignees";

/// Plan the sequence of `gh issue edit` invocations needed for an
/// `UpdateTicketReq`.
///
/// Why: Pulling the argv-construction out of `update_ticket` makes it pure
/// and unit-testable without spawning a real `gh` subprocess. Critically,
/// this is where #248 C2 was fixed — `add_labels` and `remove_labels` were
/// silently dropped before; the planner now emits dedicated `--add-label` /
/// `--remove-label` calls for them.
/// What: Returns a `Vec<Vec<String>>`; each inner vec is the arg list for
/// one `gh` invocation (excluding any `--repo` prefix, which `run_with_repo`
/// adds). Empty outer vec means "nothing to do".
/// Test: `plan_gh_issue_edit_calls_*` tests cover field combos and the
/// add/remove label paths.
fn plan_gh_issue_edit_calls(id: &str, req: &UpdateTicketReq) -> Vec<Vec<String>> {
    let mut calls: Vec<Vec<String>> = Vec::new();

    // Main combined edit (title/body/labels-replace/assignee).
    let mut main: Vec<String> = vec!["issue".into(), "edit".into(), id.to_string()];
    if let Some(t) = req.title.as_deref() {
        main.push("--title".into());
        main.push(t.to_string());
    }
    if let Some(b) = req.body.as_deref() {
        main.push("--body".into());
        main.push(b.to_string());
    }
    if let Some(labels) = req.labels.as_ref()
        && !labels.is_empty()
    {
        main.push("--add-label".into());
        main.push(labels.join(","));
    }
    if let Some(a) = req.assignee.as_deref() {
        main.push("--add-assignee".into());
        main.push(a.to_string());
    }
    if main.len() > 3 {
        calls.push(main);
    }

    // #248 C2: dedicated label-delta calls.
    if let Some(adds) = req.add_labels.as_ref()
        && !adds.is_empty()
    {
        calls.push(vec![
            "issue".into(),
            "edit".into(),
            id.to_string(),
            "--add-label".into(),
            adds.join(","),
        ]);
    }
    if let Some(rems) = req.remove_labels.as_ref()
        && !rems.is_empty()
    {
        calls.push(vec![
            "issue".into(),
            "edit".into(),
            id.to_string(),
            "--remove-label".into(),
            rems.join(","),
        ]);
    }

    calls
}
