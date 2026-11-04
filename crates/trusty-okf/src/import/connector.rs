//! The import engine's connector-store steps (SPEC-OKF-01 §2.13, §2.15).
//!
//! Why: a connector item reaches the same engine as a tree file (§2.13 "One
//! engine"), but its ID, change test and body come from the adapter record,
//! not from a path and its bytes. Keeping those steps here leaves `plan` and
//! `apply` one flow for both store kinds.
//! What: [`plan_entry`] plans one listed connector item: a `deleted` item
//! (docs `trashed`) is a tombstone on a deletion-safe run, else `missing`;
//! the ID is the ledger pin, else the Appendix C rule; an item whose listed
//! revision equals its ledger record is `unchanged` without a fetch (DOC-55
//! C2-C3), and only an item with no revision is fetched to fingerprint its
//! record. [`prepare`] turns a fetched record into the source item the
//! ingest engine writes, or into the item's report (`stale_plan`,
//! `snippet_only`, `unsupported_format`). [`write_threads`] rewrites the
//! thread concept of every mail thread the run wrote, listing its messages
//! in date order with bundle-absolute links (§2.15 item 6). Slice S9 of
//! #10746.
//! Test: `c87_three_pages_import_and_a_ceiling_tombstones_nothing`,
//! `c89_mail_keeps_thread_and_dates_and_links_the_thread`,
//! `c90_unchanged_mailbox_writes_nothing_and_a_wider_window_fetches_only_new`,
//! `c94_trashed_item_on_a_complete_recursive_listing_is_tombstoned`.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};

use super::apply::IMPORT_ACTOR;
use super::plan::{Action, ImportSpec, PlannedItem, SKIP_CODES, error_code, ownership, set};
use super::record::{ConnectorKind, Normalised, SourceRecord, concept_id, normalise, thread_of};
use super::report::{ImportFinding, ItemReport};
use super::source::{ImportSource, ListEntry};
use crate::OkfError;
use crate::build::ingest::{SourceItem, Writer, write_item};
use crate::build::ledger::{ItemStatus, Ledger};
use crate::build::registry::SourceSpec;
use crate::bundle::Bundle;
use crate::bundle::log_md::LogBatch;
use crate::format::hash::bytes_hash;
use crate::format::id::ConceptId;
use crate::profile::Profile;
use crate::state::revisions::RevisionLine;

const REV_PREFIX: &str = "rev:";

fn revision_fingerprint(family: ConnectorKind, rev: &str) -> String {
    format!("{REV_PREFIX}{rev}/{}", family.tag())
}

fn bytes_fingerprint(family: ConnectorKind, bytes: &[u8]) -> String {
    format!("{}/{}", bytes_hash(bytes), family.tag())
}

/// The planning context of one connector run.
pub(crate) struct PlanCtx<'a> {
    pub family: ConnectorKind,
    pub spec: &'a ImportSpec,
    pub ledger: &'a Ledger,
    pub lines: &'a [RevisionLine],
    pub root: &'a Path,
    pub deletion_safe: bool,
}

/// Plan one listed connector item into `p` (see the module doc).
pub(crate) fn plan_entry(
    cx: &PlanCtx<'_>,
    src: &mut dyn ImportSource,
    e: &ListEntry,
    p: &mut PlannedItem,
) {
    p.extractor = Some(cx.family.tag());
    let rec = cx.ledger.get(&e.item_id);
    if e.deleted {
        match rec {
            Some(r) if r.status == ItemStatus::Ingested => {
                p.id = ConceptId::parse(&r.entity).ok();
                let action = if cx.deletion_safe {
                    Action::Tombstone
                } else {
                    Action::Missing
                };
                set(p, action, None);
            }
            _ => set(p, Action::Skip, Some("excluded")),
        }
        return;
    }
    let id = match rec {
        Some(r) => ConceptId::parse(&r.entity),
        None => concept_id(cx.family, &cx.spec.store.id, e),
    };
    let Ok(id) = id else {
        set(p, Action::Fail, Some("invalid_id"));
        return;
    };
    p.id = Some(id.clone());
    let fp = match &e.revision {
        Some(rev) => revision_fingerprint(cx.family, rev),
        None => match src.fetch(&e.item_id) {
            Ok(bytes) => bytes_fingerprint(cx.family, &bytes),
            Err(err) => {
                let skip = SKIP_CODES.contains(&err.code.as_str());
                let action = if skip { Action::Skip } else { Action::Fail };
                set(p, action, Some(&err.code));
                return;
            }
        },
    };
    let current = rec.is_some_and(|r| r.status == ItemStatus::Ingested && r.fingerprint == fp);
    p.fingerprint = Some(fp);
    match ownership(cx.root, cx.lines, &id) {
        _ if current => set(p, Action::Unchanged, None),
        Ok(Ok(a)) => set(p, a, None),
        Ok(Err(why)) => set(p, Action::Conflict, Some(why)),
        Err(e) => set(p, Action::Fail, Some(error_code(&e))),
    }
}

