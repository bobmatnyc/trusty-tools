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
//! `c117_planted_verification_hash_never_verifies`,
//! `reingest_over_a_deprecated_concept_writes_content_and_keeps_status`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.9 (re-ingest keeps editorial keys), §2.10, §2.13.1.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde::Serialize;
use serde_yaml::{Mapping, Value};

use crate::OkfError;
use crate::build::collide::{self, Pins, targets_for};
use crate::build::ledger::{ItemStatus, Ledger, LedgerRecord, Watermark};
use crate::build::quarantine::Quarantine;
use crate::build::registry::SourceSpec;
use crate::build::trust::{TRUST_KEY, TrustLabel};
use crate::bundle::Bundle;
use crate::bundle::commit::LogMode;
use crate::bundle::log_md::LogBatch;
use crate::bundle::ops::OpKind;
use crate::bundle::put::{PutOutcome, WriteKind};
use crate::bundle::record::instant_string;
use crate::format::concept::{Concept, slugify};
use crate::format::fields::Actor;
use crate::format::hash::content_hash;
use crate::format::id::ConceptId;
use crate::format::protected::{SOURCE_FIELDS_KEY, is_protected_key};
use crate::format::tombstone::is_detectable_tombstone;
use crate::format::write::WriteMode;
use crate::profile::Profile;
use crate::state::revisions::run_id;

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

/// #10746 S8: the provenance keys the engine places from its caller's
/// [`Writer`] (`sources[]`, §2.14 rule 5; `extracted_by`, DOC-55 Q3); any
/// other key there is quarantined like a source field (§2.13.1).
pub const PROVENANCE_KEYS: [&str; 2] = ["sources", "extracted_by"];

/// #10746 S8: who writes an item: the `generated.by` actor (ingest or
/// import, §2.14 rule 11) and the provenance the writer itself derives,
/// never a source value. Kept apart from [`SourceItem`] so callers that
/// build items (trusty-agents through trusty-kb) are unchanged.
pub(crate) struct Writer<'a> {
    pub actor: &'a str,
    pub provenance: &'a Mapping,
}

/// The ingest engine's writer: [`INGEST_ACTOR`], no provenance.
pub(crate) fn ingest_writer(empty: &Mapping) -> Writer<'_> {
    Writer {
        actor: INGEST_ACTOR,
        provenance: empty,
    }
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

