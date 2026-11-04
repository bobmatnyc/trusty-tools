//! Recovery of interrupted operations (SPEC-OKF-01 §4.11 "Recovery on
//! open", rule 2, `Bundle::recover`).
//!
//! Why: a crash inside a multi-file operation must leave a bundle that the
//! next open restores to the state before or after it, losing no
//! verification event and leaving no link to a missing concept; a file
//! changed out of band after the crash must keep its bytes.
//! What: [`recover_on_open`] reads the journal (a torn intent counts as
//! absent), classifies each file step of every incomplete intent by its disk
//! byte hash (pre, post or other), and, with no step at other, rolls the
//! operation in its kind's direction (`Migrate` back, every other forward),
//! appends each missing line, writes the completion record and removes the
//! staging directory. A step at other is a `recovery_conflict`: no file
//! changes and writes to the operation's paths return `RecoveryPending`
//! until [`Bundle::recover`], which first copies each changed file under
//! `conflict/`. Leftover staging directories and ingest log batches are then
//! finished or removed. Every step is idempotent, so a crash during recovery
//! is finished by the next open (criterion 108).
//! Test: `c106_crash_at_every_boundary_rolls_forward_byte_identical`,
//! `c108_crash_during_recovery_and_torn_intent`,
//! `c109_out_of_band_edit_after_crash_is_a_recovery_conflict`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::OkfError;
use crate::build::jsonl::read_journal;
use crate::bundle::Bundle;
use crate::bundle::log_md;
use crate::bundle::ops::{
    JOURNAL_REL, OPS_DIR, OpKind, Record, RecoveryDirection, Step, append_record, disk_hash,
    failpoint, put_file, remove_staging, staged_rel,
};
use crate::bundle::paths::{append_line, confined_file, io_err, read_rel};
use crate::bundle::record::{Line, fold};
use crate::format::validate::{Finding, FindingKind, Severity};

/// What `Bundle::recover` did (SPEC-OKF-01 §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// The operation.
    pub op_id: String,
    /// Its kind.
    pub kind: OpKind,
    /// The direction taken; `None` when nothing ran.
    pub direction: Option<RecoveryDirection>,
    /// Files changed out of band, copied under `conflict/` first.
    pub conflicts: Vec<PathBuf>,
}

/// An incomplete intent.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    pub(crate) op: String,
    kind: OpKind,
    steps: Vec<Step>,
}

/// The journal: incomplete intents in order, and completed op IDs.
pub(crate) fn read_ops(b: &Bundle) -> Result<(Vec<Pending>, BTreeSet<String>), OkfError> {
    let path = confined_file(b.root(), JOURNAL_REL)?;
    let mut intents: Vec<Pending> = Vec::new();
    let mut done = BTreeSet::new();
    for r in read_journal::<Record>(&path)?.records {
        match r {
            Record::Intent {
                op, kind, steps, ..
            } => intents.push(Pending { op, kind, steps }),
            Record::Complete { op, .. } => {
                done.insert(op);
            }
        }
    }
    intents.retain(|p| !done.contains(&p.op));
    Ok((intents, done))
}

fn file_steps(p: &Pending) -> impl Iterator<Item = (&String, &Option<String>, &Option<String>)> {
    p.steps.iter().filter_map(|s| match s {
        Step::File { path, pre, post } => Some((path, pre, post)),
        Step::Append { .. } => None,
    })
}

/// The file-step paths at neither pre nor post.
fn others(b: &Bundle, p: &Pending) -> Result<Vec<String>, OkfError> {
    let mut out = Vec::new();
    for (path, pre, post) in file_steps(p) {
        let h = disk_hash(b, path)?;
        if &h != pre && &h != post {
            out.push(path.clone());
        }
    }
    Ok(out)
}

/// Whether `file` holds a line written by `op` at `step`.
fn has_line(b: &Bundle, file: &str, op: &str, step: usize) -> Result<bool, OkfError> {
    let Some(bytes) = read_rel(b.root(), file)? else {
        return Ok(false);
    };
    Ok(bytes.split(|c| *c == b'\n').any(|l| {
        serde_json::from_slice::<serde_json::Value>(l).is_ok_and(|v| {
            v.get("op").and_then(|x| x.as_str()) == Some(op)
                && v.get("step").and_then(|x| x.as_u64()) == Some(step as u64)
        })
    }))
}

