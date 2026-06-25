//! Unit tests for `pipeline::verify_prompt`.
//!
//! Why: split from `verify_prompt.rs` to keep that file under the 500-line cap
//! while fully covering the prompt assembly and forced-output schema.
//! What: asserts the verifier request carries the finding, forces the schema,
//! strips routing prefixes, and that the schema enumerates both judgments.
//! Test: this is the test module.

use super::*;
use crate::models::{Effort, Finding};

fn sample_finding() -> Finding {
    let mut f = Finding::new(
        "src/auth.rs",
        "logic-error",
        "off-by-one in loop bound",
        "use <= instead of <",
        0.8,
        Effort::Medium,
    );
    f.line = Some(42);
    f
}

#[test]
fn verify_request_contains_finding() {
    let diff = "+fn authenticate() {}\n";
    let req = build_verify_request(
        "us.anthropic.claude-haiku-4-5",
        diff,
        &sample_finding(),
        None,
        None,
        None,
    );
    let user = &req.messages[0].content;
    assert!(
        user.contains("src/auth.rs"),
        "must mention the finding file"
    );
    assert!(user.contains("42"), "must mention the finding line");
    assert!(
        user.contains("off-by-one in loop bound"),
        "must mention the finding description"
    );
    assert!(user.contains("```diff"), "must embed the diff block");
}

/// The verifier user message includes the caller-supplied author rationale when
/// provided (#1618), so the verifier can refute a finding the author already
/// empirically addressed.
///
/// Why: the adversarial verifier must see the author's own explanation (e.g.
/// "checked the data source; no values exceed X") to judge whether the only
/// remaining doubt is unverifiable external-system speculation.
/// What: passes an author-rationale block, asserts it appears in the message
/// alongside the finding.
#[test]
fn verify_request_includes_author_rationale() {
    let rationale = "## PR Discussion / Author Rationale\n\n\
                     I checked the upstream emitter; no values exceed the cap.";
    let req = build_verify_request(
        "us.anthropic.claude-haiku-4-5",
        "+fn authenticate() {}\n",
        &sample_finding(),
        None,
        None,
        Some(rationale),
    );
    let user = &req.messages[0].content;
    assert!(
        user.contains("I checked the upstream emitter; no values exceed the cap."),
        "verifier message must include the author rationale when provided"
    );
    // The finding must still be present (rationale is additive, not a replacement).
    assert!(
        user.contains("off-by-one in loop bound"),
        "verifier message must still include the finding"
    );
}

/// Absent author rationale (#1618) leaves the verifier message unchanged.
///
/// Why: existing callers pass no rationale; the message must be byte-for-byte
/// equivalent to the pre-#1618 behaviour (no empty rationale block).
/// What: builds the request with `None` and with `Some("   ")` (blank) and
/// asserts neither introduces a rationale artefact; the two are identical.
#[test]
fn verify_request_omits_absent_author_rationale() {
    let none_req = build_verify_request("m", "+fn x() {}\n", &sample_finding(), None, None, None);
    let blank_req = build_verify_request(
        "m",
        "+fn x() {}\n",
        &sample_finding(),
        None,
        None,
        Some("   \n  "),
    );
    // A blank rationale must produce the SAME message as no rationale at all.
    assert_eq!(
        none_req.messages[0].content, blank_req.messages[0].content,
        "blank author rationale must not alter the verifier message"
    );
    // And it must go straight from the diff block to the finding (no stray heading).
    assert!(
        !none_req.messages[0].content.contains("Author Rationale"),
        "absent rationale must not render a rationale heading"
    );
}

