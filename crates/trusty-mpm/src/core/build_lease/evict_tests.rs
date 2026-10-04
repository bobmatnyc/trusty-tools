//! Tests for `core::build_lease::evict` (#8451): a temp pool, a temp lease
//! store with real `flock`s, and a scripted volume reading. Every test runs
//! under the `build_slot_fds` key because several hold flocks or spawn
//! processes (#8736, see `slots::tests`).

use std::fs::{File, FileTimes};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, SystemTime};

use super::*;
use crate::core::build_lease::slots::HolderRecord;

/// A pool root, a lease store, and the temp dir that owns both.
struct Fixture {
    _tmp: tempfile::TempDir,
    pool: PathBuf,
    store: SlotDir,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pool = tmp.path().join("pool");
    std::fs::create_dir_all(&pool).expect("pool root");
    let store = SlotDir::at(tmp.path().join("store")).expect("lease store");
    Fixture {
        _tmp: tmp,
        pool,
        store,
    }
}

/// Make `<pool>/<repo>/slot-<index>/debug/artifact`, last used `age` ago.
fn make_slot(pool: &Path, repo: &str, index: u32, age: Duration) -> PathBuf {
    let slot = pool.join(repo).join(format!("slot-{index}"));
    let debug = slot.join("debug");
    std::fs::create_dir_all(&debug).expect("slot tree");
    std::fs::write(debug.join("artifact"), b"x").expect("artifact");
    let when = FileTimes::new().set_modified(SystemTime::now() - age);
    // Deepest first: writing a child moves its parent's mtime.
    for path in [debug.join("artifact"), debug.clone(), slot.clone()] {
        File::open(&path)
            .and_then(|f| f.set_times(when))
            .expect("set mtime");
    }
    slot
}

const HOUR: Duration = Duration::from_secs(3600);

/// A volume whose usage is `base` plus five points per slot left in the pool.
fn per_slot(base: f32) -> impl FnMut(&Path) -> Option<f32> {
    move |root: &Path| {
        let left = list_pool_slots(root).expect("list").len();
        Some(base + 5.0 * left as f32)
    }
}

fn swept(outcome: SweepOutcome) -> EvictReport {
    match outcome {
        SweepOutcome::Swept(report) => report,
        other => panic!("expected a sweep, got {other:?}"),
    }
}

#[test]
fn the_threshold_defaults_below_the_worktree_guard() {
    assert_eq!(effective_evict_pct(None, 90), DEFAULT_EVICT_PCT);
    assert_eq!(effective_evict_pct(Some(70), 90), 70);
}

#[test]
fn a_threshold_at_or_over_the_guard_is_held_below_it() {
    assert_eq!(effective_evict_pct(Some(95), 90), 89);
    assert_eq!(effective_evict_pct(Some(90), 90), 89);
    assert_eq!(effective_evict_pct(None, 80), 79);
    assert_eq!(
        effective_evict_pct(None, 1),
        1,
        "never 0: 0 would evict always"
    );
}

#[test]
fn an_out_of_range_threshold_uses_the_default() {
    assert_eq!(effective_evict_pct(Some(0), 90), DEFAULT_EVICT_PCT);
    assert_eq!(effective_evict_pct(Some(100), 90), DEFAULT_EVICT_PCT);
}

#[test]
#[serial_test::serial(build_slot_fds)]
fn slots_are_listed_oldest_first() {
    let f = fixture();
    let newest = make_slot(&f.pool, "o/r", 0, HOUR);
    let oldest = make_slot(&f.pool, "o2/r2", 3, 3 * HOUR);
    let middle = make_slot(&f.pool, "o/r", 1, 2 * HOUR);
    // Not slots: a staging tree, a non-numeric name, a file.
    std::fs::create_dir_all(f.pool.join("o/r/.slot-0.seeding.1.2")).expect("staging");
    std::fs::create_dir_all(f.pool.join("o/r/slot-x")).expect("odd name");
    std::fs::write(f.pool.join("o/r/slot-9"), b"").expect("file");
    let listed: Vec<(PathBuf, u32)> = list_pool_slots(&f.pool)
        .expect("list")
        .into_iter()
        .map(|s| (s.path, s.index))
        .collect();
    assert_eq!(listed, vec![(oldest, 3), (middle, 1), (newest, 0)]);
}

#[test]
#[serial_test::serial(build_slot_fds)]
fn a_missing_pool_is_empty() {
    let f = fixture();
    let missing = f.pool.join("absent");
    assert!(
        list_pool_slots(&missing)
            .expect("absent is empty")
            .is_empty()
    );
    let outcome = sweep(&missing, &f.store, 85, &mut |_: &Path| Some(99.0));
    assert_eq!(outcome, SweepOutcome::NoPool);
}

