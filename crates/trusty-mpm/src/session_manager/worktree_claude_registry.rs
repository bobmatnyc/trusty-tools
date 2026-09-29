//! Claude Code's own per-process session registry, read as proof that a Claude
//! session has ended (#7771).
//!
//! Why: an agent tree's owner file names the Claude session that dispatched
//! it. When no managed record names that session (it ran outside `tm`, or its
//! record was compacted), or its record still reads live because the PM was
//! relaunched in the same tmux window under a new session id, the session
//! store cannot show that it ended. Merged, clean trees were then kept
//! forever: 4 on one host on 2026-09-28, and `tm pr cleanup` refused every
//! tree whose owner was not the caller. Claude Code writes one
//! `<config dir>/sessions/<pid>.json` per running process. The file names the
//! session that process runs and the process's start time. It is the one
//! record that binds a live process to a Claude session id.
//! What: [`ClaudeRegistry::read`] snapshots every registry entry under the
//! consulted config dirs and judges each entry's process against the process
//! table. [`ClaudeRegistry::session_end`] answers `Ended` only when a
//! consulted dir holds the session's transcript, so its registry is the one
//! the session would appear in, and no entry naming the session belongs to a
//! running process. [`ClaudeRegistry::replaced_in`] answers whether a tmux
//! session now runs a different Claude session.
//!
//! # Fail direction
//!
//! Every answer that is not positive evidence is `Undeterminable` (ADR-0045):
//! a registry or projects dir that cannot be read, a live entry that does not
//! parse, a running pid whose start time was not recorded or cannot be read,
//! a session whose transcript no consulted dir holds, and a dir that holds the
//! transcript but no registry. [`ClaudeRegistry::NotRead`] answers nothing.
//! Test: `worktree_claude_registry_tests`.

use std::path::{Path, PathBuf};

use super::manager::SessionManager;
use super::worktree_owner_gate::{
    START_TOLERANCE_SECS, SessionEnd, parse_start_stamp, process_start_secs,
};
use super::worktree_registry::pid_liveness;

/// Whether the process a registry entry names still runs (#7771).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EntryLiveness {
    /// The pid runs with the start time the entry recorded.
    Live,
    /// The pid is gone, or a later process reused it.
    Gone,
    /// The process table could not settle it — carrying why.
    Unknown(String),
}

/// One `<config dir>/sessions/<pid>.json` entry (#7771).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegistryEntry {
    /// The Claude session id the process runs.
    pub(crate) session_id: String,
    /// The tmux session the process runs in, when it runs in one.
    pub(crate) tmux_session: Option<String>,
    /// The entry's process, judged when the registry was read.
    pub(crate) liveness: EntryLiveness,
}

/// One consulted Claude config dir, as read (#7771).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigRoot {
    /// The config dir (`CLAUDE_CONFIG_DIR`).
    root: PathBuf,
    /// `Ok(None)` when the dir keeps no registry; `Err` when the registry or a
    /// live entry in it could not be read.
    entries: Result<Option<Vec<RegistryEntry>>, String>,
}

/// Claude Code's per-process session registry, as the reclaim read it (#7771).
///
/// Test: `claude_registry_not_read_proves_nothing`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum ClaudeRegistry {
    /// Not consulted: no evidence either way. Every non-host caller.
    #[default]
    NotRead,
    /// The consulted config dirs.
    Read(Vec<ConfigRoot>),
}

impl ClaudeRegistry {
    /// The registry under `roots`, each entry judged against the real process
    /// table. No roots is [`Self::NotRead`].
    pub(crate) fn read(roots: &[PathBuf]) -> Self {
        Self::read_with(roots, &pid_liveness, &process_start_secs)
    }

    /// [`Self::read`] with the process table injected.
    ///
    /// What: each root's `sessions/*.json`. A missing `sessions` dir is no
    /// registry; one that cannot be listed is an error. An entry that does not
    /// parse is skipped only when the pid its file name carries is gone, since
    /// a dead process runs nothing; otherwise the root is an error.
    /// Test: `claude_registry_ignores_a_corrupt_entry_whose_pid_is_gone`,
    /// `claude_registry_probe_failures_are_undeterminable`.
    pub(crate) fn read_with(
        roots: &[PathBuf],
        pid_alive: &dyn Fn(u32) -> Option<bool>,
        start_of: &dyn Fn(u32) -> Result<i64, String>,
    ) -> Self {
        if roots.is_empty() {
            return Self::NotRead;
        }
        Self::Read(
            roots
                .iter()
                .map(|root| ConfigRoot {
                    root: root.clone(),
                    entries: read_entries(&root.join("sessions"), pid_alive, start_of),
                })
                .collect(),
        )
    }

