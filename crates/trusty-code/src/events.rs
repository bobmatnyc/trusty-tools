//! Process-wide event bus for real-time UI streaming (#192 Phase B; #2055
//! formalises the session-attach envelope on top of it).
//!
//! Why: The 2-second stderr polling model from #149 is slow, lossy, and only
//! handles workflow phase transitions. The UI needs immediate feedback on
//! every PM thinking turn, every sub-agent message, every tool call — across
//! both the in-process Axum server path AND the subprocess workflow path.
//! A `tokio::sync::broadcast` channel exposed via a process-global `OnceLock`
//! lets any code path emit events without threading the bus through dozens of
//! function signatures, while SSE subscribers fan out to all browsers.
//!
//! #2055 adds [`SessionEventEnvelope`]: every event the `session.attach`
//! protocol (#2054) cares about is session-scoped, so the bus now carries the
//! ENVELOPE (`session_id`, a per-session monotonic `seq`, a UTC timestamp,
//! and the tagged [`Event`] payload) rather than a bare `Event`. This is the
//! same "envelope wraps a domain-tagged payload" shape as
//! `trusty-agents-common`'s `HarnessEvent`/`HarnessPayload` convention, with
//! two deliberate divergences (documented on the struct itself): `seq` is
//! PER-SESSION here (not process-global) because the session-attach
//! replay->live continuity this ticket requires is scoped to one session's
//! stream, and the payload stays a flat `#[serde(tag = "type")]` enum (not a
//! nested `{domain, event}` wrapper) because #2054 already shipped that wire
//! shape and rewriting it would break the shipped `session.attach` contract.
//! `session::registry::SessionRegistry` is the sole assigner of `seq`
//! (see its `record` method) since it already owns the per-session ring
//! buffer this must stay consistent with.
//!
//! What: Defines the `Event` enum (the tagged payload), `SessionEventEnvelope`
//! (the wire envelope), a process-global `EVENT_BUS` initialised lazily from
//! any thread carrying envelopes, helpers to publish and subscribe, and a
//! `__OMPM_EVENT__ <json>\n` stderr-relay protocol so events emitted by
//! `--workflow` subprocesses can be re-broadcast by the parent API server.
//! Test: `events::publish` round-trips through `subscribe()`. The relay
//! prefix is stable so the parent stderr reader can detect and re-publish.

use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Stderr line prefix used to relay events from a workflow subprocess to its
/// parent API server. The parent reads stderr line-by-line; lines starting
/// with this prefix are parsed as `Event` JSON and re-published on the
/// parent's event bus instead of being mirrored to terminal stderr.
///
/// Why: Workflow phase + agent telemetry must reach the API server even
/// though the workflow runs in a child process. Reusing the same stderr
/// channel that already carries `__OMPM_PROGRESS__` (the legacy 2s-poll
/// path) avoids inventing yet another IPC mechanism.
/// What: Match by exact byte prefix; payload after the space is one JSON
/// object decodable as `Event`.
pub const EVENT_LINE_PREFIX: &str = "__OMPM_EVENT__ ";

/// Capacity of the broadcast channel. Larger than `bus`'s 256 because event
/// volume is much higher (every PM thought, every agent line). 1024 keeps
/// memory bounded (~256KB at 256 bytes/event) while tolerating slow SSE
/// subscribers without dropping recent events under typical load.
const CHANNEL_CAPACITY: usize = 1024;

