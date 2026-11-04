//! `plan_import`: list, assign IDs, pick extractors and record every
//! refusal, writing nothing (SPEC-OKF-01 §2.14 rules 3, 4, 6-9, §4.10
//! phase 1).
//!
//! Why: an operator previews an import before it writes, and the apply
//! phase needs the same decisions. A plan that wrote, or that guessed past a
//! collision or a hand edit, would break rules 1, 4 and 7.
//! What: [`plan_import`] refuses a run before reading anything when the
//! store needs a child process the spec forbids (`policy_refused`, §2.15)
//! or when the bundle and source roots overlap (`SourceOverlap`). It reads
//! every page of the listing (a repeated cursor, or a cursor still left at
//! [`MAX_LIST_PAGES`], ends it as incomplete; one page with `complete:
//! false` makes the whole listing incomplete), computes
//! `pagination_exhausted` and
//! `deletion_safe` itself (§2.13), then plans each entry: a listed skip,
//! an `excluded` glob, a worker item under `child_processes: false`
//! (`policy_refused`), an ID (pinned by the ledger, else §2.14 rule 3), the
//! DOC-55 fingerprint, and the action: `unchanged` when the ledger is
//! current; `create` or `update` when the disk bytes are engine-owned or
//! absent; `conflict` with the §4.5 reason otherwise. Colliding items fail
//! with `collision`; ledger items absent from the listing are `tombstone`
//! on a deletion-safe run, else `missing`. Reads only. Slice S8 of #10746.
//! Test: `c72_plan_writes_nothing_and_lists_every_item`,
//! `c75_colliding_sources_are_refused_and_pins_hold`,
//! `c79_vanished_files_tombstone_only_when_deletion_safe`,
//! `c82_overlapping_roots_are_refused`,
//! `repeating_or_endless_cursors_end_the_listing_incomplete`,
//! `any_incomplete_page_blocks_tombstones`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Serialize;

use super::connector::{PlanCtx, plan_entry};
use super::id::concept_id_for_source;
use super::record::{check_selection, deletion_capable};
use super::source::ImportSource;
use crate::OkfError;
use crate::build::collide::{self, Pins};
use crate::build::ledger::{ItemStatus, Ledger};
use crate::build::rebuild::{Owner, owner};
use crate::build::registry::{Locator, SourceSpec};
use crate::bundle::Bundle;
use crate::bundle::paths::read_rel;
use crate::extract::{BuiltinText, Extractor, TypeHint, fingerprint};
use crate::format::hash::bytes_hash;
use crate::format::id::ConceptId;
use crate::state::revisions::{self, run_id};

/// #10746 S9 fix round: the most pages one listing reads. A source still
/// returning a cursor at this page ends the listing as incomplete.
pub const MAX_LIST_PAGES: usize = 10_000;

/// The run's item ceiling when a caller names none.
pub const DEFAULT_MAX_ITEMS: usize = 100_000;

/// What an import copies from a source (§2.15, 3k A). A document tree has
/// one policy: the full text of each selected item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentPolicy {
    /// The full body text, no snippet in place of a body (3k A).
    #[default]
    FullText,
    /// #10746 S9: the full body text; a mail item with no decodable text
    /// part writes its snippet, flagged `degraded: snippet_only` (§2.15
    /// item 11, a per-store policy that allows snippets).
    AllowSnippet,
}

/// One import run's request (SPEC-OKF-01 §4.2 `ImportSpec`).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportSpec {
    /// The store the run registers and ingests.
    pub store: SourceSpec,
    /// Globs an item must match, against its source-relative path; empty
    /// admits every item.
    pub include: Vec<String>,
    /// Globs that exclude an item (`excluded`).
    pub exclude: Vec<String>,
    /// The run's item ceiling; a listing that reaches it is not exhausted.
    pub max_items: usize,
    /// The content policy (3k A).
    pub policy: ContentPolicy,
    /// False for a sub-agent: the run starts no child process (§2.15, 3 A).
    pub child_processes: bool,
    /// Whether an operator started the run (§2.15 "Inbound email").
    pub operator_initiated: bool,
}

impl ImportSpec {
    /// A spec for `store` with the defaults: no globs,
    /// [`DEFAULT_MAX_ITEMS`], full text, child processes allowed, not
    /// operator-initiated.
    pub fn new(store: SourceSpec) -> ImportSpec {
        ImportSpec {
            store,
            include: Vec::new(),
            exclude: Vec::new(),
            max_items: DEFAULT_MAX_ITEMS,
            policy: ContentPolicy::FullText,
            child_processes: true,
            operator_initiated: false,
        }
    }
}

