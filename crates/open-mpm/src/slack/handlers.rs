//! Slash-command and plain-text handlers plus the `chat.postMessage` senders
//! for the Slack gateway.
//!
//! Why: Command dispatch and message handling are the bulk of the adapter's
//! behavior; isolating them from the socket lifecycle, RBAC, pairing, and
//! formatting keeps each file focused and under the 500-line cap.
//! What: `handle_command`, `handle_message`, and the `post_message` /
//! `send_long_message` HTTP senders.
//! Test: Exercised indirectly; the pure helpers they call are unit-tested in
//! `slack::tests`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use serde_json::Value;
use tracing::{info, warn};

use super::format::{MAX_SLACK_MESSAGE, markdown_to_mrkdwn, split_message};
use super::pairing::{
    PAIRING_CODE_TTL, PairOutcome, PendingPairs, SENTINEL_PAIRING_CHANNEL_ID, verify_pair_attempt,
};
use super::rbac::{SlackRbacConfig, VIRTUAL_CTO_MESSAGE, identity_from_slack_user};
use super::{ChannelId, ChatSession, PairedChannels, SessionMap};
use crate::ctrl::{self, ConversationTurn};

/// Slash command dispatch.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_command(
    bot_token: &str,
    channel: ChannelId,
    user_id: String,
    command: String,
    arg: String,
    sessions: SessionMap,
    project_path: Arc<PathBuf>,
    paired: PairedChannels,
    pending: PendingPairs,
    rbac: Arc<SlackRbacConfig>,
) -> Result<()> {
    // Gate every command except /slack-start and /slack-pair behind the
    // pairing check. Unpaired channels get a uniform prompt.
    let is_unauthenticated = matches!(command.as_str(), "/slack-start" | "/slack-pair");
    if !is_unauthenticated {
        let is_paired = paired.read().await.contains_key(&channel);
        if !is_paired {
            return post_message(
                bot_token,
                &channel,
                ":lock: Not paired. Send `/slack-start` to begin.",
                None,
            )
            .await;
        }
    }

    match command.as_str() {
        "/slack-start" => {
            info!(channel = %channel, "Slack /slack-start received");
            let text = concat!(
                ":lock: *Pairing required*\n\n",
                "To link this Slack channel, go to your open-mpm REPL and run:\n\n",
                "  `/slack pair`\n\n",
                "Then send the code here: `/slack-pair <code>`\n\n",
                "(Codes expire in 5 minutes.)"
            );
            post_message(bot_token, &channel, text, None).await
        }
        "/slack-pair" => {
            let provided = arg.trim().to_string();
            if provided.is_empty() {
                return post_message(bot_token, &channel, "Usage: `/slack-pair <code>`", None)
                    .await;
            }
            let now = Instant::now();
            let (outcome, matched_key) = {
                let map = pending.lock().await;
                let sentinel_outcome = verify_pair_attempt(
                    map.get(&SENTINEL_PAIRING_CHANNEL_ID),
                    &provided,
                    now,
                    PAIRING_CODE_TTL,
                );
                (sentinel_outcome, SENTINEL_PAIRING_CHANNEL_ID)
            };
            match outcome {
                PairOutcome::NoPending => {
                    post_message(
                        bot_token,
                        &channel,
                        "No pending pairing. Run `/slack pair` in the REPL first.",
                        None,
                    )
                    .await
                }
                PairOutcome::Expired => {
                    pending.lock().await.remove(&matched_key);
                    post_message(
                        bot_token,
                        &channel,
                        "Code expired. Run `/slack pair` in the REPL to get a new code.",
                        None,
                    )
                    .await
                }
                PairOutcome::Mismatch => {
                    post_message(bot_token, &channel, "Invalid code.", None).await
                }
                PairOutcome::Success => {
                    pending.lock().await.remove(&matched_key);
                    paired.write().await.insert(channel.clone(), now);
                    info!(channel = %channel, "Slack channel paired successfully");
                    post_message(
                        bot_token,
                        &channel,
                        ":white_check_mark: *Paired successfully.* You can now send messages.",
                        None,
                    )
                    .await
                }
            }
        }
        "/slack-connect" => {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                return post_message(bot_token, &channel, "Usage: `/slack-connect <path>`", None)
                    .await;
            }
            let new_path = PathBuf::from(trimmed);
            if !new_path.is_dir() {
                return post_message(
                    bot_token,
                    &channel,
                    &format!("Path does not exist or is not a directory: `{}`", trimmed),
                    None,
                )
                .await;
            }
            {
                let mut map = sessions.lock().await;
                let entry = map.entry(channel.clone()).or_insert_with(|| {
                    ChatSession::new((*project_path).clone(), rbac.default_persona.clone())
                });
                entry.project_path = new_path.clone();
            }
            post_message(
                bot_token,
                &channel,
                &format!("Connected to `{}`", new_path.display()),
                None,
            )
            .await
        }
        "/slack-clear" => {
            let mut map = sessions.lock().await;
            if let Some(session) = map.get_mut(&channel) {
                session.history.clear();
            }
            drop(map);
            post_message(bot_token, &channel, "Conversation history cleared.", None).await
        }
        "/slack-switch" => {
            let requested = arg.trim().to_string();
            if requested.is_empty() {
                return post_message(
                    bot_token,
                    &channel,
                    "Usage: `/slack-switch <persona>`",
                    None,
                )
                .await;
            }
            // Resolve the requesting Slack user from RBAC.
            let user_cfg = match rbac.user(&user_id) {
                Some(u) => u.clone(),
                None => {
                    return post_message(bot_token, &channel, ":lock: Not authorized.", None).await;
                }
            };
            // RBAC enforcement: persona allow-list. `None` => unrestricted.
            if let Some(allowed) = &user_cfg.allowed_personas
                && !allowed.iter().any(|p| p == &requested)
            {
                info!(
                    user_id = %user_id,
                    persona = %requested,
                    "slack: /slack-switch rejected (persona not in allow-list)"
                );
                return post_message(
                    bot_token,
                    &channel,
                    &format!(
                        ":lock: Not authorized to switch to *{}*. Allowed: {}",
                        requested,
                        allowed.join(", ")
                    ),
                    None,
                )
                .await;
            }
            {
                let mut map = sessions.lock().await;
                let entry = map.entry(channel.clone()).or_insert_with(|| {
                    ChatSession::new((*project_path).clone(), rbac.default_persona.clone())
                });
                entry.active_persona = requested.clone();
            }
            info!(user_id = %user_id, persona = %requested, "slack: persona switched");
            post_message(
                bot_token,
                &channel,
                &format!(":arrows_counterclockwise: Switched to *{}*", requested),
                None,
            )
            .await
        }
        "/slack-status" => {
            let map = sessions.lock().await;
            let path = map
                .get(&channel)
                .map(|s| s.project_path.clone())
                .unwrap_or_else(|| (*project_path).clone());
            let history_len = map.get(&channel).map(|s| s.history.len()).unwrap_or(0);
            let persona = map
                .get(&channel)
                .map(|s| s.active_persona.clone())
                .unwrap_or_else(|| rbac.default_persona.clone());
            drop(map);

            let llm_label = crate::llm::credentials::pick_credentials(None)
                .map(|c| c.label())
                .unwrap_or("none");
            let text = format!(
                "*Status*\n\nProject:  `{}`\nPersona:  `{}`\nTurns:    {}\nLLM:      `{}`",
                path.display(),
                persona,
                history_len,
                llm_label
            );
            post_message(bot_token, &channel, &text, None).await
        }
        other => {
            warn!(command = %other, "slack: unknown slash command");
            Ok(())
        }
    }
}

