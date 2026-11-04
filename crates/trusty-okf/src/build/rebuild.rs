//! `rebuild`: regenerate ledger-owned concepts from store contents, keeping
//! history (SPEC-OKF-01 §4.5, §3 (b) B, §3 (q) C).
//!
//! Why: an OKG is a build product, so a rebuild is how it migrates and how
//! it follows its sources. A rebuild must never touch a hand-owned concept,
//! never overwrite bytes that trusty-okf cannot prove it wrote, never
//! rewrite an existing `log.md` entry, and never leave a concept with mixed
//! content when it is interrupted.
//! What: [`rebuild`] reads each source's ledger. For each store item the
//! ledger lists as ingested, it classifies the concept's disk bytes against
//! the revision journal (§4.5 "Ownership proof"): a proven `origin: source`
//! record is engine-owned and regenerates; a proven `origin: caller` record
//! is `caller_edit`; disk bytes no line records are `hand_edit`; an unproven
//! record or none first tries the equality proof (the trusty-kb v0.1 render
//! of the item and its ledger history) and on equal bytes records a proven
//! line, else is `no_written_hash`. Every preserved concept stays
//! byte-identical and is listed in [`RebuildReport::conflicts`]; the others
//! still rebuild. The regenerated concepts (those whose bytes change) and one
//! `**Rebuilt**` `log.md` entry run as one journaled `Rebuild` operation:
//! each target staged under `_sources/ops/<op-id>/new/`, the intent, then
//! each rename after a pre-image check, `log.md` last (§4.5 "Staged
//! generation and swap"). Every time value comes from `now`. Slice S6 of
//! #10746.
//! Test: `c28_rebuild_keeps_hand_owned_and_refuses_hand_edits`,
//! `c29_rebuilds_at_different_times_are_byte_identical`,
//! `c110_rebuild_only_inserts_its_own_log_lines`,
//! `c111_unproven_and_hand_edited_ledger_concepts_are_preserved`,
//! `c112_rebuild_crash_and_mid_swap_edits`,
//! `c130_rebuild_follows_a_source_change_over_verify_and_renewal`,
//! `c131_caller_edit_and_migrated_legacy_concepts_are_preserved`.

use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};

use crate::OkfError;
use crate::build::ingest::{INGEST_ACTOR, SourceItem, next_concept};
use crate::build::jsonl::read_journal;
use crate::build::ledger::{ItemStatus, Ledger, LedgerRecord};
use crate::build::registry::SourceSpec;
use crate::build::trust::{TRUST_KEY, TrustLabel};
use crate::bundle::Bundle;
use crate::bundle::commit::PutPlan;
use crate::bundle::log_md::{insert, prepare_entry};
use crate::bundle::ops::{self, OpKind, OpPlan};
use crate::bundle::paths::{append_line, read_rel};
use crate::bundle::put::{PutOutcome, WriteKind};
use crate::bundle::record::{instant_string, record_rel};
use crate::format::concept::{Entity, parse_concept};
use crate::format::fields::Actor;
use crate::format::hash::{bytes_hash, content_hash};
use crate::format::id::ConceptId;
use crate::format::validate::Finding;
use crate::format::write::WriteMode;
use crate::profile::Profile;
use crate::state::revisions::{self, ORIGIN_SOURCE, REVISIONS_REL, RevisionLine, run_id};

/// A ledger-owned concept `rebuild` left byte-identical (§4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildConflict {
    /// The concept.
    pub id: ConceptId,
    /// `hand_edit`, `caller_edit`, `no_written_hash`, or the reason of a
    /// `Conflict` the write refused with (for example `fold_collision`).
    pub reason: String,
}

/// What [`rebuild`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RebuildReport {
    /// Concepts whose bytes the run replaced.
    pub rebuilt: Vec<ConceptId>,
    /// Legacy concepts the equality proof bound to their bytes.
    pub proven: Vec<ConceptId>,
    /// Preserved concepts, each with its `Conflict` reason.
    pub conflicts: Vec<RebuildConflict>,
    /// Verification events the rebuilt content moved to history (§2.10).
    pub verifications_cleared: usize,
    /// The lines the run added to `log.md`, in order; removing them yields
    /// the old file (§4.5 "`log.md`").
    pub log_added: Vec<String>,
    /// Findings of the writes and the operation.
    pub findings: Vec<Finding>,
}

/// How the bytes on disk relate to the revision journal (§4.5).
enum Owner {
    Engine,
    Unproven,
    Caller,
    HandEdit,
}

