//! The Architect's tmux session name, validated and recorded (#8878 R1).
//!
//! Why: owner ruling 2026-09-29 22:23Z (R1) lets `tm fleet init --session`
//! keep a running supervisor's own tmux names (`tm-supervisor`) instead of
//! the fixed `tm-architect`. `tm fleet init` and `tm fleet status` read the
//! name back, so the launch records it beside the process record, in the
//! anchored `~/.trusty-mpm/architect-launch/` directory. The #8902 pane guard
//! does not read the name: it marks a pane by the live `<pid>.architect`
//! lineage or the fixed `tm-architect` session name.
//! What: [`validate_session_name`] is the one name rule. [`record_launch`]
//! writes `<pid>.architect-session` and then the `<pid>.architect` launch
//! record. [`architect_session_name`] reads the name back for a recorded PID, and
//! [`check_session_binding`] says whether the `claude` in a named session is
//! the launched Architect of a project. The `.architect` record keeps its
//! shape (`ArmingRecord`, `deny_unknown_fields`, shared with the twin), so the
//! name lives in a sidecar, not a new field.
//! Test: `architect_session_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::architect_launch::{
    ARCHITECT_DIR, ARCHITECT_RECORDS, LaunchRefusal, check_launched_architect,
};
use crate::core::twin_arming::process_facts;
use crate::core::twin_identity::{ArmingRecord, ClaudeProcess};

/// The Architect's tmux session when none is chosen; `tm fleet init`'s default.
pub const DEFAULT_ARCHITECT_SESSION: &str = "tm-architect";

/// Suffix of the poller's session: `<architect session>-poll`.
pub const POLL_SUFFIX: &str = "-poll";

/// Longest accepted Architect session name, in bytes (the poller adds 5).
pub const MAX_SESSION_NAME: usize = 64;

/// Extension of the session-name sidecar beside `<pid>.architect`.
///
/// #8878 R1 critic HIGH: distinct from a plain `.session`, so pm-guard can
/// deny any unplaceable write of this name without denying other files.
pub const SESSION_EXT: &str = "architect-session";

/// The sidecar's content: which launch it belongs to and the session name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRecord {
    pid: u32,
    start_time: u64,
    session: String,
}

/// Refuse a session name tmux or the fleet scripts could misread.
///
/// Why: tmux reads `:` and `.` in a target as window and pane separators, a
/// leading `=`, `$`, `@`, `%` or `-` as target or flag syntax, and the poller
/// scripts pass the name through the shell.
/// What: `Ok` only for 1 to [`MAX_SESSION_NAME`] ASCII letters, digits, `-`
/// and `_`, starting with a letter or digit.
/// Test: `session_names_are_validated`.
pub fn validate_session_name(name: &str) -> Result<(), String> {
    let first_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    if name.is_empty() {
        return Err("the session name is empty".to_owned());
    }
    if name.len() > MAX_SESSION_NAME {
        return Err(format!(
            "the session name {name:?} is longer than {MAX_SESSION_NAME} bytes"
        ));
    }
    let bad = name
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    if bad || !first_ok {
        return Err(format!(
            "the session name {name:?} must start with a letter or digit and hold only ASCII \
             letters, digits, `-` and `_` (no `:`, `.` or whitespace)"
        ));
    }
    Ok(())
}

/// The poller's session for Architect session `name`.
pub fn poll_session_name(name: &str) -> String {
    format!("{name}{POLL_SUFFIX}")
}

fn sidecar_path(root: &Path, pid: u32) -> PathBuf {
    root.join(ARCHITECT_DIR)
        .join(format!("{pid}.{SESSION_EXT}"))
}

/// Record the `claude` at `pid`, started in tmux session `session` in
/// `project_dir`, as the Architect.
///
/// Why: the launch path alone knows which session it created, so it writes
/// the name when it writes the process record.
/// What: validates `session`, writes the `<pid>.architect-session` sidecar
/// (mode 0600, atomic) with the process's start time, then the `<pid>.architect` record
/// ([`ARCHITECT_RECORDS`]). The sidecar goes first, so a record never exists
/// without its name from this path; a record from an older tm has none and
/// reads as [`DEFAULT_ARCHITECT_SESSION`].
/// Test: `a_recorded_session_name_reads_back`.
pub fn record_launch(
    root: &Path,
    pid: u32,
    project_dir: &Path,
    session: &str,
) -> Result<ArmingRecord, String> {
    validate_session_name(session)?;
    let start_time = process_facts(pid)?.start_time;
    let body = serde_json::to_vec_pretty(&SessionRecord {
        pid,
        start_time,
        session: session.to_owned(),
    })
    .map_err(|e| e.to_string())?;
    write_owner_only(&sidecar_path(root, pid), &body)?;
    ARCHITECT_RECORDS.record_claude(root, pid, project_dir)
}