/// What the plan intends for one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// A new concept.
    Create,
    /// A changed item over an engine-owned concept.
    Update,
    /// The ledger is current; nothing is written.
    Unchanged,
    /// The item vanished on a deletion-safe listing.
    Tombstone,
    /// The item vanished on a listing that is not deletion-safe.
    Missing,
    /// Not read (§2.14 rule 8 reason).
    Skip,
    /// Read or addressed, but refused (`collision`, `invalid_id`, a fetch
    /// error).
    Fail,
    /// The concept's bytes are not engine-owned (§4.5 reason).
    Conflict,
}

/// One planned item (§2.14 rule 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedItem {
    /// The source's item ID.
    pub item_id: String,
    /// The concept, when one was assigned.
    pub id: Option<ConceptId>,
    /// What apply does.
    pub action: Action,
    /// The skip, failure or conflict reason.
    pub reason: Option<String>,
    /// `<id>@<version>` of the extractor, or `worker`.
    pub extractor: Option<String>,
    /// The DOC-55 fingerprint of the bytes the plan read (built-in tier).
    pub fingerprint: Option<String>,
    /// The item's own timestamp.
    pub timestamp: Option<String>,
    /// A vanished item with equal bytes: a possible move (rule 7).
    pub possible_move_of: Option<String>,
}

/// A whole plan (SPEC-OKF-01 §4.2 `ImportPlan`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportPlan {
    /// The plan's run ID, derived from its content.
    pub run_id: String,
    /// The store ID.
    pub store: String,
    /// The request; `import` re-checks it.
    #[serde(skip)]
    pub spec: ImportSpec,
    /// Every listed entry and every vanished ledger item, once each.
    pub items: Vec<PlannedItem>,
    /// Entries the source returned.
    pub listed: usize,
    /// The engine's own finding that no page remained under the ceiling.
    pub pagination_exhausted: bool,
    /// Tombstones are allowed (§2.13).
    pub deletion_safe: bool,
    /// Where a resumed listing starts, when one remained.
    pub cursor: Option<String>,
}

/// Run-level refusals, before anything is read or written.
pub(crate) fn preflight(bundle_root: &Path, spec: &ImportSpec) -> Result<(), OkfError> {
    match &spec.store.locator {
        Locator::Connector { kind, .. } if !spec.child_processes => Err(OkfError::PolicyRefused {
            path: bundle_root.to_path_buf(),
            reason: format!(
                "policy_refused: a {kind} store runs through an adapter process, and this run \
                 allows no child process"
            ),
        }),
        // #10746 S9: an authenticated source needs an account and a
        // selection (§2.15 item 1, criterion 86).
        Locator::Connector { .. } => check_selection(spec).map(|_| ()),
        Locator::DocStore { path, .. } => {
            let canon = |p: &Path| {
                p.canonicalize()
                    .map_err(|e| crate::bundle::paths::io_err(p, e))
            };
            let source_root = canon(Path::new(path))?;
            let bundle_root = canon(bundle_root)?;
            if source_root.starts_with(&bundle_root) || bundle_root.starts_with(&source_root) {
                return Err(OkfError::SourceOverlap {
                    source_root,
                    bundle_root,
                });
            }
            Ok(())
        }
    }
}

/// A glob set; an invalid pattern is a `Parse` error before any read.
pub(crate) fn glob_set(patterns: &[String]) -> Result<GlobSet, OkfError> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let glob = GlobBuilder::new(p)
            .literal_separator(true)
            .build()
            .map_err(|e| OkfError::Parse {
                path: PathBuf::from(p),
                line: None,
                message: format!("invalid glob: {e}"),
            })?;
        b.add(glob);
    }
    b.build().map_err(|e| OkfError::Parse {
        path: PathBuf::from("globs"),
        line: None,
        message: e.to_string(),
    })
}

/// The extractor tag the plan and the report show.
fn tag(ex: &dyn Extractor) -> String {
    format!("{}@{}", ex.id(), ex.version())
}

fn sha_of(fp: &str) -> &str {
    fp.split('/').next().unwrap_or(fp)
}

fn item(item_id: &str, timestamp: Option<String>) -> PlannedItem {
    PlannedItem {
        item_id: item_id.to_string(),
        id: None,
        action: Action::Skip,
        reason: None,
        extractor: None,
        fingerprint: None,
        timestamp,
        possible_move_of: None,
    }
}

pub(crate) fn set(p: &mut PlannedItem, action: Action, reason: Option<&str>) {
    p.action = action;
    p.reason = reason.map(str::to_string);
}

