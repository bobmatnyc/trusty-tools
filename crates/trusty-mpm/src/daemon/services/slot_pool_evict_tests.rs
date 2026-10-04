//! Tests for the daemon's slot-pool eviction tick (#8451).
//!
//! What: the cadence parse, the loop's cancel, the scratch-root refusal, and
//! the tick's log line for each sweep outcome against a temp pool, a temp
//! lease store and a scripted volume reading. The sweep's in-use guard is
//! `core::build_lease::evict`'s suite.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::{
    DEFAULT_INTERVAL_SECS, TickLog, evict_loop, evict_measured, overrun_warning,
    parse_interval_secs,
};
use crate::core::build_lease::slots::SlotDir;
use crate::daemon::state::DaemonState;

fn pool_with_slot(tmp: &Path) -> (PathBuf, PathBuf, SlotDir) {
    let pool = tmp.join("pool");
    let slot = pool.join("o/r/slot-0");
    std::fs::create_dir_all(slot.join("debug")).expect("slot");
    let store = SlotDir::at(tmp.join("store")).expect("store");
    (pool, slot, store)
}

/// One uncancelled tick at 85%, with home the temp dir holding the pool — the
/// production shape, where the pool lives under home.
fn tick(pool: &Path, store: &SlotDir, measure: &mut dyn FnMut(&Path) -> Option<f32>) -> TickLog {
    let home = pool.parent().expect("pool parent");
    evict_measured(pool, home, Ok(store), 85, measure, &|| false)
}

#[test]
fn the_interval_falls_back_on_junk_and_zero() {
    assert_eq!(parse_interval_secs(None), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some("0")), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some("soon")), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some(" 30 ")), 30);
}

/// Fail-Open Check: no lease store means no slot can be proven idle.
#[test]
fn an_unusable_store_evicts_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, slot, _store) = pool_with_slot(tmp.path());
    let log = evict_measured(
        &pool,
        tmp.path(),
        Err("store broken".into()),
        85,
        &mut |_: &Path| Some(99.0),
        &|| false,
    );
    assert!(
        matches!(&log, TickLog::Warn(m) if m.contains("store broken")),
        "{log:?}"
    );
    assert!(slot.is_dir());
}

/// Fail-Open Check: an unmeasurable volume warns and evicts nothing.
#[test]
#[serial_test::serial(build_slot_fds)]
fn an_unmeasurable_tick_warns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, slot, store) = pool_with_slot(tmp.path());
    let log = tick(&pool, &store, &mut |_: &Path| None);
    assert!(
        matches!(&log, TickLog::Warn(m) if m.contains("cannot be measured")),
        "{log:?}"
    );
    assert!(slot.is_dir());
}

#[test]
#[serial_test::serial(build_slot_fds)]
fn a_tick_evicts_over_the_threshold() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, slot, store) = pool_with_slot(tmp.path());
    let mut reading = [99.0_f32, 99.0, 50.0].into_iter();
    let log = tick(&pool, &store, &mut |_: &Path| reading.next());
    assert!(
        matches!(&log, TickLog::Info(m) if m.contains("evicted 1 slot dir")),
        "{log:?}"
    );
    assert!(!slot.exists());
    let quiet = tick(&pool, &store, &mut |_: &Path| Some(10.0));
    assert_eq!(quiet, TickLog::Quiet);
}

/// Fail-Open Check: a removal that does not finish is a `Warn`, never the
/// success line.
#[test]
#[serial_test::serial(build_slot_fds)]
fn a_failed_removal_tick_warns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, slot, store) = pool_with_slot(tmp.path());
    std::fs::write(slot.join("debug/artifact"), b"x").expect("artifact");
    std::fs::set_permissions(slot.join("debug"), std::fs::Permissions::from_mode(0o555))
        .expect("chmod");
    let log = tick(&pool, &store, &mut |_: &Path| Some(99.0));
    // Restore so the tempdir can be cleaned up.
    for entry in std::fs::read_dir(pool.join("o/r")).expect("repo").flatten() {
        let _ = std::fs::set_permissions(
            entry.path().join("debug"),
            std::fs::Permissions::from_mode(0o755),
        );
    }
    assert!(
        matches!(&log, TickLog::Warn(m) if m.contains("did not complete")),
        "{log:?}"
    );
}

/// Fail-Open Check: a root at home is refused at `warn` and evicts nothing.
#[test]
#[serial_test::serial(build_slot_fds)]
fn a_refused_root_tick_warns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, slot, store) = pool_with_slot(tmp.path());
    let log = evict_measured(
        &pool,
        &pool,
        Ok(&store),
        85,
        &mut |_: &Path| Some(99.0),
        &|| false,
    );
    assert!(
        matches!(&log, TickLog::Warn(m) if m.contains("refusing to sweep")),
        "{log:?}"
    );
    assert!(slot.is_dir());
}

#[test]
#[serial_test::serial(build_slot_fds)]
fn a_cancelled_tick_says_it_stopped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, slot, store) = pool_with_slot(tmp.path());
    let log = evict_measured(
        &pool,
        tmp.path(),
        Ok(&store),
        85,
        &mut |_: &Path| Some(99.0),
        &|| true,
    );
    assert!(
        matches!(&log, TickLog::Info(m) if m.contains("stopped early")),
        "{log:?}"
    );
    assert!(slot.is_dir());
}

#[test]
fn a_pass_longer_than_the_interval_warns() {
    let interval = Duration::from_secs(600);
    assert_eq!(overrun_warning(interval, interval), None);
    let msg = overrun_warning(Duration::from_secs(601), interval).expect("an overrun warns");
    assert!(msg.contains("601s") && msg.contains("600s"), "{msg}");
}

/// A scratch-rooted daemon is a test process: its tick must refuse before it
/// reaches the operator's real pool (#6348).
#[tokio::test]
#[serial_test::serial]
async fn a_scratch_rooted_daemon_evicts_nothing() {
    let dir = tempfile::tempdir().expect("scratch framework root");
    let state = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    assert!(
        crate::daemon::host_state_refusal(&state).is_some(),
        "the refusal is what keeps this tick off the real pool"
    );
    tokio::time::timeout(
        Duration::from_secs(30),
        super::run_one_tick(&state, Duration::from_secs(600), &Default::default()),
    )
    .await
    .expect("a refused tick returns at once");
}

#[tokio::test]
#[serial_test::serial]
async fn evict_loop_exits_on_cancel() {
    let dir = tempfile::tempdir().expect("scratch framework root");
    let state = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = tokio::spawn(evict_loop(
        state,
        Duration::from_secs(3600),
        cancel.child_token(),
    ));
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(30), handle)
        .await
        .expect("the loop must exit on cancel")
        .expect("the loop must not panic");
}
