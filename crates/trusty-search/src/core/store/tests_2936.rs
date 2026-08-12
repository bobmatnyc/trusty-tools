//! Orphaned-staging-file reaping and the abort-race guarantee (issue #2936).
//!
//! Why: two MEDIUM findings from the PR #2930 review. (a) A process SIGKILLed
//! between `save`'s write and its rename leaves a staging file behind, and since
//! #4395 made those names process-scoped no later process overwrites it — with
//! no `read_dir` anywhere in `core/store/`, nothing could ever reap it. (b) The
//! "an aborted flush never reaches its rename" guarantee was asserted only by
//! inspection; no test aborted a real in-progress `save()`.
//! What: the reaper's naming rules as a pure unit test, its filesystem behaviour
//! driven through the real `load_from`, and a real `AbortHandle` abort against a
//! real `save()` at a spread of delays.
//! Test: this module. Run with `cargo test -p trusty-search tests_2936`.

use std::sync::Arc;

use super::staging_reap::{is_reapable_staging, reap_orphan_staging_files};
use super::types::VectorStore;
use super::usearch_store::{staging_path, UsearchStore};

/// A pid far beyond every platform's `pid_max` (macOS 99999, Linux 4194304), so
/// `kill(pid, 0)` reliably answers ESRCH. Matches the constant
/// `service::daemon_tests::pid_alive_current_process_is_alive` already uses.
/// `u32::MAX` would be wrong here — it narrows to `-1`, which `kill` reads as
/// "every process the caller can signal" and never reports as dead.
const DEAD_PID: u32 = 2_000_000_000;

/// Populate a store with `n` deterministic unit-ish vectors of dimension 8.
async fn store_with(n: usize) -> UsearchStore {
    let store = UsearchStore::new(8).unwrap();
    for i in 0..n {
        let mut v = vec![0.0f32; 8];
        v[i % 8] = 1.0;
        v[(i + 3) % 8] = (i % 17) as f32 / 17.0;
        store.upsert(&format!("chunk-{i}"), v).await.unwrap();
    }
    store
}

/// #2936(a): every staging-name shape must be classified correctly.
///
/// Why: the reaper deletes files, so its predicate is the part that must not be
/// loose. The three shapes that matter are the live artifact itself (never
/// reapable), the pre-#4395 bare name (always reapable), and the pid-scoped name
/// (reapable only for a dead pid). A predicate that accepted the live name would
/// delete the snapshot; one that accepted a live pid would recreate the
/// cross-process corruption #4395 fixed.
/// What: exercises each shape directly against `is_reapable_staging`.
/// Test: this IS the test.
#[test]
fn staging_is_reapable_classifies_each_name_shape() {
    let live = "hnsw.usearch";

    assert!(
        !is_reapable_staging(live, live),
        "the live snapshot must never be reaped"
    );
    assert!(
        is_reapable_staging("hnsw.usearch.tmp", live),
        "the pre-#4395 bare staging name is always abandoned"
    );
    assert!(
        is_reapable_staging(&format!("hnsw.usearch.{DEAD_PID}.tmp"), live),
        "a staging file whose pid is dead must be reaped"
    );
    assert!(
        !is_reapable_staging(&format!("hnsw.usearch.{}.tmp", std::process::id()), live),
        "a staging file whose pid is ALIVE is a concurrent save — never reap it"
    );
    assert!(
        !is_reapable_staging("hnsw.usearch.notapid.tmp", live),
        "an unparseable pid must not be treated as dead"
    );
    assert!(
        !is_reapable_staging("unrelated.usearch.tmp", live),
        "a staging file for a different artifact must not match"
    );
    assert!(
        !is_reapable_staging("hnsw.keys.json", live),
        "the sidecar is a live artifact under its own name, not a staging file"
    );
}

/// #2936(a): `load_from` must reap dead-pid and pre-#4395 staging files for BOTH
/// the snapshot and its sidecar.
///
/// Why: this is the leak the issue reports. Against the pre-fix commit it fails
/// for the right reason — `core/store/` had no `read_dir` at all, so every
/// planted orphan is still present when the assertions run.
/// What: saves a real snapshot, plants four orphans beside it (dead-pid and bare
/// names for both artifacts), loads through the real `load_from`, and asserts
/// all four are gone and the snapshot still loads.
/// Test: this IS the test.
#[tokio::test]
async fn reap_removes_dead_pid_and_bare_staging_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let sidecar = path.with_extension("keys.json");

    store_with(4).await.save(&path).await.expect("save");

    let orphans = [
        dir.path().join(format!("hnsw.usearch.{DEAD_PID}.tmp")),
        dir.path().join(format!("hnsw.keys.json.{DEAD_PID}.tmp")),
        dir.path().join("hnsw.usearch.tmp"),
        dir.path().join("hnsw.keys.json.tmp"),
    ];
    for o in &orphans {
        std::fs::write(o, b"abandoned staging bytes").expect("plant orphan");
    }

    let loaded = UsearchStore::load_from(&path)
        .await
        .expect("load ok")
        .expect("load returned Some");
    assert_eq!(loaded.len().await.unwrap(), 4, "snapshot must still load");

    for o in &orphans {
        assert!(
            !o.exists(),
            "load_from must reap the abandoned staging file {} (issue #2936)",
            o.display()
        );
    }
    assert!(path.exists(), "the live snapshot must survive the reap");
    assert!(sidecar.exists(), "the live sidecar must survive the reap");
}

