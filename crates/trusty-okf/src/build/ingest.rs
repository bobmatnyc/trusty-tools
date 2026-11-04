//! The ingest engine — fetched items in, ledger-gated concepts out.
//!
//! Why: this is the one place that decides "write, replace, skip, or tombstone",
//! and it is deliberately FETCHER-AGNOSTIC. Doc-store scanning is pure
//! filesystem work and lives in this crate; connector sources go through their
//! own adapters outside trusty-okf (B1, B6). Splitting at [`SourceItem`] means
//! every idempotency property — re-run adds nothing, a widened window adds
//! only the older items, a changed file replaces its concept — is
//! unit-testable with synthetic items and no network at all.
//!
//! What: [`SourceItem`] is one normalised fetched thing. [`ingest_items`] walks
//! them against the source's [`Ledger`], writing concepts through
//! `Bundle::put` (§2.10: a content change moves `verified[]` to the record,
//! sets `generated` and logs once), appending one durable ledger line per
//! item as it goes. [`IngestReport`] is the caller-facing summary.
//! #10746 S4 changes, per SPEC-OKF-01 §2.9 and §2.13.1:
//! - a re-ingest keeps [`PRESERVED_ON_REINGEST`] (editorial and verification
//!   keys) from the existing concept and still replaces every source-derived
//!   key;
//! - every key a fetcher field, an extractor field or a source file's
//!   frontmatter supplies is checked against the protected set BEFORE the
//!   merge; a protected key, or one the engine writes itself, moves unchanged
//!   under `source_fields` and the report gains one `quarantined_key`
//!   finding per key and item;
//! - the engine alone writes `trust` (from the registration) and `generated`
//!   (with [`INGEST_ACTOR`]).
//!
//! Test: `rerun_ingests_nothing_new`, `changed_item_replaces_entity`,
//! `wider_window_only_adds_older_items`, `deletion_tombstones_when_enabled`,
//! `volatile_item_is_never_skipped_on_a_later_run`,
//! `sweep_tombstones_only_after_the_full_set_is_known`,
//! `merge_folds_chunk_reports`, `per_item_errors_do_not_abort_the_run`,
//! `reingest_keeps_editorial_and_verification_keys`,
//! `c116_protected_keys_from_every_input_are_quarantined`,
//! `c117_planted_verification_hash_never_verifies`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.9 (re-ingest keeps editorial keys), §2.10, §2.13.1.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde::Serialize;
use serde_yaml::{Mapping, Value};

use crate::OkfError;
use crate::build::ledger::{ItemStatus, Ledger, LedgerRecord, Watermark};
use crate::build::registry::SourceSpec;
use crate::build::trust::{TRUST_KEY, TrustLabel};
use crate::bundle::Bundle;
use crate::bundle::record::instant_string;
use crate::format::concept::{Concept, slugify};
use crate::format::fields::Actor;
use crate::format::hash::content_hash;
use crate::format::id::ConceptId;
use crate::format::protected::{SOURCE_FIELDS_KEY, is_protected_key};
use crate::format::write::WriteMode;
use crate::profile::Profile;

/// How many concept paths an [`IngestReport`] lists before truncating.
const MAX_REPORTED_ENTITIES: usize = 50;

/// The `generated.by` actor of every concept the engine writes (§2.13.1).
pub const INGEST_ACTOR: &str = "process:trusty-okf-ingest";

/// Keys a re-ingest copies from the existing concept (SPEC-OKF-01 §2.9).
///
/// `status`, `supersedes`, `superseded_by`, `stale_after` are the editorial
/// keys §2.9 names; `verified`, `legacy_verified`, `legacy_generated` are the
/// verification keys (§2.10 then decides whether `verified` stays);
/// `created` and `generated` keep the concept's own history. #10746.
pub const PRESERVED_ON_REINGEST: [&str; 9] = [
    "status",
    "supersedes",
    "superseded_by",
    "stale_after",
    "verified",
    "legacy_verified",
    "legacy_generated",
    "created",
    "generated",
];

/// Source-status keys a tombstone writes (shared with trusty-common's
/// `knowledge_document`, which trusty-okf cannot link under B8).
const SOURCE_STATUS: &str = "source_status";
const DELETED: &str = "deleted";

