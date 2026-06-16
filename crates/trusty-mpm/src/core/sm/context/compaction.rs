//! Compaction call + token estimation for the SM context engine (DOC-14 §7.3).
//!
//! Why: when the rolling window overflows (§7.2) the SM must *fold* the oldest
//! round(s) into the growing `compressed_context` via an INEXPENSIVE (Haiku-tier)
//! LLM call with a FIXED faithful-summary prompt that preserves goal ids, session
//! ids, decisions, and blockers (§7.3). That call is dependency-injected through
//! the SM-2 [`LlmProvider`] trait so production routes it through the resolved
//! Haiku provider while tests pass a mock — no real network, no real model. This
//! module owns the prompt text, the request rendering, the token-estimate
//! heuristic, and the actual provider invocation; the engine (`engine.rs`) only
//! decides WHEN to call it.
//! What: exposes [`estimate_tokens`] (the §7.2 chars/4 heuristic),
//! [`FAITHFUL_SUMMARY_SYSTEM_PROMPT`] / [`COMPRESS_SUMMARY_SYSTEM_PROMPT`] (the
//! two fixed §7.3/§7.5 instructions), the [`render_compaction_user_message`] /
//! [`render_resummarise_user_message`] renderers, and the async [`fold_rounds`] /
//! [`resummarise`] helpers that build the [`LlmRequest`] (temperature `0.0`) and
//! call the injected provider.
//! Test: `compaction_tests.rs` covers the heuristic, the prompt rendering, and a
//! mock-provider fold that asserts the request model/temperature and the returned
//! summary.

use crate::core::sm::providers::{ChatMessage, LlmProvider, LlmRequest, LlmResponse, SmLlmError};

use super::model::Round;

/// Heuristic tokens-per-character divisor for the §7.2 estimate.
///
/// Why: §7 specifies a simple running token estimate to drive the safety-valve
/// trigger; it need not match a real tokenizer, only be cheap and monotonic. The
/// well-known ≈4-characters-per-token rule is the documented heuristic.
/// What: the divisor applied to a character count to approximate token count.
/// Test: `estimate_tokens_uses_chars_over_four`.
pub const CHARS_PER_TOKEN: usize = 4;

/// Max tokens the compaction call may GENERATE for the new summary.
///
/// Why: the compaction response is a bounded prose summary, not an essay; capping
/// `max_tokens` keeps the call cheap and predictable. This is the generation cap
/// for the provider request; the *retained* summary size is governed separately
/// by `compressed_context_max_tokens` (§7.6), enforced by the engine.
/// What: a conservative ceiling passed as [`LlmRequest::max_tokens`]. 2048 tokens
/// comfortably holds a multi-round faithful summary while bounding cost.
/// Test: `fold_rounds_builds_haiku_request_at_temp_zero`.
pub const COMPACTION_MAX_TOKENS: u32 = 2048;

/// Compaction temperature — deterministic per §7.3.
///
/// Why: a faithful, reproducible summary must not vary run-to-run; §7.3 mandates
/// temperature `0.0` for the compaction call.
/// What: the `temperature` set on every compaction [`LlmRequest`].
/// Test: `fold_rounds_builds_haiku_request_at_temp_zero`.
pub const COMPACTION_TEMPERATURE: f32 = 0.0;

/// The FIXED faithful-summary system prompt for folding evicted rounds (§7.3).
///
/// Why: §7.3 mandates a fixed instruction that produces a *lossless-on-decisions*
/// merge — it must explicitly preserve goal ids, session ids, decisions, blockers,
/// and open questions while dropping chit-chat. Hard-coding it (rather than
/// templating) makes the behaviour auditable and deterministic.
/// What: a `const &str` system prompt instructing the model to merge the prior
/// summary and the evicted rounds into one updated, faithful summary.
/// Test: `faithful_prompt_mentions_required_anchors` asserts the required anchors
/// are named in the text.
pub const FAITHFUL_SUMMARY_SYSTEM_PROMPT: &str = "\
You are the Session Manager's context-compaction summariser. You merge a running \
conversation summary with the OLDEST conversation rounds that are being evicted \
from the verbatim window, producing a single updated summary that the Session \
Manager will rely on as its memory of everything older than the recent window.

