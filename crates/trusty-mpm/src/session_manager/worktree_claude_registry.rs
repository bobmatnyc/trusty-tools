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
//! consulted config dirs, judges each entry's process against the process
//! table, then lists the running Claude Code processes to learn whether every
//! one is registered. [`ClaudeRegistry::session_end`] answers `Ended` only
//! when that read is complete, a consulted dir holds the session's
//! transcript, so its registry is the one the session would appear in, and no
//! entry naming the session belongs to a running process.
//! [`ClaudeRegistry::replaced_in`] answers whether a tmux session now runs a
//! different Claude session.
//!
//! # Fail direction
//!
//! Every answer that is not positive evidence is `Undeterminable` (ADR-0045):
//! a registry or projects dir that cannot be read, a live entry that does not
//! parse, a running pid whose start time was not recorded or cannot be read,
//! a session whose transcript no consulted dir holds, a dir that holds the
//! transcript but no registry, and an incomplete read — a running Claude Code
//! process no consulted registry lists, or a process table that cannot show
//! there is none. [`ClaudeRegistry::NotRead`] answers nothing.
//! Test: `worktree_claude_registry_tests`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::manager::SessionManager;
use super::worktree_claude_processes::{Identity, ProcessInfo, identify, list_processes};
use super::worktree_owner_gate::{
    START_TOLERANCE_SECS, SessionEnd, parse_start_stamp, process_start_secs,
};
use super::worktree_registry::pid_liveness;

/// Reads the registry afresh on every call, so a destructive gate never
/// judges from an earlier snapshot (#7771).
pub(crate) type RegistryReader = Arc<dyn Fn() -> ClaudeRegistry + Send + Sync>;

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
    /// The process the entry registers.
    pub(crate) pid: u32,
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

/// The process table a registry read is judged against, injected so no test
/// reads the host's (#7771).
pub(crate) struct ProcessTable<'a> {
    /// Whether a pid runs; `None` when the table cannot say.
    pub(crate) pid_alive: &'a dyn Fn(u32) -> Option<bool>,
    /// A running pid's start, as Unix seconds.
    pub(crate) start_of: &'a dyn Fn(u32) -> Result<i64, String>,
    /// Every running process, for [`identify`].
    pub(crate) list: &'a dyn Fn() -> Result<Vec<ProcessInfo>, String>,
}

impl ProcessTable<'static> {
    /// This host's process table.
    pub(crate) fn host() -> Self {
        Self {
            pid_alive: &pid_liveness,
            start_of: &process_start_secs,
            list: &list_processes,
        }
    }
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
    Read {
        /// Each consulted dir, as read.
        roots: Vec<ConfigRoot>,
        /// `Ok` when every running Claude Code process has a live entry;
        /// `Err(why)` when one has none, or that cannot be shown.
        complete: Result<(), String>,
    },
}

impl ClaudeRegistry {
    /// The registry under `roots`, judged against the real process table. No
    /// roots is [`Self::NotRead`].
    pub(crate) fn read(roots: &[PathBuf]) -> Self {
        Self::read_with(roots, &ProcessTable::host())
    }

    /// [`Self::read`] with the process table injected.
    ///
    /// What: each root's `sessions/*.json`. A missing `sessions` dir is no
    /// registry; one that cannot be listed is an error. An entry that does not
    /// parse is skipped only when the pid its file name carries is gone, since
    /// a dead process runs nothing; otherwise the root is an error. The
    /// process table is listed AFTER the entries are read, so a process that
    /// was already running and has no entry is caught ([`completeness`]).
    /// Test: `claude_registry_ignores_a_corrupt_entry_whose_pid_is_gone`,
    /// `claude_registry_probe_failures_are_undeterminable`,
    /// `claude_registry_an_unregistered_claude_process_is_undeterminable`.
    pub(crate) fn read_with(roots: &[PathBuf], table: &ProcessTable<'_>) -> Self {
        if roots.is_empty() {
            return Self::NotRead;
        }
        let roots: Vec<ConfigRoot> = roots
            .iter()
            .map(|root| ConfigRoot {
                root: root.clone(),
                entries: read_entries(&root.join("sessions"), table),
            })
            .collect();
        let complete = completeness(&roots, table.list);
        Self::Read { roots, complete }
    }

    /// Whether Claude session `id` has ended, or `None` when not read.
    ///
    /// What: `Live` when any entry naming `id` runs; `Undeterminable` when one
    /// cannot be judged, a root could not be read, no root holds the
    /// session's transcript (`projects/*/<id>.jsonl`), a root holds it but
    /// keeps no registry, or the read is incomplete. `Ended` otherwise.
    /// Test: `claude_registry_ends_a_session_no_live_process_runs`,
    /// `claude_registry_keeps_a_session_a_live_process_runs`,
    /// `claude_registry_probe_failures_are_undeterminable`,
    /// `claude_registry_an_unregistered_claude_process_is_undeterminable`.
    pub(crate) fn session_end(&self, id: &str) -> Option<SessionEnd> {
        let Self::Read { roots, complete } = self else {
            return None;
        };
        Some(judge_session(roots, complete, id))
    }

