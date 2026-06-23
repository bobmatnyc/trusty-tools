//! SESSCTL command handlers for the `tm sessctl` subcommand group (WI-2 #1593).
//!
//! Why: Phase 2 of epic #1590 wires the `tm sessctl` CLI surface to the daemon
//! HTTP API (`/api/v1/control/sessions/*`) instead of embedding a local
//! `SessionRegistry`. Each handler is a thin HTTP client wrapper so all session
//! state lives in the single shared daemon registry.
//! What: five async handler functions (`run`, `connect`, `stop`, `auth`, `list`)
//! plus a `dispatch` entry point that routes `SessctlAction` variants.
//! Test: `cli_parses_sessctl_*` in tests.rs; HTTP round-trip integration tests
//! require the daemon to be running.

use std::path::PathBuf;

use crate::cli::SessctlAction;

/// Dispatch a `SessctlAction` to the appropriate HTTP handler.
///
/// Why: keeps `main.rs` thin — one match arm dispatches every sessctl verb.
/// What: matches each `SessctlAction` variant and calls the corresponding
/// handler function.
/// Test: parse round-trips in `tests.rs`; live integration tests require daemon.
pub(crate) async fn dispatch(
    client: &reqwest::Client,
    url: &str,
    action: SessctlAction,
) -> anyhow::Result<()> {
    match action {
        SessctlAction::Run {
            project_id,
            tmux,
            prompt_file,
            workdir,
        } => sessctl_run(client, url, project_id, tmux, prompt_file, workdir).await,
        SessctlAction::Connect { session_id } => sessctl_connect(client, url, session_id).await,
        SessctlAction::Stop { session_id, force } => {
            sessctl_stop(client, url, session_id, force).await
        }
        SessctlAction::Auth { session_id } => sessctl_auth(client, url, session_id).await,
        SessctlAction::List { project, format } => {
            sessctl_list(client, url, project.as_deref(), &format).await
        }
    }
}

/// POST /api/v1/control/sessions/run — spawn a SESSCTL session via the daemon.
///
/// Why: delegates session spawning to the daemon registry so the CLI does not
/// manage actor state directly (Phase 2 requirement of #1593).
/// What: resolves the workdir (falling back to cwd when unset), POSTs the
/// spawn request, and prints the allocated session ID.
/// Test: `cli_parses_sessctl_run`; live test requires daemon.
async fn sessctl_run(
    client: &reqwest::Client,
    url: &str,
    project_id: String,
    use_tmux: bool,
    prompt_file: Option<String>,
    workdir: Option<String>,
) -> anyhow::Result<()> {
    let resolved_workdir = workdir
        .map(PathBuf::from)
        .or_else(|| resolve_workdir_from_daemon(client, url, &project_id))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let backend = if use_tmux { "tmux" } else { "stream-json" };

    let body = serde_json::json!({
        "project_id": project_id,
        "workdir": resolved_workdir.to_string_lossy(),
        "backend": backend,
        "prompt_file": prompt_file,
    });

    let resp = client
        .post(format!("{url}/api/v1/control/sessions/run"))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("daemon returned {status}: {text}");
    }

    let parsed: serde_json::Value = resp.json().await?;
    let session_id = parsed["session_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("daemon response missing session_id"))?;
    println!("{session_id}");
    Ok(())
}

/// POST /api/v1/control/sessions/{id}/connect — stream SSE events from a session.
///
/// Why: `tm sessctl connect <id>` lets an operator observe a running session in
/// real-time. Acquiring the write-lock makes the caller the active writer.
/// What: streams the SSE response body line by line and prints each event to
/// stdout until the stream ends or is interrupted.
/// Test: `cli_parses_sessctl_connect`; live test requires daemon.
async fn sessctl_connect(
    client: &reqwest::Client,
    url: &str,
    session_id: String,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!(
            "{url}/api/v1/control/sessions/{session_id}/connect"
        ))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("daemon returned {status}: {text}");
    }

    // Drain the SSE stream and print each data line.
    let mut bytes_stream = resp.bytes_stream();
    use futures::StreamExt as _;
    while let Some(chunk) = bytes_stream.next().await {
        let chunk = chunk?;
        let text = String::from_utf8_lossy(&chunk);
        for line in text.lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                println!("{data}");
            }
        }
    }
    Ok(())
}

