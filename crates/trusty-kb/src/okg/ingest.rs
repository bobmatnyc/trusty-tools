//! `KbStore` entry points over trusty-okf's ingest engine.
//!
//! Why: #10746 S4 moved the engine to `trusty_okf::build::ingest`. The
//! trusty-agents connector tools still call `KbStore::ingest_items` and
//! `KbStore::okg_sweep_deleted` until slice S11, so trusty-kb keeps the two
//! methods and delegates; there is one engine.
//! What: converts the typed registry row to trusty-okf's, parses `now` (the
//! engine takes every time from the caller as an instant, SPEC-OKF-01 §4.3),
//! creates the store root when absent (the engine requires an existing root,
//! §2.11 rule 1), and calls the engine. Then it runs trusty-kb's inverse-edge
//! reconcile over each item's entity, as `put_entity` did after every write
//! before the move; the reconcile is idempotent, and its failures join the
//! report's errors.
//! Test: `docstore_ingest_is_idempotent_end_to_end`,
//! `ingest_rejects_a_now_that_is_not_an_instant`, and the trusty-okf engine
//! suite.

use std::collections::BTreeSet;

use chrono::{DateTime, FixedOffset};
pub use trusty_okf::build::ingest::{INGEST_ACTOR, IngestFinding, IngestReport, SourceItem};
use trusty_okf::format::fields::parse_instant;

use crate::okg::registry::SourceSpec;
use crate::store::KbStore;

/// `now` as an instant with an offset, or an error naming the value.
///
/// #10746: no fallback. trusty-okf writes `generated.at` and never invents
/// a time or an offset (SPEC-OKF-01 §2.8).
fn instant(now: &str) -> anyhow::Result<DateTime<FixedOffset>> {
    parse_instant(now).ok_or_else(|| {
        anyhow::anyhow!("ingest time {now:?} is not an ISO 8601 instant with an offset")
    })
}

impl KbStore {
    /// Ingest already-fetched items for one source, gated by its ledger.
    ///
    /// Why/What: see the module doc and `trusty_okf::build::ingest::ingest_items`.
    /// `detect_deletions` must be true ONLY when `items` enumerates the
    /// source's entire corpus.
    /// Test: `docstore_ingest_is_idempotent_end_to_end`,
    /// `ingest_rejects_a_now_that_is_not_an_instant`.
    pub fn ingest_items(
        &self,
        spec: &SourceSpec,
        items: &[SourceItem],
        detect_deletions: bool,
        now: &str,
    ) -> anyhow::Result<IngestReport> {
        let at = instant(now)?;
        std::fs::create_dir_all(&self.root)?;
        let mut report = trusty_okf::build::ingest::ingest_items(
            &self.root,
            &self.profile,
            &spec.to_okf(),
            items,
            detect_deletions,
            at,
        )?;
        self.reconcile_items(spec, items, now, &mut report);
        Ok(report)
    }

    /// Materialise inverse edges for each item's entity (#10746: kept from
    /// `put_entity`, which reconciled after every write).
    fn reconcile_items(
        &self,
        spec: &SourceSpec,
        items: &[SourceItem],
        now: &str,
        report: &mut IngestReport,
    ) {
        for item in items {
            let result = self
                .entity_path(&spec.collection, &item.name)
                .and_then(|path| self.read_entity_at(&path))
                .and_then(|entity| match entity {
                    Some(e) => self
                        .reconcile_entity(&spec.collection, &item.name, &e, now)
                        .map(|_| ()),
                    None => Ok(()),
                });
            if let Err(e) = result {
                report
                    .errors
                    .push(format!("{}: reconcile: {e}", item.item_id));
            }
        }
    }

    /// Sweep for vanished items after a CHUNKED ingest; see
    /// `trusty_okf::build::ingest::sweep_deleted`.
    ///
    /// Test: `sweep_tombstones_only_after_the_full_set_is_known` (trusty-okf).
    pub fn okg_sweep_deleted(
        &self,
        spec: &SourceSpec,
        present_ids: &BTreeSet<String>,
        now: &str,
    ) -> anyhow::Result<IngestReport> {
        let now = instant(now)?;
        std::fs::create_dir_all(&self.root)?;
        Ok(trusty_okf::build::ingest::sweep_deleted(
            &self.root,
            &spec.to_okf(),
            present_ids,
            now,
        )?)
    }
}
