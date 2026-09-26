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
//! What: [`scan_report`] (the default, `--dry-run` behaviour) reads a private
//! copy of `kg.db` and of `kg.redb`, and reports the legacy rows, the ones
//! already in `kg.redb`, the ones missing, unreadable rows, legacy triples,
//! content duplicated under other ids, and any `.v2-incompatible` quarantine
//! files. [`apply_report`] imports the missing drawers verbatim (same id,
//! room, timestamps) into `kg.redb` in one transaction and embeds them so
//! recall can reach them. Nothing is ever deleted, renamed or rewritten in
//! `kg.db`; a re-run imports nothing because every legacy id is then in
//! `kg.redb`. [`unaccounted_legacy_data`] is the check `palace_delete` makes
//! before it removes a palace directory.
//!
//! Test: `dry_run_counts_legacy_drawers_and_writes_nothing`,
//! `apply_makes_legacy_drawers_reachable_and_rerun_is_a_noop`,
//! `delete_palace_refuses_while_legacy_kg_holds_unimported_drawers`.

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use trusty_common::memory_core::memory_content_hash;
use trusty_common::memory_core::palace::{Drawer, Palace};
use trusty_common::memory_core::retrieval::{PalaceHandle, VectorBackfillOptions};
use trusty_common::memory_core::store::concurrent_open::try_open_or_snapshot;
use trusty_common::memory_core::store::{OpenIntent, INCOMPATIBLE_SUFFIX};
use uuid::Uuid;

use super::store_snapshot::{with_store_copy, SCRATCH_PREFIX};

/// Filename of the pre-redb SQLite knowledge graph inside a palace directory.
pub(crate) const LEGACY_KG_FILE: &str = "kg.db";

/// SQLite sidecars that can hold committed (`-wal`) or to-be-rolled-back
/// (`-journal`) state for `kg.db`; copied with it so the copy reads the same.
const SQLITE_SIDECARS: [&str; 2] = ["-wal", "-journal"];