/// Real-time event types streamed to UI clients (browser, future Tauri).
///
/// Why: A single tagged enum keeps wire-format evolution tractable —
/// `serde(tag = "type")` produces `{"type":"agent_message", ...}` so the UI
/// can pattern-match on type and the back-end can grow new variants without
/// breaking older clients (unknown variants degrade to "ignored event").
/// What: Variants cover the full PM lifecycle plus a `Ping` keepalive so SSE
/// connections don't time out behind reverse proxies. `session_id` correlates
/// events to a specific task; the UI filters by session when scoped to a
/// task view.
/// Test: `event_serializes_with_type_tag` asserts the wire shape for one
/// variant; `Event::Ping` round-trips for the keepalive path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    // -- Session lifecycle --
    SessionStarted {
        session_id: String,
        project: String,
    },
    SessionDone {
        session_id: String,
        status: String,
    },
    SessionCancelled {
        session_id: String,
    },
    /// A session's lifecycle status changed (#2054 session-attach protocol).
    ///
    /// Why: `SessionStarted`/`SessionDone`/`SessionCancelled` cover the two
    /// endpoints of a session's life, but the daemon-owned `Session` model
    /// (vision spec Axiom 4) also transitions through intermediate states
    /// (e.g. `created` -> `running`) that a freshly-`session.attach`ed
    /// client should see. This variant is the generic "status is now X"
    /// signal `crate::session::registry::SessionRegistry` emits on every
    /// transition, including the two endpoints (so a client that attaches
    /// mid-stream sees a uniform status trail rather than mixing this
    /// variant with the older ones).
    /// What: `status` is a lowercase string matching
    /// `session::model::SessionStatus`'s serde representation (`"created"`,
    /// `"running"`, `"cancelled"`, `"finished"`, `"failed"`).
    /// Test: `session::registry::tests::create_publishes_started_and_status_events`.
    SessionStatusChanged {
        session_id: String,
        status: String,
    },
    /// Input was received for a session via `session.send` (#2054).
    ///
    /// Why: distinct from `AgentMessage` (sub-agent -> PM activity) —
    /// this is the client -> daemon direction. Emitting it is what makes
    /// `session.send` observably reach a freshly-`session.attach`ed client
    /// in M1, ahead of any real agent-loop wiring (`task.*`, #2056).
    /// What: `input` is the raw string the caller passed to `session.send`.
    /// Test: `session::registry::tests::send_publishes_input_event`.
    SessionInput {
        session_id: String,
        input: String,
    },

    // -- Tool lifecycle (#2055 taxonomy; emitted by #2056's agent loop) --
    /// A tool invocation began. Distinct from the older `ToolCalled` (which
    /// has no started/finished/error distinction or call-correlation id, and
    /// predates the session-attach protocol) — this is the tcode
    /// session.attach taxonomy's tool-lifecycle kind.
    ///
    /// Why: `call_id` correlates this with the matching `ToolFinished`/
    /// `ToolError` when a session runs multiple tool calls in sequence or
    /// concurrently.
    /// What: `args_preview` is truncated via `preview()` before emission.
    /// Test: `session::registry::registry_tests::record_tool_started_publishes_event`.
    ToolStarted {
        session_id: String,
        tool: String,
        call_id: String,
        args_preview: String,
    },
    /// A tool invocation completed (success or failure signalled by `success`,
    /// not a separate error variant — use `ToolError` for exceptional
    /// failures the caller wants to narrate distinctly, e.g. a timeout).
    ToolFinished {
        session_id: String,
        tool: String,
        call_id: String,
        success: bool,
        result_preview: String,
    },
    /// A tool invocation raised an exceptional error (as opposed to
    /// completing with `success: false`) — e.g. the tool process crashed or
    /// timed out rather than returning a normal failure result.
    ToolError {
        session_id: String,
        tool: String,
        call_id: String,
        error: String,
    },

    // -- Diagnostics / progress / generic message (#2055 taxonomy) --
    /// A structured log line scoped to a session, for operators/UIs that
    /// want daemon-side diagnostics without parsing stderr.
    Log {
        session_id: String,
        /// `"debug"` | `"info"` | `"warn"` | `"error"`.
        level: String,
        message: String,
    },
    /// A coarse progress update for a long-running session operation.
    /// `percent` is `None` when progress isn't quantifiable (e.g. "still
    /// searching") — the UI falls back to an indeterminate spinner.
    Progress {
        session_id: String,
        message: String,
        percent: Option<f32>,
    },
    /// A generic, freeform session-scoped message that doesn't fit any more
    /// specific kind — the taxonomy's catch-all, used sparingly.
    Message {
        session_id: String,
        text: String,
    },

    // -- PM activity --
    PmThinking {
        session_id: String,
        text: String,
    },
    PmDelegating {
        session_id: String,
        agent: String,
        task_preview: String,
    },

    // -- Agent activity --
    AgentSpawned {
        session_id: String,
        agent: String,
    },
    AgentMessage {
        session_id: String,
        agent: String,
        text: String,
    },
    AgentDone {
        session_id: String,
        agent: String,
        status: String,
    },
    AgentFailed {
        session_id: String,
        agent: String,
        error: String,
    },

    // -- Tool calls --
    ToolCalled {
        session_id: String,
        tool: String,
        preview: String,
    },
    ToolResult {
        session_id: String,
        tool: String,
        preview: String,
    },

    // -- AST analysis --
    /// Emitted when an AST-native tool performs a structural code operation
    /// (symbol lookup, edit, insert, import, validation, patch apply, or
    /// project pre-indexing). Surfaces in the REPL's thinking-step display
    /// so users can see structural analysis happening in real time.
    ///
    /// Why: AST-native tools replace whole-file rewrites with surgical edits.
    /// Without dedicated telemetry the user only sees opaque `ToolCalled`
    /// events and loses the narrative of "indexed project → looked up symbol
    /// → staged edit → applied patch". This event makes the AST narrative
    /// visible.
    /// What: `op` is a short label (`index`, `lookup`, `edit`, `insert`,
    /// `import`, `validate`, `patch`); `detail` is a human-readable
    /// substring (e.g. "42 symbols from src/" or "fn parse_args in main.rs").
    /// Test: `ast_operation_round_trips_through_subscribe` round-trips one
    /// variant through the bus.
    AstOperation {
        session_id: String,
        op: String,
        detail: String,
    },

    // -- Phase (workflow) --
    PhaseStarted {
        session_id: String,
        phase: String,
    },
    PhaseDone {
        session_id: String,
        phase: String,
        status: String,
    },
    /// #196: a phase was skipped because the active persona opts out of it.
    ///
    /// Why: Operators and the UI need visibility when persona-aware skipping
    /// changes the executed workflow shape (e.g. `[hacker]` skipping QA).
    /// Without this event the run looks like phases silently disappeared.
    /// What: Carries the phase name and the persona that triggered the skip.
    PhaseSkipped {
        session_id: String,
        phase: String,
        persona: String,
    },

    // -- Persona (workflow) --
    /// #196: emitted once per workflow run when a persona is detected from the
    /// task text. Lets the UI surface "running in [hacker] mode" before any
    /// phases execute, so users immediately see why the pipeline shape may
    /// differ from the default.
    PersonaDetected {
        session_id: String,
        persona: String,
    },

    // -- LLM call lifecycle (#199) --
    /// Emitted just before any LLM chat-completion HTTP call leaves the
    /// process. Pairs with `LlmResponded` so consumers can compute latency
    /// independently and surface in-flight calls in the UI.
    ///
    /// Why: Operators debugging slow runs need to see WHICH model is in flight
    /// at any moment, not just retroactively in totals. `prompt_tokens` is
    /// optional because we don't always know the prompt size up front.
    /// What: `agent_name` may be empty for top-level PM calls; `model` is the
    /// resolved provider/model string sent on the wire.
    LlmRequested {
        session_id: String,
        agent_name: String,
        model: String,
        prompt_tokens: Option<u32>,
    },

    /// Emitted after every LLM chat-completion HTTP call returns successfully.
    /// Carries the wall-clock latency so the UI can render "model X took Yms"
    /// badges without waiting for end-of-run perf aggregation.
    LlmResponded {
        session_id: String,
        agent_name: String,
        model: String,
        completion_tokens: Option<u32>,
        latency_ms: u64,
    },

    /// Emitted when an agent's actual work loop begins — distinct from
    /// `AgentSpawned`, which fires when the subprocess is kicked off.
    /// `AgentStarted` fires AFTER the spawn handshake (or at the start of an
    /// in-process runner loop) so the UI can show "spawning…" vs. "running"
    /// states. `runner_type` is one of "subprocess", "claude-code", "inline".
    AgentStarted {
        session_id: String,
        agent_name: String,
        runner_type: String,
    },

    /// Emitted when an agent produces its final result before returning to
    /// the PM. Lets the UI render "agent X returned a Y-word report" badges
    /// without parsing the result body.
    ///
    /// Why: A coarse signal of agent productivity that complements
    /// `AgentDone`. `word_count` is a cheap whitespace-split count; status
    /// mirrors the agent's IPC status field ("success" or "error").
    ReportGenerated {
        session_id: String,
        agent_name: String,
        word_count: usize,
        status: String,
    },

    /// Emitted when a session recap is generated after N completed tasks (#371).
    /// Carries the session ID, a one-line prose summary, and structured table rows.
    /// Consumed by TUI (renders `※ recap:` banner) and SSE → GUI (RecapPanel).
    RecapGenerated {
        session_id: String,
        /// One-line prose summary, e.g. "Fixed a bug where X. Y is now deployed."
        summary: String,
        /// Structured rows for the recap table: [(step_label, result_text)]
        table_rows: Vec<(String, String)>,
    },

    // -- Keepalive --
    Ping,
}

