//! Copy-then-swap compaction of a palace's `kg.redb` (#6652).
//!
//! Why: redb never shrinks a live file. `forget`, `retract` and `delete_table`
//! all return pages to redb's own free list, never to the filesystem, so a
//! palace's `kg.redb` only grows — 342 MB on `trusty-tools` for 2,425 drawers.
//! `Database::compact` exists but cannot run here: it takes `&mut Database`,
//! and this workspace deliberately shares ONE `Arc<Database>` per canonical
//! path across the daemon, the dreamer and every registry in the process; it
//! also refuses while any read transaction is live, and a daemon serving a
//! dozen stdio bridges always has one. Rewriting into a fresh file and renaming
//! it into place is what remains.
//!
//! What: [`prepare`] does the whole rewrite with NO lock held — snapshot the
//! source's per-table fingerprint, back the file up, stream every live row into
//! `kg.redb.compacting` (pruning stale `hist:` rows as a skip), fsync, and
//! verify the copy against the fingerprint. [`PreparedCompaction::commit`] does
//! the small part that needs exclusivity — re-check the fingerprint, rename,
//! and install the already-open replacement handle — and the caller holds the
//! palace write mutex across exactly that call.
//!
//! Reads during the rewrite: untouched. Every reader holds an
//! `Arc<Database>` cloned out of [`super::types::KgDbState`], and that clone
//! keeps the old `Database` — and so the old inode, which `rename` only
//! unlinks — alive for the whole life of its transaction. redb's MVCC then
//! guarantees each such transaction sees one consistent snapshot. A read
//! STARTED after the install picks up the new handle. There is no window in
//! which a reader sees a half-copied file, because no reader ever touches the
//! copy: it is written under a different name and only ever renamed INTO place.
//!
//! Test: `compaction_shrinks_the_file_and_keeps_live_rows`,
//! `a_concurrent_reader_never_observes_a_torn_state`,
//! `a_write_during_the_copy_aborts_the_swap`,
//! `a_crash_before_the_rename_leaves_the_original_untouched`.

use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, ReadableTableMetadata, TableHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::copy_tables::{CopyCounts, copy_all, known_table_names};
use super::store::KgStoreRedb;

/// Suffix for the in-progress rewrite. Never renamed FROM the live path, only
/// TO it, so the live file is untouched until one atomic syscall.
pub const COMPACTING_SUFFIX: &str = ".compacting";

/// Suffix for the pre-compaction backup kept until the next successful run.
pub const BACKUP_SUFFIX: &str = ".pre-compact.bak";

/// Named points a test can fail the compaction at.
///
/// Why (#6652): every failure branch in this file has to fail CLOSED, and the
/// only way to prove that is to make each one happen on demand. Real I/O
/// errors at these exact points cannot be provoked reliably, so the seam is
/// explicit rather than simulated.
/// What: passed to the hook in execution order. A hook returning `Err` aborts
/// the compaction at that point.
/// Test: each variant has a test in `dream::tests` named for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactStep {
    /// After the pre-compaction backup exists and has been size-verified.
    AfterBackup,
    /// After every row is written to the `.compacting` file, before fsync.
    AfterCopy,
    /// After fsync of the copy and its directory, before the source re-check.
    AfterFsync,
    /// After the source re-check passed, immediately before the rename.
    BeforeRename,
    /// After the rename committed, before the in-process handle is installed.
    AfterRename,
}

/// A test hook that can fail the compaction at a named step.
pub type CompactFaultHook = Arc<dyn Fn(CompactStep) -> Result<()> + Send + Sync>;

/// What the caller wants pruned during the rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompactPlan {
    /// Drop `hist:` rows closed before this epoch-millisecond instant. `None`
    /// keeps every history row — the rewrite is then pure space reclaim.
    pub history_cutoff_ms: Option<i64>,
    /// Write `kg.redb.pre-compact.bak` before touching anything.
    pub keep_backup: bool,
}

impl Default for CompactPlan {
    fn default() -> Self {
        Self {
            history_cutoff_ms: None,
            keep_backup: true,
        }
    }
}

/// What one completed compaction did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompactOutcome {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub rows_copied: u64,
    pub history_rows_pruned: u64,
    pub backup: Option<PathBuf>,
}

