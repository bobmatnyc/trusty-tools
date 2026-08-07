//! Unit tests for the redb-backed dedup claim store.
//!
//! Why: split out of `dedup.rs` so the #5064 concurrency coverage can grow
//! without pushing the production file past the 500-SLOC cap.
//! What: the original claim/complete/release/stale lifecycle tests, plus the
//! #5064 locking contract — several handles on one `dedup.redb`, concurrent
//! threads, a real second process, and the loud-failure path when the lock
//! never frees.
//! Test: this file.

use super::*;

/// Env var naming a `dedup.redb` that a re-exec'd child process should hold
/// open. Set only by `cross_process_holder_serialises_rather_than_locking_out`.
const CHILD_HOLD_ENV: &str = "TRUSTY_REVIEW_TEST_HOLD_DEDUP";

fn temp_store() -> (DedupStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("dedup.redb");
    let store = DedupStore::open(&path).expect("open store");
    (store, dir)
}

#[test]
fn open_creates_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("dedup.redb");
    let _store = DedupStore::open(&path).expect("open");
    assert!(path.exists(), "redb file must be created");
}

/// Why: #702 graceful-handling — a `dedup.redb` redb 4.x cannot open (a
/// stale redb-2.x file, simulated with garbage bytes) must NOT crash the
/// daemon; it is moved aside and replaced with a fresh empty store so the
/// reviewer keeps working (at worst one duplicate review).
/// What: writes garbage to `dedup.redb`, opens via `DedupStore::open`,
/// asserts the open succeeds and the backup file exists.
/// Test: this test.
#[test]
fn incompatible_dedup_db_is_recreated() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&[0xABu8; 4096]))
        .unwrap();

    let store = DedupStore::open(&path).expect("incompatible dedup db must recover, not error");
    assert!(
        path.with_file_name("dedup.redb.v2-incompatible").exists(),
        "incompatible dedup file must be backed up"
    );
    // Fresh store: a claim against any SHA succeeds (no stale history).
    assert_eq!(
        store.claim("o", "r", 1, "sha").unwrap(),
        ClaimOutcome::Claimed
    );
}

#[test]
fn first_claim_succeeds() {
    let (store, _d) = temp_store();
    let outcome = store.claim("acme", "backend", 42, "sha-abc").unwrap();
    assert_eq!(outcome, ClaimOutcome::Claimed);
}

#[test]
fn concurrent_in_progress_skips() {
    // A second claim for the same SHA while the first is still in-progress
    // (and not stale) is skipped — another worker owns it.
    let (store, _d) = temp_store();
    assert_eq!(
        store.claim("acme", "backend", 42, "sha-abc").unwrap(),
        ClaimOutcome::Claimed
    );
    assert_eq!(
        store.claim("acme", "backend", 42, "sha-abc").unwrap(),
        ClaimOutcome::Skipped
    );
}

#[test]
fn claim_then_skip_after_complete() {
    let (store, _d) = temp_store();
    assert_eq!(
        store.claim("acme", "backend", 42, "sha-abc").unwrap(),
        ClaimOutcome::Claimed
    );
    store.complete("acme", "backend", 42, "sha-abc").unwrap();
    // After completion, re-claiming the same SHA must be skipped.
    assert_eq!(
        store.claim("acme", "backend", 42, "sha-abc").unwrap(),
        ClaimOutcome::Skipped
    );
}

#[test]
fn claim_allows_after_release() {
    let (store, _d) = temp_store();
    assert_eq!(
        store.claim("acme", "backend", 42, "sha-abc").unwrap(),
        ClaimOutcome::Claimed
    );
    // Release (e.g. review aborted) → the SHA can be claimed again.
    store.release("acme", "backend", 42, "sha-abc").unwrap();
    assert_eq!(
        store.claim("acme", "backend", 42, "sha-abc").unwrap(),
        ClaimOutcome::Claimed
    );
}

#[test]
fn different_sha_not_skipped() {
    let (store, _d) = temp_store();
    store.claim("acme", "backend", 42, "sha-abc").unwrap();
    store.complete("acme", "backend", 42, "sha-abc").unwrap();
    // A new head SHA on the same PR is a fresh review.
    assert_eq!(
        store.claim("acme", "backend", 42, "sha-def").unwrap(),
        ClaimOutcome::Claimed
    );
}