/// Codes a fetch reports that are skips, not failures (§2.14 rule 8).
pub(crate) const SKIP_CODES: [&str; 10] = [
    "excluded",
    "hidden",
    "symlink",
    "too_large",
    "binary_no_extractor",
    "unsupported_format",
    "worker_absent",
    "worker_incompatible",
    "policy_refused",
    "sandbox_required",
];

/// A short reason code for an error that fails one item.
pub(crate) fn error_code(e: &OkfError) -> &'static str {
    match e {
        OkfError::PathEscape(_) => "path_escape",
        OkfError::Conflict { .. } => "conflict",
        OkfError::RecoveryPending { .. } => "recovery_pending",
        OkfError::Conformance(_) => "conformance",
        OkfError::InvalidId { .. } => "invalid_id",
        OkfError::Parse { .. } => "parse_error",
        _ => "io_error",
    }
}

/// The §4.5 action for a write to `id`: create or update when the disk
/// bytes are engine-owned or absent, else the conflict reason.
pub(crate) fn ownership(
    root: &Path,
    lines: &[revisions::RevisionLine],
    id: &ConceptId,
) -> Result<Result<Action, &'static str>, OkfError> {
    let disk = read_rel(root, &id.file_name())?;
    Ok(match (owner(lines, id, disk.as_deref()), disk.is_some()) {
        (Owner::Engine, true) => Ok(Action::Update),
        (Owner::Engine | Owner::Unproven, false) => Ok(Action::Create),
        (Owner::Caller, _) => Err("caller_edit"),
        (Owner::HandEdit, _) => Err("hand_edit"),
        (Owner::Unproven, true) => Err("no_written_hash"),
    })
}

