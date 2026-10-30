//! The keyword-subset JSON Schema walk, ported from `dashboard_schema.py`.
//!
//! Why: the dashboard schemas are plain draft 2020-12 files that must load
//! unchanged (DOC-82 §2), and the crate stays a leaf with no schema engine
//! (Architect ruling Q2 for #10063). The prototype carries a validator for
//! exactly the keywords those files use; this is its port, messages included.
//! What: [`check_keywords`] refuses a schema with a keyword outside
//! [`KNOWN_KEYWORDS`]; [`check`] walks a value and returns
//! `"<json pointer>: <message>"` lines (empty = valid) for `type` (a string
//! or a list; a boolean is never an integer or number), `enum`, `const`,
//! `required`, `properties`, `additionalProperties` (boolean or schema),
//! `items`, `pattern`, `minimum`, `maximum`, `minLength`, `oneOf` and `$ref`
//! into the file's own `$defs`. A type mismatch stops the walk of that node,
//! as in the prototype.
//! Test: `walk_reports_each_keyword_like_the_prototype`,
//! `walk_refuses_unknown_keywords_and_refs`.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::pattern::Pattern;
use crate::dashboard::canonical::{py_float, python_dumps, python_dumps_utf8};
use crate::dashboard::error::DashError;
use crate::pystr::py_repr;

/// The keywords this validator implements.
pub const KNOWN_KEYWORDS: [&str; 18] = [
    "$schema",
    "$id",
    "$ref",
    "$defs",
    "title",
    "description",
    "type",
    "enum",
    "const",
    "required",
    "properties",
    "additionalProperties",
    "items",
    "pattern",
    "minimum",
    "maximum",
    "minLength",
    "oneOf",
];

/// Compiled `pattern`s, keyed by their source.
pub type Patterns = HashMap<String, Pattern>;

