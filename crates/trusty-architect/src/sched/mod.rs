//! The Architect's one job scheduler (MB1595 spike).
//!
//! Why: the Architect's recurring work is spread over a Python poller, cron
//! lines, launchd and `meta_due.py`; routines lapse unseen. Bob asked for one
//! Rust scheduler that loads jobs from config (a script or an agent call) and
//! runs each on its schedule. This is the spike: the scheduler core, a shadow
//! run of the poll cycle beside the Python poller, and a cutover switch that
//! the Architect decides to throw after reading the parity report.
//! What: [`Registry`] loads `local/meta/registry.json` (the contract in
//! `local/meta/README.md`, plus optional extension fields, see
//! [`config`]). [`Engine`] ticks on a [`crate::Clock`]: it computes due
//! slots ([`slot`], [`cron`]), applies jitter, catch-up, overlap, daily cap
//! and backoff, runs each job through a [`JobRunner`] on its own thread, and
//! records state ([`state`]), a run log ([`runlog`]) and freshness
//! escalations ([`fresh`]). [`shadow`] runs the poll cycle as a dry-run
//! script job and [`parity`] joins its records to the live poller's output.
//! [`daemon`] adds the single-instance lock and the liveness file; [`cutover`]
//! moves the wake duty to the scheduler and back; [`plist`] renders the
//! launchd user agent. The `trusty-architect-sched` binary drives it ([`cli`]).
//! Test: `tests/sched_*.rs`.
//
// See MB1595

pub mod agent;
pub mod cli;
pub mod cli_args;
pub mod config;
pub mod cron;
pub mod cutover;
pub mod daemon;
pub mod engine;
pub mod engine_run;
pub mod fresh;
pub mod logger;
pub mod parity;
pub mod plist;
pub mod runlog;
pub mod shadow;
pub mod slot;
pub mod spawn;
pub mod state;
pub mod status;
pub mod validate;

use std::path::{Path, PathBuf};

pub use config::{Job, Registry};
pub use engine::Engine;
pub use spawn::{JobRunner, RunResult, RunSpec, SystemRunner};

/// Why the scheduler could not start or load its config.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SchedError {
    /// The config file is empty: a failure, never a pass (contract section 4).
    #[error("{0}: the config is empty")]
    Empty(PathBuf),
    /// A file could not be read or written.
    #[error("{what}: {source}")]
    Io {
        /// What was being done, with the path.
        what: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The config or state is not valid JSON of the expected shape.
    #[error("{what}: {detail}")]
    Parse {
        /// The file.
        what: String,
        /// The parser's message.
        detail: String,
    },
    /// The config parsed but breaks the contract; one problem per entry.
    #[error("config has {} problem(s): {}", .0.len(), .0.join("; "))]
    Invalid(Vec<String>),
    /// Another scheduler holds the lock.
    #[error("another scheduler is running: {} is locked", .0.display())]
    Locked(PathBuf),
    /// A bad command line.
    #[error("{0}")]
    Usage(String),
    /// A cutover or rollback step failed.
    #[error("{0}")]
    Cutover(String),
}

impl SchedError {
    /// The process exit code: 2 for an empty, missing or unreadable config
    /// or a usage error (as `meta_due.py`), 1 for everything else.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Empty(_) | Self::Io { .. } | Self::Usage(_) => 2,
            _ => 1,
        }
    }

    pub(crate) fn io(what: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            what: what.into(),
            source,
        }
    }
}

/// Where the scheduler reads and writes, relative to the Architect root.
///
/// Why: every path is named once, so tests point the whole scheduler at a
/// temp directory and the contract's layout lives in one place.
/// What: pure path arithmetic over `root`; nothing is created here.
/// Test: `layout_paths_follow_the_contract` (tests/sched_state.rs).
#[derive(Debug, Clone)]
pub struct Layout {
    /// The Architect project directory (cwd of script jobs).
    pub root: PathBuf,
    /// The home directory, for `~` in artifact paths.
    pub home: PathBuf,
}

impl Layout {
    /// A layout rooted at `root`.
    pub fn new(root: &Path, home: &Path) -> Self {
        Self {
            root: root.to_owned(),
            home: home.to_owned(),
        }
    }
    fn meta(&self, name: &str) -> PathBuf {
        self.root.join("local/meta").join(name)
    }
    /// `local/meta/registry.json`, the one job config.
    pub fn registry(&self) -> PathBuf {
        self.meta("registry.json")
    }
    /// `local/meta/sched-state.json`.
    pub fn state(&self) -> PathBuf {
        self.meta("sched-state.json")
    }
    /// `local/meta/runs.jsonl`.
    pub fn runs(&self) -> PathBuf {
        self.meta("runs.jsonl")
    }
    /// `local/meta/sched.alive`, the liveness file.
    pub fn alive(&self) -> PathBuf {
        self.meta("sched.alive")
    }
    /// `local/meta/sched.lock`.
    pub fn lock(&self) -> PathBuf {
        self.meta("sched.lock")
    }
    /// `local/meta/cutover.json`, the mode marker.
    pub fn cutover(&self) -> PathBuf {
        self.meta("cutover.json")
    }
    /// `local/meta/shadow/`, the scratch inbox of the shadow poll job.
    pub fn shadow_dir(&self) -> PathBuf {
        self.meta("shadow")
    }
    /// `local/meta/stamps/<id>.json`.
    pub fn stamp(&self, id: &str) -> PathBuf {
        self.meta("stamps").join(format!("{id}.json"))
    }
    /// `local/logs/jobs/<id>/<UTC date>.log`.
    pub fn job_log(&self, id: &str, date: &str) -> PathBuf {
        self.root
            .join("local/logs/jobs")
            .join(id)
            .join(format!("{date}.log"))
    }
    /// `local/usage/meta-agent.jsonl`.
    pub fn usage(&self) -> PathBuf {
        self.root.join("local/usage/meta-agent.jsonl")
    }
    /// `inbox/alerts.jsonl`.
    pub fn alerts(&self) -> PathBuf {
        self.root.join("inbox/alerts.jsonl")
    }
    /// `inbox/poll.log`.
    pub fn poll_log(&self) -> PathBuf {
        self.root.join("inbox/poll.log")
    }
    /// `inbox/shadow-wakes.jsonl`, the shadow record.
    pub fn shadow_log(&self) -> PathBuf {
        self.root.join("inbox/shadow-wakes.jsonl")
    }
    /// `inbox/cutover.log`.
    pub fn cutover_log(&self) -> PathBuf {
        self.root.join("inbox/cutover.log")
    }
}
