//! The Architect exemption for `secrets.set` and a local `secrets.copy`
//! (#10459 P1, Architect clearance 2026-10-29 22:58Z).
//!
//! Why: the Architect, the one fleet session tm launched for the operator,
//! must write keys the operator denied to agents. An env marker or a profile
//! name is spoofable (#8878), so the exemption rests on the launch record tm
//! wrote for the `claude` it started: `tm fleet init` / `tm fleet relaunch`
//! write `~/.trusty-mpm/architect-launch/<pid>.architect`.
//! What: [`check_architect`] holds only when the caller's NEAREST Claude Code
//! process has a record whose PID, start time and project directory equal
//! the live process's PID, start time and working directory. Every failure
//! is an [`ArchitectRefusal`], and the caller then keeps the agent rules.
//! The record is read by a minimal reader here, not through trusty-mpm
//! (`crates/trusty-mpm/src/core/architect_launch.rs`,
//! `check_launched_architect`): same fields, same `deny_unknown_fields`,
//! same refusal of a file other users can write.
//! Known limit: a Claude Code subagent runs inside its parent's process, so
//! an Architect's own in-process subagent shares its `claude` and its
//! exemption. A PM or nested session is its own `claude` and has no record.
//! Test: `architect_tests.rs` beside `agent_default_tests.rs`.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use super::ancestry::{ProcessTable, nearest_agent};

/// The record directory under the account's home directory.
pub(crate) const RECORD_DIR: &str = ".trusty-mpm/architect-launch";

/// A record's file extension, tm's `ARCHITECT_EXT`.
const RECORD_EXT: &str = "architect";

/// A record is a few hundred bytes; anything larger is not one.
const MAX_RECORD_BYTES: u64 = 4096;

/// tm's `ArmingRecord`, field for field.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchRecord {
    pid: u32,
    /// `sysinfo` start time, whole Unix seconds.
    start_time: u64,
    project_dir: PathBuf,
    // Informational in tm too; required so the shape stays tm's.
    #[serde(rename = "armed_at")]
    _armed_at: String,
}

/// Why a caller is not the launched Architect. Each one keeps agent rules.
///
/// What: the arms carry no PID, path or file content.
/// Test: `architect_refusals_each_keep_the_agent_rules`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum ArchitectRefusal {
    /// The server knows no account home, so no record directory.
    #[error("no Architect launch-record directory is known")]
    NoRecordDir,
    /// The process chain, start time or working directory did not read.
    #[error("the process table could not be read")]
    ProcessLookup,
    /// No Claude Code process is at or above the caller.
    #[error("no Claude Code process is above the caller")]
    NoClaudeAncestor,
    /// The nearest Claude Code process has no record.
    #[error("the nearest Claude Code process has no Architect launch record")]
    NoLaunchRecord,
    /// The record did not open, was not the owner's own regular file, was
    /// writable by others, too large, or did not parse.
    #[error("the Architect launch record could not be read or parsed")]
    UnreadableRecord,
    /// The record's PID differs from its file name's.
    #[error("the Architect launch record names another PID")]
    RecordPidMismatch,
    /// The live process started at another time: the PID was reused.
    #[error("the Architect launch record's start time differs (PID reuse)")]
    StartTimeMismatch,
    /// The record names another directory than the live process's.
    #[error("the Architect launch record names another project directory")]
    OtherProject,
}

/// The record directory for `account_home`, `None` without one.
pub(crate) fn records_dir(account_home: Option<&Path>) -> Option<PathBuf> {
    account_home.map(|home| home.join(RECORD_DIR))
}

/// Whether `peer` runs under the `claude` tm launched as the Architect.
///
/// Why: #10459 P1 — the exemption must bind one live process, never a name.
/// What: `Ok` only when `dir` is known, [`nearest_agent`] finds a Claude Code
/// process at or above `peer`, `<dir>/<pid>.architect` reads, and its PID,
/// start time ([`ProcessTable::start_unix_secs`]) and project directory
/// (canonically equal to [`ProcessTable::cwd`]) match. Otherwise the first
/// failure, in that order.
/// Test: `architect_with_a_valid_record_sets_a_denied_key`,
/// `architect_refusals_each_keep_the_agent_rules`,
/// `nested_claude_under_the_architect_is_not_the_architect`.
pub(crate) fn check_architect(
    table: &dyn ProcessTable,
    dir: Option<&Path>,
    peer: u32,
) -> Result<(), ArchitectRefusal> {
    let dir = dir.ok_or(ArchitectRefusal::NoRecordDir)?;
    let claude = nearest_agent(table, peer)
        .map_err(|_| ArchitectRefusal::ProcessLookup)?
        .ok_or(ArchitectRefusal::NoClaudeAncestor)?;
    let record = read_record(dir, claude)?.ok_or(ArchitectRefusal::NoLaunchRecord)?;
    if record.pid != claude {
        return Err(ArchitectRefusal::RecordPidMismatch);
    }
    let started = table
        .start_unix_secs(claude)
        .map_err(|_| ArchitectRefusal::ProcessLookup)?;
    if record.start_time != started {
        return Err(ArchitectRefusal::StartTimeMismatch);
    }
    let cwd = table
        .cwd(claude)
        .map_err(|_| ArchitectRefusal::ProcessLookup)?;
    if !same_dir(&record.project_dir, &cwd) {
        return Err(ArchitectRefusal::OtherProject);
    }
    Ok(())
}

/// Read `<dir>/<pid>.architect`; absent is `Ok(None)`.
///
/// What: opened without following a symlink; it must be a regular file of
/// this process's effective uid, not group- or other-writable, at most
/// [`MAX_RECORD_BYTES`], and parse as [`LaunchRecord`]. Anything else is
/// [`ArchitectRefusal::UnreadableRecord`].
fn read_record(dir: &Path, pid: u32) -> Result<Option<LaunchRecord>, ArchitectRefusal> {
    let unreadable = ArchitectRefusal::UnreadableRecord;
    let path = dir.join(format!("{pid}.{RECORD_EXT}"));
    let file = match open_no_follow(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unreadable),
        Ok(file) => file,
    };
    let meta = file.metadata().map_err(|_| unreadable)?;
    if !meta.is_file() || meta.len() > MAX_RECORD_BYTES || !owner_only_writable(&meta) {
        return Err(unreadable);
    }
    let mut raw = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| unreadable)?;
    serde_json::from_slice(&raw)
        .map(Some)
        .map_err(|_| unreadable)
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(_path: &Path) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

/// This uid owns the file and no group or other user can write it.
#[cfg(unix)]
fn owner_only_writable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    // SAFETY: `geteuid` takes no arguments, touches no caller memory, and
    // cannot fail (POSIX).
    let euid = unsafe { libc::geteuid() };
    meta.uid() == euid && meta.mode() & 0o022 == 0
}

#[cfg(not(unix))]
fn owner_only_writable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Whether `a` and `b` canonicalize to one directory; `false` if either fails.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
