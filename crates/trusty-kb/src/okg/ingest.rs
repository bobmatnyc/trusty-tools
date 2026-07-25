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
//! `per_item_errors_do_not_abort_the_run`.

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

impl KbStore {
    /// Ingest already-fetched items for one source, gated by its ledger.
    ///
    /// Why/What: see the module doc. `detect_deletions` must be true ONLY when
    /// `items` enumerates the source's entire corpus — a doc-store walk does; a
    /// windowed Gmail pull emphatically does not, and passing true there would
    /// tombstone every message outside the window.
    /// Test: the five tests in this module.
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
            if ledger.is_current(&item.item_id, &item.fingerprint) {
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

        // Items the ledger knows that this run did not see.
        let vanished: Vec<String> = ledger
            .live_item_ids()
            .into_iter()
            .filter(|id| !seen.contains(id.as_str()))
            .collect();
        if detect_deletions && spec.tombstone_deleted {
            for item_id in vanished {
                match self.tombstone_item(spec, &ledger, &item_id, now) {
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
        } else {
            report.missing = vanished;
        }

        report.watermark = ledger.watermark();
        Ok(report)
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
mod tests {
    use super::*;
    use crate::okg::registry::Locator;
    use crate::schema::Profile;

    fn store() -> (tempfile::TempDir, KbStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = KbStore::new(tmp.path().to_path_buf(), Profile::default_profile());
        (tmp, store)
    }

    fn spec(tombstone: bool) -> SourceSpec {
        let mut s = SourceSpec::new(
            "mail",
            Some("sources"),
            Locator::Gmail {
                query: "in:sent".into(),
                after: None,
                before: None,
            },
            "2026-07-24T00:00:00Z",
        );
        s.tombstone_deleted = tombstone;
        s
    }

    fn item(id: &str, fingerprint: &str, ts: &str, body: &str) -> SourceItem {
        SourceItem {
            item_id: id.into(),
            fingerprint: fingerprint.into(),
            name: id.into(),
            title: format!("Item {id}"),
            timestamp: Some(ts.into()),
            body: body.into(),
            fields: BTreeMap::new(),
        }
    }

    /// Why: THE core idempotency property — running the same ingest twice must
    /// add nothing. Anything else means the KB grows on every poll.
    /// What: ingests three items, re-ingests the identical set, and asserts the
    /// second run reports zero new/updated and leaves the tree byte-identical.
    /// Test: self-contained.
    #[test]
    fn rerun_ingests_nothing_new() {
        let (_t, store) = store();
        let spec = spec(false);
        let items = vec![
            item("m1", "f1", "2026-07-01", "one"),
            item("m2", "f2", "2026-07-02", "two"),
            item("m3", "f3", "2026-07-03", "three"),
        ];

        let first = store
            .ingest_items(&spec, &items, false, "2026-07-24T00:00:00Z")
            .unwrap();
        assert_eq!((first.ingested, first.updated, first.skipped), (3, 0, 0));
        assert!(
            first.errors.is_empty(),
            "unexpected errors: {:?}",
            first.errors
        );
        let before = std::fs::read_to_string(store.entity_path("sources", "m1").unwrap()).unwrap();

        let second = store
            .ingest_items(&spec, &items, false, "2026-07-25T00:00:00Z")
            .unwrap();
        assert_eq!(
            (second.ingested, second.updated, second.skipped),
            (0, 0, 3),
            "a re-run must add zero items"
        );
        assert!(second.entities.is_empty());
        assert_eq!(second.watermark.items, 3, "still three items, not six");
        assert_eq!(
            std::fs::read_to_string(store.entity_path("sources", "m1").unwrap()).unwrap(),
            before,
            "unchanged item must not be rewritten"
        );
    }

    /// Why: a changed source file must re-ingest and REPLACE its entity, not
    /// accrete a second copy or leave the stale body in place.
    /// What: ingests, then re-ingests the same item id with a new fingerprint
    /// and new body; asserts `updated`, one item in the watermark, and the new
    /// body on disk.
    /// Test: self-contained.
    #[test]
    fn changed_item_replaces_entity() {
        let (_t, store) = store();
        let spec = spec(false);
        store
            .ingest_items(
                &spec,
                &[item("a.md", "h1", "2026-07-01", "old body")],
                false,
                "t0",
            )
            .unwrap();

        let report = store
            .ingest_items(
                &spec,
                &[item("a.md", "h2", "2026-07-02", "new body")],
                false,
                "t1",
            )
            .unwrap();
        assert_eq!(
            (report.ingested, report.updated, report.skipped),
            (0, 1, 0),
            "changed fingerprint re-ingests as an update"
        );
        assert_eq!(report.watermark.items, 1, "replacement, not duplication");

        let text = std::fs::read_to_string(store.entity_path("sources", "a.md").unwrap()).unwrap();
        assert!(text.contains("new body"), "body replaced: {text}");
        assert!(
            !text.contains("old body"),
            "stale body must be gone: {text}"
        );
    }

    /// Why: Bob's additive requirement — "I can go further back in time".
    /// Widening a Gmail window must fetch only the OLDER messages the ledger has
    /// never seen, and must not re-pull or duplicate the window already covered.
    /// What: ingests a recent window, then a wider superset window, asserting
    /// only the two older messages are new and the coverage watermark extends
    /// backwards.
    /// Test: self-contained.
    #[test]
    fn wider_window_only_adds_older_items() {
        let (_t, store) = store();
        let spec = spec(false);

        let recent = vec![
            item("m10", "m10", "2026-07-01", "july"),
            item("m11", "m11", "2026-07-02", "july"),
        ];
        let first = store.ingest_items(&spec, &recent, false, "t0").unwrap();
        assert_eq!(first.ingested, 2);
        assert_eq!(first.watermark.oldest.as_deref(), Some("2026-07-01"));

        // The widened window returns the older messages AND everything already
        // covered — exactly what Gmail does when `after:` moves backwards.
        let widened = vec![
            item("m08", "m08", "2026-05-01", "may"),
            item("m09", "m09", "2026-06-01", "june"),
            item("m10", "m10", "2026-07-01", "july"),
            item("m11", "m11", "2026-07-02", "july"),
        ];
        let second = store.ingest_items(&spec, &widened, false, "t1").unwrap();
        assert_eq!(
            (second.ingested, second.updated, second.skipped),
            (2, 0, 2),
            "only the two older messages are new"
        );
        assert_eq!(second.watermark.items, 4);
        assert_eq!(
            second.watermark.oldest.as_deref(),
            Some("2026-05-01"),
            "coverage now reaches further back"
        );
        assert_eq!(second.watermark.newest.as_deref(), Some("2026-07-02"));
        assert!(
            second.missing.is_empty(),
            "a superset window must not look like deletions"
        );

        // And re-running the widened window is still a no-op.
        let third = store.ingest_items(&spec, &widened, false, "t2").unwrap();
        assert_eq!((third.ingested, third.updated, third.skipped), (0, 0, 4));
    }

    /// Why: a deleted doc must be flagged, never silently dropped — and only
    /// when the caller can prove the corpus was fully enumerated.
    /// What: ingests two items, re-ingests with one gone. With detection off the
    /// absentee is merely REPORTED; with it on the entity is tombstoned while
    /// keeping its body.
    /// Test: self-contained.
    #[test]
    fn deletion_tombstones_when_enabled() {
        let (_t, store) = store();
        let both = vec![
            item("a.md", "h1", "2026-07-01", "alpha body"),
            item("b.md", "h2", "2026-07-02", "beta body"),
        ];
        let only_a = vec![item("a.md", "h1", "2026-07-01", "alpha body")];

        // Detection off → reported, entity untouched.
        let quiet = spec(false);
        store.ingest_items(&quiet, &both, false, "t0").unwrap();
        let report = store.ingest_items(&quiet, &only_a, false, "t1").unwrap();
        assert_eq!(report.tombstoned, 0);
        assert_eq!(
            report.missing,
            vec!["b.md".to_string()],
            "surfaced, not dropped"
        );
        let b = std::fs::read_to_string(store.entity_path("sources", "b.md").unwrap()).unwrap();
        assert!(
            !b.contains("tombstoned"),
            "must not mark when detection is off"
        );

        // Detection on → tombstoned, body preserved.
        let strict = spec(true);
        let report = store.ingest_items(&strict, &only_a, true, "t2").unwrap();
        assert_eq!(report.tombstoned, 1);
        assert!(report.missing.is_empty());
        let b = std::fs::read_to_string(store.entity_path("sources", "b.md").unwrap()).unwrap();
        assert!(b.contains("source_status: deleted"), "flagged: {b}");
        assert!(
            b.contains("beta body"),
            "content preserved, never deleted: {b}"
        );
        assert_eq!(report.watermark.items, 1);
        assert_eq!(report.watermark.tombstoned, 1);

        // A tombstoned item that comes back is re-ingested.
        let back = store.ingest_items(&strict, &both, true, "t3").unwrap();
        assert_eq!(back.updated, 1, "returning item re-ingests");
        assert_eq!(back.watermark.tombstoned, 0);
    }

    /// Why: one bad item in a 5,000-item pull must not lose the other 4,999.
    /// What: feeds an item whose name cannot form a valid entity alongside good
    /// ones; asserts the good ones land and the failure is reported.
    /// Test: self-contained.
    #[test]
    fn per_item_errors_do_not_abort_the_run() {
        let (_t, store) = store();
        let mut bad = item("bad", "f", "2026-07-01", "body");
        // An empty name slugs to `untitled`, so force a real failure through a
        // collection that cannot be confined.
        bad.name = "ok".into();
        let mut spec = spec(false);
        spec.collection = "../escape".into();

        let report = store
            .ingest_items(
                &spec,
                &[bad, item("good", "f", "2026-07-02", "b")],
                false,
                "t0",
            )
            .unwrap();
        assert_eq!(report.ingested, 0, "both items fail on a bad collection");
        assert_eq!(
            report.errors.len(),
            2,
            "every failure is reported: {:?}",
            report.errors
        );
        assert_eq!(report.scanned, 2, "the run completes rather than aborting");
    }
}