fn owner(lines: &[RevisionLine], id: &ConceptId, disk: Option<&[u8]>) -> Owner {
    let Some(bytes) = disk else {
        // The ledger names a concept that is gone: nothing to protect.
        return Owner::Engine;
    };
    match revisions::record_of(lines, id.as_str(), &bytes_hash(bytes)) {
        Some(l) if !l.is_proven() => Owner::Unproven,
        Some(l) if l.is_source() => Owner::Engine,
        Some(_) => Owner::Caller,
        None if lines.iter().any(|l| l.id == id.as_str()) => Owner::HandEdit,
        None => Owner::Unproven,
    }
}

/// The trusty-kb v0.1 render of `item` (§3 (q) A): the mapping
/// trusty-kb's ingest `write_item` built at `c1eed98441`, with `created` the item's first
/// ledger time and `updated` its newest, through the moved v0.1 renderer.
fn legacy_render(
    root: &Path,
    profile: &Profile,
    spec: &SourceSpec,
    item: &SourceItem,
) -> Result<String, OkfError> {
    let s = |v: &str| Value::String(v.to_string());
    let records: Vec<LedgerRecord> =
        read_journal::<LedgerRecord>(&Ledger::path_for(root, &spec.id)?)?
            .records
            .into_iter()
            .filter(|r| r.item_id == item.item_id)
            .collect();
    let mut m = Mapping::new();
    for (k, v) in &item.fields {
        m.insert(s(k), s(v));
    }
    m.insert(s("title"), s(&item.title));
    m.insert(s("source_id"), s(&spec.id));
    m.insert(s("source_kind"), s(spec.kind()));
    m.insert(s("source_item_id"), s(&item.item_id));
    m.insert(s(TRUST_KEY), s(TrustLabel::for_source(spec).as_str()));
    if let Some(ts) = &item.timestamp {
        m.insert(s("source_timestamp"), s(ts));
    }
    m.insert(s("type"), s(&profile.default_type_for(&spec.collection)));
    if let (Some(first), Some(last)) = (records.first(), records.last()) {
        m.insert(s("created"), s(&first.ingested_at));
        m.insert(s("updated"), s(&last.ingested_at));
    }
    let entity = Entity {
        frontmatter: Value::Mapping(m),
        body: item.body.trim().to_string(),
    };
    Ok(entity.to_content())
}

/// The proven revision line the equality proof records for `bytes`.
fn proof_line(
    id: &ConceptId,
    bytes: &[u8],
    stamp: &str,
    run: &str,
) -> Result<RevisionLine, OkfError> {
    let (c, _) = parse_concept(id.clone(), &String::from_utf8_lossy(bytes))?;
    Ok(RevisionLine {
        id: id.as_str().to_string(),
        content: content_hash(&c).as_str().to_string(),
        bytes: bytes_hash(bytes),
        op_kind: "equality_proof".to_string(),
        now: stamp.to_string(),
        run: run.to_string(),
        op: None,
        step: None,
        proven: Some(true),
        origin: Some(ORIGIN_SOURCE.to_string()),
    })
}

fn json(v: &RevisionLine) -> Result<serde_json::Value, OkfError> {
    serde_json::to_value(v).map_err(|e| OkfError::Parse {
        path: REVISIONS_REL.into(),
        line: None,
        message: e.to_string(),
    })
}

