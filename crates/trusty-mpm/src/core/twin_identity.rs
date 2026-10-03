//! The supervisor-twin identity: whether a `tm hook --pm-guard` evaluation
//! runs in twin mode (#8878, ruling D1).
//!
//! Why: the owner ruling makes a supervisor session the user's "virtual twin",
//! allowed to do what the user directly asks. That relaxation (later PRs) must
//! reach only the one session the operator armed, never a session that merely
//! claims it. So twin mode builds on the #8453 supervisor profile and adds
//! three more conditions, meant to sit outside the session's own control.
//! Residual: today the session can still write `~/.trusty-mpm/config.toml`
//! and `~/.trusty-mpm/twin/armed/<pid>.json` itself. The D4 trust-anchor-write
//! floor (PR 2 of #8878) closes that; a write through an executed script file
//! stays open under D9, tracked in #8879.
//! What: [`resolve`] answers [`TwinStatus::Active`] only when ALL hold:
//! (a) the user-level `~/.trusty-mpm/config.toml` grants twin mode to the
//! project (`[supervisor.twin] projects`) and the #8453 supervisor profile
//! resolves; (b) `tm launch --twin` wrote an arming record; (c) that record is
//! bound to the nearest `claude` ancestor by PID AND start time; (d) the call
//! comes from the main thread, not a subagent. It reads no
//! `TRUSTY_MPM_PM_UNRESTRICTED` or `TRUSTY_MPM_DISABLE_HOOKS`.
//!
//! FAIL-CLOSED: every error — an unreadable config, a missing or unreadable
//! arming record, a process-table read error, a PID or start-time mismatch, an
//! unknown thread — is a [`TwinRefusal`], and a refusal is never twin mode.
//! This module decides identity only; no allow/deny decision reads it yet.
//!
//! Test: `twin_identity_tests.rs`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::config::MpmConfig;
use crate::core::session_profile::{self, SUPERVISOR_PROFILE_ID};

/// `[supervisor.twin]` in the user-level `~/.trusty-mpm/config.toml` (#8878).
///
/// Why: the operator's twin grant, kept outside every project's write
/// boundary, like the #8453 `[supervisor] projects` allowlist it narrows.
/// What: `projects` — absolute project paths granted twin mode, matched after
/// canonicalization. A relative entry matches nothing. Absent → no grant.
/// Test: `a_supervisor_without_the_twin_grant_is_not_twin`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TwinGrant {
    /// Absolute paths of the projects granted twin mode.
    pub projects: Vec<PathBuf>,
}

/// The `claude` process a hook call belongs to: its PID and start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeProcess {
    /// Process id.
    pub pid: u32,
    /// Start time, Unix seconds, as the process table reports it.
    pub start_time: u64,
}

/// What `tm launch --twin` records for the `claude` it started (#8878).
///
/// Why: the arming must name one process instance. A PID alone is reused by
/// the OS; the start time tells two holders of one PID apart.
/// What: the armed `claude`'s PID and start time, and the directory it was
/// launched in. Stored as `<root>/twin/armed/<pid>.json`
/// (`core::twin_arming`). Unknown fields are refused.
/// Test: `an_arming_record_round_trips`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmingRecord {
    /// PID of the armed `claude`.
    pub pid: u32,
    /// Start time of the armed `claude`, Unix seconds.
    pub start_time: u64,
    /// Directory the armed `claude` was launched in.
    pub project_dir: PathBuf,
    /// When `tm launch --twin` armed it, RFC 3339. Informational.
    pub armed_at: String,
}

/// Which thread of a Claude Code session made a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadKind {
    /// The session's main thread.
    Main,
    /// A subagent: a non-empty payload `agent_id`, or `CLAUDE_MPM_SUB_AGENT`.
    Subagent,
    /// The payload cannot establish which thread made the call.
    Unknown,
}

