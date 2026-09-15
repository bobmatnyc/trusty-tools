//! The cross-process half of a sweep's status: the marker the sweep itself
//! writes at both edges of a pass (#8059).
//!
//! Why: `tm doctor` runs DAEMONLESS (#6336 — see
//! `bin/tm/commands/doctor_local`), so the `background_sweeps` row is evaluated
//! in the CLI process. [`super::sweep_status`]'s atomics belong to whichever
//! process wrote them, and in the CLI nothing ever writes them, so the row read
//! "ON, idle, no pass yet" on 2026-09-15 while `pgrep -P <daemon pid>` showed
//! the reclaim sweep's `git`/`gh` children running. A row whose source cannot
//! observe the sweep is not a diagnostic.
//!
//! What: one small JSON marker per sweep at `<framework-root>/sweeps/<name>.json`,
//! written by the sweep's own timing guard — [`record_start`] before the pass
//! runs, [`record_finish`] when the guard drops — and read by [`read_pass`].
//! Start and finish timestamps are recorded, so "started and has not finished"
//! is a fact on disk rather than an inference from a clock.
//!
//! **A marker whose writer is gone reads as IDLE.** A daemon `SIGKILL`ed
//! mid-pass never runs its guard, and without the liveness check that unfinished
//! marker would pin every later `tm doctor` to "IN FLIGHT" — trading one false
//! report for a permanent one.
//!
//! **Fail-open throughout.** Every write logs at `warn` and returns; every read
//! answers `None`. A diagnostic row must never be able to break a sweep.
//! Test: `a_started_sweep_reads_as_running_from_another_process`,
//! `a_finished_pass_reads_as_idle_with_its_duration`,
//! `a_marker_whose_writer_is_gone_reads_as_idle`,
//! `an_unreadable_marker_reads_as_nothing`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Directory, under the framework root, holding one marker per sweep.
///
/// Why spelled once: the writing daemon and the reading CLI are separate
/// processes with no other channel, exactly like `core::stop_spool`.
pub(crate) const SWEEPS_DIR: &str = "sweeps";

/// One sweep's last observed pass, as recorded on disk.
///
/// Test: `a_started_sweep_reads_as_running_from_another_process`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SweepMarker {
    /// The process that wrote this marker; checked for liveness on read.
    pid: u32,
    /// When the pass started, in Unix milliseconds.
    started_unix_ms: u64,
    /// When the pass finished, or `None` while it is in flight.
    #[serde(default)]
    finished_unix_ms: Option<u64>,
    /// How long the finished pass took, measured by the guard itself.
    #[serde(default)]
    last_pass_ms: Option<u64>,
}

/// What a marker says about its sweep, once liveness has been applied.
///
/// Test: `a_marker_whose_writer_is_gone_reads_as_idle`.
pub(crate) struct SweepPass {
    /// A pass started, has not finished, and its writer is still alive.
    pub running: bool,
    /// How long the last COMPLETED pass took, if one has completed.
    pub last_pass: Option<Duration>,
}

/// Where `sweep`'s marker lives under `framework_root`.
pub(crate) fn marker_path(framework_root: &Path, sweep: &str) -> PathBuf {
    framework_root
        .join(SWEEPS_DIR)
        .join(format!("{sweep}.json"))
}

/// Record that a pass has STARTED, clearing any previous finish.
///
/// Why the clear: "started and has not finished" is the whole signal. Leaving
/// the previous pass's `finished_unix_ms` in place would make a running sweep
/// read as idle — the exact defect #8059 reports, reintroduced one layer down.
/// Test: `a_started_sweep_reads_as_running_from_another_process`.
pub(crate) fn record_start(path: &Path) {
    write_marker(
        path,
        &SweepMarker {
            pid: std::process::id(),
            started_unix_ms: now_unix_ms(),
            finished_unix_ms: None,
            last_pass_ms: None,
        },
    );
}

/// Record that the pass ENDED, however it ended, and how long it took.
///
/// What: preserves the recorded start when the marker is still readable, and
/// otherwise reconstructs it from `elapsed` so the pair stays coherent.
/// Test: `a_finished_pass_reads_as_idle_with_its_duration`.
pub(crate) fn record_finish(path: &Path, elapsed: Duration) {
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX - 1);
    let now = now_unix_ms();
    let started = read_marker(path)
        .map(|m| m.started_unix_ms)
        .unwrap_or_else(|| now.saturating_sub(elapsed_ms));
    write_marker(
        path,
        &SweepMarker {
            pid: std::process::id(),
            started_unix_ms: started,
            finished_unix_ms: Some(now),
            last_pass_ms: Some(elapsed_ms),
        },
    );
}

/// What `sweep`'s marker says right now, or `None` when there is no readable one.
///
/// Why `None` rather than a default: "no marker" and "an idle sweep" are
/// different facts, and the caller folds them differently — see
/// [`super::sweep_status::rows`].
/// What: `running` is true only for a marker with no finish AND a live writer.
/// Test: `a_marker_whose_writer_is_gone_reads_as_idle`,
/// `an_unreadable_marker_reads_as_nothing`.
pub(crate) fn read_pass(framework_root: &Path, sweep: &str) -> Option<SweepPass> {
    let marker = read_marker(&marker_path(framework_root, sweep))?;
    let running =
        marker.finished_unix_ms.is_none() && crate::core::process::is_process_alive(marker.pid);
    Some(SweepPass {
        running,
        last_pass: marker.last_pass_ms.map(Duration::from_millis),
    })
}

/// Parse one marker, treating every failure as absence.
fn read_marker(path: &Path) -> Option<SweepMarker> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write a marker temp-then-rename so a concurrent reader never sees a partial
/// file.
fn write_marker(path: &Path, marker: &SweepMarker) {
    let Some(dir) = path.parent() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        warn!(path = %path.display(), "sweep marker: cannot create {SWEEPS_DIR}: {e}");
        return;
    }
    let bytes = match serde_json::to_vec(marker) {
        Ok(b) => b,
        Err(e) => {
            warn!(path = %path.display(), "sweep marker: cannot encode: {e}");
            return;
        }
    };
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        warn!(path = %tmp.display(), "sweep marker: cannot write: {e}");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        warn!(path = %path.display(), "sweep marker: cannot publish: {e}");
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Wall-clock now, in Unix milliseconds, saturating at 0 before the epoch.
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "sweep_state_file_tests.rs"]
mod sweep_state_file_tests;
