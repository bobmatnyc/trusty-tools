//! #5552 — `ProcessTracker::load` must not read an undeterminable probe as
//! "tracker file absent".
//!
//! Why: every `ProcessTracker` entry point is a read-modify-write around
//! `load()` / `save()`, and `save()` atomically renames over `processes.json`.
//! While `load` coerced a failed `try_exists` to `Ok(false)`, one transient
//! probe error rewrote the tracker from an empty map: every previously-tracked
//! live PID was forgotten, so `cleanup_stale` could never reap those children
//! and `shutdown_all` never signalled them. The governing rule is ADR-0045 —
//! distinguish absent from undeterminable before a destructive filesystem
//! operation.
//! What: builds a fixture in which the probe genuinely fails but the write
//! path still works, so the pre-fix code reaches `save()` and clobbers real
//! data. Every test therefore asserts the DIRECTION — the surviving state —
//! and not merely that an error came back.
//! Test: this file.
//!
//! The fixture: `state/processes.json` is a symlink into `state/locked/`, and
//! `locked` is stripped to mode 000. `stat` follows the symlink and is denied
//! (`EACCES`), while `rename` does not follow it — so a write to
//! `state/processes.json` still succeeds and replaces the symlink. That is the
//! same shape as the real trigger (`EIO` / `ETIMEDOUT` / `ESTALE` on a network
//! mount): the question could not be answered, but the tracker is still
//! writable. A vanished mount point is the opposite case — `ENOENT` is a
//! truthful "absent" and must stay benign; `probe_benign_absence_arms` pins it.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;
use trusty_agents::process_tracker::{ProcessStatus, ProcessTracker};

/// PIDs seeded into the tracker before the probe is broken. High enough to be
/// dead, so `cleanup_stale` has real work it would report on.
const SEEDED: [u32; 3] = [9_100_001, 9_100_002, 9_100_003];
const NEW_PID: u32 = 9_100_777;

/// Restore a directory's mode on drop, including while unwinding.
///
/// Why: the fixture strips a directory to mode 000. If an assertion panics
/// before it is restored, `tempfile` cannot delete the tree and the run leaks
/// an undeletable directory.
struct RestoreMode(PathBuf);

impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

/// A tracker whose probe fails while its directory stays writable.
///
/// Field order is drop order: `_restore` must run before `_tmp`, or `TempDir`
/// cannot delete a tree that still contains a mode-000 directory.
struct BrokenProbe {
    _restore: RestoreMode,
    _tmp: TempDir,
    tracker: Arc<ProcessTracker>,
    locked: PathBuf,
}