/// Why an evaluation is not in twin mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TwinRefusal {
    /// The call came from a subagent (condition d).
    Subagent,
    /// The payload does not establish the main thread (condition d).
    UnknownThread,
    /// The #8453 supervisor profile does not resolve.
    NotSupervisor,
    /// The user-level config could not be read or parsed (condition a).
    ConfigUnreadable(String),
    /// The project has no `[supervisor.twin]` grant (condition a).
    NoGrant,
    /// The process table could not be read (condition c).
    ProcessTable(String),
    /// No `claude` process is an ancestor of the hook (condition c).
    NoClaudeAncestor,
    /// No arming record exists for the `claude` ancestor (condition b).
    NotArmed,
    /// The arming record could not be read or parsed (condition b).
    ArmingUnreadable(String),
    /// The record's PID or start time differs from the live `claude` (c).
    BindingMismatch,
    /// The record was armed for another project directory (condition b).
    ProjectMismatch,
}

impl std::fmt::Display for TwinRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Subagent => f.write_str("the call came from a subagent"),
            Self::UnknownThread => f.write_str("the payload does not establish the main thread"),
            Self::NotSupervisor => f.write_str("the session is not a supervisor session"),
            Self::ConfigUnreadable(e) => write!(f, "the user config could not be read: {e}"),
            Self::NoGrant => f.write_str("the project has no `[supervisor.twin]` grant"),
            Self::ProcessTable(e) => write!(f, "the process table could not be read: {e}"),
            Self::NoClaudeAncestor => f.write_str("no `claude` process is an ancestor"),
            Self::NotArmed => f.write_str("the session was not armed with `tm launch --twin`"),
            Self::ArmingUnreadable(e) => write!(f, "the arming record could not be read: {e}"),
            Self::BindingMismatch => {
                f.write_str("the arming record names another process (PID or start time differs)")
            }
            Self::ProjectMismatch => f.write_str("the arming record names another project"),
        }
    }
}

/// The twin identity of one evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TwinStatus {
    /// Twin mode: every D1 condition holds for this armed `claude`.
    Active(ClaudeProcess),
    /// Not twin mode, and why.
    Inactive(TwinRefusal),
}

impl TwinStatus {
    /// Whether twin mode is active.
    pub fn is_active(&self) -> bool {
        matches!(self, TwinStatus::Active(_))
    }
}

/// The OS facts [`resolve`] reads, injectable so every error arm is testable.
pub trait TwinProbe {
    /// The user-level config, parsed strictly: an unreadable or malformed file
    /// is `Err`, never a default.
    fn user_config(&self) -> Result<MpmConfig, String>;
    /// The nearest `claude` ancestor of the evaluating process, if any.
    fn nearest_claude(&self) -> Result<Option<ClaudeProcess>, String>;
    /// The arming record for `pid`; `Ok(None)` when none exists.
    fn arming_record(&self, pid: u32) -> Result<Option<ArmingRecord>, String>;
}

/// The per-call inputs of [`resolve`]: the payload and the hook's environment.
#[derive(Debug, Clone, Copy)]
pub struct HookContext<'a> {
    /// The `PreToolUse` stdin payload.
    pub payload: &'a Value,
    /// `TRUSTY_MPM_SESSION_PROFILE`, the #8453 launch stamp.
    pub profile_stamp: Option<&'a OsStr>,
    /// `CLAUDE_PROJECT_DIR`, the session's launch directory.
    pub project_dir: Option<&'a OsStr>,
    /// Whether `CLAUDE_MPM_SUB_AGENT` is set.
    pub sub_agent_env: bool,
}

/// Which thread made the call `payload` describes.
///
/// Why: condition (d). The fan-out guard's subagent test fails open by design
/// (#4784); twin mode must fail closed, so it has its own classifier.
/// What: [`ThreadKind::Subagent`] for `sub_agent_env` or a non-empty string
/// `agent_id`. [`ThreadKind::Main`] only for an object payload with no
/// `agent_id` key and a non-empty string `session_id`. Everything else — a
/// non-object payload, an empty or non-string `agent_id`, a missing
/// `session_id` — is [`ThreadKind::Unknown`].
/// Test: `a_subagent_call_is_not_twin`, `an_unknown_thread_is_not_twin`.
pub fn thread_kind(payload: &Value, sub_agent_env: bool) -> ThreadKind {
    let Some(obj) = payload.as_object() else {
        return ThreadKind::Unknown;
    };
    if sub_agent_env {
        return ThreadKind::Subagent;
    }
    match obj.get("agent_id") {
        None => {}
        Some(Value::String(id)) if !id.is_empty() => return ThreadKind::Subagent,
        Some(_) => return ThreadKind::Unknown,
    }
    match obj.get("session_id") {
        Some(Value::String(id)) if !id.is_empty() => ThreadKind::Main,
        _ => ThreadKind::Unknown,
    }
}

