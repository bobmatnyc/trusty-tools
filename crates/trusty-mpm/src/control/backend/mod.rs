//! Runtime-adapter seam: `SessionBackend` trait and supporting input types.
//!
//! Why: the `SessionActor` must remain backend-agnostic so that
//! `StreamJsonBackend` and `TmuxBackend` can be swapped without touching the
//! actor loop. This module defines the trait and its input type; concrete
//! implementations live in the sibling modules.
//! What: [`SessionBackend`] (§3.3 of SPEC-SESSCTL-01) with three async methods
//! (`send`, `recv`, `stop`) and the [`SessionInput`] input type. Concrete
//! backends: [`stream_json::StreamJsonBackend`] and [`tmux::TmuxBackend`].
//! Test: trait has no side-effect-free tests; concrete backends have their own
//! unit tests in `stream_json` and `tmux` sub-modules.

pub mod stream_json;
pub mod tmux;

use anyhow::Result;
use async_trait::async_trait;

use crate::control::event::SessionEvent;

/// Input sent to a session backend from the actor.
///
/// Why: the spec §3.3 shows `send(msg: SessionInput)`. In alpha-1 the only
/// meaningful input is a text line (for both stream-JSON stdin and tmux
/// send-keys). The newtype lets later phases add structured variants
/// (e.g. JSON-framed messages for stream-JSON) without changing the trait.
/// What: wraps a UTF-8 string representing the input to send.
/// Test: used by `TmuxBackend::send` and `StreamJsonBackend::send` unit tests.
#[derive(Debug, Clone)]
pub struct SessionInput {
    /// The text to send to the session (newline-terminated for stream-JSON,
    /// send-keys text for tmux).
    pub text: String,
}

impl SessionInput {
    /// Construct a `SessionInput` from any string-like value.
    ///
    /// Why: ergonomics — callers can pass `&str` without wrapping.
    /// What: converts to an owned `String` and stores.
    /// Test: used transitively in all backend send-path tests.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// The runtime-adapter seam separating `SessionActor` from execution backends.
///
/// Why: swapping between stream-JSON, tmux, and future backends (tcode) must
/// not require changes to the actor task loop (§3.3 of SPEC-SESSCTL-01). A
/// trait with three async methods is the minimum viable seam.
/// What: `send` delivers input to the backend; `recv` polls for the next batch
/// of events (returns `None` on clean exit); `stop` gracefully drains and
/// terminates. All methods are `async` so backends can await I/O without
/// blocking the actor's `select!` loop.
/// Test: `StreamJsonBackend` and `TmuxBackend` each carry their own unit tests.
#[async_trait]
pub trait SessionBackend: Send + 'static {
    /// Send a message or command to the session.
    ///
    /// Why: the actor forwards `ActorCommand::Send` events to the backend so
    /// the write-lock holder can inject input.
    /// What: delivers `msg.text` to the backend's input channel (stdin for
    /// stream-JSON, `tmux send-keys` for tmux). Returns `Err` if the backend
    /// is no longer accepting input.
    /// Test: per-backend unit tests cover the send path.
    async fn send(&mut self, msg: SessionInput) -> Result<()>;

    /// Receive the next batch of output events from the session.
    ///
    /// Why: the actor `select!` loop polls `recv()` to drain backend output
    /// and broadcast events to observers.
    /// What: returns `Some(Ok(events))` with one or more `SessionEvent`s on
    /// new output; `Some(Err(…))` on a backend error; `None` on clean exit
    /// (no more output will arrive — actor should transition to `Stopped`).
    /// Test: per-backend unit tests cover the recv path.
    async fn recv(&mut self) -> Option<Result<Vec<SessionEvent>>>;

    /// Gracefully stop the session, draining in-flight work.
    ///
    /// Why: `ActorCommand::Stop` triggers a drain-then-kill sequence; the
    /// backend knows the right procedure (SIGTERM+drain+SIGKILL for
    /// stream-JSON, `tmux kill-session` for tmux).
    /// What: sends a stop signal to the child process / tmux session; waits up
    /// to an internal grace period; then forces termination if needed.
    /// Returns `Ok(())` on clean stop, `Err` if the kill itself failed.
    /// Takes ownership (`self`) so callers with a sized generic `B: SessionBackend`
    /// can call `backend.stop().await` directly without wrapping in `Box`.
    /// Test: per-backend unit tests cover the stop path.
    async fn stop(self) -> Result<()>;
}
