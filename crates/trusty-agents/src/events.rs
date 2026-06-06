//! Process-wide event bus for real-time UI streaming (#192 Phase B).
//!
//! Why: The 2-second stderr polling model from #149 is slow, lossy, and only
//! handles workflow phase transitions. The UI needs immediate feedback on
//! every PM thinking turn, every sub-agent message, every tool call — across
//! both the in-process Axum server path AND the subprocess workflow path.
//! A `tokio::sync::broadcast` channel exposed via a process-global `OnceLock`
//! lets any code path emit events without threading the bus through dozens of
//! function signatures, while SSE subscribers fan out to all browsers.
//! What: Defines the `Event` enum (the wire format), a process-global
//! `EVENT_BUS` initialised lazily from any thread, helpers to publish and
//! subscribe, and a `__OMPM_EVENT__ <json>\n` stderr-relay protocol so events
//! emitted by `--workflow` subprocesses can be re-broadcast by the parent
//! API server.
//! Test: `events::publish` round-trips through `subscribe()`. The relay
//! prefix is stable so the parent stderr reader can detect and re-publish.

use std::sync::OnceLock;

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
}

/// Process-global event bus, initialised on first access.
///
/// Why: Threading a `broadcast::Sender<Event>` through every code path that
/// might want to emit telemetry (workflow engine, agent runners, ctrl) would
/// touch hundreds of function signatures. A `OnceLock` mirrors how `tracing`
/// solves the same problem — emit-from-anywhere with zero overhead when no
/// one is listening.
/// What: `Sender::send` is non-blocking; when there are no subscribers it
/// returns an error which we silently drop (events without listeners are
/// the expected baseline).
static EVENT_BUS: OnceLock<broadcast::Sender<Event>> = OnceLock::new();

/// Get (or initialise) the process-global event bus sender.
///
/// Why: Called from any code path that wants to emit. First call wins; all
/// subsequent calls receive the same `Sender`. Cheap (single atomic load on
/// the hot path).
/// What: Returns a clone of the global sender. The receiver half is created
/// per subscriber via `subscribe()`.
/// Test: `bus_is_singleton` confirms repeated calls return the same channel.
pub fn bus() -> broadcast::Sender<Event> {
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
pub fn subscribe() -> broadcast::Receiver<Event> {
    bus().subscribe()
}

/// Publish one event to the bus (best-effort, never panics).
///
/// Why: Most callers don't care whether anyone is listening — events are
/// telemetry, not control flow. Failures (no subscribers) are normal.
/// What: Sends the event on the broadcast channel, ignoring `SendError`.
/// Test: `publish_round_trips_through_subscribe`.
pub fn publish(event: Event) {
    let _ = bus().send(event);
}

/// Publish an event AND emit it on stderr with the `__OMPM_EVENT__` prefix
/// so a parent process can re-broadcast on its own bus.
///
/// Why: Workflow runs spawn as `trusty-agents --workflow ...` subprocesses of the
/// API server (`api/server.rs::run_task`). Events emitted inside that child
/// reach the parent's bus only via the existing stderr stream. This helper
/// does both in one call so emit sites don't have to know whether they're
/// running as a child.
/// What: Calls `publish(event.clone())` for in-process subscribers, then
/// writes one NDJSON line to stderr prefixed with `__OMPM_EVENT__ `. The
/// parent's stderr reader (in `api::server::run_task`) detects the prefix
/// and re-publishes on its own bus.
/// Test: Indirect — exercised by the workflow integration path.
pub fn emit(event: Event) {
    publish(event.clone());
    if let Ok(line) = serde_json::to_string(&event) {
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
        let _ = a.send(Event::Ping);
        // Drain via try_recv to avoid an async runtime in this sync test.
        let got = rx.try_recv().expect("expected ping");
        assert!(matches!(got, Event::Ping));
    }

    #[tokio::test]
    async fn publish_round_trips_through_subscribe() {
        let mut rx = subscribe();
        publish(Event::SessionStarted {
            session_id: "t1".into(),
            project: "demo".into(),
        });
        let got = rx.recv().await.unwrap();
        match got {
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