impl BrokenProbe {
    fn unlock(&self) {
        std::fs::set_permissions(&self.locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn relock(&self) {
        std::fs::set_permissions(&self.locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    }

    /// Lift the denial, read back what the tracker path actually resolves to
    /// now, and restore it.
    async fn surviving_pids(&self) -> Vec<u32> {
        self.unlock();
        let loaded = self.tracker.load().await;
        self.relock();
        let mut pids: Vec<u32> = loaded
            .expect("the tracker must be readable again once the denial is lifted")
            .keys()
            .copied()
            .collect();
        pids.sort_unstable();
        pids
    }

    /// The raw bytes the tracker path resolves to, read with the denial lifted.
    fn raw(&self) -> Option<Vec<u8>> {
        self.unlock();
        let bytes = std::fs::read(self.tracker.path()).ok();
        self.relock();
        bytes
    }
}

/// Seed a tracker with `SEEDED`, then make its probe undeterminable.
///
/// What: writes the real data to `state/locked/processes.json` through the
/// public API, points `state/processes.json` at it with a symlink, and strips
/// `locked` to mode 000. Verifies the denial actually took hold — root and
/// filesystems that ignore POSIX mode bits would otherwise let every test here
/// pass vacuously, which on a fail-open guard is worse than no test at all.
async fn broken_probe() -> BrokenProbe {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().join("state");
    let locked = state.join("locked");
    std::fs::create_dir_all(&locked).unwrap();

    let seeder = ProcessTracker::new(&locked);
    for (i, pid) in SEEDED.iter().enumerate() {
        seeder
            .register(*pid, "engineer", &format!("task-{i}"))
            .await
            .unwrap();
    }

    std::os::unix::fs::symlink(locked.join("processes.json"), state.join("processes.json"))
        .unwrap();

    let tracker = Arc::new(ProcessTracker::new(&state));

    // Baseline: the tracker resolves through the symlink and sees all of them.
    let seen = tracker.load().await.expect("readable before the lock");
    assert_eq!(
        seen.len(),
        SEEDED.len(),
        "fixture is wrong — the tracker must see the seeded PIDs before the denial"
    );

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = RestoreMode(locked.clone());

    assert_probe_is_denied(tracker.path());
    assert!(
        std::fs::metadata(&state).is_ok(),
        "fixture is wrong — the tracker's own directory must stay writable, \
         otherwise the pre-fix save() would fail for the wrong reason"
    );

    BrokenProbe {
        _restore: restore,
        _tmp: tmp,
        tracker,
        locked,
    }
}

fn assert_probe_is_denied(path: &Path) {
    match std::fs::metadata(path) {
        Ok(_) => panic!(
            "cannot exercise #5552: stat of {} still succeeds through a mode-000 parent. \
             Run this suite as a non-root user on a filesystem that honours POSIX \
             permission bits.",
            path.display()
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected the locked parent to deny the probe, got {e}"
        ),
    }
}

/// Why (#5552): `try_exists(..).unwrap_or(false)` reported "cannot determine"
/// as "not there", and `load` returned `Ok(HashMap::new())`.
/// What: probes a path the process is forbidden to stat, and asserts the error
/// reaches the caller instead of an empty map.
/// Test: this test.
#[tokio::test]
async fn load_propagates_an_undeterminable_probe() {
    let fx = broken_probe().await;

    let err = fx
        .tracker
        .load()
        .await
        .expect_err("a tracker whose existence cannot be determined must not read as empty");

    let root = err.root_cause().to_string();
    assert!(
        root.contains("denied"),
        "the denial must survive into the error chain, got: {err:#}"
    );
}

/// Why (#5552): this is the whole defect. `register` did
/// `self.load().await.unwrap_or_default()`, so a failed probe produced an empty
/// map, the new PID went into it, and `save()` renamed that one-entry map over
/// `processes.json`. Every previously-tracked live PID was gone — unreapable by
/// `cleanup_stale`, unkillable by `shutdown_all`.
/// What: registers a new PID while the probe is denied, then lifts the denial
/// and asserts the tracker still resolves to all three seeded PIDs. Asserting
/// only that `register` returned `Err` would not prove the file was left
/// intact, which is the invariant that matters.
/// Test: this test.
#[tokio::test]
async fn register_leaves_previously_tracked_pids_intact_when_the_probe_fails() {
    let fx = broken_probe().await;

    let err = fx
        .tracker
        .register(NEW_PID, "engineer", "task-new")
        .await
        .expect_err("register must not build a tracker state from a load it could not perform");
    assert!(
        format!("{err:#}").contains(&NEW_PID.to_string()),
        "the error should name the pid that could not be registered, got: {err:#}"
    );

    let surviving = fx.surviving_pids().await;
    assert_eq!(
        surviving,
        SEEDED.to_vec(),
        "processes.json was rewritten from a failed load — every tracked PID is now \
         invisible to cleanup_stale and shutdown_all"
    );
    assert!(
        !surviving.contains(&NEW_PID),
        "the registration must not have landed, since it could only land by clobbering"
    );
}

/// Why (#5552): concurrent registration is the realistic interleaving — several
/// sub-agents spawn at once, each doing its own read-modify-write over the same
/// file. A transient probe failure hitting that window used to have every one
/// of them save its own one-entry map.
/// What: fires eight concurrent `register` calls against the denied probe and
/// asserts all eight fail and the tracker is byte-identical afterwards.
/// Test: this test.
#[tokio::test]
async fn concurrent_registers_all_fail_without_clobbering_the_tracker() {
    let fx = broken_probe().await;
    let before = fx.raw();

    let mut tasks = Vec::new();
    for i in 0..8u32 {
        let tracker = Arc::clone(&fx.tracker);
        tasks.push(tokio::spawn(async move {
            tracker
                .register(NEW_PID + i, "engineer", "concurrent")
                .await
                .is_err()
        }));
    }
    for (i, t) in tasks.into_iter().enumerate() {
        assert!(
            t.await.unwrap(),
            "concurrent register #{i} succeeded off a failed load"
        );
    }

    assert_eq!(
        before,
        fx.raw(),
        "the tracker file changed even though no registration could read it first"
    );
    assert_eq!(fx.surviving_pids().await, SEEDED.to_vec());
}

/// Why (#5552): `cleanup_stale` returned `Ok(0)` off an empty map, and
/// `runtime/startup.rs` treats `Ok(0)` as a clean reap and logs nothing. "I
/// cannot tell what is tracked" was indistinguishable from "nothing needed
/// reaping" — the three seeded PIDs here are all dead and would otherwise have
/// been reaped.
/// What: asserts the denial propagates instead of surfacing as a zero count.
/// Test: this test.
#[tokio::test]
async fn cleanup_stale_propagates_rather_than_reporting_zero_reaped() {
    let fx = broken_probe().await;

    let err = fx
        .tracker
        .cleanup_stale()
        .await
        .expect_err("an unreadable tracker must not report a clean zero-reap run");
    assert!(
        format!("{err:#}").contains("reaping stale pids"),
        "the error should name the reap it could not perform, got: {err:#}"
    );

    assert_eq!(
        fx.surviving_pids().await,
        SEEDED.to_vec(),
        "cleanup_stale must leave the entries it never read"
    );
}

/// Why (#5552): `shutdown_all` off an empty map signals nobody, marks nothing,
/// saves the empty map, and returns `Ok(())`. The caller exits believing every
/// child was terminated; the children keep running.
/// What: asserts the denial propagates and the tracker still names the PIDs
/// that were never signalled.
/// Test: this test.
#[tokio::test]
async fn shutdown_all_propagates_rather_than_signalling_nobody() {
    let fx = broken_probe().await;

    let err = fx
        .tracker
        .shutdown_all()
        .await
        .expect_err("shutdown must not report success over children it never enumerated");
    assert!(
        format!("{err:#}").contains("shutting down pids"),
        "the error should name the shutdown it could not perform, got: {err:#}"
    );

    assert_eq!(
        fx.surviving_pids().await,
        SEEDED.to_vec(),
        "shutdown_all must not overwrite the tracker with the empty map it signalled"
    );
}

/// Why (#5552): `mark_completed` off an empty map finds no entry, so it is a
/// no-op and returns `Ok(())` — but only because it could not see the entry it
/// was asked to update. Had the PID been inserted first, the save would have
/// dropped the other two.
/// What: asserts the denial propagates and every seeded entry keeps its
/// `Running` status.
/// Test: this test.
#[tokio::test]
async fn mark_completed_propagates_and_leaves_the_tracker_intact() {
    let fx = broken_probe().await;

    let err = fx
        .tracker
        .mark_completed(SEEDED[0])
        .await
        .expect_err("mark_completed must not silently no-op on a tracker it could not read");
    assert!(
        format!("{err:#}").contains(&SEEDED[0].to_string()),
        "the error should name the pid it could not mark, got: {err:#}"
    );

    fx.unlock();
    let loaded = fx.tracker.load().await;
    fx.relock();
    let entries = loaded.unwrap();
    assert_eq!(entries.len(), SEEDED.len());
    for pid in SEEDED {
        assert_eq!(
            entries.get(&pid).unwrap().status,
            ProcessStatus::Running,
            "pid {pid} must be untouched"
        );
    }
}

/// Why (#5552): the fix must not turn a truthful "absent" into an error. A
/// first run has no tracker file; an unplugged external volume gives `ENOENT`
/// on the whole path, not a probe failure; a dangling symlink is also plain
/// absence. All three must keep returning an empty map — that is the
/// legitimate case the `unwrap_or(false)` was reaching for.
/// What: exercises the three `ENOENT` shapes and asserts each yields an empty
/// map, and that a normal register/reload round-trip still works on top of one.
/// Test: this test.
#[tokio::test]
async fn probe_benign_absence_arms() {
    let tmp = TempDir::new().unwrap();

    // 1. Directory exists, file does not — the first-run case.
    let fresh = ProcessTracker::new(tmp.path());
    assert!(fresh.load().await.unwrap().is_empty());

    // 2. No parent directory at all — the vanished mount point.
    let gone = ProcessTracker::new(&tmp.path().join("no").join("such").join("mount"));
    assert!(
        gone.load().await.unwrap().is_empty(),
        "a missing parent is ENOENT, a truthful absence, and must stay benign"
    );

    // 3. Dangling symlink — stat resolves to a missing target, still ENOENT.
    let dangling = tmp.path().join("dangling");
    std::fs::create_dir(&dangling).unwrap();
    std::os::unix::fs::symlink(
        dangling.join("nowhere.json"),
        dangling.join("processes.json"),
    )
    .unwrap();
    assert!(
        ProcessTracker::new(&dangling)
            .load()
            .await
            .unwrap()
            .is_empty(),
        "a broken symlink is absence, not a probe failure"
    );

    // And the ordinary path still round-trips.
    fresh.register(4242, "qa-agent", "t").await.unwrap();
    assert!(fresh.load().await.unwrap().contains_key(&4242));
}
