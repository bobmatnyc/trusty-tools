//! Handler for `trusty-memory inbox-check` (issue #99).
//!
//! Why: Claude Code's `SessionStart` hook ingests stdout verbatim and
//! injects it as context for the new session. We use this to deliver
//! inter-project messages that have piled up in the project's palace
//! since the previous session — without polling, IPC, or background
//! workers. The receiver session "picks up the mail" exactly once per
//! delivery.
//!
//! What: a side-effect-only command that:
//!   1. Resolves the calling project's palace slug via
//!      [`crate::messaging::cwd_palace_slug`] (or `--palace` override).
//!   2. Queries the daemon's `GET /api/v1/messages?palace=<slug>&unread_only=true`
//!      endpoint for unread messages.
//!   3. Renders each message into a Markdown injection block and writes
//!      them to stdout in chronological order.
//!   4. Atomically marks each delivered message read via the same HTTP API
//!      (`POST /api/v1/messages/mark_read` with the drawer id).
//!
//! Like `prompt-context`, every error path degrades to exit 0 with empty
//! stdout — failing the SessionStart hook would block the new Claude Code
//! session. The mark-read step is best-effort: if it fails, the next
//! SessionStart will redeliver, which is preferable to silently dropping a
//! message we never confirmed delivery for.
//!
//! Test: `inbox_check_returns_ok_without_daemon` covers the no-daemon
//! branch; the round-trip is exercised by
//! `web::tests::messages_endpoint_round_trip`.

use anyhow::Result;
use serde::Deserialize;
use std::time::Duration;

/// Connect + total request timeout. Kept short so a slow/dead daemon can
/// never block a Claude Code session for more than a few seconds.
const HTTP_TIMEOUT: Duration = Duration::from_millis(2500);

/// Server payload schema for one decoded message.
///
/// Why: deserialise the daemon's `GET /api/v1/messages` response into
/// something we can render. The shape mirrors
/// [`crate::messaging::Message`] but we keep it local to the command so a
/// future on-wire change to the daemon can be absorbed without leaking
/// dependencies.
/// What: `id` carries the drawer UUID we POST back to `mark_read`;
/// `formatted` carries the Markdown block built by `to_injection_block` so
/// the CLI doesn't have to know the rendering rules. (Both fields are
/// optional in the JSON for forward compatibility with daemons that don't
/// pre-render.)
/// Test: indirectly via `web::tests::messages_endpoint_round_trip`.
#[derive(Deserialize)]
struct ServerMessage {
    id: String,
    #[serde(default)]
    formatted: Option<String>,
    // Raw fields, used as a fallback if `formatted` is absent.
    #[serde(default)]
    from_palace: Option<String>,
    #[serde(default)]
    to_palace: Option<String>,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    sent_at: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

/// Entry point for `trusty-memory inbox-check`.
///
/// Why: SessionStart hook. Same fail-soft contract as
/// [`crate::commands::prompt_context::handle_prompt_context`] — every
/// failure path exits 0 silently with no stdout so the user's session
/// start is never blocked.
/// What:
///   1. Resolves the recipient palace slug from cwd (or explicit `--palace`).
///   2. Fetches unread messages via the daemon's HTTP API.
///   3. Prints the formatted Markdown blocks to stdout.
///   4. POSTs back to mark each delivered message read.
///
/// `palace` overrides the cwd-derived slug; useful for test rigs and for
/// projects whose repo basename does not match their preferred palace.
/// Test: `inbox_check_returns_ok_without_daemon`.
pub async fn handle_inbox_check(palace: Option<String>) -> Result<()> {
    // Resolve daemon address — missing = exit silently.
    let addr = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(addr)) => addr,
        _ => return Ok(()),
    };
    let base = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{addr}")
    };

    // Resolve recipient palace.
    let recipient = match palace {
        Some(s) => s,
        None => match crate::messaging::cwd_palace_slug() {
            Ok(s) => s,
            Err(_) => return Ok(()),
        },
    };

    let client = match reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    // Fetch unread messages.
    let list_url = format!("{base}/api/v1/messages?palace={recipient}&unread_only=true");
    let resp = match client.get(&list_url).send().await {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    if !resp.status().is_success() {
        return Ok(());
    }
    let messages: Vec<ServerMessage> = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    if messages.is_empty() {
        return Ok(());
    }

    // Render and acknowledge.
    println!("# Inter-project inbox (trusty-memory, palace `{recipient}`)\n");
    for m in &messages {
        let block = match &m.formatted {
            Some(s) => s.clone(),
            None => render_fallback(m),
        };
        // One blank line between messages for readability.
        println!("{block}");
        println!();
    }

    // Mark each delivered message read. Best-effort: a failed ack means the
    // next SessionStart will redeliver, which is safer than silently
    // dropping a message we never confirmed.
    let mark_url = format!("{base}/api/v1/messages/mark_read");
    for m in &messages {
        let body = serde_json::json!({"palace": recipient, "drawer_id": m.id});
        let _ = client.post(&mark_url).json(&body).send().await;
    }

    Ok(())
}

/// Fallback renderer used when the daemon does not pre-format messages.
///
/// Why: defends against an older daemon that returns the raw envelope
/// fields without a `formatted` key. The block shape matches
/// `Message::to_injection_block` so the rendered session context is
/// indistinguishable from the server-side format.
/// What: synthesises the `## Message from <from>` heading plus metadata
/// and body using whichever optional fields are present, substituting
/// `"<unknown>"` when a field is absent.
/// Test: indirectly via the integration tests; defensive fallback only.
fn render_fallback(m: &ServerMessage) -> String {
    let from = m.from_palace.as_deref().unwrap_or("<unknown>");
    let to = m.to_palace.as_deref().unwrap_or("<unknown>");
    let purpose = m.purpose.as_deref().unwrap_or("<unknown>");
    let sent_at = m.sent_at.as_deref().unwrap_or("<unknown>");
    let content = m.content.as_deref().unwrap_or("<missing body>");
    format!(
        "## Message from {from} (purpose: {purpose})\n\
         _sent {sent_at} → {to}_\n\
         \n\
         {content}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the hook is wired into every Claude Code session start; failing
    /// it would block the session opening. Without a running daemon
    /// `read_daemon_addr` returns `None`, and we must degrade silently.
    /// What: pin a tempdir as the data directory, then call the handler
    /// with an unreachable daemon and assert it returns `Ok(())`.
    #[tokio::test]
    async fn inbox_check_returns_ok_without_daemon() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: tests serialise on `TRUSTY_DATA_DIR_OVERRIDE` by
        // convention; we only mutate inside this test's scope.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let res = handle_inbox_check(Some("test-palace".to_string())).await;
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            res.is_ok(),
            "missing daemon lockfile must degrade to Ok(()), got {res:?}"
        );
    }
}
