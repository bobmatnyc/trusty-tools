//! Slash-command and plain-text handlers plus the `chat.postMessage` senders
//! for the Slack gateway.
//!
//! Why: Command dispatch and message handling are the bulk of the adapter's
//! behavior; isolating them from the socket lifecycle, RBAC, pairing, and
//! formatting keeps each file focused and under the 500-line cap.
//! What: `handle_command`, `handle_message`, and the `post_message` /
//! `send_long_message` HTTP senders. The #3852 eventstream mirror lives in
//! the sibling `events` module and is re-exported here.
//! Test: Exercised indirectly; the pure helpers they call are unit-tested in
//! `slack::tests`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tracing::{info, warn};

use super::format::markdown_to_mrkdwn;
#[path = "send.rs"]
mod send;
use super::pairing::{
    PAIRING_CODE_TTL, PairOutcome, PendingPairs, SENTINEL_PAIRING_CHANNEL_ID, save_paired_channels,
    should_auto_pair, verify_pair_attempt,
};
use super::rbac::{SlackRbacConfig, VIRTUAL_CTO_MESSAGE, identity_from_slack_user};
use super::{ChannelId, ChatSession, PairedChannels, SessionMap};
pub(super) use send::{post_message, send_long_message};
// #4853: `record_listener_event` moved to `events.rs` when this file crossed
// the 500-SLOC cap. Re-exported here so the #3852 call site and its tests keep
// referring to it by the path they always have.
pub(super) use super::events::record_listener_event;
use crate::ctrl::{self, ConversationTurn};

/// Record a Slack slash command as a human turn, behind the same two gates
/// `handle_message` records behind (#4683).
///
/// Why: `handle_command` is the second of this transport's two inbound paths,
/// and it recorded nothing — so a human polling `/slack-status` while a long
/// task ran read as unattended after the threshold. The gate is composed here
/// rather than inline because it is a SECURITY decision, not plumbing: pairing
/// alone is a per-channel check, so an unknown Slack user posting in a paired
/// channel could otherwise manufacture attendance for someone else's assistant
/// and mute their notifications. `handle_message` already refuses to record for
/// an unknown user (it returns the Virtual CTO reply before its own hook); this
/// keeps the two paths agreeing.
/// What: records a turn of the caller-declared `origin` for `persona` at `now`
/// when the channel is paired AND the sender resolves to a known RBAC identity.
/// `origin` is threaded rather than assumed (#4685) so this transport obeys the
/// same rule every other surface does — no wrapper hardcodes humanity on a
/// caller's behalf. Infallible; returns whether the clock advanced.
/// Deliberately does NOT gate whether the command itself proceeds — an unknown
/// user's `/slack-status` still answers, exactly as before.
/// Test: `paired_slash_command_records_a_human_turn`,
/// `unpaired_slash_command_records_nothing`,
/// `unknown_rbac_user_cannot_manufacture_attendance`.
pub(super) fn note_command_turn(
    root: Option<&std::path::Path>,
    persona: &str,
    origin: crate::attendance::TurnOrigin,
    is_paired: bool,
    sender_is_known_to_rbac: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    crate::attendance::note_command_turn_in(
        root,
        persona,
        origin,
        is_paired && sender_is_known_to_rbac,
        now,
    )
}

