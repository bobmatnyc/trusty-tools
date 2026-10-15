//! Error types for tmux access and the guarded send path.

use crate::input::InputState;

/// A tmux call that could not run, or ran and failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TmuxError {
    /// `run` was given no arguments, so there is no tmux verb to run.
    #[error("tmux: no verb given")]
    EmptyCommand,
    /// `Tmux::run` was given the `send-keys` verb. Keys go out only through
    /// [`crate::send_guarded`], which checks the input box first.
    #[error("tmux send-keys is refused here; use send_guarded")]
    SendKeysRefused,
    /// The `tmux` process could not be started (not on `PATH`, no permission).
    #[error("tmux {verb}: could not start: {source}")]
    Spawn {
        /// The tmux verb, e.g. `send-keys`.
        verb: String,
        /// The underlying spawn error.
        #[source]
        source: std::io::Error,
    },
    /// tmux ran and exited non-zero.
    #[error("tmux {verb}: exit {code:?}: {stderr}")]
    Failed {
        /// The tmux verb, e.g. `send-keys`.
        verb: String,
        /// The exit code, or `None` when a signal ended the process.
        code: Option<i32>,
        /// tmux's stderr, trimmed.
        stderr: String,
    },
    /// tmux did not finish within the deadline; the child was killed and
    /// reaped (#9171 A3a).
    #[error("tmux {verb}: timed out after {after:?}")]
    Timeout {
        /// The tmux verb, e.g. `capture-pane`.
        verb: String,
        /// The deadline that passed.
        after: std::time::Duration,
    },
    /// Waiting on the tmux child failed; the child was killed and reaped.
    #[error("tmux {verb}: wait failed: {source}")]
    Wait {
        /// The tmux verb.
        verb: String,
        /// The underlying wait error.
        #[source]
        source: std::io::Error,
    },
    /// A pane target that is not tmux's immutable `%N` pane id.
    #[error("not a tmux pane id (want %N): {0:?}")]
    InvalidPaneId(String),
}

/// Why [`crate::send_guarded`] did not deliver keys.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SendError {
    /// The input box did not accept input; nothing was sent.
    #[error("refused to send to {pane}: input box reads {state}")]
    Refused {
        /// The pane the keys were meant for.
        pane: String,
        /// The input-box state that blocked the send.
        state: InputState,
    },
    /// The box accepted input, but the tmux call failed.
    #[error(transparent)]
    Tmux(#[from] TmuxError),
}

/// Why the poller's wake did not complete (#9171 A3b-1).
///
/// The prototype raises `TmuxError` for each of these and `dispatch_wake`
/// reports `failed: <message>`; the messages match the prototype's where it
/// has one.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WakeError {
    /// The pane's live session is not `tm-architect`; nothing was sent.
    #[error(
        "refusing to send to {pane} (session {})",
        crate::pystr::py_repr(session)
    )]
    NotOwner {
        /// The pane the keys were meant for.
        pane: String,
        /// The session `display-message` reported, stripped.
        session: String,
    },
    /// The send reading did not accept input; nothing was typed.
    #[error("refusing to type into {pane}: input box reads {state}")]
    Refused {
        /// The pane the pointer was meant for.
        pane: String,
        /// The reading that blocked the send.
        state: InputState,
    },
    /// The text to type was empty after whitespace is removed, so a later
    /// cleanup could not tell it from any box.
    #[error("refusing to type an empty pointer into {0}")]
    EmptyMessage(String),
    /// An owner check made for one pane was offered for another.
    #[error("owner check for {checked} cannot authorise a send to {pane}")]
    WrongPane {
        /// The pane the owner check read.
        checked: String,
        /// The pane the send was for.
        pane: String,
    },
    /// A tmux call failed.
    #[error(transparent)]
    Tmux(#[from] TmuxError),
}

/// Why [`crate::self_ctx::self_ctx_tokens`] could not measure a transcript.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SelfCtxError {
    /// The chosen transcript could not be read.
    #[error("read transcript {}: {source}", path.display())]
    Io {
        /// The transcript path.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A usage field (or `usage` itself) is not what the sum needs.
    #[error("transcript usage field {0:?} is not an integer")]
    BadUsage(String),
}