impl CompactOutcome {
    /// Bytes the rewrite actually returned to the filesystem.
    pub fn bytes_reclaimed(&self) -> u64 {
        self.bytes_before.saturating_sub(self.bytes_after)
    }
}

/// Per-table `(rows, stored_bytes, metadata_bytes)`, the source fingerprint.
///
/// Why: row counts alone do not detect a concurrent write. A retract removes
/// one `TRIPLES` row and inserts one `hist:` row, leaving `len()` identical
/// while the contents differ — so a count-only re-check would wave through a
/// rewrite that silently dropped that retraction. The stored and metadata byte
/// totals move for that case (the history key is five bytes longer and the
/// value gains a `valid_to`), which is what makes the check worth running.
/// What: a sorted `(name, rows, stored, metadata)` list, compared for equality.
/// Test: `a_write_during_the_copy_aborts_the_swap`.
type Fingerprint = Vec<(String, u64, u64, u64)>;

/// A verified rewrite waiting for its rename.
///
/// Why: the rewrite must NOT hold the palace write mutex — it streams the whole
/// file and legitimately outlasts every write budget in the system — while the
/// rename must. Splitting the operation in two is what lets the caller hold the
/// mutex across exactly the rename and nothing else.
/// What: owns the open replacement `Database`, the two paths, and the draft
/// outcome. Dropping it WITHOUT calling [`Self::commit`] deletes the
/// `.compacting` file, so every early return leaves the directory as it found
/// it.
/// Test: `a_crash_before_the_rename_leaves_the_original_untouched`.
#[derive(Debug)]
pub struct PreparedCompaction {
    live_path: PathBuf,
    tmp_path: PathBuf,
    /// `None` only after a successful commit has taken the handle.
    replacement: Option<Database>,
    fingerprint: Fingerprint,
    counts: CopyCounts,
    bytes_before: u64,
    backup: Option<PathBuf>,
}

impl Drop for PreparedCompaction {
    fn drop(&mut self) {
        if self.replacement.is_none() {
            return;
        }
        // Release redb's lock on the temp inode before unlinking it.
        self.replacement = None;
        if self.tmp_path.exists()
            && let Err(e) = std::fs::remove_file(&self.tmp_path)
        {
            tracing::warn!(
                path = %self.tmp_path.display(),
                "#6652: could not remove an abandoned compaction temp file: {e}"
            );
        }
    }
}

/// Rewrite the store's file into a verified `.compacting` sibling.
///
/// Why/What: see the module doc. Takes no lock; the live file is only READ.
/// Every failure returns `Err` with `kg.redb` byte-identical to how it was
/// found, because the only write this function makes to the live directory is
/// the backup and the temp file, and neither shares the live file's name.
/// Test: `compaction_shrinks_the_file_and_keeps_live_rows`,
/// `a_backup_write_failure_aborts_before_the_copy_starts`,
/// `unknown_table_aborts_the_compaction`.
pub fn prepare(
    store: &KgStoreRedb,
    plan: CompactPlan,
    hook: Option<&CompactFaultHook>,
) -> Result<PreparedCompaction> {
    if store.is_read_only() {
        anyhow::bail!(
            "refusing to compact {}: this handle is a read-only snapshot, so the rewrite \
             would replace the live file with a copy of a copy",
            store.path().display()
        );
    }
    let live_path = store.path().to_path_buf();
    let tmp_path = sibling(&live_path, COMPACTING_SUFFIX);
    let bytes_before = file_len(&live_path)?;
    let live = store.db();

    guard_unknown_tables(&live)?;
    let fingerprint = fingerprint(&live)?;

    // A stale `.compacting` from a killed process is expected, not corruption.
    // Remove it before the new attempt rather than reusing or renaming it.
    if tmp_path.exists() {
        std::fs::remove_file(&tmp_path)
            .with_context(|| format!("remove stale {}", tmp_path.display()))?;
    }

    let backup = if plan.keep_backup {
        Some(write_backup(&live_path, bytes_before)?)
    } else {
        None
    };
    fire(hook, CompactStep::AfterBackup)?;

    let replacement = Database::create(&tmp_path)
        .with_context(|| format!("create compaction target {}", tmp_path.display()))?;

    // Build the guard NOW, before anything that can fail. Every error below
    // drops it, and its `Drop` releases redb's lock on the temp inode and
    // unlinks the file — which is what makes "an abort leaves the directory as
    // it found it" true rather than merely intended. Assembling it at the end
    // instead left a `.compacting` orphan behind every failed attempt.
    let mut prepared = PreparedCompaction {
        live_path,
        tmp_path,
        replacement: Some(replacement),
        fingerprint,
        counts: CopyCounts::default(),
        bytes_before,
        backup,
    };
    fill_and_verify(&mut prepared, &live, plan, hook)?;
    Ok(prepared)
}