/// The staged bytes of `path` on `side`.
fn staged(b: &Bundle, op: &str, side: &str, path: &str) -> Result<Vec<u8>, OkfError> {
    let rel = staged_rel(op, side, path);
    read_rel(b.root(), &rel)?.ok_or_else(|| OkfError::Io {
        path: PathBuf::from(rel),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "staged file missing"),
    })
}

/// A verification-record line appended by `op`, restored to the event's
/// state before `op`, for a roll back. `None` when nothing needs restoring.
fn compensation(b: &Bundle, file: &str, op: &str, line: &str) -> Result<Option<Line>, OkfError> {
    let Ok(appended) = serde_json::from_str::<Line>(line) else {
        return Ok(None);
    };
    let bytes = read_rel(b.root(), file)?.unwrap_or_default();
    let before: Vec<Line> = bytes
        .split(|c| *c == b'\n')
        .filter(|l| {
            serde_json::from_slice::<serde_json::Value>(l)
                .is_ok_and(|v| v.get("op").and_then(|x| x.as_str()) != Some(op))
        })
        .filter_map(|l| serde_json::from_slice::<Line>(l).ok())
        .collect();
    let prior = fold(before)
        .into_iter()
        .find(|l| l.by == appended.by && l.at == appended.at);
    Ok(Some(prior.unwrap_or(Line {
        invalidated_at: None,
        invalidated_by_hash: None,
        reason: Some("rolled_back".to_string()),
        ..appended
    })))
}

/// Bring every step of `p` to `dir`, without a pre-image check.
fn apply(b: &mut Bundle, p: &Pending, dir: RecoveryDirection) -> Result<(), OkfError> {
    match dir {
        RecoveryDirection::RollForward => {
            for (i, s) in p.steps.iter().enumerate() {
                match s {
                    Step::File { path, post, .. } => {
                        if &disk_hash(b, path)? != post {
                            let bytes = match post {
                                Some(_) => Some(staged(b, &p.op, "new", path)?),
                                None => None,
                            };
                            put_file(b, path, bytes.as_deref())?;
                        }
                    }
                    Step::Append { file, line } => {
                        if !has_line(b, file, &p.op, i)? {
                            append_line(b.root(), file, line)?;
                        }
                    }
                }
                failpoint(&format!("recover-step-{i}"))?;
            }
        }
        RecoveryDirection::RollBack => {
            for (i, s) in p.steps.iter().enumerate().rev() {
                match s {
                    Step::File { path, pre, .. } => {
                        if &disk_hash(b, path)? != pre {
                            let bytes = match pre {
                                Some(_) => Some(staged(b, &p.op, "old", path)?),
                                None => None,
                            };
                            put_file(b, path, bytes.as_deref())?;
                        }
                    }
                    Step::Append { file, line } if file.starts_with("_sources/verification/") => {
                        if has_line(b, file, &p.op, i)?
                            && let Some(restore) = compensation(b, file, &p.op, line)?
                        {
                            let json =
                                serde_json::to_string(&restore).map_err(|e| OkfError::Parse {
                                    path: PathBuf::from(file),
                                    line: None,
                                    message: e.to_string(),
                                })?;
                            append_line(b.root(), file, &json)?;
                        }
                    }
                    // A revision line stays: it describes bytes that were
                    // written; the save axis counts only matching bytes.
                    Step::Append { .. } => {}
                }
                failpoint(&format!("recover-step-{i}"))?;
            }
        }
    }
    append_record(
        b,
        &Record::Complete {
            op: p.op.clone(),
            direction: Some(dir),
        },
    )?;
    remove_staging(b, &p.op)?;
    b.clear_conflict(&p.op);
    Ok(())
}

/// Roll back an operation that failed in-process (see `ops::run`).
pub(crate) fn roll_back_now(
    b: &mut Bundle,
    op: &str,
    kind: OpKind,
    steps: &[Step],
) -> Result<(), OkfError> {
    let p = Pending {
        op: op.to_string(),
        kind,
        steps: steps.to_vec(),
    };
    apply(b, &p, RecoveryDirection::RollBack)
}

