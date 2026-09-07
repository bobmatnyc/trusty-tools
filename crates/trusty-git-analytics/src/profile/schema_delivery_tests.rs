//! Tests for [`super::deliver_schema`] (#5588).
//!
//! Why: the fork this module owns decides whether the model is constrained by a
//! schema or merely asked to follow one, and both arms are reachable from the
//! command line (`--model openrouter/…` vs `--model bedrock/…`).
//! What: covers both arms and the once-only property that keeps the two from
//! overlapping.
//! Test: included as `#[cfg(test)] mod tests` from `schema_delivery.rs`.

use serde_json::json;

use super::deliver_schema;

fn schema() -> serde_json::Value {
    json!({"type": "object", "properties": {"findings": {"type": "array"}}})
}

/// Why: this is #5588's closure condition for tga — the schema travels in the
/// request field, not in the prompt, wherever the provider can enforce it.
/// What: asserts the directive carries the name and the schema verbatim, and
/// that the system turn is the base prompt untouched.
/// Test: this test itself.
#[test]
fn structured_provider_gets_the_field_and_a_clean_system_turn() {
    let (system, directive) = deliver_schema("BASE", "period_findings", schema(), true);

    let directive = directive.expect("a structured-output provider gets the real field");
    assert_eq!(directive.name, "period_findings");
    assert_eq!(directive.schema, schema());
    assert!(
        directive.strict,
        "the passes want the constraint, not best-effort JSON"
    );
    assert_eq!(
        system, "BASE",
        "the schema must not also be pasted into the prompt: {system}"
    );
}

/// Why: Bedrock's Converse API cannot honour `response_schema`, and sending it
/// there fails the request before any network call — every period of a
/// `--model bedrock/…` run would be skipped.
/// What: asserts the directive is absent and the schema is rendered into the
/// system turn instead.
/// Test: this test itself.
#[test]
fn unsupported_provider_gets_the_prose_fallback() {
    let (system, directive) = deliver_schema("BASE", "period_findings", schema(), false);

    assert!(
        directive.is_none(),
        "a provider without the capability must not be sent the field"
    );
    assert!(system.starts_with("BASE"), "{system}");
    assert!(
        system.contains("\"findings\""),
        "the schema must still reach the model: {system}"
    );
}

/// Why: delivering both would spend prompt budget restating a constraint the
/// provider already enforces, and would let the two copies drift.
/// What: asserts exactly one of the two carries the schema, in both arms.
/// Test: this test itself.
#[test]
fn the_schema_is_delivered_exactly_once() {
    for structured_output in [true, false] {
        let (system, directive) = deliver_schema("BASE", "n", schema(), structured_output);
        assert_eq!(
            system.contains("\"findings\""),
            directive.is_none(),
            "exactly one delivery for structured_output={structured_output}: {system}"
        );
    }
}
