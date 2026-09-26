//! `trusty-memory palace legacy-kg <name> [--apply]` — recover the drawers a
//! pre-redb SQLite `kg.db` still holds (#8434).
//!
//! Why: the one-shot SQLite → redb migration (#45) lived behind trusty-common's
//! `sqlite-kg` feature, which the `trusty-memory` binary never enabled, so every
//! daemon build ran the no-op stub instead. #989 then deleted the migration on
//! the belief that every palace had migrated. The result: palaces whose drawers
//! are still rows in `<palace>/kg.db`, beside a `kg.redb` that has never seen
//! them — invisible to recall and list, and destroyed by a `palace_delete` that
//! sees an empty palace. Nothing on the current write path creates this shape;
//! it is legacy data a migration skipped, so recovery is an explicit operator
//! step rather than an at-open migration that would run against every palace
//! the moment a new binary is installed.
//!
//! What: [`scan_report`] (the default, `--dry-run` behaviour) reads `kg.db`
//! read-only and a private copy of `kg.redb`, and reports the legacy rows, the
//! ones already live, the ones missing, unreadable rows, legacy triples, and
//! any `.v2-incompatible` quarantine files. [`apply_report`] imports the
//! missing drawers verbatim (same id, room, timestamps) into `kg.redb` in one
//! transaction and embeds them so recall can reach them. Nothing is ever
//! deleted, renamed or rewritten in `kg.db`; a re-run imports nothing because
//! every legacy id is then live. [`unaccounted_legacy_data`] is the check
//! `palace_delete` makes before it removes a palace directory.
//!
//! Test: `dry_run_counts_legacy_drawers_and_writes_nothing`,
//! `apply_makes_legacy_drawers_reachable_and_rerun_is_a_noop`,
//! `delete_palace_refuses_while_legacy_kg_holds_unimported_drawers`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use trusty_common::memory_core::palace::{Drawer, Palace};
use trusty_common::memory_core::retrieval::{PalaceHandle, VectorBackfillOptions};
use trusty_common::memory_core::store::{L1Cache, OpenIntent, INCOMPATIBLE_SUFFIX};
use uuid::Uuid;

use super::store_snapshot::with_store_copy;

/// Filename of the pre-redb SQLite knowledge graph inside a palace directory.
pub(crate) const LEGACY_KG_FILE: &str = "kg.db";

/// The 16-byte header every SQLite 3 database file starts with.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Rows of the legacy `drawers` table, as read from `kg.db`.
#[derive(Debug, Default)]
pub struct LegacyDrawers {
    /// Every row in the table, readable or not.
    pub total_rows: usize,
    /// The rows that decoded into a [`Drawer`].
    pub drawers: Vec<Drawer>,
    /// `id: reason` for each row that could not be decoded. Reported, never
    /// imported — and never dropped without a word.
    pub unreadable: Vec<String>,
    /// Rows in the legacy `triples` table. Reported only: merging stale legacy
    /// facts into a live graph is an owner decision, not a repair.
    pub triple_rows: usize,
}

