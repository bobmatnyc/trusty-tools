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

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
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
///
/// What: every field is read from the drawer row or measured from the hook logs,
/// with one qualification the reader must be able to see. `injections` is keyed
/// on the 220-char `excerpt`, so if two drawers in one palace truncate to the
/// same excerpt they are indistinguishable in the logs and both receive the
/// combined count — misattributed to each, not split between them. That case is
/// never left silent: [`collision_peers`](Self::collision_peers) is `Some` on
/// every row it affects and [`content_digest`](Self::content_digest)
/// distinguishes rows whose excerpts read identically. The live estate has zero
/// collisions across 2,287 rows today; near-duplicate session checkpoints (§C5)
/// are the content most likely to converge on a shared prefix as it grows.
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
    /// Other drawers in this palace whose `excerpt` is byte-identical to this
    /// one. `None` when the excerpt is unique — the case for every row in the
    /// live estate today.
    ///
    /// When `Some(n)`, this row's `injections` is the count for all `n + 1`
    /// drawers together, not this drawer alone. Acting on the number without
    /// reading the peers would retire the wrong drawer.
    pub collision_peers: Option<usize>,
    /// Short digest of the drawer's **full, untruncated** content.
    ///
    /// Why: when two rows collide their stanzas are visually identical down to
    /// the last rendered character, so a reader cannot tell which is which. The
    /// digest differs whenever the underlying content differs, which is what
    /// makes the two rows separable on the page. Stable only within one run —
    /// it disambiguates rows, it is not an identifier to store or compare across
    /// runs.
    pub content_digest: String,
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
/// each to its injection count, drops rows below `min_injections`, marks excerpt
/// collisions, and sorts by injections descending with the oldest drawer
/// breaking ties. One palace failing to open is recorded in `outcomes` and
/// skipped — a single locked or corrupt palace must not deny the operator the
/// other 92.
/// Test: `ranks_by_injection_count`, `min_injections_filters`,
/// `incompatible_store_is_reported_not_recreated` (one palace fails to open, is
/// recorded in `outcomes`, and is skipped while the rest still report),
/// `colliding_excerpts_are_marked_on_every_affected_row`.
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
    let window_start = index.stats.earliest;

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
                        signals: super::signals::observe(drawer, age_days, window_start),
                        // Filled in below, once the whole palace has been read.
                        collision_peers: None,
                        content_digest: content_digest(&drawer.content),
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

    mark_excerpt_collisions(&mut census.rows);
    census.rows.sort_by(|a, b| {
        b.injections
            .cmp(&a.injections)
            .then_with(|| b.age_days.total_cmp(&a.age_days))
    });
    Ok(census)
}

/// Mark every row whose `(palace, excerpt)` is shared with another row.
///
/// Why: the hook logs key on the rendered excerpt, so colliding drawers are
/// genuinely indistinguishable there and both receive the combined count. That
/// is a real limit of the measurement, and the one thing it must never do is
/// stay invisible — a reader who trusts a misattributed number retires the wrong
/// drawer, which is exactly the harm ADR-0028's human gate exists to prevent.
///
/// What: counts occurrences of each `(palace, excerpt)` pair and writes
/// `collision_peers = Some(n - 1)` on every row in a group of `n > 1`. Rows with
/// a unique excerpt keep `None`.
///
/// A collision never hides behind `min_injections`: colliding rows share one
/// excerpt and therefore one count, so the filter keeps or drops them together.
///
/// Test: `colliding_excerpts_are_marked_on_every_affected_row`,
/// `unique_excerpts_are_not_marked`.
fn mark_excerpt_collisions(rows: &mut [CandidateRow]) {
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    for row in rows.iter() {
        *counts
            .entry((row.palace.as_str(), row.excerpt.as_str()))
            .or_default() += 1;
    }
    let shared: HashMap<(String, String), usize> = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|((p, e), n)| ((p.to_string(), e.to_string()), n))
        .collect();
    if shared.is_empty() {
        return;
    }
    for row in rows.iter_mut() {
        if let Some(n) = shared.get(&(row.palace.clone(), row.excerpt.clone())) {
            row.collision_peers = Some(n - 1);
        }
    }
}

/// Short digest of a drawer's full content, for telling colliding rows apart.
///
/// What: `DefaultHasher` rendered as 8 hex characters. Deliberately not a
/// cryptographic or cross-run-stable hash — its only job is to differ when two
/// visually identical stanzas have different underlying content, within one
/// report. Nothing persists or compares it.
fn content_digest(content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
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
