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
//!
//! Gmail and Drive fetchers deliberately live in `trusty-agents`, where the
//! authenticated `trusty-gworkspace` client already is — this crate stays pure,
//! deterministic, and network-free.
//!
//! This file adds the store-level entry points that compose those pieces:
//! [`KbStore::okg_register_source`], [`KbStore::okg_sources`], and
//! [`KbStore::okg_ingest_docstore`].
//!
//! Test: `register_is_additive_across_sources`,
//! `docstore_ingest_is_idempotent_end_to_end`.

pub mod docstore;
pub mod ingest;
pub mod ledger;
pub mod policy;
pub mod registry;

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
    /// derived from the ledgers, so it cannot drift from what was ingested.
    /// Test: `register_is_additive_across_sources`.
    pub fn okg_sources(&self) -> anyhow::Result<Vec<SourceStatus>> {
        let reg = SourceRegistry::load(&self.root)?;
        let mut out = Vec::with_capacity(reg.sources.len());
        for spec in &reg.sources {
            let watermark = Ledger::load(&self.root, &spec.id)?.watermark();
            out.push(SourceStatus {
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
}