/// POST /api/v1/control/sessions/{id}/stop — send a stop command to a session.
///
/// Why: mirrors `tm sessions stop` for the SESSCTL surface.
/// What: POSTs with `?force=<bool>` and prints a confirmation.
/// Test: `cli_parses_sessctl_stop`; live test requires daemon.
async fn sessctl_stop(
    client: &reqwest::Client,
    url: &str,
    session_id: String,
    force: bool,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!(
            "{url}/api/v1/control/sessions/{session_id}/stop?force={force}"
        ))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("daemon returned {status}: {text}");
    }

    let mode = if force { "force-stop" } else { "stop" };
    eprintln!("sent {mode} to session {session_id}");
    Ok(())
}

/// GET /api/v1/control/sessions/{id}/auth — poll the auth state for a session.
///
/// Why: lets the operator detect an `awaiting-auth` condition without attaching
/// to the SSE stream.
/// What: GETs the auth endpoint and prints the state label.
/// Test: `cli_parses_sessctl_auth`; live test requires daemon.
async fn sessctl_auth(
    client: &reqwest::Client,
    url: &str,
    session_id: String,
) -> anyhow::Result<()> {
    let resp = client
        .get(format!("{url}/api/v1/control/sessions/{session_id}/auth"))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("daemon returned {status}: {text}");
    }

    let parsed: serde_json::Value = resp.json().await?;
    let state = parsed["state"].as_str().unwrap_or("unknown");
    let awaiting = parsed["awaiting_auth"].as_bool().unwrap_or(false);
    println!("session {session_id}: state={state} awaiting_auth={awaiting}");
    Ok(())
}

/// GET /api/v1/control/sessions — list SESSCTL sessions.
///
/// Why: `tm sessctl list` gives operators a snapshot of every live control-plane
/// session without requiring access to the daemon process.
/// What: GETs the sessions list (with optional `?project=` filter using reqwest's
/// query builder so project names with special characters are properly encoded)
/// and renders as a table or raw JSON.
/// Test: `cli_parses_sessctl_list`; live test requires daemon.
async fn sessctl_list(
    client: &reqwest::Client,
    url: &str,
    project: Option<&str>,
    format: &str,
) -> anyhow::Result<()> {
    let endpoint = format!("{url}/api/v1/control/sessions");
    let mut req = client.get(&endpoint);
    if let Some(proj) = project {
        req = req.query(&[("project", proj)]);
    }

    let resp = req.send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("daemon returned {status}: {text}");
    }

    let parsed: serde_json::Value = resp.json().await?;

    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&parsed)?);
        return Ok(());
    }

    // Table format.
    let sessions = parsed["sessions"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    if sessions.is_empty() {
        eprintln!("no sessions");
        return Ok(());
    }
    println!(
        "{:<30} {:<20} {:<12} {:<20} {:>10}",
        "SESSION_ID", "PROJECT", "BACKEND", "STATE", "UPTIME"
    );
    for s in sessions {
        let sid = s["session_id"].as_str().unwrap_or("-");
        let proj = s["project_id"].as_str().unwrap_or("-");
        let backend = s["backend"].as_str().unwrap_or("-");
        let state = s["state"].as_str().unwrap_or("-");
        let uptime = s["uptime_secs"].as_u64().unwrap_or(0);
        println!(
            "{:<30} {:<20} {:<12} {:<20} {:>9}s",
            sid, proj, backend, state, uptime
        );
    }
    Ok(())
}

/// Attempt to resolve a project's workdir from the daemon registry (best-effort).
///
/// Why: when `--workdir` is omitted the daemon may know the project's working
/// directory from its project registry. This preserves the lookup seam for WI-3.
/// What: currently returns `None` (fallback to cwd) with an explicit stderr
/// warning so the operator knows why cwd was chosen. A full HTTP lookup is
/// deferred to WI-3 when the project registry HTTP endpoint is wired.
/// Test: callers fall back to `std::env::current_dir()` when this returns
/// `None`; the warning message is observable in integration tests.
fn resolve_workdir_from_daemon(
    _client: &reqwest::Client,
    _url: &str,
    project_id: &str,
) -> Option<PathBuf> {
    // TODO(#1593/WI-3): implement daemon project-workdir lookup once the
    // project registry HTTP endpoint is wired. For now, fall back to cwd.
    eprintln!(
        "warning: no --workdir given and daemon workdir lookup is not yet wired \
         (WI-3); using current directory as workdir for project '{project_id}'"
    );
    None
}
