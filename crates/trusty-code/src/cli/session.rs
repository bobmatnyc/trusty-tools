//! `tcode session list` / `tcode session status <id>` (#2060).
//!
//! Why: the thin-client view of the running daemon's session registry — no
//! streaming, just dial, call, print, exit.
//! What: [`list`] calls `session.list` and prints
//! `cli_client::render::render_session_table`; [`status`] calls
//! `session.status` and prints one line. Both dial the daemon serving
//! `--project` on its Unix socket ([`super::daemon_client::connect`]), so they
//! see every session that daemon holds. Before #10169 each invocation spawned
//! its own `serve --stdio` child and could only ever see an empty registry.
//! Test: `thin_clients_uds_10169::session_list_shows_a_session_created_on_the_daemon`,
//! `thin_clients_uds_10169::session_status_reads_a_session_created_on_the_daemon`,
//! `thin_clients_uds_10169::session_list_refuses_an_unverifiable_daemon_instead_of_listing_nothing`.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use trusty_code::cli_client::render::render_session_table;
use trusty_code::session::Session;

use super::daemon_client;

/// `tcode session list [--project P]`.
///
/// Why/What: see module docs. A response without a `sessions` array is an
/// error, never an empty table.
pub async fn list(project: &Path) -> Result<()> {
    // #10169: the running daemon's registry, not a fresh stdio child's.
    let client = daemon_client::connect(project).await?;
    let result = client.call("session.list", json!({})).await?;

    let sessions: Vec<Session> = serde_json::from_value(
        result
            .get("sessions")
            .cloned()
            .context("tcode session list: daemon response carried no `sessions` field")?,
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
    // #10169: see `list`.
    let client = daemon_client::connect(project).await?;
    let result = client
        .call("session.status", json!({"session_id": session_id}))
        .await?;

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