/// Fail-Open Check: a volume that cannot be measured evicts nothing.
#[test]
#[serial_test::serial(build_slot_fds)]
fn an_unmeasurable_volume_evicts_nothing() {
    let f = fixture();
    let slot = make_slot(&f.pool, "o/r", 0, HOUR);
    let outcome = sweep(&f.pool, &f.store, 85, &mut |_: &Path| None);
    assert_eq!(outcome, SweepOutcome::Unmeasurable);
    assert!(slot.is_dir());
}

#[test]
#[serial_test::serial(build_slot_fds)]
fn an_under_threshold_volume_evicts_nothing() {
    let f = fixture();
    let slot = make_slot(&f.pool, "o/r", 0, HOUR);
    let outcome = sweep(&f.pool, &f.store, 85, &mut |_: &Path| Some(84.9));
    assert_eq!(outcome, SweepOutcome::UnderThreshold(84.9));
    assert!(slot.is_dir());
}

/// Fail-Open Check: a pool root that cannot be listed evicts nothing.
#[test]
#[serial_test::serial(build_slot_fds)]
fn an_unlistable_pool_evicts_nothing() {
    let f = fixture();
    make_slot(&f.pool, "o/r", 0, HOUR);
    std::fs::set_permissions(&f.pool, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let outcome = sweep(&f.pool, &f.store, 85, &mut |_: &Path| Some(99.0));
    std::fs::set_permissions(&f.pool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(
        matches!(outcome, SweepOutcome::Unlistable(_)),
        "{outcome:?}"
    );
    assert!(f.pool.join("o/r/slot-0").is_dir());
}

/// The budget arithmetic end to end: 3 slots read 90%, so the two oldest go
/// and the sweep stops at 80%, under the 85% threshold.
#[test]
#[serial_test::serial(build_slot_fds)]
fn an_over_threshold_volume_evicts_oldest_first_until_below() {
    let f = fixture();
    let oldest = make_slot(&f.pool, "o/r", 0, 3 * HOUR);
    let middle = make_slot(&f.pool, "p/q", 1, 2 * HOUR);
    let newest = make_slot(&f.pool, "o/r", 2, HOUR);
    let report = swept(sweep(&f.pool, &f.store, 85, &mut per_slot(75.0)));
    assert_eq!(report.evicted, vec![oldest.clone(), middle.clone()]);
    assert!(report.spared.is_empty() && !report.failed());
    assert_eq!(report.usage_after, Some(80.0));
    assert!(!oldest.exists() && !middle.exists());
    assert!(newest.is_dir(), "the newest slot keeps its warm cache");
    // No `.evicting.` tree is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(f.pool.join("o/r"))
        .expect("repo dir")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".evicting."))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // The evicted slot's lease is free again.
    assert!(f.store.try_acquire(0).expect("lock").is_some());
}

/// Run a sweep that wants every slot gone and return the report for slot-0,
/// which the caller made busy, plus whether slot-1 (idle) was evicted.
fn sweep_with_busy_slot_0(f: &Fixture) -> (Vec<(PathBuf, Spared)>, bool) {
    let busy = f.pool.join("o/r/slot-0");
    let idle = make_slot(&f.pool, "o/r", 1, HOUR);
    let report = swept(sweep(&f.pool, &f.store, 85, &mut |_: &Path| Some(95.0)));
    assert!(busy.is_dir(), "the busy slot was evicted");
    assert!(!report.failed(), "{:?}", report.failed);
    (report.spared, report.evicted == vec![idle])
}

#[test]
#[serial_test::serial(build_slot_fds)]
fn a_held_lease_spares_its_slot() {
    let f = fixture();
    make_slot(&f.pool, "o/r", 0, 3 * HOUR);
    let _held = f.store.try_acquire(0).expect("lock").expect("free");
    let (spared, idle_evicted) = sweep_with_busy_slot_0(&f);
    assert_eq!(spared, vec![(f.pool.join("o/r/slot-0"), Spared::LeaseHeld)]);
    assert!(idle_evicted);
}

#[test]
#[serial_test::serial(build_slot_fds)]
fn an_orphaned_build_spares_its_slot() {
    let f = fixture();
    make_slot(&f.pool, "o/r", 0, 3 * HOUR);
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("stand-in build");
    let mut record = HolderRecord::new(0, "cargo test", "/repo");
    record.child_pid = Some(child.id());
    let mut guard = f.store.try_acquire(0).expect("lock").expect("free");
    guard.write_record(&record).expect("record");
    guard.release_keeping_record();
    let (spared, idle_evicted) = sweep_with_busy_slot_0(&f);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        spared,
        vec![(f.pool.join("o/r/slot-0"), Spared::OrphanedBuild(child.id()))]
    );
    assert!(idle_evicted);
    // The record that keeps the slot held survived the probe.
    assert!(
        !std::fs::read(f.store.path().join("slot-0.lock"))
            .expect("read")
            .is_empty()
    );
}

