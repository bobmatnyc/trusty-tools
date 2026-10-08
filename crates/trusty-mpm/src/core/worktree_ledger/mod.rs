//! The machine-wide, append-only worktree ledger (#8994 slice 1).
//!
//! Why: no machine-wide record of worktrees existed, so none could be
//! reclaimed or even counted on evidence — adaptive-crm reached 84 worktrees
//! and a 93% data volume before anyone could say which project owned what.
//! This file is that record: `~/.trusty-mpm/worktrees.jsonl`, one JSON event
//! per line, written by every tm provisioning route and by the backfill.
//!
//! What: [`LedgerEvent`] (`created` / `observed` / `measured` / `removed`),
//! [`WorktreeLedger`] (append and read), and the submodules that build on it —
//! [`fold`] (current state as a PURE fold over the events), [`record`] (the
//! provisioning hook and the size measurement) and [`backfill`] (registering
//! trees that predate the ledger). An append error is RETURNED, never logged
//! and swallowed: a provisioning route that cannot record refuses instead.
//!
//! Scope (slice 1): nothing here removes a worktree. [`reconcile`] appends a
//! `removed` event for a tree already gone from disk and from
//! `git worktree list` — it observes, it never deletes. The ledger feeds
//! `merged_pr_reclaim` and `agent_worktree_reap` in a later slice and replaces
//! neither.
//!
//! Concurrency: the daemon and any `tm` process append. Each event is one
//! `write` of one complete line to a file opened `O_APPEND`, so concurrent
//! appenders interleave whole lines. A torn trailing line (a crash or a short
//! write) is counted in [`LedgerRead::malformed`], never silently dropped: the
//! next appender starts its first line with `\n` so nothing is concatenated
//! onto the fragment, and the read parses each line's bytes on its own.
//! Test: `worktree_ledger::tests`.

pub mod backfill;
pub mod fold;
pub mod reconcile;
pub mod record;

#[cfg(test)]
mod tests;

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// File name of the ledger under the framework root (`~/.trusty-mpm`).
pub const LEDGER_FILE: &str = "worktrees.jsonl";

/// Which route put a worktree into the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The daemon's in-project spawn (`reserve_inproject_worktree`).
    TmDaemon,
    /// The CLI's in-process provisioning (`tm launch --worktree`, guided fallback).
    TmCli,
    /// A pre-existing tree registered by [`backfill`].
    Backfill,
    /// A Claude Code agent-isolation tree (`.claude/worktrees/<name>`).
    AgentIsolation,
}

/// What happened to the worktree at [`LedgerEvent::path`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    /// tm created the tree, or the backfill found a tree tm owns.
    Created {
        /// Main checkout (or base clone) the tree belongs to.
        repo: PathBuf,
        /// Branch checked out in the tree, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// The route that recorded it.
        origin: Origin,
        /// Managed session the tree was created for, when there is one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// A tree tm did not create and does not manage (agent isolation).
    Observed {
        /// Main checkout the tree belongs to.
        repo: PathBuf,
        /// Branch checked out in the tree, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// The route that recorded it.
        origin: Origin,
    },
    /// The tree's allocated size at [`LedgerEvent::ts`].
    Measured {
        /// Bytes allocated on disk, as `du` counts them.
        bytes: u64,
    },
    /// The tree is gone from disk and from `git worktree list`
    /// ([`reconcile`] writes it).
    Removed,
}

/// One line of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEvent {
    /// When the event was recorded.
    pub ts: DateTime<Utc>,
    /// The worktree, canonicalized when it existed at record time.
    pub path: PathBuf,
    /// The event itself.
    #[serde(flatten)]
    pub kind: EventKind,
}

impl LedgerEvent {
    /// An event for `path` stamped with the current time.
    pub fn now(path: PathBuf, kind: EventKind) -> Self {
        Self {
            ts: Utc::now(),
            path,
            kind,
        }
    }
}

/// Why a ledger operation failed.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// No absolute home directory, so there is no `~/.trusty-mpm` to write to.
    #[error("worktree ledger: no absolute home directory to hold {LEDGER_FILE}")]
    NoHome,
    /// The ledger file or its directory could not be opened, written or read.
    #[error("worktree ledger {path}: {source}")]
    Io {
        /// The ledger path.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// An event could not be encoded as JSON.
    #[error("worktree ledger: cannot encode event: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Everything read back from the ledger file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerRead {
    /// `false` when the file does not exist yet.
    pub present: bool,
    /// Every parseable event, in file order.
    pub events: Vec<LedgerEvent>,
    /// Non-empty lines that did not parse (a torn write, a future event kind).
    pub malformed: usize,
}

/// The ledger file at one path.
///
/// Why: production writes `~/.trusty-mpm/worktrees.jsonl`; every test names a
/// scratch path instead, so no test can append to the operator's ledger.
/// What: a path plus append/read. Holds no state of its own.
/// Test: `append_then_read_round_trips_every_event_kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeLedger {
    path: PathBuf,
}

