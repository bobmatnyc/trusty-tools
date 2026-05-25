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
use serde_json::{json, Value};
use std::time::Duration;

/// Default HTTP timeout for the send call.
///
/// Why: The daemon's write path is short (one drawer insert) but we don't
/// want a stalled daemon to hang the CLI for minutes. 10 s leaves room for
/// a paged-out cache while still surfacing real outages quickly.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

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
    let addr = trusty_common::read_daemon_addr("trusty-memory")
        .context("read daemon address")?
        .ok_or_else(|| {
            anyhow!(
                "trusty-memory daemon is not running — start it with \
                 `trusty-memory start` and retry"
            )
        })?;
    let base = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{addr}")
    };
    let url = format!("{base}/api/v1/messages");

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

    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_TIMEOUT)
        .build()
        .context("build http client")?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("POST /api/v1/messages")?;
    let status = resp.status();
    let text = resp.text().await.context("read response body")?;
    if !status.is_success() {
        return Err(anyhow!("daemon returned {status}: {text}"));
    }
    // Print the response so scripts can capture the drawer id.
    // (Daemon returns a JSON object; we passthrough verbatim.)
    let pretty: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    println!("{}", serde_json::to_string_pretty(&pretty)?);
    Ok(())
}
