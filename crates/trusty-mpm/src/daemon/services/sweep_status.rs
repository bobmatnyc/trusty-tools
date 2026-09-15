//! The daemon's single background-maintenance lane, and what it did last
//! (#7965).
//!
//! Why: the daemon runs two unattended subprocess sweeps — the merged-PR
//! worktree reclaim ([`super::merged_pr_reclaim`]) and the in-project hygiene
//! pass ([`crate::daemon::managed_routes::inproject_hygiene`]). Before #7965 they
//! started within milliseconds of each other at boot and then ran concurrently,
//! unbounded, for as long as they liked: hygiene alone was measured still
//! running NINE MINUTES after start on a host with 72 base clones, ~10 `git`
//! subprocesses each including a network fetch. The resource the two of them
//! consume is the one every HTTP handler also needs — this process's CPU and its
//! share of the host's IO — and with both of them on it `GET /health` answered in
//! ~5 s, past the 500 ms probe in [`crate::core::discovery`], so every `tm`
//! command reported the daemon unreachable.
//!
//! What: [`lane`] is one permit. Both sweeps take it for the duration of a pass,
//! so at most ONE subprocess storm exists at a time and neither can start while
//! the other is mid-pass. [`RECLAIM`] and [`HYGIENE`] are the in-memory records
//! this process reads, and [`super::sweep_state_file`] is the durable marker the
//! same guard writes for every OTHER process.
//!
//! # Why the row needs both (#8059)
//!
//! `tm doctor` runs daemonless (#6336), so the row is evaluated in the CLI
//! process — where the atomics are untouched and every sweep therefore reads
//! "ON, idle, no pass yet", which is what it reported on 2026-09-15 while the
//! daemon's reclaim sweep had live `git`/`gh` children. The atomics remain the
//! daemon's own lock-free answer; the marker is what makes the same answer
//! readable from outside it. The row takes the two together, so neither source
//! can silently become the only one.
//!
//! # Why a lane and not a thread-count cap
//!
//! Neither sweep is parallel inside itself; each is a serial loop on ONE
//! blocking-pool thread. What made the machine unusable was two such loops plus
//! `git fetch`'s own internal `index-pack` threads, all at once. Serialising the
//! two loops is what bounds the concurrent demand; the per-subprocess ceilings in
//! [`crate::core::bounded_proc`] are what bound each loop's duration.
//!
//! Test: `sweep_status_records_the_last_pass_duration`,
//! `sweep_status_guard_records_even_on_panic`,
//! `the_lane_serialises_two_sweeps` in `sweep_status_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

/// Sentinel for "this sweep has not completed a pass in this process".
///
/// Why a sentinel rather than `Option` behind a lock: the whole point of the
/// atomics is that the doctor row costs no lock on a busy daemon.
const NEVER: u64 = u64::MAX;

/// One sweep's observable state: is it on, is it running, how long did the last
/// pass take.
///
/// Why: an operator whose daemon has gone quiet needs to see a sweep OVERRUNNING
/// before they can connect it to the latency they are feeling. Before #7965 the
/// only evidence was `sample`.
/// What: two atomics — no lock, no persistence. `enabled` is NOT stored: it is
/// read from the env gate at display time so a doctor run always reports the live
/// policy rather than what boot happened to see.
/// Test: `sweep_status_records_the_last_pass_duration`.
pub(crate) struct SweepStatus {
    /// This sweep's name — the doctor row's label, and its marker's basename.
    name: &'static str,
    /// Wall-clock milliseconds the last completed pass took, or [`NEVER`].
    last_pass_ms: AtomicU64,
    /// Whether a pass is in flight right now.
    running: AtomicBool,
}

