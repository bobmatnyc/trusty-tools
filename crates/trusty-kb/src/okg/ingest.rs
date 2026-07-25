//! The ingest engine — fetched items in, ledger-gated KB entities out.
//!
//! Why: this is the one place that decides "write, replace, skip, or tombstone",
//! and it is deliberately FETCHER-AGNOSTIC. Doc-store scanning is pure
//! filesystem work and lives in this crate; Gmail and Drive must go through the
//! existing authenticated `trusty-gworkspace` client, which has no place in a
//! deterministic, network-free crate. Splitting at [`SourceItem`] means every
//! idempotency property — re-run adds nothing, a widened window adds only the
//! older items, a changed file replaces its entity — is unit-testable with
//! synthetic items and no network at all.
//!
//! What: [`SourceItem`] is one normalised fetched thing. [`KbStore::ingest_items`]
//! walks them against the source's [`Ledger`], writing entities through the
//! existing deterministic [`KbStore::put_entity`] path (overwrite mode, so a
//! re-ingest REPLACES the prior entry rather than accreting), appending one
//! durable ledger line per item as it goes. [`IngestReport`] is the caller-facing
//! summary.
//!
//! Test: `rerun_ingests_nothing_new`, `changed_item_replaces_entity`,
//! `wider_window_only_adds_older_items`, `deletion_tombstones_when_enabled`,
//! `volatile_item_is_never_skipped_on_a_later_run`,
//! `sweep_tombstones_only_after_the_full_set_is_known`,
//! `merge_folds_chunk_reports`, `per_item_errors_do_not_abort_the_run`
//! (all in the sibling `ingest_tests.rs`).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_yaml::Value as Yaml;

use crate::okg::ledger::{ItemStatus, Ledger, LedgerRecord, Watermark};
use crate::okg::registry::SourceSpec;
use crate::store::KbStore;

/// How many entity paths an [`IngestReport`] lists before truncating.
const MAX_REPORTED_ENTITIES: usize = 50;

/// One normalised item fetched from a source, ready to become an entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceItem {
    /// The source's own stable id for this item (relative path, message id,
    /// file id). Ledger key — must be stable across runs or nothing dedupes.
    pub item_id: String,
    /// Change token: content hash, revision, or an immutable id.
    pub fingerprint: String,
    /// Entity display name; slugged for the filename.
    pub name: String,
    /// Human title for the frontmatter.
    pub title: String,
    /// The item's own timestamp, ISO-8601, when known.
    pub timestamp: Option<String>,
    /// Markdown body.
    pub body: String,
    /// Extra provenance frontmatter (`from`, `source_path`, `url`, …).
    pub fields: BTreeMap<String, String>,
    /// This item has NO reliable change signal, so its fingerprint carries no
    /// information and must never be used to skip it.
    ///
    /// Why: a constant fingerprint is indistinguishable from "unchanged", so an
    /// item that cannot report a revision would be skipped FOREVER after its
    /// first ingest — silently serving stale content, the exact opposite of the
    /// intended fail-open. Marking it volatile makes the engine re-write it
    /// every run; the write itself is still byte-compare-guarded, so an
    /// unchanged item costs no disk churn.
    pub volatile: bool,
}

/// What one ingest run did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IngestReport {
    /// Canonical source id.
    pub source_id: String,
    /// `docstore` / `gmail` / `drive`.
    pub kind: String,
    /// Collection entities were written into.
    pub collection: String,
    /// Items presented to the engine.
    pub scanned: usize,
    /// Items never seen before — genuinely new entities.
    pub ingested: usize,
    /// Items seen before whose fingerprint changed — entities replaced.
    pub updated: usize,
    /// Items already current — untouched.
    pub skipped: usize,
    /// Entities marked because their source item vanished.
    pub tombstoned: usize,
    /// Previously-ingested item ids absent from this run that were NOT
    /// tombstoned (because the source does not enumerate its full corpus, or
    /// tombstoning is off). Surfaced, never silently dropped.
    pub missing: Vec<String>,
    /// Entities written this run (capped — see `entities_truncated`).
    pub entities: Vec<String>,
    /// True when `entities` was capped at [`MAX_REPORTED_ENTITIES`].
    pub entities_truncated: bool,
    /// Per-item failures. An item that fails does not abort the run.
    pub errors: Vec<String>,
    /// Coverage after this run.
    pub watermark: Watermark,
}

