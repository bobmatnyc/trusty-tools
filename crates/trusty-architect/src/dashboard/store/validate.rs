//! Store-wide `validate`: every document against its schema, then the
//! references between documents.
//!
//! Why: a write checks one document; a goal removed later leaves items that
//! still name it. `dashboard-db.py validate` (`validate_store`) checks the
//! whole store and the cross references, and the Rust store must answer the
//! same (#10063 PR 2).
//! What: [`Store::validate`] lists every collection, validates each document
//! with the [`SchemaSet`] (collections in name order, documents in id
//! order), then reports an item whose string `goal` is not a goal id and a
//! `goal_removals` doc whose `goal` is not one (when the store has goals),
//! and a `step_progress` doc whose `item` is not an item id (when it has
//! items), in the prototype's words.
//! Test: `validate_reports_dangling_cross_references`,
//! `validate_missing_root_is_an_error`.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;

use super::super::error::DashError;
use super::super::ids::py_str;
use super::super::schema_check::SchemaSet;
use super::{Doc, Store, fsck::require_root};

/// The result of [`Store::validate`], in the prototype's JSON shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Validation {
    /// Documents checked.
    pub documents: usize,
    /// `"<coll>/<id>: <pointer>: <message>"` lines; empty when valid.
    pub errors: Vec<String>,
}

impl Store {
    /// Validates every document and the cross references.
    ///
    /// # Errors
    /// [`DashError::NotFound`] when the root does not exist,
    /// [`DashError::Schema`] when a schema cannot be applied, and
    /// [`Store::list`]'s errors.
    pub fn validate(&self, schemas: &dyn SchemaSet) -> Result<Validation, DashError> {
        require_root(&self.root)?;
        let colls = self.collections()?;
        let refs: Vec<&str> = colls.iter().map(String::as_str).collect();
        let snap = self.snapshot(&refs)?;
        let mut errors = Vec::new();
        for (coll, docs) in &snap {
            for d in docs {
                errors.extend(schemas.validate(coll, &d.id, &d.data)?);
            }
        }
        let ids = |coll: &str| -> BTreeSet<&str> {
            snap.get(coll)
                .into_iter()
                .flatten()
                .map(|d| d.id.as_str())
                .collect()
        };
        let docs = |coll: &str| snap.get(coll).into_iter().flatten();
        let (goals, items) = (ids("goals"), ids("items"));
        let missing = |d: &Doc, key: &str, set: &BTreeSet<&str>| {
            let v = d.data.get(key);
            let found = v.and_then(Value::as_str).is_some_and(|s| set.contains(s));
            (!found).then(|| v.map_or_else(|| "None".to_owned(), py_str))
        };
        if snap.contains_key("goals") {
            for d in docs("items") {
                if let Some(Value::String(g)) = d.data.get("goal")
                    && !goals.contains(g.as_str())
                {
                    errors.push(format!(
                        "items/{}: /goal: {g} is not a goal id in the store",
                        d.id
                    ));
                }
            }
            for d in docs("goal_removals") {
                if let Some(g) = missing(d, "goal", &goals) {
                    errors.push(format!(
                        "goal_removals/{}: /goal: {g} is not a goal id in the store",
                        d.id
                    ));
                }
            }
        }
        if snap.contains_key("items") {
            for d in docs("step_progress") {
                if let Some(i) = missing(d, "item", &items) {
                    errors.push(format!(
                        "step_progress/{}: /item: {i} is not an item id in the store",
                        d.id
                    ));
                }
            }
        }
        let documents = snap.values().map(Vec::len).sum();
        Ok(Validation { documents, errors })
    }
}
