//! The store's schema hook and the dashboard schema set.
//!
//! Why: every dashboard document is described by a JSON Schema (DOC-82 §2),
//! and a write is checked against it (#10062 gap G15). The schema files are
//! vendored at #10063 PR 3; until then tests load them from the prototype
//! (Architect ruling Q1), so the store takes the set through a trait.
//! What: [`SchemaSet`] (the hook [`super::Store::with_schemas`] calls on each
//! written document), [`SchemaMode`] (`off`, `warn`: log and write,
//! `enforce`: refuse) and [`SchemaDir`], which loads every `*.schema.json` in
//! a directory, refuses any keyword or pattern the validator does not
//! implement, and checks a document by collection (`meta` by doc id) plus
//! the goal invariants JSON Schema cannot say. A document with no schema is
//! an error, as in the prototype.
//! Test: `walk_reports_each_keyword_like_the_prototype`,
//! `walk_refuses_unknown_keywords_and_refs`, `goal_invariants_are_checked`,
//! `schema_enforce_refuses_and_warn_writes`, `prototype_schemas_load_and_fixtures_validate`.

mod pattern;
mod walk;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use serde_json::{Map, Value};

use super::error::DashError;
pub use pattern::Pattern;
use walk::Patterns;

/// Collections with a schema of the same name.
pub const COLLECTIONS: [&str; 11] = [
    "items",
    "sessions",
    "goals",
    "goal_reorders",
    "goal_removals",
    "step_progress",
    "accounts",
    "effort_tunes",
    "il_steers",
    "briefs",
    "questions",
];
/// `meta` doc ids with a `meta-<id>` schema.
pub const META_IDS: [&str; 4] = ["state", "settings", "seen", "gh_track"];

/// What a store does with schema errors on a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaMode {
    /// No check.
    Off,
    /// Log each error and write.
    Warn,
    /// Refuse the batch with every error.
    Enforce,
}

impl FromStr for SchemaMode {
    type Err = DashError;

    fn from_str(s: &str) -> Result<Self, DashError> {
        match s {
            "off" => Ok(Self::Off),
            "warn" => Ok(Self::Warn),
            "enforce" => Ok(Self::Enforce),
            other => Err(DashError::Invalid(format!(
                "schema mode {other:?}: use off, warn or enforce"
            ))),
        }
    }
}

/// Validates one document body.
pub trait SchemaSet: Send + Sync {
    /// `"<coll>/<id>: <pointer>: <message>"` lines; empty when valid.
    ///
    /// # Errors
    /// [`DashError::Schema`] when the schema itself cannot be applied.
    fn validate(
        &self,
        collection: &str,
        id: &str,
        data: &Map<String, Value>,
    ) -> Result<Vec<String>, DashError>;
}

/// The schema file name (without `.schema.json`) for a document, if any.
pub fn schema_name(collection: &str, id: &str) -> Option<String> {
    if collection == "meta" {
        return META_IDS.contains(&id).then(|| format!("meta-{id}"));
    }
    COLLECTIONS
        .contains(&collection)
        .then(|| collection.to_owned())
}

/// The `*.schema.json` files of one directory, keyword-checked at load.
#[derive(Debug)]
pub struct SchemaDir {
    schemas: BTreeMap<String, Value>,
    patterns: Patterns,
}

impl SchemaDir {
    /// Loads every `<name>.schema.json` in `dir`.
    ///
    /// # Errors
    /// [`DashError::Schema`] for a file that is not JSON, or that uses a
    /// keyword or pattern the validator does not implement;
    /// [`DashError::Io`] when the directory cannot be read.
    pub fn load(dir: &Path) -> Result<Self, DashError> {
        let mut schemas = BTreeMap::new();
        let mut patterns = Patterns::new();
        for entry in fs::read_dir(dir).map_err(DashError::io(dir))? {
            let path = entry.map_err(DashError::io(dir))?.path();
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".schema.json"))
                .map(str::to_owned)
            else {
                continue;
            };
            let text = fs::read_to_string(&path).map_err(DashError::io(&path))?;
            let schema: Value = serde_json::from_str(&text).map_err(|e| {
                DashError::Schema(format!("cannot load schema {}: {e}", path.display()))
            })?;
            walk::check_keywords(&schema, &name, &mut patterns)?;
            schemas.insert(name, schema);
        }
        Ok(Self { schemas, patterns })
    }

    /// The loaded schema names, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.schemas.keys().map(String::as_str).collect()
    }
}

impl SchemaSet for SchemaDir {
    fn validate(
        &self,
        collection: &str,
        id: &str,
        data: &Map<String, Value>,
    ) -> Result<Vec<String>, DashError> {
        let Some(schema) = schema_name(collection, id).and_then(|n| self.schemas.get(&n)) else {
            let meta = if collection == "meta" {
                " or meta id"
            } else {
                ""
            };
            return Ok(vec![format!(
                "{collection}/{id}: no schema for this collection{meta}"
            )]);
        };
        let value = Value::Object(data.clone());
        let mut errs = walk::check(schema, &value, &self.patterns)?;
        errs.extend(invariants(collection, data));
        Ok(errs
            .into_iter()
            .map(|e| format!("{collection}/{id}: {e}"))
            .collect())
    }
}

/// The goal rules JSON Schema cannot say: lifecycle `parked` needs a
/// `parked` object, a factory goal needs a `board`, an active goal has no
/// `parked` object.
fn invariants(collection: &str, data: &Map<String, Value>) -> Vec<String> {
    let mut errs = Vec::new();
    if collection != "goals" {
        return errs;
    }
    let life = data.get("lifecycle").and_then(Value::as_str);
    let parked = data.get("parked").is_some_and(Value::is_object);
    if life == Some("parked") && !parked {
        errs.push("/lifecycle: 'parked' needs a parked object".to_owned());
    }
    if data.get("kind").and_then(Value::as_str) == Some("factory")
        && !data.get("board").is_some_and(Value::is_object)
    {
        errs.push("/board: required when kind is 'factory'".to_owned());
    }
    if life == Some("active") && parked {
        errs.push(
            "/parked: set while lifecycle is 'active' (park writes lifecycle 'parked')".to_owned(),
        );
    }
    errs
}

#[cfg(test)]
mod tests;
