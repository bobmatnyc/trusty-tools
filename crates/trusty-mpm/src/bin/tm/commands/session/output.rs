//! `tm sessions output` — capture a session's tmux pane.
//!
//! Why: the `Output` arm of the `session` dispatcher grew a managed-session
//! path (#9162) and `session.rs` sits at the 500-SLOC cap, so the verb lives
//! here.
//! What: [`session_output`] fetches the output body and prints it; the body is
//! fetched by [`fetch_output`].
//! Test: `output_tests.rs` (`managed_*_9162`, `project_*`).

use crate::formatters::session::{input_box_note, print_compression_stats};

/// `tm sessions output <id|name>`: print the pane text, then the stats and the
/// input-box note.
///
/// Why: stdout stays the pane text byte for byte; the notes go to stderr (#8407).
/// What: prints the body's `output` to stdout, compression stats, and the
/// `[input box: ...]` note to stderr.
/// Test: `output_tests.rs`.
pub(crate) async fn session_output(
    daemon: &trusty_mpm::client::DaemonClient,
    id_or_name: &str,
    lines: u32,
    summarize: bool,
) -> anyhow::Result<()> {
    let body = fetch_output(daemon, id_or_name, lines, summarize).await?;
    let output = body.get("output").and_then(|v| v.as_str()).unwrap_or("");
    print!("{output}");
    print_compression_stats(&body);
    // #8407: on stderr, so stdout stays the pane text byte for byte.
    eprintln!("{}", input_box_note(&body));
    Ok(())
}

/// Fetch the output body for a session.
///
/// Why: a seam the tests drive without capturing stdout.
/// What: `GET /sessions/{id}/output` (project sessions only).
/// Test: `output_tests.rs`.
pub(crate) async fn fetch_output(
    daemon: &trusty_mpm::client::DaemonClient,
    id_or_name: &str,
    lines: u32,
    summarize: bool,
) -> anyhow::Result<serde_json::Value> {
    // #6288: `lines` typed, so the socket's params struct decodes it.
    let mut req = daemon
        .get(format!("/sessions/{id_or_name}/output"))
        .query(&[("lines", lines)]);
    if summarize {
        req = req.query(&[("compress", "summarise")]);
    }
    let resp = req.send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("session '{id_or_name}' not found");
    }
    Ok(resp.error_for_status()?.json().await?)
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod output_tests;
