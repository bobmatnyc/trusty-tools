//! JSON-Schema-subset validator for tool-call arguments.
//!
//! Why: Every tool's `schema()` (see `tools::traits::ToolExecutor`) emits an
//! OpenAI-function-style JSON Schema for its `parameters` object. Before a
//! model-produced tool call is dispatched, its arguments must be checked
//! against that schema so malformed calls are caught and reported precisely
//! (#1023) — feeding the bounded repair loop in `super::repair` — instead of
//! silently degrading to `{}` (the pre-#1023 behaviour in `agent_loop::parse_args`).
//! What: [`validate_args`] walks `args` against a `parameters` JSON Schema
//! object, supporting the subset every tool in this crate actually emits:
//! `type` (object/string/integer/number/boolean/array/null), `properties`,
//! `required`, `additionalProperties` (bool), `enum`, `minimum`/`maximum`, and
//! a single `items` schema for arrays. Collects every violation rather than
//! stopping at the first, so a repair message can address them all at once.
//! Test: `schema::tests::*`.

use serde_json::Value;

/// One schema-validation failure, with a JSON-pointer-like `path` for context.
///
/// Why: A repair message that says "arguments are invalid" is useless to a
/// model; naming the exact field and what was wrong lets the corrective
/// message be actionable.
/// What: `path` is a dotted/bracketed pointer from the argument root (`$`);
/// `message` is a human-readable description of the mismatch.
/// Test: `schema::tests::missing_required_property_reports_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolation {
    /// Location of the offending value, rooted at `$` (the whole arguments object).
    pub path: String,
    /// Human-readable description of the violation.
    pub message: String,
}

impl SchemaViolation {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SchemaViolation {
    /// Why: `repair::build_corrective_message` renders violations into the
    /// text sent back to the model; a stable `Display` keeps that formatting
    /// in one place.
    /// What: Renders as `"<path>: <message>"`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Validate `args` against a tool's `parameters` JSON Schema object.
///
/// Why: This is the single validation entry point shared by every extraction
/// strategy and every provider (#1023 acceptance criterion: "all providers
/// share same validation plumbing") — native, fenced-JSON, angle-bracket, and
/// balanced-scan results all funnel through here before dispatch.
/// What: Returns `Ok(())` when `args` satisfies `schema`; otherwise `Err` with
/// every violation found (never just the first).
/// Test: `schema::tests::*`.
pub fn validate_args(schema: &Value, args: &Value) -> Result<(), Vec<SchemaViolation>> {
    let mut violations = Vec::new();
    validate_value(schema, args, "$", &mut violations);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Recursive worker behind [`validate_args`].
///
/// Why: Separating the recursive walk from the public entry point keeps the
/// `path` threading (for precise violation messages) out of the public API.
/// What: Checks `type` and `enum` at this node, then recurses into
/// `properties`/`required`/`additionalProperties` for objects, `items` for
/// arrays, and `minimum`/`maximum` for numbers. A `type` mismatch stops
/// descent (there is nothing meaningful left to check underneath).
/// Test: Exercised via `validate_args` in every test below.
fn validate_value(schema: &Value, value: &Value, path: &str, out: &mut Vec<SchemaViolation>) {
    if let Some(expected) = schema.get("type").and_then(Value::as_str)
        && !type_matches(expected, value)
    {
        out.push(SchemaViolation::new(
            path,
            format!(
                "expected type '{expected}', got '{}'",
                value_type_name(value)
            ),
        ));
        return;
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        out.push(SchemaViolation::new(
            path,
            format!("value {value} is not one of the allowed enum values {allowed:?}"),
        ));
    }

    match value {
        Value::Object(map) => {
            let props = schema.get("properties").and_then(Value::as_object);
            if let Some(props) = props {
                for (key, subschema) in props {
                    if let Some(v) = map.get(key) {
                        validate_value(subschema, v, &format!("{path}.{key}"), out);
                    }
                }
            }
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for req in required {
                    if let Some(name) = req.as_str()
                        && !map.contains_key(name)
                    {
                        out.push(SchemaViolation::new(
                            path,
                            format!("missing required property '{name}'"),
                        ));
                    }
                }
            }
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                let allowed_keys = props;
                for key in map.keys() {
                    let known = allowed_keys.is_some_and(|p| p.contains_key(key));
                    if !known {
                        out.push(SchemaViolation::new(
                            path,
                            format!("unexpected property '{key}' (additionalProperties: false)"),
                        ));
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    validate_value(item_schema, item, &format!("{path}[{i}]"), out);
                }
            }
        }
        Value::Number(n) => {
            if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
                && n.as_f64().is_some_and(|v| v < min)
            {
                out.push(SchemaViolation::new(
                    path,
                    format!("value {n} is below minimum {min}"),
                ));
            }
            if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
                && n.as_f64().is_some_and(|v| v > max)
            {
                out.push(SchemaViolation::new(
                    path,
                    format!("value {n} exceeds maximum {max}"),
                ));
            }
        }
        _ => {}
    }
}

/// Whether `value`'s runtime JSON type matches a JSON-Schema `type` keyword.
///
/// Why: JSON Schema's `"integer"` has no direct `serde_json::Value` variant —
/// it is a `Number` with no fractional part — so this needs its own check
/// rather than a straight enum match.
/// What: Returns `true` for a matching type; an unrecognised `expected`
/// string is treated permissively (`true`) rather than rejecting every value,
/// matching JSON Schema's general laxness toward unknown keywords.
/// Test: `schema::tests::type_mismatch_is_reported`, `integer_type_rejects_float`.
fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "object" => value.is_object(),
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// Human-readable JSON type name for a value, used in violation messages.
///
/// Why: Error messages should say "got 'string'" not "got 'Value::String(..)'".
/// What: Maps each `Value` variant to its JSON Schema type name, distinguishing
/// `integer` from `number` the same way [`type_matches`] does.
/// Test: Exercised via violation message assertions in `schema::tests::*`.
fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "timeout_secs": { "type": "integer", "minimum": 1 }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    /// A fully valid argument object passes with no violations.
    ///
    /// Why: Guard the happy path before testing failure modes.
    /// What: Valid `{command, timeout_secs}` against `bash_schema()`.
    /// Test: this test.
    #[test]
    fn valid_args_pass() {
        let args = json!({"command": "ls", "timeout_secs": 30});
        assert_eq!(validate_args(&bash_schema(), &args), Ok(()));
    }