/// Do the copy, the fsync, and both verifications into an already-built guard.
///
/// Why: see the guard-construction comment in [`prepare`]. Every `?` here
/// unwinds through `PreparedCompaction::drop`, so no failure path has to
/// remember to clean the temp file up.
/// Test: `a_crash_before_the_rename_leaves_the_original_untouched`.
fn fill_and_verify(
    prepared: &mut PreparedCompaction,
    live: &Arc<Database>,
    plan: CompactPlan,
    hook: Option<&CompactFaultHook>,
) -> Result<()> {
    let replacement = prepared
        .replacement
        .as_ref()
        .context("compaction target already taken")?;
    let counts = {
        let rtx = live.begin_read().context("begin compaction source read")?;
        copy_all(&rtx, replacement, plan.history_cutoff_ms)?
    };
    fire(hook, CompactStep::AfterCopy)?;

    sync_path(&prepared.tmp_path)?;
    sync_parent_dir(&prepared.tmp_path)?;
    fire(hook, CompactStep::AfterFsync)?;

    verify_copy(replacement, &prepared.fingerprint, counts)?;
    verify_source_unchanged(live, &prepared.fingerprint)
        .context("source changed during the rewrite; refusing to swap")?;
    prepared.counts = counts;
    Ok(())
}

impl PreparedCompaction {
    /// Re-check, rename, and install the replacement handle, atomically.
    ///
    /// Why (#6652, code-critic BLOCK on effb8c343): this is the only step that
    /// needs exclusivity, and it needs the RIGHT exclusivity. The palace write
    /// mutex is not it: `KgWriter`'s actor commits on a `spawn_blocking` thread
    /// holding only `Arc<KgStoreRedb>`, so `KnowledgeGraph::assert` / `retract`
    /// never take that mutex. A commit landing between the fingerprint re-check
    /// and the `rename` wrote to the inode the rename was about to unlink, and
    /// the caller was told it succeeded. The three steps therefore run inside
    /// [`KgStoreRedb::swap_database_exclusively`], which holds the store's own
    /// `swap_lock` — the lock every kg.redb write transaction takes — across
    /// all three. One `rename` syscall plus one pointer store: microseconds,
    /// nowhere near any write budget.
    /// What: re-fingerprints the source (a write that landed since [`prepare`]
    /// aborts here with the live file untouched), renames the temp file over
    /// `kg.redb`, and installs the ALREADY-OPEN replacement into the shared
    /// [`super::types::KgDbState`]. That ordering closes the stale-handle hole:
    /// the replacement is opened before the rename, so after the rename the
    /// only remaining step is an infallible pointer store.
    /// Test: `a_kg_writer_commit_inside_the_swap_window_is_never_dropped`,
    /// `a_write_during_the_copy_aborts_the_swap`,
    /// `compaction_swaps_the_live_handle_in_place`,
    /// `a_crash_between_rename_and_install_recovers_on_reopen`.
    pub fn commit(
        mut self,
        store: &KgStoreRedb,
        hook: Option<&CompactFaultHook>,
    ) -> Result<CompactOutcome> {
        let replacement = self
            .replacement
            .take()
            .context("compaction already committed")?;
        let tmp_path = self.tmp_path.clone();
        let live_path = self.live_path.clone();
        let fingerprint = &self.fingerprint;

        store.swap_database_exclusively(move || {
            // Inside the exclusion: no kg.redb write transaction can be open,
            // and none can start until this closure returns. The re-check and
            // the rename are therefore indivisible.
            verify_source_unchanged(&store.db(), fingerprint)
                .context("a write landed between the rewrite and the swap; refusing to swap")?;
            fire(hook, CompactStep::BeforeRename)?;
            std::fs::rename(&tmp_path, &live_path).with_context(|| {
                format!("rename {} onto {}", tmp_path.display(), live_path.display())
            })?;
            fire(hook, CompactStep::AfterRename)?;
            Ok(replacement)
        })?;

        // Past the point of no return: the rename committed and the handle is
        // swapped, so the compaction SUCCEEDED. Neither the directory fsync nor
        // the size re-stat can un-do that, and returning `Err` for either would
        // make the dreamer log "the live file is unchanged" — false — and
        // record zero reclaimed bytes for a run that reclaimed them. Both
        // degrade to a warning and a best-effort number.
        if let Err(e) = sync_parent_dir(&self.live_path) {
            tracing::warn!(
                path = %self.live_path.display(),
                "#6652: swap committed but the directory fsync failed: {e:#}"
            );
        }
        let bytes_after = file_len(&self.live_path).unwrap_or_else(|e| {
            tracing::warn!(
                path = %self.live_path.display(),
                "#6652: swap committed but the size re-stat failed; reporting 0: {e:#}"
            );
            0
        });
        Ok(CompactOutcome {
            bytes_before: self.bytes_before,
            bytes_after,
            rows_copied: self.counts.rows_copied,
            history_rows_pruned: self.counts.history_rows_pruned,
            backup: self.backup.take(),
        })
    }

