//! Strict-mode normalization for the OpenAI `json_schema` dialect (#7082).
//!
//! Why: an OpenAI-family provider given `response_format: {type: json_schema,
//! json_schema: {strict: true, …}}` validates the schema document itself before
//! it runs anything, and rejects the call with `'additionalProperties' is
//! required to be supplied and to be false` unless every object node sets
//! `additionalProperties: false` and lists every one of its properties in
//! `required`. A hand-written schema reliably misses one of those on a nested
//! object, and the caller sees a 400 it cannot diagnose. The same defect has
//! now shipped three times in this workspace (#1235 and #5675 in trusty-review,
//! #7082 in trusty-git-analytics), so the fix belongs on the wire path every
//! caller already crosses rather than in each schema builder.
//! What: [`strict_json_schema`] returns a normalized copy of a JSON Schema —
//! the caller's value is never mutated — with `additionalProperties: false` and
//! a complete `required` list on every object node, recursing through
//! `properties`, `items` and `prefixItems` (single schema or tuple), `$defs`,
//! `definitions`, `anyOf`/`oneOf`/`allOf`, and an object-valued
//! `additionalProperties`.
//!
//! #7082: the gate here is broader than trusty-review's `enforce_strict_mode`
//! this replaced — that one closed a node only when it carried a `properties`
//! map, so a bare `{"type": "object"}` was left open and still 400'd. A node
//! declaring `"type": "object"` is now closed whether or not it lists
//! properties.
//!
//! Test: inline `tests` — `sets_additional_properties_false_on_every_object`,
//! `fills_required_with_every_property`, `normalizes_nested_defs_and_variants`,
//! `leaves_a_map_style_object_open`, `is_idempotent_and_does_not_mutate_input`,
//! `recurses_into_prefix_items`.

use serde_json::{Map, Value};

/// Keys whose value is a map of name → sub-schema.
const SUBSCHEMA_MAPS: [&str; 3] = ["properties", "$defs", "definitions"];

/// Keys whose value is a list of alternative sub-schemas.
const SUBSCHEMA_LISTS: [&str; 3] = ["anyOf", "oneOf", "allOf"];

/// Keys whose value is either one sub-schema or a positional tuple of them.
///
/// `prefixItems` is draft 2020-12's tuple keyword; `items` carries the tuple in
/// the older drafts and the element schema in 2020-12. Both spellings are
/// walked so an object nested under either cannot stay open (#7082).
const SUBSCHEMA_ITEMS: [&str; 2] = ["items", "prefixItems"];

/// Return a copy of `schema` that satisfies OpenAI strict-mode's schema rules.
///
/// Why: strict mode is what makes a schema a constraint rather than a hint, and
/// a provider that enforces it also polices the schema document — so the single
/// place the workspace renders the OpenAI dialect is the single place that can
/// guarantee compliance for every caller. Doing it here means a schema builder
/// in any crate stays a description of the shape it wants.
/// What: clones `schema`, then for every node that is a closed object schema
/// (it carries a `properties` map, or declares `"type": "object"`) sets
/// `additionalProperties: false` and rewrites `required` to list every property
/// key, sorted. A node whose `additionalProperties` is itself a sub-schema is a
/// map-style object: its value shape is kept and recursed into rather than
/// overwritten, because closing it would forbid the keys it exists to allow.
/// Recursion covers `properties`, `$defs`, `definitions`, `anyOf`, `oneOf`,
/// `allOf`, `items` and `prefixItems` in both the single-schema and tuple
/// forms, and an object-valued `additionalProperties`. Idempotent.
///
/// A property this normalization forces into `required` can still be omitted in
/// substance: give it a nullable type union (`["string", "null"]`) and the model
/// may answer `null`. A schema builder whose optional fields are NOT nullable
/// gets them made mandatory here, so it must declare the union itself — see
/// trusty-git-analytics' `period_findings_schema` for a worked example.
///
/// Test: `sets_additional_properties_false_on_every_object`,
/// `fills_required_with_every_property`, `normalizes_nested_defs_and_variants`,
/// `leaves_a_map_style_object_open`, `is_idempotent_and_does_not_mutate_input`,
/// `recurses_into_prefix_items`, `keeps_a_nullable_type_union_intact`.
pub fn strict_json_schema(schema: &Value) -> Value {
    let mut normalized = schema.clone();
    enforce(&mut normalized);
    normalized
}

