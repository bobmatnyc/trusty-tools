//! `KgStoreRedb` struct definition and lifecycle (open / accessors).
//!
//! Why: Separating struct definition and `open` from the read/write methods
//! keeps each file under the 500-SLOC cap while remaining coherent units.
//! What: Defines `KgStoreRedb`, implements `open`, `is_read_only`, `db`,
//! and `check_writable`.
//! Test: `open_then_reopen_persists_state`, `write_on_snapshot_returns_read_only_error`.

use crate::memory_core::store::concurrent_open::{
    OpenIntent, OpenMode, backoff_sleep_ms, is_incompatible_format_refusal, try_open_or_snapshot,
};
use crate::memory_core::store::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DRAWERS, DRAWERS_BY_FACT_KEY, KG_SCHEMA, ROOM_KEYS, ROOMS, TRIPLES,
    TRIPLES_BY_OBJECT, WING_KEYS, WINGS,
};
use anyhow::{Context, Result};
use redb::Database;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::migrate::migrate_triple_keys_fail_open;
use super::types::{GuardedWrite, KgDbState, READ_ONLY_ERROR_MSG, canonical_key, db_cache};

/// Why: All KG callers go through a single `KnowledgeGraph` handle that is
/// cheap to clone and Send + Sync. Holding `Arc<Database>` lets background
/// tasks (Dreamer, compaction) share the same db without re-opening.
/// What: Owns the redb `Database` plus the on-disk path for diagnostics.
/// Test: Implicit — every test below constructs one.
#[derive(Clone)]
pub struct KgStoreRedb {
    pub(super) state: Arc<KgDbState>,
    pub(super) path: PathBuf,
}