#[test]
fn stale_in_progress_is_reclaimable() {
    // Simulate a crashed worker by writing an in-progress claim with an old
    // timestamp directly, then verify a new claim reclaims it.  Writing it
    // through a *separate* redb handle is only possible because the store no
    // longer holds the lock between operations (#5064).
    let (store, dir) = temp_store();
    let path = dir.path().join("dedup.redb");
    let key = DedupStore::key("acme", "backend", 42, "sha-stale");
    let stale = ClaimRecord {
        state: ClaimState::InProgress,
        updated_at: now_secs().saturating_sub(DEDUP_STALE_SECS + 10),
    };
    let json = serde_json::to_string(&stale).unwrap();
    {
        let db = Database::create(&path).expect("second handle must be able to open the file");
        let write = db.begin_write().unwrap();
        {
            let mut t = write.open_table(CLAIMS).unwrap();
            t.insert(key.as_str(), json.as_str()).unwrap();
        }
        write.commit().unwrap();
    }

    assert_eq!(
        store.claim("acme", "backend", 42, "sha-stale").unwrap(),
        ClaimOutcome::Claimed,
        "a stale in-progress claim must be reclaimable"
    );
}

// ─── #5064: exclusive-flock collision ────────────────────────────────────────

/// Why: #5064 — `serve` (HTTP), `serve --stdio`, and an ADR-0034
/// console-spawned webhook worker all call `build_app_state` against the same
/// `--log-dir`. When the store held redb open for the process's lifetime the
/// second opener got `DatabaseAlreadyOpen`, which the caller downgraded to a
/// warning and continued with no dedup at all.
/// What: two `DedupStore` handles on one path must both open AND both see the
/// same durable state.
/// Test: this test. Fails before the fix at the second `open`.
#[test]
fn two_stores_on_one_path_both_work() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");

    let a = DedupStore::open(&path).expect("first store must open");
    let b = DedupStore::open(&path)
        .expect("a second holder of the same dedup.redb must not be locked out (#5064)");

    assert_eq!(
        a.claim("acme", "backend", 42, "sha-abc").unwrap(),
        ClaimOutcome::Claimed
    );
    a.complete("acme", "backend", 42, "sha-abc").unwrap();

    // The point of the store is shared state: b must see a's completed claim.
    assert_eq!(
        b.claim("acme", "backend", 42, "sha-abc").unwrap(),
        ClaimOutcome::Skipped,
        "the second holder must observe the first holder's completed claim"
    );
}

/// Why: a store that quietly stopped deduplicating would be indistinguishable
/// from a working one, which is exactly the fail-open shape #5064 is about.
/// What: after `open` returns, an unrelated redb handle must be able to take
/// the exclusive lock immediately — i.e. the store holds no lock at rest.
/// Test: this test. Fails before the fix.
#[test]
fn store_holds_no_lock_between_operations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");
    let store = DedupStore::open(&path).expect("open");

    let db = Database::create(&path)
        .expect("DedupStore must not hold redb's exclusive lock at rest (#5064)");
    drop(db);

    // And it still works afterwards.
    assert_eq!(
        store.claim("acme", "backend", 1, "sha").unwrap(),
        ClaimOutcome::Claimed
    );
}

/// Why: the dedup gate is what stops two workers posting the same review
/// comment. Under real contention exactly one caller must win — never zero
/// (silent degradation) and never two.
/// What: eight threads, each with its own `DedupStore` handle on one file,
/// released together by a barrier, all claim the same SHA.
/// Test: this test. Fails before the fix: seven of the eight `open` calls
/// return `DatabaseAlreadyOpen`.
#[test]
fn concurrent_threads_claim_exactly_once() {
    use std::sync::{Arc, Barrier};

    const THREADS: usize = 8;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");
    // Create the file up front so every thread races on `claim`, not on create.
    DedupStore::open(&path).expect("seed store");

    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || -> Result<ClaimOutcome, DedupError> {
                let store = DedupStore::open(&path)?;
                barrier.wait();
                store.claim("acme", "backend", 42, "sha-race")
            })
        })
        .collect();

    let mut claimed = 0usize;
    let mut skipped = 0usize;
    for h in handles {
        match h.join().expect("thread must not panic") {
            Ok(ClaimOutcome::Claimed) => claimed += 1,
            Ok(ClaimOutcome::Skipped) => skipped += 1,
            Err(e) => panic!("concurrent claim must not error under contention: {e}"),
        }
    }
    assert_eq!(claimed, 1, "exactly one caller may own the claim");
    assert_eq!(skipped, THREADS - 1, "every other caller must be skipped");
}

