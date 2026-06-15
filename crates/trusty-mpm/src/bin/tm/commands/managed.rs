//! Managed session-manager CLI handlers (session-manager MVP).
//!
//! Why: the session-manager MVP adds operator commands (`tm session new/ls/
//! activity/send/answer/attach/managed-stop` and `tm catalog sync/ls`) that talk
//! to the daemon's `/api/v1/sessions/managed/*` surface. Keeping these handlers
//! in their own file keeps `session.rs` under the SLOC cap.
//! What: thin async functions that issue HTTP requests via `reqwest` and render
//! the JSON responses; plus the local `catalog` handler that drives `CatalogSync`.
//! Test: `cli_parses_session_new`, `cli_parses_catalog_sync` exercise the parse
//! path; the HTTP round-trip is covered by `tests/session_manager_mvp.rs`.

use serde::Deserialize;

use crate::cli::CatalogAction;

/// A managed-session summary as returned by the daemon list/get endpoints.
///
/// Why: the CLI renders a stable subset of fields; deriving Deserialize on a
/// dedicated struct decouples the CLI from the daemon's internal record shape.
/// What: mirrors `daemon::managed_routes::SessionSummary`.
/// Test: rendered by `ls`; round-trip covered by the integration test.
#[derive(Debug, Deserialize)]
struct ManagedSummary {
    id: String,
    name: String,
    state: String,
    #[serde(default)]
    pending_decision: Option<String>,
}

