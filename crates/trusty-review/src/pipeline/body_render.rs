//! Render the reviewer's structured payload as prose before it becomes a
//! review body (#4999 Part A).
//!
//! Why: with forced structured output active — Bedrock tool-use or OpenRouter
//! `json_schema` — the reviewer's response text IS the JSON object, and
//! `ReviewResult::apply_llm_response` copies that text into `review_body`
//! verbatim. Consumers treat `review_body` as the human-readable review: the
//! GitHub comment builder splices it in as the prose summary, and a downstream
//! app that passes a non-empty daemon body through unchanged posts it as the
//! WHOLE comment. #4999's audit of 22 dry-run reviews found 19 of them posting
//! `{"findings":[],"grade":"A",…}` as the entire review — every review except
//! the map-reduce path, which builds its body from the synthesis summary
//! instead and was never affected. The failure is silent from the consumer's
//! side, because a non-empty body looks like a successful one.
//!
//! What: [`render_review_body`] converts a raw structured payload into the
//! narrative the model wrote — its `summary`, plus its `grade_justification`
//! when that adds something — and returns anything else untouched. It
//! deliberately renders NO verdict and NO grade: those are the envelope's, and
//! the pipeline settles them after this runs (severity floor, coverage floor,
//! verification round). A payload with no narrative at all renders a marker
//! rather than the JSON, so "the model wrote no prose" stays distinguishable
//! from "the renderer did not run".
//!
//! Test: `body_render_tests.rs`.

use serde_json::{Map, Value};

/// Stand-in body for a structured payload that carries no narrative at all.
///
/// Why: falling back to the raw JSON would reinstate the exact defect; falling
/// back to an empty string would read as "no review happened". A marker says
/// which of the two actually occurred, and the structured fields still carry
/// the verdict, grade, and findings.
const NO_NARRATIVE: &str =
    "_The reviewer returned a structured verdict with no narrative summary._";

/// Convert a raw structured review payload into markdown prose.
///
/// Why: see the module docs — `review_body` reaches humans, so it must never be
/// the wire payload.
/// What: returns `raw` unchanged unless it is a JSON object carrying a
/// `verdict` key (the reviewer payload's signature — a body that merely starts
/// with `{`, or a prose body ending in a fenced ```json block, is left alone).
/// For a payload, renders `summary`, then `grade_justification` under a label
/// when it is present and not already the summary; renders [`NO_NARRATIVE`]
/// when neither field carries text.
/// Test: `raw_structured_payload_renders_as_prose`,
/// `payload_without_narrative_signals_rather_than_dumping_json`,
/// `free_text_body_passes_through_unchanged`,
/// `fenced_json_block_body_is_untouched`,
/// `json_without_a_verdict_key_passes_through`,
/// `rendered_body_never_carries_a_grade_or_verdict`.
pub(crate) fn render_review_body(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.starts_with('{') {
        return raw.to_string();
    }
    let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(trimmed) else {
        return raw.to_string();
    };
    if !payload.contains_key("verdict") {
        return raw.to_string();
    }

    let summary = text_field(&payload, "summary");
    let justification = text_field(&payload, "grade_justification");

    let mut out = String::with_capacity(summary.len() + justification.len() + NO_NARRATIVE.len());
    out.push_str(summary);
    if !justification.is_empty() && justification != summary {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("**Grade rationale:** ");
        out.push_str(justification);
    }
    if out.is_empty() {
        return NO_NARRATIVE.to_string();
    }
    out
}

/// Read a string field from the payload, trimmed; empty when absent or non-string.
fn text_field<'a>(payload: &'a Map<String, Value>, key: &str) -> &'a str {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "body_render_tests.rs"]
mod tests;
