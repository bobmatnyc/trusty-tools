//! `tcode cancel <session-id>` (#2060).
//!
//! Why: the thin-client cancellation path — one `session.cancel` call.
//! What: spawns an ephemeral daemon (same in-memory-per-process caveat as
//! `cli::session` — see its module docs), calls `session.cancel`, prints the
//! resulting status. Cooperative-vs-immediate cancellation semantics
//! (#2056) are entirely the daemon's decision; this handler never inspects
//! `is_executing` itself.
//! Test: `tests/cli_e2e.rs::cancel_unknown_session_errors_cleanly`.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use trusty_code::cli_client::StdioRpcClient;
use trusty_code::session::Session;

use super::tcode_exe;

/// `tcode cancel <SESSION_ID> [--project P]`.
///
/// Why/What: see module docs.
pub async fn run(project: &Path, session_id: &str) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;
    let result = client
        .call("session.cancel", json!({"session_id": session_id}))
        .await;
    client.shutdown(Duration::from_secs(5)).await?;

    let result = result?;
    let session: Session = serde_json::from_value(result)
        .map_err(|e| anyhow::anyhow!("tcode cancel: malformed daemon response: {e}"))?;
    println!("{}  {}", session.id, session.status.as_str());
    Ok(())
}
