//! The OS side of the supervisor-twin identity (#8878, ruling D1).
//!
//! Why: [`crate::core::twin_identity::resolve`] decides from facts; this
//! module reads them from the machine — the user config, the process table
//! and the arming record `tm launch --twin` writes — and writes that record.
//! What: [`OsProbe`] implements [`TwinProbe`] against `~/.trusty-mpm/` and the
//! live process table; [`resolve_hook`] is the hook's entry point;
//! [`arm_claude`] records one launched `claude` as armed. Every read error is
//! returned as an error, which the resolver turns into "not twin".
//! Test: `twin_identity_tests.rs` (`an_arming_record_round_trips`,
//! `a_writable_by_others_arming_record_is_refused`,
//! `the_nearest_claude_ancestor_is_found_with_its_start_time`,
//! `the_walk_stops_at_the_nearest_claude`,
//! `a_session_nested_under_the_twin_is_not_twin`,
//! `a_reaped_process_has_no_claude_ancestor_it_is_an_error`,
//! `a_malformed_user_config_is_an_error_not_a_default`) and
//! `is_claude_of_a_dead_pid_is_an_error` here.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::config::MpmConfig;
use crate::core::session_profile::{PROJECT_DIR_ENV, SESSION_PROFILE_ENV};
use crate::core::twin_identity::{
    ArmingRecord, ClaudeProcess, HookContext, TwinProbe, TwinStatus, resolve,
};

/// Directory, under the `~/.trusty-mpm` root, holding one record per armed
/// `claude`, named `<pid>.json`.
pub const ARMED_DIR: &str = "twin/armed";

/// Ancestor hops walked before the search gives up: the hook's parent only.
///
/// Why (#8878 review): Claude Code spawns a hook as `spawn(command, [],
/// {shell})`, i.e. `sh -c "<command>"`, and the shell execs a simple command in
/// place; the `tm hook` processes observed live had their `claude` as the
/// direct parent. Any hop past the parent can cross a session boundary — a
/// nested session started from the twin's Bash tool, including one whose
/// process is not named `claude` (npm installs run as `node`), sits between
/// its hook and the twin. A compound hook command (a surviving `sh`) or
/// `CLAUDE_CODE_SHELL_PREFIX` puts the `claude` further up and fails closed.
const MAX_ANCESTOR_HOPS: usize = 1;

/// The machine-backed [`TwinProbe`].
#[derive(Debug, Clone)]
pub struct OsProbe {
    /// The `~/.trusty-mpm` root; `None` when the home directory is unknown.
    pub root: Option<PathBuf>,
    /// The process whose `claude` ancestor is sought (the hook itself).
    pub start_pid: u32,
}

impl OsProbe {
    /// The probe for this process and the operator's `~/.trusty-mpm`.
    pub fn current() -> Self {
        Self {
            root: dirs::home_dir().map(|home| home.join(".trusty-mpm")),
            start_pid: std::process::id(),
        }
    }

    fn root(&self) -> Result<&Path, String> {
        self.root
            .as_deref()
            .ok_or_else(|| "the home directory is unknown".to_string())
    }
}

impl TwinProbe for OsProbe {
    fn user_config(&self) -> Result<MpmConfig, String> {
        load_user_config_strict(self.root()?)
    }

    fn nearest_claude(&self) -> Result<Option<ClaudeProcess>, String> {
        nearest_claude_ancestor(self.start_pid)
    }

    fn arming_record(&self, pid: u32) -> Result<Option<ArmingRecord>, String> {
        read_record(self.root()?, pid)
    }
}

