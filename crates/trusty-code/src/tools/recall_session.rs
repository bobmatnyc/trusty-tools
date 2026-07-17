//! `recall_session` tool (#2348): session-scoped memory query (RAG over
//! session history).
//!
//! Why: epic #2343 (Infinite Sessions) requires all other session context
//! beyond the 5 pinned goal slots and the compressed working history to be
//! retrieved ON DEMAND by querying trusty-memory, rather than kept resident
//! in the prompt. #2345's turn recorder already dual-writes every turn into
//! trusty-memory (tagged `session:<id>`, `turn`) as the semantic recall
//! surface; this tool is the model-invoked query side of that surface — an
//! explicit tool call the model makes when it needs something from earlier
//! in THIS session that has scrolled out of its visible context, never an
//! automatic per-turn injection.
//! What: [`RecallSessionTool`] implements `ToolExecutor`. `execute` calls
//! trusty-memory's `memory_recall` via the `tools/call` MCP envelope
//! (spike-verified on issue #2348: the daemon wraps the tool result as a
//! STRINGIFIED JSON blob inside `result.content[0].text`, not a raw JSON
//! value — `crate::memory_envelope::parse_tools_call_envelope`, the shared
//! unwrap this module originally owned and #2424 promoted so the turn
//! recorder writes through the same shape, handles that), over-fetching
//! `top_k * `[`OVER_FETCH_FACTOR`] results because the daemon cannot
//! tag-filter server-side (filtering happens client-side, AFTER the
//! daemon's own truncation — see the spike comment on #2348). Results are
//! filtered to those tagged `session:<id>` (matching the exact tag
//! `memory_sink::write_turn` writes), capped at `top_k` (clamped to
//! [`MAX_TOP_K`]), and truncated to fit [`TOKEN_BUDGET`] (the epic's
//! `recall_injection_reserve`) by dropping WHOLE lowest-scored results
//! (never mid-text) — daemon results already arrive score-sorted
//! highest-first, so this is a simple prefix take. Unreachable daemon /
//! malformed response is fail-open: a successful (non-error) `ToolResult`
//! carrying an explicit "unavailable" message plus a `tracing::warn!`, so a
//! flaky trusty-memory never derails the agent loop. (#2857) Dropping
//! lowest-scored results for the token budget also logs, at `tracing::info!`
//! — a degradation the model silently acts on (see [`render_results`]).
//! Test: `recall_session::tests::*`.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::warn;
use trusty_common::mcp::memory_rpc::call_memory_tool_at;

use crate::agent_loop::estimate_tokens;
use crate::events::RecalledMemory;
use crate::memory_envelope::{parse_tools_call_envelope, tools_call_params};
use crate::tools::telemetry::{RecallTelemetry, ToolTelemetry};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// The tool's registered/advertised name.
pub const RECALL_SESSION_TOOL_NAME: &str = "recall_session";

/// Default `top_k` when the model omits it.
pub const DEFAULT_TOP_K: usize = 5;

/// Hard cap on `top_k` regardless of what the model requests — keeps a
/// single call bounded even if the model asks for an unreasonable count.
pub const MAX_TOP_K: usize = 10;

/// Daemon-side over-fetch multiplier (spike guidance on #2348: the daemon
/// cannot tag-filter server-side, so client-side filtering needs a wider
/// candidate pool than the final `top_k` to avoid starving the session-tagged
/// subset).
const OVER_FETCH_FACTOR: usize = 4;

/// Token budget for this tool's rendered result text — the epic #2343
/// `recall_injection_reserve` (~4K of the ~80K max_overhead_fraction budget
/// on a 200K window).
const TOKEN_BUDGET: usize = 4000;

/// Parsed `recall_session` tool-call arguments.
#[derive(Debug, Deserialize)]
struct RecallSessionArgs {
    query: String,
    #[serde(default)]
    top_k: Option<usize>,
}

/// `ToolExecutor` for the `recall_session` tool (#2348).
///
/// Why: gives the model an explicit way to query ITS OWN durable session
/// history rather than relying on whatever still fits in its visible
/// context window.
/// What: holds the (session-scoped) identifiers needed to issue a
/// `memory_recall` call against the right daemon/palace and filter to the
/// right session — all three are resolved once, at construction, by the
/// caller (`task::executor::run_and_record`, reusing
/// `session::TurnMemorySink::base_url`/`palace` rather than re-deriving
/// them).
/// Test: `tests::*`.
pub struct RecallSessionTool {
    session_id: String,
    base_url: String,
    palace: String,
}