/// Plan one import (SPEC-OKF-01 §4.3 `plan_import`). Writes nothing.
///
/// Why/What: see the module doc. Errors: the run-level refusals, a listing
/// failure, an invalid glob, or a bundle read failure; per-item problems
/// are planned items.
pub fn plan_import(
    bundle: &Bundle,
    spec: &ImportSpec,
    src: &mut dyn ImportSource,
) -> Result<ImportPlan, OkfError> {
    let root = bundle.root();
    preflight(root, spec)?;
    let (include, exclude) = (glob_set(&spec.include)?, glob_set(&spec.exclude)?);
    let mut entries = Vec::new();
    let mut cursor: Option<String> = None;
    let mut complete = true;
    // #10746 S9 fix round: every cursor asked for so far, and the page
    // count, so a repeating or endless cursor sequence ends the listing.
    let mut seen = std::collections::BTreeSet::new();
    let mut pages = 0usize;
    loop {
        pages += 1;
        let page = src.list(cursor.as_deref())?;
        // #10746 S9: Architect ruling 440388fb9a26 (option B, §2.13): one
        // page that says incomplete makes the whole listing incomplete.
        complete &= page.complete;
        let room = spec.max_items.saturating_sub(entries.len());
        let cut = page.entries.len() > room;
        entries.extend(page.entries.into_iter().take(room));
        let stalled = page
            .cursor
            .as_ref()
            .is_some_and(|c| !seen.insert(c.clone()))
            || (pages >= MAX_LIST_PAGES && page.cursor.is_some());
        // #10746 S9: a page cut at the ceiling is listed again on resume,
        // so the reported cursor is the one that asked for it; the ledger
        // skips the items already taken (§2.15 item 9).
        if !cut {
            cursor = page.cursor;
        }
        // #10746 S9: a listing that fills the ceiling stops there, so a
        // page token left over is reported, never followed (criterion 132).
        let full = entries.len() >= spec.max_items;
        if cut || full || stalled || cursor.is_none() {
            complete &= !stalled;
            break;
        }
    }
    let listed = entries.len();
    let exhausted = complete && cursor.is_none() && listed < spec.max_items;
    // #10746 S9: a recursive docs listing can detect deletions too; mail
    // never can (Appendix C "Complete listing").
    let deletion_safe = exhausted && deletion_capable(spec) && spec.store.tombstone_deleted;
    let family = check_selection(spec)?;
    let ledger = Ledger::load(root, &spec.store.id)?;
    let lines = revisions::read(root)?;
    let cx = family.map(|family| PlanCtx {
        family,
        spec,
        ledger: &ledger,
        lines: &lines,
        root,
        deletion_safe,
    });
    let mut items = Vec::new();
    let mut shas: BTreeMap<String, String> = BTreeMap::new();
    for e in &entries {
        let mut p = item(&e.item_id, e.timestamp.clone());
        if let Some(r) = &e.skip {
            set(&mut p, Action::Skip, Some(r));
            items.push(p);
            continue;
        }
        let path = Path::new(&e.item_id);
        if (!spec.include.is_empty() && !include.is_match(path)) || exclude.is_match(path) {
            set(&mut p, Action::Skip, Some("excluded"));
            items.push(p);
            continue;
        }
        if let Some(cx) = &cx {
            plan_entry(cx, src, e, &mut p);
            items.push(p);
            continue;
        }
        let hint = TypeHint::for_path(&e.item_id);
        let builtin = BuiltinText::handles(&hint);
        p.extractor = Some(if builtin {
            tag(&BuiltinText)
        } else {
            "worker".into()
        });
        if !builtin && !spec.child_processes {
            set(&mut p, Action::Skip, Some("policy_refused"));
            items.push(p);
            continue;
        }
        let id = match ledger.get(&e.item_id) {
            Some(r) => ConceptId::parse(&r.entity),
            None => concept_id_for_source(&e.item_id),
        };
        let id = match id {
            Ok(id) => id,
            Err(_) => {
                set(&mut p, Action::Fail, Some("invalid_id"));
                items.push(p);
                continue;
            }
        };
        p.id = Some(id.clone());
        let bytes = match src.fetch(&e.item_id) {
            Ok(b) => b,
            Err(err) => {
                let skip = SKIP_CODES.contains(&err.code.as_str());
                set(
                    &mut p,
                    if skip { Action::Skip } else { Action::Fail },
                    Some(&err.code),
                );
                items.push(p);
                continue;
            }
        };
        let sha = bytes_hash(&bytes);
        shas.insert(e.item_id.clone(), sha.clone());
        let current = match ledger.get(&e.item_id) {
            Some(r) if r.status == ItemStatus::Ingested => {
                if builtin {
                    r.fingerprint == fingerprint(&bytes, &BuiltinText)
                } else {
                    sha_of(&r.fingerprint) == sha
                }
            }
            _ => false,
        };
        if builtin {
            p.fingerprint = Some(fingerprint(&bytes, &BuiltinText));
        }
        match ownership(root, &lines, &id) {
            _ if current => set(&mut p, Action::Unchanged, None),
            Ok(Ok(a)) => set(&mut p, a, None),
            Ok(Err(why)) => set(&mut p, Action::Conflict, Some(why)),
            Err(e) => set(&mut p, Action::Fail, Some(error_code(&e))),
        }
        items.push(p);
    }
    let targets: Vec<(String, ConceptId)> = items
        .iter()
        .filter(|p| {
            matches!(
                p.action,
                Action::Create | Action::Update | Action::Unchanged | Action::Conflict
            )
        })
        .filter_map(|p| p.id.clone().map(|id| (p.item_id.clone(), id)))
        .collect();
    let refused = collide::refused(&spec.store.id, &targets, &Pins::load(root, &spec.store.id)?);
    for p in items.iter_mut().filter(|p| refused.contains(&p.item_id)) {
        set(p, Action::Fail, Some("collision"));
    }
    let listed_ids: std::collections::BTreeSet<&str> =
        entries.iter().map(|e| e.item_id.as_str()).collect();
    let mut vanished = Vec::new();
    for gone in ledger.live_item_ids() {
        if listed_ids.contains(gone.as_str()) {
            continue;
        }
        let rec = ledger.get(&gone);
        let mut p = item(&gone, rec.and_then(|r| r.timestamp.clone()));
        p.id = rec.and_then(|r| ConceptId::parse(&r.entity).ok());
        let action = if deletion_safe {
            Action::Tombstone
        } else {
            Action::Missing
        };
        set(&mut p, action, None);
        if let Some(r) = rec {
            let gone_sha = sha_of(&r.fingerprint).to_string();
            for created in items.iter_mut().filter(|c| c.action == Action::Create) {
                if shas.get(&created.item_id) == Some(&gone_sha) {
                    created.possible_move_of = Some(gone.clone());
                }
            }
        }
        vanished.push(p);
    }
    items.extend(vanished);
    let digest: Vec<String> = items
        .iter()
        .map(|p| {
            format!(
                "{}={:?}:{}",
                p.item_id,
                p.action,
                p.fingerprint.as_deref().unwrap_or("")
            )
        })
        .collect();
    let digest = digest.join("\n");
    Ok(ImportPlan {
        run_id: run_id("import", &[&spec.store.id, &digest]),
        store: spec.store.id.clone(),
        spec: spec.clone(),
        items,
        listed,
        pagination_exhausted: exhausted,
        deletion_safe,
        cursor,
    })
}