impl IngestReport {
    /// Fold another chunk's report into this one.
    ///
    /// Why: a large network source is ingested page by page so partial progress
    /// commits, but the caller still owes the agent ONE coherent summary.
    /// What: sums the counters, concatenates errors/missing, and appends entity
    /// paths up to the listing cap. `watermark` is taken from `other` because
    /// later chunks observe strictly more of the journal.
    /// Test: `merge_folds_chunk_reports`.
    pub fn merge(&mut self, other: IngestReport) {
        self.scanned += other.scanned;
        self.ingested += other.ingested;
        self.updated += other.updated;
        self.skipped += other.skipped;
        self.tombstoned += other.tombstoned;
        self.missing.extend(other.missing);
        self.errors.extend(other.errors);
        for entity in other.entities {
            push_entity(self, entity);
        }
        self.entities_truncated |= other.entities_truncated;
        self.watermark = other.watermark;
    }
}

impl KbStore {
    /// Ingest already-fetched items for one source, gated by its ledger.
    ///
    /// Why/What: see the module doc. `detect_deletions` must be true ONLY when
    /// `items` enumerates the source's entire corpus — a doc-store walk does; a
    /// windowed Gmail pull emphatically does not, and passing true there would
    /// tombstone every message outside the window.
    /// Test: `rerun_ingests_nothing_new`, `deletion_tombstones_when_enabled`.
    pub fn ingest_items(
        &self,
        spec: &SourceSpec,
        items: &[SourceItem],
        detect_deletions: bool,
        now: &str,
    ) -> anyhow::Result<IngestReport> {
        let mut ledger = Ledger::load(&self.root, &spec.id)?;
        let mut report = IngestReport {
            source_id: spec.id.clone(),
            kind: spec.kind().to_string(),
            collection: spec.collection.clone(),
            scanned: items.len(),
            ..IngestReport::default()
        };

        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for item in items {
            seen.insert(item.item_id.as_str());
            // A volatile item's fingerprint carries no information, so the skip
            // test must not be consulted for it at all — see `SourceItem::volatile`.
            if !item.volatile && ledger.is_current(&item.item_id, &item.fingerprint) {
                report.skipped += 1;
                continue;
            }
            let first_time = !ledger.contains(&item.item_id);
            match self.write_item(spec, item, now) {
                Ok(entity) => {
                    // Ledger append happens AFTER the entity write and BEFORE
                    // the next item, so a crash can only ever leave an entity
                    // whose ledger line is missing — which the next run simply
                    // rewrites identically. The reverse order would lose data.
                    ledger.append(LedgerRecord {
                        item_id: item.item_id.clone(),
                        fingerprint: item.fingerprint.clone(),
                        entity: entity.clone(),
                        ingested_at: now.to_string(),
                        timestamp: item.timestamp.clone(),
                        status: ItemStatus::Ingested,
                    })?;
                    if first_time {
                        report.ingested += 1;
                    } else {
                        report.updated += 1;
                    }
                    push_entity(&mut report, entity);
                }
                Err(e) => report.errors.push(format!("{}: {e}", item.item_id)),
            }
        }

        if detect_deletions {
            let present: BTreeSet<String> = seen.iter().map(|s| s.to_string()).collect();
            self.sweep_into(spec, &mut ledger, &present, now, &mut report)?;
        } else {
            report.missing = ledger
                .live_item_ids()
                .into_iter()
                .filter(|id| !seen.contains(id.as_str()))
                .collect();
        }

        report.watermark = ledger.watermark();
        Ok(report)
    }

    /// Sweep for vanished items after a CHUNKED ingest.
    ///
    /// Why: `ingest_items` can only judge deletions from the slice it was given,
    /// which is wrong once a caller streams a large source page by page — each
    /// page would look like "everything else was deleted". A network source
    /// therefore ingests in chunks with detection off, then calls this ONCE with
    /// the complete set of ids it actually observed.
    /// What: tombstones (or merely reports) every live ledger item absent from
    /// `present_ids`, and returns the counts folded into a report.
    /// Test: `sweep_tombstones_only_after_the_full_set_is_known`.
    pub fn okg_sweep_deleted(
        &self,
        spec: &SourceSpec,
        present_ids: &BTreeSet<String>,
        now: &str,
    ) -> anyhow::Result<IngestReport> {
        let mut ledger = Ledger::load(&self.root, &spec.id)?;
        let mut report = IngestReport {
            source_id: spec.id.clone(),
            kind: spec.kind().to_string(),
            collection: spec.collection.clone(),
            ..IngestReport::default()
        };
        self.sweep_into(spec, &mut ledger, present_ids, now, &mut report)?;
        report.watermark = ledger.watermark();
        Ok(report)
    }