/// Read `<data_dir>/kg.db` without writing to it.
///
/// Why: this is the only reader of the legacy store, and it runs against the
/// one copy of data nothing else can reach, so it must not be able to change a
/// byte of it.
/// What: `Ok(None)` when `kg.db` is genuinely absent. A file that is present
/// but not SQLite is an `Err`, not "no legacy data". Otherwise opens it
/// `SQLITE_OPEN_READ_ONLY` and decodes every `drawers` row the way the removed
/// #45 reader did — except a row it cannot decode lands in
/// [`LegacyDrawers::unreadable`] instead of vanishing.
/// Test: `dry_run_counts_legacy_drawers_and_writes_nothing`.
pub fn read_legacy_kg(data_dir: &Path) -> Result<Option<LegacyDrawers>> {
    let path = data_dir.join(LEGACY_KG_FILE);
    let present = path
        .try_exists()
        .with_context(|| format!("cannot stat {}", path.display()))?;
    if !present {
        return Ok(None);
    }
    let mut header = [0u8; 16];
    let n = std::io::Read::read(
        &mut std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?,
        &mut header,
    )
    .with_context(|| format!("read header of {}", path.display()))?;
    if n < header.len() || &header != SQLITE_MAGIC {
        bail!("{} is not a SQLite database", path.display());
    }
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open {} read-only", path.display()))?;

    let mut out = LegacyDrawers::default();
    if has_table(&conn, "triples")? {
        out.triple_rows = conn
            .query_row("SELECT COUNT(*) FROM triples", [], |r| r.get::<_, i64>(0))
            .context("count legacy triples")?
            .try_into()
            .unwrap_or(0);
    }
    if !has_table(&conn, "drawers")? {
        return Ok(Some(out));
    }
    let mut stmt = conn
        .prepare(
            "SELECT id, room_id, content, importance, tags, source_file, created_at \
             FROM drawers",
        )
        .context("prepare legacy drawer scan")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(LegacyRow {
                id: r.get(0)?,
                room_id: r.get(1)?,
                content: r.get(2)?,
                importance: r.get(3)?,
                tags: r.get(4)?,
                source_file: r.get(5)?,
                created_at: r.get(6)?,
            })
        })
        .context("scan legacy drawers")?;
    for row in rows {
        out.total_rows += 1;
        let row = row.context("read legacy drawer row")?;
        let id = row.id.clone().unwrap_or_default();
        match row.into_drawer() {
            Ok(d) => out.drawers.push(d),
            Err(reason) => out.unreadable.push(format!("{id}: {reason}")),
        }
    }
    Ok(Some(out))
}

/// Whether the SQLite database has a table named `name`.
fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |r| r.get(0),
        )
        .with_context(|| format!("look up legacy table {name}"))?;
    Ok(n > 0)
}

/// One `drawers` row with every column nullable, so a malformed row decodes
/// far enough to be reported by id.
struct LegacyRow {
    id: Option<String>,
    room_id: Option<String>,
    content: Option<String>,
    importance: Option<f64>,
    tags: Option<String>,
    source_file: Option<String>,
    created_at: Option<String>,
}

impl LegacyRow {
    /// Decode into a [`Drawer`] carrying the legacy id, room and timestamp.
    fn into_drawer(self) -> std::result::Result<Drawer, String> {
        let id = parse_uuid(self.id.as_deref(), "id")?;
        let room_id = parse_uuid(self.room_id.as_deref(), "room_id")?;
        let content = self.content.ok_or("content is NULL")?;
        let created_at = parse_timestamp(self.created_at.as_deref())?;
        let mut d = Drawer::new(room_id, content);
        d.id = id;
        d.created_at = created_at;
        d.importance = self.importance.unwrap_or(0.5) as f32;
        d.source_file = self.source_file.map(PathBuf::from);
        // The #45 reader also treated undecodable tags as none.
        d.tags = self
            .tags
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Ok(d)
    }
}

fn parse_uuid(raw: Option<&str>, column: &str) -> std::result::Result<Uuid, String> {
    let raw = raw.ok_or_else(|| format!("{column} is NULL"))?;
    Uuid::parse_str(raw).map_err(|e| format!("invalid {column}: {e}"))
}

/// RFC 3339 (what the legacy writer stored), or SQLite's `YYYY-MM-DD HH:MM:SS`.
fn parse_timestamp(raw: Option<&str>) -> std::result::Result<DateTime<Utc>, String> {
    let raw = raw.ok_or("created_at is NULL")?;
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
        .map(|n| n.and_utc())
        .map_err(|e| format!("invalid created_at {raw:?}: {e}"))
}

/// A `.v2-incompatible` quarantine file and its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompatibleFile {
    pub path: PathBuf,
    pub bytes: u64,
}

