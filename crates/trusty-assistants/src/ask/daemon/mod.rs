//! The per-instance ask daemon and its client (#11650, slice 3b).
//!
//! Why: `ta ask <instance> <question>` must answer through one long-lived
//! process per instance, so the turn log has one writer and concurrent asks
//! queue instead of racing. The owner's constraints: one on-demand daemon per
//! instance at `<trusty-agents data dir>/trusty-agents-<id>.sock`, a singleton
//! hardened bind (0600 socket, peer-uid check), idle exit with no timers, no
//! polling and no network listener, and the #11550 `agents.health` socket
//! served beside it.
//! What:
//! - this file — paths, the wire protocol ([`AskRequest`], [`AskReply`]) and
//!   [`AskDaemonError`];
//! - `server` — [`bind`] and [`BoundDaemon::serve`]: the accept loop, the
//!   peer check, one ask at a time, cancel on client hang-up, idle exit;
//! - `client` — [`ask_instance`]: start the daemon on demand, then ask;
//! - `run` — [`run_daemon`], the `ta ask-daemon` body.
//!
//! Wire protocol: one connection carries one ask. The client writes one
//! newline-terminated [`AskRequest`] and keeps its write half OPEN; the daemon
//! writes one newline-terminated [`AskReply`]. EOF on the daemon's read side
//! therefore means the client is gone, and the daemon cancels that ask.
//!
//! Idle exit: the daemon exits when no connection is open, no ask is in
//! flight, and its spawner has released it (closed the daemon's stdin). Every
//! wake-up is an event — an accept, a connection ending, stdin EOF, a signal.
//! Test: `tests` (in-process) and `tests/ask_daemon.rs` (the built binary).

mod client;
mod run;
mod server;

#[cfg(test)]
mod tests;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use trusty_common::uds::UdsSecurityError;

use crate::assistants::{AssistantError, AssistantHome, AssistantInstanceId};

pub use client::{DaemonLauncher, START_BUDGET, ask_at, ask_instance};
pub use run::run_daemon;
pub use server::{Bound, BoundDaemon, EngineFactory, ServeExit, ServeOptions, ServeSummary, bind};

/// The wire protocol version both ends must speak.
pub const PROTOCOL_VERSION: u32 = 1;

/// The `ta` verb that runs one instance's daemon.
pub const DAEMON_VERB: &str = "ask-daemon";

/// The line a daemon prints on stdout once its socket is bound.
pub const READY_LINE: &str = "ready";

/// The line a daemon prints when another daemon already serves the socket.
pub const BUSY_LINE: &str = "busy";

/// Largest request or reply line, in bytes.
pub const MAX_LINE_BYTES: u64 = 1024 * 1024;

/// One question for the daemon.
/// Test: `tests::an_ask_round_trips_and_the_daemon_exits_idle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRequest {
    /// The sender's [`PROTOCOL_VERSION`].
    pub protocol: u32,
    /// The question, as typed.
    pub question: String,
}

/// The daemon's one reply on a connection.
/// Test: `tests::an_ask_round_trips_and_the_daemon_exits_idle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskReply {
    /// The daemon's [`PROTOCOL_VERSION`].
    pub protocol: u32,
    /// The daemon's process id, so a client can tell daemons apart.
    pub daemon_pid: u32,
    /// The answer or the failure.
    #[serde(flatten)]
    pub outcome: ReplyOutcome,
}

/// What one ask produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReplyOutcome {
    /// The model's answer; the turn is in the log.
    Answer { answer: String },
    /// No answer; the turn log is unchanged.
    Error {
        kind: ReplyErrorKind,
        message: String,
    },
}

/// Why a reply carries no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyErrorKind {
    /// The request named a protocol version this daemon does not speak.
    ProtocolMismatch,
    /// The request was not a valid [`AskRequest`].
    BadRequest,
    /// The ask itself failed ([`crate::ask::AskError`]).
    Ask,
}

