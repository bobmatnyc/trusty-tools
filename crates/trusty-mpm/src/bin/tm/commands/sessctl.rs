//! SESSCTL command handlers for the `tm sessctl` subcommand group (WI-2 #1593).
//!
//! Why: Phase 2 of epic #1590 wires the `tm sessctl` CLI surface to the daemon's
//! control-plane routes (`/api/v1/control/sessions/*`) instead of embedding a
//! local `SessionRegistry`, so all session state lives in the single shared
//! daemon registry. #6288: every verb reaches them over the daemon socket.
//! What: five async handler functions (`run`, `connect`, `stop`, `auth`, `list`)
//! plus a `dispatch` entry point that routes `SessctlAction` variants. `connect`
//! reads the `mpm.control.connect` frame stream.
//! Test: `cli_parses_sessctl_*` in tests.rs; `session_path_encodes_the_id`;
//! the stream in `tm_client_socket_cutover_6288::sessctl_connect_streams_over_the_socket`.

use std::path::PathBuf;

use trusty_mpm::client::DaemonClient;

use crate::cli::SessctlAction;

/// The control route for `session_id`, with the id percent-encoded.
///
/// Why: session ids derive from project ids, which may hold `/`, `?`, `#` or
/// spaces; the socket transport decodes each segment back.
/// What: `/api/v1/control/sessions/<encoded id>[/<suffix>]`; every byte outside
/// the RFC 3986 unreserved set is `%XX`.
/// Test: `session_path_encodes_the_id`.
fn session_path(session_id: &str, suffix: &str) -> String {
    let mut encoded = String::with_capacity(session_id.len());
    for byte in session_id.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    match suffix {
        "" => format!("/api/v1/control/sessions/{encoded}"),
        _ => format!("/api/v1/control/sessions/{encoded}/{suffix}"),
    }
}