impl RecallSessionTool {
    /// Construct a tool scoped to one session's daemon/palace/id.
    pub fn new(
        session_id: impl Into<String>,
        base_url: impl Into<String>,
        palace: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            base_url: base_url.into(),
            palace: palace.into(),
        }
    }

    /// The exact tag `memory_sink::write_turn` writes for this session —
    /// the client-side filter predicate.
    fn session_tag(&self) -> String {
        format!("session:{}", self.session_id)
    }
}

/// Whether a single `memory_recall` result entry carries `tag`.
fn result_has_tag(result: &Value, tag: &str) -> bool {
    result
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|tags| tags.iter().any(|t| t.as_str() == Some(tag)))
        .unwrap_or(false)
}

/// Filter `results` to those tagged `tag`, then cap at `top_k`.
///
/// Why: shared by `execute` and directly unit-testable without a mock
/// server. Results arrive score-sorted highest-first from the daemon, so a
/// prefix `truncate` after filtering preserves that ordering.
/// What: preserves input order; drops non-matching entries; returns at most
/// `top_k` entries.
/// Test: `tests::filter_and_cap_keeps_only_tagged_results_in_order`.
fn filter_and_cap(results: &[Value], tag: &str, top_k: usize) -> Vec<Value> {
    let mut filtered: Vec<Value> = results
        .iter()
        .filter(|r| result_has_tag(r, tag))
        .cloned()
        .collect();
    filtered.truncate(top_k);
    filtered
}

/// Render filtered results into the tool's final text, dropping WHOLE
/// lowest-scored entries (never mid-text) to fit [`TOKEN_BUDGET`].
///
/// Why: the epic's budget model caps this tool's contribution to the
/// prompt; truncating mid-result would produce a half-sentence a model
/// might misread as complete. Dropping whole entries from the tail keeps
/// every included result fully legible.
/// What: `results` must already be capped/ordered (highest-score first,
/// per [`filter_and_cap`]). The first result is always included even if it
/// alone exceeds the budget — an over-budget single result is still more
/// useful than an empty response. (#2857) When one or more lowest-scored
/// results are dropped for budget, emits `tracing::info!` with the dropped/
/// included counts — the model silently receives fewer memories than it
/// asked for, so an operator diagnosing "the model missed a fact it should
/// have recalled" needs this visible from stderr, not just inferable from
/// the rendered text length.
///
/// (UI Phase 1) Also returns HOW MANY leading results made it into that
/// text. Because the loop takes a strict prefix (it `break`s at the first
/// over-budget entry rather than skipping it and trying the next), the
/// included set is always `results[..injected_count]` — so one count fully
/// describes the injected/held-back split, and
/// [`recall_telemetry`] turns it into the per-result `injected` flags.
/// Test: `tests::render_drops_whole_lowest_scored_entries_over_budget`,
/// `tests::render_includes_all_entries_within_budget`.
fn render_results(query: &str, results: &[Value]) -> (String, usize) {
    let mut entries: Vec<String> = Vec::new();
    let mut budget_used = 0usize;
    for r in results {
        let content = r.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let entry_tokens = estimate_tokens(content);
        if !entries.is_empty() && budget_used + entry_tokens > TOKEN_BUDGET {
            break;
        }
        budget_used += entry_tokens;
        entries.push(content.to_string());
    }
    let dropped = results.len() - entries.len();
    if dropped > 0 {
        tracing::info!(
            dropped,
            included = entries.len(),
            budget_tokens = TOKEN_BUDGET,
            "recall_session: token budget exceeded — dropping lowest-scored result(s)"
        );
    }
    let injected_count = entries.len();
    (
        format!(
            "Session memory results for \"{query}\":\n\n{}",
            entries.join("\n\n---\n\n")
        ),
        injected_count,
    )
}

