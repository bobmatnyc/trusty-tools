//! The TUI's always-on prompter claim on its session (#8184).
//!
//! Why: the daemon only suspends a tool call on an `ask` rule while SOMEONE
//! is watching that session (#8100 — see
//! `crate::session::registry_prompters`); with no prompter the gate denies at
//! once. The only thing that claims a prompter on this transport is an open
//! `session.events` stream (`crate::session::events_stream::open`), and
//! `EngineState::run_chat_turn` used to issue `task.run` BEFORE opening one.
//! Since #8184 the interactive default runs the solo agent, whose first
//! `write_file`/`bash` call is gated, so that ordering raced the stream open:
//! a call that lost the race was denied with no prompt the user could answer.
//! Every reconnect gap in `pump_session_events` reopened the same hole.
//! What: [`hold_prompter_claim`] opens one dedicated `session.events` stream
//! at `setup()` time, CONFIRMS the claim exists before returning, and hands
//! the stream to a background task that drains and discards frames —
//! `pump_session_events` is what renders — reconnecting a bounded number of
//! times. The per-turn pump still claims its own prompter while it is up, so
//! a gap now needs BOTH streams down at once.
//! Test: `tests/tui_client_engine.rs::{setup_opens_a_prompter_claim_stream,
//! setup_fails_when_the_claim_stream_is_refused}` pin the ordering and the
//! confirmation deterministically;
//! `session::events_stream::events_stream_tests::session_events_stream_is_a_prompter`
//! and `task::executor::tests::solo_run_of_the_stock_pm_asks_before_write_file`
//! pin the daemon-side half this depends on — that an events stream, with no
//! `session.attach` anywhere, is the prompter a gated call suspends for. Both
//! run in-process because the `--stdio` e2e harness serves no stream requests
//! at all (`serve::transport`), so `session.events` is unreachable there.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::engine::{RECONNECT_BACKOFF, SESSION_STREAM_MAX_RECONNECTS};
use super::engine_state::EngineState;
use super::error::EngineError;

/// Open the claim stream and confirm it before returning.
///
/// Why the first frame is AWAITED and INSPECTED: `open_stream` returns once
/// the request is written, so a dial that succeeded proves nothing about the
/// daemon having run the handler that takes the claim. A refusal
/// (`session_not_found`) arrives as a terminal frame on the stream, not as a
/// dial error, so discarding the frame would let `setup` report success over a
/// session with no claim — the exact failure this module exists to prevent.
/// A received frame proves the daemon reached `events_stream::open`, which
/// claims the prompter before it sends anything.
///
/// # Errors
///
/// The dial failure; the daemon's own refusal, verbatim; or
/// [`EngineError::StreamClosed`] when the stream ends, or says nothing inside
/// [`DEFAULT_CALL_TIMEOUT`], before a first frame. `setup()` then reports a
/// daemon that answered `session.create` and could not be watched, rather than
/// starting a session whose every gated call would be denied unprompted.
/// Test: see the module docs.
pub(super) async fn hold_prompter_claim(
    state: Arc<EngineState>,
    session_id: String,
) -> Result<(), EngineError> {
    let mut stream = open(&state, &session_id).await?;
    // Bounded like every other `setup` step (`UdsRpcClient::call`), NOT by the
    // 15-minute `STREAM_FRAME_TIMEOUT` a long-lived tail is allowed: a freshly
    // created session always has ring events to replay, so this frame is due
    // immediately and a silence here means the daemon never served the stream.
    match tokio::time::timeout(super::uds_rpc::DEFAULT_CALL_TIMEOUT, stream.next_frame()).await {
        Ok(Some(Ok(_))) => {}
        Ok(Some(Err(source))) => {
            return Err(EngineError::Transport {
                socket: state.rpc.socket().to_path_buf(),
                source: Box::new(source),
            });
        }
        Ok(None) | Err(_) => {
            return Err(EngineError::StreamClosed {
                socket: state.rpc.socket().to_path_buf(),
            });
        }
    }
    tokio::spawn(async move {
        let mut attempts = 0u32;
        loop {
            while let Some(Ok(_)) = stream.next_frame().await {
                // Drained and dropped on purpose: this stream exists to hold
                // the claim, and `pump_session_events` owns rendering. A
                // delivered frame is genuine progress, so it clears the budget
                // — the same rule, and the same #3411 reasoning, as
                // `pump_session_events`' own `attempts` reset.
                attempts = 0;
            }
            if state.shutting_down.load(Ordering::SeqCst)
                || attempts >= SESSION_STREAM_MAX_RECONNECTS
            {
                // A daemon that is gone must not be re-dialled for the life of
                // the process — unlike `workstream_subscription`, which tails
                // a daemon-wide feed with nothing to give up on, this claim
                // belongs to ONE session that a dead daemon no longer has.
                return;
            }
            attempts += 1;
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

/// One `session.events` dial: with no `after_seq` the daemon replays the ring,
/// then tails (`crate::session::events_stream::open`).
async fn open(
    state: &Arc<EngineState>,
    session_id: &str,
) -> Result<trusty_common::uds::FramedStream<Value>, EngineError> {
    state
        .rpc
        .open_stream::<Value>("session.events", json!({"session_id": session_id}))
        .await
}