/// Refuses an unknown keyword anywhere in `node`, and compiles every
/// `pattern` into `patterns`.
///
/// # Errors
/// [`DashError::Schema`] naming the place and the keyword or pattern.
pub fn check_keywords(node: &Value, at: &str, patterns: &mut Patterns) -> Result<(), DashError> {
    let Some(obj) = node.as_object() else {
        return Ok(());
    };
    for (key, sub) in obj {
        if key == "properties" || key == "$defs" {
            for (name, child) in sub.as_object().into_iter().flatten() {
                check_keywords(child, &format!("{at}/{key}/{name}"), patterns)?;
            }
            continue;
        }
        if !KNOWN_KEYWORDS.contains(&key.as_str()) {
            return Err(DashError::Schema(format!(
                "{at}: unsupported keyword {}",
                py_repr(key)
            )));
        }
        match (key.as_str(), sub) {
            ("additionalProperties" | "items", Value::Object(_)) => {
                check_keywords(sub, &format!("{at}/{key}"), patterns)?
            }
            ("oneOf", Value::Array(list)) => {
                for (i, child) in list.iter().enumerate() {
                    check_keywords(child, &format!("{at}/oneOf/{i}"), patterns)?;
                }
            }
            ("pattern", Value::String(src)) if !patterns.contains_key(src) => {
                let compiled =
                    Pattern::compile(src).map_err(|e| DashError::Schema(format!("{at}: {e}")))?;
                patterns.insert(src.clone(), compiled);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Errors of `value` against `schema` (whose `$defs` resolve `$ref`).
///
/// # Errors
/// [`DashError::Schema`] for an unresolved `$ref` or an unknown `type`.
pub fn check(schema: &Value, value: &Value, patterns: &Patterns) -> Result<Vec<String>, DashError> {
    let mut errs = Vec::new();
    walk(schema, schema, value, "", patterns, &mut errs)?;
    Ok(errs)
}

fn resolve<'a>(root: &'a Value, reference: &str) -> Result<&'a Value, DashError> {
    let path = reference
        .strip_prefix("#/")
        .ok_or_else(|| DashError::Schema(format!("unsupported $ref {}", py_repr(reference))))?;
    path.split('/').try_fold(root, |node, part| {
        node.get(part)
            .ok_or_else(|| DashError::Schema(format!("unresolved $ref {}", py_repr(reference))))
    })
}

fn type_ok(value: &Value, t: &str) -> Result<bool, DashError> {
    Ok(match t {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        other => {
            return Err(DashError::Schema(format!(
                "unsupported type {}",
                py_repr(other)
            )));
        }
    })
}

/// Python's `type(value).__name__` (the prototype says `boolean` for a bool).
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python's `str()` of a number.
fn num_text(v: &Value) -> String {
    match v.as_f64() {
        Some(f) if v.is_f64() => py_float(f),
        _ => v.to_string(),
    }
}

/// The first `n` code points of `s`.
fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn walk(
    root: &Value,
    schema: &Value,
    value: &Value,
    ptr: &str,
    patterns: &Patterns,
    errs: &mut Vec<String>,
) -> Result<(), DashError> {
    let Some(sch) = schema.as_object() else {
        return Ok(());
    };
    if let Some(r) = sch.get("$ref").and_then(Value::as_str) {
        return walk(root, resolve(root, r)?, value, ptr, patterns, errs);
    }
    let at = if ptr.is_empty() { "/" } else { ptr };
    if let Some(t) = sch.get("type") {
        let types: Vec<&str> = match t {
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            other => other.as_str().into_iter().collect(),
        };
        let mut ok = false;
        for ty in &types {
            ok |= type_ok(value, ty)?;
        }
        if !ok {
            errs.push(format!(
                "{at}: expected {}, got {}",
                types.join("|"),
                type_name(value)
            ));
            return Ok(());
        }
    }
    if let Some(list) = sch
        .get("enum")
        .and_then(Value::as_array)
        .filter(|l| !l.contains(value))
    {
        let shown = head(&python_dumps_utf8(value), 60);
        errs.push(format!(
            "{at}: {shown} is not one of {}",
            python_dumps(&Value::Array(list.clone()), false)
        ));
    }
    if let Some(c) = sch.get("const").filter(|c| *c != value) {
        errs.push(format!("{at}: must be {}", python_dumps(c, false)));
    }
    if let Some(shapes) = sch.get("oneOf").and_then(Value::as_array) {
        let mut hits = 0;
        for sub in shapes {
            let mut trial = Vec::new();
            walk(root, sub, value, ptr, patterns, &mut trial)?;
            hits += usize::from(trial.is_empty());
        }
        if hits != 1 {
            errs.push(format!(
                "{at}: matches {hits} of the {} allowed shapes (need exactly 1)",
                shapes.len()
            ));
        }
    }
    if let Value::String(s) = value {
        if let Some(src) = sch.get("pattern").and_then(Value::as_str) {
            let hit = patterns.get(src).is_some_and(|p| p.search(s));
            if !hit {
                errs.push(format!(
                    "{at}: {} does not match {src}",
                    py_repr(&head(s, 60))
                ));
            }
        }
        if let Some(min) = sch
            .get("minLength")
            .filter(|m| m.as_u64().is_some_and(|m| (s.chars().count() as u64) < m))
        {
            errs.push(format!("{at}: shorter than {}", num_text(min)));
        }
    }
    if let Some(x) = value.as_f64().filter(|_| value.is_number()) {
        if let Some(min) = sch
            .get("minimum")
            .filter(|m| m.as_f64().is_some_and(|m| x < m))
        {
            errs.push(format!(
                "{at}: {} is below {}",
                num_text(value),
                num_text(min)
            ));
        }
        if let Some(max) = sch
            .get("maximum")
            .filter(|m| m.as_f64().is_some_and(|m| x > m))
        {
            errs.push(format!(
                "{at}: {} is above {}",
                num_text(value),
                num_text(max)
            ));
        }
    }
    if let (Value::Array(list), Some(items)) = (value, sch.get("items")) {
        for (i, v) in list.iter().enumerate() {
            walk(root, items, v, &format!("{ptr}/{i}"), patterns, errs)?;
        }
    }
    if let Value::Object(obj) = value {
        walk_object(root, sch, obj, ptr, patterns, errs)?;
    }
    Ok(())
}

fn walk_object(
    root: &Value,
    sch: &Map<String, Value>,
    obj: &Map<String, Value>,
    ptr: &str,
    patterns: &Patterns,
    errs: &mut Vec<String>,
) -> Result<(), DashError> {
    for req in sch
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(name) = req.as_str().filter(|n| !obj.contains_key(*n)) {
            errs.push(format!("{ptr}/{name}: required property is missing"));
        }
    }
    let props = sch.get("properties").and_then(Value::as_object);
    let extra = sch.get("additionalProperties");
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    for key in keys {
        let child = format!("{ptr}/{key}");
        match (props.and_then(|p| p.get(key)), extra) {
            (Some(sub), _) => walk(root, sub, &obj[key], &child, patterns, errs)?,
            (None, Some(Value::Bool(false))) => {
                errs.push(format!("{child}: property is not allowed"))
            }
            (None, Some(sub @ Value::Object(_))) => {
                walk(root, sub, &obj[key], &child, patterns, errs)?
            }
            _ => {}
        }
    }
    Ok(())
}