/// One normalised item fetched from a source, ready to become a concept.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceItem {
    /// The source's own stable id for this item (relative path, message id,
    /// file id). Ledger key — must be stable across runs or nothing dedupes.
    pub item_id: String,
    /// Change token: content hash, revision, or an immutable id.
    pub fingerprint: String,
    /// Concept display name; slugged for the filename.
    pub name: String,
    /// Human title for the frontmatter.
    pub title: String,
    /// The item's own timestamp, ISO-8601, when known.
    pub timestamp: Option<String>,
    /// Markdown body.
    pub body: String,
    /// Extra provenance frontmatter from the fetcher (`from`, `source_path`,
    /// `url`, …). Checked against the protected set (§2.13.1).
    pub fields: BTreeMap<String, String>,
    /// #10746: structured frontmatter the item carries — a source `.md`
    /// file's own frontmatter, or an extractor's `fields` (§2.13.1). Checked
    /// like `fields`; a key `fields` already set is quarantined.
    pub frontmatter: Mapping,
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

/// One report finding (§2.13.1, Appendix A).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestFinding {
    /// `quarantined_key`.
    pub kind: String,
    /// The item the finding is about.
    pub item_id: String,
    /// The key that was quarantined.
    pub key: String,
}

/// What one ingest run did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IngestReport {
    /// Canonical source id.
    pub source_id: String,
    /// `docstore` or the connector kind.
    pub kind: String,
    /// Collection concepts were written into.
    pub collection: String,
    /// Items presented to the engine.
    pub scanned: usize,
    /// Items never seen before — genuinely new concepts.
    pub ingested: usize,
    /// Items seen before whose fingerprint changed — concepts replaced.
    pub updated: usize,
    /// Items already current — untouched.
    pub skipped: usize,
    /// Concepts marked because their source item vanished.
    pub tombstoned: usize,
    /// Previously-ingested item ids absent from this run that were NOT
    /// tombstoned (because the source does not enumerate its full corpus, or
    /// tombstoning is off). Surfaced, never silently dropped.
    pub missing: Vec<String>,
    /// Concepts written this run (capped — see `entities_truncated`).
    pub entities: Vec<String>,
    /// True when `entities` was capped at [`MAX_REPORTED_ENTITIES`].
    pub entities_truncated: bool,
    /// Per-item failures. An item that fails does not abort the run.
    pub errors: Vec<String>,
    /// #10746: one `quarantined_key` finding per key and item (§2.13.1).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<IngestFinding>,
    /// Coverage after this run.
    pub watermark: Watermark,
}

impl IngestReport {
    /// Fold another chunk's report into this one.
    ///
    /// Why: a large network source is ingested page by page so partial progress
    /// commits, but the caller still owes the agent ONE coherent summary.
    /// What: sums the counters, concatenates errors/missing/findings, and
    /// appends concept paths up to the listing cap. `watermark` is taken from
    /// `other` because later chunks observe strictly more of the journal.
    /// Test: `merge_folds_chunk_reports`.
    pub fn merge(&mut self, other: IngestReport) {
        self.scanned += other.scanned;
        self.ingested += other.ingested;
        self.updated += other.updated;
        self.skipped += other.skipped;
        self.tombstoned += other.tombstoned;
        self.missing.extend(other.missing);
        self.errors.extend(other.errors);
        self.findings.extend(other.findings);
        for entity in other.entities {
            push_entity(self, entity);
        }
        self.entities_truncated |= other.entities_truncated;
        self.watermark = other.watermark;
    }
}

/// The concept a source's item is written to: `<collection>/<slug(name)>`.
///
/// #10746: unchanged addressing from trusty-kb; slice S8 replaces it with the
/// §2.14 ID rule and full-ID addressing (criteria 136, 138).
pub fn concept_id_for(collection: &str, name: &str) -> Result<ConceptId, OkfError> {
    ConceptId::parse(&format!("{collection}/{}", slugify(name)))
}

fn report_for(spec: &SourceSpec, scanned: usize) -> IngestReport {
    IngestReport {
        source_id: spec.id.clone(),
        kind: spec.kind().to_string(),
        collection: spec.collection.clone(),
        scanned,
        ..IngestReport::default()
    }
}

