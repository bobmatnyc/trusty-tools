//! Per-run start and finish lines under `_sources/imports/` (SPEC-OKF-01
//! §2.14 rule 10).
//!
//! Why: an interrupted import recovers by re-running, but an operator must
//! still see that a run was killed. A start line with no finish line marks
//! the attempt as interrupted; progress itself comes from the ledger.
//! What: `<plan run ID>.jsonl` holds one [`RunLine`] per event. Each apply
//! is an attempt `<run>-<n>`, `n` the number of attempts already logged.
//! [`RunLog::start`] appends the start line, synced, right before the
//! attempt's first bundle write, so an attempt that writes nothing logs
//! nothing and a repeat import writes zero bytes (criterion 77).
//! [`RunLog::finish`] appends the finish line after the last write. A torn
//! line is skipped. Slice S8 of #10746.
//! Test: `c83_killed_run_reruns_to_the_same_bundle_and_reads_interrupted`,
//! `c77_second_import_writes_zero_bytes_and_a_change_updates`.

use serde::{Deserialize, Serialize};

use crate::OkfError;
use crate::build::jsonl::read_journal;
use crate::bundle::Bundle;
use crate::bundle::paths::{append_line, confined_file};

/// The directory of the run files, bundle-relative.
pub(crate) const IMPORTS_DIR: &str = "_sources/imports";

/// One run-file line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunLine {
    /// `start` or `finish`.
    pub rec: String,
    /// The attempt ID.
    pub attempt: String,
    /// The caller's `now`.
    pub now: String,
    /// The store ID.
    pub store: String,
    /// On `finish`: the counts `created`, `updated`, `tombstoned`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written: Option<[usize; 3]>,
}

/// One attempt's run file, with whether its start line is written.
pub(crate) struct RunLog {
    rel: String,
    attempt: String,
    store: String,
    started: bool,
}

impl RunLog {
    /// The next attempt of `run` in `b`.
    pub(crate) fn new(b: &Bundle, run: &str, store: &str) -> Result<RunLog, OkfError> {
        let rel = format!("{IMPORTS_DIR}/{run}.jsonl");
        let lines = read_journal::<RunLine>(&confined_file(b.root(), &rel)?)?.records;
        let n = lines.iter().filter(|l| l.rec == "start").count();
        Ok(RunLog {
            rel,
            attempt: format!("{run}-{n}"),
            store: store.to_string(),
            started: false,
        })
    }

    /// The attempt ID.
    pub(crate) fn attempt(&self) -> &str {
        &self.attempt
    }

    fn line(&self, rec: &str, now: &str, written: Option<[usize; 3]>) -> Result<String, OkfError> {
        let l = RunLine {
            rec: rec.to_string(),
            attempt: self.attempt.clone(),
            now: now.to_string(),
            store: self.store.clone(),
            written,
        };
        serde_json::to_string(&l).map_err(|e| OkfError::Parse {
            path: self.rel.clone().into(),
            line: None,
            message: e.to_string(),
        })
    }

    /// Append the start line once, before the attempt's first write.
    pub(crate) fn start(&mut self, b: &Bundle, now: &str) -> Result<(), OkfError> {
        if self.started {
            return Ok(());
        }
        crate::bundle::ops::io_seam("import-start")?;
        append_line(b.root(), &self.rel, &self.line("start", now, None)?)?;
        self.started = true;
        Ok(())
    }

    /// Append the finish line, when the attempt started.
    pub(crate) fn finish(
        &self,
        b: &Bundle,
        now: &str,
        written: [usize; 3],
    ) -> Result<(), OkfError> {
        if !self.started {
            return Ok(());
        }
        crate::bundle::ops::io_seam("import-finish")?;
        append_line(
            b.root(),
            &self.rel,
            &self.line("finish", now, Some(written))?,
        )
    }
}

/// Every line of every run file, oldest file name first.
pub(crate) fn all_lines(b: &Bundle) -> Result<Vec<RunLine>, OkfError> {
    let dir = confined_file(b.root(), IMPORTS_DIR)?;
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(it) => it
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".jsonl"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(crate::bundle::paths::io_err(&dir, e)),
    };
    names.sort();
    let mut out = Vec::new();
    for n in names {
        let path = confined_file(b.root(), &format!("{IMPORTS_DIR}/{n}"))?;
        out.extend(read_journal::<RunLine>(&path)?.records);
    }
    Ok(out)
}
