//! `tcode transcript <session-id>` (#2060).
//!
//! Why: the thin-client read path over #2058's `session.get_transcript` —
//! one call, human or `--json` output.
//! What: dials the daemon serving `--project` on its Unix socket
//! ([`super::daemon_client::connect`], #10169), calls
//! `session.get_transcript`, and prints either the pretty-printed raw JSON
//! (`--json`) or `cli_client::render::render_transcript_human`'s readable
//! view.
//! Test: `thin_clients_uds_10169::transcript_reads_a_session_created_on_the_daemon`,
//! `thin_clients_uds_10169::transcript_unknown_session_errors_cleanly`.

use std::path::Path;

use anyhow::Result;
use serde_json::json;

use trusty_code::cli_client::render::render_transcript_human;
use trusty_code::session::TranscriptRecord;

use super::daemon_client;

/// `tcode transcript <SESSION_ID> [--project P] [--json]`.
///
/// Why/What: see module docs.
pub async fn run(project: &Path, session_id: &str, json_output: bool) -> Result<()> {
    // #10169: the running daemon's transcript, not a fresh stdio child's.
    let client = daemon_client::connect(project).await?;
    let result = client
        .call("session.get_transcript", json!({"session_id": session_id}))
        .await?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&result)
                .map_err(|e| anyhow::anyhow!("tcode transcript: failed to render JSON: {e}"))?
        );
        return Ok(());
    }

    let record: TranscriptRecord = serde_json::from_value(result)
        .map_err(|e| anyhow::anyhow!("tcode transcript: malformed daemon response: {e}"))?;
    println!("{}", render_transcript_human(&record));
    Ok(())
}
