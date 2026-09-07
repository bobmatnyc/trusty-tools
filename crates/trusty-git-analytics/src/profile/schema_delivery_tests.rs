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

// ─── #7082: strict-mode compliance of the schemas this crate actually sends ───

/// Assert every object node in a JSON Schema satisfies OpenAI strict mode.
///
/// Why: the rule is recursive, and #7082 was the nested `findings.items` node —
/// a top-level-only check would have passed while every call still 400'd.
/// What: for a node carrying a `properties` map, asserts
/// `additionalProperties == false` and that `required` lists every property
/// key; recurses through `properties`, `items`, and an object-valued
/// `additionalProperties`. `path` names the failing node.
/// Test: used by `the_sent_schemas_are_openai_strict_compliant`.
fn assert_strict(node: &serde_json::Value, path: &str) {
    let Some(map) = node.as_object() else { return };

    if let Some(props) = map.get("properties").and_then(|p| p.as_object()) {
        assert_eq!(
            map.get("additionalProperties"),
            Some(&serde_json::Value::Bool(false)),
            "{path}: object node must set additionalProperties:false"
        );
        let required: Vec<&str> = map
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(
            required.len(),
            props.len(),
            "{path}: required must list every property, got {required:?}"
        );
        for key in props.keys() {
            assert!(
                required.contains(&key.as_str()),
                "{path}: property {key} missing from required"
            );
        }
        for (key, child) in props {
            assert_strict(child, &format!("{path}.{key}"));
        }
    }
    if let Some(items) = map.get("items") {
        assert_strict(items, &format!("{path}[]"));
    }
    match map.get("additionalProperties") {
        Some(additional) if additional.is_object() => {
            assert_strict(additional, &format!("{path}.*"));
        }
        _ => {}
    }
}

/// Why (#7082): on installed tga 8.0.0 every period and synthesis call to
/// `openai/gpt-4o-mini` via OpenRouter returned 400 — `Invalid schema for
/// response_format 'period_findings': ... 'additionalProperties' is required to
/// be supplied and to be false`. Both builders describe the shape they want and
/// leave strict-mode compliance to the wire renderer, so the property has to be
/// pinned on the value that actually leaves the process.
/// What: takes both real schemas through [`deliver_schema`]'s structured arm and
/// asserts the rendered `response_format` payload is strict-compliant at every
/// object level. Fails on `origin/main`, where the renderer sent the schema
/// verbatim and `findings.items` carried neither key.
/// Test: this test itself.
#[test]
fn the_sent_schemas_are_openai_strict_compliant() {
    let cases = [
        (
            crate::profile::batch_reviewer::PERIOD_FINDINGS_SCHEMA_NAME,
            crate::profile::batch_reviewer::period_findings_schema(),
        ),
        (
            crate::profile::synthesizer::SYNTHESIS_OUTPUT_SCHEMA_NAME,
            crate::profile::synthesizer::synthesis_output_schema(),
        ),
    ];

    for (name, schema) in cases {
        let (_, directive) = deliver_schema("BASE", name, schema, true);
        let directive = directive.expect("the structured arm yields a directive");
        let sent = directive.openai_response_format();
        assert_eq!(sent["json_schema"]["strict"], json!(true), "{sent}");
        assert_strict(&sent["json_schema"]["schema"], name);
    }
}