/// Forward a plain-text message to ctrl and reply with the result.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_message(
    bot_token: &str,
    channel: ChannelId,
    user_id: String,
    text: String,
    thread_ts: Option<String>,
    sessions: SessionMap,
    project_path: Arc<PathBuf>,
    paired: PairedChannels,
    rbac: Arc<SlackRbacConfig>,
) -> Result<()> {
    // Gate behind pairing.
    if !paired.read().await.contains_key(&channel) {
        return post_message(
            bot_token,
            &channel,
            ":lock: Not paired. Send `/slack-start` to begin.",
            thread_ts.as_deref(),
        )
        .await;
    }

    // #481: RBAC identity gate. Unknown Slack users get the static Virtual
    // CTO reply — no LLM call, no tool dispatch.
    let user_cfg = match rbac.user(&user_id) {
        Some(u) => u.clone(),
        None => {
            info!(user_id = %user_id, "slack: unknown user → virtual CTO reply");
            return send_long_message(
                bot_token,
                &channel,
                thread_ts.as_deref(),
                VIRTUAL_CTO_MESSAGE,
            )
            .await;
        }
    };
    let user_identity = identity_from_slack_user(&user_cfg);

    let (path, history_snapshot, active_persona) = {
        let mut map = sessions.lock().await;
        let entry = map.entry(channel.clone()).or_insert_with(|| {
            ChatSession::new((*project_path).clone(), rbac.default_persona.clone())
        });
        // Cache the resolved identity so it isn't re-looked-up per turn.
        entry.user_identity = Some(user_identity.clone());
        (
            entry.project_path.clone(),
            entry.history.clone(),
            entry.active_persona.clone(),
        )
    };

    info!(
        user_id = %user_id,
        user_name = %user_cfg.name,
        persona = %active_persona,
        "slack dispatch"
    );

    let result = ctrl::run_pm_task_with_persona(
        &path,
        &active_persona,
        &text,
        &history_snapshot,
        None,
        ctrl::SessionOverrides {
            user: Some(user_identity),
            ..Default::default()
        },
    )
    .await;

    let response_text = match result {
        Ok(reply) => {
            let mut map = sessions.lock().await;
            let entry = map.entry(channel.clone()).or_insert_with(|| {
                ChatSession::new((*project_path).clone(), rbac.default_persona.clone())
            });
            entry.history.push(ConversationTurn {
                user: text.clone(),
                assistant: reply.clone(),
            });
            drop(map);
            markdown_to_mrkdwn(&reply)
        }
        Err(e) => {
            warn!(channel = %channel, error = %e, "ctrl dispatch failed");
            ":warning: LLM backend not configured. Set `CLAUDE_CODE_OAUTH_TOKEN`, \
             `ANTHROPIC_API_KEY`, or `OPENROUTER_API_KEY`."
                .to_string()
        }
    };

    send_long_message(bot_token, &channel, thread_ts.as_deref(), &response_text).await
}

/// Post a single message via `chat.postMessage`.
pub(super) async fn post_message(
    bot_token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<()> {
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
pub(super) async fn send_long_message(
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