/// The verifier system prompt carries the generic author-rationale guidance
/// (#1618) so the model knows to weigh an author's empirical external-system
/// verification.
///
/// Why: the REFUTED-on-unverifiable-speculation rule must be in the prompt for
/// the behaviour to exist; this pins the wording's presence (provider-agnostic).
#[test]
fn verify_system_prompt_mentions_author_rationale_rule() {
    let p = verifier_system_prompt();
    assert!(
        p.contains("Author rationale"),
        "system prompt must document the author-rationale rule"
    );
    assert!(
        p.contains("external") && p.contains("REFUTED"),
        "system prompt must instruct REFUTED on unverifiable external-system speculation"
    );
}

#[test]
fn verify_request_forces_schema() {
    let req = build_verify_request(
        "us.anthropic.claude-haiku-4-5",
        "diff",
        &sample_finding(),
        None,
        None,
        None,
    );
    let schema = req
        .response_schema
        .as_ref()
        .expect("verifier request must force structured output");
    assert_eq!(schema.name, VERIFY_SCHEMA_NAME);
}

#[test]
fn verify_request_strips_provider_prefix() {
    let req = build_verify_request(
        "bedrock/us.anthropic.claude-haiku-4-5",
        "diff",
        &sample_finding(),
        None,
        None,
        None,
    );
    assert_eq!(
        req.model, "us.anthropic.claude-haiku-4-5",
        "routing prefix must be stripped before reaching the provider"
    );
}

#[test]
fn verify_request_uses_role_overrides() {
    let req = build_verify_request("m", "diff", &sample_finding(), Some(0.2), Some(128), None);
    assert!((req.temperature - 0.2).abs() < f32::EPSILON);
    assert_eq!(req.max_tokens, 128);
}

#[test]
fn verify_request_defaults_when_no_overrides() {
    let req = build_verify_request("m", "diff", &sample_finding(), None, None, None);
    // Role defaults: temperature 1.0, tight token cap.
    assert!((req.temperature - 1.0).abs() < f32::EPSILON);
    assert!(req.max_tokens <= 128, "verifier output cap must stay tight");
}

#[test]
fn verify_schema_enumerates_judgments() {
    let schema = verify_response_schema();
    let enum_vals = &schema.schema["properties"]["judgment"]["enum"];
    assert_eq!(enum_vals[0], "CONFIRMED");
    assert_eq!(enum_vals[1], "REFUTED");
}

/// The verify response schema must be OpenAI strict-mode compliant.
///
/// Why: the verify pass also runs through OpenRouter with `strict: true` for
/// `openai/*` models; a missing `additionalProperties:false` or an incomplete
/// `required` list would block every OpenAI verification call exactly as the
/// review schema bug blocked reviews.
/// What: asserts the top-level object sets `additionalProperties:false` and
/// requires BOTH `judgment` and `reason` (strict mode requires all properties).
/// Test: no network — pure schema inspection.
#[test]
fn verify_schema_is_openai_strict_compliant() {
    let schema = verify_response_schema();
    assert_eq!(
        schema.schema["additionalProperties"],
        serde_json::json!(false),
        "verify schema object must set additionalProperties:false"
    );
    let required: std::collections::BTreeSet<&str> = schema.schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert_eq!(
        required,
        ["judgment", "reason"].into_iter().collect(),
        "strict mode requires every property (judgment AND reason)"
    );
}

#[test]
fn verify_system_prompt_mentions_refuted_guard() {
    let sys = verifier_system_prompt();
    assert!(sys.contains("REFUTED"), "must define the REFUTED judgment");
    assert!(
        sys.contains("CONFIRMED"),
        "must define the CONFIRMED judgment"
    );
    assert!(
        sys.to_lowercase().contains("does not appear in the diff")
            || sys.to_lowercase().contains("not appear in the diff"),
        "must encode the truncation/hallucination guard"
    );
}

#[test]
fn verify_request_handles_missing_line() {
    let mut f = sample_finding();
    f.line = None;
    let req = build_verify_request("m", "diff", &f, None, None, None);
    assert!(
        req.messages[0].content.contains("(unspecified)"),
        "missing line must render a stable placeholder"
    );
}