    /// Whether the record whose tmux session is `tmux` was relaunched onto a
    /// different Claude session, leaving `id` behind (#7771).
    ///
    /// What: `id` is [`SessionEnd::Ended`] by [`Self::session_end`], and a
    /// running process in `tmux` runs another session id.
    /// Test: `worktree_7771_a_session_replaced_in_its_tmux_window_is_reclaimed`,
    /// `claude_registry_a_live_id_is_not_replaced_by_a_newer_one`,
    /// `worktree_7771_a_live_id_beside_a_newer_one_keeps_the_tree`.
    pub(crate) fn replaced_in(&self, id: &str, tmux: &str) -> bool {
        let Self::Read { roots, complete } = self else {
            return false;
        };
        // #7771 critic: a newer session in the same tmux session does not end
        // `id` while a process still runs it.
        judge_session(roots, complete, id) == SessionEnd::Ended
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

/// Whether every running Claude Code process has a live entry (#7771).
///
/// Why: an older Claude Code, or a background-job worker, runs a session and
/// writes no entry, so an absent entry proves nothing while one runs (#7771
/// critic, HIGH). What: `Err` when the table cannot be listed, a process
/// named like Claude Code cannot be read, no Claude Code process is found at
/// all, a Claude Code process has no live entry, or a live entry's process
/// was not identified as Claude Code — the identification is then not
/// trusted.
/// Test: `claude_registry_an_unregistered_claude_process_is_undeterminable`,
/// `claude_registry_an_unidentified_table_is_undeterminable`.
fn completeness(
    roots: &[ConfigRoot],
    list: &dyn Fn() -> Result<Vec<ProcessInfo>, String>,
) -> Result<(), String> {
    let processes = list().map_err(|e| {
        format!(
            "the process table could not be listed ({e}), so a running Claude Code process \
             with no registry entry cannot be ruled out"
        )
    })?;
    let mut running = BTreeSet::new();
    for p in &processes {
        match identify(p) {
            Identity::Session => {
                running.insert(p.pid);
            }
            Identity::Unreadable => {
                return Err(format!(
                    "process {} is named `{}` and neither its executable nor its argv can be \
                     read, so whether it is an unregistered Claude Code process is unknown",
                    p.pid, p.name
                ));
            }
            Identity::EmbeddedTool | Identity::Other => {}
        }
    }
    if running.is_empty() {
        return Err(
            "no running Claude Code process could be identified in the process table, \
                    so it cannot show that every one is registered"
                .into(),
        );
    }
    let registered: BTreeSet<u32> = entries(roots)
        .filter(|e| e.liveness == EntryLiveness::Live)
        .map(|e| e.pid)
        .collect();
    if let Some(pid) = running.difference(&registered).next() {
        return Err(format!(
            "Claude Code process {pid} runs with no live entry in any consulted registry (an \
             older Claude Code, or a background worker), so the session it runs is unknown"
        ));
    }
    if let Some(pid) = registered.difference(&running).next() {
        return Err(format!(
            "registered process {pid} runs but was not identified as Claude Code, so the \
             process identification cannot be trusted"
        ));
    }
    Ok(())
}

/// See [`ClaudeRegistry::session_end`].
fn judge_session(roots: &[ConfigRoot], complete: &Result<(), String>, id: &str) -> SessionEnd {
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
    if !known {
        let dirs: Vec<String> = roots.iter().map(|r| r.root.display().to_string()).collect();
        return SessionEnd::Undeterminable(format!(
            "no consulted Claude config dir ({}) holds its transcript, so no registry read here \
             would list the process running it",
            dirs.join(", ")
        ));
    }
    // #7771 critic: no entry names it, which proves nothing while a Claude
    // Code process runs unregistered.
    match complete {
        Ok(()) => SessionEnd::Ended,
        Err(why) => SessionEnd::Undeterminable(why.clone()),
    }
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
    table: &ProcessTable<'_>,
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
            Ok((start, mut entry)) => {
                entry.liveness = judge_entry(entry.pid, start, table.pid_alive, table.start_of);
                out.push(entry);
            }
            Err(why) => {
                let stem_pid = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u32>().ok());
                if stem_pid.and_then(table.pid_alive) != Some(false) {
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

/// One entry file: its recorded process start, and the entry.
fn parse_entry(path: &Path) -> Result<(Option<i64>, RegistryEntry), String> {
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
        pid,
        session_id,
        tmux_session,
        liveness: EntryLiveness::Gone,
    };
    Ok((start, entry))
}

/// Judge one entry's process: gone, reused, running, or unknown.
///
/// What: a running pid is `Live` when its start matches the recorded one
/// within [`START_TOLERANCE_SECS`], and `Gone` (reused) when it started
/// later. One that started EARLIER than recorded is `Unknown`: a reused pid
/// always starts after the entry was written, so an earlier start is clock or
/// format skew, not reuse.
/// Test: `claude_registry_reused_pid_is_not_live`,
/// `claude_registry_an_earlier_start_is_skew_not_reuse`,
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
        Ok(actual) if actual > recorded => EntryLiveness::Gone,
        // #7771 critic: a pid reused after the entry was written starts later.
        Ok(actual) => EntryLiveness::Unknown(format!(
            "Claude process {pid} started at {actual}, before the {recorded} its registry entry \
             records — clock or format skew, not a reused pid"
        )),
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
        self.install_claude_registry(Arc::new(move || ClaudeRegistry::read(&roots)))
    }

    /// [`Self::install_claude_registry_roots`] with the reader injected, so a
    /// test supplies its own process table (#7771).
    pub(crate) fn install_claude_registry(&self, reader: RegistryReader) -> bool {
        self.claude_registry.set(reader).is_ok()
    }

    /// The installed registry, read now; `NotRead` when none was installed.
    pub(crate) fn claude_registry(&self) -> ClaudeRegistry {
        self.claude_registry
            .get()
            .map_or(ClaudeRegistry::NotRead, |read| read())
    }
}

#[cfg(test)]
#[path = "worktree_claude_registry_tests.rs"]
mod worktree_claude_registry_tests;
