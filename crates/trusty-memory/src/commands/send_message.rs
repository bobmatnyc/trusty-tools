//! Handler for `trusty-memory send-message` (issue #99).
//!
//! Why: gives non-MCP callers (shell scripts, Makefiles, the `claude-mpm`
//! migration shim) a way to deliver inter-project messages without going
//! through stdio MCP. Talks to the running daemon over the same HTTP API
//! the MCP tool ultimately uses, so behaviour stays in lockstep with the
//! MCP path.
//!
//! What: a one-shot async command that posts to
//! `POST /api/v1/messages` and prints the new drawer id on success.
//! Defaults `--from` to the cwd-derived palace slug when omitted.
//!
//! Test: round-trip via the unit test in `messaging::tests`; the HTTP
//! handler is itself covered by `web::tests::messages_post_then_get_unread`.

use anyhow::{anyhow, Context, Result};
use serde_json::json;
use std::time::Duration;

/// Default HTTP timeout for the send call.
///
/// Why: The daemon's write path is short (one drawer insert) but we don't
/// want a stalled daemon to hang the CLI for minutes. 10 s leaves room for
/// a paged-out cache while still surfacing real outages quickly.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Entry point for `trusty-memory send-message`.
///
/// Why: pure CLI shim around the HTTP send endpoint so the binary surface
/// stays consistent ("anything you can do via MCP, you can do via CLI").
/// What: resolves the daemon address from
/// `trusty_common::read_daemon_addr("trusty-memory")`, posts the message
/// payload, prints the response JSON, and exits non-zero on any failure
/// (unlike the SessionStart hook, this command is run by a human / script
/// who wants to see failures).
/// Test: covered manually via `trusty-memory start && trusty-memory
/// send-message --to <p> --purpose <p> --content <c>`.
pub async fn handle_send_message(
    to: String,
    purpose: String,
    content: String,
    from: Option<String>,
) -> Result<()> {
    let from_palace = match from {
        Some(s) => s,
        None => crate::messaging::cwd_palace_slug().context("derive --from palace from cwd")?,
    };

    let body = json!({
        "to_palace":   to,
        "from_palace": from_palace,
        "purpose":     purpose,
        "content":     content,
    });

    // Unlike the SessionStart hook, this command is run by a human or a script
    // that wants to see a failure, so nothing here degrades quietly. A daemon
    // that is not running surfaces as the dial error, with the start hint
    // attached.
    let result = crate::client::call_with_timeout("memory.message_send", body, CALL_TIMEOUT)
        .await
        .map_err(|e| {
            anyhow!(
                "{e:#}\n\nIf the daemon is not running, start it with \
                 `trusty-memory start` and retry."
            )
        })?;
    // Print the response so a script can capture the drawer id.
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
