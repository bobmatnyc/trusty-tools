//! Verification-round prompt construction (Phase 2, #583).
//!
//! Why: the per-finding verification pass needs a strict, single-purpose prompt
//! that asks the verifier LLM for a binary CONFIRMED / REFUTED judgment on one
//! finding at a time, mirroring the code-intelligence verifier protocol.  Keeping
//! the prompt text and its forced-output schema in their own module keeps
//! `verify.rs` focused on orchestration and keeps every file under the 500-line
//! cap.
//!
//! What: exposes `build_verify_request` (assembles the `LlmRequest` for one
//! finding, forcing structured `{judgment, reason}` output via the same
//! `response_schema` mechanism the reviewer pass uses) and `VERIFY_SCHEMA_NAME`.
//! The system prompt encodes the truncation/hallucination guard: if the finding
//! references a file or line absent from the diff/context, the verifier MUST
//! answer REFUTED.
//!
//! Test: `verify_request_contains_finding`, `verify_request_forces_schema`,
//! `verify_schema_enumerates_judgments` in `verify_prompt_tests.rs`.

use crate::{
    llm::{ChatMessage, LlmRequest, ResponseSchema, strip_provider_prefix},
    models::Finding,
};

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Temperature for the verifier role.
///
/// Why: the verifier must be decisive but not pinned to a single deterministic
/// token across paraphrases; the role default (spec REV-310) is 1.0.  The
/// caller passes the resolved role temperature, so this is only the fallback.
/// What: matches the verifier `RoleConfig` default temperature (1.0).
const VERIFY_TEMPERATURE: f32 = 1.0;

/// Maximum output tokens for a verification call.
///
/// Why: the response is a single judgment plus a short reason; a tight cap keeps
/// latency and cost low (verifier calls are high-volume).
/// What: 64 tokens is ample for `{"judgment":"REFUTED","reason":"..."}`.
const VERIFY_MAX_TOKENS: u32 = 64;

/// Name used for the verifier's forced-output tool / json_schema.
///
/// Why: the Bedrock tool-use and OpenRouter json_schema paths both key off this
/// identifier; it must be a valid identifier and stable across calls.
/// What: a fixed snake_case string distinct from the reviewer's `review_output`.
pub const VERIFY_SCHEMA_NAME: &str = "verification_judgment";

// ─── System prompt ──────────────────────────────────────────────────────────────

/// Return the verifier system prompt.
///
/// Why: the verifier is a *fact-checker*, not a re-reviewer — its sole job is to
/// confirm or refute the specific finding it is handed.  Encoding the
/// truncation/hallucination guard here (REFUTE anything that references a
/// file/line not present in the provided diff) is the key false-positive defence
/// borrowed from the code-intelligence verifier protocol.
/// What: returns a static string instructing the model to emit exactly one of
/// CONFIRMED / REFUTED with a one-line reason, and to default to REFUTED when the
/// finding cannot be grounded in the supplied diff.
/// Test: `verify_system_prompt_mentions_refuted_guard`.
pub fn verifier_system_prompt() -> &'static str {
    r#"You are a strict code-review fact-checker. You are given the unified diff of a
pull request and ONE finding another reviewer raised about that diff. Your only
job is to decide whether the finding is a real, defensible issue grounded in the
diff shown.

## Judgment (MANDATORY — pick exactly one)
- CONFIRMED — the finding is real: the cited problem is actually present in the
  diff, the failure path is plausible, and a reasonable engineer would agree it
  needs attention.
- REFUTED — the finding is NOT defensible: it is speculative, incorrect,
  contradicted by the diff, or cannot be located in the diff at all.

## Hard rule (truncation / hallucination guard)
If the finding references a file or line that does NOT appear in the diff shown
below, you MUST answer REFUTED. Do not assume context you cannot see. A finding
about code that is not in the diff is, by definition, not verifiable and must be
refuted. This rule is absolute — it overrides any plausibility you might infer.

## Burden of proof
The default answer is REFUTED. Answer CONFIRMED only when the diff clearly shows
the problem the finding describes. When in doubt, REFUTE.

Populate the structured response fields: `judgment` (CONFIRMED or REFUTED) and
`reason` (one short sentence)."#
}

// ─── Output schema ───────────────────────────────────────────────────────────────

/// Build the forced-output schema for a verification call.
///
/// Why: forcing structured output eliminates the "model answered in prose and we
/// guessed" failure mode — the provider guarantees `LlmResponse.text` is a clean
/// JSON object with a `judgment` enum, so a parse can never silently default.
/// What: returns a `ResponseSchema` whose object requires `judgment` ∈
/// {CONFIRMED, REFUTED} plus an optional one-line `reason`.
/// Test: `verify_schema_enumerates_judgments`.
pub fn verify_response_schema() -> ResponseSchema {
    ResponseSchema {
        name: VERIFY_SCHEMA_NAME.to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "judgment": {
                    "type": "string",
                    "enum": ["CONFIRMED", "REFUTED"],
                    "description": "Binary verification judgment for the finding"
                },
                "reason": {
                    "type": "string",
                    "description": "One short sentence justifying the judgment"
                }
            },
            "required": ["judgment"]
        }),
    }
}

// ─── Request builder ─────────────────────────────────────────────────────────────

/// Build the `LlmRequest` to verify a single finding against the diff.
///
/// Why: each finding is verified independently so a refutation of one cannot
/// taint another; the request bundles the diff and the one finding with the
/// forced-output schema so the verifier returns a clean judgment.
/// What: assembles a system + user message (diff block + the finding's
/// file/line/kind/description) and sets `response_schema` to force the binary
/// judgment.  `verifier_model` may carry a `bedrock/`/`openrouter/` routing
/// prefix; it is stripped before being set as the bare API model id.  The
/// resolved role `temperature` / `max_tokens` are passed through so config
/// overrides apply; `None` falls back to the role defaults.
/// Test: `verify_request_contains_finding`, `verify_request_forces_schema`.
pub fn build_verify_request(
    verifier_model: &str,
    diff: &str,
    finding: &Finding,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> LlmRequest {
    let line = finding
        .line
        .map(|l| l.to_string())
        .unwrap_or_else(|| "(unspecified)".to_string());

    let user_message = format!(
        "## Unified diff\n\n```diff\n{diff}\n```\n\n\
         ## Finding to verify\n\
         - file: `{file}`\n\
         - line: {line}\n\
         - kind: {kind}\n\
         - description: {description}\n\
         - proposed fix: {suggestion}\n\n\
         Decide CONFIRMED or REFUTED per the rules in the system prompt. \
         If `{file}` or line {line} does not appear in the diff above, answer REFUTED.",
        diff = diff,
        file = finding.file,
        line = line,
        kind = finding.kind,
        description = finding.description,
        suggestion = if finding.suggestion.is_empty() {
            "(none)"
        } else {
            &finding.suggestion
        },
    );

    LlmRequest {
        model: strip_provider_prefix(verifier_model).to_string(),
        system: verifier_system_prompt().to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: user_message,
        }],
        temperature: temperature.unwrap_or(VERIFY_TEMPERATURE),
        max_tokens: max_tokens.unwrap_or(VERIFY_MAX_TOKENS),
        response_schema: Some(verify_response_schema()),
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "verify_prompt_tests.rs"]
mod tests;
