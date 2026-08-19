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
use crate::store::dedup_open::{DedupNeed, open_for};
use std::time::{Duration, Instant};

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
        store.claim_blocking("o", "r", 1, "sha").unwrap(),
        ClaimOutcome::Claimed
    );
}

#[test]
fn first_claim_succeeds() {
    let (store, _d) = temp_store();
    let outcome = store
        .claim_blocking("acme", "backend", 42, "sha-abc")
        .unwrap();
    assert_eq!(outcome, ClaimOutcome::Claimed);
}

/// REGRESSION (#5126): a fresh in-progress claim must not report as a
/// completed review.
///
/// Why: `Skipped` is what the runner turns into `Verdict::Approve`. When a
/// held (or stranded) in-progress claim also returned `Skipped`, a review that
/// never ran reported approval — for the whole `DEDUP_STALE_SECS` window after
/// a holder crashed between `claim` and `complete`.
/// What: claims a SHA, then re-claims it while the first claim is still
/// in-progress and not stale.
/// Test: this test. Fails pre-fix at the `assert_ne!`, which observes the
/// `Skipped` the old two-way outcome produced.
#[test]
fn fresh_in_progress_claim_is_not_a_completed_review() {
    let (store, _d) = temp_store();
    assert_eq!(
        store
            .claim_blocking("acme", "backend", 42, "sha-abc")
            .unwrap(),
        ClaimOutcome::Claimed
    );

    let second = store
        .claim_blocking("acme", "backend", 42, "sha-abc")
        .unwrap();

    assert_ne!(
        second,
        ClaimOutcome::Skipped,
        "a held in-progress claim means nothing was reviewed — reporting it as a \
         completed review is what made the runner approve an unrun review (#5126)"
    );
    assert_eq!(
        second,
        ClaimOutcome::InProgressElsewhere,
        "the second caller must be told another holder owns the slot"
    );
}

#[test]
fn claim_then_skip_after_complete() {
    let (store, _d) = temp_store();
    assert_eq!(
        store
            .claim_blocking("acme", "backend", 42, "sha-abc")
            .unwrap(),
        ClaimOutcome::Claimed
    );
    store
        .complete_blocking("acme", "backend", 42, "sha-abc")
        .unwrap();
    // After completion, re-claiming the same SHA must be skipped.
    assert_eq!(
        store
            .claim_blocking("acme", "backend", 42, "sha-abc")
            .unwrap(),
        ClaimOutcome::Skipped
    );
}

#[test]
fn claim_allows_after_release() {
    let (store, _d) = temp_store();
    assert_eq!(
        store
            .claim_blocking("acme", "backend", 42, "sha-abc")
            .unwrap(),
        ClaimOutcome::Claimed
    );
    // Release (e.g. review aborted) → the SHA can be claimed again.
    store
        .release_blocking("acme", "backend", 42, "sha-abc")
        .unwrap();
    assert_eq!(
        store
            .claim_blocking("acme", "backend", 42, "sha-abc")
            .unwrap(),
        ClaimOutcome::Claimed
    );
}

#[test]
fn different_sha_not_skipped() {
    let (store, _d) = temp_store();
    store
        .claim_blocking("acme", "backend", 42, "sha-abc")
        .unwrap();
    store
        .complete_blocking("acme", "backend", 42, "sha-abc")
        .unwrap();
    // A new head SHA on the same PR is a fresh review.
    assert_eq!(
        store
            .claim_blocking("acme", "backend", 42, "sha-def")
            .unwrap(),
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
        store
            .claim_blocking("acme", "backend", 42, "sha-stale")
            .unwrap(),
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
        a.claim_blocking("acme", "backend", 42, "sha-abc").unwrap(),
        ClaimOutcome::Claimed
    );
    a.complete_blocking("acme", "backend", 42, "sha-abc")
        .unwrap();

    // The point of the store is shared state: b must see a's completed claim.
    assert_eq!(
        b.claim_blocking("acme", "backend", 42, "sha-abc").unwrap(),
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
        store.claim_blocking("acme", "backend", 1, "sha").unwrap(),
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
                store.claim_blocking("acme", "backend", 42, "sha-race")
            })
        })
        .collect();

    let mut claimed = 0usize;
    let mut blocked = 0usize;
    for h in handles {
        match h.join().expect("thread must not panic") {
            Ok(ClaimOutcome::Claimed) => claimed += 1,
            // #5126: the losers see the winner's in-progress claim, which is
            // not a completed review.
            Ok(ClaimOutcome::InProgressElsewhere) => blocked += 1,
            Ok(other) => panic!("no thread may see a completed review here: {other:?}"),
            Err(e) => panic!("concurrent claim must not error under contention: {e}"),
        }
    }
    assert_eq!(claimed, 1, "exactly one caller may own the claim");
    assert_eq!(blocked, THREADS - 1, "every other caller must be blocked");
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
        .claim_blocking("acme", "backend", 42, "sha-x")
        .expect_err("a permanently-held lock must surface as an error, never as silent no-dedup");
    assert!(
        matches!(err, DedupError::Contended { .. }),
        "expected DedupError::Contended, got {err:?}"
    );
    drop(holder);

    // Once the holder releases, the very same store works again.
    assert_eq!(
        store
            .claim_blocking("acme", "backend", 42, "sha-x")
            .unwrap(),
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
        .claim_blocking("acme", "backend", 42, "sha-xproc")
        .expect("claim must wait out the other process, not fail");
    assert_eq!(outcome, ClaimOutcome::Claimed);

    let status = child.wait().expect("child wait");
    assert!(status.success(), "lock-holder child failed: {status}");
}

