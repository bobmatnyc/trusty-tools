//! Per-path serialization + panic-safety guard for opening a corpus redb file
//! under concurrency (issue #3659).
//!
//! Why: warm-boot (eager `restore_indexes`), lazy load (`get_or_load_index`),
//! and the `POST /indexes` create/relocate handlers all eventually call
//! `CorpusStore::open` for a given index's `index.redb` via
//! `persistence_loader::build_indexer_from_entry`. Before an index is
//! registered in `IndexRegistry`, none of the existing dedup guards can see
//! it: the lazy-load path's per-index `Mutex` (`ColdIndexStore::loading_gate`)
//! only covers callers that go through `get_or_load_index`, and the
//! root-path-collision guard in `server::helpers` only rejects a second
//! *registered* handle — it can't see an in-flight, not-yet-registered
//! restore. So an eager warm-boot restore of index `main` racing an explicit
//! `POST /indexes` (re)create of `main` moments after daemon start — before
//! warm-boot has finished registering it — hits `Database::create`/`open` on
//! the SAME on-disk file from two tasks at once with nothing serializing them.
//!
//! redb's on-disk format is not safe to read while another handle is
//! mid-`Database::create`, writing its header / page-manager metadata. A torn
//! read does not always surface as a clean `DatabaseError` — it can trip an
//! internal `assert!`/bounds check in redb's `page_manager` and PANIC (issue
//! #3659: `page_manager.rs:243`, recurring across ~0.29–0.36.1 despite
//! #702/#703's graceful-old-format handling, which only covers a *complete*,
//! non-racing file — it classifies `DatabaseError` variants, but a page-level
//! panic during a torn concurrent read never reaches that classifier).
//!
//! What: [`open_serialized`] closes this gap with two independent guards: (1)
//! a process-wide keyed async mutex so at most one task ever has an open call
//! in flight for a given (best-effort-canonicalized) path at a time — every
//! other task awaits the same outcome instead of racing the bytes; (2)
//! `catch_unwind` around the blocking redb call (still possibly panicking on
//! a genuinely corrupt file even with no racing reader), converting any panic
//! into a typed `Err` so the caller's existing migration/rebuild retry ladder
//! (`persistence_loader::restore_corpus_for_entry`) always sees a normal
//! `Result` — never a raw panic — regardless of which failure mode tripped.
//! The gate registry never shrinks (a handful of entries per registered index
//! for the process lifetime; negligible memory) — see `gate_key` for the
//! canonicalization caveat on a not-yet-existing path.
//!
//! Test: `tests` covers (a) N tasks concurrently opening the same path never
//! panic — exactly one wins with a live store, the rest fail cleanly with
//! `DatabaseAlreadyOpen`; (b) a closure that panics (simulating redb's
//! internal `page_manager` assertion) is converted to a typed `Err`, never a
//! raw panic; (c) two DIFFERENT paths do not serialize against each other.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use anyhow::{anyhow, Result};
use tokio::sync::Mutex as AsyncMutex;

/// Process-wide registry of per-canonical-path async mutexes.
fn open_gates() -> &'static StdMutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>> {
    static GATES: OnceLock<StdMutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    GATES.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Best-effort canonicalize `path` for use as a gate key.
