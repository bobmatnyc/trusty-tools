//! Which `claude` process registered each session id, from the kernel (#8531).
//!
//! Why: a delegation repair is granted to the session that owns the record,
//! and "owns" must be a fact about a process, not a name or a field a caller
//! can write. A session record's `pid` is PATCHable (`mpm.sessions.set_pid`)
//! and its `tmux_name` can be squatted with `tmux new-session`; the record
//! itself is dropped by the reaper and can then be re-created by anyone who
//! names the id. This registry is none of those things.
//! What: [`SessionClaudes`] records, per session id, the FIRST `SessionStart`
//! the daemon saw for it: the `claude` process (pid + start time) above the
//! kernel-reported socket peer, or [`Announcement::Unproven`] when that first
//! announcement proved no process (HTTP, no peer pid, a failed walk). A
//! recorded id is never rewritten or cleared, and the reaper never touches it.
//! The registry lives in `<framework root>/session-claudes.json` (mode
//! `0600`, replaced atomically on every new id), so a daemon restart does not
//! reopen an id to a second announcer.
//!
//! A file the daemon cannot trust — unreadable, unparseable, not a regular
//! file, owned by another uid, or open to other users — SEALS the registry:
//! it grants no owner and records no new id until an operator removes or
//! fixes the file. A missing file (first start) is an empty registry: no
//! owner is granted until its `claude` announces itself again.
//!
//! Trade-off (#8531 LOW): the registry is unbounded. Dropping an entry would
//! make its id announceable again, which is the impersonation this module
//! closes, so entries are kept for good: one small JSON entry per session id
//! the daemon ever saw start, and every new id rewrites the whole file. An
//! entry whose process has exited grants nothing (see
//! `delegation_repair_caller::owner_claude`).
//!
//! Residuals (#8531, accepted). The first three leave an owner unable to
//! repair its own records until the stale (6 h) or owner-gone path ends
//! them; the fourth leaves an id open to a deliberate impersonator.
//! - Upgrade window: the first daemon start on this code has no file, so no
//!   session that started before it is bound.
//! - A first `SessionStart` that fell back to HTTP settles its id as
//!   unproven for good, so that owner can never repair its records.
//! - A resumed session (`claude --resume <id>` keeps the id; tm relaunches
//!   this way, see `runtime/claude_code.rs` and
//!   `daemon/managed_routes/lifecycle.rs`) cannot clear its own records: the
//!   id is settled and the old binding fails the pid + start-time check. No
//!   rebind is safe, since the new `claude` is indistinguishable from a
//!   sibling announcing the same id.
//! - An id whose `SessionStart` never reached the daemon (daemon down at
//!   start, no tm `SessionStart` hook — doctor check
//!   `hooks_missing_tm_group` — or a pre-upgrade session) stays announceable
//!   for the session's life. Claiming it takes a sibling that knows the id
//!   and announces it first, on purpose.
//!
//! Test: `session_claudes_tests.rs`.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::DaemonState;
use crate::core::session::SessionId;
use crate::core::twin_arming::{
    ProcessFacts, STATUS_MAX_ANCESTOR_HOPS, nearest_claude_for_status_in, process_facts,
};
use crate::core::twin_identity::ClaudeProcess;

/// The registry file, under the daemon's framework root.
pub(crate) const SESSION_CLAUDES_FILE: &str = "session-claudes.json";

/// The on-disk format version this build reads and writes.
const FORMAT_VERSION: u32 = 1;

/// What a session id's first `SessionStart` proved about its sender (#8531).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Announcement {
    /// The `claude` above the kernel-reported socket peer.
    Claude(ClaudeProcess),
    /// The first announcement proved no process; the id is never bound.
    Unproven,
}

/// The first-announcement registry of every session id (#8531).
///
/// Why: see the module doc.
/// What: session id → [`Announcement`], or the reason the registry is
/// sealed. Every new binding is written through to [`SESSION_CLAUDES_FILE`]
/// before it counts; an unproven id counts even when its save fails.
/// Test: `a_session_is_bound_once_first_writer_wins_8531`,
/// `a_restart_keeps_the_binding_8531`.
#[derive(Debug)]
pub struct SessionClaudes {
    path: PathBuf,
    registry: Mutex<Result<HashMap<SessionId, Announcement>, String>>,
}

