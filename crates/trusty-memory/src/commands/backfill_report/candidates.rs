//! Read-only drawer census, joined to injection frequency and ranked.
//!
//! Why: ADR-0028 does not migrate the drawers already on disk — §"What this does
//! not fix" is explicit that tier assignment for them is a classification
//! problem tags cannot settle, so Migration step 3 makes backfill *read-only and
//! human-gated*. This module is the read half of that gate: it produces the
//! evidence a human needs to decide one drawer at a time, and it deliberately
//! produces no decision.
//!
//! What: enumerates palaces, opens each palace's KG store **read-only**, reads
//! the drawer table, renders each drawer's preview exactly as the injection
//! pipeline would, looks that preview up in the hook-log index, and ranks by the
//! resulting injection count.
//!
//! The read-only guarantee is structural, and two separate write paths had to be
//! closed to make it true rather than merely intended:
//!
//! 1. **No `PalaceHandle`.** `PalaceHandle::open_with_intent` deletes every
//!    drawer whose `expires_at` has passed (`handle.rs`, issue #61) as a side
//!    effect of opening. A report built on it would retire drawers merely by
//!    being run — the one outcome ADR-0028's human-gated migration forbids. It
//!    would also delete the rows a human most wants to see.
//! 2. **No open against the live file.** Going one level lower to
//!    [`KgStoreRedb`] is not enough on its own: `OpenIntent::ReadOnlyClient`
//!    only snapshots when the file is *already locked*. On an unlocked palace it
//!    reaches `Database::create`, which opens read-write and runs a table-init
//!    write transaction — and on a file in an incompatible redb format it
//!    *renames the palace's store aside* and creates a fresh empty one
//!    (`concurrent_open.rs`, issue #702). So this module copies each store to a
//!    private temporary file and opens the copy. Whatever redb does on open —
//!    init transaction, recovery, or that recreate — lands on a throwaway.
//!
//! Test: `ranks_by_injection_count`, `zero_injection_drawers_rank_last`,
//! `report_writes_nothing_to_the_palace`,
//! `incompatible_store_is_reported_not_recreated`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use trusty_common::memory_core::decay::DecayConfig;
use trusty_common::memory_core::palace::Drawer;
use trusty_common::memory_core::store::kg_redb::KgStoreRedb;
use trusty_common::memory_core::store::rooms::list_room_summaries;
use trusty_common::memory_core::store::OpenIntent;
use trusty_common::memory_core::PalaceRegistry;

use super::log_index::InjectionIndex;
use super::signals::Signal;
use crate::commands::prompt_context::format::drawer_preview;

/// One drawer's evidence row.
///
/// Why: the ticket's actionability bar is that a human can triage a single
/// drawer from one row — which needs identity, enough content to recognise it,
/// how old it is, how much it costs, and how privileged it currently is.
/// What: every field is measured or read, none inferred. `injections` and
/// `share_of_turns` come from the hook logs; the rest from the drawer row.
#[derive(Debug, Clone)]
pub struct CandidateRow {
    pub palace: String,
    pub drawer_id: uuid::Uuid,
    pub room: String,
    /// Whitespace-collapsed content, exactly as the injection renders it.
    pub excerpt: String,
    pub age_days: f32,
    /// Turns this drawer's preview reached, over the scanned log window.
    pub injections: u64,
    /// `injections` as a fraction of the palace's total injections.
    pub share_of_turns: f64,
    /// Stored `importance`, the field §C5 identifies as the working privilege dial.
    pub importance: f32,
    /// `importance` after the 90-day-half-life decay §C7 blames for the staleness.
    pub effective_importance: f32,
    /// Whether an `expires_at` is already set — i.e. already triaged.
    pub has_expiry: bool,
    /// Objective observations about this drawer. Not a classification.
    pub signals: Vec<Signal>,
}

/// What one palace's read produced, including a failure that did not stop the run.
#[derive(Debug, Clone)]
pub struct PalaceOutcome {
    pub palace: String,
    pub drawers_read: usize,
    pub error: Option<String>,
}

/// Everything the report needs to print.
#[derive(Debug, Default)]
pub struct Census {
    pub rows: Vec<CandidateRow>,
    pub outcomes: Vec<PalaceOutcome>,
    /// Drawers read across every palace, before any filtering.
    pub drawers_total: usize,
}

