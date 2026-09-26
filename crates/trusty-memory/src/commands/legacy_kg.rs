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
//! files. Both screen the missing drawers with the `memory_remember` write
//! gates and report the refused ids by reason ([`guard`]). [`apply_report`]
//! first backs up and verifies every store file it can modify, then imports
//! the drawers that passed verbatim (same id, room, timestamps) into
//! `kg.redb` in one transaction and embeds them so recall can reach them;
//! content duplicates are skipped unless
//! `--include-content-duplicates` is passed. Nothing is ever deleted, renamed
//! or rewritten in `kg.db`; a re-run imports nothing because every imported id
//! is then in `kg.redb` and a skipped duplicate is skipped again. [`unaccounted_legacy_data`] is the check `palace_delete` makes
//! before it removes a palace directory.
//!
//! Test: `dry_run_counts_legacy_drawers_and_writes_nothing`,
//! `apply_makes_legacy_drawers_reachable_and_rerun_is_a_noop`,
//! `delete_palace_refuses_while_legacy_kg_holds_unimported_drawers`,
//! `credential_row_is_rejected_in_dry_run_and_apply`,
//! `apply_with_a_failing_backup_writes_nothing`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use trusty_common::memory_core::palace::{Drawer, Palace};
use trusty_common::memory_core::retrieval::{PalaceHandle, VectorBackfillOptions};
use trusty_common::memory_core::store::{OpenIntent, INCOMPATIBLE_SUFFIX};
use trusty_common::memory_core::{memory_content_hash, ContentHash};
use uuid::Uuid;

use super::store_snapshot::{with_store_copy, SCRATCH_PREFIX};

#[path = "legacy_kg_guard.rs"]
pub mod guard;
use guard::{backup_stores, probe_stores, screen_drawers, Backup, CopyFn, Rejected};

/// Filename of the pre-redb SQLite knowledge graph inside a palace directory.
pub(crate) const LEGACY_KG_FILE: &str = "kg.db";

/// Suffixes of the files copied together: `kg.db` itself, then the SQLite
/// sidecars that can hold committed (`-wal`) or to-be-rolled-back (`-journal`)
/// state for it, so the copy reads the same.
const COPIED_SUFFIXES: [&str; 3] = ["", "-wal", "-journal"];

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
/// and its `-wal`/`-journal` sidecars into a private temp dir — an `Err` when
/// they keep changing mid-copy ([`copy_stable`]) — opens the copy (never the
/// original), and decodes every `drawers` row the way the removed
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
    copy_stable(data_dir, scratch.path(), |from, to| std::fs::copy(from, to))?;
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

/// `(len, mtime)` of `kg.db` and each sidecar, `None` for an absent file.
type Fingerprint = Vec<Option<(u64, Option<std::time::SystemTime>)>>;

fn fingerprint(data_dir: &Path) -> Result<Fingerprint> {
    COPIED_SUFFIXES
        .iter()
        .map(|suffix| {
            let p = data_dir.join(format!("{LEGACY_KG_FILE}{suffix}"));
            match std::fs::metadata(&p) {
                Ok(m) => Ok(Some((m.len(), m.modified().ok()))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e).with_context(|| format!("cannot stat {}", p.display())),
            }
        })
        .collect()
}