/// Dispatch a `SessctlAction` to the appropriate handler.
///
/// Why: keeps `main.rs` thin — one match arm dispatches every sessctl verb.
/// What: builds the daemon client for `url` (the socket locator `main`
/// resolved) and calls the handler for the variant.
/// Test: parse round-trips in `tests.rs`.
pub(crate) async fn dispatch(
    client: &reqwest::Client,
    url: &str,
    action: SessctlAction,
) -> anyhow::Result<()> {
    let client = &DaemonClient::with_client(client.clone(), url);
    match action {
        SessctlAction::Run {
            project_id,
            tmux,
            prompt_file,
            workdir,
        } => sessctl_run(client, project_id, tmux, prompt_file, workdir).await,
        SessctlAction::Connect { session_id } => sessctl_connect(client, session_id).await,
        SessctlAction::Stop { session_id, force } => sessctl_stop(client, session_id, force).await,
        SessctlAction::Auth { session_id } => sessctl_auth(client, session_id).await,
        SessctlAction::List { project, format } => {
            sessctl_list(client, project.as_deref(), &format).await
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
    client: &DaemonClient,
    project_id: String,
    use_tmux: bool,
    prompt_file: Option<String>,
    workdir: Option<String>,
) -> anyhow::Result<()> {
    let resolved_workdir = workdir
        .map(PathBuf::from)
        .or_else(|| resolve_workdir_from_daemon(&project_id))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let backend = if use_tmux { "tmux" } else { "stream-json" };

    let body = serde_json::json!({
        "project_id": project_id,
        "workdir": resolved_workdir.to_string_lossy(),
        "backend": backend,
        "prompt_file": prompt_file,
    });

    let resp = client
        .post("/api/v1/control/sessions/run")
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

/// `mpm.control.connect` — stream a session's events over the daemon socket.
///
/// Why: `tm sessctl connect <id>` lets an operator observe a running session in
/// real-time. The socket form observes only: it takes no write-lock (see
/// `daemon::rpc::managed::control::connect_stream`).
/// What: opens the frame stream (#6288; the SSE route's socket form) and prints
/// each event as one JSON line — the payload the SSE `data:` lines carried —
/// until the stream ends. A refusal or a cut stream is an error.
/// Test: `cli_parses_sessctl_connect`;
/// `tm_client_socket_cutover_6288::sessctl_connect_streams_over_the_socket`.
async fn sessctl_connect(client: &DaemonClient, session_id: String) -> anyhow::Result<()> {
    let mut stream = client.connect_control_session(&session_id).await?;
    while let Some(frame) = stream.next_frame().await {
        println!("{}", frame?);
    }
    Ok(())
}

/// POST /api/v1/control/sessions/{id}/stop — send a stop command to a session.
///
/// Why: mirrors `tm session stop` for the SESSCTL surface.
/// What: POSTs with `?force=<bool>` and prints a confirmation.
/// Test: `cli_parses_sessctl_stop`; live test requires daemon.
async fn sessctl_stop(
    client: &DaemonClient,
    session_id: String,
    force: bool,
) -> anyhow::Result<()> {
    // #6288: typed, so the socket's params struct decodes it.
    let resp = client
        .post(session_path(&session_id, "stop"))
        .query(&[("force", force)])
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
async fn sessctl_auth(client: &DaemonClient, session_id: String) -> anyhow::Result<()> {
    let resp = client.get(session_path(&session_id, "auth")).send().await?;

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
    client: &DaemonClient,
    project: Option<&str>,
    format: &str,
) -> anyhow::Result<()> {
    let mut req = client.get("/api/v1/control/sessions");
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

    // Table format — shows core fields; use --format json for the full §4.5 schema.
    let sessions = parsed["sessions"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    if sessions.is_empty() {
        eprintln!("no sessions");
        return Ok(());
    }
    println!(
        "{:<30} {:<20} {:<12} {:<22} {:>8} {:<5} {:<40}",
        "SESSION_ID", "PROJECT", "BACKEND", "STATE", "UPTIME", "RST", "LAST_SUMMARY"
    );
    for s in sessions {
        let sid = s["session_id"].as_str().unwrap_or("-");
        let proj = s["project_id"].as_str().unwrap_or("-");
        let backend = s["backend"].as_str().unwrap_or("-");
        let state = s["state"].as_str().unwrap_or("-");
        let uptime = s["uptime_secs"].as_u64().unwrap_or(0);
        let restart_count = s["restart_count"].as_u64().unwrap_or(0);
        // Truncate last_summary to 40 Unicode scalar values with ellipsis to
        // keep the table row aligned regardless of summary length (review bug #5).
        // Known cosmetic limitation: CJK characters and emoji consume 2 display
        // columns each, so a summary containing them may visually exceed 40
        // columns. The `unicode-width` crate would fix this, but it is not a
        // current workspace dependency; adding it for a cosmetic CLI improvement
        // is not justified — document here instead.
        let raw_summary = s["last_summary"].as_str().unwrap_or("-");
        let last_summary: String = if raw_summary.chars().count() > 40 {
            let truncated: String = raw_summary.chars().take(37).collect();
            format!("{truncated}...")
        } else {
            raw_summary.to_owned()
        };
        println!(
            "{:<30} {:<20} {:<12} {:<22} {:>7}s {:<5} {:<40}",
            sid, proj, backend, state, uptime, restart_count, last_summary
        );
        // Show pending decision inline if present.
        if let Some(pd) = s["pending_decision"].as_str() {
            let default = s["proposed_default"].as_str().unwrap_or("?");
            println!("  pending: {pd}  [default: {default}]");
        }
    }
    Ok(())
}

/// Attempt to resolve a project's workdir from the daemon registry (best-effort).
///
/// Why: when `--workdir` is omitted the daemon may know the project's working
/// directory from its project registry. This preserves the lookup seam for WI-3
/// without taking `client`/`url` parameters that cannot be used yet.
/// What: currently returns `None` (fallback to cwd) with an explicit stderr
/// warning so the operator knows why cwd was chosen. A full HTTP lookup is
/// deferred to WI-3 when the project registry HTTP endpoint is wired.
/// Test: callers fall back to `std::env::current_dir()` when this returns
/// `None`; the warning message is observable in integration tests.
fn resolve_workdir_from_daemon(project_id: &str) -> Option<PathBuf> {
    // TODO(#1593/WI-3): implement daemon project-workdir lookup once the
    // project registry HTTP endpoint is wired. For now, fall back to cwd.
    eprintln!(
        "warning: no --workdir given and daemon workdir lookup is not yet wired \
         (WI-3); using current directory as workdir for project '{project_id}'"
    );
    None
}

#[cfg(test)]
mod tests {
    /// #6288: an id with reserved characters reaches the socket as one segment.
    #[test]
    fn session_path_encodes_the_id() {
        assert_eq!(
            super::session_path("proj/a b?#", "stop"),
            "/api/v1/control/sessions/proj%2Fa%20b%3F%23/stop"
        );
        assert_eq!(
            super::session_path("p-1.x_y~", ""),
            "/api/v1/control/sessions/p-1.x_y~"
        );
    }
}
