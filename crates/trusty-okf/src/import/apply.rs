//! `import`: apply a plan under the bundle lock (SPEC-OKF-01 §4.10 phase
//! 2, §2.14 rules 5, 7, 10, 11).
//!
//! Why: an import is the ingest engine run against a registered store
//! (§2.13). Each write must converge after a crash, never overwrite bytes
//! trusty-okf cannot prove it wrote, never take an ID another item holds,
//! and never append the revision journal while a conflicted operation (a
//! pending `migrate`) holds it.
//! What: [`import`] re-runs the plan's refusals, opens the bundle with its
//! lock held for the run (recovery first), and refuses with
//! `RecoveryPending` before any write while a conflicted operation holds
//! the revision journal (§4.11 rule 2). It then registers the store, and for
//! each `create`, `update` or `conflict` item re-fetches the bytes, checks
//! them against the plan's fingerprint (`stale_plan`), extracts them (the
//! built-in tier, else `ex`, which is never called when the spec allows no
//! child process), re-checks collisions and §4.5 ownership under the lock,
//! and writes the concept through the ingest engine: revision line, then the
//! concept (a multi-file write is one operation-journal operation), then
//! the ledger line. Tombstones follow, on a deletion-safe plan only. The
//! run's `log.md` entries are written once at the end. The `_sources/
//! imports/` start line precedes the first write and the finish line
//! follows the last, so an attempt that writes nothing writes no byte. An
//! item failure is reported and the run goes on; a ledger, `log.md` or
//! run-file failure ends the run with an error, and the ledger records
//! nothing it did not write. Slice S8 of #10746.
//! Test: `c77_second_import_writes_zero_bytes_and_a_change_updates`,
//! `c78_hand_edited_concept_is_refused_on_reimport`,
//! `crash_at_every_import_boundary_recovers_to_the_uninterrupted_bundle`,
//! `ledger_append_failure_ends_the_run_and_records_nothing`.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};

use super::plan::{Action, ImportPlan, SKIP_CODES, error_code, ownership, preflight};
use super::report::{ImportFinding, ImportReport, ItemReport, is_refusal};
use super::runlog::RunLog;
use super::source::ImportSource;
use crate::OkfError;
use crate::build::collide::{self, Pins};
use crate::build::ingest::{
    IngestFinding, SourceItem, TOMBSTONE_UNDETECTABLE, Writer, tombstone_item, write_item,
};
use crate::build::ledger::{ItemStatus, Ledger, LedgerRecord};
use crate::build::registry::SourceRegistry;
use crate::bundle::Bundle;
use crate::bundle::log_md::LogBatch;
use crate::bundle::ops::{failpoint, io_seam};
use crate::bundle::put::PutOutcome;
use crate::bundle::record::instant_string;
use crate::extract::{BuiltinText, Extractor, TypeHint, fingerprint};
use crate::format::id::ConceptId;
use crate::profile::Profile;
use crate::state::revisions;

/// The actor of every imported concept's `generated.by` (§2.14 rule 11).
pub const IMPORT_ACTOR: &str = "process:trusty-okf-import";

/// A refusal-shaped error raised after the first write: an I/O error, so
/// the §4.10 mapping gives 1, not 2.
fn after_writes(root: &std::path::Path, e: OkfError) -> OkfError {
    if is_refusal(&e) {
        OkfError::Io {
            path: root.to_path_buf(),
            source: std::io::Error::other(format!("after writes began: {e}")),
        }
    } else {
        e
    }
}

fn finding(f: &IngestFinding) -> ImportFinding {
    let severity = match f.kind.as_str() {
        TOMBSTONE_UNDETECTABLE => "error",
        "quarantined_key" => "warning",
        _ => "info",
    };
    ImportFinding {
        severity: severity.to_string(),
        kind: f.kind.clone(),
        item_id: f.item_id.clone(),
        detail: f.key.clone(),
    }
}

