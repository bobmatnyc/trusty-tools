//! The technical-level refusal: a level key outside the six values.
//!
//! Why: #11064 ruling (a) says a write of `meta/settings.technical_level` or
//! an item's `level_override` outside the six levels is refused. The schema
//! check only logs in warn mode and is off when no schemas are attached, so
//! the refusal must hold whatever the schema mode, as the prototype's
//! `technical_level.refusal` does.
//! What: [`TECHNICAL_LEVELS`] is the one Rust list of the six levels, in
//! `technical_level.py`'s `LEVELS` order. [`Store::refuse_bad_levels`]
//! refuses the first `set` or `update` whose written fields carry the level
//! key of its document with any other value (`null` included). Only a key
//! present in the write is checked, so an unrelated write never fails on it.
//! Test: `technical_level_outside_the_six_is_refused_on_set`,
//! `technical_level_outside_the_six_is_refused_on_update`,
//! `technical_level_outside_the_six_aborts_the_batch`.

use serde_json::Value;

use super::{Op, Store};
use crate::dashboard::error::DashError;

// #11064: the six levels; mirrors `LEVELS` in `scripts/technical_level.py`.
/// The six technical levels, lowest to highest.
pub(crate) const TECHNICAL_LEVELS: [&str; 6] = [
    "non-technical",
    "vibe-coder",
    "pm",
    "engineer",
    "senior-engineer",
    "cto",
];

/// The level key a write to `coll/id` may carry, if any.
fn level_key(coll: &str, id: &str) -> Option<&'static str> {
    match (coll, id) {
        ("meta", "settings") => Some("technical_level"),
        ("items", _) => Some("level_override"),
        _ => None,
    }
}

impl Store {
    /// Refuses the first op whose body sets its level key outside
    /// [`TECHNICAL_LEVELS`]; the text names the op and the value, cut to 40
    /// characters, as the prototype's refusal does.
    pub(super) fn refuse_bad_levels(&self, ops: &[Op]) -> Result<(), DashError> {
        for (i, op) in ops.iter().enumerate() {
            let (coll, id, data) = op.scan_parts();
            let Some(key) = level_key(coll, id) else {
                continue;
            };
            let Some(value) = data.and_then(Value::as_object).and_then(|m| m.get(key)) else {
                continue;
            };
            if value
                .as_str()
                .is_some_and(|v| TECHNICAL_LEVELS.contains(&v))
            {
                continue;
            }
            let shown: String = value.to_string().chars().take(40).collect();
            return Err(DashError::Invalid(format!(
                "op {n} refused: {coll}/{id} {key} {shown} is not a technical level \
                 (expected one of: {})",
                TECHNICAL_LEVELS.join(", "),
                n = i + 1,
            )));
        }
        Ok(())
    }
}
