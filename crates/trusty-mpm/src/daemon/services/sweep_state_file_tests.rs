//! Tests for the cross-process sweep marker (#8059).
//!
//! Every case drives the marker DIRECTLY — no sweep, no timer — because the
//! defect is about what a second process can read, not about how long a pass
//! takes.

use std::time::Duration;

use super::{SweepMarker, marker_path, read_pass, record_finish, record_start};

/// The sweep name used throughout; any name would do.
const SWEEP: &str = "worktree-reclaim";

/// 🔴 #8059 REGRESSION: a started, unfinished pass reads as RUNNING from a
/// reader that is not the daemon.
///
/// Why this is the assertion: `tm doctor` evaluates the row in the CLI process
/// (#6336), where the sweep's in-memory atomics are untouched. The marker is the
/// only thing that can carry "a pass is in flight" across that boundary.
#[test]
fn a_started_sweep_reads_as_running_from_another_process() {
    let root = tempfile::TempDir::new().expect("tempdir");
    record_start(&marker_path(root.path(), SWEEP));

    let pass = read_pass(root.path(), SWEEP).expect("a marker was written");
    assert!(
        pass.running,
        "a started pass that has not finished is running"
    );
    assert_eq!(pass.last_pass, None, "no pass has completed yet");
}

/// A finished pass reads as idle and carries the duration the guard measured.
#[test]
fn a_finished_pass_reads_as_idle_with_its_duration() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let path = marker_path(root.path(), SWEEP);
    record_start(&path);
    record_finish(&path, Duration::from_millis(1_500));

    let pass = read_pass(root.path(), SWEEP).expect("a marker was written");
    assert!(!pass.running, "a finished pass is not in flight");
    assert_eq!(pass.last_pass, Some(Duration::from_millis(1_500)));
}

/// A marker left unfinished by a process that is gone reads as IDLE.
///
/// Why: a daemon `SIGKILL`ed mid-pass never runs its guard. Without the liveness
/// check that marker would report "IN FLIGHT" on every later `tm doctor` run,
/// which is a worse defect than the one #8059 fixes.
#[test]
fn a_marker_whose_writer_is_gone_reads_as_idle() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let path = marker_path(root.path(), SWEEP);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let orphan = SweepMarker {
        // `u32::MAX` is above any real pid and is explicitly rejected by
        // `core::process::is_process_alive`.
        pid: u32::MAX,
        started_unix_ms: 1,
        finished_unix_ms: None,
        last_pass_ms: None,
    };
    std::fs::write(&path, serde_json::to_vec(&orphan).expect("encode")).expect("write");

    let pass = read_pass(root.path(), SWEEP).expect("a marker exists");
    assert!(
        !pass.running,
        "an unfinished marker whose writer is gone must not report a live pass"
    );
}

/// No marker, and an unparsable one, both read as nothing at all.
#[test]
fn an_unreadable_marker_reads_as_nothing() {
    let root = tempfile::TempDir::new().expect("tempdir");
    assert!(
        read_pass(root.path(), SWEEP).is_none(),
        "no marker is not an idle sweep"
    );

    let path = marker_path(root.path(), SWEEP);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, b"{ not json").expect("write");
    assert!(
        read_pass(root.path(), SWEEP).is_none(),
        "a corrupt marker is discarded, never guessed at"
    );
}