/// #10746 S8: one item refused before any write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemFailure {
    /// The item.
    pub item_id: String,
    /// Why: `collision` (§2.9 "Path collisions").
    pub reason: String,
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
    /// #10746 S8: items refused before any write, each with its reason
    /// (`collision`, §2.9). Counted as failed, never as written.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<ItemFailure>,
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
        self.failed.extend(other.failed);
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
/// #10746: trusty-kb's addressing, for an item with no ledger record. An
/// item the ledger knows keeps the full ID its record pins (§2.13 "Full-ID
/// addressing"); import assigns IDs by the §2.14 rule instead
/// (`import::id`).
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
    // #10746 S5: the run's `log.md` entries are written once, at the end.
    let batch = LogBatch::begin(&bundle, &run_id("ingest", &[&spec.id, &stamp]))?;

    // #10746 S8: map every item to its concept before any write; a
    // colliding item is refused and counted as failed (§2.9).
    let (targets, bad_ids) = targets_for(&ledger, spec, items);
    let refused = collide::refused(&spec.id, &targets, &Pins::load(bundle.root(), &spec.id)?);
    let target_of: BTreeMap<&str, &ConceptId> =
        targets.iter().map(|(i, id)| (i.as_str(), id)).collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for item in items {
        seen.insert(item.item_id.as_str());
        // A volatile item's fingerprint carries no information, so the skip
        // test must not be consulted for it at all — see `SourceItem::volatile`.
        if !item.volatile && ledger.is_current(&item.item_id, &item.fingerprint) {
            report.skipped += 1;
            continue;
        }
        if let Some(e) = bad_ids.get(item.item_id.as_str()) {
            report.errors.push(format!("{}: {e}", item.item_id));
            continue;
        }
        if refused.contains(&item.item_id) {
            report.failed.push(ItemFailure {
                item_id: item.item_id.clone(),
                reason: "collision".to_string(),
            });
            continue;
        }
        let id = target_of[item.item_id.as_str()];
        let first_time = !ledger.contains(&item.item_id);
        match write_item(
            &mut bundle,
            profile,
            spec,
            id,
            item,
            &now,
            &batch,
            &ingest_writer(&Mapping::new()),
        ) {
            Ok((_, findings)) => {
                let entity = id.as_str().to_string();
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
        sweep_into(
            &mut bundle,
            spec,
            &mut ledger,
            &present,
            &now,
            &batch,
            &mut report,
        )?;
    } else {
        report.missing = ledger
            .live_item_ids()
            .into_iter()
            .filter(|id| !seen.contains(id.as_str()))
            .collect();
    }

    report.watermark = ledger.watermark();
    // #10746 S5: a failed `log.md` write fails the run closed.
    let (_, left) = batch.flush_now(&mut bundle)?;
    push_op_findings(&mut report, left);
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
    let batch = LogBatch::begin(
        &bundle,
        &run_id("sweep", &[&spec.id, &instant_string(&now)]),
    )?;
    sweep_into(
        &mut bundle,
        spec,
        &mut ledger,
        present_ids,
        &now,
        &batch,
        &mut report,
    )?;
    report.watermark = ledger.watermark();
    let (_, left) = batch.flush_now(&mut bundle)?;
    push_op_findings(&mut report, left);
    Ok(report)
}

/// Shared body of the deletion sweep, used by both entry points above.
fn sweep_into(
    bundle: &mut Bundle,
    spec: &SourceSpec,
    ledger: &mut Ledger,
    present_ids: &BTreeSet<String>,
    now: &DateTime<FixedOffset>,
    batch: &LogBatch,
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
        match tombstone_item(bundle, ledger, &item_id, now, batch, INGEST_ACTOR) {
            Ok(Some((record, findings))) => {
                ledger.append(record)?;
                report.findings.extend(findings);
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

/// The source-derived top-level keys of one item, after the §2.13.1 check.
///
/// What: the envelope (`title`, `source_*`, `trust`) first; then `fields`,
/// then `frontmatter`. A protected key, an envelope key, or a key an earlier
/// input already set is quarantined; every other key is kept (unknown-key
/// preservation runs only after the check).
fn source_values(
    spec: &SourceSpec,
    item: &SourceItem,
    provenance: &Mapping,
) -> (Mapping, Quarantine) {
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
    // #10746 S8: the importer's own provenance is placed before any source
    // value; a key outside `PROVENANCE_KEYS` falls through to the check.
    let mut rest = Mapping::new();
    for (k, v) in provenance {
        match k.as_str() {
            Some(key) if PROVENANCE_KEYS.contains(&key) && !top.contains_key(k) => {
                top.insert(k.clone(), v.clone());
            }
            _ => {
                rest.insert(k.clone(), v.clone());
            }
        }
    }
    let inputs = rest
        .into_iter()
        .chain(item.fields.iter().map(|(k, v)| (s(k), s(v))))
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

/// Write one item as concept `id`, returning the put outcome and the
/// item's findings.
///
/// Overwrite (not merge) of the source-derived keys is deliberate: a
/// re-ingested item must REPLACE its prior entry, so stale provenance fields
/// from an earlier version cannot linger. #10746: [`PRESERVED_ON_REINGEST`]
/// survives; a legacy `updated` survives only when the content is unchanged
/// (a content change gets `generated` instead).
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_item(
    bundle: &mut Bundle,
    profile: &Profile,
    spec: &SourceSpec,
    id: &ConceptId,
    item: &SourceItem,
    now: &DateTime<FixedOffset>,
    batch: &LogBatch,
    by: &Writer<'_>,
) -> Result<(PutOutcome, Vec<IngestFinding>), OkfError> {
    let old = existing(bundle, id)?;
    let (next, findings) = next_concept(old.as_ref(), id, profile, spec, item, by.provenance);
    // #10746 S8: `generated.by` names the caller: ingest or import (§2.14
    // rule 11).
    let actor = Actor::parse(by.actor);
    // #10746: `SourceReingest`, so a deprecated concept takes the new source
    // content (SPEC-OKF-01 §3 (c2) (i)); §2.10 and `log.md` still apply.
    let (outcome, _, _) = bundle.put_inner(
        next,
        &actor,
        false,
        WriteMode::Lenient,
        now,
        WriteKind::SourceReingest,
        LogMode::Defer(batch),
    )?;
    Ok((outcome, findings))
}

/// The concept a source item renders to over `old` (#10746 S6: shared by
/// `write_item` and `rebuild`): the source-derived keys replaced,
/// [`PRESERVED_ON_REINGEST`] kept, `updated` kept only when the content is
/// unchanged. Returns the item's quarantine findings.
pub(crate) fn next_concept(
    old: Option<&Concept>,
    id: &ConceptId,
    profile: &Profile,
    spec: &SourceSpec,
    item: &SourceItem,
    provenance: &Mapping,
) -> (Concept, Vec<IngestFinding>) {
    let (top, quarantine) = source_values(spec, item, provenance);
    let mut next = match old {
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
    if let Some(o) = old
        && content_hash(o) == content_hash(&next)
        && let Some(updated) = o.get("updated")
    {
        next.set("updated", updated.clone());
    }
    (next, quarantine.findings)
}

/// Mark a vanished item's concept, returning the ledger line to append and
/// the tombstone's findings.
///
/// Returns `Ok(None)` when the concept is already gone from disk — there is
/// nothing to mark, and inventing a record would misreport the tree. A
/// tombstone changes lifecycle keys only, so §2.10 keeps `verified`.
/// #10746 S8: the concept is the full ID the ledger record pins (§2.13
/// "Full-ID addressing", criterion 136). The rendered bytes are checked with
/// the §4.8 detection rule before the rename; an undetectable tombstone
/// still lands, with a `tombstone_undetectable` error finding (criterion
/// 141).
pub(crate) fn tombstone_item(
    bundle: &mut Bundle,
    ledger: &Ledger,
    item_id: &str,
    now: &DateTime<FixedOffset>,
    batch: &LogBatch,
    actor: &str,
) -> Result<Option<(LedgerRecord, Vec<IngestFinding>)>, OkfError> {
    let Some(prior) = ledger.get(item_id) else {
        return Ok(None);
    };
    let id = ConceptId::parse(&prior.entity)?;
    let Some(mut next) = existing(bundle, &id)? else {
        return Ok(None);
    };
    next.set(SOURCE_STATUS, s(DELETED));
    next.set("tombstoned", s(&instant_string(now)));
    // A tombstone is a flag, never a deletion: the body and every distilled
    // fact survive.
    let actor = Actor::parse(actor);
    // #10746: `SourceReingest`, so a deprecated concept takes the new source
    // content (SPEC-OKF-01 §3 (c2) (i)); §2.10 and `log.md` still apply.
    let kind = WriteKind::SourceReingest;
    let plan = bundle.plan_put(next, &actor, false, WriteMode::Lenient, now, kind, None)?;
    let mut findings = Vec::new();
    if !is_detectable_tombstone(&String::from_utf8_lossy(&plan.post)) {
        findings.push(IngestFinding {
            kind: TOMBSTONE_UNDETECTABLE.to_string(),
            item_id: item_id.to_string(),
            key: String::new(),
        });
    }
    if plan.outcome != PutOutcome::Unchanged {
        let left = bundle.commit_op(plan, &actor, now, LogMode::Defer(batch), OpKind::Correct)?;
        findings.extend(op_findings(left));
    }
    Ok(Some((
        LedgerRecord {
            item_id: item_id.to_string(),
            fingerprint: prior.fingerprint.clone(),
            entity: prior.entity.clone(),
            ingested_at: instant_string(now),
            timestamp: prior.timestamp.clone(),
            status: ItemStatus::Tombstoned,
        },
        findings,
    )))
}

/// #10746 S8: the finding kind of a tombstone trusty-search will not detect
/// (§4.8); its severity is error.
pub const TOMBSTONE_UNDETECTABLE: &str = "tombstone_undetectable";

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

/// #10746 S5 r2: an operation finding (`staging_left`) as an ingest finding.
fn push_op_findings(report: &mut IngestReport, found: Vec<crate::format::validate::Finding>) {
    report.findings.extend(op_findings(found));
}

/// Operation findings as ingest findings.
pub(crate) fn op_findings(found: Vec<crate::format::validate::Finding>) -> Vec<IngestFinding> {
    found
        .into_iter()
        .map(|f| IngestFinding {
            kind: format!("{:?}", f.kind),
            item_id: f.path,
            key: String::new(),
        })
        .collect()
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