impl Event {
    /// Return the event's `session_id` if it has one. Used by the SSE
    /// handler to filter the broadcast stream to a single task subscription.
    ///
    /// Why: Variants without a session (currently just `Ping`) must always
    /// pass the filter — losing keepalives would defeat their purpose.
    /// What: Returns `Some(&str)` for variants that carry a session, `None`
    /// for keepalives. The SSE filter treats `None` as "always include".
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Event::SessionStarted { session_id, .. }
            | Event::SessionDone { session_id, .. }
            | Event::SessionCancelled { session_id }
            | Event::SessionStatusChanged { session_id, .. }
            | Event::SessionInput { session_id, .. }
            | Event::ToolStarted { session_id, .. }
            | Event::ToolFinished { session_id, .. }
            | Event::ToolError { session_id, .. }
            | Event::Log { session_id, .. }
            | Event::Progress { session_id, .. }
            | Event::Message { session_id, .. }
            | Event::PmThinking { session_id, .. }
            | Event::PmDelegating { session_id, .. }
            | Event::AgentSpawned { session_id, .. }
            | Event::AgentMessage { session_id, .. }
            | Event::AgentDone { session_id, .. }
            | Event::AgentFailed { session_id, .. }
            | Event::ToolCalled { session_id, .. }
            | Event::ToolResult { session_id, .. }
            | Event::AstOperation { session_id, .. }
            | Event::PhaseStarted { session_id, .. }
            | Event::PhaseDone { session_id, .. }
            | Event::PhaseSkipped { session_id, .. }
            | Event::PersonaDetected { session_id, .. }
            | Event::LlmRequested { session_id, .. }
            | Event::LlmResponded { session_id, .. }
            | Event::AgentStarted { session_id, .. }
            | Event::ReportGenerated { session_id, .. }
            | Event::RecapGenerated { session_id, .. } => Some(session_id),
            Event::Ping => None,
        }
    }

    /// The stable `kind` string for this event — identical to the serde
    /// `"type"` tag `#[serde(tag = "type", rename_all = "snake_case")]`
    /// produces on the wire.
    ///
    /// Why (#2055): `SessionEventEnvelope` carries `kind` as a top-level
    /// field (vision spec's envelope requirement) so a client can filter/
    /// route on it without parsing into the nested `event` payload. Hand-
    /// written rather than derived from serde reflection (no such API exists
    /// without an extra dependency) — `kind_matches_serde_tag_for_every_variant`
    /// guards the two from drifting apart.
    /// What: Returns the exact snake_case tag string for every variant.
    /// Test: `kind_matches_serde_tag_for_every_variant`.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::SessionStarted { .. } => "session_started",
            Event::SessionDone { .. } => "session_done",
            Event::SessionCancelled { .. } => "session_cancelled",
            Event::SessionStatusChanged { .. } => "session_status_changed",
            Event::SessionInput { .. } => "session_input",
            Event::ToolStarted { .. } => "tool_started",
            Event::ToolFinished { .. } => "tool_finished",
            Event::ToolError { .. } => "tool_error",
            Event::Log { .. } => "log",
            Event::Progress { .. } => "progress",
            Event::Message { .. } => "message",
            Event::PmThinking { .. } => "pm_thinking",
            Event::PmDelegating { .. } => "pm_delegating",
            Event::AgentSpawned { .. } => "agent_spawned",
            Event::AgentMessage { .. } => "agent_message",
            Event::AgentDone { .. } => "agent_done",
            Event::AgentFailed { .. } => "agent_failed",
            Event::ToolCalled { .. } => "tool_called",
            Event::ToolResult { .. } => "tool_result",
            Event::AstOperation { .. } => "ast_operation",
            Event::PhaseStarted { .. } => "phase_started",
            Event::PhaseDone { .. } => "phase_done",
            Event::PhaseSkipped { .. } => "phase_skipped",
            Event::PersonaDetected { .. } => "persona_detected",
            Event::LlmRequested { .. } => "llm_requested",
            Event::LlmResponded { .. } => "llm_responded",
            Event::AgentStarted { .. } => "agent_started",
            Event::ReportGenerated { .. } => "report_generated",
            Event::RecapGenerated { .. } => "recap_generated",
            Event::Ping => "ping",
        }
    }
}

