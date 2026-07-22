//! Per-path serialization + panic-safety guard for opening a corpus redb file
//! under concurrency (issue #3659).
//!
//! Why: warm-boot (eager `restore_indexes`), lazy load (`get_or_load_index`),
//! and the `POST /indexes` create/relocate handlers all eventually call
//! `CorpusStore::open` for a given index's `index.redb` via
//! `persistence_loader::build_indexer_from_entry` (and the reindex atomic-swap
//! path in `service::reindex::corpus_swap` re-opens the same live path after a
//! rename/abort). Before an index is registered in `IndexRegistry`, none of
//! the existing dedup guards can see it: the lazy-load path's per-index
//! `Mutex` (`ColdIndexStore::loading_gate`) only covers callers that go
//! through `get_or_load_index`, and the root-path-collision guard in
//! `server::helpers` only rejects a second *registered* handle — it can't see
//! an in-flight, not-yet-registered restore. So an eager warm-boot restore of
//! index `main` racing an explicit `POST /indexes` (re)create of `main`
//! moments after daemon start — before warm-boot has finished registering it
//! — hits `Database::create`/`open` on the SAME on-disk file from two tasks
//! at once with nothing serializing them.
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
//! What: [`open_serialized`] closes this gap with a per-canonical-path async
//! mutex (at most one opener in flight for a given file) plus a panic→`Err`
//! boundary, with two additional invariants added after adversarial review
//! (issue #3659 PR #3666 code-critic findings):
//!  - **Bounded wait, never a permanent hang.** `spawn_blocking` work can
//!    never be cancelled — if the underlying blocking redb call wedges
//!    forever (e.g. a TCC-denied/blocked volume, issue #718), naively
//!    wrapping "acquire gate + await the blocking call" in one
//!    `tokio::time::timeout` would only bound how long the FIRST caller waits
//!    before giving up; the mutex guard it was holding would then be dropped
//!    (releasing the gate) while the abandoned blocking-pool thread might
//!    still be mid-write on the file — letting a fresh caller start a SECOND
//!    concurrent open and reintroducing this exact race. Instead, the actual
//!    gate-acquire + open runs as an independent [`tokio::spawn`] task that
//!    keeps holding the mutex for as long as it genuinely runs (mirroring
//!    `warm_boot::restore::restore_one_index_bounded`'s accepted-leak pattern
//!    for the same underlying #718 hazard); `tokio::time::timeout` only
//!    bounds how long THIS caller waits for it, and on timeout the path is
//!    marked "wedged" so every later caller fails fast instead of queuing
//!    behind a possibly-forever-running opener.
//!  - **Syscall-isolated, stable gate key.** The gate key is derived by
//!    canonicalizing the file's PARENT directory (never the file itself,
//!    which may not exist yet) inside `spawn_blocking` — never on the calling
//!    async task's thread, which would otherwise be the exact #718 Part-3
//!    worker-starvation class for a TCC-denied volume. See [`gate_key`] for
//!    why the parent (not the full path) is canonicalized.
//!
//! Test: `tests` covers (a) N tasks concurrently opening the same path never
//! panic — exactly one wins with a live store, the rest fail cleanly with
//! `DatabaseAlreadyOpen`; (b) a closure that panics (simulating redb's
//! internal `page_manager` assertion) is converted to a typed `Err`, never a
//! raw panic; (c) two DIFFERENT paths do not serialize against each other;
//! (d) the gate key is stable across the file's own creation (not just at a
//! single instant); (e) a wedged opener times out instead of hanging forever,
//! and a subsequent caller for the same path fails fast rather than queuing.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::Mutex as AsyncMutex;

/// Per-path serialization gate: an async mutex plus a "wedged" latch.
///
/// Why: split out of a bare `Arc<AsyncMutex<()>>` (issue #3659 review finding
/// #1) so a caller that times out waiting for a wedged opener can record that
/// fact for every LATER caller, without needing the mutex itself to ever be
/// released by the wedged attempt (it may never be — `spawn_blocking` work
/// cannot be cancelled).
/// What: `mutex` serializes attempts exactly as before; `wedged` is a
/// one-way latch (never reset — a wedge is treated as permanent for the
/// process lifetime, consistent with #718's accepted-leak policy) checked
/// before a caller even attempts to queue on `mutex`.
struct PathGate {
    mutex: AsyncMutex<()>,
    wedged: AtomicBool,
}