/// Whether `config` grants `project_dir` twin mode and its supervisor profile
/// resolves (condition a).
///
/// Why: shared by the hook ([`resolve`]) and `tm launch --twin`, so the launch
/// refuses exactly what the hook would refuse.
/// What: `Err(NotSupervisor)` unless [`session_profile::resolve`] says
/// supervisor; `Err(NoGrant)` unless `[supervisor.twin] projects` lists the
/// project ([`session_profile::path_is_listed`]).
/// Test: `a_supervisor_without_the_twin_grant_is_not_twin`,
/// `a_twin_grant_without_the_supervisor_allowlist_is_not_twin`.
pub fn check_grant(project_dir: &Path, config: &MpmConfig) -> Result<(), TwinRefusal> {
    if !session_profile::resolve(project_dir, config).is_supervisor() {
        return Err(TwinRefusal::NotSupervisor);
    }
    if !session_profile::path_is_listed(project_dir, &config.supervisor.twin.projects) {
        return Err(TwinRefusal::NoGrant);
    }
    Ok(())
}

/// The twin identity of one `tm hook --pm-guard` evaluation (#8878 D1).
///
/// Why: see the module doc. Checks run cheapest first, so a PM session pays
/// for two environment reads and nothing else.
/// What: [`TwinStatus::Active`] only when, in order: [`thread_kind`] is
/// [`ThreadKind::Main`]; the launch stamp is the supervisor's; the strict user
/// config reads; [`check_grant`] passes for `CLAUDE_PROJECT_DIR`; the process
/// table names a `claude` ancestor; an arming record exists for its PID; the
/// record's PID and start time equal the live process's; and the record's
/// project canonicalizes to the same directory. The first failure is the
/// [`TwinStatus::Inactive`] reason.
/// Test: every test in `twin_identity_tests.rs`; `all_conditions_arm_twin_mode`
/// is the positive case.
pub fn resolve(ctx: &HookContext<'_>, probe: &impl TwinProbe) -> TwinStatus {
    match armed_claude(ctx, probe) {
        Ok(claude) => TwinStatus::Active(claude),
        Err(refusal) => TwinStatus::Inactive(refusal),
    }
}

/// [`resolve`]'s checks, as a `Result` so each arm is one `?`.
fn armed_claude(
    ctx: &HookContext<'_>,
    probe: &impl TwinProbe,
) -> Result<ClaudeProcess, TwinRefusal> {
    match thread_kind(ctx.payload, ctx.sub_agent_env) {
        ThreadKind::Main => {}
        ThreadKind::Subagent => return Err(TwinRefusal::Subagent),
        ThreadKind::Unknown => return Err(TwinRefusal::UnknownThread),
    }
    if ctx.profile_stamp != Some(OsStr::new(SUPERVISOR_PROFILE_ID)) {
        return Err(TwinRefusal::NotSupervisor);
    }
    let project = session_profile::hook_project_dir(ctx.project_dir.map(OsString::from))
        .ok_or(TwinRefusal::NotSupervisor)?;
    let config = probe.user_config().map_err(TwinRefusal::ConfigUnreadable)?;
    check_grant(&project, &config)?;
    let claude = probe
        .nearest_claude()
        .map_err(TwinRefusal::ProcessTable)?
        .ok_or(TwinRefusal::NoClaudeAncestor)?;
    let record = probe
        .arming_record(claude.pid)
        .map_err(TwinRefusal::ArmingUnreadable)?
        .ok_or(TwinRefusal::NotArmed)?;
    if record.pid != claude.pid || record.start_time != claude.start_time {
        return Err(TwinRefusal::BindingMismatch);
    }
    if !same_dir(&record.project_dir, &project) {
        return Err(TwinRefusal::ProjectMismatch);
    }
    Ok(claude)
}

/// Whether `a` and `b` canonicalize to one directory; `false` if either fails.
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
#[path = "twin_identity_tests.rs"]
mod tests;
