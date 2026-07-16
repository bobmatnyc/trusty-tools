//! Structured telemetry a tool reports ALONGSIDE its rendered text (UI Phase 1).
//!
//! Why: the retrieval tools already know things the UI needs and the event
//! stream throws away. `search_code` knows which trusty-search lane actually
//! served a query, how many hits came back, and what it cost;
//! `recall_session` knows every result's score and which ones its token
//! budget dropped. Today all of that is flattened into prose and then
//! TRUNCATED into an `args_preview`/`result_preview` string. This module is
//! the escape hatch: a tool returns structured data attached to its
//! [`crate::tools::ToolResult`], and the agent loop — the one layer that
//! knows WHICH agent is running and owns the `ToolEventSink` — forwards it.
//!
//! The alternative (handing every tool a sink plus its own agent name at
//! construction) was rejected: it would make each tool's attribution an
//! independent copy of the loop's, free to drift, and would force the
//! fail-open retrieval tools to grow a second reason to fail. Tools stay
//! pure functions of their arguments — unit-testable with no sink, no
//! session, and no bus — and attribution keeps ONE source of truth.
//! What: [`ToolTelemetry`] is the tool -> loop payload, one variant per
//! structured event kind. It is deliberately NOT the wire type: the wire
//! types live in `crate::events` and are built from this by the sink, which
//! is where `session_id` and `agent` get stamped on.
//! Test: `crate::agent_loop::tests::sink_events::sink_receives_tool_telemetry_with_agent`
//! (forwarding), plus each tool's own `telemetry_*` tests (production).

use crate::events::RecalledMemory;

/// Structured data a tool reports about what it actually did (UI Phase 1).
///
/// Why: see the module docs — this is the seam that lets a tool's internal
/// knowledge (real search lane, per-memory scores, what entered context)
/// reach the event stream as DATA instead of dying inside its rendered text.
/// What: one variant per structured event kind; `None` on a `ToolResult`
/// means "this tool has nothing structured to say", which is every tool but
/// the two retrieval tools.
/// Test: `tools::traits::tests::tool_result_carries_optional_telemetry`.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolTelemetry {
    /// A `search_code` call — becomes `Event::SearchPerformed`.
    Search(SearchTelemetry),
    /// A `recall_session` call — becomes `Event::MemoryRecalled`.
    Recall(RecallTelemetry),
}

/// What a `search_code` call actually did (UI Phase 1).
///
/// Why: the UI traces a change back to the searches that drove it, which
/// needs the REAL lane rather than the mode the model requested — those
/// differ whenever a not-yet-built index forces a lexical retry (#2783).
/// What: `lane` is the lane that served the results; `hit_count` is `None`
/// when the result shape could not be counted (never a misleading `0`);
/// `latency_ms` covers the whole call including any retry.
/// Test: `tools::trusty_search_tests::telemetry_reports_lexical_fallback_lane`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchTelemetry {
    /// The trusty-search lane that actually served this query.
    pub lane: String,
    /// The query as the model asked it.
    pub query: String,
    /// Hits returned, or `None` when uncountable.
    pub hit_count: Option<usize>,
    /// Wall-clock duration of the whole tool call.
    pub latency_ms: u64,
}

/// What a `recall_session` call recalled, and what reached the model
/// (UI Phase 1).
///
/// Why: `injected` is the differentiating signal — see
/// [`crate::events::Event::MemoryRecalled`].
/// What: `results` stays in the daemon's score-sorted (highest-first) order.
/// Test: `tools::recall_session::tests::telemetry_marks_budget_dropped_results_held_back`.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallTelemetry {
    /// The query as the model asked it.
    pub query: String,
    /// Every recalled result, score-sorted highest-first, each flagged with
    /// whether it entered context.
    pub results: Vec<RecalledMemory>,
}