impl WorktreeLedger {
    /// The ledger at an explicit file path.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The ledger under `home`: `<home>/.trusty-mpm/worktrees.jsonl`.
    pub fn under_home(home: &Path) -> Self {
        Self::at(
            home.join(crate::core::paths::FRAMEWORK_DIR_NAME)
                .join(LEDGER_FILE),
        )
    }

    /// The operator's ledger, resolved from the home directory.
    ///
    /// Why: a missing home must refuse, not fall back to the working directory
    /// the way `FrameworkPaths::home_base` does (#7290).
    /// What: [`Self::under_home`] of an absolute `dirs::home_dir()`, else
    /// [`LedgerError::NoHome`].
    /// Test: covered through `reserve_inproject_worktree_refuses_when_the_ledger_cannot_record`.
    pub fn host() -> Result<Self, LedgerError> {
        dirs::home_dir()
            .filter(|h| h.is_absolute())
            .map(|h| Self::under_home(&h))
            .ok_or(LedgerError::NoHome)
    }

    /// The ledger file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn io(&self, source: std::io::Error) -> LedgerError {
        LedgerError::Io {
            path: self.path.clone(),
            source,
        }
    }

    /// Open the ledger for appending, creating it and its directory.
    ///
    /// Why: the provisioning hook opens BEFORE it creates a tree, so a ledger
    /// that cannot be written refuses the creation with nothing on disk.
    /// What: `create_dir_all` on the parent, then an `O_APPEND` open. When the
    /// file is non-empty and its last byte is not `\n` (a torn line from a
    /// short write), the appender's first write starts with `\n`.
    /// Test: `create_recorded_refuses_before_creating_when_the_ledger_is_unwritable`,
    /// `an_append_after_a_torn_line_with_no_newline_keeps_the_new_event`.
    pub fn open_appender(&self) -> Result<LedgerAppender, LedgerError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| self.io(e))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| self.io(e))?;
        // #8994: without this guard the next line is concatenated onto a torn
        // fragment and the new event is lost from the fold.
        let needs_newline = ends_without_newline(&mut file).map_err(|e| self.io(e))?;
        Ok(LedgerAppender {
            ledger: self.clone(),
            file,
            needs_newline,
        })
    }

    /// Append one event.
    pub fn append(&self, event: &LedgerEvent) -> Result<(), LedgerError> {
        self.open_appender()?.append(event)
    }

    /// Read every event.
    ///
    /// Why: the fold, `tm worktrees` and the doctor row all start here.
    /// What: an absent file reads as `present: false` with no events; any other
    /// I/O failure is an error. The file is read as bytes and each line parsed
    /// on its own, so an unparseable or non-UTF-8 line is counted in
    /// [`LedgerRead::malformed`] and never fails the whole read.
    /// Test: `read_counts_a_torn_line_without_losing_the_rest`,
    /// `a_torn_line_cut_inside_a_multibyte_path_leaves_the_ledger_readable`,
    /// `read_of_an_absent_ledger_is_empty_not_an_error`.
    pub fn read(&self) -> Result<LedgerRead, LedgerError> {
        // #8994: bytes, not `read_to_string` — one torn multibyte path must not
        // make the whole ledger unreadable.
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LedgerRead::default());
            }
            Err(e) => return Err(self.io(e)),
        };
        let mut read = LedgerRead {
            present: true,
            ..LedgerRead::default()
        };
        for line in bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.trim_ascii().is_empty())
        {
            match serde_json::from_slice::<LedgerEvent>(line) {
                Ok(event) => read.events.push(event),
                Err(_) => read.malformed += 1,
            }
        }
        Ok(read)
    }
}

/// `true` when `file` is non-empty and its last byte is not `\n`.
fn ends_without_newline(file: &mut File) -> std::io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1))?;
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

/// An open handle on the ledger, for one or more appends.
#[derive(Debug)]
pub struct LedgerAppender {
    ledger: WorktreeLedger,
    file: File,
    /// The file ends in a torn line, so the next write must start with `\n`.
    needs_newline: bool,
}

impl LedgerAppender {
    /// Append one event as one complete line in a single write.
    ///
    /// What: when the file ends in a torn line (found on open, or left by a
    /// failed write on this handle) the line is prefixed with `\n`, so it
    /// starts a line of its own.
    /// Test: `an_append_after_a_torn_line_with_no_newline_keeps_the_new_event`.
    pub fn append(&mut self, event: &LedgerEvent) -> Result<(), LedgerError> {
        let mut line = Vec::new();
        if self.needs_newline {
            line.push(b'\n');
        }
        line.extend(serde_json::to_vec(event)?);
        line.push(b'\n');
        let written = self.file.write_all(&line).and_then(|()| self.file.flush());
        // #8994: a failed write may itself leave a torn line behind.
        self.needs_newline = written.is_err();
        written.map_err(|e| self.ledger.io(e))
    }
}

/// `path` canonicalized, or unchanged when it cannot be resolved.
///
/// Why: the provisioning route and the backfill must key one tree identically,
/// or the backfill re-registers what provisioning already recorded (macOS
/// `/var` → `/private/var` is the usual divergence).
pub fn ledger_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