/// Build the ranked census.
///
/// Why: this is the whole report, minus rendering. Keeping it a pure function of
/// (registry dir, log index, filters) makes the read-only property testable —
/// a test can point it at a fixture palace and assert the files are unchanged.
/// What: for each palace (optionally filtered), reads drawers read-only, joins
/// each to its injection count, drops rows below `min_injections`, and sorts by
/// injections descending with the oldest drawer breaking ties. One palace
/// failing to open is recorded in `outcomes` and skipped — a single locked or
/// corrupt palace must not deny the operator the other 92.
/// Test: `ranks_by_injection_count`, `min_injections_filters`,
/// `unreadable_palace_is_recorded_not_fatal`.
pub fn build_census(
    registry_dir: &Path,
    index: &InjectionIndex,
    palace_filter: Option<&str>,
    min_injections: u64,
) -> Result<Census> {
    let palaces = PalaceRegistry::list_palaces(registry_dir)
        .with_context(|| format!("list palaces under {}", registry_dir.display()))?;
    let mut census = Census::default();
    let decay = DecayConfig::default();
    let now = Utc::now();

    for palace in palaces {
        let slug = palace.id.0.clone();
        if palace_filter.is_some_and(|f| f != slug) {
            continue;
        }
        match read_palace_drawers(&palace.data_dir) {
            Ok((drawers, rooms)) => {
                census.outcomes.push(PalaceOutcome {
                    palace: slug.clone(),
                    drawers_read: drawers.len(),
                    error: None,
                });
                census.drawers_total += drawers.len();
                let total = index.total_injections(&slug);
                for drawer in &drawers {
                    let excerpt = drawer_preview(&drawer.content);
                    let injections = index.injections_for(&slug, &excerpt);
                    if injections < min_injections {
                        continue;
                    }
                    let age_days = DecayConfig::age_days(drawer.created_at);
                    census.rows.push(CandidateRow {
                        palace: slug.clone(),
                        drawer_id: drawer.id,
                        room: room_label(&rooms, drawer),
                        excerpt,
                        age_days,
                        injections,
                        share_of_turns: if total == 0 {
                            0.0
                        } else {
                            injections as f64 / total as f64
                        },
                        importance: drawer.importance,
                        effective_importance: decay.effective_importance(
                            drawer.importance,
                            age_days,
                            0.0,
                        ),
                        has_expiry: drawer.expires_at.is_some(),
                        signals: super::signals::observe(drawer, age_days, now),
                    });
                }
            }
            Err(e) => census.outcomes.push(PalaceOutcome {
                palace: slug,
                drawers_read: 0,
                error: Some(format!("{e:#}")),
            }),
        }
    }

    census.rows.sort_by(|a, b| {
        b.injections
            .cmp(&a.injections)
            .then_with(|| b.age_days.total_cmp(&a.age_days))
    });
    Ok(census)
}

/// Filename of a palace's KG store.
const KG_FILE: &str = "kg.redb";

/// Suffix `redb_open::backup_incompatible_file` gives a store it moves aside.
const INCOMPATIBLE_SUFFIX: &str = ".v2-incompatible";

/// Read one palace's drawer table and rooms without touching its store.
///
/// Why: see the module doc — this is the one place the read-only guarantee is
/// made, and it is made by never handing the palace's own file to redb.
/// What: copies `<data_dir>/kg.redb` into a private temp dir, opens the copy,
/// and performs two reads. The `TempDir` drops at return, taking the copy and
/// anything redb wrote beside it. A store in an incompatible format is detected
/// by the backup redb leaves next to the copy and reported as an error rather
/// than silently reading as an empty palace.
/// Test: `report_writes_nothing_to_the_palace`,
/// `incompatible_store_is_reported_not_recreated`.
fn read_palace_drawers(
    data_dir: &Path,
) -> Result<(
    Vec<Drawer>,
    Vec<trusty_common::memory_core::store::rooms::RoomSummary>,
)> {
    let live = data_dir.join(KG_FILE);
    if !live.exists() {
        // A palace directory with no KG store has no drawers to report. That is
        // an empty palace, not a failure.
        return Ok((Vec::new(), Vec::new()));
    }
    // #4891: read a copy, never the live file. `OpenIntent::ReadOnlyClient`
    // alone does NOT prevent writes — it only snapshots when the file is
    // already locked, and otherwise reaches `Database::create`, which runs an
    // init write txn and can rename an incompatible store aside.
    let scratch = tempfile::tempdir().context("create scratch dir for read-only palace copy")?;
    let copy = scratch.path().join(KG_FILE);
    std::fs::copy(&live, &copy)
        .with_context(|| format!("copy {} for read-only inspection", live.display()))?;

    let store = KgStoreRedb::open_with_intent(&copy, OpenIntent::ReadOnlyClient)
        .with_context(|| format!("open copy of KG store {}", live.display()))?;
    if scratch
        .path()
        .join(format!("{KG_FILE}{INCOMPATIBLE_SUFFIX}"))
        .exists()
    {
        anyhow::bail!(
            "KG store at {} is in an incompatible redb format — reading it would have \
             required recreating it, which this report never does. Rebuild the palace.",
            live.display()
        );
    }
    let drawers = store.load_drawers().context("load drawers")?;
    let store = Arc::new(store);
    let rooms = list_room_summaries(&store).unwrap_or_default();
    Ok((drawers, rooms))
}

/// Resolve a drawer's room label, falling back to a short id.
///
/// ADR-0027 D1.3: labels are read from the ROOMS table, never recomputed. A
/// drawer whose room predates the registry has no row, so the id stands in.
fn room_label(
    rooms: &[trusty_common::memory_core::store::rooms::RoomSummary],
    drawer: &Drawer,
) -> String {
    rooms
        .iter()
        .find(|r| r.id == drawer.room_id)
        .map(|r| r.label.clone())
        .unwrap_or_else(|| short_id(&drawer.room_id))
}

/// First 8 characters of a UUID — enough to identify a drawer in conversation,
/// which is how the ADR itself refers to them (`drawer f59fb536`).
pub fn short_id(id: &uuid::Uuid) -> String {
    id.to_string().chars().take(8).collect()
}
