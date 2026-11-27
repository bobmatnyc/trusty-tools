//! `dashboard_schema.py`'s free functions, one schema file at a time (#12029 P2a).
//!
//! Why: the script loads a schema only when a document of that collection
//! is checked, so a missing or broken file fails only the documents that
//! need it, and `validate_store` takes the documents as data, not a store.
//! [`super::SchemaDir`] loads a whole directory up front for the store's
//! write hook; this is the script's own surface, for its callers' ports.
//! What: [`mode`] (the `DASHBOARD_SCHEMA` value from an environment lookup),
//! [`load_schema`] (one `<name>.schema.json`, with the script's error text),
//! [`validate`] (one body, any JSON value), [`validate_store`] (every
//! document, then the cross references) and [`architect_do_missing`]. No
//! cache: the script's cache only saves reads of a file that does not change.
//! Test: `dashboard_schema_recordings_replay_clean_against_the_port`,
//! `architect_do_missing_names_each_gap`.

use std::path::Path;

use serde_json::Value;

use super::walk::{self, Patterns};
use super::{invariants, schema_name};
use crate::dashboard::error::DashError;
use crate::dashboard::ids::truthy;
use crate::dashboard::store::Snapshot;
use crate::dashboard::store::cross_refs;
use crate::pyjson_file::{LoadError, read_text};
use crate::pystr::{py_repr, strip};

use super::SchemaMode;

/// The `DASHBOARD_SCHEMA` values.
pub const MODES: [&str; 3] = ["off", "warn", "enforce"];

/// The validation mode `DASHBOARD_SCHEMA` names in `env`; `None` when it is
/// unset or empty (the caller picks its default).
///
/// # Errors
/// [`DashError::Schema`] for any other value, in the script's words.
pub fn mode(env: &dyn Fn(&str) -> Option<String>) -> Result<Option<SchemaMode>, DashError> {
    match env("DASHBOARD_SCHEMA").filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => v.parse().map(Some).map_err(|_| {
            DashError::Schema(format!(
                "DASHBOARD_SCHEMA={}: use one of {}",
                py_repr(&v),
                MODES.join(", ")
            ))
        }),
    }
}

/// A loaded schema and its compiled patterns.
#[derive(Debug)]
pub struct LoadedSchema {
    /// The schema document.
    pub schema: Value,
    pub(super) patterns: Patterns,
}

/// Loads `<dir>/<name>.schema.json`, refusing a keyword or pattern the
/// validator does not implement.
///
/// # Errors
/// [`DashError::Schema`]: `cannot load schema <path>: <Python's error>` for
/// a file that cannot be read or is not JSON, or the keyword refusal.
pub fn load_schema(dir: &Path, name: &str) -> Result<LoadedSchema, DashError> {
    let mut patterns = Patterns::new();
    let schema = load_file(
        &dir.join(format!("{name}.schema.json")),
        name,
        &mut patterns,
    )?;
    Ok(LoadedSchema { schema, patterns })
}

/// One schema file: read, decoded and keyword-checked into `patterns`.
pub(super) fn load_file(
    path: &Path,
    name: &str,
    patterns: &mut Patterns,
) -> Result<Value, DashError> {
    let fail = |e: String| DashError::Schema(format!("cannot load schema {}: {e}", path.display()));
    let text = read_text(path).map_err(|e| match e {
        LoadError::NotFound(t) | LoadError::Other(t) => fail(t),
    })?;
    crate::pyjson_load::loads(&text).map_err(|e| fail(e.py_str()))?;
    let schema: Value = serde_json::from_str(&text).map_err(|e| fail(e.to_string()))?;
    walk::check_keywords(&schema, name, patterns)?;
    Ok(schema)
}

/// Errors for one document body as `<coll>/<id>: <pointer>: <message>`;
/// empty when valid. A document with no schema is one error line.
///
/// # Errors
/// [`load_schema`]'s errors, an unresolved `$ref` or an unknown `type`.
pub fn validate(dir: &Path, coll: &str, id: &str, data: &Value) -> Result<Vec<String>, DashError> {
    let Some(name) = schema_name(coll, id) else {
        let meta = if coll == "meta" { " or meta id" } else { "" };
        return Ok(vec![format!(
            "{coll}/{id}: no schema for this collection{meta}"
        )]);
    };
    let loaded = load_schema(dir, &name)?;
    let mut errs = walk::check(&loaded.schema, data, &loaded.patterns)?;
    if let Value::Object(obj) = data {
        errs.extend(invariants(coll, obj));
    }
    Ok(errs
        .into_iter()
        .map(|e| format!("{coll}/{id}: {e}"))
        .collect())
}

/// Every document (collections in name order, documents as given), then
/// the cross references of [`cross_refs`].
///
/// # Errors
/// [`validate`]'s errors.
pub fn validate_store(dir: &Path, docs: &Snapshot) -> Result<Vec<String>, DashError> {
    let mut errs = Vec::new();
    for (coll, list) in docs {
        for d in list {
            errs.extend(validate(dir, coll, &d.id, &Value::Object(d.data.clone()))?);
        }
    }
    errs.extend(cross_refs(docs));
    Ok(errs)
}

/// What an Architect Do answer lacks: an IL task id, and a session or
/// project link. Informational: the schema accepts an unlinked answer.
pub fn architect_do_missing(data: &Value) -> Vec<&'static str> {
    let Value::Object(obj) = data else {
        return Vec::new();
    };
    // `str(data.get(k) or "").strip()` is empty.
    let blank = |k: &str| match obj.get(k) {
        Some(Value::String(s)) => strip(s).is_empty(),
        Some(v) => !truthy(v),
        None => true,
    };
    let linked = matches!(obj.get("projects"), Some(Value::Array(p)) if p.iter().any(truthy));
    let mut miss = Vec::new();
    if blank("task") {
        miss.push("an IL task id (task)");
    }
    if blank("session") && !linked {
        miss.push("a session or project link (session or projects)");
    }
    miss
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn architect_do_missing_names_each_gap() {
        let both = [
            "an IL task id (task)",
            "a session or project link (session or projects)",
        ];
        assert_eq!(architect_do_missing(&json!({})), both);
        assert_eq!(
            architect_do_missing(&json!({"task": " ", "projects": [0, ""]})),
            both
        );
        assert!(architect_do_missing(&json!({"task": 5, "projects": ["p"]})).is_empty());
        assert_eq!(
            architect_do_missing(&json!({"task": "t", "session": "\u{3000}"})),
            both[1..]
        );
        assert!(architect_do_missing(&json!([1])).is_empty());
    }
}