impl KgStoreRedb {
    /// Open or create the redb database at `path` with read-only-client intent.
    ///
    /// Why: Preserves the historical zero-config signature for the many
    /// CLI / read / test call sites. Snapshot-fallback on a cross-process
    /// lock (issue #59) is the right behaviour for those callers.
    /// What: Delegates to [`KgStoreRedb::open_with_intent`] with
    /// [`OpenIntent::ReadOnlyClient`].
    /// Test: `open_then_reopen_persists_state`.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_intent(path, OpenIntent::ReadOnlyClient)
    }

    /// Open or create the redb database at `path` with the caller's intent.
    ///
    /// Why (issue #1487): the HTTP daemon is the sole writer and must open
    /// with [`OpenIntent::Writer`] so a second instance fails LOUD (after a
    /// bounded handoff-retry window) instead of silently degrading to a
    /// read-only snapshot and rejecting every write for its lifetime.
    /// Read-only clients (stdio MCP, CLI, tests) keep
    /// [`OpenIntent::ReadOnlyClient`] so they can serve reads from a snapshot
    /// while the daemon holds the live lock.
    /// What: Creating the file plus initializing every table must be
    /// idempotent so daemon restarts succeed without manual setup. redb's
    /// `Database::create` opens an existing file or creates a fresh one. For
    /// `ReadOnlyClient`, a short in-process TOCTOU retry loop (issue #1152)
    /// absorbs an aborting async writer dropping the lock. For `Writer`, the
    /// bounded handoff retry lives inside `try_open_or_snapshot` itself, so
    /// this function makes a single intent-passing call and does not double-
    /// retry. Opens the file, then in a single write transaction touches
    /// every table so the file always carries a stable schema even when no
    /// data has been written.
    /// Test: `open_then_reopen_persists_state`,
    /// `writer_intent_open_fails_loud_on_locked_file`.
    pub fn open_with_intent(path: &Path, intent: OpenIntent) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create kg db parent dir {}", parent.display()))?;
        }

        // TOCTOU note (issue #1152, Tier 3):
        // redb's `DatabaseAlreadyOpen` can fire in-process when an async
        // `KgWriter::spawn` task is aborting (holding `Arc<KgStoreRedb>` and
        // hence the redb flock) while a separate blocking thread races to
        // re-open the same file. The abort is triggered synchronously in
        // `AbortDropGuard::drop`, but the tokio runtime only drops the task's
        // captured state asynchronously (at the next scheduling point). In a
        // `current_thread` test runtime the abort may take up to ~50 ms to
        // process while `spawn_blocking` threads run concurrently.
        //
        // Strategy: check the db_cache first (fast, no I/O). If the Weak is
        // dead, attempt `try_open_or_snapshot` with `OpenIntent::ReadOnlyClient`.
        // On failure (another process holds the lock), the function falls back
        // to a read-only snapshot — writes against that snapshot are rejected
        // with the `READ_ONLY_ERROR_MSG` guard. For in-process TOCTOU races
        // (an aborting async task), the cache check on the next retry cycle
        // will find the Weak has been re-inserted or will succeed opening fresh.
        //
        // Retry schedule: exponential backoff 2/10/50/100 ms. Total max wait
        // 162 ms — invisible at daemon startup, sufficient for async-drop.
        // This outer loop ONLY covers the in-process TOCTOU race for the
        // `ReadOnlyClient` path. For the `Writer` path the authoritative
        // bounded handoff retry lives inside `try_open_or_snapshot`
        // (issue #1487), so we make a single attempt here and never
        // double-retry — otherwise the ~1.55 s writer window would be
        // multiplied 5×.
        const RETRIES: u8 = 4;
        const RETRY_SLEEP_MS: [u64; 4] = [2, 10, 50, 100];
        // Writer → 0 extra attempts (handoff retry lives in `try_open_or_snapshot`).
        // ReadOnlyClient → RETRIES in-process TOCTOU retries (issue #1152). The
        // `u8::from(bool)` keeps this a single non-`if` expression (fmt-stable).
        let max_attempts = RETRIES * u8::from(intent != OpenIntent::Writer);

        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..=max_attempts {
            // Check in-process cache first — avoids re-opening the same
            // redb file from two code paths within one process.
            {
                let mut cache = db_cache().lock().expect("db_cache poisoned");
                let key = canonical_key(path);
                if let Some(weak) = cache.get(&key)
                    && let Some(state) = weak.upgrade()
                {
                    return Ok(Self {
                        state,
                        path: path.to_path_buf(),
                    });
                }
                cache.remove(&key);
            }

            // Attempt an exclusive open. On cross-process `DatabaseAlreadyOpen`
            // we fall back to a read-only snapshot (issue #59); writes to the
            // snapshot are rejected via `READ_ONLY_ERROR_MSG`. In-process TOCTOU
            // races resolve on the next retry cycle (the aborting task drops
            // the lock within the exponential-backoff window).
            match try_open_or_snapshot(path, intent) {
                Ok((db, snapshot_guard, mode)) => {
                    // Touch every table in a single write txn so they exist
                    // on disk even before the first write. Skip in snapshot
                    // mode because (a) the live file already initialised every
                    // table — we copied a fully-formed redb image — and (b)
                    // any write here would only land in the throw-away
                    // snapshot, masking the read-only intent of every later
                    // write rejection. #702: Recreated inits like ReadWrite.
                    if matches!(mode, OpenMode::ReadWrite | OpenMode::Recreated) {
                        let wtx = db.begin_write().context("begin init txn")?;
                        {
                            let _ = wtx.open_table(TRIPLES).context("init triples table")?;
                            let _ = wtx
                                .open_table(TRIPLES_BY_OBJECT)
                                .context("init triples_by_object table")?;
                            let _ = wtx
                                .open_table(ACTIVE_SUBJECT_COUNTS)
                                .context("init active_subject_counts table")?;
                            let _ = wtx.open_table(DRAWERS).context("init drawers table")?;
                            // #4884: the slot index is initialised in the SAME
                            // transaction as DRAWERS for the reason ADR-0027
                            // gives for ROOMS — a palace can never present
                            // drawers without the table that indexes their
                            // slots, so no reader has to handle a missing one.
                            let _ = wtx
                                .open_table(DRAWERS_BY_FACT_KEY)
                                .context("init drawers_by_fact_key table")?;
                            // ADR-0027 T1: the room registry is initialised in
                            // the SAME transaction as DRAWERS so the schema is
                            // always whole — a palace can never present drawers
                            // without the tables that name their rooms.
                            let _ = wtx.open_table(ROOMS).context("init rooms table")?;
                            let _ = wtx.open_table(ROOM_KEYS).context("init room_keys table")?;
                            // ADR-0027 T9: same whole-schema rule for the wing
                            // registry — a palace can never present rooms
                            // without the tables that scope them.
                            let _ = wtx.open_table(WINGS).context("init wings table")?;
                            let _ = wtx.open_table(WING_KEYS).context("init wing_keys table")?;
                            // #4810: same whole-schema rule — the migration
                            // gate reads this table on every open, so it must
                            // exist before anything asks.
                            let _ = wtx.open_table(KG_SCHEMA).context("init kg_schema table")?;
                        }
                        wtx.commit().context("commit init txn")?;

                        // #4810: rewrite pre-object triple keys once per
                        // palace. Deliberately fail-open — a palace that cannot
                        // be migrated still opens and behaves exactly as it did
                        // before #4810. Snapshot mode never reaches here: a
                        // migration written into a throw-away snapshot would be
                        // discarded and re-attempted on every open.
                        migrate_triple_keys_fail_open(&db, path);
                    }

                    let state = Arc::new(KgDbState {
                        db: std::sync::RwLock::new(db),
                        swap_lock: std::sync::RwLock::new(()),
                        mode,
                        _snapshot_guard: snapshot_guard,
                    });
                    {
                        let mut cache = db_cache().lock().expect("db_cache poisoned");
                        // Use the post-create canonical path so symlinks
                        // resolve correctly on macOS (/var → /private/var).
                        let key = canonical_key(path);
                        cache.insert(key, Arc::downgrade(&state));
                    }
                    return Ok(Self {
                        state,
                        path: path.to_path_buf(),
                    });
                }
                Err(e) => {
                    // #4911: an incompatible on-disk format never resolves by
                    // waiting, unlike the lock races this loop exists for.
                    if is_incompatible_format_refusal(&e) {
                        return Err(e);
                    }
                    last_err = Some(e);
                    if attempt < max_attempts {
                        // Exponential backoff: let any concurrent in-process
                        // `Database::drop` (or async `KgWriter` abort) finish
                        // releasing the OS file lock before retrying.
                        //
                        // This function is sync and may be called from an async
                        // context (e.g. via `PalaceHandle::open` from an axum
                        // handler). `backoff_sleep_ms` uses
                        // `tokio::task::block_in_place` on multi-thread runtimes
                        // so the executor can schedule other tasks during the wait
                        // instead of starving them on the blocked worker thread.
                        let sleep_ms = RETRY_SLEEP_MS[attempt as usize];
                        backoff_sleep_ms(sleep_ms);
                    }
                }
            }
        }
        Err(last_err.expect("at least one attempt was made"))
    }

    /// Whether this store is operating against a read-only snapshot.
    ///
    /// Why: Issue #59 — `KnowledgeGraph` exposes this through to
    /// `PalaceHandle::is_read_only` so write paths can short-circuit
    /// before touching the store. Cheap field read, no I/O.
    /// What: Returns `true` when the underlying database was opened via
    /// the snapshot fallback rather than directly.
    /// Test: `write_on_snapshot_returns_read_only_error`.
    pub fn is_read_only(&self) -> bool {
        self.state.mode.is_read_only()
    }

    /// Internal accessor used by every method that previously read
    /// `self.db`. Centralising it lets the cache and snapshot guard live
    /// inside `KgDbState` without rewriting every call site.
    ///
    /// #6652: returns an owned `Arc<Database>` rather than a borrow, because
    /// the handle is now swappable (see [`KgDbState::db`]). Call sites are
    /// unchanged — `self.db().begin_read()` auto-derefs through the `Arc`, and
    /// redb's `ReadTransaction`/`WriteTransaction` are owned, so the temporary
    /// may drop while the transaction lives.
    pub(super) fn db(&self) -> Arc<Database> {
        self.state.db()
    }

    /// The canonical on-disk path this store was opened from.
    ///
    /// Why (#6652): the compaction path needs the file's directory to place the
    /// `.compacting` and `.pre-compact.bak` siblings, and the `path` field is
    /// crate-private.
    /// What: clone of the path passed to `open_with_intent`.
    /// Test: `kg_redb_path_reports_the_open_path`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current `Database`, for crate-internal tests (#6652).
    ///
    /// Why: the dream-level compaction tests have to seed `hist:` rows directly
    /// — the real `retract` path closes every row at `now`, so history built
    /// through it can never be old enough to cross the age gate. That needs a
    /// write transaction against the same file the phase will rewrite, and
    /// `db()` is module-private.
    /// What: the same `Arc<Database>` clone `db()` returns. `pub(crate)`, so it
    /// is not part of the crate's public API.
    /// Test: `kg_compaction_shrinks_the_file_in_a_dream_cycle`.
    #[cfg(test)]
    pub(crate) fn db_for_test(&self) -> Arc<Database> {
        self.db()
    }

    /// Begin a write transaction that the compaction swap cannot race.
    ///
    /// Why (#6652, code-critic BLOCK on effb8c343): every kg.redb write in this
    /// crate reaches redb through `self.db().begin_write()`, and that is the
    /// only chokepoint all of them share — `KgWriter`'s actor, `apply_batch`,
    /// `import_all`, and the room/wing writers hold no palace handle and take
    /// no palace mutex. Putting the exclusion here catches every writer with no
    /// plumbing, and catches the ones that do not exist yet.
    /// What: takes [`super::types::KgDbState::swap_lock`] for reading FIRST,
    /// then reads the live handle, so a writer that blocked on an in-flight
    /// swap resumes against the NEW file rather than the unlinked one. The
    /// returned [`GuardedWrite`] holds both for the transaction's lifetime.
    /// Test: `a_kg_writer_commit_inside_the_swap_window_is_never_dropped`.
    pub(super) fn begin_write_guarded(&self) -> Result<GuardedWrite<'_>> {
        let swap = self.state.swap_lock.read().expect("kg swap lock poisoned");
        let db = self.state.db();
        let txn = db.begin_write().context("begin kg.redb write txn")?;
        Ok(GuardedWrite {
            _swap: swap,
            _db: db,
            txn,
        })
    }

    /// Wall-clock budget for taking the swap exclusion.
    ///
    /// Why (#6366): the caller holds the palace write mutex across the swap, so
    /// an unbounded wait here would park every `remember` behind whatever
    /// long-running write happens to hold the exclusion — `import_all` can hold
    /// one for a while. Giving up instead turns "the palace is busy" into a
    /// skipped cycle that retries, rather than a stall that surfaces as write
    /// timeouts.
    const SWAP_EXCLUSION_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

    /// Take the swap exclusion, or give up inside the budget.
    ///
    /// Why/What: see [`Self::SWAP_EXCLUSION_BUDGET`]. `std::sync::RwLock` has no
    /// timed acquire, so this polls `try_write`. A poisoned lock means a
    /// previous holder panicked mid-swap — an invariant break, not a runtime
    /// condition, so it propagates as an error rather than being swallowed.
    /// Test: `a_kg_writer_commit_inside_the_swap_window_is_never_dropped`
    /// exercises the contended path.
    fn acquire_swap_exclusion(&self) -> Result<std::sync::RwLockWriteGuard<'_, ()>> {
        let deadline = std::time::Instant::now() + Self::SWAP_EXCLUSION_BUDGET;
        loop {
            match self.state.swap_lock.try_write() {
                Ok(guard) => return Ok(guard),
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    anyhow::bail!("kg swap lock poisoned by a previous swap");
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        anyhow::bail!(
                            "kg.redb writers stayed busy for {:?}; abandoning the compaction \
                             swap with the live file unchanged (it retries next cycle)",
                            Self::SWAP_EXCLUSION_BUDGET
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }

    /// Run the compaction swap with every kg.redb writer excluded (#6652).
    ///
    /// Why: the rename and the handle install have to be indivisible from the
    /// fingerprint re-check that authorised them. Anything committing in
    /// between writes to an inode the rename is about to unlink, and the
    /// caller is told it succeeded — the fail-open the critic blocked on.
    /// What: takes `swap_lock` for writing, runs `swap` (which re-checks,
    /// renames, and hands back the already-open replacement), installs that
    /// replacement, and releases. Every writer is either fully before or fully
    /// after this window. An `Err` from `swap` leaves the handle untouched.
    /// Test: `a_kg_writer_commit_inside_the_swap_window_is_never_dropped`,
    /// `compaction_swaps_the_live_handle_in_place`.
    pub(super) fn swap_database_exclusively<F>(&self, swap: F) -> Result<()>
    where
        F: FnOnce() -> Result<Database>,
    {
        let _exclusive = self.acquire_swap_exclusion()?;
        let replacement = swap()?;
        // Infallible from here: one pointer store into the `KgDbState` every
        // clone of this store shares, so the daemon's own long-lived handle is
        // swapped by this line — not lazily on some later open.
        let mut slot = self
            .state
            .db
            .write()
            .expect("kg db handle lock poisoned during compaction swap");
        *slot = Arc::new(replacement);
        Ok(())
    }

    /// Reject the operation when the store is in snapshot mode.
    ///
    /// Why: Every write path (`assert`, `retract`, drawer upsert/delete)
    /// must surface the same actionable error so users see the same
    /// guidance regardless of which mutation they attempted.
    /// What: Returns `Err(READ_ONLY_ERROR_MSG)` when `is_read_only()`,
    /// otherwise `Ok(())`.
    /// Test: `write_on_snapshot_returns_read_only_error`.
    pub(super) fn check_writable(&self) -> Result<()> {
        if self.is_read_only() {
            Err(anyhow::anyhow!(READ_ONLY_ERROR_MSG))
        } else {
            Ok(())
        }
    }
}