/// Build the structured account of what was recalled and what reached the
/// model (UI Phase 1; `text`/`run_id` added for DOC-39 Slice C).
///
/// Why: this is the whole point of `Event::MemoryRecalled` — the UI renders
/// held-back memories ("41% · held") beside injected ones, and only this
/// tool ever knows which was which. Slice C extends that to the memory-
/// provenance debugging surface ("what memory drove this / what was held
/// back"), which needs the actual recalled TEXT, not just a score — a held
/// -back result the UI can only count but never read is not debuggable.
/// What: `results` is the filtered/capped, score-sorted list; the first
/// `injected_count` of them entered context (see [`render_results`]) and the
/// rest were recalled but dropped whole by the token budget. A result whose
/// entry carries no numeric `score` reports `0.0` rather than being omitted
/// — the UI must still see that the memory was recalled and held back.
/// `text`/`run_id` are read from the SAME already-parsed `content` value
/// `render_results` reads from (no second parse): `text` falls back to an
/// empty string, `run_id` to `None`, when the entry carries no such field —
/// never a panic.
/// Test: `tests::telemetry_marks_budget_dropped_results_held_back`,
/// `tests::telemetry_marks_all_injected_when_within_budget`,
/// `tests::telemetry_carries_recalled_text_and_run_id`.
fn recall_telemetry(query: &str, results: &[Value], injected_count: usize) -> RecallTelemetry {
    RecallTelemetry {
        query: query.to_string(),
        results: results
            .iter()
            .enumerate()
            .map(|(i, r)| RecalledMemory {
                score: r.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                injected: i < injected_count,
                text: r
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                run_id: r.get("run_id").and_then(Value::as_str).map(str::to_string),
            })
            .collect(),
    }
}

#[async_trait]
impl ToolExecutor for RecallSessionTool {
    fn name(&self) -> &str {
        RECALL_SESSION_TOOL_NAME
    }

    /// JSON schema for `recall_session`.
    ///
    /// Why: the description explicitly scopes the tool to THIS session's own
    /// history so the model does not mistake it for a general/cross-session
    /// or cross-project search.
    /// Test: `tests::schema_has_required_fields`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": RECALL_SESSION_TOOL_NAME,
                "description": "Search THIS session's own history stored in durable memory. Use when you need something from earlier in this session that is no longer visible in your current context (e.g. a decision, file path, or fact from many turns ago). Does not search other sessions or other projects.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "What to search for in this session's history."
                        },
                        "top_k": {
                            "type": "integer",
                            "description": "Maximum number of results to return (default 5, max 10)."
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute a session-scoped recall query, fail-open on any daemon or
    /// parsing failure.
    ///
    /// Why: an explicit model tool call must never derail the agent loop —
    /// a `ToolResult::err`/`fatal` here would surface as a tool error the
    /// model might retry-loop on; "session memory unavailable" as a
    /// SUCCESSFUL result lets the model simply proceed without it.
    /// Test: `tests::execute_returns_only_session_tagged_results`,
    /// `tests::execute_over_fetches_top_k_times_factor`,
    /// `tests::execute_is_fail_open_on_unreachable_daemon`,
    /// `tests::execute_rejects_malformed_args_recoverably`.
    async fn execute(&self, args: Value) -> ToolResult {
        let parsed: RecallSessionArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::err(format!(
                    "recall_session arguments did not match the expected shape ({e}). \
                     'query' (string) is required."
                ));
            }
        };
        let top_k = parsed.top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K);
        let fetch_k = top_k * OVER_FETCH_FACTOR;

        let rpc_params = tools_call_params(
            "memory_recall",
            json!({
                "palace": self.palace,
                "query": parsed.query,
                "top_k": fetch_k,
            }),
        );

        let envelope = match call_memory_tool_at(&self.base_url, "tools/call", rpc_params).await {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    session_id = %self.session_id,
                    error = %e,
                    "recall_session: memory_recall RPC failed (fail-open)"
                );
                return ToolResult::ok(
                    "session memory unavailable (trusty-memory unreachable)".to_string(),
                );
            }
        };

        let Some(body) = parse_tools_call_envelope(&envelope) else {
            warn!(
                session_id = %self.session_id,
                "recall_session: unexpected memory_recall response shape (fail-open)"
            );
            return ToolResult::ok(
                "session memory unavailable (unexpected response shape)".to_string(),
            );
        };

        let results = body
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let filtered = filter_and_cap(&results, &self.session_tag(), top_k);

        if filtered.is_empty() {
            return ToolResult::ok(format!(
                "No session memory found for query: {}",
                parsed.query
            ));
        }

        // (UI Phase 1) The render decides the injected/held-back split; the
        // telemetry reports it. Both derive from the SAME call so they can
        // never disagree about what the model actually saw.
        let (text, injected_count) = render_results(&parsed.query, &filtered);
        ToolResult::ok(text).with_telemetry(ToolTelemetry::Recall(recall_telemetry(
            &parsed.query,
            &filtered,
            injected_count,
        )))
    }
}

#[cfg(test)]
#[path = "recall_session_tests.rs"]
mod tests;