    /// Whether Claude session `id` has ended, or `None` when not read.
    ///
    /// What: `Live` when any entry naming `id` runs; `Undeterminable` when one
    /// cannot be judged, a root could not be read, or no root holds the
    /// session's transcript (`projects/*/<id>.jsonl`), or a root holds it but
    /// keeps no registry. `Ended` otherwise.
    /// Test: `claude_registry_ends_a_session_no_live_process_runs`,
    /// `claude_registry_keeps_a_session_a_live_process_runs`,
    /// `claude_registry_probe_failures_are_undeterminable`.
    pub(crate) fn session_end(&self, id: &str) -> Option<SessionEnd> {
        let Self::Read(roots) = self else {
            return None;
        };
        Some(judge_session(roots, id))
    }

    /// Whether the record whose tmux session is `tmux` was relaunched onto a
    /// different Claude session, leaving `id` behind (#7771).
    ///
    /// What: `id` is [`SessionEnd::Ended`] by [`Self::session_end`], and a
    /// running process in `tmux` runs another session id.
    /// Test: `worktree_7771_a_session_replaced_in_its_tmux_window_is_reclaimed`.
    pub(crate) fn replaced_in(&self, id: &str, tmux: &str) -> bool {
        let Self::Read(roots) = self else {
            return false;
        };
        judge_session(roots, id) == SessionEnd::Ended
            && entries(roots).any(|e| {
                e.tmux_session.as_deref() == Some(tmux)
                    && e.liveness == EntryLiveness::Live
                    && e.session_id != id
            })
    }
}

/// Every entry of every root that could be read.
fn entries(roots: &[ConfigRoot]) -> impl Iterator<Item = &RegistryEntry> {
    roots
        .iter()
        .filter_map(|r| r.entries.as_ref().ok().and_then(Option::as_ref))
        .flatten()
}

/// See [`ClaudeRegistry::session_end`].
fn judge_session(roots: &[ConfigRoot], id: &str) -> SessionEnd {
    // The id becomes a file name below, so nothing but a UUID shape passes.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return SessionEnd::Undeterminable(format!(
            "`{id}` is not shaped like a Claude session id"
        ));
    }
    let mut unknown = None;
    for e in entries(roots).filter(|e| e.session_id == id) {
        match &e.liveness {
            EntryLiveness::Live => return SessionEnd::Live,
            EntryLiveness::Unknown(why) => unknown = Some(why.clone()),
            EntryLiveness::Gone => {}
        }
    }
    if let Some(why) = unknown {
        return SessionEnd::Undeterminable(why);
    }
    let mut known = false;
    for r in roots {
        if let Err(why) = &r.entries {
            return SessionEnd::Undeterminable(why.clone());
        }
        match transcript_known(&r.root, id) {
            Err(why) => return SessionEnd::Undeterminable(why),
            Ok(false) => {}
            Ok(true) if matches!(r.entries, Ok(None)) => {
                return SessionEnd::Undeterminable(format!(
                    "{} holds its transcript but no Claude session registry, so a process \
                     running it cannot be ruled out",
                    r.root.display()
                ));
            }
            Ok(true) => known = true,
        }
    }
    if known {
        return SessionEnd::Ended;
    }
    let dirs: Vec<String> = roots.iter().map(|r| r.root.display().to_string()).collect();
    SessionEnd::Undeterminable(format!(
        "no consulted Claude config dir ({}) holds its transcript, so no registry read here \
         would list the process running it",
        dirs.join(", ")
    ))
}

/// Whether `<root>/projects/*/<id>.jsonl` exists.
fn transcript_known(root: &Path, id: &str) -> Result<bool, String> {
    let projects = root.join("projects");
    let unreadable = |e: std::io::Error| {
        format!(
            "the Claude transcripts under {} cannot be read: {e}",
            projects.display()
        )
    };
    let listing = match std::fs::read_dir(&projects) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(unreadable(e)),
    };
    for item in listing {
        let dir = item.map_err(unreadable)?.path();
        if !dir.is_dir() {
            continue;
        }
        match std::fs::symlink_metadata(dir.join(format!("{id}.jsonl"))) {
            Ok(_) => return Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(unreadable(e)),
        }
    }
    Ok(false)
}

