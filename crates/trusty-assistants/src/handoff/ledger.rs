//! The per-instance handoff ledger (#11650, slice 3c).
//!
//! Why: a handoff dispatches work to another process. A retry must not
//! dispatch it twice, and a crash between dispatch and the record of its
//! handle must never read as success. The ledger is the one record of both.
//! What: [`Ledger`] over `<home>/stores/handoffs.jsonl`, one
//! [`LedgerRecord`] per line, append-only. [`Ledger::open`] takes an exclusive
//! lock on the file and holds it until the [`Ledger`] is dropped, so a whole
//! handoff (key check, intent, dispatch, completion) runs under one lock, the
//! same `File::lock` the turn log uses. A crash releases the lock with the
//! process. [`Ledger::entries`] folds the records per key; an intent with no
//! completion after it reads as [`Outcome::OutcomeUnknown`].
//! Test: `super::tests::ledger_tests` — the whole module.

use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{HandoffError, Target};
use crate::assistants::AssistantHome;

/// File name of the ledger inside the home's `stores/` directory.
pub const LEDGER_FILE: &str = "handoffs.jsonl";

/// Record format version written by this build.
pub const LEDGER_RECORD_VERSION: u32 = 1;

/// The detail an intent with no completion reads as.
pub const NO_COMPLETION: &str = "an intent was recorded but no completion followed (the handoff stopped before a handle was recorded)";

/// One ledger line.
/// Test: `super::tests::ledger_tests::records_round_trip_through_the_file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LedgerRecord {
    /// Written and synced BEFORE the dispatch.
    Intent {
        v: u32,
        key: String,
        at: String,
        target: Target,
        task: String,
    },
    /// Written after the dispatch returned.
    Done {
        v: u32,
        key: String,
        at: String,
        result: Outcome,
    },
}

impl LedgerRecord {
    fn version(&self) -> u32 {
        match self {
            Self::Intent { v, .. } | Self::Done { v, .. } => *v,
        }
    }
}

/// What one handoff attempt produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The target accepted the work; `handle` names it there.
    Dispatched {
        handle: String,
        /// tm only: how the instance's durable session was used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tm_session: Option<TmSessionUse>,
    },
    /// The target refused, or nothing was sent. A retry may dispatch.
    Failed { error: String },
    /// The work may or may not have been dispatched. Never success.
    OutcomeUnknown { error: String },
}

/// How a tm handoff used the instance's one durable session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TmSessionUse {
    /// No session was recorded; this handoff created it.
    Created,
    /// The recorded session was live and the task was sent into it.
    Reused,
    /// The recorded session was gone; this handoff created a new one.
    Replaced { previous: String, reason: String },
}

/// One key's history, folded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    /// The target the key's first intent named.
    pub target: Target,
    /// The task text the key's first intent carried.
    pub task: String,
    /// The latest state; an open intent is [`Outcome::OutcomeUnknown`].
    pub outcome: Outcome,
}

/// One instance's ledger, locked for as long as this value lives.
/// Test: `super::tests::ledger_tests` — the whole module.
#[derive(Debug)]
pub struct Ledger {
    path: PathBuf,
    file: File,
    records: Vec<LedgerRecord>,
}

impl Ledger {
    /// The ledger path of `home`. Touches no filesystem.
    pub fn path_for(home: &AssistantHome) -> PathBuf {
        home.stores_dir().join(LEDGER_FILE)
    }

    /// Open (creating) and exclusively lock the ledger at `path`, then read it.
    ///
    /// Why: one lock across the whole handoff makes the key check and the
    /// dispatch atomic per instance, so two concurrent retries of one key
    /// dispatch once, and an open intent seen under the lock means its writer
    /// is dead, not in flight.
    /// What: creates the parent directory, opens read+append, waits for the
    /// lock on a blocking thread, then parses every line. An unterminated
    /// last line (a crash mid-append) is copied to
    /// `handoffs.jsonl.corrupt-<time>`, synced, and truncated off, with a
    /// warning; a completion lost that way leaves its key reading
    /// [`Outcome::OutcomeUnknown`]. Any other bad line, or an unknown record
    /// version, is [`HandoffError::CorruptLedger`] and the file is untouched.
    /// Test: `super::tests::ledger_tests::a_torn_last_line_is_quarantined`,
    /// `super::tests::ledger_tests::a_bad_middle_line_is_an_error`,
    /// `super::tests::ledger_tests::concurrent_appends_keep_every_line_whole`.
    pub async fn open(path: &Path) -> Result<Self, HandoffError> {
        let path = path.to_path_buf();
        let locked = path.clone();
        let file = tokio::task::spawn_blocking(move || -> std::io::Result<File> {
            if let Some(dir) = locked.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .append(true)
                .create(true)
                .open(&locked)?;
            file.lock()?;
            Ok(file)
        })
        .await
        .map_err(|e| ledger_io(&path, std::io::Error::other(e)))?
        .map_err(|e| ledger_io(&path, e))?;
        let mut ledger = Self {
            path,
            file,
            records: Vec::new(),
        };
        ledger.load()?;
        Ok(ledger)
    }