/// An environment value the poller cannot use (#9171 A3a).
///
/// A bad `LOAD_FACTOR`, `PANE_CTX_WINDOW`, `SELF_CTX_WINDOW` or
/// `SELF_CTX_THRESHOLD` turns off the sensor that reads it and is listed in
/// `Config::problems` (for `LOAD_FACTOR`, Architect ruling Q4, #9171 A3b-2).
/// A bad poll interval is returned as this error before the loop starts.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("environment {name}={value:?} is not a valid {want}")]
pub struct ConfigError {
    /// The variable name, e.g. `LOAD_FACTOR`.
    pub name: &'static str,
    /// The value found.
    pub value: String,
    /// What the variable must hold (`number`, `integer`).
    pub want: &'static str,
}

/// Why `poll-state.json` could not be read or written (#9171 A3a).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StateError {
    /// The file could not be read, written, synced or renamed.
    #[error("state file {}: {source}", path.display())]
    Io {
        /// The path involved.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file holds text that is not a poller state object.
    #[error("state file {}: not a poller state: {source}", path.display())]
    Corrupt {
        /// The state file.
        path: std::path::PathBuf,
        /// The parse error.
        #[source]
        source: serde_json::Error,
    },
}

/// Why one poll cycle stopped before its alerts were written (#9171 A3b-2).
///
/// The prototype raises from `read_events` and `host_alerts` and its main
/// loop logs the exception; the port returns this, and the run commits
/// nothing from that cycle.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CycleError {
    /// `events.jsonl` exists but could not be read.
    #[error("events: {0}")]
    Events(#[source] std::io::Error),
    /// The load average or the data disk could not be read.
    #[error("host: {0}")]
    Host(#[source] std::io::Error),
}

/// Why the poll loop, or one run of it, stopped (#9171 A3b-2).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// Another poller holds `poll.lock` for this inbox.
    #[error("another poller is running: {} is locked", path.display())]
    AlreadyRunning {
        /// The lock file.
        path: std::path::PathBuf,
    },
    /// A setting the loop cannot start with (the interval).
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The cycle stopped before its alerts were written; nothing was committed.
    #[error(transparent)]
    Cycle(#[from] CycleError),
    /// A file the run writes could not be written, or a phase hook stopped it.
    #[error("{what}: {source}")]
    Io {
        /// What was being done (`append alerts`, `lock`, a phase).
        what: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The state could not be saved; the cycle's alerts are already written.
    #[error(transparent)]
    State(#[from] StateError),
    /// A single run ([`crate::poller::Runner::run_single`]) did not finish its
    /// cycle: tmux could not list panes, or the state file could not be read
    /// and so was not saved (#9822).
    #[error("cycle incomplete: {0}")]
    Incomplete(String),
    /// A single run was refused because a setting is bad: each
    /// `Config::problems` entry, with the sensor it would turn off (#9822).
    #[error("poller configuration has problems: {0}")]
    BadConfig(String),
}

/// Why the usage step produced no reading or no alerts (#9801). Each message
/// starts with the Python exception the prototype raises in the same case.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UsageError {
    /// A time that `datetime.fromisoformat` would reject, or one without a
    /// UTC offset (the prototype reads that as host-local time; the port
    /// refuses it).
    #[error("ValueError: not an ISO time with a UTC offset: {0:?}")]
    BadTime(String),
    /// A time outside `datetime`'s years 1-9999, or an infinite span.
    #[error("OverflowError: date value out of range")]
    OutOfRange,
    /// A ledger or state value of the wrong JSON type.
    #[error("TypeError: {key} is {value}, not {want}")]
    WrongType {
        /// The key read.
        key: String,
        /// The JSON found.
        value: String,
        /// What was wanted (`a number`, `a string`, `an object`).
        want: &'static str,
    },
    /// The ledger is not UTF-8.
    #[error("UnicodeDecodeError: {0} is not UTF-8")]
    NotUtf8(std::path::PathBuf),
    /// A ledger append failed.
    #[error("OSError: {what} {path}: {source}")]
    Io {
        /// What was being done.
        what: &'static str,
        /// The file.
        path: std::path::PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// Why a macOS notification was not shown (#9801).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NotifyError {
    /// `osascript` could not be started.
    #[error("osascript could not start: {0}")]
    Spawn(#[source] std::io::Error),
    /// `osascript` did not finish in time; it was killed and reaped.
    #[error("osascript timed out after {0:?}")]
    Timeout(std::time::Duration),
    /// Waiting on `osascript` failed; it was killed and reaped.
    #[error("osascript wait failed: {0}")]
    Wait(#[source] std::io::Error),
    /// `osascript` ran and exited non-zero.
    #[error("osascript exit {code:?}: {stderr}")]
    Failed {
        /// The exit code, or `None` when a signal ended it.
        code: Option<i32>,
        /// Its stderr, trimmed.
        stderr: String,
    },
}
