//! Unit tests for `enforce_strict_mode`.
//!
//! Why: OpenAI strict-mode compliance is a hard provider requirement — a
//! single non-compliant object node blocks every OpenAI review.  These tests
//! lock the recursive invariant so a future schema change cannot silently
//! reintroduce the bug.
//! What: exercises the canonical review/verify shapes, nested arrays of
//! objects, and idempotency, and provides a reusable recursive assertion
//! (`assert_object_nodes_strict`) used here and re-exported for the prompt
//! schema tests.
//! Test: this file IS the tests.

use serde_json::{Value, json};

use super::enforce_strict_mode;

/// Recursively assert every object node in a JSON Schema is strict-compliant.
///
/// Why: the strict-mode contract is recursive — top-level, array `items`, and
/// nested object properties must ALL satisfy it; a flat check would miss the
/// exact bug we are fixing (`findings.items` lacked `additionalProperties`).
/// What: for every node that is a closed object schema (carries a `properties`
/// map), asserts `additionalProperties == false` and that `required` lists
/// exactly the `properties` keys; recurses into `properties`, `items`, and
/// object-valued `additionalProperties`.
/// Test: used by the strict-mode assertions in this file.
fn assert_object_nodes_strict(schema: &Value) {
    if let Value::Object(map) = schema {
        // Mirror `enforce_strict_mode`: a node is a closed object schema (the
        // kind strict mode constrains) when it carries a `properties` map.
        let has_properties = map.get("properties").is_some_and(Value::is_object);

        if has_properties {
            assert_eq!(
                map.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "object node must set additionalProperties:false — node: {map:?}"
            );

            let prop_keys: std::collections::BTreeSet<String> = map
                .get("properties")
                .and_then(Value::as_object)
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default();
            let required_keys: std::collections::BTreeSet<String> = map
                .get("required")
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                prop_keys, required_keys,
                "every property must be required (and vice versa) — node: {map:?}"
            );
        }

        if let Some(props) = map.get("properties").and_then(Value::as_object) {
            for child in props.values() {
                assert_object_nodes_strict(child);
            }
        }
        if let Some(items) = map.get("items") {
            assert_object_nodes_strict(items);
        }
        if let Some(ap) = map.get("additionalProperties")
            && ap.is_object()
        {
            assert_object_nodes_strict(ap);
        }
    } else if let Value::Array(items) = schema {
        for item in items {
            assert_object_nodes_strict(item);
        }
    }
}

#[test]
fn enforces_additional_properties_false() {
    let mut schema = json!({
        "type": "object",
        "properties": { "a": { "type": "string" } },
        "required": ["a"]
    });
    enforce_strict_mode(&mut schema);
    assert_eq!(schema["additionalProperties"], json!(false));
}

#[test]
fn marks_all_properties_required() {
    // Only `a` was required; strict mode must add `b` too.
    let mut schema = json!({
        "type": "object",
        "properties": { "a": { "type": "string" }, "b": { "type": "number" } },
        "required": ["a"]
    });
    enforce_strict_mode(&mut schema);
    let required: std::collections::BTreeSet<&str> = schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(required, ["a", "b"].into_iter().collect());
}

#[test]
fn recurses_into_array_items() {
    // The exact bug shape: an array whose items object lacks strict fields.
    let mut schema = json!({
        "type": "object",
        "properties": {
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "title": { "type": "string" }, "line": { "type": ["integer", "null"] } },
                    "required": ["title"]
                }
            }
        },
        "required": ["findings"]
    });
    enforce_strict_mode(&mut schema);
    assert_object_nodes_strict(&schema);
    // Specifically assert the nested items object was fixed.
    let items = &schema["properties"]["findings"]["items"];
    assert_eq!(items["additionalProperties"], json!(false));
    let required: std::collections::BTreeSet<&str> = items["required"]
        .as_array()
        .expect("required")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(required, ["line", "title"].into_iter().collect());
}

#[test]
fn is_idempotent() {
    let mut schema = json!({
        "type": "object",
        "properties": { "a": { "type": "string" } },
        "required": ["a"]
    });
    enforce_strict_mode(&mut schema);
    let once = schema.clone();
    enforce_strict_mode(&mut schema);
    assert_eq!(schema, once, "applying strict mode twice must be a no-op");
}

#[test]
fn leaves_non_object_schemas_untouched() {
    // A bare string schema has no object semantics; must not gain `required`.
    let mut schema = json!({ "type": "string", "enum": ["x", "y"] });
    let before = schema.clone();
    enforce_strict_mode(&mut schema);
    assert_eq!(schema, before);
}

#[test]
fn recurses_into_object_valued_additional_properties() {
    // A map type whose additionalProperties is itself an object schema.
    let mut schema = json!({
        "type": "object",
        "properties": {
            "meta": {
                "type": "object",
                "additionalProperties": {
                    "type": "object",
                    "properties": { "v": { "type": "string" } }
                }
            }
        }
    });
    enforce_strict_mode(&mut schema);
    // The inner additionalProperties object schema must itself be strict.
    let inner = &schema["properties"]["meta"]["properties"];
    // `meta` had no `properties`, so its additionalProperties stays the object
    // schema (we only overwrite to `false` when properties drive the node);
    // verify recursion still made the inner schema strict.
    let _ = inner; // meta has no properties; assert via the additionalProperties path
    let ap = &schema["properties"]["meta"]["additionalProperties"];
    assert_object_nodes_strict(ap);
}