    /// The ledger file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&mut self) -> Result<(), HandoffError> {
        let mut raw = Vec::new();
        (&self.file)
            .read_to_end(&mut raw)
            .map_err(|e| ledger_io(&self.path, e))?;
        let mut offset = 0;
        let mut line = 0;
        while offset < raw.len() {
            line += 1;
            let Some(len) = raw[offset..].iter().position(|b| *b == b'\n') else {
                return self.quarantine_tail(&raw, offset, line);
            };
            let record = std::str::from_utf8(&raw[offset..offset + len])
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    serde_json::from_str::<LedgerRecord>(text).map_err(|e| e.to_string())
                })
                .and_then(|r| match r.version() {
                    LEDGER_RECORD_VERSION => Ok(r),
                    v => Err(format!("unknown record version {v}")),
                })
                .map_err(|reason| HandoffError::CorruptLedger {
                    path: self.path.clone(),
                    line,
                    reason,
                })?;
            self.records.push(record);
            offset += len + 1;
        }
        Ok(())
    }

    /// Move an unterminated last line aside and truncate it off.
    fn quarantine_tail(
        &mut self,
        raw: &[u8],
        offset: usize,
        line: usize,
    ) -> Result<(), HandoffError> {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".corrupt-{stamp}"));
        let quarantine = PathBuf::from(name);
        let io = |e| ledger_io(&self.path, e);
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&quarantine)
            .map_err(io)?;
        out.write_all(&raw[offset..])
            .and_then(|()| out.sync_all())
            .map_err(io)?;
        self.file
            .set_len(offset as u64)
            .and_then(|()| self.file.sync_all())
            .map_err(io)?;
        tracing::warn!(
            path = %self.path.display(),
            line,
            quarantine = %quarantine.display(),
            "handoff: the ledger's last line was unterminated; moved it aside and truncated it",
        );
        Ok(())
    }

    /// Append `record` as one line and sync it before returning.
    ///
    /// What: one `write_all` on the locked append-only handle, then
    /// `sync_data`. Earlier lines are never touched. On error the record must
    /// be treated as not written.
    /// Test: `super::tests::ledger_tests::records_round_trip_through_the_file`.
    pub fn append(&mut self, record: LedgerRecord) -> Result<(), HandoffError> {
        let mut line = serde_json::to_string(&record)
            .map_err(|e| ledger_io(&self.path, std::io::Error::new(ErrorKind::InvalidData, e)))?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.sync_data())
            .map_err(|e| ledger_io(&self.path, e))?;
        self.records.push(record);
        Ok(())
    }

    /// Every key's folded history.
    ///
    /// What: walks the records in order. A key's target and task come from
    /// its first intent; its outcome is its latest completion, or
    /// [`Outcome::OutcomeUnknown`] with [`NO_COMPLETION`] when an intent is
    /// the last record for it. A completion with no intent before it is
    /// ignored.
    /// Test: `super::tests::ledger_tests::an_intent_without_completion_reads_outcome_unknown`.
    pub fn entries(&self) -> HashMap<String, LedgerEntry> {
        let mut out: HashMap<String, LedgerEntry> = HashMap::new();
        for record in &self.records {
            match record {
                LedgerRecord::Intent {
                    key, target, task, ..
                } => {
                    let open = Outcome::OutcomeUnknown {
                        error: NO_COMPLETION.to_string(),
                    };
                    out.entry(key.clone())
                        .and_modify(|e| e.outcome = open.clone())
                        .or_insert_with(|| LedgerEntry {
                            target: *target,
                            task: task.clone(),
                            outcome: open,
                        });
                }
                LedgerRecord::Done { key, result, .. } => {
                    if let Some(entry) = out.get_mut(key) {
                        entry.outcome = result.clone();
                    }
                }
            }
        }
        out
    }

    /// The instance's durable tm session: the handle of the latest
    /// dispatched tm handoff, if any.
    ///
    /// Why: #11650 — tm reuses one durable session per instance; the ledger
    /// is where its id is recorded, so there is no second file to drift.
    /// Test: `super::tests::flow_tests::the_second_tm_handoff_reuses_the_session`.
    pub fn tm_session(&self) -> Option<String> {
        let mut targets: HashMap<&str, Target> = HashMap::new();
        let mut current = None;
        for record in &self.records {
            match record {
                LedgerRecord::Intent { key, target, .. } => {
                    targets.entry(key.as_str()).or_insert(*target);
                }
                LedgerRecord::Done {
                    key,
                    result: Outcome::Dispatched { handle, .. },
                    ..
                } if targets.get(key.as_str()) == Some(&Target::Tm) => {
                    current = Some(handle.clone());
                }
                LedgerRecord::Done { .. } => {}
            }
        }
        current
    }
}

fn ledger_io(path: &Path, source: std::io::Error) -> HandoffError {
    HandoffError::LedgerIo {
        path: path.to_path_buf(),
        source,
    }
}

/// The current time as RFC 3339, for record stamps.
pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