/// Apply the strict-mode rules to `node` and everything below it, in place.
fn enforce(node: &mut Value) {
    match node {
        Value::Object(map) => {
            let has_properties = map.get("properties").is_some_and(Value::is_object);
            let is_closed_object = has_properties || declares_object_type(map);
            let has_schema_valued_additional = map
                .get("additionalProperties")
                .is_some_and(Value::is_object);

            if is_closed_object && !has_schema_valued_additional {
                map.insert("additionalProperties".to_string(), Value::Bool(false));
            }
            if has_properties {
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
            }

            for key in SUBSCHEMA_MAPS {
                if let Some(children) = map.get_mut(key).and_then(Value::as_object_mut) {
                    for child in children.values_mut() {
                        enforce(child);
                    }
                }
            }
            for key in SUBSCHEMA_LISTS {
                if let Some(children) = map.get_mut(key).and_then(Value::as_array_mut) {
                    for child in children {
                        enforce(child);
                    }
                }
            }
            // The `Value::Array` arm below covers the tuple form of either key.
            for key in SUBSCHEMA_ITEMS {
                if let Some(items) = map.get_mut(key) {
                    enforce(items);
                }
            }
            if let Some(additional) = map.get_mut("additionalProperties")
                && additional.is_object()
            {
                enforce(additional);
            }
        }
        Value::Array(items) => {
            for item in items {
                enforce(item);
            }
        }
        _ => {}
    }
}

