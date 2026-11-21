//! The run log: one JSONL line per run, skip or overrun.
//!
//! Why: the contract names `local/meta/runs.jsonl` as the record of every
//! run: `{ts, job, slot, start, end, exit, outcome}`.
//! What: [`RunRecord`] is that line; [`append_jsonl`] writes any serializable
//! value as one `write_all` in append mode (a reader never sees half a line
//! from one writer), creating the directory first.
//! Test: `due_slot_runs_once_and_logs_one_line` (tests/sched_engine.rs).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use super::SchedError;
use crate::classify::alerts::iso;

/// One `runs.jsonl` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunRecord {
    /// When the line was written, UTC ISO.
    pub ts: String,
    /// The job id.
    pub job: String,
    /// The slot, Unix seconds.
    pub slot: i64,
    /// Start, Unix seconds (equal to `end` for a skip).
    pub start: i64,
    /// End, Unix seconds.
    pub end: i64,
    /// The exit code; `None` when killed, or never started.
    pub exit: Option<i32>,
    /// `ok`, `fail`, `timeout`, `overrun` or `skipped`.
    pub outcome: &'static str,
}

impl RunRecord {
    /// A record for a run that started at `start` and ended at `end`.
    pub fn new(
        job: &str,
        slot: i64,
        start: i64,
        end: i64,
        exit: Option<i32>,
        outcome: &'static str,
    ) -> Self {
        Self {
            ts: iso(end as f64),
            job: job.to_owned(),
            slot,
            start,
            end,
            exit,
            outcome,
        }
    }
}

/// Appends `value` to `path` as one JSON line.
///
/// # Errors
/// [`SchedError::Io`] when the file cannot be opened or written.
pub fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<(), SchedError> {
    let io = |e| SchedError::io(format!("append {}", path.display()), e);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let mut line = serde_json::to_string(value).map_err(|e| SchedError::Parse {
        what: path.display().to_string(),
        detail: e.to_string(),
    })?;
    line.push('\n');
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(io)?;
    f.write_all(line.as_bytes()).map_err(io)
}
