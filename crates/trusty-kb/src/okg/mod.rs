//! OKG builder — grow one KB tree from N idempotent, additive sources.
//!
//! Why: a KB tree is only a knowledge graph once something FILLS it, repeatedly,
//! without duplicating what it already holds. Bob's requirement is three
//! properties at once: idempotent (re-running an ingest changes nothing),
//! additive (registering a new doc store, or reaching further back in time,
//! appends and never rebuilds), and crash-convergent (a killed run re-runs to
//! the same state). This module is the machinery for all three; the entity
//! writes themselves reuse [`KbStore::put_entity`] rather than inventing a
//! second KB format.
//!
//! What:
//!   - [`registry`] — `_sources/registry.toml`, the human-readable source list.
//!   - [`ledger`]   — `_sources/<id>.jsonl`, the per-item append-only journal.
//!   - [`ingest`]   — the fetcher-agnostic engine ([`ingest::SourceItem`] in,
//!     entities + ledger lines out).
//!   - [`docstore`] — the in-crate filesystem fetcher.
//!   - [`index_journal`] — `_sources/<id>.index.jsonl`, the record of what
//!     reached the bound SEARCH index, and the reconcile that diffs it against
//!     the ledger (#3892).
//!   - [`trust`]    — the per-entity trust label the engine stamps at ingest
//!     and the retrieval fence reads back (#4532, DOC-63 §6.3).
//!
//! Gmail and Drive fetchers deliberately live in `trusty-agents`, where the
//! authenticated `trusty-gworkspace` client already is — this crate stays pure,
//! deterministic, and network-free. For the same reason the index PUSH lives
//! there too (`trusty_agents::stores::index_feed`); this crate only decides
//! what is owed.
//!
//! This file adds the store-level entry points that compose those pieces:
//! [`KbStore::okg_register_source`], [`KbStore::okg_sources`], and
//! [`KbStore::okg_ingest_docstore`].
//!
//! Test: `register_is_additive_across_sources`,
//! `docstore_ingest_is_idempotent_end_to_end`.

pub mod docstore;
pub mod index_journal;
pub mod ingest;
mod jsonl;
pub mod ledger;
pub mod policy;
pub mod registry;
pub mod trust;

use serde::Serialize;
use serde_json::Value as Json;

use crate::okg::ingest::IngestReport;
use crate::okg::ledger::{Ledger, Watermark};
use crate::okg::registry::{Locator, RegisterOutcome, SourceRegistry, SourceSpec};
use crate::store::KbStore;

/// One row of the `okg_sources` status view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceStatus {
    /// Canonical source id.
    pub id: String,
    /// `docstore` / `gmail` / `drive`.
    pub kind: String,
    /// Collection this source writes into.
    pub collection: String,
    /// Whether ingestion runs for it.
    pub enabled: bool,
    /// Whether vanished items are tombstoned.
    pub tombstone_deleted: bool,
    /// First registration timestamp.
    pub added_at: String,
    /// The locator, as JSON, so a caller can see exactly what is covered.
    pub locator: Json,
    /// Derived coverage: item counts, time span, last run.
    pub watermark: Watermark,
    /// How much of this source has reached the bound SEARCH INDEX (#3892).
    pub index: IndexCoverage,
}

