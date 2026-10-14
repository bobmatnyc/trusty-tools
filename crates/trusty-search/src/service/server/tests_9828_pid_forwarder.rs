//! The AppState pid forwarder follows a lazily spawned, recycled or
//! respawned embed sidecar, so `search.health` keeps reporting
//! `embedderd_rss_mb` (#9828).
//!
//! Why: in a live soak with `TRUSTY_EMBEDDER=python` the field was absent.
//! The lazy handle's pid slot reads 0 when `install_embedderd_pid_slot` runs,
//! and the forwarder stopped at its first 0, so it never saw the first spawn,
//! a recycle, or a respawn after a restart gap.
//! What: each test drives the source slot the way the supervisor does (first
//! spawn 0 -> A, recycle A -> B with no gap, crash or idle-shutdown A -> 0 ->
//! B) against real `sleep` processes, and reads the result through
//! `health_report`, the `search.health` call.
//! Test: this module.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::core::registry::IndexRegistry;
use crate::service::server::{health_report, SearchAppState};

/// Ceiling on each wait; a wait ends as soon as its condition holds.
const WAIT_CEILING: Duration = Duration::from_secs(10);

/// A live process with a real RSS, killed when dropped.
fn live_process() -> std::process::Child {
    std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn a sleep process")
}

/// Wait until the state reports `want` as the sidecar pid; true on success.
async fn wait_for_pid(state: &SearchAppState, want: Option<u32>) -> bool {
    tokio::time::timeout(WAIT_CEILING, async {
        while state.current_embedderd_pid() != want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

/// Kill and reap a fixture child.
fn reap(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Why: the lazy python arm installs its slot before the first spawn, so the
/// slot reads 0 at install; a recycle then publishes the new pid with no gap.
/// What: install a 0 slot, publish pid A (first spawn), then pid B (recycle);
/// `search.health` must carry `embedderd_rss_mb` after each, tracking B.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn pid_forwarder_follows_a_lazy_first_spawn_then_a_recycle() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let slot = Arc::new(AtomicU32::new(0));
    state.install_embedderd_pid_slot(Arc::clone(&slot)).await;
    let report = health_report(Arc::clone(&state)).await;
    assert!(
        report.get("embedderd_rss_mb").is_none(),
        "no sidecar yet, so no RSS: {report}"
    );

    let first = live_process();
    slot.store(first.id(), Ordering::Release);
    let saw_first = wait_for_pid(&state, Some(first.id())).await;
    let report = health_report(Arc::clone(&state)).await;
    let second = live_process();
    slot.store(second.id(), Ordering::Release);
    let saw_second = wait_for_pid(&state, Some(second.id())).await;
    let after_recycle = health_report(Arc::clone(&state)).await;
    let (first_pid, second_pid) = (first.id(), second.id());
    reap(first);
    reap(second);

    assert!(
        saw_first,
        "the lazy first spawn (pid {first_pid}) never reached the state: {:?}",
        state.current_embedderd_pid()
    );
    assert!(
        report.get("embedderd_rss_mb").is_some(),
        "search.health must report embedderd_rss_mb after a lazy spawn: {report}"
    );
    assert!(
        saw_second,
        "the recycled sidecar (pid {second_pid}) never reached the state"
    );
    assert!(
        after_recycle.get("embedderd_rss_mb").is_some(),
        "search.health must report embedderd_rss_mb after a recycle: {after_recycle}"
    );
}

/// Why: a crash restart and an idle shutdown both publish 0 before the next
/// pid. 0 means "no sidecar for now", never "stop forwarding".
/// What: install a live pid A, publish 0, confirm the field goes absent, then
/// publish pid B and confirm the state and `search.health` follow it.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn pid_forwarder_survives_a_zero_gap_and_follows_the_respawn() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let first = live_process();
    let slot = Arc::new(AtomicU32::new(first.id()));
    state.install_embedderd_pid_slot(Arc::clone(&slot)).await;
    let saw_first = wait_for_pid(&state, Some(first.id())).await;

    slot.store(0, Ordering::Release);
    let saw_gap = wait_for_pid(&state, None).await;
    let during_gap = health_report(Arc::clone(&state)).await;

    let second = live_process();
    slot.store(second.id(), Ordering::Release);
    let saw_second = wait_for_pid(&state, Some(second.id())).await;
    let after_respawn = health_report(Arc::clone(&state)).await;
    let second_pid = second.id();
    reap(first);
    reap(second);

    assert!(saw_first, "the installed pid never reached the state");
    assert!(saw_gap, "the restart gap never reached the state");
    assert!(
        during_gap.get("embedderd_rss_mb").is_none(),
        "no sidecar during the gap, so no RSS: {during_gap}"
    );
    assert!(
        saw_second,
        "the respawned sidecar (pid {second_pid}) never reached the state after a 0 gap: {:?}",
        state.current_embedderd_pid()
    );
    assert!(
        after_respawn.get("embedderd_rss_mb").is_some(),
        "search.health must report embedderd_rss_mb after a respawn: {after_respawn}"
    );
}

/// Why: with no stop at 0, shutdown must end the forwarder itself.
/// What: install a slot, stop the forwarder, then move the slot; the handle
/// is gone, the task has finished, and the state keeps reporting no sidecar.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn pid_forwarder_stops_on_shutdown() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let slot = Arc::new(AtomicU32::new(4242));
    state.install_embedderd_pid_slot(Arc::clone(&slot)).await;
    let task = state
        .embedderd_pid_forwarder_handle
        .lock()
        .await
        .clone()
        .expect("install records the forwarder");
    state.stop_embedderd_pid_forwarder().await;
    slot.store(4343, Ordering::Release);
    let finished = tokio::time::timeout(WAIT_CEILING, async {
        while !task.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok();
    tokio::time::sleep(Duration::from_millis(600)).await;

    assert!(finished, "the forwarder survived shutdown");
    assert!(state.embedderd_pid_forwarder_handle.lock().await.is_none());
    assert_eq!(
        state.current_embedderd_pid(),
        None,
        "a stopped forwarder must not copy a later pid"
    );
}