///
/// Why: `std::fs::canonicalize` fails for a not-yet-created file (the common
/// first-boot case for a fresh index) — falling back to the given path keeps
/// the gate usable even then. Two callers racing to CREATE the same
/// not-yet-existing path with the identical `PathBuf` spelling (the realistic
/// warm-boot-vs-create-handler scenario — both resolve the path via the same
/// `corpus_redb_path_for_entry` helper) still serialize correctly, since both
/// fall back to the same literal key. Two callers racing via two DIFFERENT
/// spellings of a not-yet-existing path (an exotic symlink-not-yet-resolvable
/// case) would not dedupe — an accepted, documented gap mirroring the same
/// canonicalization caveat in `server::helpers::find_root_path_collision`.
fn gate_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Fetch (creating if absent) the async mutex guarding `path`.
fn gate_for(path: &Path) -> Arc<AsyncMutex<()>> {
    let key = gate_key(path);
    let mut gates = open_gates().lock().unwrap_or_else(|e| e.into_inner());
    gates
        .entry(key)
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

/// Extract a human-readable message from a caught panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Run `open_fn` (a blocking redb open/create call) serialized per-path and
/// panic-safe (issue #3659).
///
/// Why/What: see module docs. `open_fn` is the exact `CorpusStore::open` (or
/// `open_fresh`) call the caller would otherwise have run directly; this
/// wrapper adds the per-path gate and the panic→`Err` boundary without
/// changing what gets opened.
/// Test: `tests::concurrent_opens_of_same_path_serialize_without_panicking`,
/// `tests::panicking_open_becomes_typed_err_not_a_panic`,
/// `tests::different_paths_do_not_serialize`.
pub(crate) async fn open_serialized<F, T>(path: &Path, open_fn: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let gate = gate_for(path);
    let _permit = gate.lock().await;

    let path_display = path.display().to_string();
    let join_result =
        tokio::task::spawn_blocking(move || std::panic::catch_unwind(AssertUnwindSafe(open_fn)))
            .await;

    match join_result {
        Ok(Ok(inner)) => inner,
        Ok(Err(panic_payload)) => {
            let msg = panic_message(panic_payload.as_ref());
            tracing::error!(
                path = %path_display,
                "corpus open task PANICKED inside redb ({msg}) — converting to a typed \
                 error so the migration/rebuild retry ladder handles it as a normal Err \
                 (issue #3659)"
            );
            Err(anyhow!(
                "redb corpus open panicked for {path_display}: {msg}"
            ))
        }
        Err(join_err) => {
            tracing::error!(
                path = %path_display,
                "corpus open blocking task could not be joined ({join_err}) — \
                 treating as an open failure (issue #3659)"
            );
            Err(anyhow!(
                "redb corpus open task join failure for {path_display}: {join_err}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::corpus::CorpusStore;
    use std::io::Write;
    use tempfile::tempdir;

    /// Why: the load-bearing regression test for #3659 — N tasks racing to
    /// open/create the SAME redb path must never PANIC. redb itself only
    /// ever allows one *live* `Database` handle per file (a second `open()`
    /// while the first is still held returns the classified
    /// `DatabaseAlreadyOpen` error, by design — see
    /// `corpus_recovery::tests::database_already_open_variant_is_stable`), so
    /// the correct end state for N racing openers is exactly one `Ok` (the
    /// winner, whose store is kept alive for the duration of the test) and
    /// the rest a clean `Err` — never a raw panic from a torn concurrent
    /// read of a half-initialized file. Without the per-path gate, that torn
    /// read is exactly what trips redb's internal `page_manager` assertion
    /// (issue #3659); with it, every loser's `CorpusStore::open` call only
    /// ever runs after the winner's has fully completed, so it always sees a
    /// byte-consistent file (either not-yet-existing or fully committed) and
    /// fails cleanly via `DatabaseAlreadyOpen` instead.
    /// What: spawns 8 tokio tasks that all call `open_serialized` on the same
    /// not-yet-existing path at once; asserts none of them panicked, exactly
    /// one succeeded, and the losers all report `DatabaseAlreadyOpen`.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_opens_of_same_path_serialize_without_panicking() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("shared.redb");

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let p_ref = path.clone();
            let p_open = path.clone();
            tasks.push(tokio::spawn(async move {
                open_serialized(&p_ref, move || CorpusStore::open(&p_open)).await
            }));
        }
        let mut oks = 0;
        let mut already_open_errs = 0;
        let mut stores = Vec::new();
        for task in tasks {
            // `task.await` itself yields `Err` only on a PANIC inside the
            // spawned task — the key assertion. `open_serialized` must never
            // let a panic escape, so every `task.await` here must be `Ok`.
            let result = task
                .await
                .expect("open_serialized must never let a panic escape the task (issue #3659)");
            match result {
                Ok(store) => {
                    oks += 1;
                    stores.push(store); // keep alive so later contenders see DatabaseAlreadyOpen
                }
                Err(e) => {
                    let is_already_open = e
                        .downcast_ref::<redb::DatabaseError>()
                        .map(|db_err| matches!(db_err, redb::DatabaseError::DatabaseAlreadyOpen))
                        .unwrap_or(false);
                    assert!(
                        is_already_open,
                        "a losing opener must fail cleanly with DatabaseAlreadyOpen, not some \
                         other error (which would indicate a torn/racing read): {e}"
                    );
                    already_open_errs += 1;
                }
            }
        }
        assert_eq!(oks, 1, "exactly one racing opener must win the file");
        assert_eq!(
            already_open_errs, 7,
            "every other racing opener must fail cleanly with DatabaseAlreadyOpen"
        );
    }

    /// Why: a garbage/truncated file that trips a genuine redb-internal panic
    /// (not merely a classified `DatabaseError`) must still surface as a
    /// typed `Err` to the caller — never as a raw unwind — so the existing
    /// migration/rebuild retry ladder can consume it like any other failure.
    /// What: writes garbage bytes, then calls `open_serialized` with a closure
    /// that force-panics (simulating a redb-internal panic on a corrupt page)
    /// instead of returning an `Err`; asserts the wrapper still returns `Err`
    /// and the calling task itself never panics.
    /// Test: this test.
    #[tokio::test]
    async fn panicking_open_becomes_typed_err_not_a_panic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("garbage.redb");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&[0xAB_u8; 4096]))
            .unwrap();

        let result: Result<()> = open_serialized(&path, || -> Result<()> {
            panic!("simulated redb page_manager.rs:243 assertion (issue #3659)");
        })
        .await;

        assert!(
            result.is_err(),
            "a panicking open must be converted to Err, not propagate as a panic"
        );
    }

    /// Why: the gate must be scoped per-path — two callers opening two
    /// DIFFERENT redb files must never block on each other.
    /// What: opens two distinct paths concurrently via `open_serialized` and
    /// asserts both succeed (a shared/global lock covering all paths would
    /// still pass this test functionally, so this is a sanity check that the
    /// per-path design doesn't regress correctness; the concurrency benefit
    /// is structural, not asserted via timing here).
    /// Test: this test.
    #[tokio::test]
    async fn different_paths_do_not_serialize() {
        let dir = tempdir().unwrap();
        let path_a = dir.path().join("a.redb");
        let path_b = dir.path().join("b.redb");

        let a_ref = path_a.clone();
        let a_open = path_a.clone();
        let b_ref = path_b.clone();
        let b_open = path_b.clone();
        let (ra, rb) = tokio::join!(
            open_serialized(&a_ref, move || { CorpusStore::open(&a_open) }),
            open_serialized(&b_ref, move || { CorpusStore::open(&b_open) })
        );
        assert!(ra.is_ok(), "path a must open successfully");
        assert!(rb.is_ok(), "path b must open successfully");
    }
}