/// Per-source search-index coverage — the "is it actually findable?" half.
///
/// Why (#3892): `watermark` answers "what is in the tree", and for a long time
/// that was silently read as "what the assistant can find". It is not: an
/// entity is only searchable once it has been pushed into the store's bound
/// trusty-search index, and that push can lag (daemon down, push failed, no
/// binding at all). This field makes the lag VISIBLE — an "ingested but not yet
/// searchable" backlog is the exact failure mode #3892 reports, and it must
/// never again be invisible on both sides.
/// What: `synced` entities the index journal says are current, and `pending`
/// still owed — pushes, withdrawals, AND rows that could not be resolved to a
/// readable entity file (unsearchable too, and additionally listed in `notes`).
/// Derived, like the watermark, so it cannot claim more than was recorded.
/// Test: `sources_report_index_coverage`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IndexCoverage {
    /// Entities recorded as current in the bound index.
    pub synced: usize,
    /// Entities still owed to the index (not yet searchable, or not yet
    /// withdrawn after a tombstone).
    pub pending: usize,
    /// Rows the reconcile could not resolve to a readable entity file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl KbStore {
    /// Register a source, or update an existing one in place.
    ///
    /// Why: this is the additive entry point. A new id appends a row and starts
    /// an empty ledger; a known id (e.g. the same Gmail source with an earlier
    /// `after:`) updates only its locator, so the ledger — and therefore
    /// everything already ingested — is preserved and the next run pulls only
    /// what is genuinely new.
    /// What: loads the registry, upserts, and write-if-changed saves it.
    /// Test: `register_is_additive_across_sources`.
    pub fn okg_register_source(&self, spec: SourceSpec) -> anyhow::Result<RegisterOutcome> {
        let mut reg = SourceRegistry::load(&self.root)?;
        let id = spec.id.clone();
        let (created, changed) = reg.upsert(spec);
        let wrote = reg.save(&self.root)?;
        Ok(RegisterOutcome {
            id,
            created,
            changed: changed || wrote,
            path: self.rel(&SourceRegistry::path(&self.root)?),
        })
    }

    /// Fetch a registered source's spec, erroring when it is unknown.
    pub fn okg_source(&self, source_id: &str) -> anyhow::Result<SourceSpec> {
        SourceRegistry::load(&self.root)?
            .get(source_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no source registered with id {source_id:?}"))
    }

    /// The status view: every registered source with its live coverage.
    ///
    /// Why: "what does this store cover, and how far back?" is the question an
    /// operator asks before widening a window or adding a store. Coverage is
    /// derived from the ledgers, so it cannot drift from what was ingested. It
    /// additionally reports SEARCH coverage (#3892) — see [`IndexCoverage`] —
    /// because "ingested" and "findable" are two different claims and conflating
    /// them is what made the ingest→search gap silent.
    /// What: one row per registered source. The index reconcile is local
    /// filesystem work (one `stat` per settled entity, a re-hash only when the
    /// cheap check moves), never a daemon call, so this stays a read-only,
    /// offline-safe status view.
    /// Test: `register_is_additive_across_sources`,
    /// `sources_report_index_coverage`.
    pub fn okg_sources(&self) -> anyhow::Result<Vec<SourceStatus>> {
        let reg = SourceRegistry::load(&self.root)?;
        let mut out = Vec::with_capacity(reg.sources.len());
        for spec in &reg.sources {
            let watermark = Ledger::load(&self.root, &spec.id)?.watermark();
            let backlog = self.okg_index_backlog(spec)?;
            out.push(SourceStatus {
                index: IndexCoverage {
                    synced: backlog.synced,
                    pending: backlog.tasks.len() + backlog.notes.len(),
                    notes: backlog.notes,
                },
                id: spec.id.clone(),
                kind: spec.kind().to_string(),
                collection: spec.collection.clone(),
                enabled: spec.enabled,
                tombstone_deleted: spec.tombstone_deleted,
                added_at: spec.added_at.clone(),
                locator: serde_json::to_value(&spec.locator)?,
                watermark,
            });
        }
        Ok(out)
    }

    /// Tree-wide search coverage, folded across every registered source.
    ///
    /// Why (#3892): the store card and the `[[stores]]` status endpoint answer
    /// "is this store connected?" per INDEX; this answers the other half — how
    /// much of the tree has actually reached that index. A store can be
    /// perfectly connected and still hold nothing the assistant ingested.
    /// What: sums [`IndexCoverage`] over the registry. Returns zeroes for a tree
    /// with no registry at all, which is the normal state for a hand-built tree.
    /// Test: `sources_report_index_coverage`.
    pub fn okg_index_coverage(&self) -> anyhow::Result<IndexCoverage> {
        let mut total = IndexCoverage::default();
        for spec in &SourceRegistry::load(&self.root)?.sources {
            let backlog = self.okg_index_backlog(spec)?;
            total.synced += backlog.synced;
            total.pending += backlog.tasks.len() + backlog.notes.len();
            total.notes.extend(backlog.notes);
        }
        Ok(total)
    }

    /// Scan a registered doc store and ingest whatever changed.
    ///
    /// Why: the whole doc-store path is local and deterministic, so it runs
    /// end-to-end inside this crate — the agent tool is a thin wrapper.
    /// What: resolves the source, walks its directory (subject to `policy`, the
    /// read-side confinement gate), and hands the items to the ingest engine
    /// with deletion detection enabled (a full walk DOES enumerate the corpus,
    /// so an absent file is genuinely absent).
    /// Test: `docstore_ingest_is_idempotent_end_to_end`,
    /// `ingest_rechecks_the_policy_on_every_run`.
    pub fn okg_ingest_docstore(
        &self,
        source_id: &str,
        policy: &policy::DocStorePolicy,
        now: &str,
    ) -> anyhow::Result<IngestReport> {
        let spec = self.okg_source(source_id)?;
        let Locator::DocStore {
            path,
            extensions,
            recursive,
        } = &spec.locator
        else {
            anyhow::bail!(
                "source {} is a {} source, not a doc store",
                spec.id,
                spec.kind()
            );
        };
        if !spec.enabled {
            anyhow::bail!("source {} is disabled", spec.id);
        }

        let scan = docstore::scan(
            std::path::Path::new(path),
            extensions,
            *recursive,
            docstore::DEFAULT_CHUNK_CHARS,
            policy,
        )?;
        let mut report = self.ingest_items(&spec, &scan.items, true, now)?;
        report.errors.extend(scan.errors);
        for binary in scan.skipped_binary {
            report
                .errors
                .push(format!("{binary}: skipped (binary content)"));
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okg::policy::DocStorePolicy;
    use crate::schema::Profile;

    /// A policy permitting the whole fixture tempdir.
    fn policy(tmp: &tempfile::TempDir) -> DocStorePolicy {
        DocStorePolicy::new(vec![tmp.path().canonicalize().unwrap()])
    }

    fn store() -> (tempfile::TempDir, KbStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = KbStore::new(tmp.path().join("kb"), Profile::default_profile());
        (tmp, store)
    }

    /// Why: code-critic CRITICAL 2 — the read-side gate must live in the ENGINE,
    /// not only at the tool boundary. A `registry.toml` row can be hand-edited
    /// (or was written before the policy existed) to point at `~/.ssh`, and the
    /// row is re-read on every run; validating only at registration time would
    /// let that poisoned row read credentials forever.
    /// What: registers a doc store while the policy permits it, then re-runs with
    /// a policy that does NOT, and asserts the ingest is refused.
    /// Test: self-contained.
    #[test]
    fn ingest_rechecks_the_policy_on_every_run() {
        let (tmp, store) = store();
        let corpus = tmp.path().join("corpus");
        std::fs::create_dir_all(&corpus).unwrap();
        std::fs::write(corpus.join("a.md"), "alpha").unwrap();

        store
            .okg_register_source(SourceSpec::new(
                "docs",
                Some("notes"),
                Locator::DocStore {
                    path: corpus.to_string_lossy().to_string(),
                    extensions: vec![],
                    recursive: true,
                },
                "t0",
            ))
            .unwrap();
        assert_eq!(
            store
                .okg_ingest_docstore("docs", &policy(&tmp), "t0")
                .unwrap()
                .ingested,
            1
        );

        // The same registered row, now outside the permitted roots.
        let narrowed = DocStorePolicy::new(vec![tmp.path().join("somewhere-else")]);
        let err = store
            .okg_ingest_docstore("docs", &narrowed, "t1")
            .expect_err("a registered row must not bypass the policy");
        assert!(
            err.to_string().contains("outside every configured"),
            "unexpected error: {err}"
        );

        // And an empty policy denies it too — the gate fails closed.
        let err = store
            .okg_ingest_docstore("docs", &DocStorePolicy::default(), "t2")
            .expect_err("unconfigured policy must deny");
        assert!(err.to_string().contains("no doc-store roots"), "{err}");
    }

    /// Why: Bob's core requirement — "I can add a new doc store" — must not
    /// disturb what other sources already built.
    /// What: registers a doc store, ingests it, then registers a SECOND source
    /// and asserts the first source's ledger, entities, and status row are all
    /// untouched while the new row simply appends.
    /// Test: self-contained.
    #[test]
    fn register_is_additive_across_sources() {
        let (tmp, store) = store();
        let docs = tmp.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("a.md"), "alpha").unwrap();

        let first = store
            .okg_register_source(SourceSpec::new(
                "docs",
                Some("notes"),
                Locator::DocStore {
                    path: docs.to_string_lossy().to_string(),
                    extensions: vec![],
                    recursive: true,
                },
                "t0",
            ))
            .unwrap();
        assert!(first.created && first.changed);
        store
            .okg_ingest_docstore("docs", &policy(&tmp), "t0")
            .unwrap();

        let before = store.okg_sources().unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].watermark.items, 1);

        // Add an unrelated source.
        let second = store
            .okg_register_source(SourceSpec::new(
                "mail",
                None,
                Locator::Gmail {
                    query: "in:sent".into(),
                    after: Some("2026/01/01".into()),
                    before: None,
                },
                "t1",
            ))
            .unwrap();
        assert!(second.created);

        let after = store.okg_sources().unwrap();
        assert_eq!(after.len(), 2, "append, not replace");
        assert_eq!(after[0], before[0], "the existing source row is untouched");
        assert_eq!(after[1].kind, "gmail");
        assert_eq!(after[1].watermark.items, 0, "new source starts empty");

        // Re-registering the identical doc store is a no-op.
        let again = store
            .okg_register_source(SourceSpec::new(
                "docs",
                Some("notes"),
                Locator::DocStore {
                    path: docs.to_string_lossy().to_string(),
                    extensions: vec![],
                    recursive: true,
                },
                "t2",
            ))
            .unwrap();
        assert!(
            !again.created && !again.changed,
            "identical re-register is inert"
        );
        assert_eq!(store.okg_sources().unwrap()[0], before[0]);
    }

    /// Why: the end-to-end doc-store path must satisfy every property at once —
    /// first run ingests, second run is inert, an edit re-ingests exactly one
    /// entity, and a deletion is tombstoned rather than dropped.
    /// What: drives all four phases against a real directory.
    /// Test: self-contained.
    #[test]
    fn docstore_ingest_is_idempotent_end_to_end() {
        let (tmp, store) = store();
        let docs = tmp.path().join("corpus");
        std::fs::create_dir_all(docs.join("sub")).unwrap();
        std::fs::write(docs.join("one.md"), "first note").unwrap();
        std::fs::write(docs.join("sub/two.txt"), "second note").unwrap();
        std::fs::write(docs.join("photo.bin"), b"\x00\x01binary").unwrap();

        let mut spec = SourceSpec::new(
            "corpus",
            Some("notes"),
            Locator::DocStore {
                path: docs.to_string_lossy().to_string(),
                extensions: vec![],
                recursive: true,
            },
            "t0",
        );
        spec.tombstone_deleted = true;
        store.okg_register_source(spec).unwrap();

        let first = store
            .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
            .unwrap();
        assert_eq!((first.ingested, first.updated, first.skipped), (2, 0, 0));
        assert_eq!(first.scanned, 2, ".bin filtered by extension");
        let one = store.entity_path("notes", "one").unwrap();
        assert!(
            std::fs::read_to_string(&one)
                .unwrap()
                .contains("first note")
        );

        // Re-run: nothing changes.
        let second = store
            .okg_ingest_docstore("corpus", &policy(&tmp), "t1")
            .unwrap();
        assert_eq!(
            (
                second.ingested,
                second.updated,
                second.skipped,
                second.tombstoned
            ),
            (0, 0, 2, 0),
            "an unchanged corpus must produce zero writes"
        );

        // Edit one file: exactly one entity re-ingests.
        std::fs::write(docs.join("one.md"), "first note, revised").unwrap();
        let third = store
            .okg_ingest_docstore("corpus", &policy(&tmp), "t2")
            .unwrap();
        assert_eq!((third.ingested, third.updated, third.skipped), (0, 1, 1));
        assert!(
            std::fs::read_to_string(&one).unwrap().contains("revised"),
            "edited content replaces the entity"
        );

        // Delete one file: tombstoned, content preserved.
        std::fs::remove_file(docs.join("sub/two.txt")).unwrap();
        let fourth = store
            .okg_ingest_docstore("corpus", &policy(&tmp), "t3")
            .unwrap();
        assert_eq!(fourth.tombstoned, 1);
        let two = std::fs::read_to_string(store.entity_path("notes", "sub/two").unwrap()).unwrap();
        assert!(two.contains("source_status: deleted"), "flagged: {two}");
        assert!(two.contains("second note"), "never silently dropped: {two}");

        let status = store.okg_sources().unwrap();
        assert_eq!(status[0].watermark.items, 1);
        assert_eq!(status[0].watermark.tombstoned, 1);
    }

    /// Why (#3892): "N entities ingested" was truthful and still meant "findable
    /// by nobody". The status view must therefore separate tree coverage from
    /// SEARCH coverage, so an operator can see a backlog instead of guessing.
    /// What: ingests two files with no index feed at all and asserts both the
    /// per-source row and the tree-wide fold report them as pending, then
    /// asserts recording the pushes moves them to synced.
    /// Test: self-contained.
    #[test]
    fn sources_report_index_coverage() {
        let (tmp, store) = store();
        let docs = tmp.path().join("corpus");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("one.md"), "first note").unwrap();
        std::fs::write(docs.join("two.md"), "second note").unwrap();
        store
            .okg_register_source(SourceSpec::new(
                "corpus",
                Some("notes"),
                Locator::DocStore {
                    path: docs.to_string_lossy().to_string(),
                    extensions: vec![],
                    recursive: true,
                },
                "t0",
            ))
            .unwrap();
        store
            .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
            .unwrap();

        let status = store.okg_sources().unwrap();
        assert_eq!(status[0].watermark.items, 2, "both are in the TREE");
        assert_eq!(
            (status[0].index.synced, status[0].index.pending),
            (0, 2),
            "and neither is searchable yet — the #3892 state, now visible"
        );
        assert_eq!(store.okg_index_coverage().unwrap().pending, 2);

        // Record the pushes the feed layer would have made.
        let spec = store.okg_source("corpus").unwrap();
        let backlog = store.okg_index_backlog(&spec).unwrap();
        let mut journal =
            crate::okg::index_journal::IndexJournal::load(&store.root, "corpus").unwrap();
        for task in &backlog.tasks {
            store.okg_record_index(&mut journal, task, "t0").unwrap();
        }

        let settled = store.okg_sources().unwrap();
        assert_eq!((settled[0].index.synced, settled[0].index.pending), (2, 0));
        assert_eq!(store.okg_index_coverage().unwrap().synced, 2);
    }
}