fn conflict_finding(op: &str, paths: &[String]) -> Finding {
    Finding::new(
        Severity::Warning,
        FindingKind::RecoveryConflict,
        paths.first().cloned().unwrap_or_default(),
        format!(
            "operation {op} is incomplete and these files changed outside trusty-okf: {}; \
             writes to its paths return RecoveryPending until Bundle::recover",
            paths.join(", ")
        ),
        "SPEC-OKF-01 §4.11",
    )
}

/// Recovery run by `Bundle::open` and the build layer (see the module doc).
pub(crate) fn recover_on_open(b: &mut Bundle) -> Result<Vec<Finding>, OkfError> {
    let mut findings = Vec::new();
    let ops = confined_file(b.root(), OPS_DIR)?;
    if !ops.is_dir() {
        return Ok(findings);
    }
    let (pending, _) = read_ops(b)?;
    let mut kept = BTreeSet::new();
    for p in &pending {
        let other = others(b, p)?;
        if other.is_empty() {
            let dir = if p.kind == OpKind::Migrate {
                RecoveryDirection::RollBack
            } else {
                RecoveryDirection::RollForward
            };
            apply(b, p, dir)?;
        } else {
            let paths: Vec<String> = file_steps(p).map(|(path, _, _)| path.clone()).collect();
            b.mark_conflict(&p.op, &paths);
            findings.push(conflict_finding(&p.op, &other));
            kept.insert(p.op.clone());
        }
    }
    // Staging with no live intent: a torn intent, or a completed op whose
    // cleanup did not run. Ingest log batches are finished.
    let entries = std::fs::read_dir(&ops).map_err(|e| io_err(&ops, e))?;
    let mut batches = BTreeMap::new();
    for e in entries {
        let e = e.map_err(|e| io_err(&ops, e))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(run) = name
            .strip_prefix("log-")
            .and_then(|r| r.strip_suffix(".jsonl"))
        {
            batches.insert(run.to_string(), ());
        } else if e.path().is_dir() && !kept.contains(&name) {
            remove_staging(b, &name)?;
        }
    }
    // Read again: the loop above completed operations, a batch's included.
    let (_, done) = read_ops(b)?;
    for run in batches.keys() {
        log_md::LogBatch::resume(run).flush(b, &done)?;
    }
    Ok(findings)
}

impl Bundle {
    /// Resolve a conflicted operation by explicit choice (SPEC-OKF-01 §4.11
    /// rule 2).
    ///
    /// Why: recovery never guesses over an out-of-band edit. What: copies
    /// each file step at neither pre nor post to
    /// `_sources/ops/<op-id>/conflict/<path>.okf-staged`, then brings every
    /// step to `dir` with no pre-image check, writes the completion record,
    /// removes the staging directory and clears the conflict marker. An op
    /// ID with no incomplete intent is `NotFound`-like `Parse`.
    /// Test: `c109_out_of_band_edit_after_crash_is_a_recovery_conflict`,
    /// `c126_out_of_band_edit_mid_move_stops_with_recovery_conflict`.
    pub fn recover(
        &mut self,
        op_id: &str,
        dir: RecoveryDirection,
    ) -> Result<RecoveryReport, OkfError> {
        let (pending, _) = read_ops(self)?;
        let Some(p) = pending.into_iter().find(|p| p.op == op_id) else {
            return Err(OkfError::Parse {
                path: PathBuf::from(JOURNAL_REL),
                line: None,
                message: format!("no incomplete operation {op_id}"),
            });
        };
        let other = others(self, &p)?;
        for path in &other {
            if let Some(bytes) = read_rel(self.root(), path)? {
                put_file(self, &staged_rel(&p.op, "conflict", path), Some(&bytes))?;
            }
        }
        apply(self, &p, dir)?;
        Ok(RecoveryReport {
            op_id: p.op.clone(),
            kind: p.kind,
            direction: Some(dir),
            conflicts: other.into_iter().map(PathBuf::from).collect(),
        })
    }
}
