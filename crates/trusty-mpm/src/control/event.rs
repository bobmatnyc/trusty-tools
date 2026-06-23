//! Session event model for the SESSCTL control plane.
//!
//! Why: every session in the registry fan-outs its lifecycle and output events
//! to all registered observers via a `broadcast` channel. Having a single,
//! well-typed event enum means observers get structured data at zero per-byte
//! allocation cost in the non-matching path, and the compiler ensures all
//! variants are handled.
//! What: defines [`SessionEvent`] (§7.3 of SPEC-SESSCTL-01) with all ten
//! variants, plus supporting types [`ActivityKind`], [`StreamJsonEvent`],
//! [`StopReason`], and [`BackendKind`].
//! Test: `session_event_serialize` (serde round-trip), `stop_reason_display`
//! in the inline test module. Observer-dispatch coverage in `registry` tests.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::id::ControlSessionId;

/// The kind of execution backend that produced the event.
///
/// Why: observers that render output differently for stream-JSON vs. tmux
/// need to know the source without inspecting `structured` field nullness.
/// What: two-variant enum serialized as lowercase string.
/// Test: `session_event_serialize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    /// The `claude -p --output-format stream-json` headless backend.
    StreamJson,
    /// The `tmux send-keys` / `capture-pane` interactive backend.
    Tmux,
}

/// Reason a session was stopped.
///
/// Why: consumers (TUI, SM agent, `tm list`) need to distinguish a requested
/// stop from an error or a natural completion so they can decide what to do.
/// What: four-variant enum covering every session-exit path.
/// Test: `stop_reason_display`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// Operator requested a graceful stop (`tm stop`).
    Requested,
    /// Operator requested a forced stop (`tm stop --force`).
    Forced,
    /// The child process exited with code 0 (stream-JSON: session complete).
    Completed,
    /// The child process exited unexpectedly, and max restarts were exceeded.
    MaxRestartsExceeded,
    /// Auth failure on stream-JSON backend forced an early exit.
    AuthFailed,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Requested => f.write_str("requested"),
            Self::Forced => f.write_str("forced"),
            Self::Completed => f.write_str("completed"),
            Self::MaxRestartsExceeded => f.write_str("max-restarts-exceeded"),
            Self::AuthFailed => f.write_str("auth-failed"),
        }
    }
}

/// A single parsed stream-JSON event from `claude -p --output-format stream-json`.
///
/// Why: the stream-JSON wire format is JSON-newline-delimited; parsing it into
/// a typed enum lets the actor emit structured `Output` events without per-byte
/// allocation on the unrecognized-event path.
/// What: wraps the raw JSON value and optionally carries the event type tag
/// extracted from the `type` field.
/// Test: `session_event_serialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamJsonEvent {
    /// The `type` field from the Claude stream-JSON envelope.
    pub event_type: String,
    /// The full raw JSON object for this event.
    pub payload: serde_json::Value,
}

/// Activity kinds produced by the §8.2 regex/NLP parse layer.
///
/// Why: observers (SM agent, TUI) need structured labels for activity, not raw
/// bytes, so they can trigger autonomy decisions or render helpful summaries.
/// What: each variant corresponds to a recognizable pattern in session output.
/// Test: regex-layer tests (Phase 3 / §8.2 scope); variants used here for type
/// completeness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityKind {
    /// Claude Code interactive prompt `> ` detected; session awaiting input.
    AwaitingInput,
    /// A tool-use block was detected (`Tool use:` / `Using tool:`).
    ToolUse { name: String },
    /// An error pattern was detected (`Error:` / ANSI error-colored output).
    Error { excerpt: String },
    /// Test result line detected (`Tests passed` / `cargo test` output).
    TestResult { passed: u32, failed: u32 },
    /// Git operation confirmation detected (`git commit` / `git push`).
    GitOp { verb: String },
    /// OAuth or Max login prompt detected.
    AuthPrompt,
    /// Session completed normally (`Session complete` or exit-code 0).
    SessionComplete,
    /// Rate-limit error detected in stderr (§9.2).
    RateLimited,
}

