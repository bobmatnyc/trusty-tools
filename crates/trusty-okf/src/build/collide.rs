//! Path collisions at the ingest boundary (SPEC-OKF-01 §2.9 "Path
//! collisions", §2.14 rule 4).
//!
//! Why: two items that map to one concept path would overwrite each other,
//! and the report would count both as written. A ledger record pins the path
//! it wrote, so a later item must not take over an existing concept either.
//! What: [`Pins`] maps the §2.11 fold key of every concept path any ledger
//! in the bundle names to the `(store, item)` pairs that claim it.
//! [`refused`] takes one run's `(item, target)` pairs and returns the items
//! that collide: every item of a fold-key group of two or more, except the
//! item the ledger already pins there; and every item whose target another
//! item pins. `build`, `rebuild` and `import` share it. Slice S8 of #10746.
//! Test: `c138_colliding_items_are_refused_and_pins_hold`,
//! `c75_colliding_sources_are_refused_and_pins_hold`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::OkfError;
use crate::build::ingest::{SourceItem, concept_id_for};
use crate::build::ledger::Ledger;
use crate::build::registry::{SourceRegistry, SourceSpec};
use crate::bundle::paths::fold_key;
use crate::format::concept::slugify;
use crate::format::id::ConceptId;

/// Fold key of a concept path to the `(store, item)` pairs whose ledger
/// record names it.
#[derive(Debug, Default)]
pub(crate) struct Pins(BTreeMap<String, Vec<(String, String)>>);

impl Pins {
    /// The pins of every registered store's ledger, plus `extra` (a store
    /// the caller is about to register, or one ingested without a row).
    pub(crate) fn load(root: &Path, extra: &str) -> Result<Pins, OkfError> {
        let mut stores: BTreeSet<String> = SourceRegistry::load(root)?
            .sources
            .into_iter()
            .map(|s| slugify(&s.id))
            .collect();
        stores.insert(slugify(extra));
        let mut pins = Pins::default();
        for store in stores {
            let ledger = Ledger::load(root, &store)?;
            for r in ledger.records() {
                pins.add(&store, &r.item_id, &r.entity);
            }
        }
        Ok(pins)
    }

    fn add(&mut self, store: &str, item: &str, entity: &str) {
        let key = fold_key(&format!("{entity}.md"));
        let claim = (store.to_string(), item.to_string());
        let claims = self.0.entry(key).or_default();
        if !claims.contains(&claim) {
            claims.push(claim);
        }
    }

    fn claims(&self, id: &ConceptId) -> &[(String, String)] {
        self.0
            .get(&fold_key(&id.file_name()))
            .map_or(&[], Vec::as_slice)
    }
}

/// The items of one run of `store` that must not be written (§2.9).
///
/// What: `targets` pairs each item ID with the concept it would write; see
/// the module doc for the rule. The result names refused item IDs.
pub(crate) fn refused(
    store: &str,
    targets: &[(String, ConceptId)],
    pins: &Pins,
) -> BTreeSet<String> {
    // A ledger file is named by the slug of its store ID; so are the pins.
    let store = slugify(store);
    let mut groups: BTreeMap<String, Vec<&(String, ConceptId)>> = BTreeMap::new();
    for t in targets {
        groups
            .entry(fold_key(&t.1.file_name()))
            .or_default()
            .push(t);
    }
    let mut out = BTreeSet::new();
    for group in groups.values() {
        for (item, id) in group {
            let claims = pins.claims(id);
            let owns = claims.iter().any(|(s, i)| *s == store && i == item);
            let taken = !claims.is_empty() && !owns;
            let shared = group.len() > 1 && !owns;
            if taken || shared {
                out.insert(item.clone());
            }
        }
    }
    out
}

/// #10746 S8: each item's concept: the full ID its ledger record pins, else
/// [`concept_id_for`]. Items whose computed ID does not parse are returned
/// apart, with the error.
pub(crate) fn targets_for<'a>(
    ledger: &Ledger,
    spec: &SourceSpec,
    items: &'a [SourceItem],
) -> (Vec<(String, ConceptId)>, BTreeMap<&'a str, OkfError>) {
    let mut targets = Vec::new();
    let mut bad = BTreeMap::new();
    for item in items {
        let id = match ledger.get(&item.item_id) {
            Some(r) => ConceptId::parse(&r.entity),
            None => concept_id_for(&spec.collection, &item.name),
        };
        match id {
            Ok(id) => targets.push((item.item_id.clone(), id)),
            Err(e) => {
                bad.insert(item.item_id.as_str(), e);
            }
        }
    }
    (targets, bad)
}