/// The source item one planned entry becomes, and the provenance the
/// importer derives for it (§2.14 rules 5 and 11).
fn source_item(
    rel: &str,
    id: &ConceptId,
    fp: String,
    ts: Option<String>,
    ex: &dyn Extractor,
    mut fields: Mapping,
    text: String,
) -> (SourceItem, Mapping) {
    let stem = rel.rsplit('/').next().unwrap_or(rel);
    let stem = stem.strip_suffix(".md").unwrap_or(stem);
    let title = match fields.remove(Value::String("title".into())) {
        Some(Value::String(t)) if !t.trim().is_empty() => t,
        Some(other) => {
            // Not a usable title: kept, and quarantined by the engine.
            fields.insert(Value::String("title".into()), other);
            stem.to_string()
        }
        None => stem.replace(['-', '_'], " "),
    };
    let mut provenance = Mapping::new();
    let mut source = Mapping::new();
    source.insert(
        Value::String("resource".into()),
        Value::String(rel.to_string()),
    );
    provenance.insert(
        Value::String("sources".into()),
        Value::Sequence(vec![Value::Mapping(source)]),
    );
    if ex.id() != BuiltinText.id() {
        provenance.insert(
            Value::String("extracted_by".into()),
            Value::String(format!("{}@{}", ex.id(), ex.version())),
        );
    }
    let item = SourceItem {
        item_id: rel.to_string(),
        fingerprint: fp,
        name: id.as_str().to_string(),
        title,
        timestamp: ts,
        body: text,
        fields: BTreeMap::new(),
        frontmatter: fields,
        volatile: false,
    };
    (item, provenance)
}