    /// Rows the rewrite would keep and history rows it would drop.
    ///
    /// Why: a caller that prepared and then decided not to swap (a `--dry-run`
    /// that still wanted real numbers) needs the counts without the rename.
    /// Test: `dry_run_prepares_nothing_and_writes_no_bytes`.
    pub fn counts(&self) -> (u64, u64) {
        (self.counts.rows_copied, self.counts.history_rows_pruned)
    }
}

/// Abort when the file carries a table this code does not know how to copy.
///
/// Why: redb cannot iterate a table whose types it does not know, so a rewrite
/// can only move tables named in [`known_table_names`]. A table outside that
/// list would be dropped by the swap. Failing closed turns silent data loss
/// into a logged refusal with the live file untouched.
/// Test: `unknown_table_aborts_the_compaction`.
fn guard_unknown_tables(db: &Database) -> Result<()> {
    let rtx = db.begin_read().context("begin table-inventory read")?;
    let known = known_table_names();
    let unknown: Vec<String> = rtx
        .list_tables()
        .context("list tables before compaction")?
        .map(|h| h.name().to_string())
        .filter(|n| !known.contains(&n.as_str()))
        .collect();
    if !unknown.is_empty() {
        anyhow::bail!(
            "refusing to compact: {} carries table(s) this build cannot copy ({}); the \
             rewrite would drop them, so nothing was touched",
            "kg.redb",
            unknown.join(", ")
        );
    }
    Ok(())
}

/// Per-table `(rows, stored, metadata)` for every table in `db`.
fn fingerprint(db: &Database) -> Result<Fingerprint> {
    let rtx = db.begin_read().context("begin fingerprint read")?;
    fingerprint_of(&rtx)
}

/// [`fingerprint`] against an already-open read transaction.
fn fingerprint_of(rtx: &redb::ReadTransaction) -> Result<Fingerprint> {
    let handles: Vec<redb::UntypedTableHandle> = rtx
        .list_tables()
        .context("list tables for fingerprint")?
        .collect();
    let mut out = Fingerprint::with_capacity(handles.len());
    for handle in handles {
        let name = handle.name().to_string();
        let table = rtx
            .open_untyped_table(handle)
            .with_context(|| format!("open {name} for fingerprint"))?;
        let st = table.stats().context("read table stats")?;
        out.push((
            name,
            table.len().context("read table row count")?,
            st.stored_bytes(),
            st.metadata_bytes(),
        ));
    }
    out.sort();
    Ok(out)
}