/// A failure to run, reach or use an instance's ask daemon.
///
/// Why: item 6 of the 3b acceptance list — every failure names what to fix
/// (the instance id, the socket or the daemon's log). None is turned into an
/// answer, and a bind failure never falls back to an unhardened socket.
#[derive(Debug, Error)]
pub enum AskDaemonError {
    /// The instance id is not a valid name.
    #[error("invalid assistant instance id: {0}")]
    InvalidInstance(#[source] AssistantError),

    /// No home directory exists for the instance.
    #[error("unknown assistant instance `{id}`: no home at {}", home.display())]
    UnknownInstance { id: String, home: PathBuf },

    /// The trusty-agents data directory could not be resolved.
    #[error("cannot resolve the trusty-agents data directory: {0}")]
    DataDir(String),

    /// The daemon could not bind its socket. Never retried unhardened.
    #[error("cannot bind the ask socket {}: {source}", socket.display())]
    Bind {
        socket: PathBuf,
        #[source]
        source: UdsSecurityError,
    },

    /// The daemon process could not be spawned.
    #[error("cannot start the ask daemon `{}`: {source}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The daemon exited before it was ready.
    #[error(
        "the ask daemon for `{id}` failed to start ({detail}); its log is {}",
        log.display()
    )]
    StartFailed {
        id: String,
        log: PathBuf,
        detail: String,
    },

    /// The daemon did not report ready within the start budget.
    #[error(
        "the ask daemon for `{id}` was not ready within {secs}s and was stopped; its log is {}",
        log.display()
    )]
    StartTimeout { id: String, log: PathBuf, secs: u64 },

    /// Starting raced with other clients more often than allowed.
    #[error(
        "no ask daemon for `{id}` was reachable after {attempts} starts; its log is {}",
        log.display()
    )]
    Unreachable {
        id: String,
        log: PathBuf,
        attempts: u32,
    },

    /// The socket exists but failed the ownership or mode check, or the
    /// connect failed for a reason other than "nobody is serving".
    #[error("cannot connect to the ask daemon: {0}")]
    Connect(#[source] UdsSecurityError),

    /// The two ends speak different protocol versions.
    #[error(
        "protocol version mismatch: this client speaks {ours}, the daemon speaks {theirs}; \
         stop the old daemon (it exits when idle) and retry"
    )]
    ProtocolMismatch { ours: u32, theirs: u32 },

    /// The daemon closed the connection without a reply.
    #[error("the ask daemon closed the connection without an answer")]
    Disconnected,

    /// The reply was not a valid [`AskReply`].
    #[error("the ask daemon sent an unreadable reply: {0}")]
    BadReply(String),

    /// The daemon refused the request as malformed.
    #[error("the ask daemon refused the request: {0}")]
    BadRequest(String),

    /// The ask ran and failed; the turn log is unchanged.
    #[error("{0}")]
    Ask(String),

    /// An I/O failure on the socket, the log or a pipe.
    #[error("{what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl AskDaemonError {
    /// An [`AskDaemonError::Io`] naming what was being done.
    pub(crate) fn io(what: &'static str) -> impl FnOnce(std::io::Error) -> Self {
        move |source| Self::Io { what, source }
    }
}

/// The trusty-agents data directory, created if absent.
/// Test: `tests/ask_daemon.rs` resolves it through `TRUSTY_DATA_DIR_OVERRIDE`.
pub fn data_dir() -> Result<PathBuf, AskDaemonError> {
    trusty_common::resolve_data_dir(trusty_agents_common::health::AGENTS_APP_NAME)
        .map_err(|e| AskDaemonError::DataDir(format!("{e:#}")))
}

/// `<data_dir>/trusty-agents-<id>.sock`, the owner's spelling.
/// Test: `tests::paths_follow_the_owners_spelling`.
pub fn instance_socket_in(data_dir: &Path, id: &AssistantInstanceId) -> PathBuf {
    data_dir.join(format!("trusty-agents-{id}.sock"))
}

/// `<data_dir>/trusty-agents-<id>.log`, where a spawned daemon's stderr goes.
/// Test: `tests::paths_follow_the_owners_spelling`.
pub fn instance_log_in(data_dir: &Path, id: &AssistantInstanceId) -> PathBuf {
    data_dir.join(format!("trusty-agents-{id}.log"))
}

/// The home of `raw_id` under `root`, which must exist.
///
/// Why: an unknown id is refused before any daemon starts, naming the path
/// that was looked for.
/// What: `root` `None` means [`crate::assistants::assistants_root`].
/// Test: `tests::an_unknown_instance_is_named_with_its_path`.
pub fn resolve_home(root: Option<&Path>, raw_id: &str) -> Result<AssistantHome, AskDaemonError> {
    let home = match root {
        Some(root) => AssistantInstanceId::new(raw_id)
            .map(|id| AssistantHome::under(root, id))
            .map_err(AskDaemonError::InvalidInstance)?,
        None => AssistantHome::for_instance(raw_id).map_err(AskDaemonError::InvalidInstance)?,
    };
    if !home.path().is_dir() {
        return Err(AskDaemonError::UnknownInstance {
            id: home.id().to_string(),
            home: home.path().to_path_buf(),
        });
    }
    Ok(home)
}

/// A socket path this process bound, remembered by inode.
///
/// Why: requirement 5 — the daemon removes its own socket on exit, and only
/// its own: a successor may have bound the same path after this process
/// stopped answering it.
/// What: records `(dev, ino)` after the bind; [`Self::remove`] unlinks the
/// path only while it still names that inode.
/// Test: `tests::an_ask_round_trips_and_the_daemon_exits_idle`,
/// `tests::the_health_socket_is_served_and_removed`.
#[derive(Debug)]
pub(crate) struct OwnedSocketPath {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl OwnedSocketPath {
    /// Record the inode now at `path`.
    pub(crate) fn capture(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::symlink_metadata(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    /// The recorded path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Unlink the path if it still names the recorded inode.
    pub(crate) fn remove(&self) {
        match std::fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.dev() == self.dev && meta.ino() == self.ino => {
                if let Err(e) = std::fs::remove_file(&self.path) {
                    tracing::warn!(socket = %self.path.display(), error = %e, "socket not removed");
                }
            }
            Ok(_) => tracing::info!(
                socket = %self.path.display(),
                "socket path now names another socket; left in place",
            ),
            Err(_) => {}
        }
    }
}
