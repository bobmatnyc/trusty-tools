//! The long-lived `workstream.events` background subscription
//! `TuiEngine::subscribe_workstream_events` spawns (issue #3415, DOC-48 §5.3;
//! moved off the `GET /workstreams/{id}/events` SSE route in #6637).
//!
//! Why: split out of `engine.rs` (issue #610's 500-SLOC production-file
//! cap) — this is the one self-contained loop with its own wire-parsing
//! helper, so it earns its own file rather than staying inline alongside
//! `EngineState`.
//! What: [`WireWorkstreamEnvelope`] (the AC-7.2 wire shape this client
//! deserialises stream frames into) and [`run_workstream_subscription`] (the
//! reconnect loop itself, spawned via `tokio::spawn` by `engine.rs`'s
//! `TuiEngine::subscribe_workstream_events`).
//!
//! **The reconnect is bare, with no cursor.** `workstream.events` has no
//! `after_seq` and cannot have one — see
//! `crate::workstreams::events_stream`'s module docs for why a workstream has
//! no ordering of its own to resume from.
//! Test: `engine_tests::parse_workstream_envelope_round_trips_activation_changed`
//! (in the sibling `engine_tests.rs`, included from `engine.rs`); the full
//! reconnect-to-a-new-workstream behaviour is covered end-to-end against a
//! real daemon socket in `tests/tui_client_engine.rs`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::mpsc::UnboundedSender;
use trusty_code_tui::ReplEvent;

use crate::events::Event;

use super::engine::RECONNECT_BACKOFF;
use super::engine_state::EngineState;

/// The AC-7.2 wire envelope this client deserialises off
/// `workstream.events` — mirrors
/// `crate::workstreams::sse::WorkstreamEventEnvelope`
/// (`trusty_agents_common::transport::EventEnvelope<Event>`) exactly, but
/// with `Deserialize` (the shared type only derives `Serialize` — it's
/// built server-side, never parsed) — see module docs for why a small
/// mirror struct here is preferable to adding `Deserialize` to the shared
/// type for one client.
#[derive(Debug, serde::Deserialize)]
pub(super) struct WireWorkstreamEnvelope {
    #[allow(dead_code)]
    session_id: String,
    #[allow(dead_code)]
    event_type: String,
    pub(super) payload: Event,
}

/// Parse one AC-7.2 envelope from its JSON text.
///
/// Test-only since #6637: the stream reader decodes frames into
/// [`WireWorkstreamEnvelope`] directly, so nothing in production parses a
/// string. `engine_tests::parse_workstream_envelope_round_trips_activation_changed`
/// still pins the wire field names against DOC-48 §5.3, which is what this
/// keeps it alive for.
#[cfg(test)]
pub(super) fn parse_workstream_envelope(payload_json: &str) -> Option<WireWorkstreamEnvelope> {
    serde_json::from_str(payload_json).ok()
}