/// `tm session new` — spawn a managed session from a repo + ref.
///
/// Why: the operator-facing entry point to provision an isolated workspace and
/// start a harness in it.
/// What: POSTs repo/ref/task/name_hint to `/api/v1/sessions/managed` and prints
/// the new session id, state, and attach command.
/// Test: HTTP path covered by `tests/session_manager_mvp.rs`.
pub(crate) async fn session_new(
    client: &reqwest::Client,
    url: &str,
    repo: String,
    git_ref: String,
    task: String,
    name_hint: Option<String>,
) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct SpawnResp {
        id: String,
        name: String,
        state: String,
        attach_cmd: String,
    }
    let resp: SpawnResp = client
        .post(format!("{url}/api/v1/sessions/managed"))
        .json(&serde_json::json!({
            "repo_url": repo,
            "ref": git_ref,
            "task": task,
            "name_hint": name_hint,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    println!("spawned {} ({}) [{}]", resp.name, resp.id, resp.state);
    println!("  attach: {}", resp.attach_cmd);
    Ok(())
}

/// `tm session ls` — list managed sessions.
///
/// Why: operators need a quick view of every managed session and its pending
/// decision.
/// What: GETs `/api/v1/sessions/managed` and prints a table or raw JSON.
/// Test: HTTP path covered by the integration test.
pub(crate) async fn session_ls(
    client: &reqwest::Client,
    url: &str,
    json: bool,
) -> anyhow::Result<()> {
    let raw = client
        .get(format!("{url}/api/v1/sessions/managed"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    if json {
        println!("{raw}");
        return Ok(());
    }
    #[derive(Deserialize)]
    struct ListResp {
        sessions: Vec<ManagedSummary>,
    }
    let body: ListResp = serde_json::from_str(&raw)?;
    if body.sessions.is_empty() {
        println!("no managed sessions");
    }
    for s in &body.sessions {
        let pending = s
            .pending_decision
            .as_deref()
            .map(|d| format!(" pending=\"{d}\""))
            .unwrap_or_default();
        println!("{} {} {}{}", s.id, s.name, s.state, pending);
    }
    Ok(())
}

/// `tm session activity <id>` — classify a managed session's activity state.
///
/// Why: inspect what a session is doing without attaching; the LLM verdict
/// tells the calling agentic process whether to intervene.
/// What: GETs `/api/v1/sessions/managed/{id}/activity` and prints the verdict
/// fields (state, summary, confidence, cache_hit, token costs) plus any pending
/// decision.
/// Test: HTTP path covered by the integration test.
pub(crate) async fn session_activity(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct ActivityResp {
        state: String,
        summary: String,
        confidence: f32,
        cache_hit: bool,
        input_tokens: u32,
        output_tokens: u32,
        latency_ms: u64,
        total_input_tokens: u64,
        total_output_tokens: u64,
        #[serde(default)]
        pending_decision: Option<String>,
        #[serde(default)]
        proposed_default: Option<String>,
    }
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed/{id}/activity"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    let a: ActivityResp = resp.error_for_status()?.json().await?;
    println!("state:      {} (confidence: {:.2})", a.state, a.confidence);
    println!("summary:    {}", a.summary);
    let cache = if a.cache_hit { "hit" } else { "miss" };
    println!(
        "cache:      {} | tokens: in={} out={} | latency: {}ms",
        cache, a.input_tokens, a.output_tokens, a.latency_ms
    );
    println!(
        "total:      in={} out={}",
        a.total_input_tokens, a.total_output_tokens
    );
    if let Some(pending) = &a.pending_decision {
        println!("pending decision: {pending}");
        if let Some(default) = &a.proposed_default {
            println!("  proposed default: {default}");
        }
    }
    Ok(())
}

/// `tm session send <id> <text>` — inject text into a managed session's pane.
///
/// Why: send a message to the harness without attaching to tmux.
/// What: POSTs `/api/v1/sessions/managed/{id}/send`.
/// Test: HTTP path covered by the integration test.
pub(crate) async fn session_send(
    client: &reqwest::Client,
    url: &str,
    id: String,
    text: String,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/send"))
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await?;
    handle_simple_ok(resp, "sent").await
}

/// `tm session answer <id> <answer>` — answer a pending decision.
///
/// Why: resolve a decision the harness is blocked on.
/// What: POSTs `/api/v1/sessions/managed/{id}/answer`.
/// Test: HTTP path covered by the integration test.
pub(crate) async fn session_answer(
    client: &reqwest::Client,
    url: &str,
    id: String,
    answer: String,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/answer"))
        .json(&serde_json::json!({ "answer": answer }))
        .send()
        .await?;
    handle_simple_ok(resp, "answered").await
}

/// `tm session attach <id>` — print the tmux attach command.
///
/// Why: operators need the exact `tmux attach` command to take over a pane.
/// What: GETs `/api/v1/sessions/managed/{id}/attach-cmd`.
/// Test: HTTP path covered by the integration test.
pub(crate) async fn session_attach(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed/{id}/attach-cmd"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    #[derive(Deserialize)]
    struct AttachResp {
        attach_cmd: String,
    }
    let body: AttachResp = resp.error_for_status()?.json().await?;
    println!("{}", body.attach_cmd);
    Ok(())
}

/// `tm session managed-stop <id>` — stop and deregister a managed session.
///
/// Why: terminate a managed session when its work is done.
/// What: DELETEs `/api/v1/sessions/managed/{id}`.
/// Test: HTTP path covered by the integration test.
pub(crate) async fn session_managed_stop(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    let resp = client
        .delete(format!("{url}/api/v1/sessions/managed/{id}"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    resp.error_for_status()?;
    println!("stopped {id}");
    Ok(())
}

/// Render a uniform success/not-found message for the send/answer endpoints.
///
/// Why: both endpoints share the same 404-or-OK response shape; centralizing the
/// rendering avoids duplication.
/// What: prints "not found" on 404, the success verb otherwise.
/// Test: covered indirectly by send/answer integration coverage.
async fn handle_simple_ok(resp: reqwest::Response, verb: &str) -> anyhow::Result<()> {
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    resp.error_for_status()?;
    println!("{verb}");
    Ok(())
}

/// `tm catalog` — sync or list the claude-mpm agent/skill catalog.
///
/// Why: the session-manager MVP deploys agents/skills from the claude-mpm repo;
/// this command keeps the local cache current and lists what is available.
/// What: `Sync` drives `CatalogSync::sync`; `Ls` lists cached agents and skills.
/// Catalog operations are local (no daemon round-trip).
/// Test: `cli_parses_catalog_sync`, `cli_parses_catalog_ls`.
pub(crate) async fn catalog(action: CatalogAction) -> anyhow::Result<()> {
    let catalog_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?
        .join(".trusty-mpm")
        .join("catalog");
    let sync =
        trusty_mpm::content::CatalogSync::new(trusty_mpm::provisioner::RealGitBackend, catalog_dir);
    match action {
        CatalogAction::Sync { force } => {
            let result = sync.sync(force)?;
            if result.fetched {
                println!(
                    "catalog synced: {} agents, {} skills",
                    result.agent_count, result.skill_count
                );
            } else {
                println!(
                    "catalog cache fresh ({} agents, {} skills); use --force to refetch",
                    result.agent_count, result.skill_count
                );
            }
        }
        CatalogAction::Ls { json } => {
            let agents = sync.list_agents();
            let skills = sync.list_skills();
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "agents": agents, "skills": skills })
                );
            } else {
                println!("agents ({}):", agents.len());
                for a in &agents {
                    println!("  {a}");
                }
                println!("skills ({}):", skills.len());
                for s in &skills {
                    println!("  {s}");
                }
            }
        }
    }
    Ok(())
}
