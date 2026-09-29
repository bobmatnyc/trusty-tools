//! The Architect's process-bound identity (#8878, owner ruling option A,
//! 2026-09-29).
//!
//! Why: the trust-anchor floor lets only the Architect's main thread write the
//! operator's grants. An identity read from the environment and project files
//! alone — the #8453 launch stamp, `CLAUDE_PROJECT_DIR`, `profile =
//! "supervisor"` — is spoofed by `TRUSTY_MPM_SESSION_PROFILE=supervisor claude`
//! run from the Architect directory. The ruling binds the Architect to the one
//! `claude` process tm itself launched for it.
//! What: `tm fleet init` records the `claude` it started in the `tm-architect`
//! session with [`record_architect`], by PID and start time, under
//! `~/.trusty-mpm/architect-launch/<pid>.architect` — the twin's
//! [`RecordStore`] mechanics in another directory. [`is_launched_architect`]
//! holds only when the hook's nearest `claude` ancestor, walked as
//! [`crate::core::twin_arming::nearest_claude_ancestor`] does, has a record
//! with the same PID, start time and project. Every error is `false`.
//! Test: `pm_guard_trust_anchor_tests.rs` in the `tm` binary
//! (`a_supervisor_stamp_without_a_launch_record_is_denied`,
//! `a_launch_record_binds_one_process`); `tests/tm_fleet.rs`
//! (`fleet_init_launches_the_architect_and_status_is_complete`).

use std::path::Path;

use crate::core::twin_arming::RecordStore;
use crate::core::twin_identity::{ArmingRecord, ClaudeProcess, same_dir};

/// Directory, under the `~/.trusty-mpm` root, of the Architect launch records.
pub const ARCHITECT_DIR: &str = "architect-launch";

/// Extension of an Architect launch record, distinct so the anchor floor can
/// refuse a write of one whose directory it cannot place.
pub const ARCHITECT_EXT: &str = "architect";

/// The Architect launch records: `architect-launch/<pid>.architect`.
pub const ARCHITECT_RECORDS: RecordStore = RecordStore {
    dir: ARCHITECT_DIR,
    ext: ARCHITECT_EXT,
};

/// Record the `claude` at `pid`, launched in `project_dir`, as the Architect.
///
/// Why: only tm's Architect launch path calls this, with the PID of the
/// `claude` it found in the session it had just created.
/// What: [`RecordStore::record_claude`] into [`ARCHITECT_RECORDS`] under
/// `root`, the `~/.trusty-mpm` directory.
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`.
pub fn record_architect(root: &Path, pid: u32, project_dir: &Path) -> Result<ArmingRecord, String> {
    ARCHITECT_RECORDS.record_claude(root, pid, project_dir)
}

/// Whether the hook runs under the `claude` tm launched as the Architect.
///
/// Why: the process half of the Architect identity (ruling A); the caller
/// adds the thread and profile checks.
/// What: `true` only when `nearest` yields a `claude`, a record exists for its
/// PID under `root`, and the record's PID, start time and project directory
/// equal the live process's and `project_dir`. A table error, no `claude`, a
/// missing or unreadable record, a reused PID (another start time) or another
/// project is `false`.
/// Test: `a_launch_record_binds_one_process`,
/// `a_supervisor_stamp_without_a_launch_record_is_denied`.
pub fn is_launched_architect(
    root: &Path,
    project_dir: &Path,
    nearest: impl FnOnce() -> Result<Option<ClaudeProcess>, String>,
) -> bool {
    let Ok(Some(claude)) = nearest() else {
        return false;
    };
    let Ok(Some(record)) = ARCHITECT_RECORDS.read(root, claude.pid) else {
        return false;
    };
    record.pid == claude.pid
        && record.start_time == claude.start_time
        && same_dir(&record.project_dir, project_dir)
}
