//! Palace → JSONL export (#5902).
//!
//! Why: the palace had no export of any kind, so there was no artefact two
//! machines could exchange. JSONL is the format `trusty-agents memories export`
//! already uses and the one a git-committed file wants: one memory per line, so a
//! diff shows added and removed facts rather than a reflowed blob, and a
//! half-written file loses only its last line.
//! What: [`export_palace_records`] (the pure half: drawers to records) and
//! [`export_palace_jsonl`] (write them to a path).
//! Test: `export_writes_one_line_per_drawer`, `export_then_import_preserves_metadata`,
//! `export_is_ordered_by_hash` in `share::tests`.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use uuid::Uuid;

use super::record::SharedMemoryRecord;
use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::store::rooms::list_room_summaries;

/// Every shareable memory in `handle`, as records, ordered by content hash.
///
/// Why the ordering is by hash and not by time: the file is committed to git, and
/// a stable order is what keeps a re-export from producing a diff that has
/// nothing to do with what changed. Content hash is the only key both machines
/// compute identically — `created_at` differs between two machines' copies of one
/// fact by construction, and drawer UUIDs are v4.
///
/// Why every drawer and not a filtered set: filtering is the caller's business,
/// and the caller here is a CLI command that does not exist yet. Two exclusions
/// are structural rather than policy, so they live here: a drawer whose TTL has
/// already elapsed is not a fact any more (`Drawer::is_expired_at`), and a Tier C
/// drawer holds a point-in-time claim whose retirement condition is local to the
/// machine that wrote it (`Drawer::is_tier_c`) — shipping `pr:4818/state` to
/// another machine would assert a stale slot there.
///
/// 🔴 SECURITY: nothing on this path screens content for secrets.
/// `memory_core::filter::check_secret` runs at WRITE time only, so any credential
/// that predates the secret filter, or that the filter's heuristics missed, is
/// already sitting in a palace and this function will copy it out verbatim. That
/// is acceptable while the destination is a local file. It is NOT acceptable for
/// the git-commit workflow this primitive exists to serve, which must gate on a
/// re-scan of every exported record — see #5902 and #1683.
///
/// What: reads the in-memory drawer table (a complete mirror of `DRAWERS`), maps
/// each drawer's `room_id` to its registered label, and returns one record per
/// surviving drawer. A drawer whose room is not in the registry falls back to
/// `"general"`, matching how recall treats an unregistered room.
/// Test: `export_writes_one_line_per_drawer`, `export_skips_expired_and_tier_c`,
/// `export_is_ordered_by_hash`.
pub fn export_palace_records(handle: &PalaceHandle) -> Result<Vec<SharedMemoryRecord>> {
    let labels = room_labels(handle)?;
    let now = chrono::Utc::now();
    let drawers = handle.drawers.read().clone();

    let mut records: Vec<SharedMemoryRecord> = drawers
        .iter()
        .filter(|d| !d.is_expired_at(now))
        .filter(|d| !d.is_tier_c())
        .map(|d| {
            let label = labels
                .get(&d.room_id)
                .map(String::as_str)
                .unwrap_or("general");
            SharedMemoryRecord::from_drawer(d, label)
        })
        .collect();

    // Content hash is the only total order both machines agree on.
    records.sort_by(|a, b| {
        a.content_hash
            .cmp(&b.content_hash)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    Ok(records)
}

/// Write `handle`'s shareable memories to `path` as JSONL. Returns the line
/// count.
///
/// Why the write is atomic: the file is the input to an import on another
/// machine, and a torn file is one an importer would read as a truncated
/// export — silently short rather than obviously broken. Writing to a sibling
/// temp file and renaming is the same discipline `PalaceStore::save_palace` and
/// `L1Cache::save_l1_cache` already use.
/// What: creates `path`'s parent, serializes one record per line to
/// `<path>.tmp`, then renames over `path`.
/// Test: `export_writes_one_line_per_drawer`, `export_then_import_preserves_metadata`.
pub fn export_palace_jsonl(handle: &PalaceHandle, path: &Path) -> Result<usize> {
    let records = export_palace_records(handle)?;
    write_jsonl(&records, path)?;
    Ok(records.len())
}

/// Serialize `records` to `path` as one JSON object per line, atomically.
///
/// Why separate from [`export_palace_jsonl`]: PR 2's commit flow needs to write a
/// filtered or merged record set it assembled itself, and tests need to write a
/// hand-built file without standing up a palace.
/// What: as described on [`export_palace_jsonl`].
/// Test: `export_then_import_preserves_metadata`, `two_machines_converge_on_one_memory`.
pub fn write_jsonl(records: &[SharedMemoryRecord], path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {} for the export", parent.display()))?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    let mut buf = Vec::new();
    for rec in records {
        serde_json::to_writer(&mut buf, rec).context("serialize a share record")?;
        buf.push(b'\n');
    }
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(&buf)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} onto {}", tmp.display(), path.display()))?;
    Ok(())
}

/// `room_id` → registered label for every room in this palace.
///
/// Why: the export carries the room LABEL so a receiving palace can mint the
/// same UUIDv5 id from it (ADR-0027), and the label lives in the ROOMS registry
/// rather than on the drawer.
/// What: one registry read, collected into a map so the per-drawer lookup is not
/// a redb round trip.
/// Test: `export_then_import_preserves_metadata` asserts the label survives.
fn room_labels(handle: &PalaceHandle) -> Result<HashMap<Uuid, String>> {
    let store = handle.kg.store();
    let summaries = list_room_summaries(&store).context("read the room registry for export")?;
    Ok(summaries.into_iter().map(|r| (r.id, r.label)).collect())
}