// ─── #5064 review round 2 ────────────────────────────────────────────────────

/// Why: the #702 rename-aside recovery used to run once per process, at
/// startup, under redb's own exclusive lock. Making the open per-operation lets
/// several processes recover at once, and an unguarded rename is data loss: a
/// recoverer that read the old unreadable file renames away the *fresh*
/// database a sibling has already committed claims into. The guard is a sidecar
/// lock plus a re-check, so at most one process rebuilds and a loser adopts the
/// winner's database instead of renaming it.
///
/// What: a winner takes the recovery-lock sidecar, and while holding it clears
/// the unreadable file, rebuilds, and commits a claim. A second recoverer starts
/// from the unreadable file and must (a) wait for the sidecar rather than
/// rebuild immediately, and (b) come back to the winner's database with the
/// claim intact.
///
/// The loss ordering itself — a recoverer renaming away a database committed
/// during its own microsecond-wide window — is not reachable deterministically
/// without a test hook inside the production open path, so this asserts the
/// guard that closes it: the wait. An unguarded recovery ignores the sidecar
/// and returns immediately, which is what makes this red.
/// Test: this test.
#[test]
fn concurrent_recovery_waits_and_adopts_the_winners_database() {
    use std::io::Write;
    use std::sync::mpsc;

    /// How long the winner holds the recovery lock.
    const HOLD: Duration = Duration::from_millis(250);
    /// Slack below `HOLD` that still proves the recoverer waited.
    const MIN_WAIT: Duration = Duration::from_millis(120);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");
    let lock_path =
        crate::store::dedup_open::sibling(&path, crate::store::dedup_open::RECOVERY_LOCK_SUFFIX);

    // Unreadable file: any opener enters the recovery path.
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&[0xABu8; 4096]))
        .unwrap();

    let (holding_tx, holding_rx) = mpsc::channel();
    let (winner_path, winner_lock) = (path.clone(), lock_path.clone());
    let winner = std::thread::spawn(move || {
        let lock = std::fs::File::create(&winner_lock).expect("create recovery lock");
        lock.lock().expect("take recovery lock");
        holding_tx.send(()).expect("signal");
        // Hold first, so the other recoverer observes the *unreadable* file and
        // genuinely enters the recovery path, then rebuild under the lock.
        std::thread::sleep(HOLD);
        std::fs::remove_file(&winner_path).expect("clear the unreadable file");
        {
            let store = DedupStore::open(&winner_path).expect("winner rebuilds");
            store
                .claim_blocking("acme", "backend", 42, "sha-won")
                .expect("winner claims");
            store
                .complete_blocking("acme", "backend", 42, "sha-won")
                .expect("winner completes");
        }
        drop(lock);
    });

    holding_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("winner took the recovery lock");

    let started = Instant::now();
    let store =
        DedupStore::open(&path).expect("the losing recoverer must still get a usable store");
    let waited = started.elapsed();
    winner.join().unwrap();

    assert!(
        waited >= MIN_WAIT,
        "recovery is not serialised — the second recoverer rebuilt immediately \
         instead of waiting for the recovery lock (waited {waited:?})"
    );
    assert_eq!(
        store
            .claim_blocking("acme", "backend", 42, "sha-won")
            .unwrap(),
        ClaimOutcome::Skipped,
        "the winner's completed claim must survive — a loser must adopt the \
         rebuilt database, never rename it away"
    );
}