    /// Shared body of the deletion sweep, used by both entry points above.
    fn sweep_into(
        &self,
        spec: &SourceSpec,
        ledger: &mut Ledger,
        present_ids: &BTreeSet<String>,
        now: &str,
        report: &mut IngestReport,
    ) -> anyhow::Result<()> {
        let vanished: Vec<String> = ledger
            .live_item_ids()
            .into_iter()
            .filter(|id| !present_ids.contains(id))
            .collect();
        if !spec.tombstone_deleted {
            report.missing.extend(vanished);
            return Ok(());
        }
        for item_id in vanished {
            match self.tombstone_item(spec, ledger, &item_id, now) {
                Ok(Some(record)) => {
                    ledger.append(record)?;
                    report.tombstoned += 1;
                }
                // The ledger names an entity that is no longer on disk —
                // nothing to mark, but the caller should still see it.
                Ok(None) => report.missing.push(item_id),
                Err(e) => report.errors.push(format!("{item_id}: {e}")),
            }
        }
        Ok(())
    }

    /// Write one item as an entity, returning its `<collection>/<slug>`.
    ///
    /// Overwrite (not merge) is deliberate: a re-ingested item must REPLACE its
    /// prior entry, so stale provenance fields from an earlier version cannot
    /// linger. `put_entity` still preserves `created` and only touches the file
    /// when the rendered bytes differ.
    fn write_item(
        &self,
        spec: &SourceSpec,
        item: &SourceItem,
        now: &str,
    ) -> anyhow::Result<String> {
        let mut fm = serde_yaml::Mapping::new();
        fm.insert(yaml("title"), yaml(&item.title));
        fm.insert(yaml("source_id"), yaml(&spec.id));
        fm.insert(yaml("source_kind"), yaml(spec.kind()));
        fm.insert(yaml("source_item_id"), yaml(&item.item_id));
        if let Some(ts) = item.timestamp.as_deref() {
            fm.insert(yaml("source_timestamp"), yaml(ts));
        }
        for (k, v) in &item.fields {
            // Provenance keys are namespaced by the fetcher; never let one
            // shadow the envelope fields written above.
            if !fm.contains_key(yaml(k)) {
                fm.insert(yaml(k), yaml(v));
            }
        }

        let outcome = self.put_entity(
            &spec.collection,
            &item.name,
            Yaml::Mapping(fm),
            Some(&item.body),
            false,
            now,
        )?;
        Ok(format!("{}/{}", spec.collection, outcome.slug))
    }

    /// Mark a vanished item's entity, returning the ledger line to append.
    ///
    /// Returns `Ok(None)` when the entity is already gone from disk — there is
    /// nothing to mark, and inventing a record would misreport the tree.
    fn tombstone_item(
        &self,
        spec: &SourceSpec,
        ledger: &Ledger,
        item_id: &str,
        now: &str,
    ) -> anyhow::Result<Option<LedgerRecord>> {
        let Some(prior) = ledger.get(item_id) else {
            return Ok(None);
        };
        let slug = prior
            .entity
            .rsplit('/')
            .next()
            .unwrap_or(&prior.entity)
            .to_string();
        let path = self.entity_path(&spec.collection, &slug)?;
        if self.read_entity_at(&path)?.is_none() {
            return Ok(None);
        }

        let mut fm = serde_yaml::Mapping::new();
        fm.insert(yaml("source_status"), yaml("deleted"));
        fm.insert(yaml("tombstoned"), yaml(now));
        // Merge, not overwrite: the body and every distilled fact survive. A
        // tombstone is a flag, never a deletion.
        self.put_entity(&spec.collection, &slug, Yaml::Mapping(fm), None, true, now)?;

        Ok(Some(LedgerRecord {
            item_id: item_id.to_string(),
            fingerprint: prior.fingerprint.clone(),
            entity: prior.entity.clone(),
            ingested_at: now.to_string(),
            timestamp: prior.timestamp.clone(),
            status: ItemStatus::Tombstoned,
        }))
    }
}

/// Record an entity path in the report, respecting the listing cap.
fn push_entity(report: &mut IngestReport, entity: String) {
    if report.entities.len() < MAX_REPORTED_ENTITIES {
        report.entities.push(entity);
    } else {
        report.entities_truncated = true;
    }
}

fn yaml(s: &str) -> Yaml {
    Yaml::String(s.to_string())
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod tests;