/// The twin identity of the running `tm hook --pm-guard` call (#8878).
///
/// Why: the hook's single entry point; later PRs gate relaxations on it.
/// What: [`resolve`] over `payload`, the #8453 launch stamp,
/// `CLAUDE_PROJECT_DIR`, `CLAUDE_MPM_SUB_AGENT` (read through `env`) and
/// [`OsProbe::current`]. `env` is injected so a test never mutates the process
/// environment; production passes `std::env::var_os`.
/// Test: `unrestricted_and_disable_hooks_do_not_imply_twin_mode`.
pub fn resolve_hook(
    payload: &Value,
    env: impl Fn(&str) -> Option<OsString>,
    probe: &impl TwinProbe,
) -> TwinStatus {
    let stamp = env(SESSION_PROFILE_ENV);
    let project_dir = env(PROJECT_DIR_ENV);
    let sub_agent = env(trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR);
    let ctx = HookContext {
        payload,
        profile_stamp: stamp.as_deref(),
        project_dir: project_dir.as_deref(),
        sub_agent_env: sub_agent.is_some(),
    };
    resolve(&ctx, probe)
}

/// `<root>/config.toml`, parsed strictly (#8878 condition a).
///
/// Why: [`MpmConfig::load`] turns an unreadable or malformed file into the
/// defaults, which is right for most settings. The twin grant needs the error
/// itself, so a damaged file is reported as a refusal, not read as "no grant".
/// What: an absent file → `Ok(MpmConfig::default())` (no grant, no
/// allowlist); any other read error or a TOML parse error → `Err`.
/// Test: `a_malformed_user_config_is_an_error_not_a_default`.
pub fn load_user_config_strict(root: &Path) -> Result<MpmConfig, String> {
    let path = root.join("config.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MpmConfig::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(raw) => toml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Path of the arming record for `pid` under `root`.
pub fn record_path(root: &Path, pid: u32) -> PathBuf {
    root.join(ARMED_DIR).join(format!("{pid}.json"))
}

/// Read the arming record for `pid` (#8878 condition b).
///
/// What: absent → `Ok(None)`. `Err` for any other read error, a file other
/// users can write (unix), or content that does not parse as an
/// [`ArmingRecord`].
/// Test: `an_arming_record_round_trips`,
/// `a_writable_by_others_arming_record_is_refused`.
pub fn read_record(root: &Path, pid: u32) -> Result<Option<ArmingRecord>, String> {
    let path = record_path(root, pid);
    let raw = match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
        Ok(raw) => raw,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o022 != 0 {
            return Err(format!(
                "{} is writable by other users (mode {mode:o})",
                path.display()
            ));
        }
    }
    serde_json::from_slice(&raw)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Write `record` atomically as `<root>/twin/armed/<pid>.json`, mode 0600.
///
/// Test: `an_arming_record_round_trips`.
pub fn write_record(root: &Path, record: &ArmingRecord) -> Result<PathBuf, String> {
    let path = record_path(root, record.pid);
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let body = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    // NamedTempFile is created 0600 on unix, and `persist` renames it in place.
    let mut staged =
        tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    staged
        .write_all(&body)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    staged
        .persist(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Arm the `claude` at `pid`, launched in `project_dir`, for twin mode.
///
/// Why: `tm launch --twin` binds the arming to the process it started, by PID
/// and start time, so a later process that reuses the PID is not armed.
/// What: reads `pid`'s start time from the process table, then
/// [`write_record`]s it with the canonical `project_dir`.
/// Test: `the_nearest_claude_ancestor_is_found_with_its_start_time` (the
/// start-time read), `an_arming_record_round_trips` (the write).
pub fn arm_claude(root: &Path, pid: u32, project_dir: &Path) -> Result<ArmingRecord, String> {
    let start_time = process_facts(pid)?.start_time;
    let project_dir = std::fs::canonicalize(project_dir)
        .map_err(|e| format!("{}: {e}", project_dir.display()))?;
    let record = ArmingRecord {
        pid,
        start_time,
        project_dir,
        armed_at: chrono::Utc::now().to_rfc3339(),
    };
    write_record(root, &record)?;
    Ok(record)
}

/// Parent and start time of one process-table entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessFacts {
    /// Parent PID; `None` when the table records no parent.
    pub parent: Option<u32>,
    /// Start time, Unix seconds.
    pub start_time: u64,
}

/// Read `pid`'s parent and start time; `Err` when the table has no entry.
fn process_facts(pid: u32) -> Result<ProcessFacts, String> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let spid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[spid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let proc_ = sys
        .process(spid)
        .ok_or_else(|| format!("the process table holds no entry for pid {pid}"))?;
    Ok(ProcessFacts {
        parent: proc_.parent().map(Pid::as_u32),
        start_time: proc_.start_time(),
    })
}

/// Whether `pid`'s command name contains `claude`, any case.
///
/// Why: the launch side finds the `claude` it arms by this same rule
/// (`core::process::process_name_is_claude`), so the hook must match the same
/// processes. A broader rule is also the fail-closed one: every extra match can
/// only stop the walk nearer the hook, at a process with no arming record.
/// What: `/proc/<pid>/comm` on Linux, else `ps -o comm=`. Unlike the launch
/// side, a failed read is `Err`, never `false`, so the walk cannot skip a
/// process it could not identify.
/// Test: `is_claude_of_a_dead_pid_is_an_error`.
fn is_claude(pid: u32) -> Result<bool, String> {
    #[cfg(target_os = "linux")]
    if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        return Ok(comm.to_ascii_lowercase().contains("claude"));
    }
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .map_err(|e| format!("ps: {e}"))?;
    if !out.status.success() {
        return Err(format!("ps found no process {pid}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .to_ascii_lowercase()
        .contains("claude"))
}

/// The nearest `claude` ancestor of `start_pid`, excluding `start_pid` itself.
///
/// Why: condition (c). Claude Code runs a hook as its own child, so the
/// `claude` directly above the hook is the session the call belongs to.
/// What: [`nearest_claude_in`] over the live process table.
/// Test: `the_nearest_claude_ancestor_is_found_with_its_start_time`,
/// `a_reaped_process_has_no_claude_ancestor_it_is_an_error`.
pub fn nearest_claude_ancestor(start_pid: u32) -> Result<Option<ClaudeProcess>, String> {
    nearest_claude_in(start_pid, process_facts, is_claude)
}

/// The nearest ancestor of `start_pid` that `is_claude` accepts.
///
/// Why: the walk's fail-closed rules are testable only over an injected table.
/// What: walks parent links from `start_pid` for at most
/// [`MAX_ANCESTOR_HOPS`] and returns the FIRST accepted ancestor, with its
/// start time — a nested `claude` below an armed one is the caller, not the
/// armed one. Reaching PID 1, a process with no parent, or the hop limit →
/// `Ok(None)`. Any `facts` or `is_claude` error → `Err`: an ancestor that
/// cannot be identified is never skipped.
/// Test: `the_walk_stops_at_the_nearest_claude`,
/// `a_session_nested_under_the_twin_is_not_twin`,
/// `an_unidentifiable_ancestor_stops_the_walk`.
pub fn nearest_claude_in(
    start_pid: u32,
    facts: impl Fn(u32) -> Result<ProcessFacts, String>,
    is_claude: impl Fn(u32) -> Result<bool, String>,
) -> Result<Option<ClaudeProcess>, String> {
    let mut pid = match facts(start_pid)?.parent {
        Some(parent) => parent,
        None => return Ok(None),
    };
    for _ in 0..MAX_ANCESTOR_HOPS {
        if pid <= 1 {
            return Ok(None);
        }
        let entry = facts(pid)?;
        if is_claude(pid)? {
            return Ok(Some(ClaudeProcess {
                pid,
                start_time: entry.start_time,
            }));
        }
        match entry.parent {
            Some(parent) => pid = parent,
            None => return Ok(None),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    /// A name read that cannot find the process is an error, never "not
    /// claude", so the walk cannot step past an unidentified ancestor.
    #[cfg(unix)]
    #[test]
    fn is_claude_of_a_dead_pid_is_an_error() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        child.wait().expect("reap true");
        let got = super::is_claude(pid);
        assert!(got.is_err(), "{got:?}");
    }
}