impl PathGate {
    fn new() -> Self {
        Self {
            mutex: AsyncMutex::new(()),
            wedged: AtomicBool::new(false),
        }
    }
}

/// Process-wide registry of per-canonical-path gates.
fn open_gates() -> &'static StdMutex<HashMap<PathBuf, Arc<PathGate>>> {
    static GATES: OnceLock<StdMutex<HashMap<PathBuf, Arc<PathGate>>>> = OnceLock::new();
    GATES.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Best-effort, syscall-isolated gate key for `path`.
///
/// Why (issue #3659 review findings #2 and #3):
///  - **#2 (blocking syscall off the async thread):** a bare
///    `std::fs::canonicalize` call is a `realpath` syscall that can block
///    indefinitely on a TCC-denied / blocked volume — the exact #718 Part-3
///    worker-starvation class this crate already fixed once for the
///    warm-boot scan itself. Isolating it in `spawn_blocking` keeps a hung
///    syscall confined to a blocking-pool thread, never an async worker.
///  - **#3 (TOCTOU on the file's own existence):** canonicalizing the FULL
///    path (including the redb file itself) is time-dependent — a racer
///    keying BEFORE the file exists falls back to the literal path, while a
///    racer keying AFTER the file exists resolves the real canonical path.
///    On macOS `/var` is itself a symlink to `/private/var` (see
///    `allowlist::mod` for the identical caveat), so those two keys can
///    differ even though they name the same file — silently defeating
///    serialization for exactly the racer pair this module exists to guard
///    (an eager warm-boot restore vs. a create/relocate handler for a
///    not-yet-existing corpus file). The PARENT directory, by contrast, is
///    always provisioned up front (`persistence_loader`/`corpus_swap` only
///    ever resolve a path under an already-created data/colocated directory)
///    and does not change identity as a side effect of the file being
///    created inside it, so canonicalizing the parent and rejoining the file
///    name gives every racer the same key regardless of which one wins.
///    Falling back to the literal full path only when the PARENT itself
///    cannot be resolved (the narrower, genuine first-boot-before-any-
///    directory-exists edge case) keeps behavior sane even then.
///
/// What: canonicalizes `path.parent()` inside `spawn_blocking` and rejoins
/// `path.file_name()`; falls back to the literal `path` when there is no
/// parent/file-name component, or when the parent cannot be canonicalized
/// (e.g. it does not exist yet), or if the blocking task itself cannot be
/// joined.
/// Test: `tests::gate_key_is_stable_across_file_creation`.
async fn gate_key(path: &Path) -> PathBuf {
    let owned = path.to_path_buf();
    let for_task = owned.clone();
    tokio::task::spawn_blocking(move || canonicalize_gate_key(&for_task))
        .await
        .unwrap_or(owned)
}

fn canonicalize_gate_key(path: &Path) -> PathBuf {
    let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    match std::fs::canonicalize(parent) {
        Ok(canonical_parent) => canonical_parent.join(file_name),
        Err(_) => path.to_path_buf(),
    }
}

/// Fetch (creating if absent) the gate guarding `path`.
async fn gate_for(path: &Path) -> Arc<PathGate> {
    let key = gate_key(path).await;
    let mut gates = open_gates().lock().unwrap_or_else(|e| e.into_inner());
    gates
        .entry(key)
        .or_insert_with(|| Arc::new(PathGate::new()))
        .clone()
}

