//! Validator unit tests; the expected lines are the prototype's own output
//! (`dashboard_schema.check_schema` on the same schema and values).

use serde_json::json;

use super::walk::{Patterns, check, check_keywords};
use super::*;

fn schema() -> Value {
    json!({
        "$defs": {"ts": {"type": "string", "pattern": "^[0-9]{2}$"}},
        "type": "object", "required": ["a", "b"], "additionalProperties": false,
        "properties": {
            "a": {"type": "integer", "minimum": 1, "maximum": 5},
            "e": {"enum": ["x", "y", 1]}, "c": {"const": "k"},
            "s": {"type": "string", "minLength": 3}, "t": {"$ref": "#/$defs/ts"},
            "l": {"type": "array", "items": {"type": ["string", "null"]}},
            "o": {"oneOf": [{"type": "string"}, {"type": "integer"}, {"type": "number"}]},
            "f": {"type": "number", "maximum": 1.5},
            "m": {"type": "object", "additionalProperties": {"type": "boolean"}}
        }
    })
}

fn run(value: &Value) -> Result<Vec<String>, DashError> {
    let schema = schema();
    let mut patterns = Patterns::new();
    check_keywords(&schema, "t", &mut patterns)?;
    check(&schema, value, &patterns)
}

#[test]
fn walk_reports_each_keyword_like_the_prototype() -> Result<(), DashError> {
    let many = "é".repeat(70);
    let value = json!({"a": true, "e": 1.0, "c": "z", "s": "ab", "t": "123",
        "l": ["x", null, 3, true, {"q": 1}], "o": 2, "f": 2.25, "m": {"k": true, "j": "no"},
        "z": 1, "zz": many});
    assert_eq!(
        run(&value)?,
        [
            "/b: required property is missing",
            "/a: expected integer, got boolean",
            "/c: must be \"k\"",
            "/e: 1.0 is not one of [\"x\", \"y\", 1]",
            "/f: 2.25 is above 1.5",
            "/l/2: expected string|null, got int",
            "/l/3: expected string|null, got boolean",
            "/l/4: expected string|null, got dict",
            "/m/j: expected boolean, got str",
            "/o: matches 2 of the 3 allowed shapes (need exactly 1)",
            "/s: shorter than 3",
            "/t: '123' does not match ^[0-9]{2}$",
            "/z: property is not allowed",
            "/zz: property is not allowed",
        ]
    );
    let value = json!({"a": 0, "e": "é".repeat(70), "s": "abc", "t": "12"});
    let shown = format!("\"{}", "é".repeat(59));
    assert_eq!(
        run(&value)?,
        [
            "/b: required property is missing".to_owned(),
            "/a: 0 is below 1".to_owned(),
            format!("/e: {shown} is not one of [\"x\", \"y\", 1]"),
        ]
    );
    Ok(())
}

#[test]
fn walk_refuses_unknown_keywords_and_refs() {
    let mut patterns = Patterns::new();
    let bad = json!({"properties": {"x": {"type": "string", "format": "date"}}});
    let err = check_keywords(&bad, "items", &mut patterns).map_err(|e| e.to_string());
    assert_eq!(
        err,
        Err("items/properties/x: unsupported keyword 'format'".to_owned())
    );
    let bad_pattern = json!({"oneOf": [{"pattern": "\\d+"}]});
    assert!(check_keywords(&bad_pattern, "x", &mut patterns).is_err());
    let dangling = json!({"$ref": "#/$defs/nope"});
    assert!(matches!(
        check(&dangling, &json!(1), &patterns),
        Err(DashError::Schema(_))
    ));
}

#[test]
fn goal_invariants_are_checked() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    fs::write(
        tmp.path().join("goals.schema.json"),
        "{\"type\": \"object\"}",
    )?;
    let set = SchemaDir::load(tmp.path())?;
    let data = json!({"lifecycle": "parked", "kind": "factory"});
    let errs = set.validate("goals", "G01", data.as_object().ok_or("object")?)?;
    assert_eq!(
        errs,
        [
            "goals/G01: /lifecycle: 'parked' needs a parked object",
            "goals/G01: /board: required when kind is 'factory'",
        ]
    );
    let active = json!({"lifecycle": "active", "parked": {}});
    assert_eq!(
        set.validate("goals", "G02", active.as_object().ok_or("object")?)?
            .len(),
        1
    );
    let none = set.validate("meta", "other", &Map::new())?;
    assert_eq!(
        none,
        ["meta/other: no schema for this collection or meta id"]
    );
    Ok(())
}
