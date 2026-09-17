//! The TUI's always-on prompter claim on its session (#8184).
//!
//! Why: the daemon only suspends a tool call on an `ask` rule while SOMEONE
//! is watching that session and can answer (#8100 — see
//! `crate::session::registry_prompters`); with no prompter the gate denies at
//! once. The only thing that claims a prompter on this transport is an open
//! `session.events` stream (`crate::session::events_stream::open`), and
//! `EngineState::run_chat_turn` used to issue `task.run` BEFORE opening one.
//! Since #8184 the interactive default runs the solo agent, whose first
//! `write_file`/`bash` call is gated, so that ordering raced the stream open:
//! a call that lost the race was denied with no prompt the user could answer.
//! Every reconnect gap in `pump_session_events` reopened the same hole.
//! What: [`hold_prompter_claim`] opens one dedicated `session.events` stream
//! at `setup()` time, CONFIRMS the claim exists before returning (the daemon
//! claims it inside its handler, before the first frame, so one received
//! frame proves it), and hands the stream to a background task that drains
//! and discards frames — `pump_session_events` is what renders — reconnecting
//! until the engine shuts down. The per-turn pump still claims its own
//! prompter while it is up, so a gap now needs BOTH streams down at once.
//! Test: `tests/tui_client_engine.rs::setup_opens_a_prompter_claim_stream`
//! pins the ordering deterministically (the behavioural form — losing the
//! race against an in-process mock LLM — is inherently timing-dependent, so
//! the wire order is what is asserted).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::engine::RECONNECT_BACKOFF;
use super::engine_state::EngineState;
use super::error::EngineError;

/// Open the claim stream and confirm it before returning.
///
/// # Errors
///
/// The dial failure, so `setup()` reports a daemon that answered `session.create`
/// and then refused a stream rather than starting a session whose every gated
/// call would be denied unprompted.
/// Test: see the module docs.
pub(super) async fn hold_prompter_claim(
    state: Arc<EngineState>,
    session_id: String,
) -> Result<(), EngineError> {
    let mut stream = open(&state, &session_id).await?;
    // A freshly created session always has ring events to replay, so this
    // resolves immediately; it is the handshake that proves the daemon ran
    // `session.events`' handler — and therefore took the claim — not a wait
    // for anything the user did.
    let _confirmed = stream.next_frame().await;
    tokio::spawn(async move {
        loop {
            while let Some(Ok(_)) = stream.next_frame().await {
                // Drained and dropped on purpose: this stream exists to hold
                // the claim, and `pump_session_events` owns rendering.
            }
            if state.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(RECONNECT_BACKOFF).await;
            match open(&state, &session_id).await {
                Ok(reopened) => stream = reopened,
                Err(e) => {
                    tracing::debug!(session_id = %session_id, "prompter claim reopen failed: {e}");
                }
            }
        }
    });
    Ok(())
}

/// One `session.events` dial, tailing only what arrives from now on.
async fn open(
    state: &Arc<EngineState>,
    session_id: &str,
) -> Result<trusty_common::uds::FramedStream<Value>, EngineError> {
    state
        .rpc
        .open_stream::<Value>("session.events", json!({"session_id": session_id}))
        .await
}
