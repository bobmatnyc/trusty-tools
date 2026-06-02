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

#[test]
fn verify_request_forces_schema() {
    let req = build_verify_request(
        "us.anthropic.claude-haiku-4-5",
        "diff",
        &sample_finding(),
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
    );
    assert_eq!(
        req.model, "us.anthropic.claude-haiku-4-5",
        "routing prefix must be stripped before reaching the provider"
    );
}

#[test]
fn verify_request_uses_role_overrides() {
    let req = build_verify_request("m", "diff", &sample_finding(), Some(0.2), Some(128));
    assert!((req.temperature - 0.2).abs() < f32::EPSILON);
    assert_eq!(req.max_tokens, 128);
}

#[test]
fn verify_request_defaults_when_no_overrides() {
    let req = build_verify_request("m", "diff", &sample_finding(), None, None);
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
    let req = build_verify_request("m", "diff", &f, None, None);
    assert!(
        req.messages[0].content.contains("(unspecified)"),
        "missing line must render a stable placeholder"
    );
}
