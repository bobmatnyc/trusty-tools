//! `tcode session list` / `tcode session status <id>` (#2060).
//!
//! Why: the thin-client demonstration of the simplest `session.*` calls —
//! no streaming, just spawn, call, print, exit.
//! What: [`list`] calls `session.list` and prints
//! `cli_client::render::render_session_table`; [`status`] calls
//! `session.status` and prints one line. Both spawn a FRESH ephemeral
//! daemon per invocation (per `cli_client::stdio`'s module docs) — since
//! M1 sessions are in-memory-only per daemon process (`session::registry`'s
//! own docs), these commands only ever see sessions created within THIS
//! same invocation's daemon; they cannot inspect a session created by a
//! separate `tcode run-task` process. This is a known scope limitation, not
//! a bug — see this crate's `tests/cli_e2e.rs` module docs for the fuller
//! rationale.
//! Test: `tests/cli_e2e.rs::session_list_on_fresh_project_reports_no_sessions`.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use trusty_code::cli_client::StdioRpcClient;
use trusty_code::cli_client::render::render_session_table;
use trusty_code::session::Session;

use super::tcode_exe;

/// `tcode session list [--project P]`.
///
/// Why/What: see module docs.
pub async fn list(project: &Path) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;
    let result = client.call("session.list", json!({})).await?;
    client.shutdown(Duration::from_secs(5)).await?;

    let sessions: Vec<Session> = serde_json::from_value(
        result
            .get("sessions")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|e| anyhow::anyhow!("tcode session list: malformed daemon response: {e}"))?;
    println!("{}", render_session_table(&sessions));
    Ok(())
}

/// `tcode session status <SESSION_ID> [--project P]`.
///
/// Why/What: see module docs. Errors (e.g. `session_not_found`) propagate as
/// a plain `anyhow::Error` — `main.rs` prints it and exits nonzero.
pub async fn status(project: &Path, session_id: &str) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;
    let result = client
        .call("session.status", json!({"session_id": session_id}))
        .await;
    client.shutdown(Duration::from_secs(5)).await?;

    let result = result?;
    let session: Session = serde_json::from_value(result)
        .map_err(|e| anyhow::anyhow!("tcode session status: malformed daemon response: {e}"))?;
    println!(
        "{}  {}  agent={}  task={}",
        session.id,
        session.status.as_str(),
        session.agent.as_deref().unwrap_or("-"),
        session.task
    );
    Ok(())
}