/// The entries of one `sessions` dir; see [`ClaudeRegistry::read_with`].
fn read_entries(
    dir: &Path,
    pid_alive: &dyn Fn(u32) -> Option<bool>,
    start_of: &dyn Fn(u32) -> Result<i64, String>,
) -> Result<Option<Vec<RegistryEntry>>, String> {
    let unreadable = |e: std::io::Error| {
        format!(
            "the Claude session registry {} cannot be read: {e}",
            dir.display()
        )
    };
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(e)),
    };
    let mut out = Vec::new();
    for item in listing {
        let path = item.map_err(unreadable)?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        match parse_entry(&path) {
            Ok((pid, start, mut entry)) => {
                entry.liveness = judge_entry(pid, start, pid_alive, start_of);
                out.push(entry);
            }
            Err(why) => {
                let stem_pid = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u32>().ok());
                if stem_pid.and_then(pid_alive) != Some(false) {
                    return Err(format!(
                        "the Claude session registry entry {} {why}, and its process may still run",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(Some(out))
}

/// One entry file: its pid, its recorded process start, and the entry.
fn parse_entry(path: &Path) -> Result<(u32, Option<i64>, RegistryEntry), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot be read ({e})"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("does not parse ({e})"))?;
    let pid = v["pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .ok_or("names no pid")?;
    let session_id = v["sessionId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("names no session")?
        .to_string();
    // `tm-x:@4.%4` — a tmux session name holds no `:`.
    let tmux_session = v["tmux"]
        .as_str()
        .and_then(|t| t.split(':').next())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let start = v["procStart"].as_str().and_then(parse_start_stamp);
    let entry = RegistryEntry {
        session_id,
        tmux_session,
        liveness: EntryLiveness::Gone,
    };
    Ok((pid, start, entry))
}

/// Judge one entry's process: gone, reused, running, or unknown.
///
/// Test: `claude_registry_reused_pid_is_not_live`,
/// `claude_registry_probe_failures_are_undeterminable`.
fn judge_entry(
    pid: u32,
    recorded: Option<i64>,
    pid_alive: &dyn Fn(u32) -> Option<bool>,
    start_of: &dyn Fn(u32) -> Result<i64, String>,
) -> EntryLiveness {
    match pid_alive(pid) {
        Some(false) => return EntryLiveness::Gone,
        None => {
            return EntryLiveness::Unknown(format!(
                "whether Claude process {pid} runs could not be read from the process table"
            ));
        }
        Some(true) => {}
    }
    let Some(recorded) = recorded else {
        return EntryLiveness::Unknown(format!(
            "Claude process {pid} runs and its registry entry records no readable start \
             time, so a reused pid cannot be told from the process that wrote it"
        ));
    };
    match start_of(pid) {
        Ok(actual) if (actual - recorded).abs() <= START_TOLERANCE_SECS => EntryLiveness::Live,
        Ok(_) => EntryLiveness::Gone,
        Err(e) => EntryLiveness::Unknown(format!(
            "Claude process {pid} runs and its start time could not be read: {e}"
        )),
    }
}

/// The Claude config dirs this host's sessions run under (#7771).
///
/// What: the managed `CLAUDE_CONFIG_DIR`, this process's `CLAUDE_CONFIG_DIR`,
/// and `~/.claude`, without duplicates. Callers gate on host-state access.
pub fn host_claude_config_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let env = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty());
    let candidates = crate::core::trusty_tools_config::managed_claude_config_dir()
        .into_iter()
        .chain(env.map(PathBuf::from))
        .chain(dirs::home_dir().map(|h| h.join(".claude")));
    for dir in candidates {
        let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !roots
            .iter()
            .any(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.clone()) == key)
        {
            roots.push(dir);
        }
    }
    roots
}

/// [`ClaudeRegistry::read`] over this host's roots, or `NotRead` when this
/// process may not touch host state (a scratch `$HOME`).
pub(crate) fn host_claude_registry() -> ClaudeRegistry {
    if crate::core::host_state_gate::host_state_access()
        .skip_reason()
        .is_some()
    {
        return ClaudeRegistry::NotRead;
    }
    ClaudeRegistry::read(&host_claude_config_roots())
}

impl SessionManager {
    /// Consult Claude Code's session registry under `roots` when judging
    /// whether an unrecorded or replaced owner session ended (#7771).
    ///
    /// Why: tests build managers over scratch stores, and must never read the
    /// operator's own Claude dirs, so only the host daemon and supervisor
    /// install roots. What: the first install wins; `false` on a repeat.
    pub fn install_claude_registry_roots(&self, roots: Vec<PathBuf>) -> bool {
        self.claude_registry_roots.set(roots).is_ok()
    }

    /// The installed registry, read now; `NotRead` when none was installed.
    pub(crate) fn claude_registry(&self) -> ClaudeRegistry {
        self.claude_registry_roots
            .get()
            .map_or(ClaudeRegistry::NotRead, |roots| ClaudeRegistry::read(roots))
    }
}

#[cfg(test)]
#[path = "worktree_claude_registry_tests.rs"]
mod worktree_claude_registry_tests;