impl SweepStatus {
    /// An empty record — no pass has run.
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            last_pass_ms: AtomicU64::new(NEVER),
            running: AtomicBool::new(false),
        }
    }

    /// Mark a pass as started; the returned guard records its duration on drop.
    ///
    /// Why a guard rather than paired calls: a pass that panics must still clear
    /// `running`, or the doctor row would report a sweep in flight forever.
    /// What: `framework_root` is where the durable marker goes (#8059) — every
    /// production sweep passes the daemon's own root, and `None` keeps a pass
    /// in memory alone, which is what a unit test driving a local
    /// [`SweepStatus`] wants.
    /// Test: `sweep_status_guard_records_even_on_panic`,
    /// `a_running_sweep_is_reported_by_a_process_that_did_not_run_it`.
    pub(crate) fn begin(&'static self, framework_root: Option<&Path>) -> PassGuard {
        self.running.store(true, Ordering::Relaxed);
        // #8059: the atomic above is invisible to `tm doctor`, which runs in its
        // own process; the marker is the half that crosses it.
        let marker =
            framework_root.map(|root| super::sweep_state_file::marker_path(root, self.name));
        if let Some(path) = marker.as_deref() {
            super::sweep_state_file::record_start(path);
        }
        PassGuard {
            status: self,
            started: Instant::now(),
            marker,
        }
    }

    /// How long the last completed pass took, or `None` if none has.
    pub(crate) fn last_pass(&self) -> Option<Duration> {
        match self.last_pass_ms.load(Ordering::Relaxed) {
            NEVER => None,
            ms => Some(Duration::from_millis(ms)),
        }
    }

    /// Whether a pass is in flight right now.
    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

/// Records a pass's duration when it ends, however it ends.
///
/// Test: `sweep_status_guard_records_even_on_panic`.
pub(crate) struct PassGuard {
    status: &'static SweepStatus,
    started: Instant,
    /// Where this pass's durable marker lives, when one is being kept (#8059).
    marker: Option<PathBuf>,
}

impl Drop for PassGuard {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX - 1);
        self.status.last_pass_ms.store(ms, Ordering::Relaxed);
        self.status.running.store(false, Ordering::Relaxed);
        // #8059: the marker ends with the pass, panic or not — same guarantee
        // the atomics above get, for the same reason.
        if let Some(path) = self.marker.as_deref() {
            super::sweep_state_file::record_finish(path, elapsed);
        }
    }
}

/// The merged-PR worktree reclaim sweep's name, in the row and on disk.
pub(crate) const RECLAIM_NAME: &str = "worktree-reclaim";

/// The in-project hygiene sweep's name, in the row and on disk.
pub(crate) const HYGIENE_NAME: &str = "inproject-hygiene";

/// The merged-PR worktree reclaim sweep's record.
pub(crate) static RECLAIM: SweepStatus = SweepStatus::new(RECLAIM_NAME);

/// The in-project hygiene sweep's record.
pub(crate) static HYGIENE: SweepStatus = SweepStatus::new(HYGIENE_NAME);

/// The one background-maintenance permit both sweeps take.
///
/// Why one and not two: see the module doc — the failure was two concurrent
/// subprocess storms, and one permit is the smallest thing that makes that
/// impossible without ordering the sweeps against each other.
/// What: a `Semaphore` with a single permit, acquired for a whole pass. A sweep
/// that finds it taken WAITS rather than skipping, because both sweeps are
/// idempotent and a skipped hygiene pass would leave a base clone stale for the
/// life of the daemon (hygiene runs once per boot).
/// Test: `the_lane_serialises_two_sweeps`.
pub(crate) fn lane() -> &'static Semaphore {
    static LANE: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(1));
    &LANE
}

/// One row of the `background_sweeps` doctor check.
///
/// Test: `sweep_rows_report_each_sweeps_switch_and_last_pass`.
pub(crate) struct SweepRow {
    /// Human name of the sweep.
    pub name: &'static str,
    /// The environment variable that switches it off.
    pub env: &'static str,
    /// Whether it is enabled right now.
    pub enabled: bool,
    /// Whether a pass is in flight right now.
    pub running: bool,
    /// How long the last completed pass took, if any.
    pub last_pass: Option<Duration>,
}