/// Why: `claim_blocking` sleeps for up to two seconds on the file lock, and the
/// webhook review runs inside a `tokio::spawn`ed task. Blocking a runtime
/// worker that long stalls every other task on it.
/// What: on a single-worker runtime, holds the lock from a plain thread —
/// synchronising on a channel so the hold is established before the claim
/// starts — and runs the async `claim` concurrently with a short
/// `tokio::time::sleep`. The timer must fire while the claim is still waiting,
/// which it can only do if the wait is not occupying the only worker.
/// Test: this test. Fails if `claim` runs its blocking body on the worker: the
/// timer cannot be polled until the claim returns.
#[tokio::test(flavor = "current_thread")]
async fn async_claim_runs_off_the_runtime_worker() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");
    let store = Arc::new(DedupStore::open(&path).expect("open"));

    let hold_path = path.clone();
    let (holding_tx, holding_rx) = std::sync::mpsc::channel();
    let (released_tx, released_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let db = Database::create(&hold_path).expect("holder takes the lock");
        holding_tx.send(()).expect("signal");
        std::thread::sleep(Duration::from_millis(300));
        drop(db);
        let _ = released_tx.send(());
    });

    // Do not start the timer or the claim until the holder actually owns the
    // lock — otherwise `claim` can win the race and return before the timer is
    // due, failing on correct code.
    holding_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("holder took the lock");

    let timer_fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&timer_fired);
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let outcome = store
        .claim("acme", "backend", 42, "sha-async")
        .await
        .expect("claim must wait out the holder");
    assert_eq!(outcome, ClaimOutcome::Claimed);
    assert!(
        timer_fired.load(std::sync::atomic::Ordering::SeqCst),
        "the runtime worker was blocked — a 50ms timer did not fire during a 300ms lock wait"
    );

    timer.await.unwrap();
    released_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("holder released");
    holder.join().unwrap();
}

/// Why: the async wrappers must not change the claim lifecycle, only where it
/// runs.
/// What: claim → complete → re-claim → release → re-claim through the async
/// surface, asserting the same outcomes the blocking tests assert.
/// Test: this test.
#[tokio::test]
async fn async_claim_complete_release_round_trip() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DedupStore::open(&dir.path().join("dedup.redb")).expect("open"));

    assert_eq!(
        store.claim("acme", "backend", 7, "sha-1").await.unwrap(),
        ClaimOutcome::Claimed
    );
    store.complete("acme", "backend", 7, "sha-1").await.unwrap();
    assert_eq!(
        store.claim("acme", "backend", 7, "sha-1").await.unwrap(),
        ClaimOutcome::Skipped
    );
    store.release("acme", "backend", 7, "sha-1").await.unwrap();
    assert_eq!(
        store.claim("acme", "backend", 7, "sha-1").await.unwrap(),
        ClaimOutcome::Claimed
    );
}

/// Why: `serve --stdio` and the embedded MCP `AppState` never post, so they
/// have no reason to create — or lock — the shared file.
/// What: `NotNeeded` returns `None` and leaves the filesystem untouched.
/// Test: this test.
#[test]
fn open_for_not_needed_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let dedup = open_for(dir.path(), DedupNeed::NotNeeded).expect("must not fail");
    assert!(dedup.is_none(), "a non-posting mode must hold no store");
    assert!(
        !dir.path().join("dedup.redb").exists(),
        "a non-posting mode must not create dedup.redb (#5064)"
    );
}

/// Why: the HTTP/webhook mode posts, so it must have the claim gate.
/// What: `Required` opens the store and creates the file.
/// Test: this test.
#[test]
fn open_for_required_opens() {
    let dir = tempfile::tempdir().unwrap();
    let dedup = open_for(dir.path(), DedupNeed::Required).expect("must open");
    assert!(dedup.is_some());
    assert!(dir.path().join("dedup.redb").exists());
}

/// Why: the fail-open shape #5064 exists to remove — an unopenable store
/// downgraded to `None`, leaving a posting-capable server with no idempotency
/// and a green health signal.
/// What: makes `dedup.redb` a directory so the open cannot succeed, then
/// asserts the error propagates instead of becoming `None`.
/// Test: this test.
#[test]
fn open_for_required_propagates_failure() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("dedup.redb")).unwrap();
    let err = open_for(dir.path(), DedupNeed::Required)
        .expect_err("an unopenable required store must be an error, not `None`");
    assert!(
        matches!(err, DedupError::Open(_)),
        "expected DedupError::Open, got {err:?}"
    );
}

/// Why: the ADR-0034 topology — a console-spawned webhook worker and the HTTP
/// daemon are both posting-capable and both point at one `--log-dir`.
/// What: two `Required` opens on the same log dir must both succeed.
/// Test: this test. Fails before the fix at the second open.
#[test]
fn two_posting_servers_share_a_log_dir() {
    let dir = tempfile::tempdir().unwrap();
    let first = open_for(dir.path(), DedupNeed::Required).expect("first server");
    let second = open_for(dir.path(), DedupNeed::Required)
        .expect("a second posting-capable server on the same --log-dir must open too (#5064)");
    assert!(first.is_some() && second.is_some());
}
