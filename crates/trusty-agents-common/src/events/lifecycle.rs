//! Lifecycle event taxonomy + harness source enum for the shared event bus.
//!
//! Why: Wave 3 unifies the three harnesses (trusty-agents, trusty-mpm,
//!      trusty-code) onto a single event envelope (see ADR-0005). The richest
//!      existing event taxonomy is `trusty-agents::events::Event` — the full PM
//!      lifecycle. To avoid a flag-day rewrite, Phase 0 *copies* that taxonomy
//!      here as `LifecycleEvent` (minus `Ping`, which becomes a transport-level
//!      `HarnessPayload::Ping` arm). The de-duplication / migration of the
//!      original `trusty-agents` enum happens in later phases (P1/P2); for now
//!      this is purely additive and wires up no consumers.
//! What: Defines `HarnessSource` (which harness produced an event) and
//!       `LifecycleEvent` (the per-domain lifecycle taxonomy), both
//!       serde-round-trippable with stable snake_case wire tags.
//! Test: `super::tests` round-trips representative variants and asserts the
//!       `{"type": ...}` tag shape; `session_id` is covered there too.

use serde::{Deserialize, Serialize};

/// Which harness emitted an event.
///
/// Why: Once all three harnesses share one bus, a subscriber (UI, relay,
///      aggregator) must be able to tell trusty-agents events from trusty-mpm
///      or trusty-code events without inspecting the payload. Encoding the
///      origin in the envelope keeps that routing decision O(1) and explicit.
/// What: A copy-able enum with three variants serialising as snake_case
///       (`"agents"`, `"mpm"`, `"code"`).
/// Test: `super::tests::harness_source_round_trips`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessSource {
    /// The `trusty-agents` orchestration harness.
    Agents,
    /// The `trusty-mpm` platform.
    Mpm,
    /// The `trusty-code` per-project harness.
    Code,
}

/// Real-time lifecycle events streamed to UI clients across all harnesses.
///
/// Why: A single tagged enum keeps wire-format evolution tractable —
///      `serde(tag = "type")` produces `{"type":"agent_message", ...}` so the
///      UI can pattern-match on type and the back-end can grow new variants
///      without breaking older clients (unknown variants degrade to "ignored
///      event"). This is a verbatim copy of `trusty-agents::events::Event`
///      *minus* the `Ping` keepalive, which is promoted to a transport-level
///      `HarnessPayload::Ping` arm (keepalives are not a lifecycle concern).
/// What: Variants cover the full PM lifecycle — session, PM activity, agent
///       activity, tool calls, AST analysis, workflow phases, persona
///       detection, LLM call lifecycle, and report/recap generation.
///       `session_id` correlates events to a specific task; the UI filters by
///       session when scoped to a task view.
/// Test: `super::tests::lifecycle_event_serializes_with_type_tag` asserts the
///       wire shape; `super::tests::lifecycle_session_id_*` cover the helper.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LifecycleEvent {
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
    /// project pre-indexing). `op` is a short label (`index`, `lookup`,
    /// `edit`, `insert`, `import`, `validate`, `patch`); `detail` is a
    /// human-readable substring.
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
    /// A phase was skipped because the active persona opts out of it. Carries
    /// the phase name and the persona that triggered the skip.
    PhaseSkipped {
        session_id: String,
        phase: String,
        persona: String,
    },

    // -- Persona (workflow) --
    /// Emitted once per workflow run when a persona is detected from the task
    /// text, so the UI can surface "running in [persona] mode" before any
    /// phases execute.
    PersonaDetected {
        session_id: String,
        persona: String,
    },

    // -- LLM call lifecycle --
    /// Emitted just before any LLM chat-completion HTTP call leaves the
    /// process. Pairs with `LlmResponded` so consumers can compute latency.
    /// `agent_name` may be empty for top-level PM calls; `model` is the
    /// resolved provider/model string sent on the wire.
    LlmRequested {
        session_id: String,
        agent_name: String,
        model: String,
        prompt_tokens: Option<u32>,
    },

    /// Emitted after every LLM chat-completion HTTP call returns successfully.
    /// Carries wall-clock latency so the UI can render "model X took Yms".
    LlmResponded {
        session_id: String,
        agent_name: String,
        model: String,
        completion_tokens: Option<u32>,
        latency_ms: u64,
    },

    /// Emitted when an agent's actual work loop begins — distinct from
    /// `AgentSpawned`. `runner_type` is one of "subprocess", "claude-code",
    /// "inline".
    AgentStarted {
        session_id: String,
        agent_name: String,
        runner_type: String,
    },

    /// Emitted when an agent produces its final result before returning to the
    /// PM. `word_count` is a cheap whitespace-split count; status mirrors the
    /// agent's IPC status field ("success" or "error").
    ReportGenerated {
        session_id: String,
        agent_name: String,
        word_count: usize,
        status: String,
    },

    /// Emitted when a session recap is generated after N completed tasks.
    /// Carries the session ID, a one-line prose summary, and structured rows.
    RecapGenerated {
        session_id: String,
        /// One-line prose summary, e.g. "Fixed a bug where X. Y is now deployed."
        summary: String,
        /// Structured rows for the recap table: [(step_label, result_text)]
        table_rows: Vec<(String, String)>,
    },
}

impl LifecycleEvent {
    /// Return the event's `session_id` if it has one.
    ///
    /// Why: Filtering the broadcast stream to a single task subscription needs
    ///      a uniform accessor across the ~21 variants without each call site
    ///      re-matching. Every current variant carries a session, but the
    ///      `Option` keeps the contract stable if a session-less lifecycle
    ///      variant is ever added.
    /// What: Returns `Some(&str)` for variants that carry a session.
    /// Test: `super::tests::lifecycle_session_id_returns_correct_field`.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            LifecycleEvent::SessionStarted { session_id, .. }
            | LifecycleEvent::SessionDone { session_id, .. }
            | LifecycleEvent::SessionCancelled { session_id }
            | LifecycleEvent::PmThinking { session_id, .. }
            | LifecycleEvent::PmDelegating { session_id, .. }
            | LifecycleEvent::AgentSpawned { session_id, .. }
            | LifecycleEvent::AgentMessage { session_id, .. }
            | LifecycleEvent::AgentDone { session_id, .. }
            | LifecycleEvent::AgentFailed { session_id, .. }
            | LifecycleEvent::ToolCalled { session_id, .. }
            | LifecycleEvent::ToolResult { session_id, .. }
            | LifecycleEvent::AstOperation { session_id, .. }
            | LifecycleEvent::PhaseStarted { session_id, .. }
            | LifecycleEvent::PhaseDone { session_id, .. }
            | LifecycleEvent::PhaseSkipped { session_id, .. }
            | LifecycleEvent::PersonaDetected { session_id, .. }
            | LifecycleEvent::LlmRequested { session_id, .. }
            | LifecycleEvent::LlmResponded { session_id, .. }
            | LifecycleEvent::AgentStarted { session_id, .. }
            | LifecycleEvent::ReportGenerated { session_id, .. }
            | LifecycleEvent::RecapGenerated { session_id, .. } => Some(session_id),
        }
    }
}