/// Both sweeps' current state, for the doctor row (#7965, #8059).
///
/// Why here rather than in the doctor module: the two statics, the two markers
/// and the two env gates are this module's and the sweeps' own, and a doctor
/// check that re-derived any of them would drift from what the sweep actually
/// does.
/// What: each row folds this process's atomics together with the sweep's durable
/// marker under `framework_root` — see [`fold`].
/// Test: `sweep_rows_report_each_sweeps_switch_and_last_pass`,
/// `a_running_sweep_is_reported_by_a_process_that_did_not_run_it`.
pub(crate) fn rows(framework_root: &Path) -> Vec<SweepRow> {
    vec![
        fold(
            &RECLAIM,
            super::merged_pr_reclaim::ENV_ENABLED,
            super::merged_pr_reclaim::enabled(),
            framework_root,
        ),
        fold(
            &HYGIENE,
            crate::daemon::managed_routes::inproject_hygiene_sweep::ENV_ENABLED,
            crate::daemon::managed_routes::inproject_hygiene_sweep::enabled(),
            framework_root,
        ),
    ]
}

/// One sweep's row, from this process's atomics AND its on-disk marker (#8059).
///
/// Why both rather than the marker alone: inside the daemon the atomics are the
/// authoritative, lock-free answer and cost no syscall, and a marker that could
/// not be written must not silently downgrade the daemon's own row. So the two
/// are OR-ed for `running` and the atomics win for `last_pass` — a reader that
/// has neither (every process but the daemon, before #8059) now has one.
/// Test: `a_running_sweep_is_reported_by_a_process_that_did_not_run_it`,
/// `a_row_with_no_marker_still_reports_this_processs_own_pass`.
fn fold(
    status: &'static SweepStatus,
    env: &'static str,
    enabled: bool,
    framework_root: &Path,
) -> SweepRow {
    let durable = super::sweep_state_file::read_pass(framework_root, status.name);
    SweepRow {
        name: status.name,
        env,
        enabled,
        running: status.is_running() || durable.as_ref().is_some_and(|p| p.running),
        last_pass: status
            .last_pass()
            .or_else(|| durable.and_then(|p| p.last_pass)),
    }
}

/// The `background_sweeps` doctor row (#7965, #8059).
///
/// Why: before this, an operator whose `tm` commands had started timing out had
/// no way to see that a background sweep was mid-pass — the only evidence on the
/// reporting host was `sample` against the daemon. The row makes "hygiene has
/// been running for nine minutes" a thing you can read.
/// What: one line per sweep — its kill switch, whether it is on, whether a pass
/// is in flight, and how long the last completed pass took, read from the state
/// the sweep itself updates at both edges of a pass ([`rows`]). `Warn` when a
/// pass is currently running or the last one exceeded [`SLOW_PASS`], because that
/// is the state that costs request latency; `Ok` otherwise. Never `Unknown`:
/// both sources are always readable, and an absent marker is an absent pass.
/// Test: `background_sweeps_row_warns_on_a_slow_pass`,
/// `background_sweeps_row_is_ok_when_both_sweeps_are_idle`,
/// `a_running_sweep_is_reported_by_a_process_that_did_not_run_it`.
pub(crate) fn check_background_sweeps(framework_root: &Path) -> crate::core::doctor::DoctorCheck {
    use crate::core::doctor::{CheckStatus, DoctorCheck};

    let rows = rows(framework_root);
    let slow = rows
        .iter()
        .any(|r| r.running || r.last_pass.is_some_and(|d| d >= SLOW_PASS));
    let detail = rows
        .iter()
        .map(|r| {
            let state = if !r.enabled {
                format!("off via {}", r.env)
            } else if r.running {
                "ON, pass IN FLIGHT".to_string()
            } else {
                "ON, idle".to_string()
            };
            let last = match r.last_pass {
                Some(d) => format!("last pass {:.1}s", d.as_secs_f64()),
                None => "no pass yet".to_string(),
            };
            format!("{}: {state}, {last}", r.name)
        })
        .collect::<Vec<_>>()
        .join("; ");
    let status = if slow {
        CheckStatus::Warn
    } else {
        CheckStatus::Ok
    };
    DoctorCheck::new("background_sweeps", status, detail)
}

/// Pass duration past which a sweep is worth an operator's attention.
///
/// Why 60 seconds: both sweeps are bounded now — hygiene by its own pass budget,
/// the reclaim by its per-subprocess ceilings — so a pass over a minute means the
/// host has enough bases or worktrees that the sweep is a measurable share of the
/// daemon's time, which is the thing #7965 is about.
const SLOW_PASS: Duration = Duration::from_secs(60);

#[cfg(test)]
#[path = "sweep_status_tests.rs"]
mod sweep_status_tests;