/// The vector store's redb file (`index.usearch` + `.redb`, see `vector.rs`).
const INDEX_FILE: &str = "index.usearch.redb";

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
/// but not SQLite is an `Err`, not "no legacy data". Otherwise copies `kg.db`
/// and its `-wal`/`-journal` sidecars into a private temp dir, opens the copy
/// (never the original), and decodes every `drawers` row the way the removed
/// #45 reader did — except a row it cannot decode lands in
/// [`LegacyDrawers::unreadable`] instead of vanishing. Reading a copy means
/// rows only in a `-wal` are seen and SQLite creates no `-shm` in the palace.
/// Test: `dry_run_counts_legacy_drawers_and_writes_nothing`,
/// `wal_only_legacy_rows_are_counted_and_imported`.
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
    // #8434: a copy, so a WAL read or a hot-journal rollback never touches the
    // palace dir. `scratch` outlives `conn` (locals drop in reverse order).
    let scratch = tempfile::TempDir::with_prefix_in(SCRATCH_PREFIX, std::env::temp_dir())
        .context("create scratch dir for the legacy kg.db copy")?;
    let copy = scratch.path().join(LEGACY_KG_FILE);
    std::fs::copy(&path, &copy).with_context(|| format!("copy {}", path.display()))?;
    for suffix in SQLITE_SIDECARS {
        let side = data_dir.join(format!("{LEGACY_KG_FILE}{suffix}"));
        if side
            .try_exists()
            .with_context(|| format!("cannot stat {}", side.display()))?
        {
            std::fs::copy(
                &side,
                scratch.path().join(format!("{LEGACY_KG_FILE}{suffix}")),
            )
            .with_context(|| format!("copy {}", side.display()))?;
        }
    }
    // Read-write on the private copy only: a read-only connection cannot roll
    // back a hot journal, so it would refuse exactly the crash-left file.
    let conn = Connection::open_with_flags(
        &copy,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open a copy of {}", path.display()))?;

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
    /// Legacy drawers whose id `kg.redb` already holds. The L1 snapshot does
    /// not count: it is a capped cache, not a store (#8434).
    pub already_live: usize,
    /// Legacy drawers `kg.redb` lacks (before this run's import).
    pub missing: usize,
    /// Dry run only: how many `missing` drawers repeat the content of a live
    /// drawer under another id. Counted, never deduplicated.
    pub content_duplicates: Option<usize>,
    /// Drawers this run wrote to `kg.redb`. Always 0 on a dry run.
    pub imported: usize,
    /// `(repaired, still_missing)` from the vector backfill, when it ran.
    pub vectors: Option<(usize, usize)>,
    /// The embed error after a committed import. The caller prints the report
    /// first, then fails, so the import is never unreported.
    pub embed_error: Option<String>,
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
            if let Some(dups) = self.content_duplicates {
                out.push_str(&format!(
                    "  content_duplicates={dups} (missing drawers whose content a live drawer \
                     already holds under another id; --apply imports them anyway)\n"
                ));
            }
        } else {
            out.push_str("  legacy kg.db: none\n");
        }
        if let Some((repaired, still)) = self.vectors {
            out.push_str(&format!(
                "  vectors: repaired={repaired} still_missing={still}\n"
            ));
        }
        if let Some(e) = &self.embed_error {
            out.push_str(&format!(
                "  vectors: FAILED after the import committed: {e}\n"
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
/// What: reads a copy of `kg.db` and the drawers of a private copy of
/// `kg.redb`. Only `kg.redb` counts as live: a drawer present only in the L1
/// snapshot is lost at the next L1 flush, so it is reported missing (#8434).
/// Also counts missing drawers whose content a live drawer already holds.
/// Safe while the daemon holds the palace.
/// Test: `dry_run_counts_legacy_drawers_and_writes_nothing`,
/// `l1_only_legacy_drawer_is_imported_to_redb`.
pub fn scan_report(palace: &Palace) -> Result<LegacyReport> {
    let data_dir = &palace.data_dir;
    let mut report = base_report(palace, true)?;
    let Some(legacy) = read_legacy_kg(data_dir)? else {
        return Ok(report);
    };
    let (live, live_hashes) = with_store_copy(data_dir, &std::env::temp_dir(), |s| {
        let hashes: HashSet<_> = s
            .load_drawers()?
            .iter()
            .map(|d| memory_content_hash(d.content()))
            .collect();
        Ok((s.load_drawer_ids()?, hashes))
    })?
    .unwrap_or_default();
    fill_legacy_counts(&mut report, &legacy, &live);
    report.content_duplicates = Some(
        legacy
            .drawers
            .iter()
            .filter(|d| !live.contains(&d.id))
            .filter(|d| live_hashes.contains(&memory_content_hash(d.content())))
            .count(),
    );
    Ok(report)
}

/// Apply: import every missing legacy drawer, then embed the palace's
/// vector-less drawers so recall reaches them.
///
/// Why: see the module doc. Needs the write lock — a daemon holding the palace
/// makes the `Writer` open fail loud rather than write to a snapshot.
/// What: first probes private copies of `kg.redb` and the vector index and
/// bails if either fails to open — a `Writer` open would rename such a store
/// aside and recreate it empty (#702). Then opens the palace `Writer`; refuses
/// a handle whose drawer table loaded degraded (a partial live set would
/// re-import rows it merely failed to read, overwriting them with their legacy
/// text). Dedupes against the ids in `kg.redb` only, never the L1 snapshot.
/// Upserts the missing drawers in one redb transaction, adds the ones not
/// already in memory to the in-memory table, and — unless `embed` is false —
/// runs the palace's own missing-vector backfill. An embed failure after the
/// commit lands in [`LegacyReport::embed_error`]. Never writes `kg.db`.
/// Test: `apply_makes_legacy_drawers_reachable_and_rerun_is_a_noop`,
/// `l1_only_legacy_drawer_is_imported_to_redb`,
/// `apply_refuses_a_store_the_writer_open_would_rename_aside`.
pub async fn apply_report(palace: &Palace, embed: bool) -> Result<LegacyReport> {
    let mut report = base_report(palace, false)?;
    let Some(legacy) = read_legacy_kg(&palace.data_dir)? else {
        return Ok(report);
    };
    probe_stores(&palace.data_dir)?;
    let handle = PalaceHandle::open_with_intent(palace, OpenIntent::Writer)
        .with_context(|| format!("open palace {} for writing", palace.id))?;
    if handle.drawer_load_degraded {
        bail!(
            "palace {}: the live drawer table loaded degraded; refusing to import over rows \
             it could not read",
            palace.id
        );
    }
    // #8434: redb ids only — an L1-only drawer is not persisted anywhere.
    let live = handle
        .kg
        .load_drawer_ids()
        .context("load the drawer ids in kg.redb")?;
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
        let mut in_memory = handle.drawers.write();
        let held: HashSet<Uuid> = in_memory.iter().map(|d| d.id).collect();
        in_memory.extend(missing.into_iter().filter(|d| !held.contains(&d.id)));
    }
    if embed {
        match handle
            .backfill_missing_vectors(VectorBackfillOptions {
                dry_run: false,
                ..VectorBackfillOptions::default()
            })
            .await
        {
            Ok(v) => report.vectors = Some((v.repaired, v.still_missing_ids.len())),
            Err(e) => report.embed_error = Some(format!("{e:#}")),
        }
    }
    Ok(report)
}

/// Refuse to import when a `Writer` open would recreate a store (#8434).
///
/// Why: `OpenIntent::Writer` renames an incompatible-format `kg.redb` or
/// vector index aside and creates it empty (`concurrent_open.rs`, #702). An
/// import must not be the step that does that.
/// What: opens private copies of both files the way the dry run does and
/// returns the first failure; an absent file passes.
/// Test: `apply_refuses_a_store_the_writer_open_would_rename_aside`.
fn probe_stores(data_dir: &Path) -> Result<()> {
    with_store_copy(data_dir, &std::env::temp_dir(), |s| s.load_drawer_ids())
        .context("kg.redb failed a read-only probe; refusing to open it for writing")?;
    let live = data_dir.join(INDEX_FILE);
    if !live
        .try_exists()
        .with_context(|| format!("cannot stat {}", live.display()))?
    {
        return Ok(());
    }
    let scratch = tempfile::TempDir::with_prefix_in(SCRATCH_PREFIX, std::env::temp_dir())
        .context("create scratch dir for the vector index probe")?;
    let copy = scratch.path().join(INDEX_FILE);
    std::fs::copy(&live, &copy).with_context(|| format!("copy {}", live.display()))?;
    // redb can panic on a torn file; see `with_store_copy`.
    match std::panic::catch_unwind(AssertUnwindSafe(|| {
        try_open_or_snapshot(&copy, OpenIntent::ReadOnlyClient).map(drop)
    })) {
        Ok(opened) => opened.with_context(|| {
            format!(
                "{} failed a read-only probe; refusing to open it for writing",
                live.display()
            )
        }),
        Err(_) => bail!("panic while probing a copy of {}", live.display()),
    }
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
/// What: `Some(reason)` when `kg.db` has drawer rows `live` lacks (unreadable
/// rows count — they cannot be proven imported), when it has any legacy
/// triples (never imported, with or without a `drawers` table), when `kg.db`
/// cannot be read — any schema or page it cannot decode — or when any
/// `.v2-incompatible` file is present; `None` otherwise.
/// Test: `delete_palace_refuses_while_legacy_kg_holds_unimported_drawers`,
/// `unaccounted_legacy_data_refuses_triples_and_unreadable_kg_db`.
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
            // #8434: triples are never imported, so kg.db is their only copy.
            if l.triple_rows > 0 {
                reasons.push(format!(
                    "legacy kg.db holds {} triple(s) the live graph never imported",
                    l.triple_rows
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