/// List the `.v2-incompatible` files directly under `data_dir`.
///
/// Why: #8434 — these are redb 2.x stores quarantined at open. Their contents
/// cannot be read by this binary's redb, so the report can only say they exist
/// and how large they are; the delete guard refuses while any remain.
/// Test: `dry_run_counts_legacy_drawers_and_writes_nothing`.
pub fn list_incompatible_files(data_dir: &Path) -> Result<Vec<IncompatibleFile>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(data_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("list {}", data_dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("list {}", data_dir.display()))?;
        if entry
            .file_name()
            .to_string_lossy()
            .contains(INCOMPATIBLE_SUFFIX)
        {
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(IncompatibleFile {
                path: entry.path(),
                bytes,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// What one scan or import of a palace found and did.
#[derive(Debug, Default)]
pub struct LegacyReport {
    pub palace: String,
    pub dry_run: bool,
    /// `false` when the palace has no `kg.db` at all.
    pub legacy_present: bool,
    pub legacy_rows: usize,
    pub unreadable: Vec<String>,
    pub legacy_triples: usize,
    /// Legacy drawers whose id the live store already holds.
    pub already_live: usize,
    /// Legacy drawers the live store lacks (before this run's import).
    pub missing: usize,
    /// Drawers this run wrote to `kg.redb`. Always 0 on a dry run.
    pub imported: usize,
    /// `(repaired, still_missing)` from the vector backfill, when it ran.
    pub vectors: Option<(usize, usize)>,
    pub incompatible: Vec<IncompatibleFile>,
}

impl LegacyReport {
    /// The plain-text report an operator reads before deciding.
    pub fn render(&self) -> String {
        let mut out = format!(
            "palace={} mode={}\n",
            self.palace,
            if self.dry_run { "dry-run" } else { "apply" }
        );
        if self.legacy_present {
            out.push_str(&format!(
                "  legacy kg.db: rows={} unreadable={} already_live={} missing={} imported={} \
                 legacy_triples={} (not imported)\n",
                self.legacy_rows,
                self.unreadable.len(),
                self.already_live,
                self.missing,
                self.imported,
                self.legacy_triples
            ));
            for u in &self.unreadable {
                out.push_str(&format!("    unreadable row {u}\n"));
            }
        } else {
            out.push_str("  legacy kg.db: none\n");
        }
        if let Some((repaired, still)) = self.vectors {
            out.push_str(&format!(
                "  vectors: repaired={repaired} still_missing={still}\n"
            ));
        }
        let total: u64 = self.incompatible.iter().map(|f| f.bytes).sum();
        out.push_str(&format!(
            "  .v2-incompatible files: {} ({total} bytes; redb 2.x, not readable here, left \
             untouched)\n",
            self.incompatible.len()
        ));
        if self.dry_run && self.missing > 0 {
            out.push_str("nothing was written — stop the daemon and re-run with --apply\n");
        }
        out
    }
}

/// Dry run: measure without writing to any palace file.
///
/// Why: the owner decides whether to import, so the default must be a report
/// that provably changes nothing.
/// What: reads `kg.db` read-only and the drawer ids from a private copy of
/// `kg.redb` plus the L1 snapshot — the two sources a palace open serves.
/// Safe while the daemon holds the palace.
/// Test: `dry_run_counts_legacy_drawers_and_writes_nothing`.
pub fn scan_report(palace: &Palace) -> Result<LegacyReport> {
    let data_dir = &palace.data_dir;
    let mut report = base_report(palace, true)?;
    let Some(legacy) = read_legacy_kg(data_dir)? else {
        return Ok(report);
    };
    let mut live: HashSet<Uuid> =
        with_store_copy(data_dir, &std::env::temp_dir(), |s| s.load_drawer_ids())?
            .unwrap_or_default();
    live.extend(
        L1Cache::load_l1_cache(data_dir)
            .context("load L1 snapshot")?
            .iter()
            .map(|d| d.id),
    );
    fill_legacy_counts(&mut report, &legacy, &live);
    Ok(report)
}

/// Apply: import every missing legacy drawer, then embed the palace's
/// vector-less drawers so recall reaches them.
///
/// Why: see the module doc. Needs the write lock — a daemon holding the palace
/// makes the `Writer` open fail loud rather than write to a snapshot.
/// What: opens the palace `Writer`; refuses a handle whose drawer table loaded
/// degraded (a partial live set would re-import rows it merely failed to read,
/// overwriting them with their legacy text). Upserts the missing drawers in one
/// redb transaction, adds them to the in-memory table, and — unless `embed` is
/// false — runs the palace's own missing-vector backfill. Never writes `kg.db`.
/// Test: `apply_makes_legacy_drawers_reachable_and_rerun_is_a_noop`.
pub async fn apply_report(palace: &Palace, embed: bool) -> Result<LegacyReport> {
    let mut report = base_report(palace, false)?;
    let Some(legacy) = read_legacy_kg(&palace.data_dir)? else {
        return Ok(report);
    };
    let handle = PalaceHandle::open_with_intent(palace, OpenIntent::Writer)
        .with_context(|| format!("open palace {} for writing", palace.id))?;
    if handle.drawer_load_degraded {
        bail!(
            "palace {}: the live drawer table loaded degraded; refusing to import over rows \
             it could not read",
            palace.id
        );
    }
    let live: HashSet<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    fill_legacy_counts(&mut report, &legacy, &live);
    let missing: Vec<Drawer> = legacy
        .drawers
        .into_iter()
        .filter(|d| !live.contains(&d.id))
        .collect();
    if !missing.is_empty() {
        handle
            .kg
            .upsert_drawers_atomic(missing.clone())
            .await
            .context("import legacy drawers into kg.redb")?;
        report.imported = missing.len();
        handle.drawers.write().extend(missing);
    }
    if embed {
        let v = handle
            .backfill_missing_vectors(VectorBackfillOptions {
                dry_run: false,
                ..VectorBackfillOptions::default()
            })
            .await
            .context("embed imported drawers")?;
        report.vectors = Some((v.repaired, v.still_missing_ids.len()));
    }
    Ok(report)
}

/// The fields every report carries whether or not `kg.db` exists.
fn base_report(palace: &Palace, dry_run: bool) -> Result<LegacyReport> {
    Ok(LegacyReport {
        palace: palace.id.as_str().to_string(),
        dry_run,
        incompatible: list_incompatible_files(&palace.data_dir)?,
        ..LegacyReport::default()
    })
}

fn fill_legacy_counts(report: &mut LegacyReport, legacy: &LegacyDrawers, live: &HashSet<Uuid>) {
    report.legacy_present = true;
    report.legacy_rows = legacy.total_rows;
    report.unreadable = legacy.unreadable.clone();
    report.legacy_triples = legacy.triple_rows;
    report.already_live = legacy
        .drawers
        .iter()
        .filter(|d| live.contains(&d.id))
        .count();
    report.missing = legacy.drawers.len() - report.already_live;
}

/// Why `palace_delete` must not remove `data_dir`, if anything.
///
/// Why: #8434 — a palace whose live store is empty can still hold the only
/// copy of legacy drawers (`kg.db`) or quarantined redb 2.x stores. Deleting
/// it on "0 drawers" destroys them unrecovered.
/// What: `Some(reason)` when `kg.db` has rows `live` lacks (unreadable rows
/// count — they cannot be proven imported), when `kg.db` cannot be read, or
/// when any `.v2-incompatible` file is present; `None` otherwise.
/// Test: `delete_palace_refuses_while_legacy_kg_holds_unimported_drawers`.
pub fn unaccounted_legacy_data(data_dir: &Path, live: &HashSet<Uuid>) -> Option<String> {
    let mut reasons = Vec::new();
    match read_legacy_kg(data_dir) {
        Ok(None) => {}
        Ok(Some(l)) => {
            let missing =
                l.drawers.iter().filter(|d| !live.contains(&d.id)).count() + l.unreadable.len();
            if missing > 0 {
                reasons.push(format!(
                    "legacy kg.db holds {missing} drawer(s) absent from the live store"
                ));
            }
        }
        Err(e) => reasons.push(format!("legacy kg.db could not be checked: {e:#}")),
    }
    match list_incompatible_files(data_dir) {
        Ok(f) if !f.is_empty() => reasons.push(format!("{} .v2-incompatible file(s)", f.len())),
        Ok(_) => {}
        Err(e) => reasons.push(format!("quarantine files could not be listed: {e:#}")),
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

#[cfg(test)]
#[path = "legacy_kg_tests.rs"]
pub(crate) mod tests;