impl SessionClaudes {
    /// The registry persisted under `root`, as the daemon's own uid reads it.
    pub(crate) fn load(root: &Path) -> Self {
        Self::load_as(root.join(SESSION_CLAUDES_FILE), current_uid())
    }

    /// The registry at `path`, trusted only when `uid` owns it (#8531).
    ///
    /// What: [`read_registry`]; a refusal seals the registry and is logged.
    /// Test: `a_corrupt_file_seals_the_registry_8531`,
    /// `a_foreign_owned_file_seals_the_registry_8531`,
    /// `an_unreadable_file_seals_the_registry_8531`.
    pub(crate) fn load_as(path: PathBuf, uid: u32) -> Self {
        let registry = read_registry(&path, uid);
        if let Err(why) = &registry {
            tracing::error!(
                "#8531: the session-claude registry is sealed — no delegation repair is \
                 granted to an owning session until it is fixed or removed: {why}"
            );
        }
        Self {
            path,
            registry: Mutex::new(registry),
        }
    }

    /// The `claude` bound to `session`, if its first announcement proved one.
    /// `None` for an unproven or unknown id, and always while sealed.
    pub fn get(&self, session: SessionId) -> Option<ClaudeProcess> {
        match &*self.registry.lock() {
            Ok(map) => match map.get(&session) {
                Some(Announcement::Claude(claude)) => Some(*claude),
                _ => None,
            },
            Err(_) => None,
        }
    }

    /// Whether nothing more can be recorded for `session`: its first
    /// announcement is recorded, or the registry is sealed.
    pub fn is_settled(&self, session: SessionId) -> bool {
        match &*self.registry.lock() {
            Ok(map) => map.contains_key(&session),
            Err(_) => true,
        }
    }

    /// Why the registry is sealed, if it is.
    pub fn sealed(&self) -> Option<String> {
        self.registry.lock().as_ref().err().cloned()
    }

    /// Record `announcement` as `session`'s first (#8531).
    ///
    /// What: `Ok(None)` when it was recorded and saved; `Ok(Some(earlier))`
    /// when `session` already had one, which is left as it was; `Err` when
    /// the registry is sealed, `admit` refuses, or the save fails. On a failed
    /// save an [`Announcement::Claude`] is rolled back, so no unsaved binding
    /// ever grants; an [`Announcement::Unproven`] stays in memory, since it
    /// can only deny, and reaches the file with the next saved id.
    /// Test: `an_unsaved_binding_does_not_count_8531`,
    /// `an_unsaved_unproven_announcement_still_settles_the_id_8531`.
    fn record(
        &self,
        session: SessionId,
        announcement: Announcement,
        admit: impl FnOnce() -> Result<(), String>,
    ) -> Result<Option<Announcement>, String> {
        let mut guard = self.registry.lock();
        let map = guard
            .as_mut()
            .map_err(|why| format!("the session-claude registry is sealed: {why}"))?;
        if let Some(earlier) = map.get(&session) {
            return Ok(Some(*earlier));
        }
        admit()?;
        map.insert(session, announcement);
        if let Err(e) = write_registry(&self.path, map) {
            // #8531: only a grant is rolled back; a deny-only entry keeps the
            // id closed to the next announcer.
            if matches!(announcement, Announcement::Claude(_)) {
                map.remove(&session);
            }
            return Err(format!(
                "the announcement could not be saved to {}: {e}",
                self.path.display()
            ));
        }
        Ok(None)
    }

    /// The nearest `claude` above the kernel-reported peer `pid` (#8531).
    ///
    /// Why: both the binding and the repair check start from a pid the kernel
    /// reported when the request arrived, and the walk runs later; by then
    /// the peer may have exited and its pid been reused.
    /// What: [`peer_claude_with`] over the live process table and the same
    /// `claude` name rule the hook's walk uses.
    /// Test: `peer_claude_with`'s cases.
    pub fn peer_claude(&self, pid: u32, seen_at: u64) -> Result<ClaudeProcess, String> {
        peer_claude_with(
            pid,
            seen_at,
            process_facts,
            crate::core::twin_arming::is_claude,
        )
    }
}

