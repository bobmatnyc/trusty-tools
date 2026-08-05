//! Read-only plan of what a room backfill would write (ADR-0027 T10 / D1.4).
//!
//! Why: the backfill runs automatically at palace open, against live palaces
//! holding real memory. ADR-0027 D1.4 makes it "inspectable before it writes"
//! — an operator must be able to see the label each `room_id` would be given,
//! and by which confidence step, *before* anything is committed. That audit
//! and the write itself must not be two implementations that can disagree, so
//! this module computes the plan and `room_backfill::backfill_rooms` executes
//! exactly it.
//! What: `plan_rooms` — a pure read over the drawer vector plus the `ROOMS`
//! table. It opens read transactions only; it never begins a write.
//! Test: `dry_run_plan_writes_nothing`,
//! `plan_reports_registered_rooms_as_untouched`,
//! `plan_matches_what_backfill_inserts`.

use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::store::kg::KnowledgeGraph;
use crate::memory_core::store::room_backfill::{LabelSource, resolve_room_label};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use uuid::Uuid;

/// What `--apply` would do to one observed `room_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomPlanAction {
    /// No `ROOMS` row yet — a backfill would insert one under this label.
    Insert { room: RoomType, source: LabelSource },
    /// Already registered. A backfill leaves it exactly as it is; this is what
    /// makes a human rename survive every reopen (ADR-0027 D1.4).
    Registered { label: String, resolved: bool },
}

/// One row of the audit: an observed `room_id`, its drawer count, and the
/// action a backfill would take on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomPlanEntry {
    pub room_id: Uuid,
    pub drawer_count: usize,
    pub action: RoomPlanAction,
}

impl RoomPlanEntry {
    /// The label this room carries, or would be given.
    pub fn label(&self) -> String {
        match &self.action {
            RoomPlanAction::Insert { room, .. } => {
                crate::memory_core::room_identity::room_label(room)
            }
            RoomPlanAction::Registered { label, .. } => label.clone(),
        }
    }

    /// Whether `--apply` would write a row for this entry.
    pub fn would_insert(&self) -> bool {
        matches!(self.action, RoomPlanAction::Insert { .. })
    }
}

/// Compute, without writing, what a backfill of `drawers` would do.
///
/// Why: see the module doc — this is the single source of truth both the
/// `--dry-run` audit and the real backfill read from, so the audit cannot
/// drift from the write.
/// What: counts drawers per distinct `room_id` (a `BTreeMap`, so the order —
/// and hence which room wins a canonical-key collision — is deterministic and
/// identical to what `backfill_rooms` will apply), then per id either reports
/// the registered row or resolves a label through the four-step ladder.
/// Read-only: the only redb calls here are `get_room` and, lazily, the KG
/// subject scan behind step 3.
/// Test: `dry_run_plan_writes_nothing`, `plan_matches_what_backfill_inserts`.
pub fn plan_rooms(kg: &KnowledgeGraph, drawers: &[Drawer]) -> Result<Vec<RoomPlanEntry>> {
    let mut counts: BTreeMap<Uuid, usize> = BTreeMap::new();
    for d in drawers {
        *counts.entry(d.room_id).or_insert(0) += 1;
    }
    let store = kg.store();
    // Loaded lazily: most palaces resolve everything in steps 1-2 and never
    // pay for the KG scan.
    let mut dictionary: Option<Vec<String>> = None;
    let mut out = Vec::with_capacity(counts.len());
    for (room_id, drawer_count) in counts {
        let action = match store
            .get_room(room_id)
            .with_context(|| format!("probe room row {room_id}"))?
        {
            Some(record) => RoomPlanAction::Registered {
                label: record.label,
                resolved: record.resolved,
            },
            None => {
                let (room, source) = resolve_room_label(kg, room_id, &mut dictionary);
                RoomPlanAction::Insert { room, source }
            }
        };
        out.push(RoomPlanEntry {
            room_id,
            drawer_count,
            action,
        });
    }
    Ok(out)
}
