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

use crate::core::twin_arming::{RecordStore, process_facts};
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

/// Why the hook's process is not the `claude` tm launched as the Architect.
///
/// Why: #8878 PR-I — the deny text and `tm fleet status` name the first
/// failed check. Each arm carries no PID, start time, path or error detail,
/// so its text reveals nothing `tm fleet status` does not already print.
/// Test: `each_identity_failure_names_its_own_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchRefusal {
    /// The process table could not be read.
    ProcessLookup,
    /// No `claude` process is an ancestor of the hook.
    NoClaudeAncestor,
    /// No launch record exists for the `claude` ancestor.
    NoLaunchRecord,
    /// The launch record could not be read or parsed.
    UnreadableRecord,
    /// The record's own PID differs from the one its file name gives.
    RecordPidMismatch,
    /// The record's start time differs: the PID was reused.
    StartTimeMismatch,
    /// The record was written for another project directory.
    OtherProject,
}

impl std::fmt::Display for LaunchRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ProcessLookup => "the process table could not be read",
            Self::NoClaudeAncestor => "no `claude` process is an ancestor of the hook",
            Self::NoLaunchRecord => {
                "the `claude` ancestor has no Architect launch record (`tm fleet init` did not \
                 launch it)"
            }
            Self::UnreadableRecord => "the Architect launch record could not be read or parsed",
            Self::RecordPidMismatch => {
                "the Architect launch record names a PID other than its file name"
            }
            Self::StartTimeMismatch => {
                "the Architect launch record's start time differs from the `claude` ancestor's \
                 (PID reuse)"
            }
            Self::OtherProject => {
                "the Architect launch record was written for another project directory"
            }
        })
    }
}

/// Whether the hook runs under the `claude` tm launched as the Architect.
///
/// Why: the process half of the Architect identity (ruling A); the caller
/// adds the thread and profile checks.
/// What: [`check_launched_architect`] is `Ok`.
/// Test: `a_launch_record_binds_one_process`,
/// `a_supervisor_stamp_without_a_launch_record_is_denied`.
pub fn is_launched_architect(
    root: &Path,
    project_dir: &Path,
    nearest: impl FnOnce() -> Result<Option<ClaudeProcess>, String>,
) -> bool {
    check_launched_architect(root, project_dir, nearest).is_ok()
}

/// [`is_launched_architect`], naming the first check that failed.
///
/// Why: #8878 PR-I; the verdict is unchanged, only the reason is new.
/// What: `Ok` only when `nearest` yields a `claude`, a record exists for its
/// PID under `root`, and the record's PID, start time and project directory
/// equal the live process's and `project_dir`. Otherwise the first failure,
/// in that order, as a [`LaunchRefusal`].
/// Test: `each_identity_failure_names_its_own_reason`,
/// `the_reason_verdict_equals_the_bool_verdict`.
pub fn check_launched_architect(
    root: &Path,
    project_dir: &Path,
    nearest: impl FnOnce() -> Result<Option<ClaudeProcess>, String>,
) -> Result<(), LaunchRefusal> {
    let claude = nearest()
        .map_err(|_| LaunchRefusal::ProcessLookup)?
        .ok_or(LaunchRefusal::NoClaudeAncestor)?;
    let record = ARCHITECT_RECORDS
        .read(root, claude.pid)
        .map_err(|_| LaunchRefusal::UnreadableRecord)?
        .ok_or(LaunchRefusal::NoLaunchRecord)?;
    if record.pid != claude.pid {
        return Err(LaunchRefusal::RecordPidMismatch);
    }
    if record.start_time != claude.start_time {
        return Err(LaunchRefusal::StartTimeMismatch);
    }
    if !same_dir(&record.project_dir, project_dir) {
        return Err(LaunchRefusal::OtherProject);
    }
    Ok(())
}

/// Parent hops walked above a live Architect `claude`: its disclaim wrapper,
/// its pane shell and the tmux server.
const LINEAGE_HOPS: usize = 3;

/// The live Architect `claude` processes and their ancestors, by PID (#8902).
///
/// Why: the pane guard binds the Architect's tmux pane to its launch record
/// rather than a registry of its own. A pane whose `#{pane_pid}` is in this
/// list runs the recorded `claude`.
/// What: reads every `<pid>.architect` record under `root`. A record whose
/// process is gone, or runs with another start time, is stale and skipped.
/// Each live `claude` adds its PID and up to [`LINEAGE_HOPS`] parents. No
/// record directory is `Ok(vec![])`. An unreadable directory, a record name
/// that is not a PID, or a record that does not read is `Err`, which the guard
/// treats as a live Architect.
/// Test: `a_live_launch_record_yields_its_lineage_and_a_stale_one_none`.
pub fn live_architect_lineage(root: &Path) -> Result<Vec<u32>, String> {
    let dir = root.join(ARCHITECT_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
        Ok(entries) => entries,
    };
    let mut lineage = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some(ARCHITECT_EXT) {
            continue;
        }
        let pid = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| format!("{} is not named for a PID", path.display()))?;
        let Some(record) = ARCHITECT_RECORDS.read(root, pid)? else {
            continue;
        };
        let Ok(facts) = process_facts(pid) else {
            continue;
        };
        if facts.start_time != record.start_time {
            continue;
        }
        lineage.push(pid);
        let mut parent = facts.parent;
        for _ in 0..LINEAGE_HOPS {
            let Some(p) = parent.filter(|p| *p > 1) else {
                break;
            };
            lineage.push(p);
            parent = process_facts(p).ok().and_then(|f| f.parent);
        }
    }
    Ok(lineage)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #8902: a record of a running process yields it and its parent (this
    /// test process); the same record once the process exits yields nothing;
    /// a record named for no PID is an error the guard fails closed on.
    #[test]
    fn a_live_launch_record_yields_its_lineage_and_a_stale_one_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(".trusty-mpm");
        assert_eq!(live_architect_lineage(&root), Ok(Vec::new()), "no records");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        record_architect(&root, child.id(), dir.path()).expect("record");
        let lineage = live_architect_lineage(&root).expect("lineage");
        assert_eq!(lineage.first(), Some(&child.id()));
        assert!(lineage.contains(&std::process::id()), "{lineage:?}");
        child.kill().expect("kill");
        child.wait().expect("reap");
        assert_eq!(live_architect_lineage(&root), Ok(Vec::new()), "stale");
        std::fs::write(root.join(ARCHITECT_DIR).join("x.architect"), "{}").expect("write");
        assert!(live_architect_lineage(&root).is_err());
    }
}
