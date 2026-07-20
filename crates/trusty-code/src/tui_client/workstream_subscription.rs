//! The long-lived `GET /workstreams/{id}/events` background subscription
//! `TuiEngine::subscribe_workstream_events` spawns (issue #3415, DOC-48
//! §5.3).
//!
//! Why: split out of `engine.rs` (issue #610's 500-SLOC production-file
//! cap) — this is the one self-contained loop with its own wire-parsing
//! helper, so it earns its own file rather than staying inline alongside
//! `EngineState`.
//! What: [`WireWorkstreamEnvelope`]/[`parse_workstream_envelope`] (the
//! AC-7.2 wire shape this client deserialises SSE payloads into) and
//! [`run_workstream_subscription`] (the reconnect loop itself, spawned via
//! `tokio::spawn` by `engine.rs`'s `TuiEngine::subscribe_workstream_events`).
//! Test: `engine_tests::parse_workstream_envelope_round_trips_activation_changed`
//! (in the sibling `engine_tests.rs`, included from `engine.rs`); the full
//! reconnect-to-a-new-workstream behaviour is covered end-to-end against a
//! mock daemon in `tests/tui_client_engine.rs`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::mpsc::UnboundedSender;
use trusty_tui::ReplEvent;

use crate::events::Event;

use super::engine::{RECONNECT_BACKOFF, SSE_IDLE_TIMEOUT};
use super::engine_state::EngineState;
use super::sse::SseLines;

/// The AC-7.2 wire envelope this client deserialises off
/// `GET /workstreams/{id}/events` — mirrors
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

pub(super) fn parse_workstream_envelope(payload_json: &str) -> Option<WireWorkstreamEnvelope> {
    serde_json::from_str(payload_json).ok()
}

/// Long-lived background loop for `subscribe_workstream_events`: holds one
/// SSE connection to `GET /workstreams/{current_id}/events` at a time,
/// reconnecting (to a possibly-NEW workstream id, per DOC-48 §5.3 point 5)
/// on activation changes, transport errors, or idle timeouts, until
/// `state.shutting_down` is set.
pub(super) async fn run_workstream_subscription(
    state: Arc<EngineState>,
    mut current_id: String,
    tx: UnboundedSender<ReplEvent>,
) {
    loop {
        if state.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        let url = format!("{}/workstreams/{current_id}/events", state.rpc.base_url());
        let resp = match state.rpc.http().get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => {
                let _ = tx.send(ReplEvent::ConnectionLost {
                    reason: format!(
                        "could not reach workstream events endpoint for {current_id}; retrying…"
                    ),
                });
                tokio::time::sleep(RECONNECT_BACKOFF).await;
                continue;
            }
        };

        let mut lines = SseLines::new(resp);
        loop {
            if state.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            match tokio::time::timeout(SSE_IDLE_TIMEOUT, lines.next_data()).await {
                Ok(Ok(Some(payload))) => {
                    let Some(env) = parse_workstream_envelope(&payload) else {
                        continue;
                    };
                    if let Event::WorkstreamActivationChanged {
                        new_active_id: Some(new_id),
                        prior_id,
                    } = env.payload
                    {
                        let _ = tx.send(ReplEvent::WorkstreamActivationChanged {
                            new_active_id: new_id.clone(),
                            prior_id,
                        });
                        state.refresh_workstream_cache().await;
                        if new_id != current_id {
                            current_id = new_id;
                            break; // reconnect to the newly-active workstream's endpoint
                        }
                    }
                }
                Ok(Ok(None)) => {
                    let _ = tx.send(ReplEvent::ConnectionLost {
                        reason: "workstream event stream closed; reconnecting…".to_string(),
                    });
                    tokio::time::sleep(RECONNECT_BACKOFF).await;
                    break;
                }
                Ok(Err(_)) | Err(_) => {
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