/// Regenerate every ledger-owned concept from store contents (SPEC-OKF-01
/// §4.3, §4.5).
///
/// Why/What: see the module doc. `sources` pairs each source with its
/// complete current items; an item the ledger does not list as ingested is
/// not ledger-owned here and is left to `ingest_items`. #10746: the §4.3
/// sketch takes a bundle spec, stores and an item source; this takes the S4 shapes plus the
/// caller's `now`, as `ingest_items` does (§4.3: no clock).
/// Errors: a ledger or journal read failure, a path of a conflicted
/// operation (`RecoveryPending`, before any write), or a pre-image mismatch
/// at swap time (`RecoveryPending`; the op stays as `recovery_conflict`).
/// Test: see the module doc.
pub fn rebuild(
    root: &Path,
    profile: &Profile,
    sources: &[(SourceSpec, Vec<SourceItem>)],
    now: DateTime<FixedOffset>,
) -> Result<RebuildReport, OkfError> {
    let mut bundle = Bundle::at(root)?;
    // #10746: every rebuild write appends the revision journal, the
    // no-plans proof append included; a pending migrate that holds it
    // refuses the run before any write (§4.11 rule 2).
    bundle.check_recovery(&[])?;
    let root = bundle.root().to_path_buf();
    let lines = revisions::read(&root)?;
    let stamp = instant_string(&now);
    let run = run_id("rebuild", &[&stamp]);
    let actor = Actor::parse(INGEST_ACTOR);
    let mut report = RebuildReport::default();
    let mut proofs: Vec<RevisionLine> = Vec::new();
    let mut plans: Vec<PutPlan> = Vec::new();
    for (spec, items) in sources {
        let ledger = Ledger::load(&root, &spec.id)?;
        for item in items {
            let Some(rec) = ledger.get(&item.item_id) else {
                continue;
            };
            if rec.status != ItemStatus::Ingested {
                continue;
            }
            let id = ConceptId::parse(&rec.entity)?;
            let disk = read_rel(&root, &id.file_name())?;
            let conflict = |why: &str| RebuildConflict {
                id: id.clone(),
                reason: why.to_string(),
            };
            match owner(&lines, &id, disk.as_deref()) {
                Owner::Engine => {}
                Owner::Caller => {
                    report.conflicts.push(conflict("caller_edit"));
                    continue;
                }
                Owner::HandEdit => {
                    report.conflicts.push(conflict("hand_edit"));
                    continue;
                }
                Owner::Unproven => {
                    let bytes = disk.as_deref().unwrap_or_default();
                    if legacy_render(&root, profile, spec, item)?.as_bytes() != bytes {
                        report.conflicts.push(conflict("no_written_hash"));
                        continue;
                    }
                    proofs.push(proof_line(&id, bytes, &stamp, &run)?);
                    report.proven.push(id.clone());
                }
            }
            let old = match &disk {
                Some(b) => Some(parse_concept(id.clone(), &String::from_utf8_lossy(b))?.0),
                None => None,
            };
            let (next, _) = next_concept(old.as_ref(), &id, profile, spec, item);
            let kind = WriteKind::SourceReingest;
            // #10746 S6: a `Conflict` refusal of one concept is reported and
            // the run goes on (§4.5); any other error fails the run.
            match bundle.plan_put(next, &actor, false, WriteMode::Lenient, &now, kind, None) {
                Ok(p) if p.outcome == PutOutcome::Unchanged => {}
                Ok(p) => plans.push(p),
                Err(OkfError::Conflict { reason, .. }) => report.conflicts.push(conflict(&reason)),
                Err(e) => return Err(e),
            }
        }
    }
    if plans.is_empty() {
        if !proofs.is_empty() {
            let text: Vec<String> = proofs
                .iter()
                .map(RevisionLine::json)
                .collect::<Result<_, _>>()?;
            append_line(&root, REVISIONS_REL, &text.join("\n"))?;
        }
        return Ok(report);
    }
    let mut op = OpPlan::new(OpKind::Rebuild, &stamp, INGEST_ACTOR);
    for line in &proofs {
        op.append(REVISIONS_REL, json(line)?);
    }
    let mut named = Vec::new();
    for p in &plans {
        for l in &p.lines {
            op.append(&record_rel(&p.id), l.value()?);
        }
        let mut rev = p.revision("rebuild", &now, &run);
        rev.stamp(&root, p.pre.as_deref(), true)?;
        op.append(REVISIONS_REL, json(&rev)?);
        op.file(&p.id.file_name(), p.pre.clone(), Some(p.post.clone()));
        let cleared = match p.outcome {
            PutOutcome::Updated {
                verifications_cleared,
                ..
            } => verifications_cleared,
            _ => 0,
        };
        report.verifications_cleared += cleared;
        named.push(format!("`/{}` ({cleared} cleared)", p.id.file_name()));
        report.rebuilt.push(p.id.clone());
        report.findings.extend(p.findings.iter().cloned());
    }
    let text = format!(
        "**Rebuilt** {} concept(s) from store contents: {}; {} verification event(s) moved to history",
        plans.len(),
        named.join(", "),
        report.verifications_cleared
    );
    let entry = prepare_entry(&root, &now, &text)?;
    let old_log = String::from_utf8_lossy(entry.old.as_deref().unwrap_or_default()).into_owned();
    report.log_added = insert(&old_log, &now.format("%Y-%m-%d").to_string(), &text).1;
    // §4.5 step 4: `log.md` is renamed last, after the same pre-image check.
    op.file("log.md", entry.old, Some(entry.text.into_bytes()));
    bundle.check_recovery(&op.file_paths())?;
    report.findings.extend(ops::run(&mut bundle, op)?);
    Ok(report)
}