/// Broadcast event emitted by a `SessionActor` to all registered observers.
///
/// Why: all observer types — CLI `tm connect`, TUI display panes, TELUI, and
/// the SM agent — subscribe to the same `broadcast::Sender<SessionEvent>` on
/// `SessionActorHandle.event_tx`. A single well-typed enum keeps the fan-out
/// cheap and keeps every consumer strongly typed.
/// What: ten variants covering the complete session lifecycle per §7.3 of
/// SPEC-SESSCTL-01.
/// Test: `session_event_serialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionEvent {
    /// Session backend has started; the actor transitions to `Running`.
    Started {
        session_id: ControlSessionId,
        backend: BackendKind,
        ts: DateTime<Utc>,
    },

    /// Raw output (and optional structured parse) from the backend.
    ///
    /// `structured` is `Some(…)` for the stream-JSON backend and `None` for
    /// the tmux pane-capture path.
    Output {
        session_id: ControlSessionId,
        /// Raw bytes as UTF-8 (best-effort; invalid sequences replaced).
        raw: String,
        structured: Option<StreamJsonEvent>,
        ts: DateTime<Utc>,
    },

    /// Regex/NLP parse layer (§8.2) identified a recognizable activity.
    ActivityParsed {
        session_id: ControlSessionId,
        kind: ActivityKind,
        summary: String,
        ts: DateTime<Utc>,
    },

    /// The session is awaiting a decision from the operator or SM agent.
    PendingDecision {
        session_id: ControlSessionId,
        prompt: String,
        proposed_default: Option<String>,
        ts: DateTime<Utc>,
    },

    /// The backend restarted after a crash (§3.1).
    ///
    /// `context_lost` is always `true` in alpha-1 (no history replay; see §13.3).
    Restarted {
        session_id: ControlSessionId,
        attempt: u8,
        context_lost: bool,
        ts: DateTime<Utc>,
    },

    /// A best-effort observer fell behind the broadcast ring-buffer (§6.1).
    ///
    /// The observer MUST re-sync state from `GET /sessions/{id}` before
    /// resuming event-driven logic.
    ObserverLagged {
        session_id: ControlSessionId,
        dropped: u64,
    },

    /// A critical observer (e.g. SM agent) lagged; session → `AwaitingIntervention`.
    ///
    /// Unlike `ObserverLagged`, this is NOT silently resubscribed — recovery
    /// requires an explicit SM-agent re-attach or session re-launch.
    ObserverCriticalLag {
        session_id: ControlSessionId,
        dropped: u64,
    },

    /// Auth failed on the stream-JSON backend (terminal error; requires re-launch).
    AuthFailed {
        session_id: ControlSessionId,
        backend: BackendKind,
        reason: String,
        ts: DateTime<Utc>,
    },

    /// Session reached a terminal stopped state.
    Stopped {
        session_id: ControlSessionId,
        reason: StopReason,
        ts: DateTime<Utc>,
    },

    /// Session failed (max restarts exceeded or unrecoverable error).
    Failed {
        session_id: ControlSessionId,
        reason: String,
        ts: DateTime<Utc>,
    },
}

impl SessionEvent {
    /// Extract the session ID from any variant.
    ///
    /// Why: routing code that needs to demux events by session ID shouldn't
    /// have to match every variant.
    /// What: returns a reference to the `ControlSessionId` embedded in the
    /// variant's `session_id` field.
    /// Test: `session_event_session_id_accessor`.
    pub fn session_id(&self) -> &ControlSessionId {
        match self {
            Self::Started { session_id, .. }
            | Self::Output { session_id, .. }
            | Self::ActivityParsed { session_id, .. }
            | Self::PendingDecision { session_id, .. }
            | Self::Restarted { session_id, .. }
            | Self::ObserverLagged { session_id, .. }
            | Self::ObserverCriticalLag { session_id, .. }
            | Self::AuthFailed { session_id, .. }
            | Self::Stopped { session_id, .. }
            | Self::Failed { session_id, .. } => session_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_reason_display() {
        assert_eq!(StopReason::Requested.to_string(), "requested");
        assert_eq!(StopReason::Completed.to_string(), "completed");
        assert_eq!(StopReason::MaxRestartsExceeded.to_string(), "max-restarts-exceeded");
        assert_eq!(StopReason::AuthFailed.to_string(), "auth-failed");
    }

    #[test]
    fn session_event_serialize() {
        let id = ControlSessionId::new("my-proj", 0);
        let event = SessionEvent::Started {
            session_id: id.clone(),
            backend: BackendKind::StreamJson,
            ts: Utc::now(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let back: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.session_id(), &id);
    }

    #[test]
    fn session_event_session_id_accessor() {
        let id = ControlSessionId::new("p", 5);
        let event = SessionEvent::Failed {
            session_id: id.clone(),
            reason: "test".into(),
            ts: Utc::now(),
        };
        assert_eq!(event.session_id(), &id);
    }

    #[test]
    fn backend_kind_serializes_kebab_case() {
        let json = serde_json::to_string(&BackendKind::StreamJson).unwrap();
        assert_eq!(json, "\"stream-json\"");
        let json = serde_json::to_string(&BackendKind::Tmux).unwrap();
        assert_eq!(json, "\"tmux\"");
    }
}