/// Read the per-path corpus-open timeout from `TRUSTY_CORPUS_OPEN_TIMEOUT_SECS`
/// (default 30s).
///
/// Why: mirrors `warm_boot::warmboot_index_timeout`'s env-var-overridable
/// pattern, but is defined locally rather than imported from
/// `service::warm_boot` — this module is the low-level corpus-open primitive
/// underneath BOTH `persistence_loader` (service layer) and
/// `service::reindex::corpus_swap`, and `core` must not take a dependency on
/// `service`. Bounds how long any single caller (whether it is the one
/// actually performing the blocking I/O, or a later caller queued behind it)
/// will wait before giving up and marking the path wedged — see
/// `open_serialized`.
/// What: parses the env var as a positive integer number of seconds;
/// missing, unset, zero, or unparsable falls back to 30s.
/// Test: `tests::corpus_open_timeout_reads_env`,
/// `tests::corpus_open_timeout_defaults_without_env`.
fn corpus_open_timeout() -> Duration {
    let secs = std::env::var("TRUSTY_CORPUS_OPEN_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(30);
    Duration::from_secs(secs)
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
/// wrapper adds the per-path gate, a bounded wait, and the panic→`Err`
/// boundary without changing what gets opened.
///
/// Safety note on `AssertUnwindSafe`: `open_fn` is required to be `FnOnce`
/// with no shared/aliased mutable state — it is expected to be a plain
/// constructor call (e.g. `move || CorpusStore::open(&path)`) that owns
/// everything it touches and produces a fresh, fully-owned `T` or `Err`. Do
/// NOT pass a closure here that mutates external state through a shared
/// reference (`&mut` behind an `Arc`/`RwLock` captured by the closure) —
/// `catch_unwind` only guarantees memory safety across the unwind, not
/// logical consistency of state a panicking closure left half-mutated.
///
/// Test: `tests::concurrent_opens_of_same_path_serialize_without_panicking`,
/// `tests::panicking_open_becomes_typed_err_not_a_panic`,
/// `tests::different_paths_do_not_serialize`,
/// `tests::wedged_opener_times_out_and_blocks_future_callers_fast`.
pub(crate) async fn open_serialized<F, T>(path: &Path, open_fn: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let gate = gate_for(path).await;
    let path_display = path.display().to_string();

    if gate.wedged.load(Ordering::Acquire) {
        return Err(anyhow!(
            "corpus opener previously wedged for {path_display} (issue #3659) — refusing \
             to race a possibly still-running blocking open; restart the daemon once the \
             underlying volume/I-O issue is resolved"
        ));
    }

    let timeout_dur = corpus_open_timeout();
    let gate_for_task = Arc::clone(&gate);
    let path_for_task = path_display.clone();

    // Issue #3659 review finding #1: run the gate-acquire + actual blocking
    // open as an INDEPENDENT task so it is not cancelled if this caller gives
    // up waiting. `spawn_blocking` work can never be aborted, so if we
    // instead wrapped "gate.lock().await + spawn_blocking(...).await" inline
    // in one `tokio::time::timeout`, this caller's own timeout would drop the
    // future holding the mutex guard — releasing the gate while the
    // abandoned blocking-pool thread might still be mid-write on `path`, so
    // a brand-new caller could start a SECOND concurrent open. Spawning the
    // attempt keeps the mutex held for as long as it genuinely runs (however
    // long that is); `tokio::time::timeout` below only bounds how long THIS
    // caller waits for it.
    let handle = tokio::spawn(async move {
        let _permit = gate_for_task.mutex.lock().await;
        run_open(open_fn, path_for_task).await
    });

    match tokio::time::timeout(timeout_dur, handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => {
            tracing::error!(
                path = %path_display,
                "corpus open supervising task panicked ({join_err}) — treating as an open \
                 failure (issue #3659)"
            );
            Err(anyhow!(
                "redb corpus open supervising task panicked for {path_display}: {join_err}"
            ))
        }
        Err(_elapsed) => {
            // The spawned attempt is NOT aborted — it keeps holding the
            // per-path mutex for as long as it actually runs (possibly
            // forever, e.g. a TCC-denied volume, issue #718). Mark the path
            // wedged now so every later caller fails fast instead of queuing
            // behind a possibly-forever-running opener.
            gate.wedged.store(true, Ordering::Release);
            tracing::error!(
                path = %path_display,
                timeout = ?timeout_dur,
                "corpus open wedged past the timeout — a blocking redb open never returned \
                 (likely a TCC-denied/blocked volume, issue #718); this path is now \
                 permanently refused for the rest of this process's lifetime (issue #3659) \
                 — restart the daemon once the underlying volume issue is resolved"
            );
            Err(anyhow!(
                "corpus open timed out after {timeout_dur:?} for {path_display} — opener \
                 wedged (issue #3659); restart the daemon to retry"
            ))
        }
    }
}

/// Run the actual blocking `open_fn` on the blocking pool, converting a
/// panic into a typed `Err` (issue #3659).
async fn run_open<F, T>(open_fn: F, path_display: String) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
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
                "corpus open blocking task could not be joined ({join_err}) — treating as \
                 an open failure (issue #3659)"
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

    /// Why (issue #3659 review finding #3): the gate key must be stable
    /// across the file's OWN creation, not just at a single instant. A naive
    /// full-path canonicalize gives a not-yet-existing-file racer a different
    /// key than a racer that arrives after the file exists (on macOS `/var`
    /// is itself a symlink to `/private/var` — see `allowlist::mod` — so this
    /// is a real, not merely theoretical, divergence), silently defeating
    /// serialization for exactly the racer pair this module exists to guard.
    /// What: computes the gate key before the file exists, creates the file,
    /// then computes the gate key again; asserts they are identical.
    /// Test: this IS the test.
    #[tokio::test]
    async fn gate_key_is_stable_across_file_creation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("shared.redb");

        let key_before = gate_key(&path).await;
        std::fs::File::create(&path).unwrap();
        let key_after = gate_key(&path).await;

        assert_eq!(
            key_before, key_after,
            "the gate key must be stable across the file's own creation — two callers \
             separated by a real file-creation event must still land on the SAME gate"
        );
    }

    /// Why (issue #3659 review finding #1): a wedged opener (one whose
    /// blocking redb call never returns — the exact #718 TCC-denied-volume
    /// class) must not hang its caller forever, and once wedged, later
    /// callers for the SAME path must fail fast rather than queue behind a
    /// possibly-forever-running attempt.
    /// What: sets a short timeout, then calls `open_serialized` with a
    /// closure that blocks (via a real `std::thread::sleep`, simulating a
    /// frozen syscall) far longer than the timeout; asserts the caller is
    /// released at the timeout boundary, not after the full sleep. A second,
    /// fresh call for the same path must then fail near-instantly.
    /// Test: this IS the test.
    #[tokio::test]
    #[serial_test::serial]
    async fn wedged_opener_times_out_and_blocks_future_callers_fast() {
        unsafe { std::env::set_var("TRUSTY_CORPUS_OPEN_TIMEOUT_SECS", "1") };
        let dir = tempdir().unwrap();
        let path = dir.path().join("wedge.redb");

        let start = std::time::Instant::now();
        let result: Result<()> = open_serialized(&path, || -> Result<()> {
            std::thread::sleep(std::time::Duration::from_secs(3));
            Ok(())
        })
        .await;
        assert!(
            result.is_err(),
            "a wedged opener must time out with Err, not hang forever"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "the caller must be released at the ~1s timeout boundary, not wait out the \
             full 3s blocking sleep: elapsed {:?}",
            start.elapsed()
        );

        let start2 = std::time::Instant::now();
        let result2: Result<()> = open_serialized(&path, || -> Result<()> { Ok(()) }).await;
        assert!(
            result2.is_err(),
            "a path marked wedged must keep refusing new openers"
        );
        assert!(
            start2.elapsed() < std::time::Duration::from_millis(500),
            "a wedged path must fail new callers FAST, not make them wait out another \
             full timeout: elapsed {:?}",
            start2.elapsed()
        );

        unsafe { std::env::remove_var("TRUSTY_CORPUS_OPEN_TIMEOUT_SECS") };
    }

    /// Why: pins the env-var parsing contract for `corpus_open_timeout`
    /// (mirroring `warm_boot::warmboot_index_timeout_parses_env_var`).
    /// What: sets the env var to a valid positive integer; asserts it is
    /// honored.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn corpus_open_timeout_reads_env() {
        unsafe { std::env::set_var("TRUSTY_CORPUS_OPEN_TIMEOUT_SECS", "7") };
        assert_eq!(corpus_open_timeout(), Duration::from_secs(7));
        unsafe { std::env::remove_var("TRUSTY_CORPUS_OPEN_TIMEOUT_SECS") };
    }

    /// Why: an unset/invalid/zero env var must fall back to the 30s default
    /// rather than panicking or producing a zero-duration timeout.
    /// What: removes the env var and asserts the default; sets it to `"0"`
    /// and asserts the default still applies.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn corpus_open_timeout_defaults_without_env() {
        unsafe { std::env::remove_var("TRUSTY_CORPUS_OPEN_TIMEOUT_SECS") };
        assert_eq!(corpus_open_timeout(), Duration::from_secs(30));
        unsafe { std::env::set_var("TRUSTY_CORPUS_OPEN_TIMEOUT_SECS", "0") };
        assert_eq!(corpus_open_timeout(), Duration::from_secs(30));
        unsafe { std::env::remove_var("TRUSTY_CORPUS_OPEN_TIMEOUT_SECS") };
    }
}