/// #2936(a): a staging file belonging to a LIVE process must be left alone.
///
/// Why: colocated snapshots live in the project root, outside every data
/// directory, so two daemons can legitimately be staging beside the same
/// snapshot at once. Deleting a live process's staging file mid-write is
/// precisely the cross-process corruption #4395 removed — the reaper must not
/// reintroduce it while cleaning up after dead ones.
/// What: plants a staging file carrying THIS process's pid alongside a dead-pid
/// one, then asserts the reaper takes only the dead one.
/// Test: this IS the test.
#[tokio::test]
async fn reap_leaves_live_pid_staging_and_the_live_snapshot_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    store_with(4).await.save(&path).await.expect("save");

    let live_staging = staging_path(&path, "usearch");
    std::fs::write(&live_staging, b"a concurrent save in progress").expect("plant live staging");
    let dead_staging = dir.path().join(format!("hnsw.usearch.{DEAD_PID}.tmp"));
    std::fs::write(&dead_staging, b"abandoned").expect("plant dead staging");

    let reaped = reap_orphan_staging_files(&path);

    assert_eq!(
        reaped, 1,
        "exactly the dead-pid staging file must be reaped"
    );
    assert!(
        live_staging.exists(),
        "a LIVE process's staging file must never be reaped — {} was deleted",
        live_staging.display()
    );
    assert!(!dead_staging.exists(), "the dead-pid file must be gone");
    assert!(path.exists(), "the live snapshot must survive");
}

/// #2936(b): aborting a real in-progress `save()` must never leave the on-disk
/// snapshot unloadable or at a count that was never committed.
///
/// Why: the "an aborted flush never reaches its rename" guarantee was asserted
/// only by inspection. `save` writes to a staging file and only then renames, so
/// an abort either lands before the rename (disk keeps the OLD snapshot) or
/// after it (disk holds the NEW one) — never a torn state in between. Sweeping
/// the abort delay is what makes this a race test rather than a single-timing
/// anecdote.
/// What: saves a baseline, upserts more, spawns `save()` on a task, aborts it
/// after a varying delay, and asserts `load_from` still yields a store at either
/// the baseline or the new count.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborted_save_leaves_a_loadable_store_at_the_old_or_new_count() {
    const BASELINE: usize = 600;
    const GROWN: usize = 900;

    for delay_us in [0u64, 50, 200, 500, 1_000, 3_000, 8_000] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.usearch");

        let store = Arc::new(store_with(BASELINE).await);
        store.save(&path).await.expect("baseline save");

        for i in BASELINE..GROWN {
            let mut v = vec![0.0f32; 8];
            v[i % 8] = 1.0;
            store.upsert(&format!("chunk-{i}"), v).await.unwrap();
        }

        let saver = store.clone();
        let save_path = path.clone();
        let task = tokio::spawn(async move { saver.save(&save_path).await });
        tokio::time::sleep(std::time::Duration::from_micros(delay_us)).await;
        task.abort();
        let _ = task.await;

        let loaded = UsearchStore::load_from(&path)
            .await
            .unwrap_or_else(|e| panic!("delay={delay_us}us: load errored: {e}"))
            .unwrap_or_else(|| {
                panic!("delay={delay_us}us: an aborted save destroyed the snapshot (#2936)")
            });
        let count = loaded.len().await.unwrap();
        assert!(
            count == BASELINE || count == GROWN,
            "delay={delay_us}us: aborted save left a torn snapshot at {count} vectors; \
             only {BASELINE} (rename never happened) or {GROWN} (rename completed) are \
             valid (issue #2936)"
        );
    }
}

/// #2936(b), the companion fact: a staging file this process abandoned is
/// consumed by its own next successful save.
///
/// Why: the reaper deliberately never touches a LIVE pid's staging file, which
/// raises the obvious question of what cleans up after an abort inside a
/// still-running daemon. The answer is the naming scheme itself — `staging_path`
/// is deterministic per process, so the next save writes through the same name
/// and renames it away. Stating it as a test keeps the reaper's live-pid
/// exemption from looking like an unhandled leak.
/// What: aborts a save, confirms a staging file survives, then runs a save to
/// completion and asserts nothing `.tmp` is left in the directory.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completed_save_consumes_this_processs_own_abandoned_staging_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    let store = Arc::new(store_with(600).await);
    store.save(&path).await.expect("baseline save");
    for i in 600..900 {
        let mut v = vec![0.0f32; 8];
        v[i % 8] = 1.0;
        store.upsert(&format!("chunk-{i}"), v).await.unwrap();
    }

    // Plant the abandoned staging file directly: an abort's exact timing is not
    // what this test is about, and the name is what the mechanism turns on.
    let staging = staging_path(&path, "usearch");
    std::fs::write(&staging, b"left over from an aborted save").expect("plant staging");
    assert!(staging.exists(), "precondition: a staging file is present");

    store.save(&path).await.expect("second save");

    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a completed save must leave no staging file behind; found {leftovers:?} \
         (issue #2936)"
    );
}