/// Apply `plan` to `bundle` (SPEC-OKF-01 §4.3 `import`).
///
/// Why/What: see the module doc. `ex` extracts every item the built-in
/// tier does not handle; pass [`crate::extract::NoWorker`] when no worker is
/// configured. Every time value comes from `now`.
/// Errors before any write: the plan's refusals, `RecoveryPending`, a
/// registry or ledger read failure. After the first write: the ledger,
/// `log.md` and run-file failures, as I/O errors (see [`super::exit_code`]).
pub fn import(
    bundle: &mut Bundle,
    plan: &ImportPlan,
    src: &mut dyn ImportSource,
    ex: &dyn Extractor,
    now: DateTime<FixedOffset>,
) -> Result<ImportReport, OkfError> {
    let spec = &plan.spec;
    preflight(bundle.root(), spec)?;
    let mut b = Bundle::at(bundle.root())?;
    // #10746 S8: every import write appends the revision journal; a pending
    // migrate that holds it refuses the run before any write (S6 rule).
    b.check_recovery(&[])?;
    let root = b.root().to_path_buf();
    let stamp = instant_string(&now);
    let mut registry = SourceRegistry::load(&root)?;
    let (_, row_changed) = registry.upsert(spec.store.clone());
    let store = registry
        .get(&spec.store.id)
        .cloned()
        .unwrap_or_else(|| spec.store.clone());
    let mut ledger = Ledger::load(&root, &store.id)?;
    let lines = revisions::read(&root)?;
    let mut log = RunLog::new(&b, &plan.run_id, &store.id)?;
    let mut report = ImportReport {
        run_id: log.attempt().to_string(),
        store: store.id.clone(),
        listed: plan.listed,
        pagination_exhausted: plan.pagination_exhausted,
        deletion_safe: plan.deletion_safe,
        cursor: plan.cursor.clone(),
        ..ImportReport::default()
    };
    let targets: Vec<(String, ConceptId)> = plan
        .items
        .iter()
        .filter(|p| {
            matches!(
                p.action,
                Action::Create | Action::Update | Action::Unchanged | Action::Conflict
            )
        })
        .filter_map(|p| p.id.clone().map(|id| (p.item_id.clone(), id)))
        .collect();
    let refused = collide::refused(&store.id, &targets, &Pins::load(&root, &store.id)?);
    let batch = LogBatch::begin(&b, log.attempt())?;
    if row_changed {
        log.start(&b, &stamp)?;
        registry.save(&root).map_err(|e| after_writes(&root, e))?;
        failpoint("import-registry")?;
    }
    let profile = Profile::default_profile();
    for p in &plan.items {
        let item = |read: &str, write: &str| ItemReport::new(&p.item_id, p.id.clone(), read, write);
        let entry = match (p.action, &p.id) {
            (Action::Skip, _) => item("skipped", "none").because(p.reason.as_deref().unwrap_or("")),
            // A collision is an item read and refused before its write.
            (Action::Fail, _) if p.reason.as_deref() == Some("collision") => {
                item("read", "failed").because("collision")
            }
            (Action::Fail, _) => item("failed", "none").because(p.reason.as_deref().unwrap_or("")),
            (Action::Missing, _) => item("none", "missing"),
            (Action::Unchanged, _) => item("none", "unchanged"),
            (Action::Tombstone, _) if !plan.deletion_safe => item("none", "missing"),
            (Action::Tombstone, _) => {
                log.start(&b, &stamp)?;
                match tombstone_item(&mut b, &ledger, &p.item_id, &now, &batch, IMPORT_ACTOR) {
                    Ok(Some((rec, found))) => {
                        io_seam("import-ledger").map_err(|e| after_writes(&root, e))?;
                        ledger.append(rec).map_err(|e| after_writes(&root, e))?;
                        report.findings.extend(found.iter().map(finding));
                        item("none", "tombstoned")
                    }
                    Ok(None) => item("none", "missing"),
                    Err(e) => item("none", "failed").because(error_code(&e)),
                }
            }
            (_, None) => item("failed", "none").because("invalid_id"),
            (_, Some(id)) => {
                failpoint("import-item").map_err(|e| after_writes(&root, e))?;
                if refused.contains(&p.item_id) {
                    report
                        .items
                        .push(item("read", "failed").because("collision"));
                    continue;
                }
                let bytes = match src.fetch(&p.item_id) {
                    Ok(bytes) => bytes,
                    Err(e) if SKIP_CODES.contains(&e.code.as_str()) => {
                        report.items.push(item("skipped", "none").because(&e.code));
                        continue;
                    }
                    Err(e) => {
                        report.items.push(item("failed", "none").because(&e.code));
                        continue;
                    }
                };
                let hint = TypeHint::for_path(&p.item_id);
                let tier: &dyn Extractor = if BuiltinText::handles(&hint) {
                    &BuiltinText
                } else if !spec.child_processes {
                    report
                        .items
                        .push(item("skipped", "none").because("policy_refused"));
                    continue;
                } else {
                    ex
                };
                let fp = fingerprint(&bytes, tier);
                if p.fingerprint.as_ref().is_some_and(|planned| *planned != fp) {
                    report
                        .items
                        .push(item("failed", "none").because("stale_plan"));
                    continue;
                }
                if ledger.is_current(&p.item_id, &fp) {
                    report.items.push(item("read", "unchanged"));
                    continue;
                }
                let got = match tier.extract(&bytes, &hint) {
                    Ok(got) => got,
                    Err(e) if SKIP_CODES.contains(&e.code.as_str()) => {
                        report.items.push(item("skipped", "none").because(&e.code));
                        continue;
                    }
                    Err(e) => {
                        report.items.push(item("failed", "none").because(&e.code));
                        continue;
                    }
                };
                match ownership(&root, &lines, id) {
                    Ok(Ok(_)) => {}
                    Ok(Err(why)) => {
                        report.items.push(item("read", "failed").because(why));
                        continue;
                    }
                    Err(e) => {
                        report
                            .items
                            .push(item("read", "failed").because(error_code(&e)));
                        continue;
                    }
                }
                for note in &got.notes {
                    report.findings.push(ImportFinding {
                        severity: "warning".into(),
                        kind: note.clone(),
                        item_id: p.item_id.clone(),
                        detail: String::new(),
                    });
                }
                let (si, provenance) = source_item(
                    &p.item_id,
                    id,
                    fp,
                    p.timestamp.clone(),
                    tier,
                    got.fields,
                    got.text,
                );
                log.start(&b, &stamp)?;
                let first = !ledger.contains(&p.item_id);
                match write_item(
                    &mut b,
                    &profile,
                    &store,
                    id,
                    &si,
                    &now,
                    &batch,
                    &Writer {
                        actor: IMPORT_ACTOR,
                        provenance: &provenance,
                    },
                ) {
                    Ok((outcome, found)) => {
                        failpoint("import-ledger").map_err(|e| after_writes(&root, e))?;
                        io_seam("import-ledger").map_err(|e| after_writes(&root, e))?;
                        ledger
                            .append(LedgerRecord {
                                item_id: p.item_id.clone(),
                                fingerprint: si.fingerprint.clone(),
                                entity: id.as_str().to_string(),
                                ingested_at: stamp.clone(),
                                timestamp: si.timestamp.clone(),
                                status: ItemStatus::Ingested,
                            })
                            .map_err(|e| after_writes(&root, e))?;
                        report.findings.extend(found.iter().map(finding));
                        let (write, cleared) = match outcome {
                            PutOutcome::Created => ("created", 0),
                            PutOutcome::Unchanged if first => ("created", 0),
                            PutOutcome::Unchanged => ("unchanged", 0),
                            PutOutcome::Updated {
                                verifications_cleared,
                                ..
                            } => ("updated", verifications_cleared),
                        };
                        let mut r = item("read", write);
                        r.invalidated = cleared > 0;
                        r
                    }
                    Err(e) => item("read", "failed").because(error_code(&e)),
                }
            }
        };
        report.items.push(entry);
    }
    let (_, left) = batch
        .flush_now(&mut b)
        .map_err(|e| after_writes(&root, e))?;
    report
        .findings
        .extend(crate::build::ingest::op_findings(left).iter().map(finding));
    report.unfed_states(&b)?;
    let written = [report.created, report.updated, report.tombstoned];
    failpoint("import-finish")?;
    log.finish(&b, &stamp, written)
        .map_err(|e| after_writes(&root, e))?;
    Ok(report)
}
