//! `tcode cancel <session-id>` (#2060).
//!
//! Why: the thin-client cancellation path — one `session.cancel` call.
//! What: dials the daemon serving `--project` on its Unix socket
//! ([`super::daemon_client::connect`], #10169), calls `session.cancel`, and
//! prints the resulting status. Cooperative-vs-immediate cancellation
//! semantics (#2056) are entirely the daemon's decision; this handler never
//! inspects `is_executing` itself.
//! Test: `thin_clients_uds_10169::cancel_cancels_a_session_created_on_the_daemon`,
//! `thin_clients_uds_10169::cancel_unknown_session_errors_cleanly`.

use std::path::Path;

use anyhow::Result;
use serde_json::json;

use trusty_code::session::Session;

use super::daemon_client;

/// `tcode cancel <SESSION_ID> [--project P]`.
///
/// Why/What: see module docs.
pub async fn run(project: &Path, session_id: &str) -> Result<()> {
    // #10169: cancel on the running daemon, not on a fresh stdio child.
    let client = daemon_client::connect(project).await?;
    let result = client
        .call("session.cancel", json!({"session_id": session_id}))
        .await?;

    let session: Session = serde_json::from_value(result)
        .map_err(|e| anyhow::anyhow!("tcode cancel: malformed daemon response: {e}"))?;
    println!("{}  {}", session.id, session.status.as_str());
    Ok(())
}