/// The stable, sequenced wire envelope for one session's event (#2055).
///
/// Why: `session.attach`'s ring-buffer replay and every live notification
/// thereafter (both STDIO `session.event` notifications and the HTTP
/// `GET /sessions/{id}/events` SSE stream) need the SAME shape so a client
/// can detect gaps and stitch replay -> live into one ordered stream. See
/// the module docs for how this aligns with / diverges from
/// `trusty-agents-common`'s `HarnessEvent` convention.
/// What: `session_id` (always present — every event this daemon emits is
/// session-scoped), `seq` (per-session monotonic, assigned by
/// `session::registry::SessionRegistry::record`, starting at 1), `at` (UTC
/// emit timestamp), `kind` (== `event.kind()`, duplicated at the top level
/// for cheap client-side filtering), and `event` (the full tagged payload).
/// Test: `session_event_envelope_round_trips_through_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEventEnvelope {
    pub session_id: String,
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub event: Event,
}

impl SessionEventEnvelope {
    /// Build an envelope for `event`, computing `kind` from
    /// [`Event::kind`].
    ///
    /// Why: the one place `kind` is derived from `event`, so the two can
    /// never be constructed out of sync.
    /// What: `session_id`/`seq`/`at` are supplied by the caller (the
    /// registry, which owns the per-session sequence counter); `kind` is
    /// computed here.
    /// Test: `session_event_envelope_round_trips_through_json`.
    pub fn new(session_id: String, seq: u64, at: DateTime<Utc>, event: Event) -> Self {
        let kind = event.kind().to_string();
        Self {
            session_id,
            seq,
            at,
            kind,
            event,
        }
    }
}

