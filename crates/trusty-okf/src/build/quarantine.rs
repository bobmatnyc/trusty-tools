//! The §2.13.1 quarantine of protected source keys (SPEC-OKF-01 §2.13.1).
//!
//! What: [`Quarantine`] collects the source values the ingest engine may not
//! place at the top level; they go, unchanged, under `source_fields`, with
//! one `quarantined_key` finding per key and item. #10746 S8: split from
//! `ingest` for the file-size cap, unchanged.
//! Test: `c116_protected_keys_from_every_input_are_quarantined`.

use std::collections::BTreeSet;

use serde_yaml::{Mapping, Value};

use crate::build::ingest::IngestFinding;

/// Quarantined values for one item, plus the findings they produce.
#[derive(Default)]
pub(crate) struct Quarantine {
    pub(crate) values: Mapping,
    /// Keys whose entry already holds a list of distinct source values.
    multi: BTreeSet<String>,
    pub(crate) findings: Vec<IngestFinding>,
}

impl Quarantine {
    /// Move `key: value` under `source_fields`, unchanged. A second distinct
    /// value for one key turns the entry into a list of the values, so
    /// nothing is dropped.
    pub(crate) fn hold(&mut self, item_id: &str, key: String, value: Value) {
        let k = Value::String(key.clone());
        match self.values.get_mut(&k) {
            None => {
                self.values.insert(k, value);
            }
            Some(prior) if *prior == value => {}
            Some(prior) => {
                if self.multi.insert(key.clone()) {
                    let first = std::mem::take(prior);
                    *prior = Value::Sequence(vec![first, value]);
                } else if let Value::Sequence(list) = prior
                    && !list.contains(&value)
                {
                    list.push(value);
                }
            }
        }
        let finding = IngestFinding {
            kind: "quarantined_key".to_string(),
            item_id: item_id.to_string(),
            key,
        };
        if !self.findings.contains(&finding) {
            self.findings.push(finding);
        }
    }
}