/// Why: contention is waited out, but not forever — and when the budget expires
/// the caller must be told. Returning `Ok` with dedup silently disabled is the
/// fail-open shape this issue exists to remove.
/// What: holds redb open for longer than `LOCK_WAIT_BUDGET`, then asserts
/// `claim` returns `DedupError::Contended` — an error, not a degraded success.
/// Test: this test.
#[test]
fn held_lock_that_never_releases_reports_contention() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");
    let store = DedupStore::open(&path).expect("open");

    let holder = Database::create(&path).expect("holder takes the exclusive lock");
    let err = store
        .claim("acme", "backend", 42, "sha-x")
        .expect_err("a permanently-held lock must surface as an error, never as silent no-dedup");
    assert!(
        matches!(err, DedupError::Contended { .. }),
        "expected DedupError::Contended, got {err:?}"
    );
    drop(holder);

    // Once the holder releases, the very same store works again.
    assert_eq!(
        store.claim("acme", "backend", 42, "sha-x").unwrap(),
        ClaimOutcome::Claimed
    );
}

/// Helper for `cross_process_holder_serialises_rather_than_locking_out`.
///
/// Why: redb's lock is a real advisory file lock, so the only faithful proof
/// that two *processes* no longer lock each other out is a second process.
/// What: a no-op unless `CHILD_HOLD_ENV` is set, in which case it opens the
/// named `dedup.redb`, writes a `.held` sentinel, holds the lock briefly, and
/// exits. Re-exec'd by the parent test via `current_exe()`.
/// Test: driven by `cross_process_holder_serialises_rather_than_locking_out`.
#[test]
fn dedup_lock_holder_child() {
    let Ok(path) = std::env::var(CHILD_HOLD_ENV) else {
        return;
    };
    let db = Database::create(&path).expect("child process must acquire the dedup lock");
    std::fs::write(format!("{path}.held"), b"1").expect("write sentinel");
    std::thread::sleep(Duration::from_millis(500));
    drop(db);
}

/// Why: #5064's failing topology is cross-process — an HTTP daemon, a
/// `serve --stdio` session, and a console-spawned webhook worker on one
/// `--log-dir`. A same-process test proves the lock semantics but not the
/// deployment shape.
/// What: re-execs this test binary as a child that holds `dedup.redb` for
/// 500 ms, then claims from the parent. The claim must wait the child out and
/// succeed, not fail and not silently skip dedup.
/// Test: this test. Fails before the fix — the parent store holds the lock for
/// its lifetime, so the child never acquires it and never writes its sentinel.
#[test]
fn cross_process_holder_serialises_rather_than_locking_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");
    let store = DedupStore::open(&path).expect("open");
    let sentinel = PathBuf::from(format!("{}.held", path.display()));

    let exe = std::env::current_exe().expect("current test binary");
    let mut child = std::process::Command::new(exe)
        .args([
            "--exact",
            "store::dedup::tests::dedup_lock_holder_child",
            "--nocapture",
        ])
        .env(CHILD_HOLD_ENV, &path)
        .spawn()
        .expect("spawn lock-holder child");

    // Wait for the child to actually hold the lock (bounded, so a failed child
    // fails this test rather than hanging it).
    let deadline = Instant::now() + Duration::from_secs(20);
    while !sentinel.exists() {
        assert!(
            Instant::now() < deadline,
            "child never acquired the dedup lock — the parent store is holding it (#5064)"
        );
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!("lock-holder child exited early with {status}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let outcome = store
        .claim("acme", "backend", 42, "sha-xproc")
        .expect("claim must wait out the other process, not fail");
    assert_eq!(outcome, ClaimOutcome::Claimed);

    let status = child.wait().expect("child wait");
    assert!(status.success(), "lock-holder child failed: {status}");
}