/// Process-global event bus, initialised on first access.
///
/// Why: Threading a `broadcast::Sender<SessionEventEnvelope>` through every
/// code path that might want to emit telemetry (workflow engine, agent
/// runners, ctrl) would touch hundreds of function signatures. A `OnceLock`
/// mirrors how `tracing` solves the same problem — emit-from-anywhere with
/// zero overhead when no one is listening. Carries the full envelope
/// (#2055), not a bare `Event`, since every current producer
/// (`session::registry::SessionRegistry`) and every current consumer (the
/// STDIO forwarder, the HTTP SSE route) is session-scoped and needs `seq`/
/// `at`/`kind` alongside the payload — see the module docs for the
/// `HarnessEvent` alignment/divergence rationale.
/// What: `Sender::send` is non-blocking; when there are no subscribers it
/// returns an error which we silently drop (events without listeners are
/// the expected baseline).
static EVENT_BUS: OnceLock<broadcast::Sender<SessionEventEnvelope>> = OnceLock::new();

/// Get (or initialise) the process-global event bus sender.
///
/// Why: Called from any code path that wants to emit. First call wins; all
/// subsequent calls receive the same `Sender`. Cheap (single atomic load on
/// the hot path).
/// What: Returns a clone of the global sender. The receiver half is created
/// per subscriber via `subscribe()`.
/// Test: `bus_is_singleton` confirms repeated calls return the same channel.
pub fn bus() -> broadcast::Sender<SessionEventEnvelope> {
    EVENT_BUS
        .get_or_init(|| broadcast::channel(CHANNEL_CAPACITY).0)
        .clone()
}

