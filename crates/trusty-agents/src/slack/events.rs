//! Eventstream mirror for the Slack gateway (#3852 hybrid architecture).
//!
//! Why: The Socket-Mode gateway answers Slack DMs directly — `handlers`
//! dispatches to ctrl and replies via `chat.postMessage`. Mirroring each
//! inbound message onto the harness-wide eventstream is a SEPARATE concern
//! with its own failure posture (append is best-effort; the live SSE mirror
//! publishes regardless), so it lives in its own file. Split out of
//! `handlers.rs` under #4853, when that file crossed the 500-SLOC cap.
//! What: `record_listener_event` plus its snippet helper and the two id/size
//! constants it owns.
//! Test: `slack::tests::eventstream_tests`.

use tracing::warn;

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