/// Confirm the rewrite holds exactly the rows it should.
///
/// Why: the rename is irreversible from the process's point of view — the old
/// inode is unlinked the instant it happens. Everything checkable is therefore
/// checked before it, and a mismatch aborts rather than logs.
/// What: every table's row count in the copy must equal the source's, minus the
/// history rows the plan deliberately pruned from `TRIPLES`, and the dropped
/// tables must be absent.
/// Test: `compaction_preserves_every_live_row`.
fn verify_copy(dest: &Database, source: &Fingerprint, counts: CopyCounts) -> Result<()> {
    use crate::memory_core::store::kg_store::TRIPLES;
    let rtx = dest.begin_read().context("begin copy-verify read")?;
    let copy = fingerprint_of(&rtx)?;
    let dropped = super::copy_tables::DROPPED_TABLES;
    for (name, rows, _, _) in source {
        if dropped.contains(&name.as_str()) {
            if copy.iter().any(|(n, r, _, _)| n == name && *r > 0) {
                anyhow::bail!("compaction copied {name}, which it was supposed to drop");
            }
            continue;
        }
        let expected = if name == TRIPLES.name() {
            rows.saturating_sub(counts.history_rows_pruned)
        } else {
            *rows
        };
        let actual = copy
            .iter()
            .find(|(n, _, _, _)| n == name)
            .map_or(0, |(_, r, _, _)| *r);
        if actual != expected {
            anyhow::bail!(
                "compaction row-count mismatch on {name}: expected {expected}, copy has \
                 {actual}; the live file was not touched"
            );
        }
    }
    Ok(())
}

/// Confirm nothing wrote to the source since its fingerprint was taken.
///
/// Test: `a_write_during_the_copy_aborts_the_swap`.
fn verify_source_unchanged(db: &Database, expected: &Fingerprint) -> Result<()> {
    let now = fingerprint(db)?;
    if &now != expected {
        anyhow::bail!(
            "kg.redb changed during the rewrite (table fingerprint differs); the swap was \
             abandoned and the live file is unchanged"
        );
    }
    Ok(())
}

/// Copy the live file to its `.pre-compact.bak` sibling and verify the size.
///
/// Why: a backup that was itself truncated is worse than none, because an
/// operator will trust it. A failed or short copy aborts the compaction BEFORE
/// the temp file is created, so a palace with no room for a backup is a palace
/// that simply does not get compacted.
/// What: removes the previous backup first — keeping every generation would
/// defeat the point of shrinking the data directory — then copies and re-stats.
/// Test: `a_backup_write_failure_aborts_before_the_copy_starts`.
fn write_backup(live: &Path, expect_len: u64) -> Result<PathBuf> {
    let backup = sibling(live, BACKUP_SUFFIX);
    if backup.exists() {
        std::fs::remove_file(&backup)
            .with_context(|| format!("remove previous backup {}", backup.display()))?;
    }
    std::fs::copy(live, &backup)
        .with_context(|| format!("back up {} to {}", live.display(), backup.display()))?;
    let got = file_len(&backup)?;
    if got != expect_len {
        let _ = std::fs::remove_file(&backup);
        anyhow::bail!(
            "pre-compaction backup is {got} bytes but {} is {expect_len}; refusing to \
             compact without a verified backup",
            live.display()
        );
    }
    Ok(backup)
}

/// `<path><suffix>` in the same directory, so `rename` stays atomic.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// `metadata(path).len()`, with the path in the error.
fn file_len(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len())
}

/// fsync one file.
///
/// Why: redb commits durably, but the compaction's own guarantee should not
/// depend on redb's default durability staying what it is today.
fn sync_path(path: &Path) -> Result<()> {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("open {} for fsync", path.display()))?;
    f.sync_all()
        .with_context(|| format!("fsync {}", path.display()))
}

/// fsync the directory entry, so the rename itself survives a power loss.
fn sync_parent_dir(path: &Path) -> Result<()> {
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    let f =
        std::fs::File::open(dir).with_context(|| format!("open {} for fsync", dir.display()))?;
    // A directory fsync is not portable to every filesystem; a failure here is
    // not a reason to abandon a rewrite that is otherwise complete.
    if let Err(e) = f.sync_all() {
        tracing::debug!(dir = %dir.display(), "#6652: directory fsync unsupported: {e}");
    }
    Ok(())
}

/// Run the fault hook for `step`, if one is installed.
fn fire(hook: Option<&CompactFaultHook>, step: CompactStep) -> Result<()> {
    match hook {
        Some(h) => h(step),
        None => Ok(()),
    }
}