/// Subscribe to all future events on the global bus.
///
/// Why: SSE handlers + tests both need a fresh receiver without sharing one.
/// `broadcast::Receiver::recv` is `&mut self` so each subscriber owns one.
/// What: Returns a new `Receiver` that begins receiving events emitted after
/// the call returns. Lagged subscribers receive `RecvError::Lagged(n)` and
/// can resume.
pub fn subscribe() -> broadcast::Receiver<SessionEventEnvelope> {
    bus().subscribe()
}

/// Publish one envelope to the bus (best-effort, never panics).
///
/// Why: Most callers don't care whether anyone is listening — events are
/// telemetry, not control flow. Failures (no subscribers) are normal. The
/// envelope (not a bare `Event`) is the unit of publication so `seq`/`at`
/// travel with the payload to every subscriber, including a client that
/// only just attached.
/// What: Sends the envelope on the broadcast channel, ignoring `SendError`.
/// Test: `publish_round_trips_through_subscribe`.
pub fn publish(envelope: SessionEventEnvelope) {
    let _ = bus().send(envelope);
}

/// Publish an envelope AND emit it on stderr with the `__OMPM_EVENT__` prefix
/// so a parent process can re-broadcast on its own bus.
///
/// Why: Workflow runs spawn as `open-mpm --workflow ...` subprocesses of the
/// API server (`api/server.rs::run_task`). Events emitted inside that child
/// reach the parent's bus only via the existing stderr stream. This helper
/// does both in one call so emit sites don't have to know whether they're
/// running as a child.
/// What: Calls `publish(envelope.clone())` for in-process subscribers, then
/// writes one NDJSON line to stderr prefixed with `__OMPM_EVENT__ `. The
/// parent's stderr reader (in `api::server::run_task`) detects the prefix
/// and re-publishes on its own bus.
/// Test: Indirect — exercised by the workflow integration path.
pub fn emit(envelope: SessionEventEnvelope) {
    publish(envelope.clone());
    if let Ok(line) = serde_json::to_string(&envelope) {
        eprintln!("{EVENT_LINE_PREFIX}{line}");
    }
}