/// A fetched connector item ready for the ingest engine.
pub(crate) struct Prepared {
    pub item: SourceItem,
    pub provenance: Mapping,
    pub notes: Vec<String>,
    pub degraded: Option<String>,
}

/// The source item of a fetched record, or its report (see the module doc).
pub(crate) fn prepare(
    family: ConnectorKind,
    spec: &ImportSpec,
    p: &PlannedItem,
    id: &ConceptId,
    bytes: &[u8],
) -> Result<Prepared, ItemReport> {
    let report = |read: &str, write: &str, why: &str| {
        ItemReport::new(&p.item_id, p.id.clone(), read, write).because(why)
    };
    let Ok(rec) = serde_json::from_slice::<SourceRecord>(bytes) else {
        return Err(report("failed", "none", "protocol_error"));
    };
    let planned = p.fingerprint.clone().unwrap_or_default();
    let fp = match planned.strip_prefix(REV_PREFIX) {
        Some(_) => match &rec.revision {
            Some(rev) if revision_fingerprint(family, rev) == planned => planned,
            _ => return Err(report("failed", "none", "stale_plan")),
        },
        None => {
            let fp = bytes_fingerprint(family, bytes);
            if !planned.is_empty() && planned != fp {
                return Err(report("failed", "none", "stale_plan"));
            }
            fp
        }
    };
    match normalise(family, &rec, spec.policy) {
        Normalised::Skip(why) => Err(report("skipped", "none", &why)),
        Normalised::Degraded(why) => {
            let mut r = report("read", "none", &why);
            r.degraded = Some(why);
            Err(r)
        }
        Normalised::Concept {
            title,
            body,
            timestamp,
            provenance,
            fields,
            degraded,
            notes,
        } => Ok(Prepared {
            item: SourceItem {
                item_id: p.item_id.clone(),
                fingerprint: fp,
                name: id.as_str().to_string(),
                title,
                timestamp: timestamp.or_else(|| p.timestamp.clone()),
                body,
                fields: Default::default(),
                frontmatter: fields,
                volatile: false,
            },
            provenance,
            notes,
            degraded,
        }),
    }
}

/// The body of a thread concept: its messages in date order, each a
/// bundle-absolute link.
fn thread_body(ledger: &Ledger, thread: &ConceptId) -> String {
    let prefix = format!("{}/", thread.as_str());
    let mut msgs: Vec<(&str, &str, &str)> = ledger
        .records()
        .filter(|r| r.status == ItemStatus::Ingested && r.entity.starts_with(&prefix))
        .map(|r| {
            let ts = r.timestamp.as_deref().unwrap_or("");
            (ts, r.item_id.as_str(), r.entity.as_str())
        })
        .collect();
    msgs.sort();
    let mut body = String::from("Messages in date order:\n");
    for (ts, item, entity) in msgs {
        body.push_str(&format!("\n- {ts} [{item}](/{entity}.md)"));
    }
    body
}

/// What [`write_threads`] needs from the run.
pub(crate) struct ThreadRun<'a> {
    pub store: &'a SourceSpec,
    pub ledger: &'a Ledger,
    pub lines: &'a [RevisionLine],
    pub now: &'a DateTime<FixedOffset>,
    pub batch: &'a LogBatch,
}

/// Rewrite the thread concept of each mail item this run wrote. A thread
/// concept that is not engine-owned is left alone and reported.
pub(crate) fn write_threads(
    b: &mut Bundle,
    run: &ThreadRun<'_>,
    touched: &BTreeSet<ConceptId>,
    findings: &mut Vec<ImportFinding>,
) -> Result<(), OkfError> {
    let profile = Profile::default_profile();
    for thread in touched {
        let finding = |kind: &str, detail: &str| ImportFinding {
            severity: "warning".into(),
            kind: kind.into(),
            item_id: thread.as_str().into(),
            detail: detail.into(),
        };
        match ownership(b.root(), run.lines, thread)? {
            Ok(_) => {}
            Err(why) => {
                findings.push(finding("thread_conflict", why));
                continue;
            }
        }
        let tid = thread.as_str().rsplit('/').next().unwrap_or_default();
        let body = thread_body(run.ledger, thread);
        let mut provenance = Mapping::new();
        provenance.insert(Value::String("thread_id".into()), Value::String(tid.into()));
        let item = SourceItem {
            item_id: format!("thread:{tid}"),
            fingerprint: bytes_hash(body.as_bytes()),
            name: thread.as_str().to_string(),
            title: format!("Thread {tid}"),
            body,
            ..SourceItem::default()
        };
        let writer = Writer {
            actor: IMPORT_ACTOR,
            provenance: &provenance,
        };
        if let Err(e) = write_item(
            b, &profile, run.store, thread, &item, run.now, run.batch, &writer,
        ) {
            findings.push(finding("thread_write_failed", error_code(&e)));
        }
    }
    Ok(())
}

/// The thread concept of a written mail concept.
pub(crate) fn touched_thread(family: Option<ConnectorKind>, id: &ConceptId) -> Option<ConceptId> {
    if family == Some(ConnectorKind::Mail) {
        thread_of(id)
    } else {
        None
    }
}
