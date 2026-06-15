//! OpenAI strict-mode JSON Schema enforcement.
//!
//! Why: OpenAI's structured-output (`response_format: json_schema`, `strict:
//! true`) mode — which OpenRouter forwards verbatim for `openai/*` models —
//! rejects any schema in which an `object` node either omits
//! `"additionalProperties": false` or fails to list every one of its
//! `properties` keys in `"required"`.  Hand-written schemas reliably miss one
//! of these on nested objects (e.g. `findings.items`), producing
//! `Invalid schema for response_format 'review_output': ...
//! 'additionalProperties' is required to be supplied and to be false`, which
//! blocks EVERY OpenAI review.  Bedrock/Anthropic and Gemini are lenient and
//! accept the stricter-but-valid form unchanged, so enforcing strict mode for
//! all providers is safe.
//!
//! What: [`enforce_strict_mode`] recursively walks a `serde_json::Value` JSON
//! Schema and, for every node typed `"object"` (or carrying a `properties`
//! map), sets `"additionalProperties": false` and rewrites `"required"` to
//! list every property key.  It also descends into array `items`, nested
//! object `properties`, and the `additionalProperties` schema when it is
//! itself an object schema (rather than the boolean `false`).
//!
//! Test: see `schema_tests.rs` — covers the canonical review/verify shapes,
//! nested arrays-of-objects, idempotency, and the recursive invariant
//! (`assert_object_nodes_strict`).

use serde_json::Value;

/// Recursively make a JSON Schema OpenAI strict-mode compliant in place.
///
/// Why: a single source of truth for strict-mode compliance means individual
/// schema builders (`review_response_schema`, `verify_response_schema`) never
/// have to hand-maintain `additionalProperties`/`required` on every nested
/// object — they declare the shape and call this once, eliminating the class
/// of bug where a nested object silently violates strict mode.
/// What: for every object node, sets `"additionalProperties": false` and makes
/// `"required"` contain exactly the keys present under `"properties"` (sorted
/// for deterministic output).  Recurses into `properties` values, array
/// `items` (object or array of schemas), and a schema-valued
/// `additionalProperties`.  Non-object values are returned unchanged.
/// Test: `enforces_additional_properties_false`, `marks_all_properties_required`,
/// `recurses_into_array_items`, `is_idempotent` in `schema_tests.rs`.
pub fn enforce_strict_mode(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            // A node is a closed object schema — the kind strict mode
            // constrains — when it carries a `properties` map.  We deliberately
            // gate on `properties` (not a bare `"type": "object"`) so that a
            // map-style object (object-valued `additionalProperties`, no fixed
            // `properties`) is not mangled by overwriting its
            // `additionalProperties` with `false`.  Our review/verify schemas
            // contain no map types; this keeps the helper correct if one is
            // ever added.
            let has_properties = map.get("properties").is_some_and(Value::is_object);

            if has_properties {
                // Collect property keys first (immutable borrow) before mutating.
                let mut keys: Vec<String> = map
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|props| props.keys().cloned().collect())
                    .unwrap_or_default();
                keys.sort();

                map.insert(
                    "required".to_string(),
                    Value::Array(keys.into_iter().map(Value::String).collect()),
                );
                map.insert("additionalProperties".to_string(), Value::Bool(false));
            }

            // Recurse into every nested schema-bearing field.
            if let Some(props) = map.get_mut("properties").and_then(Value::as_object_mut) {
                for child in props.values_mut() {
                    enforce_strict_mode(child);
                }
            }
            if let Some(items) = map.get_mut("items") {
                enforce_strict_mode(items);
            }
            // `additionalProperties` may itself be a sub-schema (object) rather
            // than the boolean we may have set above; recurse only when it is
            // still an object schema.
            if let Some(ap) = map.get_mut("additionalProperties")
                && ap.is_object()
            {
                enforce_strict_mode(ap);
            }
        }
        Value::Array(items) => {
            // `items` may be an array of schemas (tuple validation); recurse each.
            for item in items {
                enforce_strict_mode(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