/// Long-lived background loop for `subscribe_workstream_events`: holds one
/// `workstream.events` stream on `current_id` at a time, reconnecting (to a
/// possibly-NEW workstream id, per DOC-48 §5.3 point 5) on activation changes,
/// stream errors, or an early end, until `state.shutting_down` is set.
pub(super) async fn run_workstream_subscription(
    state: Arc<EngineState>,
    mut current_id: String,
    tx: UnboundedSender<ReplEvent>,
) {
    loop {
        if state.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        let mut stream = match state
            .rpc
            .open_stream::<WireWorkstreamEnvelope>(
                "workstream.events",
                serde_json::json!({"workstream_id": current_id}),
            )
            .await
        {
            Ok(stream) => stream,
            Err(_) => {
                let _ = tx.send(ReplEvent::ConnectionLost {
                    reason: format!(
                        "could not open the workstream event stream for {current_id}; retrying…"
                    ),
                });
                tokio::time::sleep(RECONNECT_BACKOFF).await;
                continue;
            }
        };

        loop {
            // Checked between frames rather than on a timer: `next_frame` is
            // not cancel-safe (a cancelled read loses whatever it had
            // buffered), so this loop cannot race it against a poll. A TUI
            // that quits while the stream is silent leaves this task parked
            // until the process exits, which is where a detached task ends
            // anyway.
            if state.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            match stream.next_frame().await {
                Some(Ok(env)) => {
                    if let Event::WorkstreamActivationChanged {
                        new_active_id,
                        prior_id,
                    } = env.payload
                    {
                        // Always refresh — a `None` new_active_id (this
                        // workstream deactivated with no replacement, DOC-48
                        // §4.2/§4.3) is just as real a state change as an
                        // activation and must invalidate the cache too
                        // (HIGH finding: silently skipping this arm left
                        // `active_workstream`/the `"workstream"` picker
                        // stale indefinitely).
                        //
                        // Use the return value (Slice 6, #3418): it only
                        // clears `EngineState::active_workstream` (this
                        // client's OWN cache, used by `/workstream list`'s
                        // marker and `subscribe_workstream_events`'s next
                        // `current_id`) — the shared `ReplApp::active_workstream`
                        // the STATUS LINE actually renders from lives entirely
                        // on the `trusty-code-tui` side and is populated ONLY by
                        // `ReplEvent::WorkstreamUpdated`. Discarding this
                        // return value (as the code did before this fix) left
                        // the status line showing a stale workstream name
                        // forever after every activation change — a
                        // `WorkstreamUpdated` was never actually sent, despite
                        // `trusty-code-tui`'s reducer being written to expect one
                        // as this event's follow-up.
                        let refreshed = state.refresh_workstream_cache().await;
                        if let Some(ws) = refreshed {
                            let _ = tx.send(ReplEvent::WorkstreamUpdated(ws));
                        }
                        // Push the status line's Workstream segment in step
                        // with the cache — `statusline_segments()` reads the
                        // SAME `active_workstream` mutex `refresh_workstream_cache`
                        // just wrote, so this is `[Workstream{..}]` when
                        // `refreshed` was `Some` above and `[]` (collapsing
                        // the segment away, not leaving it stale) when the
                        // daemon reports no active workstream — covers BOTH
                        // the `Some(new_id)` and `None` (deactivation) arms
                        // below in one place.
                        let _ = tx.send(ReplEvent::StatuslineUpdate(state.statusline_segments()));
                        match new_active_id {
                            Some(new_id) => {
                                let _ = tx.send(ReplEvent::WorkstreamActivationChanged {
                                    new_active_id: Some(new_id.clone()),
                                    prior_id,
                                });
                                if new_id != current_id {
                                    current_id = new_id;
                                    break; // reconnect to the newly-active workstream's endpoint
                                }
                            }
                            None => {
                                // Deactivated, no replacement active. The
                                // shared `ReplEvent::WorkstreamActivationChanged`
                                // (`trusty_code_tui::event`) now carries
                                // `new_active_id: Option<String>`, so this
                                // state is representable structurally rather
                                // than as free text — `ReplEvent::WorkstreamUpdated`
                                // cannot represent "no active workstream" (its
                                // payload is a concrete `WorkstreamSummary`,
                                // not `Option`), which is exactly why this
                                // event exists: `trusty-code-tui`'s reducer clears
                                // `ReplApp::active_workstream` directly on
                                // `new_active_id: None`, rather than waiting
                                // for a `WorkstreamUpdated` that will never
                                // come (see `refreshed` above — `None` here
                                // too, since the daemon reports no active
                                // workstream). Stay connected to
                                // `current_id`'s stream rather than
                                // reconnecting anywhere: deactivation is not
                                // closure (DOC-48 §4.4), and
                                // `crate::workstreams::sse`'s `classify()`
                                // still forwards a LATER activation naming
                                // `current_id` as its `prior_id` to this same
                                // connection.
                                let _ = tx.send(ReplEvent::WorkstreamActivationChanged {
                                    new_active_id: None,
                                    prior_id,
                                });
                            }
                        }
                    }
                }
                None => {
                    let _ = tx.send(ReplEvent::ConnectionLost {
                        reason: "workstream event stream closed; reconnecting…".to_string(),
                    });
                    tokio::time::sleep(RECONNECT_BACKOFF).await;
                    break;
                }
                Some(Err(_)) => {
                    let _ = tx.send(ReplEvent::ConnectionLost {
                        reason: "workstream event stream error; reconnecting…".to_string(),
                    });
                    tokio::time::sleep(RECONNECT_BACKOFF).await;
                    break;
                }
            }
        }
    }
}