/// Copy `kg.db` and its sidecars into `dest`, refusing a copy that may be torn.
///
/// Why: #8434 — the files are copied one after another, so a writer that
/// touches `kg.db` or its WAL mid-copy leaves a copy that matches neither
/// state, and the counts drawn from it would be wrong.
/// What: stats every file (size and mtime) before and after one copy pass. A
/// change means the pass is retried once; a second change is an `Err`, which
/// the delete guard turns into a refusal. `copy` is the per-file copy, a
/// seam for the test. A retry first removes the previous pass's files.
/// Test: `copy_stable_retries_once_then_refuses_a_changing_kg_db`.
pub(crate) fn copy_stable(
    data_dir: &Path,
    dest: &Path,
    mut copy: impl FnMut(&Path, &Path) -> std::io::Result<u64>,
) -> Result<()> {
    for _ in 0..2 {
        let before = fingerprint(data_dir)?;
        for (suffix, stat) in COPIED_SUFFIXES.iter().zip(&before) {
            let name = format!("{LEGACY_KG_FILE}{suffix}");
            let (src, dst) = (data_dir.join(&name), dest.join(&name));
            match std::fs::remove_file(&dst) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("clear {}", dst.display()));
                }
                _ => {}
            }
            if stat.is_some() {
                copy(&src, &dst).with_context(|| format!("copy {}", src.display()))?;
            }
        }
        if fingerprint(data_dir)? == before {
            return Ok(());
        }
    }
    bail!(
        "{} changed during both copy attempts; refusing to read a possibly torn copy",
        data_dir.join(LEGACY_KG_FILE).display()
    )
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
    /// How many `missing` drawers repeat the content of a `kg.redb` drawer
    /// under another id; `None` without a `kg.db`. An apply skips them unless
    /// `include_content_duplicates` is set.
    pub content_duplicates: Option<usize>,
    /// Missing drawers the `memory_remember` write gates refuse; never imported.
    pub rejected: Vec<Rejected>,
    /// Apply only: the verified pre-write backup (#8434).
    pub backup: Option<Backup>,
    /// `--allow-short`: the 8-token minimum was skipped (#8434).
    pub allow_short: bool,
    /// Apply only: `--include-content-duplicates` imported the duplicates.
    pub include_content_duplicates: bool,
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
                let fate = match (self.dry_run, self.include_content_duplicates) {
                    (true, _) => "--apply skips them unless --include-content-duplicates",
                    (false, false) => "skipped; --include-content-duplicates imports them",
                    (false, true) => "imported by --include-content-duplicates",
                };
                out.push_str(&format!(
                    "  content_duplicates={dups} (missing drawers whose content a live drawer \
                     already holds under another id; {fate})\n"
                ));
            }
            out.push_str(&guard::render_guards(
                self.dry_run,
                self.allow_short,
                &self.rejected,
                self.backup.as_ref(),
            ));
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
/// Also counts missing drawers whose content a live drawer already holds, and
/// lists the ones the write gates refuse, as the apply with the same
/// `allow_short` will (#8434).
/// Safe while the daemon holds the palace.
/// Test: `dry_run_counts_legacy_drawers_and_writes_nothing`,
/// `noise_row_is_rejected_in_dry_run_and_apply`,
/// `short_row_is_rejected_by_default_and_imported_with_allow_short`,
/// `l1_only_legacy_drawer_is_imported_to_redb`.
pub fn scan_report(palace: &Palace, allow_short: bool) -> Result<LegacyReport> {
    let data_dir = &palace.data_dir;
    let mut report = base_report(palace, true, allow_short)?;
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
    let (_, duplicates, rejected) = split_missing(legacy.drawers, &live, &live_hashes, allow_short);
    report.content_duplicates = Some(duplicates.len());
    report.rejected = rejected;
    Ok(report)
}

/// Split the legacy drawers `live` lacks into `(distinct, duplicates,
/// rejected)`: `rejected` fail the write gates (#8434), and of the rest a
/// duplicate repeats the content of a `kg.redb` drawer under another id.
fn split_missing(
    drawers: Vec<Drawer>,
    live: &HashSet<Uuid>,
    live_hashes: &HashSet<ContentHash>,
    allow_short: bool,
) -> (Vec<Drawer>, Vec<Drawer>, Vec<Rejected>) {
    let missing = drawers.into_iter().filter(|d| !live.contains(&d.id));
    let (passed, rejected) = screen_drawers(missing, allow_short);
    let (distinct, duplicates) = passed
        .into_iter()
        .partition(|d| !live_hashes.contains(&memory_content_hash(d.content())));
    (distinct, duplicates, rejected)
}

/// Put the drawers just written to `kg.redb` into the in-memory table.
///
/// Why: #8434 — an entry already in memory for an imported id came from the
/// L1 snapshot, not from `kg.redb`; keeping it would leave memory and redb
/// disagreeing about that drawer.
/// What: replaces an entry with a matching id in place and appends the rest.
/// Test: `merge_imported_replaces_l1_entries_and_appends_the_rest`.
fn merge_imported(in_memory: &mut Vec<Drawer>, imported: Vec<Drawer>) {
    let mut fresh: HashMap<Uuid, Drawer> = imported.into_iter().map(|d| (d.id, d)).collect();
    for slot in in_memory.iter_mut() {
        if let Some(d) = fresh.remove(&slot.id) {
            *slot = d;
        }
    }
    in_memory.extend(fresh.into_values());
}

