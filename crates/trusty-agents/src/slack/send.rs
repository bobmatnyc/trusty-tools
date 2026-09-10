//! Slack response delivery helpers shared by command and message handlers.
//! Existing slack::tests cover validation and splitting behavior.
use super::super::format::{MAX_SLACK_MESSAGE, split_message};
use anyhow::{Result, anyhow};
use serde_json::Value;
use tracing::warn;
/// Post a single message via `chat.postMessage`.
pub(crate) async fn post_message(
    bot_token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<()> {
    // #4703: refuse to issue a request that cannot succeed. `chat.postMessage`
    // without a bearer token always answers `not_authed`, and the client built
    // here carries NO timeout — so on a network that blackholes rather than
    // refuses, a doomed request does not fail, it HANGS, holding whichever
    // handler called it. Failing closed locally is strictly better.
    //
    // This is an ERROR, not a silent `Ok(())`. Reporting success for a message
    // that was never sent is the worse bug: `handle_message` mirrors its reply
    // to the GUI only `if send_result.is_ok()`, so a swallowed failure would
    // show the operator a reply the Slack channel never received.
    // Test: `post_message_without_a_token_errors_instead_of_requesting`.
    if bot_token.is_empty() {
        warn!(channel, "chat.postMessage refused: no bot token configured");
        return Err(anyhow!(
            "chat.postMessage: no bot token configured; message not sent"
        ));
    }
    let mut body = serde_json::Map::new();
    body.insert("channel".to_string(), Value::String(channel.to_string()));
    body.insert("text".to_string(), Value::String(text.to_string()));
    body.insert("mrkdwn".to_string(), Value::Bool(true));
    if let Some(ts) = thread_ts {
        body.insert("thread_ts".to_string(), Value::String(ts.to_string()));
    }
    let resp = reqwest::Client::new()
        .post("https://slack.com/api/chat.postMessage")
        .bearer_auth(bot_token)
        .json(&Value::Object(body))
        .send()
        .await
        .map_err(|e| anyhow!("chat.postMessage failed: {}", e))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("chat.postMessage: bad json (status {status}): {e}"))?;
    if !body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        warn!(error = %err, "chat.postMessage returned not-ok");
    }
    Ok(())
}

/// Send a (possibly long) mrkdwn reply, splitting on the 3000-char boundary
/// at newlines where possible. Thread reply attached to all chunks for
/// coherence (Slack threads tolerate this, unlike Telegram replies).
pub(crate) async fn send_long_message(
    bot_token: &str,
    channel: &str,
    thread_ts: Option<&str>,
    text: &str,
) -> Result<()> {
    let chunks = split_message(text, MAX_SLACK_MESSAGE);
    for chunk in chunks.iter() {
        if let Err(e) = post_message(bot_token, channel, chunk, thread_ts).await {
            warn!(channel = %channel, error = %e, "slack chunk post failed");
        }
    }
    Ok(())
}