/// Write `body` to `path` atomically, mode 0600, as `RecordStore::write` does.
fn write_owner_only(path: &Path, body: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // NamedTempFile is created 0600 on unix, and `persist` renames it in place.
    let mut staged =
        tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    staged
        .write_all(body)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    staged
        .persist(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

/// The tmux session name recorded for the Architect `claude` at `pid`.
///
/// Why: #8878 R1 — `tm fleet init` and `tm fleet status` must name the
/// Architect's session from the launch record, not assume `tm-architect`.
/// The #8902 pane guard does not call this; it matches the pane by launch
/// lineage or by the `tm-architect` name.
/// What: reads `<pid>.architect` under `root` (`~/.trusty-mpm`): none is
/// [`LaunchRefusal::NoLaunchRecord`]; unreadable is
/// [`LaunchRefusal::UnreadableRecord`]. Then `<pid>.architect-session`: absent
/// means an older tm launched it, so [`DEFAULT_ARCHITECT_SESSION`]; unreadable, writable
/// by others, unparseable, naming another PID or start time, or failing
/// [`validate_session_name`] is [`LaunchRefusal::UnreadableRecord`]. Never
/// falls back to the default on an error.
/// Test: `a_recorded_session_name_reads_back`,
/// `a_bad_session_sidecar_is_unreadable_never_the_default`.
pub fn architect_session_name(root: &Path, pid: u32) -> Result<String, LaunchRefusal> {
    let record = ARCHITECT_RECORDS
        .read(root, pid)
        .map_err(|_| LaunchRefusal::UnreadableRecord)?
        .ok_or(LaunchRefusal::NoLaunchRecord)?;
    let path = sidecar_path(root, pid);
    let raw = match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DEFAULT_ARCHITECT_SESSION.to_owned());
        }
        Err(_) => return Err(LaunchRefusal::UnreadableRecord),
        Ok(raw) => raw,
    };
    if writable_by_others(&path) {
        return Err(LaunchRefusal::UnreadableRecord);
    }
    let sidecar: SessionRecord =
        serde_json::from_slice(&raw).map_err(|_| LaunchRefusal::UnreadableRecord)?;
    if sidecar.pid != record.pid || sidecar.start_time != record.start_time {
        return Err(LaunchRefusal::UnreadableRecord);
    }
    validate_session_name(&sidecar.session).map_err(|_| LaunchRefusal::UnreadableRecord)?;
    Ok(sidecar.session)
}

/// Whether `path` is writable by group or others; an unreadable mode counts.
#[cfg(unix)]
fn writable_by_others(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).map_or(true, |m| m.permissions().mode() & 0o022 != 0)
}

#[cfg(not(unix))]
fn writable_by_others(_path: &Path) -> bool {
    false
}

/// Why the `claude` in a tmux session is not the project's bound Architect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingRefusal {
    /// The launch record check failed; see [`LaunchRefusal`].
    Launch(LaunchRefusal),
    /// The launch record names another tmux session.
    OtherSession {
        /// The session the record names.
        recorded: String,
    },
}

impl std::fmt::Display for BindingRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Launch(why) => write!(f, "{why}"),
            Self::OtherSession { recorded } => write!(
                f,
                "the Architect launch record names tmux session {recorded}, not this one"
            ),
        }
    }
}

/// Whether the `claude` at `pid`, found in tmux session `session`, is the
/// Architect tm launched for `project_dir`.
///
/// Why: #8878 R1 Fail-Open Check — a missing record, an unreadable name and a
/// name that differs from the session must all read as not bound.
/// What: `pid`'s start time from the process table (an error is
/// [`LaunchRefusal::ProcessLookup`]), then [`check_launched_architect`], then
/// [`architect_session_name`] equal to `session`.
/// Test: `a_session_binding_needs_the_record_and_the_same_name`.
pub fn check_session_binding(
    root: &Path,
    project_dir: &Path,
    session: &str,
    pid: u32,
) -> Result<(), BindingRefusal> {
    let start_time = process_facts(pid)
        .map_err(|_| BindingRefusal::Launch(LaunchRefusal::ProcessLookup))?
        .start_time;
    let claude = ClaudeProcess { pid, start_time };
    check_launched_architect(root, project_dir, || Ok(Some(claude)))
        .map_err(BindingRefusal::Launch)?;
    let recorded = architect_session_name(root, pid).map_err(BindingRefusal::Launch)?;
    if recorded != session {
        return Err(BindingRefusal::OtherSession { recorded });
    }
    Ok(())
}

#[cfg(test)]
#[path = "architect_session_tests.rs"]
mod tests;