/// Fail-Open Check: a slot record that cannot be read spares the slot.
#[test]
#[serial_test::serial(build_slot_fds)]
fn a_corrupt_slot_record_spares_its_slot() {
    let f = fixture();
    make_slot(&f.pool, "o/r", 0, 3 * HOUR);
    std::fs::write(f.store.path().join("slot-0.lock"), b"not a record").expect("corrupt");
    let (spared, idle_evicted) = sweep_with_busy_slot_0(&f);
    assert!(
        matches!(spared.as_slice(), [(_, Spared::Unknown(e))] if e.contains("corrupt")),
        "{spared:?}"
    );
    assert!(idle_evicted);
}

/// Fail-Open Check: a slot file that cannot be opened spares the slot.
#[test]
#[serial_test::serial(build_slot_fds)]
fn a_broken_slot_file_spares_its_slot() {
    let f = fixture();
    make_slot(&f.pool, "o/r", 0, 3 * HOUR);
    std::fs::create_dir_all(f.store.path().join("slot-0.lock")).expect("a directory, not a file");
    let (spared, idle_evicted) = sweep_with_busy_slot_0(&f);
    assert!(
        matches!(spared.as_slice(), [(_, Spared::Unknown(_))]),
        "{spared:?}"
    );
    assert!(idle_evicted);
}

#[test]
#[serial_test::serial(build_slot_fds)]
fn a_held_cargo_lock_spares_its_slot() {
    let f = fixture();
    let slot = make_slot(&f.pool, "o/r", 0, 3 * HOUR);
    let lock = File::create(slot.join("debug/.cargo-lock")).expect("lock file");
    lock.try_lock().expect("hold cargo's lock");
    let (spared, idle_evicted) = sweep_with_busy_slot_0(&f);
    drop(lock);
    assert_eq!(spared, vec![(slot, Spared::CargoLockHeld)]);
    assert!(idle_evicted);
    // A probe that spared the slot left its lease free.
    assert!(f.store.try_acquire(0).expect("lock").is_some());
}

#[test]
#[serial_test::serial(build_slot_fds)]
fn a_live_seed_staging_spares_its_slot() {
    let f = fixture();
    let slot = make_slot(&f.pool, "o/r", 0, 3 * HOUR);
    let staging = f
        .pool
        .join(format!("o/r/.slot-0.seeding.{}.1", std::process::id()));
    std::fs::create_dir_all(&staging).expect("staging tree");
    let (spared, idle_evicted) = sweep_with_busy_slot_0(&f);
    assert_eq!(spared, vec![(slot, Spared::Seeding)]);
    assert!(idle_evicted);
    assert!(
        staging.is_dir(),
        "a live seed's staging tree is not touched"
    );
}

/// Fail-Open Check: a delete that does not finish is reported as failed, and
/// the renamed tree is finished off by the next sweep. A live other process's
/// `.evicting.` tree is left to it.
#[test]
#[serial_test::serial(build_slot_fds)]
fn a_failed_removal_is_reported_and_left_for_the_next_sweep() {
    let f = fixture();
    let slot = make_slot(&f.pool, "o/r", 0, HOUR);
    // pid 1 is always alive and never this test.
    let foreign = f.pool.join("o/r/.slot-7.evicting.1.1");
    std::fs::create_dir_all(&foreign).expect("foreign tree");
    let debug = slot.join("debug");
    std::fs::set_permissions(&debug, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let report = swept(sweep(&f.pool, &f.store, 85, &mut |_: &Path| Some(95.0)));
    assert!(report.failed(), "{report:?}");
    assert!(report.evicted.is_empty());
    let doomed: Vec<PathBuf> = std::fs::read_dir(f.pool.join("o/r"))
        .expect("repo dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p != &foreign && p.to_string_lossy().contains(".evicting."))
        .collect();
    assert_eq!(doomed.len(), 1, "{doomed:?}");
    assert!(
        !slot.exists(),
        "the slot name is gone even though the delete failed"
    );
    std::fs::set_permissions(
        doomed[0].join("debug"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("chmod back");
    let again = swept(sweep(&f.pool, &f.store, 85, &mut |_: &Path| Some(95.0)));
    assert_eq!(again.leftovers_removed, 1);
    assert!(!again.failed(), "{again:?}");
    assert!(!doomed[0].exists());
    assert!(foreign.is_dir(), "a live process's tree is left to it");
}
