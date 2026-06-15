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

/// Build the one-line deprecation message for a renamed CLI verb.
///
/// Why: splitting message construction from the stderr write makes the wording
/// unit-testable without capturing process stderr (#1205).
/// What: returns `warning: '<old>' is deprecated; use '<new>'`.
/// Test: `deprecation_notice_format` in `tests.rs` asserts the exact text.
pub(crate) fn deprecation_message(old: &str, new: &str) -> String {
    format!("warning: '{old}' is deprecated; use '{new}'")
}

/// Emit a one-line deprecation notice to stderr for a renamed CLI verb.
///
/// Why: the verbose managed-lifecycle verbs (`runtime-stop`, `managed-resume`,
/// `managed-stop`) were renamed to the cleaner `stop`/`resume`/`decommission`
/// family (#1205). The old spellings still parse for backward compatibility, but
/// every invocation must nudge the operator toward the canonical verb so the
/// aliases can eventually be retired.
/// What: writes `deprecation_message(old, new)` to stderr, leaving stdout clean
/// for scriptable output.
/// Test: `cli_parses_session_runtime_stop`/`_managed_resume` assert the aliases
/// still parse; the message text is asserted by `deprecation_notice_format`.
pub(crate) fn deprecation_notice(old: &str, new: &str) {
    eprintln!("{}", deprecation_message(old, new));
}

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
/// start a harness in it, optionally selecting the runtime backend.
/// What: POSTs repo/ref/task/name_hint/runtime to `/api/v1/sessions/managed` and
/// prints the new session id, state, runtime, and attach command. `runtime`
/// defaults to `claude-code`; pass `--runtime tcode` for the direct-API backend.
/// Test: arg parsing covered by `cli_parses_session_new`; HTTP path covered by
/// `tests/session_manager_mvp.rs`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn session_new(
    client: &reqwest::Client,
    url: &str,
    repo: String,
    git_ref: String,
    task: String,
    name_hint: Option<String>,
    runtime: String,
) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct SpawnResp {
        id: String,
        name: String,
        state: String,
        attach_cmd: String,
        #[serde(default)]
        runtime: String,
    }
    let resp: SpawnResp = client
        .post(format!("{url}/api/v1/sessions/managed"))
        .json(&serde_json::json!({
            "repo_url": repo,
            "ref": git_ref,
            "task": task,
            "name_hint": name_hint,
            "runtime": runtime,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    println!(
        "spawned {} ({}) [{}] runtime={}",
        resp.name, resp.id, resp.state, resp.runtime
    );
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

/// `tm session activity <id>` — inspect a managed session's activity state.
///
/// Why: inspect what a session is doing without attaching; the raw pane is
/// always returned for the calling agentic process to reason over. The LLM
/// classification is shown when available (OpenRouter key set); when absent,
/// `classification: null` and the raw pane are still returned with no error.
/// What: GETs `/api/v1/sessions/managed/{id}/activity` and prints the raw pane,
/// structured state, classification (or "no classifier"), and pending decision.
/// Test: HTTP path covered by the integration test.
pub(crate) async fn session_activity(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct ActivityResp {
        raw_pane: String,
        runtime_active: bool,
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
        classification: Option<String>,
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
    let runtime_str = if a.runtime_active {
        "running"
    } else {
        "stopped"
    };
    println!("runtime:    {runtime_str}");
    println!("state:      {} (confidence: {:.2})", a.state, a.confidence);
    println!("summary:    {}", a.summary);
    let classification_str = a
        .classification
        .as_deref()
        .unwrap_or("(no classifier — raw pane available for agentic inference)");
    println!("classification: {classification_str}");
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
    if !a.raw_pane.is_empty() {
        println!("--- raw pane (last 60 lines) ---");
        println!("{}", a.raw_pane);
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

/// `tm session managed-stop <id>` — stop runtime only (keep workspace, deprecated alias).
///
/// Why: backward-compatible alias for `session_stop`; existing scripts that call
/// `managed-stop` keep working but get a deprecation nudge toward `stop` (#1205).
/// What: emits the deprecation notice, then POSTs
/// `/api/v1/sessions/managed/{id}/runtime-stop` via `session_stop`.
/// Test: HTTP path covered by the integration test; parse by
/// `cli_parses_session_managed_stop`.
pub(crate) async fn session_managed_stop(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    deprecation_notice("managed-stop", "stop");
    session_stop(client, url, id).await
}

/// `tm session runtime-stop <id>` — stop runtime only (deprecated alias).
///
/// Why: `runtime-stop` was renamed to `stop` (#1205); the old spelling still
/// parses but emits a deprecation notice steering operators to `stop`.
/// What: emits the deprecation notice, then delegates to `session_stop`.
/// Test: parse by `cli_parses_session_runtime_stop`; HTTP path via `session_stop`.
pub(crate) async fn session_runtime_stop(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    deprecation_notice("runtime-stop", "stop");
    session_stop(client, url, id).await
}

/// `tm session managed-resume <id>` — resume a stopped session (deprecated alias).
///
/// Why: `managed-resume` was renamed to `resume` (#1205); the old spelling still
/// parses but emits a deprecation notice steering operators to `resume`.
/// What: emits the deprecation notice, then delegates to `session_resume`.
/// Test: parse by `cli_parses_session_managed_resume`; HTTP path via
/// `session_resume`.
pub(crate) async fn session_managed_resume(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    deprecation_notice("managed-resume", "resume");
    session_resume(client, url, id).await
}

/// `tm session stop <id>` — stop the runtime of a managed session, keep the workspace.
///
/// Why: a session ENDURES beyond its runtime; `stop` kills only the tmux session
/// and claude process, preserving the workspace for later `resume`. Renamed from
/// the verbose `runtime-stop` in #1205 (which remains a deprecated alias).
/// What: POSTs `/api/v1/sessions/managed/{id}/runtime-stop`.
/// Test: HTTP path covered by the integration test; parse by
/// `cli_parses_session_managed_stop_verb`.
pub(crate) async fn session_stop(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/runtime-stop"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    resp.error_for_status()?;
    println!("runtime stopped {id} (workspace intact; use 'resume' to restart)");
    Ok(())
}

/// `tm session resume <id>` — resume a stopped managed session in its existing workspace.
///
/// Why: after `stop`, the workspace is still on disk; `resume` re-spawns the
/// runtime there without re-cloning. Renamed from the verbose `managed-resume`
/// in #1205 (which remains a deprecated alias).
/// What: POSTs `/api/v1/sessions/managed/{id}/resume`.
/// Test: HTTP path covered by the integration test; parse by
/// `cli_parses_session_managed_resume_verb`.
pub(crate) async fn session_resume(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/resume"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    if resp.status() == reqwest::StatusCode::CONFLICT {
        let msg = resp.text().await.unwrap_or_default();
        println!("cannot resume: {msg}");
        return Ok(());
    }
    #[derive(Deserialize)]
    struct ResumeResp {
        id: String,
        name: String,
        state: String,
    }
    let body: ResumeResp = resp.error_for_status()?.json().await?;
    println!("resumed {} ({}) [{}]", body.name, body.id, body.state);
    Ok(())
}

/// `tm session decommission <id>` — full teardown (remove workspace from disk).
///
/// Why: the ONLY operation that permanently removes the workspace directory.
/// Unlike `runtime-stop`, decommission is terminal — no resume is possible.
/// A tombstone record is kept so `ls` shows history.
/// What: POSTs `/api/v1/sessions/managed/{id}/decommission`.
/// Test: HTTP path covered by the integration test.
pub(crate) async fn session_decommission(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/decommission"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    resp.error_for_status()?;
    println!("decommissioned {id} (workspace removed; tombstone record kept)");
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