/// Whether a schema node declares `"type": "object"`, in either the single-type
/// or the union-type spelling.
fn declares_object_type(map: &Map<String, Value>) -> bool {
    match map.get("type") {
        Some(Value::String(ty)) => ty == "object",
        Some(Value::Array(types)) => types.iter().any(|ty| ty.as_str() == Some("object")),
        _ => false,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Recursively assert every object node satisfies the strict-mode rules.
    fn assert_strict(schema: &Value) {
        match schema {
            Value::Object(map) => {
                let has_properties = map.get("properties").is_some_and(Value::is_object);
                let schema_valued_additional = map
                    .get("additionalProperties")
                    .is_some_and(Value::is_object);
                if (has_properties || declares_object_type(map)) && !schema_valued_additional {
                    assert_eq!(
                        map.get("additionalProperties"),
                        Some(&Value::Bool(false)),
                        "object node must close additionalProperties: {map:?}"
                    );
                }
                if has_properties {
                    let props: Vec<&String> = map["properties"]
                        .as_object()
                        .expect("properties map")
                        .keys()
                        .collect();
                    let required: Vec<&str> = map
                        .get("required")
                        .and_then(Value::as_array)
                        .map(|r| r.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    assert_eq!(
                        props.len(),
                        required.len(),
                        "every property must be required: {map:?}"
                    );
                    for key in props {
                        assert!(
                            required.contains(&key.as_str()),
                            "property {key} missing from required: {map:?}"
                        );
                    }
                }
                // Mirror `enforce`'s recursion so a property literally named
                // `type` or `properties` is not mistaken for a schema node.
                for key in SUBSCHEMA_MAPS {
                    if let Some(children) = map.get(key).and_then(Value::as_object) {
                        children.values().for_each(assert_strict);
                    }
                }
                for key in SUBSCHEMA_LISTS {
                    if let Some(children) = map.get(key).and_then(Value::as_array) {
                        children.iter().for_each(assert_strict);
                    }
                }
                for key in SUBSCHEMA_ITEMS {
                    if let Some(items) = map.get(key) {
                        assert_strict(items);
                    }
                }
                if let Some(additional) = map.get("additionalProperties")
                    && additional.is_object()
                {
                    assert_strict(additional);
                }
            }
            Value::Array(items) => items.iter().for_each(assert_strict),
            _ => {}
        }
    }

    fn nested() -> Value {
        json!({
            "type": "object",
            "properties": {
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string"},
                            "severity": {"type": "string"}
                        },
                        "required": ["kind"]
                    }
                }
            },
            "required": ["findings"]
        })
    }

    /// Why: the nested `items` object is exactly the node #7082 was filed for —
    /// the top level looking right is what made the omission survive review.
    /// Test: itself.
    #[test]
    fn sets_additional_properties_false_on_every_object() {
        let out = strict_json_schema(&nested());
        assert_eq!(out["additionalProperties"], json!(false));
        assert_eq!(
            out["properties"]["findings"]["items"]["additionalProperties"],
            json!(false)
        );
        assert_strict(&out);
    }

    /// Why: strict mode rejects a partial `required` as firmly as a missing
    /// `additionalProperties`, and the tga schema listed two of six keys.
    /// Test: itself.
    #[test]
    fn fills_required_with_every_property() {
        let out = strict_json_schema(&nested());
        assert_eq!(
            out["properties"]["findings"]["items"]["required"],
            json!(["kind", "severity"])
        );
    }

    /// Why: a schema that factors its shapes into `$defs` and branches through
    /// `anyOf` puts every object node out of reach of a `properties`-only walk.
    /// Test: itself.
    #[test]
    fn normalizes_nested_defs_and_variants() {
        let out = strict_json_schema(&json!({
            "type": "object",
            "properties": {
                "result": {"anyOf": [
                    {"type": "object", "properties": {"ok": {"type": "boolean"}}},
                    {"$ref": "#/$defs/failure"}
                ]}
            },
            "$defs": {
                "failure": {
                    "type": "object",
                    "properties": {"code": {"type": "integer"}, "message": {"type": "string"}}
                }
            }
        }));
        assert_eq!(
            out["properties"]["result"]["anyOf"][0]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            out["$defs"]["failure"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            out["$defs"]["failure"]["required"],
            json!(["code", "message"])
        );
        assert_strict(&out);
    }

    /// Why: a map-style object states its value shape in `additionalProperties`;
    /// closing it would forbid every key the schema exists to accept.
    /// Test: itself.
    #[test]
    fn leaves_a_map_style_object_open() {
        let out = strict_json_schema(&json!({
            "type": "object",
            "additionalProperties": {
                "type": "object",
                "properties": {"v": {"type": "string"}}
            }
        }));
        assert!(out["additionalProperties"].is_object(), "{out}");
        assert_eq!(
            out["additionalProperties"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(out["additionalProperties"]["required"], json!(["v"]));
    }

    /// Why: the helper runs on the wire path, so it must be safe to apply to an
    /// already-strict schema and must never edit the caller's own value.
    /// Test: itself.
    #[test]
    fn is_idempotent_and_does_not_mutate_input() {
        let input = nested();
        let once = strict_json_schema(&input);
        let twice = strict_json_schema(&once);
        assert_eq!(once, twice, "normalization must be idempotent");
        assert_eq!(input, nested(), "the caller's schema must be untouched");
    }

    /// Why: `prefixItems` is how draft 2020-12 spells a tuple, and a schema that
    /// uses it puts its object nodes out of reach of an `items`-only walk —
    /// reproducing #7082 on a schema that reads as already handled.
    /// Test: itself.
    #[test]
    fn recurses_into_prefix_items() {
        let out = strict_json_schema(&json!({
            "type": "object",
            "properties": {
                "pair": {
                    "type": "array",
                    "prefixItems": [
                        {"type": "object", "properties": {"a": {"type": "string"}}},
                        {"type": "object", "properties": {"b": {"type": "string"}}}
                    ]
                }
            }
        }));
        let prefix = &out["properties"]["pair"]["prefixItems"];
        assert_eq!(prefix[0]["additionalProperties"], json!(false), "{out}");
        assert_eq!(prefix[1]["additionalProperties"], json!(false), "{out}");
        assert_eq!(prefix[0]["required"], json!(["a"]));
        assert_eq!(prefix[1]["required"], json!(["b"]));
        assert_strict(&out);
    }

    /// Why (#7082 fix round): forcing every property into `required` is only
    /// safe because a builder can still make one optional in substance with a
    /// nullable type union. If normalization rewrote or dropped the `"null"`
    /// alternative, the model would be compelled to invent a value.
    /// What: normalizes a schema whose optional properties are `["string",
    /// "null"]` / `["number", "null"]`, and asserts both that the unions survive
    /// verbatim and that the keys are listed in `required`.
    /// Test: itself.
    #[test]
    fn keeps_a_nullable_type_union_intact() {
        let out = strict_json_schema(&json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string"},
                "severity": {"type": ["string", "null"], "enum": ["low", "high", null]},
                "confidence": {"type": ["number", "null"]}
            },
            "required": ["kind"]
        }));
        assert_eq!(
            out["properties"]["severity"]["type"],
            json!(["string", "null"]),
            "the null alternative must survive: {out}"
        );
        assert_eq!(
            out["properties"]["severity"]["enum"],
            json!(["low", "high", null]),
            "a nullable enum must keep its null member: {out}"
        );
        assert_eq!(
            out["properties"]["confidence"]["type"],
            json!(["number", "null"])
        );
        assert_eq!(out["required"], json!(["confidence", "kind", "severity"]));
        assert_strict(&out);
    }

    /// Why: a non-object schema has no object semantics to constrain — adding
    /// `required` to a string schema would be a 400 of our own making.
    /// Test: itself.
    #[test]
    fn leaves_non_object_schemas_untouched() {
        let input = json!({"type": "string", "enum": ["x", "y"]});
        assert_eq!(strict_json_schema(&input), input);
    }
}