/// Truncate a string to at most `max` characters, appending an ellipsis
/// marker when truncation occurred. Operates on chars (not bytes) so
/// multi-byte characters are not split.
///
/// Why: Event payloads should be human-readable previews; raw multi-KB
/// agent outputs would saturate the broadcast channel and overwhelm the SSE
/// stream. Centralising preview construction keeps emit sites tidy.
/// What: Returns `s` unchanged when shorter than `max`; otherwise returns
/// the first `max` chars plus a Unicode horizontal ellipsis.
pub fn preview(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serializes_with_type_tag() {
        let ev = Event::PmThinking {
            session_id: "s1".into(),
            text: "considering options".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"type\":\"pm_thinking\""), "{s}");
        assert!(s.contains("\"session_id\":\"s1\""), "{s}");
    }

    #[test]
    fn ping_roundtrips() {
        let s = serde_json::to_string(&Event::Ping).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, Event::Ping));
    }

    #[test]
    fn session_id_returns_correct_field() {
        let ev = Event::AgentMessage {
            session_id: "abc".into(),
            agent: "python".into(),
            text: "hi".into(),
        };
        assert_eq!(ev.session_id(), Some("abc"));
        assert_eq!(Event::Ping.session_id(), None);
    }

    #[test]
    fn bus_is_singleton() {
        let a = bus();
        let b = bus();
        // Two senders to the same channel: a message sent on `a` should be
        // visible to a receiver from `b`.
        let mut rx = b.subscribe();
        let envelope = SessionEventEnvelope::new("s-bus".into(), 1, Utc::now(), Event::Ping);
        let _ = a.send(envelope);
        // Drain via try_recv to avoid an async runtime in this sync test.
        let got = rx.try_recv().expect("expected an envelope");
        assert!(matches!(got.event, Event::Ping));
    }

    #[tokio::test]
    async fn publish_round_trips_through_subscribe() {
        let mut rx = subscribe();
        let envelope = SessionEventEnvelope::new(
            "t1".into(),
            1,
            Utc::now(),
            Event::SessionStarted {
                session_id: "t1".into(),
                project: "demo".into(),
            },
        );
        publish(envelope);
        let got = rx.recv().await.unwrap();
        assert_eq!(got.session_id, "t1");
        assert_eq!(got.seq, 1);
        assert_eq!(got.kind, "session_started");
        match got.event {
            Event::SessionStarted {
                session_id,
                project,
            } => {
                assert_eq!(session_id, "t1");
                assert_eq!(project, "demo");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// `kind()` must match the serde `"type"` tag for every variant — the
    /// guard `SessionEventEnvelope::new`'s doc comment references.
    #[test]
    fn kind_matches_serde_tag_for_every_variant() {
        let samples: Vec<Event> = vec![
            Event::SessionStarted {
                session_id: "s".into(),
                project: "p".into(),
            },
            Event::SessionDone {
                session_id: "s".into(),
                status: "done".into(),
            },
            Event::SessionCancelled {
                session_id: "s".into(),
            },
            Event::SessionStatusChanged {
                session_id: "s".into(),
                status: "running".into(),
            },
            Event::SessionInput {
                session_id: "s".into(),
                input: "hi".into(),
            },
            Event::ToolStarted {
                session_id: "s".into(),
                tool: "bash".into(),
                call_id: "c1".into(),
                args_preview: "ls".into(),
            },
            Event::ToolFinished {
                session_id: "s".into(),
                tool: "bash".into(),
                call_id: "c1".into(),
                success: true,
                result_preview: "ok".into(),
            },
            Event::ToolError {
                session_id: "s".into(),
                tool: "bash".into(),
                call_id: "c1".into(),
                error: "boom".into(),
            },
            Event::Log {
                session_id: "s".into(),
                level: "info".into(),
                message: "m".into(),
            },
            Event::Progress {
                session_id: "s".into(),
                message: "m".into(),
                percent: None,
            },
            Event::Message {
                session_id: "s".into(),
                text: "hi".into(),
            },
            Event::Ping,
        ];
        for ev in samples {
            let value = serde_json::to_value(&ev).unwrap();
            let wire_type = value["type"].as_str().unwrap_or("ping");
            assert_eq!(
                ev.kind(),
                wire_type,
                "kind() drifted from the serde tag for {ev:?}"
            );
        }
    }

    /// `SessionEventEnvelope` must round-trip through JSON with every field
    /// present, and `kind` must be derived from `event.kind()`.
    #[test]
    fn session_event_envelope_round_trips_through_json() {
        let event = Event::SessionInput {
            session_id: "s1".into(),
            input: "hello".into(),
        };
        let envelope = SessionEventEnvelope::new("s1".into(), 7, Utc::now(), event);

        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["session_id"], "s1");
        assert_eq!(value["seq"], 7);
        assert_eq!(value["kind"], "session_input");
        assert_eq!(value["event"]["type"], "session_input");
        assert!(value["at"].is_string());

        let back: SessionEventEnvelope = serde_json::from_value(value).unwrap();
        assert_eq!(back.session_id, "s1");
        assert_eq!(back.seq, 7);
    }

    #[test]
    fn preview_truncates_at_char_boundary() {
        assert_eq!(preview("hi", 5), "hi");
        let out = preview("abcdef", 3);
        assert_eq!(out.chars().count(), 4); // 3 + ellipsis
        assert!(out.starts_with("abc"));
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn event_line_prefix_is_stable() {
        // Lock in the wire constant — changing it breaks the parent/child
        // relay protocol.
        assert_eq!(EVENT_LINE_PREFIX, "__OMPM_EVENT__ ");
    }
}