/// Ingest already-fetched items for one source, gated by its ledger.
///
/// Why: see the module doc. `detect_deletions` must be true ONLY when `items`
/// enumerates the source's entire corpus — a doc-store walk does; a windowed
/// connector pull emphatically does not, and passing true there would
/// tombstone every item outside the window.
/// What: `root` must exist (§2.11 rule 1). Each item that is not current is
/// written through `Bundle::put` (lenient) and then recorded in the ledger;
/// a failed item is reported and the run goes on. Every time value comes from
/// `now` (§4.3). Errors: only a ledger load or append failure aborts the run.
/// Test: `rerun_ingests_nothing_new`, `deletion_tombstones_when_enabled`,
/// `reingest_keeps_editorial_and_verification_keys`,
/// `c116_protected_keys_from_every_input_are_quarantined`.
pub fn ingest_items(
    root: &Path,
    profile: &Profile,
    spec: &SourceSpec,
    items: &[SourceItem],
    detect_deletions: bool,
    now: DateTime<FixedOffset>,
) -> Result<IngestReport, OkfError> {
    let mut bundle = Bundle::at(root)?;
    let mut ledger = Ledger::load(bundle.root(), &spec.id)?;
    let mut report = report_for(spec, items.len());
    let stamp = instant_string(&now);

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
        match write_item(&mut bundle, profile, spec, item, &now) {
            Ok((entity, findings)) => {
                // Ledger append happens AFTER the concept write and BEFORE
                // the next item, so a crash can only ever leave a concept
                // whose ledger line is missing — which the next run simply
                // rewrites identically. The reverse order would lose data.
                ledger.append(LedgerRecord {
                    item_id: item.item_id.clone(),
                    fingerprint: item.fingerprint.clone(),
                    entity: entity.clone(),
                    ingested_at: stamp.clone(),
                    timestamp: item.timestamp.clone(),
                    status: ItemStatus::Ingested,
                })?;
                report.findings.extend(findings);
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
        sweep_into(&mut bundle, spec, &mut ledger, &present, &now, &mut report)?;
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
pub fn sweep_deleted(
    root: &Path,
    spec: &SourceSpec,
    present_ids: &BTreeSet<String>,
    now: DateTime<FixedOffset>,
) -> Result<IngestReport, OkfError> {
    let mut bundle = Bundle::at(root)?;
    let mut ledger = Ledger::load(bundle.root(), &spec.id)?;
    let mut report = report_for(spec, 0);
    sweep_into(
        &mut bundle,
        spec,
        &mut ledger,
        present_ids,
        &now,
        &mut report,
    )?;
    report.watermark = ledger.watermark();
    Ok(report)
}

/// Shared body of the deletion sweep, used by both entry points above.
fn sweep_into(
    bundle: &mut Bundle,
    spec: &SourceSpec,
    ledger: &mut Ledger,
    present_ids: &BTreeSet<String>,
    now: &DateTime<FixedOffset>,
    report: &mut IngestReport,
) -> Result<(), OkfError> {
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
        match tombstone_item(bundle, spec, ledger, &item_id, now) {
            Ok(Some(record)) => {
                ledger.append(record)?;
                report.tombstoned += 1;
            }
            // The ledger names a concept that is no longer on disk —
            // nothing to mark, but the caller should still see it.
            Ok(None) => report.missing.push(item_id),
            Err(e) => report.errors.push(format!("{item_id}: {e}")),
        }
    }
    Ok(())
}

/// The concept at `id`, or `None` when its file does not exist.
fn existing(bundle: &Bundle, id: &ConceptId) -> Result<Option<Concept>, OkfError> {
    match bundle.get(id) {
        Ok(c) => Ok(Some(c)),
        Err(OkfError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Quarantined values for one item, plus the findings they produce.
#[derive(Default)]
struct Quarantine {
    values: Mapping,
    /// Keys whose entry already holds a list of distinct source values.
    multi: BTreeSet<String>,
    findings: Vec<IngestFinding>,
}

impl Quarantine {
    /// Move `key: value` under `source_fields`, unchanged. A second distinct
    /// value for one key turns the entry into a list of the values, so
    /// nothing is dropped.
    fn hold(&mut self, item_id: &str, key: String, value: Value) {
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

/// The source-derived top-level keys of one item, after the §2.13.1 check.
///
/// What: the envelope (`title`, `source_*`, `trust`) first; then `fields`,
/// then `frontmatter`. A protected key, an envelope key, or a key an earlier
/// input already set is quarantined; every other key is kept (unknown-key
/// preservation runs only after the check).
fn source_values(spec: &SourceSpec, item: &SourceItem) -> (Mapping, Quarantine) {
    let mut top = Mapping::new();
    let mut q = Quarantine::default();
    top.insert(s("title"), s(&item.title));
    top.insert(s("source_id"), s(&spec.id));
    top.insert(s("source_kind"), s(spec.kind()));
    top.insert(s("source_item_id"), s(&item.item_id));
    // #4532 / DOC-63 §6.3 `S-4.3`: the trust label is stamped HERE, by the
    // engine, from the operator-written `SourceSpec` alone. #10746: a source
    // value for `trust` is quarantined below, never honoured.
    top.insert(s(TRUST_KEY), s(TrustLabel::for_source(spec).as_str()));
    if let Some(ts) = item.timestamp.as_deref() {
        top.insert(s("source_timestamp"), s(ts));
    }
    let inputs = item
        .fields
        .iter()
        .map(|(k, v)| (s(k), s(v)))
        .chain(item.frontmatter.iter().map(|(k, v)| (k.clone(), v.clone())));
    for (k, v) in inputs {
        let key = match &k {
            Value::String(key) => key.clone(),
            other => serde_yaml::to_string(other)
                .unwrap_or_default()
                .trim()
                .to_string(),
        };
        let placeable = k.is_string() && !is_protected_key(&key) && !top.contains_key(&k);
        if placeable {
            top.insert(k, v);
        } else {
            q.hold(&item.item_id, key, v);
        }
    }
    (top, q)
}

/// Write one item as a concept, returning its `<collection>/<slug>` and the
/// item's findings.
///
/// Overwrite (not merge) of the source-derived keys is deliberate: a
/// re-ingested item must REPLACE its prior entry, so stale provenance fields
/// from an earlier version cannot linger. #10746: [`PRESERVED_ON_REINGEST`]
/// survives; a legacy `updated` survives only when the content is unchanged
/// (a content change gets `generated` instead).
fn write_item(
    bundle: &mut Bundle,
    profile: &Profile,
    spec: &SourceSpec,
    item: &SourceItem,
    now: &DateTime<FixedOffset>,
) -> Result<(String, Vec<IngestFinding>), OkfError> {
    let id = concept_id_for(&spec.collection, &item.name)?;
    let old = existing(bundle, &id)?;
    let (top, quarantine) = source_values(spec, item);
    let mut next = match &old {
        Some(c) => c.clone(),
        None => Concept::new(id.clone(), Mapping::new(), String::new()),
    };
    let old_type = next.get("type").cloned();
    let stored: Vec<String> = next
        .values()
        .keys()
        .filter_map(|k| k.as_str().map(str::to_string))
        .collect();
    for key in stored {
        if !PRESERVED_ON_REINGEST.contains(&key.as_str()) {
            next.remove(&key);
        }
    }
    for (k, v) in top {
        if let Value::String(key) = k {
            next.set(&key, v);
        }
    }
    if !quarantine.values.is_empty() {
        next.set(SOURCE_FIELDS_KEY, Value::Mapping(quarantine.values));
    }
    if next.get("type").is_none() {
        let ty = old_type.unwrap_or_else(|| s(&profile.default_type_for(&spec.collection)));
        next.set("type", ty);
    }
    next.body = body_text(&item.body);
    if let Some(o) = &old
        && content_hash(o) == content_hash(&next)
        && let Some(updated) = o.get("updated")
    {
        next.set("updated", updated.clone());
    }
    let actor = Actor::parse(INGEST_ACTOR);
    bundle.put_inner(next, &actor, false, WriteMode::Lenient, now, false)?;
    Ok((id.as_str().to_string(), quarantine.findings))
}

/// Mark a vanished item's concept, returning the ledger line to append.
///
/// Returns `Ok(None)` when the concept is already gone from disk — there is
/// nothing to mark, and inventing a record would misreport the tree. A
/// tombstone changes lifecycle keys only, so §2.10 keeps `verified`.
fn tombstone_item(
    bundle: &mut Bundle,
    spec: &SourceSpec,
    ledger: &Ledger,
    item_id: &str,
    now: &DateTime<FixedOffset>,
) -> Result<Option<LedgerRecord>, OkfError> {
    let Some(prior) = ledger.get(item_id) else {
        return Ok(None);
    };
    // #10746: trusty-kb's last-segment addressing, kept until S8 (crit. 136).
    let slug = prior.entity.rsplit('/').next().unwrap_or(&prior.entity);
    let id = ConceptId::parse(&format!("{}/{slug}", spec.collection))?;
    let Some(mut next) = existing(bundle, &id)? else {
        return Ok(None);
    };
    next.set(SOURCE_STATUS, s(DELETED));
    next.set("tombstoned", s(&instant_string(now)));
    // A tombstone is a flag, never a deletion: the body and every distilled
    // fact survive.
    let actor = Actor::parse(INGEST_ACTOR);
    bundle.put_inner(next, &actor, false, WriteMode::Lenient, now, false)?;

    Ok(Some(LedgerRecord {
        item_id: item_id.to_string(),
        fingerprint: prior.fingerprint.clone(),
        entity: prior.entity.clone(),
        ingested_at: instant_string(now),
        timestamp: prior.timestamp.clone(),
        status: ItemStatus::Tombstoned,
    }))
}

/// The stored body for an item: trusty-kb's form, a blank line after the
/// fence and one trailing newline, or empty.
fn body_text(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("\n{trimmed}\n")
    }
}

/// Record a concept path in the report, respecting the listing cap.
fn push_entity(report: &mut IngestReport, entity: String) {
    if report.entities.len() < MAX_REPORTED_ENTITIES {
        report.entities.push(entity);
    } else {
        report.entities_truncated = true;
    }
}

fn s(v: &str) -> Value {
    Value::String(v.to_string())
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod tests;