/// [`SessionClaudes::peer_claude`] over an injected process table (#8531).
///
/// What: `Err` when the peer's process cannot be read, when it started after
/// `seen_at` (a reused pid), when the ancestry cannot be read, or when no
/// `claude` runs within [`STATUS_MAX_ANCESTOR_HOPS`] of it. Otherwise that
/// `claude`, with its start time.
/// Test: `a_peer_started_after_the_request_is_refused_8531`,
/// `an_unreadable_peer_is_refused_8531`,
/// `a_peer_with_no_claude_above_it_is_refused_8531`,
/// `the_peer_claude_is_the_nearest_claude_ancestor_8531`.
pub(crate) fn peer_claude_with(
    pid: u32,
    seen_at: u64,
    facts: impl Fn(u32) -> Result<ProcessFacts, String>,
    is_claude: impl Fn(u32) -> Result<bool, String>,
) -> Result<ClaudeProcess, String> {
    // #8531: a peer that started after the request arrived holds a reused pid.
    let started = facts(pid)
        .map_err(|e| format!("the calling process (pid {pid}) could not be read: {e}"))?
        .start_time;
    if started > seen_at {
        return Err(format!(
            "pid {pid} now names a process started after the request arrived"
        ));
    }
    match nearest_claude_for_status_in(pid, facts, is_claude) {
        Ok(Some(claude)) => Ok(claude),
        Ok(None) => Err(format!(
            "no claude session process runs within {STATUS_MAX_ANCESTOR_HOPS} parent hops \
             of the calling process (pid {pid})"
        )),
        Err(e) => Err(format!(
            "the calling process's ancestry (pid {pid}) could not be read: {e}"
        )),
    }
}

/// The registry file's shape: a version and one entry per session id.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRegistry {
    version: u32,
    sessions: BTreeMap<String, StoredAnnouncement>,
}

/// One entry of [`StoredRegistry`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum StoredAnnouncement {
    Claude { pid: u32, start_time: u64 },
    Unproven,
}