/// Promote an unpaired channel to paired when the headless auto-pair rule
/// allows it, persisting the result (#4854).
///
/// Why: Both inbound paths (`handle_command`, `handle_message`) must apply the
/// identical rule — a divergence between them is exactly the class of bug that
/// makes a security gate meaningless. Composed once here and called from both.
/// The security decision itself lives in the pure `should_auto_pair`; this
/// wrapper is only the side effects.
/// What: Returns the channel's paired state after the attempt. Persists via
/// `save_paired_channels` on transition only (once per channel per boot), so a
/// DM does not rewrite the state file on every message. Log-and-continue on IO
/// error: an unwritable state file must not deny an authorized user service.
/// Test: `auto_pair_*` cover the decision; `slack_paired_state_round_trip`
/// covers the persistence this calls.
async fn ensure_paired(
    channel: &ChannelId,
    paired: &PairedChannels,
    paired_state_path: &std::path::Path,
    sender_is_known_to_rbac: bool,
) -> bool {
    if paired.read().await.contains_key(channel) {
        return true;
    }
    if !should_auto_pair(channel, sender_is_known_to_rbac) {
        return false;
    }
    paired.write().await.insert(channel.clone(), Instant::now());
    info!(channel = %channel, "slack: DM from known RBAC user auto-paired (#4854)");
    if let Err(e) = save_paired_channels(paired, paired_state_path).await {
        warn!(channel = %channel, error = %e, "failed to persist auto-pair");
    }
    true
}

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
    paired_state_path: Arc<PathBuf>,
    pending: PendingPairs,
    rbac: Arc<SlackRbacConfig>,
    // #4703: injected by `run_slack_bot`, not resolved here. The inline
    // `default_attendance_root()` this replaced is what made this handler's
    // attendance hook unobservable to any test.
    attendance_root: crate::attendance::AttendanceRoot,
) -> Result<()> {
    // Gate every command except /slack-start and /slack-pair behind the
    // pairing check. Unpaired channels get a uniform prompt.
    let is_unauthenticated = matches!(command.as_str(), "/slack-start" | "/slack-pair");
    // #4854: a DM from a user already in the RBAC table pairs itself. Without
    // this, standalone `--slack` mode (no REPL, so no code can ever be minted)
    // leaves the gate permanently shut.
    let is_paired = ensure_paired(
        &channel,
        &paired,
        &paired_state_path,
        rbac.user(&user_id).is_some(),
    )
    .await;
    if !is_unauthenticated && !is_paired {
        return post_message(
            bot_token,
            &channel,
            ":lock: Not paired. Send `/slack-start` to begin.",
            None,
        )
        .await;
    }

    // #4683: mirrors the Telegram fix — a paired human polling `/slack-status`
    // is attending, but only `handle_message` recorded it.
    let persona_for_attendance = {
        let map = sessions.lock().await;
        map.get(&channel)
            .map(|s| s.active_persona.clone())
            .unwrap_or_else(|| rbac.default_persona.clone())
    };
    note_command_turn(
        attendance_root.as_deref().map(PathBuf::as_path),
        &persona_for_attendance,
        // #4685: an inbound Slack command is a person typing.
        crate::attendance::TurnOrigin::Human,
        is_paired,
        rbac.user(&user_id).is_some(),
        chrono::Utc::now(),
    );

    match command.as_str() {
        "/slack-start" => {
            info!(channel = %channel, "Slack /slack-start received");
            // #4854: the old text unconditionally told the user to run
            // `/slack pair` in a REPL — impossible under the launchd gateway,
            // which has no REPL. Describe what this channel can actually do.
            let text = if is_paired {
                ":white_check_mark: *This channel is already paired.* Just send a message."
                    .to_string()
            } else if super::pairing::is_dm_channel(&channel) {
                // A DM that is still unpaired means the sender is not in the
                // RBAC table, so no self-service path exists — by design.
                ":lock: *Not authorized.*\n\nThis assistant is limited to configured team members. \
                 Ask an operator to add your Slack user id to the bot's access list."
                    .to_string()
            } else {
                ":lock: *Pairing required*\n\nShared channels must be paired explicitly. \
                 Ask an operator to run `/slack pair` in a trusty-agents REPL and send you the code, \
                 then run `/slack-pair <code>` here. (Codes expire in 5 minutes.)\n\n\
                 Or just DM me directly — no pairing step is needed there."
                    .to_string()
            };
            post_message(bot_token, &channel, &text, None).await
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
                    // #4853: persist so the pairing survives a restart. Mirrors
                    // the Telegram #467 handler: log-and-continue on IO error,
                    // because losing persistence is recoverable on the next
                    // save and must not block the user's confirmation.
                    if let Err(e) = save_paired_channels(&paired, &paired_state_path).await {
                        warn!(channel = %channel, error = %e, "failed to persist paired-channels state");
                    }
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
    paired_state_path: Arc<PathBuf>,
    rbac: Arc<SlackRbacConfig>,
    // #4703: injected by `run_slack_bot`. This handler used the
    // `$HOME`-resolving `note_turn`, so no test could observe its hook.
    attendance_root: crate::attendance::AttendanceRoot,
) -> Result<()> {
    // Gate behind pairing. #4854: a DM from a known RBAC user pairs itself —
    // see `should_auto_pair` for why that grants no capability RBAC does not
    // already grant. Every other channel still requires an explicit code.
    if !ensure_paired(
        &channel,
        &paired,
        &paired_state_path,
        rbac.user(&user_id).is_some(),
    )
    .await
    {
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
    match msg_ts.clone() {
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
    if let Some(ts) = msg_ts {
        let event_type = format!("message.{channel_type}");
        let event = crate::listeners::store::StoredEvent {
            id: format!("slack:{channel}:{ts}"),
            listener_id: "slack".into(),
            provider: "slack".into(),
            event_type: event_type.clone(),
            ts: chrono::Utc::now().to_rfc3339(),
            from: Some(user_cfg.name.clone()),
            subject: None,
            snippet: Some(text.clone()),
            included: crate::listeners::store::EventStore::is_event_type_included(&event_type)
                .await,
            labels: vec![],
        };
        let connected_project = channel_project(&sessions, &channel, &project_path).await;
        // #7427: was `receive_slack`; the inbound path is provider-neutral now,
        // so Slack, Telegram and gworkspace produce one wake envelope through
        // one function, and Slack names itself rather than being assumed.
        if crate::api::server::agent_channels::receive_inbound(
            "slack",
            &channel,
            &event,
            &connected_project,
            &user_identity,
            user_cfg.allowed_personas.as_deref(),
        )
        .await
        {
            return Ok(());
        }
    }

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

    // #4652: past the pairing and RBAC gates, this is a known human speaking —
    // record it as attendance for the persona they addressed.
    // #4703: through the injected root. Both gates above have already answered
    // "is this sender entitled to assert presence", so the paired argument is
    // `true` here; `origin` answers "was this a person at all".
    crate::attendance::note_command_turn_in(
        attendance_root.as_deref().map(PathBuf::as_path),
        &active_persona,
        crate::attendance::TurnOrigin::Human,
        true,
        chrono::Utc::now(),
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

async fn channel_project(
    sessions: &SessionMap,
    channel: &str,
    fallback: &std::path::Path,
) -> PathBuf {
    sessions
        .lock()
        .await
        .get(channel)
        .map(|session| session.project_path.clone())
        .unwrap_or_else(|| fallback.to_path_buf())
}
#[cfg(test)]
mod channel_tests {
    use super::*;
    #[tokio::test]
    async fn agent_channels_receive_honors_connected_project() {
        let sessions: SessionMap = Default::default();
        let connected = PathBuf::from("/tmp/connected-project");
        sessions.lock().await.insert(
            "C123".into(),
            ChatSession::new(connected.clone(), "assistant".into()),
        );
        assert_eq!(
            channel_project(&sessions, "C123", std::path::Path::new("/tmp/startup")).await,
            connected
        );
        assert_eq!(
            channel_project(&sessions, "COTHER", std::path::Path::new("/tmp/startup")).await,
            PathBuf::from("/tmp/startup")
        );
    }
}
