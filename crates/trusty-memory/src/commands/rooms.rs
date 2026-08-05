//! `trusty-memory rooms backfill` — the operator audit path (ADR-0027 T10).
//!
//! Why: the room backfill runs automatically the first time a palace is opened,
//! against live palaces holding real memory. ADR-0027 D1.4 requires it to be
//! **inspectable before it writes**: an operator must be able to see the label
//! each `room_id` would be given, by which confidence step, and how many
//! drawers sit behind it — and to see that *without* anything being committed.
//! `--dry-run` is that audit; `--apply` is required to write.
//!
//! What: opens each palace **outside** the registry, deliberately. Every
//! `PalaceRegistry` open path runs the at-open backfill (`registry.rs`), so
//! going through the registry would have written the very rows the dry run
//! exists to preview. `PalaceHandle::open_with_intent` gives the drawer table
//! and the `kg.db` handle with no such side effect, which is what makes
//! "`--dry-run` writes nothing" true rather than merely intended.
//!
//! Test: `dry_run_reports_without_writing`, `apply_writes_the_planned_rooms`,
//! `audit_can_be_scoped_to_one_palace`.

use anyhow::{Context, Result};
use trusty_common::memory_core::palace::Palace;
use trusty_common::memory_core::retrieval::PalaceHandle;
use trusty_common::memory_core::store::room_plan::{plan_rooms, RoomPlanAction, RoomPlanEntry};
use trusty_common::memory_core::store::{backfill_rooms, OpenIntent};
use trusty_common::memory_core::PalaceRegistry;

/// What one palace's audit found.
#[derive(Debug, Clone)]
pub struct PalaceRoomAudit {
    pub palace_id: String,
    pub entries: Vec<RoomPlanEntry>,
    /// `Some` once `--apply` has run: rows actually inserted.
    pub inserted: Option<usize>,
    pub error: Option<String>,
}

impl PalaceRoomAudit {
    /// Rooms `--apply` would (or did) register.
    pub fn pending(&self) -> usize {
        self.entries.iter().filter(|e| e.would_insert()).count()
    }
}

/// CLI entry point for `trusty-memory rooms backfill`.
///
/// Why: a thin shim so the testable surface is [`audit_palaces`] rather than a
/// clap handler. Printing is stdout-only here because this is a one-shot CLI,
/// never the MCP stdio server (which owns stdout for JSON-RPC framing).
/// What: resolves the data root, audits every palace (or one), prints a row per
/// room, and returns non-zero context only on a hard resolve failure — a single
/// unreadable palace is reported and skipped.
/// Test: covered through [`audit_palaces`].
pub async fn handle_rooms_backfill(palace: Option<String>, apply: bool) -> Result<()> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let data_root = crate::resolve_palace_registry_dir(data_dir);
    let audits = audit_palaces(&data_root, palace.as_deref(), apply)?;
    print_audits(&audits, apply);
    Ok(())
}

/// Render the audit to stdout.
fn print_audits(audits: &[PalaceRoomAudit], apply: bool) {
    let mode = if apply { "apply" } else { "dry-run" };
    let (mut rooms, mut pending, mut written, mut errors) = (0usize, 0usize, 0usize, 0usize);
    for audit in audits {
        if let Some(e) = &audit.error {
            errors += 1;
            eprintln!("[error] palace={} {e}", audit.palace_id);
            continue;
        }
        println!("palace={}", audit.palace_id);
        for entry in &audit.entries {
            rooms += 1;
            let (verb, source) = match &entry.action {
                RoomPlanAction::Insert { source, .. } => ("register", format!("{source:?}")),
                RoomPlanAction::Registered { resolved, .. } => (
                    "keep",
                    if *resolved {
                        "existing"
                    } else {
                        "existing/unresolved"
                    }
                    .to_string(),
                ),
            };
            println!(
                "  {:<8} {}  label={:<28} drawers={:<6} via={}",
                verb,
                entry.room_id,
                entry.label(),
                entry.drawer_count,
                source,
            );
        }
        pending += audit.pending();
        written += audit.inserted.unwrap_or(0);
    }
    if apply {
        println!(
            "rooms {mode}: {} palace(s), {rooms} room(s), {written} registered, {errors} error(s)",
            audits.len()
        );
    } else {
        println!(
            "rooms {mode}: {} palace(s), {rooms} room(s), {pending} would be registered, {errors} error(s) \
             — nothing was written; re-run with --apply",
            audits.len()
        );
    }
}

/// Audit one or every palace under `data_root`.
///
/// Why: the testable surface. Separated from the printer so a test can assert
/// on the plan (and on the store being byte-identical afterwards) rather than
/// on formatted text.
/// What: for each palace, opens a handle directly (see the module doc for why
/// not through the registry), computes the plan, and — only when `apply` — runs
/// the real backfill. A palace that fails to open is captured per-palace so one
/// bad palace never aborts the sweep.
/// Test: `dry_run_reports_without_writing`, `apply_writes_the_planned_rooms`.
pub fn audit_palaces(
    data_root: &std::path::Path,
    palace_filter: Option<&str>,
    apply: bool,
) -> Result<Vec<PalaceRoomAudit>> {
    let palaces = PalaceRegistry::list_palaces(data_root).unwrap_or_default();
    let mut out = Vec::new();
    for palace in palaces {
        let id = palace.id.0.clone();
        if palace_filter.is_some_and(|f| f != id) {
            continue;
        }
        out.push(
            audit_one(&palace, apply).unwrap_or_else(|e| PalaceRoomAudit {
                palace_id: id,
                entries: Vec::new(),
                inserted: None,
                error: Some(format!("{e:#}")),
            }),
        );
    }
    Ok(out)
}

/// Audit (and optionally back-fill) a single palace.
fn audit_one(palace: &Palace, apply: bool) -> Result<PalaceRoomAudit> {
    // NOT `registry.open_palace` — that path runs the backfill itself, which
    // would make `--dry-run` write. See the module doc.
    let intent = if apply {
        OpenIntent::Writer
    } else {
        OpenIntent::ReadOnlyClient
    };
    let handle = PalaceHandle::open_with_intent(palace, intent)
        .with_context(|| format!("open palace {}", palace.id))?;
    let drawers = handle.drawers.read().clone();
    let entries = plan_rooms(&handle.kg, &drawers).context("plan rooms")?;
    let inserted = if apply {
        Some(
            backfill_rooms(&handle.kg, &drawers)
                .context("apply room backfill")?
                .inserted,
        )
    } else {
        None
    };
    Ok(PalaceRoomAudit {
        palace_id: palace.id.0.clone(),
        entries,
        inserted,
        error: None,
    })
}

#[cfg(test)]
#[path = "rooms_tests.rs"]
mod tests;