/// Read the registry at `path`, trusting it only when `uid` owns it (#8531).
///
/// Why: the file decides who may end another session's delegation, so a file
/// another user could have written, or one this build cannot read, must not
/// decide anything.
/// What: an absent file is an empty registry. `Err` naming the reason when
/// the path cannot be opened (a symlink is refused, not followed), is not a
/// regular file, is owned by another uid, has any group or other permission
/// bit, cannot be read, does not parse, carries another format version, or
/// names a malformed session id.
/// Test: `a_corrupt_file_seals_the_registry_8531`,
/// `a_foreign_owned_file_seals_the_registry_8531`,
/// `an_unreadable_file_seals_the_registry_8531`,
/// `a_file_open_to_other_users_seals_the_registry_8531`,
/// `a_missing_file_is_an_empty_registry_8531`.
fn read_registry(path: &Path, uid: u32) -> Result<HashMap<SessionId, Announcement>, String> {
    let shown = path.display();
    let mut file = match open_no_follow(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(format!("{shown} could not be opened: {e}")),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("{shown} could not be inspected: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{shown} is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != uid {
            return Err(format!(
                "{shown} is owned by uid {}, not the daemon's uid {uid}",
                meta.uid()
            ));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(format!(
                "{shown} has mode {:o}; only its owner may read or write it",
                meta.mode() & 0o777
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = uid;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| format!("{shown} could not be read: {e}"))?;
    let stored: StoredRegistry =
        serde_json::from_str(&text).map_err(|e| format!("{shown} does not parse: {e}"))?;
    if stored.version != FORMAT_VERSION {
        return Err(format!(
            "{shown} is format version {}, this daemon reads {FORMAT_VERSION}",
            stored.version
        ));
    }
    let mut map = HashMap::with_capacity(stored.sessions.len());
    for (id, entry) in stored.sessions {
        let session = uuid::Uuid::parse_str(&id)
            .map(SessionId)
            .map_err(|e| format!("{shown} names a malformed session id {id:?}: {e}"))?;
        let announcement = match entry {
            StoredAnnouncement::Claude { pid, start_time } => {
                Announcement::Claude(ClaudeProcess { pid, start_time })
            }
            StoredAnnouncement::Unproven => Announcement::Unproven,
        };
        map.insert(session, announcement);
    }
    Ok(map)
}

/// Open `path` for reading without following a symlink or blocking on a FIFO.
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

/// Replace `path` with `map`, atomically, mode `0600` (#8531).
///
/// What: a `0600` temp file beside `path`, written and synced, renamed over
/// it, then the directory synced. A failure leaves `path` as it was.
/// Test: `a_restart_keeps_the_binding_8531` (mode and round trip).
fn write_registry(path: &Path, map: &HashMap<SessionId, Announcement>) -> std::io::Result<()> {
    crate::core::home_write_fence::check(path); // #8545
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let sessions = map
        .iter()
        .map(|(session, announcement)| {
            let entry = match announcement {
                Announcement::Claude(c) => StoredAnnouncement::Claude {
                    pid: c.pid,
                    start_time: c.start_time,
                },
                Announcement::Unproven => StoredAnnouncement::Unproven,
            };
            (session.0.to_string(), entry)
        })
        .collect();
    let stored = StoredRegistry {
        version: FORMAT_VERSION,
        sessions,
    };
    let bytes = serde_json::to_vec(&stored).map_err(std::io::Error::other)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".session-claudes.")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// The daemon's effective uid; `0` off Unix, where no owner check runs.
fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` takes no arguments, reads a process property and
        // cannot fail.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

impl DaemonState {
    /// The kernel-bound `claude` registry (#8531).
    pub fn session_claudes(&self) -> &SessionClaudes {
        &self.session_claudes
    }

    /// Bind `session` to the `claude` that announced it (#8531).
    ///
    /// Why: the binding is what a later repair compares the caller against,
    /// so it can be written once only, and never for an id whose records
    /// predate it — an id that already owns delegations was announced to the
    /// daemon by someone this call cannot identify.
    /// What: `Ok` when `session` had no recorded announcement, no delegation
    /// names it, and the binding was saved; `Err` naming why otherwise.
    /// Never overwrites.
    /// Test: `a_session_is_bound_once_first_writer_wins_8531`,
    /// `a_session_that_already_owns_records_is_not_bound_8531`,
    /// `an_unproven_session_is_never_bound_8531`.
    pub fn bind_session_claude(
        &self,
        session: SessionId,
        claude: ClaudeProcess,
    ) -> Result<(), String> {
        let admit = || {
            if self.delegations.iter().any(|d| d.session == session) {
                return Err(
                    "the session already owns delegation records, so the process that \
                     announced it first is unknown"
                        .to_string(),
                );
            }
            Ok(())
        };
        match self
            .session_claudes
            .record(session, Announcement::Claude(claude), admit)?
        {
            None => Ok(()),
            Some(Announcement::Claude(_)) => {
                Err("the session is already bound to its claude".to_string())
            }
            Some(Announcement::Unproven) => Err(
                "the session was first announced without a kernel-verified claude, so it is \
                 never bound"
                    .to_string(),
            ),
        }
    }

    /// Record that `session`'s first announcement proved no process (#8531).
    ///
    /// Why: an id first announced over HTTP, or by a socket `SessionStart`
    /// that could not be bound, would otherwise stay open to whichever
    /// process announces it next over the socket.
    /// What: records [`Announcement::Unproven`] when `session` has nothing
    /// recorded; a no-op when it has. `Err` when sealed or the save fails;
    /// an unsaved record still settles the id for this daemon's lifetime.
    /// Test: `an_unproven_session_is_never_bound_8531`,
    /// `an_unsaved_unproven_announcement_still_settles_the_id_8531`.
    pub fn settle_unproven_session(&self, session: SessionId) -> Result<(), String> {
        self.session_claudes
            .record(session, Announcement::Unproven, || Ok(()))
            .map(|_| ())
    }
}

#[cfg(test)]
#[path = "session_claudes_tests.rs"]
mod session_claudes_tests;
