//! Slash-command and plain-text handlers plus the `chat.postMessage` senders
//! for the Slack gateway.
//!
//! Why: Command dispatch and message handling are the bulk of the adapter's
//! behavior; isolating them from the socket lifecycle, RBAC, pairing, and
//! formatting keeps each file focused and under the 500-line cap.
//! What: `handle_command`, `handle_message`, and the `post_message` /
//! `send_long_message` HTTP senders. Also `record_listener_event` (#3852
//! hybrid architecture): mirrors every inbound plain message onto the
//! harness-wide eventstream (`crate::listeners::store::EventStore` +
//! `Event::ListenerEventReceived`) IN ADDITION TO the direct
//! dispatch/reply path below, which is unchanged — see that function's doc
//! comment and the #3852 ADR (`docs/adr/`) for the fork rationale.
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
use crate::listeners::store::{EventStore, StoredEvent};

/// Fixed listener id used for every Slack-gateway event mirrored onto the
/// eventstream (#3852). Unlike Gmail (one `ListenerConfig` per configured
/// mailbox), the Socket-Mode gateway is a single process-wide connection —
/// there is no per-workspace listener config to derive an id from.
const SLACK_LISTENER_ID: &str = "slack";

/// Max chars of message text folded into a `ListenerEventReceived` summary
/// line (#3852) — mirrors the "one glanceable line" contract
/// `listeners::poll::listener_event_summary` documents for Gmail.
const SLACK_SNIPPET_MAX_CHARS: usize = 140;

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
                "To link this Slack channel, go to your trusty-agents REPL and run:\n\n",
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
    msg_ts: Option<String>,
    channel_type: String,
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

    // #3852 hybrid architecture: mirror this inbound message onto the
    // harness-wide eventstream BEFORE any RBAC/dispatch branching below, so
    // the Events pane sees every message in a paired channel — known RBAC
    // user or not — exactly like a Gmail poll event. The dispatch/reply path
    // beneath this is completely unchanged; see `record_listener_event`'s
    // doc comment for the append-then-filter contract it mirrors. Detached
    // via `tokio::spawn` — exactly like the `relay_event` mirror below —
    // with owned args, so a slow disk (the store append) never delays the
    // Slack reply itself.
    match msg_ts {
        Some(ts) => {
            let from_display = rbac
                .user(&user_id)
                .map(|u| u.name.clone())
                .unwrap_or_else(|| user_id.clone());
            tokio::spawn(record_listener_event(
                channel.clone(),
                ts,
                channel_type.clone(),
                from_display,
                text.clone(),
            ));
        }
        None => {
            warn!(channel = %channel, "slack: message event missing ts; skipping eventstream mirror");
        }
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

    // #3752: mirror the inbound human message to the GUI live-activity pane.
    // Detached (fire-and-forget) so a slow/absent `--api` process never delays
    // the Slack reply; the honest RBAC-tier badge comes from the resolved
    // identity, not an invented auth state.
    tokio::spawn(super::relay::relay_event(
        crate::events::Event::SlackMessageReceived {
            channel: channel.clone(),
            user_display: user_cfg.name.clone(),
            text: text.clone(),
            tier: super::relay::tier_label(&user_identity.tier).to_string(),
        },
    ));

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

    let send_result =
        send_long_message(bot_token, &channel, thread_ts.as_deref(), &response_text).await;

    // #3752: mirror the reply to the GUI only after it was actually posted to
    // Slack, so the pane never shows a reply the channel never received. The
    // identity label is honest — the bot replies as itself (no impersonation
    // mode exists). Detached so relay latency/failure never affects the reply.
    if send_result.is_ok() {
        tokio::spawn(super::relay::relay_event(
            crate::events::Event::SlackReplySent {
                channel: channel.clone(),
                text: response_text.clone(),
                identity: super::relay::BOT_IDENTITY.to_string(),
            },
        ));
    }

    send_result
}

/// Mirror one inbound Slack message onto the harness-wide listener
/// eventstream (#3852 hybrid architecture).
///
/// Why: The Socket-Mode gateway already answers Slack DMs directly —
/// `handle_message` above dispatches to `ctrl::run_pm_task_with_persona` and
/// replies via `chat.postMessage`, unchanged by this function. This
/// ADDITIONALLY makes the message visible to the Events pane / filterable
/// eventstream (`GET /api/listener-events`), mirroring EXACTLY the
/// append-then-filter order `listeners::poll::poll_once` uses for Gmail:
/// append the event (best-effort — a failure is logged, NOT fatal to the
/// rest of this function, matching `poll_once`'s own log-and-continue
/// posture at `listeners/poll.rs:326-331` rather than early-returning),
/// THEN consult `EventStore::is_event_type_included` for the CURRENT filter
/// state, THEN publish `Event::ListenerEventReceived` carrying that
/// `included` flag REGARDLESS of whether the append succeeded — a
/// persistence hiccup must not also blind the LIVE Events pane, which reads
/// the SSE mirror, not the on-disk log. Deliberately does NOT call
/// `listeners::wake` — direct dispatch already answers the message; waking a
/// bound agent here too would make it reply twice (see the #3852 ADR for the
/// fork rationale).
/// What: Takes owned `String`s (not `&str`) so callers can `tokio::spawn`
/// this directly — see the call site in `handle_message`, which detaches it
/// exactly like the sibling `relay_event` spawn so a slow disk append never
/// delays the Slack reply. `listener_id` is the fixed `SLACK_LISTENER_ID`
/// (no per-workspace `ListenerConfig` exists for the Socket-Mode gateway the
/// way Gmail has one per mailbox); `event_type` is `"message.{channel_type}"`
/// (e.g. `message.im`, `message.mpim`); `id` is `"slack:{channel}:{ts}"` —
/// stable and idempotent per Slack message, mirroring Gmail's
/// `"{listener_id}:{message_id}"`.
/// Test: `slack_listener_event_appends_and_respects_filter`,
/// `slack_listener_event_excluded_type_still_appended`,
/// `slack_listener_event_publishes_even_when_append_fails`.
pub(super) async fn record_listener_event(
    channel: String,
    ts: String,
    channel_type: String,
    from_display: String,
    text: String,
) {
    let event_type = format!("message.{channel_type}");
    let event = StoredEvent {
        id: format!("slack:{channel}:{ts}"),
        listener_id: SLACK_LISTENER_ID.to_string(),
        provider: "slack".to_string(),
        event_type: event_type.clone(),
        ts: chrono::Utc::now().to_rfc3339(),
        from: Some(from_display),
        subject: None,
        snippet: Some(truncated_snippet(&text)),
        included: true,
    };
    if let Err(e) = EventStore::append(&event).await {
        warn!(
            channel = %channel,
            error = %e,
            "slack: failed to persist listener event (non-fatal); still publishing SSE mirror"
        );
        // Fall through, deliberately — see the doc comment above and
        // `listeners::poll::poll_once`, which does the same on an append
        // error: a persistence failure must not also suppress the live SSE
        // mirror the Events pane reads from.
    }
    let included = EventStore::is_event_type_included(&event_type).await;
    crate::events::publish(crate::events::Event::ListenerEventReceived {
        listener_id: event.listener_id.clone(),
        provider: event.provider.clone(),
        event_type: event.event_type.clone(),
        summary: format!("{channel}: {}", truncated_snippet(&text)),
        included,
    });
}

/// Char-boundary-safe truncation of Slack message text to
/// `SLACK_SNIPPET_MAX_CHARS`, for the `StoredEvent::snippet` and
/// `ListenerEventReceived::summary` fields (#3852).
fn truncated_snippet(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(SLACK_SNIPPET_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
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
