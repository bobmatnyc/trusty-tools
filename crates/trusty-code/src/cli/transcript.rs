//! `tcode transcript <session-id>` (#2060).
//!
//! Why: the thin-client read path over #2058's `session.get_transcript` —
//! one call, human or `--json` output.
//! What: spawns an ephemeral daemon (same in-memory-per-process caveat as
//! `cli::session` — see its module docs), calls `session.get_transcript`,
//! and prints either the pretty-printed raw JSON (`--json`) or
//! `cli_client::render::render_transcript_human`'s readable view.
//! Test: `tests/cli_e2e.rs::transcript_unknown_session_errors_cleanly`;
//! the meaningful "real turns" case is exercised end-to-end via
//! `run_task::run`, which calls the SAME `session.get_transcript` method
//! from within one daemon's lifetime (see that module's docs for why a
//! standalone `transcript` invocation cannot see another process's
//! session).

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use trusty_code::cli_client::StdioRpcClient;
use trusty_code::cli_client::render::render_transcript_human;
use trusty_code::session::TranscriptRecord;

use super::tcode_exe;

/// `tcode transcript <SESSION_ID> [--project P] [--json]`.
///
/// Why/What: see module docs.
pub async fn run(project: &Path, session_id: &str, json_output: bool) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;
    let result = client
        .call("session.get_transcript", json!({"session_id": session_id}))
        .await;
    client.shutdown(Duration::from_secs(5)).await?;

    let result = result?;
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
