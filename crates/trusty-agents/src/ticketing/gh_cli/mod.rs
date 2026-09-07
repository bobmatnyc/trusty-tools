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

use anyhow::{Result, anyhow};
use serde_json::Value;
use trusty_common::gh::GhCommand;

use super::types::{Tag, Ticket, TicketStatus, UpdateTicketReq};

/// The page size every `gh label list` in this adapter asks for.
///
/// Why: `gh label list` returns 30 labels by default and says nothing when it
/// truncates, so a repo with more labels than that read as missing every label
/// past the first page — the ensure-labels path then re-created labels that
/// already existed (#6953; trusty-tools carries 89). Large enough that no
/// realistic repo truncates, and a page that comes back exactly this full is
/// treated as a possibly-truncated read rather than a complete one.
/// Test: `label_list_command_requests_more_than_the_gh_default`.
pub(super) const LABEL_LIST_LIMIT: usize = 1000;

/// gh's own default page size for `gh label list`.
///
/// Why: the number [`LABEL_LIST_LIMIT`] has to beat, named so the regression
/// test asserts against gh's documented default rather than a magic literal.
pub(super) const GH_DEFAULT_LABEL_PAGE: usize = 30;

/// The `gh label list …` argv — the adapter's single spelling of that command.
///
/// Why: #6953 — an omitted `--limit` is exactly the flag that goes missing when
/// a command line is spelled inline at a call site, so the argv lives in one
/// named place with a test on it. Mirrors trusty-mpm's
/// `core::policy_labels::list_labels_argv` (#6952), which is private to that
/// crate's library and so cannot be shared through `trusty-common` without
/// moving it; the `--repo` selector is omitted here because callers apply it
/// through [`GhCommand::repo`].
/// What: `label list --limit <limit> --json name,color,description` — those
/// JSON fields are exactly [`Tag`]'s, so the output feeds [`labels_from_json`]
/// directly.
/// Test: `label_list_command_requests_more_than_the_gh_default`.
pub(super) fn list_labels_argv(limit: usize) -> Vec<String> {
    vec![
        "label".to_string(),
        "list".to_string(),
        "--limit".to_string(),
        limit.to_string(),
        "--json".to_string(),
        "name,color,description".to_string(),
    ]
}

/// Parse a `gh label list --json name,color,description` page into [`Tag`]s.
///
/// Why: #6953 — a page that comes back exactly `limit` long may have been
/// truncated by `gh`, and a partial label set read as complete is the defect
/// being fixed, so it is an error rather than a set the caller trusts.
/// What: maps each entry with a `name` into a [`Tag`], dropping entries without
/// one; refuses a full page before mapping anything.
/// Test: `labels_from_json_reads_past_the_default_label_page`,
/// `labels_from_json_rejects_a_full_page`,
/// `labels_from_json_skips_entries_without_a_name`.
pub(super) fn labels_from_json(arr: &[Value], limit: usize) -> Result<Vec<Tag>> {
    if arr.len() >= limit {
        return Err(anyhow!(
            "`gh label list` returned {} label(s), the full requested page — \
             the repository may carry more than this adapter can read in one call",
            arr.len()
        ));
    }
    Ok(arr
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
        .collect())
}

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
    // #5475: the probe now lives in trusty-common's `gh` entry point.
    trusty_common::gh::gh_available().await
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
        // #5475: spawn + non-zero mapping come from the shared entry point.
        Ok(GhCommand::new(args).stdout().await?)
    }

    /// The configured `owner/repo`, if any.
    pub(super) fn repo(&self) -> Option<&str> {
        self.repo.as_deref()
    }

    /// The `gh label list` invocation this client runs, repo selector included.
    ///
    /// Why: #6953 — the label listing had its argv spelled inline inside the
    /// `TicketingClient` impl, where no test could see whether it carried a
    /// `--limit`. Building it here makes the command the test's subject.
    /// What: [`list_labels_argv`] at [`LABEL_LIST_LIMIT`], with `--repo`
    /// prepended when this client targets a specific repository.
    /// Test: `label_list_command_requests_more_than_the_gh_default`,
    /// `label_list_command_carries_the_repo_selector`.
    pub(super) fn label_list_command(&self) -> GhCommand {
        GhCommand::new(list_labels_argv(LABEL_LIST_LIMIT)).repo(self.repo())
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
