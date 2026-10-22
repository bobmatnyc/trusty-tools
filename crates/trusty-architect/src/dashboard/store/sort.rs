//! The prototype's `list --order-by` sort key (moved out of `mod.rs`,
//! #10063 PR 2, to keep it under the SLOC cap).
//!
//! Why: `dashboard_store.py` orders mixed-type values by a key Python builds;
//! the port must order them the same way.
//! What: [`sort_key`] ranks numbers, then strings, then containers (by their
//! `json.dumps` text), then booleans and null (by Python's `str`).
//! Test: `list_filters_orders_and_limits_like_the_prototype`.

use std::cmp::Ordering;

use serde_json::Value;

use super::super::canonical::python_dumps;

/// The prototype's list sort key: numbers, then strings, then containers (by
/// their `json.dumps` text), then booleans and null (by Python's `str`).
pub(super) enum SortKey {
    Number(f64),
    Text(String),
    Container(String),
    Other(&'static str),
}

impl SortKey {
    fn rank(&self) -> u8 {
        match self {
            Self::Number(_) => 0,
            Self::Text(_) => 1,
            Self::Container(_) => 2,
            Self::Other(_) => 3,
        }
    }
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Self::Text(a), Self::Text(b)) | (Self::Container(a), Self::Container(b)) => a.cmp(b),
            (Self::Other(a), Self::Other(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SortKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for SortKey {}

pub(super) fn sort_key(v: &Value) -> SortKey {
    match v {
        Value::Number(n) => SortKey::Number(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => SortKey::Text(s.clone()),
        Value::Array(_) | Value::Object(_) => SortKey::Container(python_dumps(v, true)),
        Value::Bool(true) => SortKey::Other("True"),
        Value::Bool(false) => SortKey::Other("False"),
        Value::Null => SortKey::Other("None"),
    }
}