    /// A missing required property is reported with its name and root path.
    ///
    /// Why: The repair message must name the exact missing field.
    /// What: Omit `command`; assert the violation names it.
    /// Test: this test.
    #[test]
    fn missing_required_property_reports_path() {
        let args = json!({"timeout_secs": 30});
        let err = validate_args(&bash_schema(), &args).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].path, "$");
        assert!(err[0].message.contains("command"));
    }

    /// A type mismatch on a property is reported with the property's path.
    ///
    /// Why: Nested-path reporting lets a repair message point at the exact field.
    /// What: `command` as a number instead of a string.
    /// Test: this test.
    #[test]
    fn type_mismatch_is_reported() {
        let args = json!({"command": 123});
        let err = validate_args(&bash_schema(), &args).unwrap_err();
        assert!(err.iter().any(|v| v.path == "$.command"));
        assert!(err.iter().any(|v| v.message.contains("string")));
    }

    /// JSON Schema's `"integer"` rejects a fractional number.
    ///
    /// Why: `serde_json::Value::Number` conflates ints and floats; the
    /// validator must still distinguish them per JSON Schema semantics.
    /// What: `timeout_secs: 30.5` against an `"integer"` schema.
    /// Test: this test.
    #[test]
    fn integer_type_rejects_float() {
        let args = json!({"command": "ls", "timeout_secs": 30.5});
        let err = validate_args(&bash_schema(), &args).unwrap_err();
        assert!(err.iter().any(|v| v.path == "$.timeout_secs"));
    }

    /// A value below `minimum` is reported.
    ///
    /// Why: Range constraints must actually be enforced, not just parsed.
    /// What: `timeout_secs: 0` against `minimum: 1`.
    /// Test: this test.
    #[test]
    fn below_minimum_is_reported() {
        let args = json!({"command": "ls", "timeout_secs": 0});
        let err = validate_args(&bash_schema(), &args).unwrap_err();
        assert!(err.iter().any(|v| v.message.contains("minimum")));
    }

    /// An unexpected property is reported when `additionalProperties: false`.
    ///
    /// Why: Tools declare a closed argument shape; extra hallucinated keys
    /// should be caught rather than silently ignored.
    /// What: Add an unknown `foo` key.
    /// Test: this test.
    #[test]
    fn unexpected_property_is_reported() {
        let args = json!({"command": "ls", "foo": "bar"});
        let err = validate_args(&bash_schema(), &args).unwrap_err();
        assert!(err.iter().any(|v| v.message.contains("foo")));
    }

    /// All violations are collected, not just the first.
    ///
    /// Why: A single repair round-trip should let the model fix everything at
    /// once rather than playing whack-a-mole one violation per turn.
    /// What: Two independent violations in one payload; both are reported.
    /// Test: this test.
    #[test]
    fn collects_multiple_violations() {
        let args = json!({"timeout_secs": "not-a-number"});
        let err = validate_args(&bash_schema(), &args).unwrap_err();
        // missing `command` AND wrong type for `timeout_secs`.
        assert!(
            err.len() >= 2,
            "expected at least 2 violations, got {err:?}"
        );
    }

    /// `enum` restricts a string property to its declared values.
    ///
    /// Why: Some future tool schemas may restrict a field to a fixed set;
    /// validate that path even though no current tool schema uses it yet.
    /// What: A schema with an `enum` property; an out-of-set value fails.
    /// Test: this test.
    #[test]
    fn enum_violation_is_reported() {
        let schema = json!({
            "type": "object",
            "properties": { "mode": { "type": "string", "enum": ["fast", "slow"] } },
            "required": ["mode"]
        });
        let err = validate_args(&schema, &json!({"mode": "medium"})).unwrap_err();
        assert!(err.iter().any(|v| v.message.contains("enum")));
    }

    /// `items` validates each element of an array property.
    ///
    /// Why: Guard the array-recursion path even though no current tool uses it.
    /// What: An array property with a `string` item schema; a numeric element fails.
    /// Test: this test.
    #[test]
    fn array_items_are_validated() {
        let schema = json!({
            "type": "object",
            "properties": { "tags": { "type": "array", "items": { "type": "string" } } }
        });
        let err = validate_args(&schema, &json!({"tags": ["a", 1]})).unwrap_err();
        assert!(err.iter().any(|v| v.path == "$.tags[1]"));
    }

    /// `SchemaViolation`'s `Display` renders `"path: message"`.
    ///
    /// Why: `repair::build_corrective_message` depends on this exact shape.
    /// What: Format a violation, assert the rendered string.
    /// Test: this test.
    #[test]
    fn violation_display_format() {
        let v = SchemaViolation::new("$.command", "missing required property 'command'");
        assert_eq!(
            v.to_string(),
            "$.command: missing required property 'command'"
        );
    }
}