/// Apply: import every missing legacy drawer, then embed the palace's
/// vector-less drawers so recall reaches them.
///
/// Why: see the module doc. Needs the write lock — a daemon holding the palace
/// makes the `Writer` open fail loud rather than write to a snapshot.
/// What: first probes private copies of `kg.redb` and the vector index and
/// bails if either fails to open — a `Writer` open would rename such a store
/// aside and recreate it empty (#702). Then backs up every store file into
/// `<palace>/legacy-kg-backup-<timestamp>/` and bails, writing nothing, when a
/// copy fails verification ([`guard::backup_stores`]). Then opens the palace
/// `Writer` (whose open may already sweep expired rows); refuses
/// a handle whose drawer table loaded degraded (a partial live set would
/// re-import rows it merely failed to read, overwriting them with their legacy
/// text). Dedupes against the ids in `kg.redb` only, never the L1 snapshot,
/// drops the drawers the write gates refuse (listed in
/// [`LegacyReport::rejected`]), and skips missing drawers whose content a
/// `kg.redb` drawer already holds under another id unless
/// `include_content_duplicates` is set; the skipped
/// count lands in [`LegacyReport::content_duplicates`]. Upserts the rest in
/// one redb transaction, puts them in the in-memory table (replacing an
/// L1-only entry for the same id), and — unless `embed` is false — runs the
/// palace's own missing-vector backfill. An embed failure after the commit
/// lands in [`LegacyReport::embed_error`]. Never writes `kg.db`.
/// Test: `apply_makes_legacy_drawers_reachable_and_rerun_is_a_noop`,
/// `l1_only_legacy_drawer_is_imported_to_redb`,
/// `apply_skips_content_duplicates_unless_included`,
/// `apply_refuses_a_store_the_writer_open_would_rename_aside`,
/// `apply_with_a_failing_backup_writes_nothing`,
/// `credential_row_is_rejected_in_dry_run_and_apply`,
/// `short_secret_row_is_rejected_even_with_allow_short`,
/// `apply_error_after_backup_names_the_kept_backup`.
pub async fn apply_report(
    palace: &Palace,
    embed: bool,
    include_content_duplicates: bool,
    allow_short: bool,
) -> Result<LegacyReport> {
    let copy: CopyFn = |from, to| std::fs::copy(from, to);
    let flags = (embed, include_content_duplicates, allow_short);
    apply_report_with(palace, flags, &palace.data_dir, copy).await
}

/// [`apply_report`] with the backup parent dir and per-file copy injected;
/// `flags` is `(embed, include_content_duplicates, allow_short)`.
pub(crate) async fn apply_report_with(
    palace: &Palace,
    (embed, include_content_duplicates, allow_short): (bool, bool, bool),
    backup_parent: &Path,
    copy: CopyFn,
) -> Result<LegacyReport> {
    let mut report = base_report(palace, false, allow_short)?;
    let Some(legacy) = read_legacy_kg(&palace.data_dir)? else {
        return Ok(report);
    };
    probe_stores(&palace.data_dir)?;
    // #8434: fail closed — no verified backup, no write.
    let backup = backup_stores(&palace.data_dir, backup_parent, copy)?;
    // #8434: a failure from here on must still name the backup it left.
    let kept = format!("backup kept at {}", backup.dir.display());
    report.backup = Some(backup);
    report.include_content_duplicates = include_content_duplicates;
    import_legacy(palace, legacy, report, embed)
        .await
        .context(kept)
}

/// The writes of [`apply_report_with`], run only after a verified backup.
async fn import_legacy(
    palace: &Palace,
    legacy: LegacyDrawers,
    mut report: LegacyReport,
    embed: bool,
) -> Result<LegacyReport> {
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
    let live_hashes: HashSet<ContentHash> = handle
        .kg
        .load_drawers()
        .context("load the drawers in kg.redb")?
        .iter()
        .map(|d| memory_content_hash(d.content()))
        .collect();
    fill_legacy_counts(&mut report, &legacy, &live);
    let (mut to_import, duplicates, rejected) =
        split_missing(legacy.drawers, &live, &live_hashes, report.allow_short);
    report.content_duplicates = Some(duplicates.len());
    report.rejected = rejected;
    // #8434: a content duplicate is already present under another id.
    if report.include_content_duplicates {
        to_import.extend(duplicates);
    }
    if !to_import.is_empty() {
        handle
            .kg
            .upsert_drawers_atomic(to_import.clone())
            .await
            .context("import legacy drawers into kg.redb")?;
        report.imported = to_import.len();
        merge_imported(&mut handle.drawers.write(), to_import);
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

/// The fields every report carries whether or not `kg.db` exists.
fn base_report(palace: &Palace, dry_run: bool, allow_short: bool) -> Result<LegacyReport> {
    Ok(LegacyReport {
        palace: palace.id.as_str().to_string(),
        dry_run,
        allow_short,
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