Rules — follow ALL of them faithfully:
- Produce ONE updated summary that supersedes the prior summary. Do not append; \
integrate the evicted rounds into the prior summary as a coherent whole.
- Be lossless on decisions and identifiers. You MUST preserve, verbatim where \
they appear: goal ids (e.g. g-...), session ids (e.g. s-...), explicit decisions \
(\"chose X because Y\"), blockers, and open questions.
- Preserve tool/delegation outcomes (which sessions were spawned, verified, \
stopped) — these are facts, not chit-chat.
- Drop greetings, acknowledgements, and small talk that carry no decision or fact.
- Write dense, factual prose in the third person. No preamble, no meta-commentary, \
no markdown headings — just the updated summary text.";

/// The FIXED re-summarisation prompt for compacting an oversized summary (§7.6).
///
/// Why: §7.6 says the compressed block is itself re-compacted when it exceeds
/// `compressed_context_max_tokens`, via a "compact the summary" pass. That pass
/// has the same fidelity contract (preserve ids/decisions/blockers) but only one
/// input (the summary itself), so it needs its own fixed instruction.
/// What: a `const &str` system prompt instructing the model to shorten the given
/// summary while preserving all goal ids, session ids, decisions, blockers, and
/// open questions.
/// Test: `resummarise_prompt_mentions_required_anchors`.
pub const COMPRESS_SUMMARY_SYSTEM_PROMPT: &str = "\
You are the Session Manager's context-compaction summariser. The running summary \
below has grown too large. Rewrite it more concisely WITHOUT losing any decision \
or identifier.

Rules — follow ALL of them faithfully:
- Preserve, verbatim where they appear: every goal id (g-...), session id (s-...), \
explicit decision, blocker, and open question.
- Remove redundancy and verbose phrasing; keep every distinct fact.
- Write dense, factual third-person prose. No preamble, no headings — just the \
rewritten summary.";

/// Estimate the token count of a character count via the §7.2 heuristic.
///
/// Why: the engine maintains a running `token_estimate` to fire the safety-valve
/// trigger (§7.2b) without invoking a real tokenizer on every round. The estimate
/// only needs to be cheap and roughly proportional.
/// What: integer-divides `chars` by [`CHARS_PER_TOKEN`].
/// Test: `estimate_tokens_uses_chars_over_four`.
pub fn estimate_tokens(chars: usize) -> usize {
    chars / CHARS_PER_TOKEN
}

/// Render the user message for a fold call: prior summary + the evicted rounds.
///
/// Why: the compaction call needs the prior `compressed_context` plus the
/// verbatim text (and tool traces) of the round(s) being evicted, laid out so the
/// model can integrate them. Rendering it here (not in the engine) keeps the
/// exact wire format in one place and unit-testable.
/// What: emits a labelled block — the prior summary (or an explicit "none yet"
/// marker) followed by each evicted round's user/assistant text and tool traces.
/// Test: `render_fold_message_includes_summary_and_rounds`.
pub fn render_compaction_user_message(prior_summary: &str, evicted: &[Round]) -> String {
    let mut out = String::new();
    out.push_str("PRIOR SUMMARY:\n");
    if prior_summary.trim().is_empty() {
        out.push_str("(none yet — this is the first compaction)\n");
    } else {
        out.push_str(prior_summary);
        out.push('\n');
    }
    out.push_str("\nEVICTED ROUNDS (oldest first), integrate these into the summary:\n");
    for (i, r) in evicted.iter().enumerate() {
        out.push_str(&format!("\n--- round {} ---\n", i + 1));
        out.push_str("operator: ");
        out.push_str(&r.user);
        out.push_str("\nsession-manager: ");
        out.push_str(&r.assistant);
        out.push('\n');
        for t in &r.tool_calls {
            out.push_str("tool[");
            out.push_str(&t.name);
            out.push_str("]: ");
            out.push_str(&t.summary);
            out.push('\n');
        }
    }
    out
}

/// Render the user message for a re-summarisation pass (§7.6).
///
/// Why: the "compact the summary" pass feeds only the oversized summary back to
/// the model; a tiny labelled wrapper keeps the format explicit and testable.
/// What: prefixes the summary with a `SUMMARY TO COMPACT:` label.
/// Test: `render_resummarise_message_wraps_summary`.
pub fn render_resummarise_user_message(summary: &str) -> String {
    format!("SUMMARY TO COMPACT:\n{summary}")
}

/// Fold evicted rounds into the prior summary via the injected provider (§7.3).
///
/// Why: this is the heart of §7.3 — a single, cost-bounded compaction call. It is
/// trait-driven (`provider: &dyn LlmProvider`) precisely so the engine can be
/// tested with a mock that returns a canned summary, and so production routes it
/// through the SM-2-resolved Haiku-tier provider without this code knowing which
/// concrete provider it is.
/// What: builds an [`LlmRequest`] with the fixed faithful-summary system prompt,
/// a single user message from [`render_compaction_user_message`], the resolved
/// `model` id, temperature [`COMPACTION_TEMPERATURE`] (`0.0`), and
/// [`COMPACTION_MAX_TOKENS`]; awaits `provider.complete`; returns the full
/// [`LlmResponse`] so the engine can both take the new summary text and log token
/// usage / cost (§7.6).
/// Test: `fold_rounds_builds_haiku_request_at_temp_zero`,
/// `fold_rounds_returns_mock_summary` in `compaction_tests.rs` (mock provider).
pub async fn fold_rounds(
    provider: &dyn LlmProvider,
    model: &str,
    prior_summary: &str,
    evicted: &[Round],
) -> Result<LlmResponse, SmLlmError> {
    let user = render_compaction_user_message(prior_summary, evicted);
    let req = LlmRequest {
        model: model.to_string(),
        system: FAITHFUL_SUMMARY_SYSTEM_PROMPT.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: user,
        }],
        temperature: COMPACTION_TEMPERATURE,
        max_tokens: COMPACTION_MAX_TOKENS,
    };
    provider.complete(req).await
}

/// Re-summarise an oversized summary via the injected provider (§7.6).
///
/// Why: when `compressed_context` grows past `compressed_context_max_tokens` the
/// engine runs a "compact the summary" pass to prevent unbounded growth. Same
/// trait-injection rationale as [`fold_rounds`].
/// What: builds an [`LlmRequest`] with [`COMPRESS_SUMMARY_SYSTEM_PROMPT`], the
/// summary wrapped by [`render_resummarise_user_message`], temperature `0.0`, and
/// [`COMPACTION_MAX_TOKENS`]; awaits `provider.complete`.
/// Test: `resummarise_returns_mock_summary` in `compaction_tests.rs`.
pub async fn resummarise(
    provider: &dyn LlmProvider,
    model: &str,
    summary: &str,
) -> Result<LlmResponse, SmLlmError> {
    let req = LlmRequest {
        model: model.to_string(),
        system: COMPRESS_SUMMARY_SYSTEM_PROMPT.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: render_resummarise_user_message(summary),
        }],
        temperature: COMPACTION_TEMPERATURE,
        max_tokens: COMPACTION_MAX_TOKENS,
    };
    provider.complete(req).await
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
