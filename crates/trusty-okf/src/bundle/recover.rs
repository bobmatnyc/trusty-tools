//! Recovery of interrupted operations (SPEC-OKF-01 §4.11 "Recovery on
//! open", rule 2, `Bundle::recover`).
//!
//! Why: a crash inside a multi-file operation must leave a bundle that the
//! next open restores to the state before or after it, losing no
//! verification event and leaving no link to a missing concept; a file
//! changed out of band after the crash must keep its bytes.
//! #10746 S5 r2: without the §4.4 lock, `Bundle::open` only runs
//! [`scan_interrupted`] (report and refuse, change nothing), because an
//! incomplete intent may belong to a live writer; recovery runs in
//! `Bundle::recover_interrupted` and `Bundle::recover`, whose caller is the
//! only writer.
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
//! is finished by the next recovery (criterion 108).
//! Test: `c106_crash_at_every_boundary_rolls_forward_byte_identical`,
//! `c108_crash_during_recovery_and_torn_intent`,
//! `c109_out_of_band_edit_after_crash_is_a_recovery_conflict`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::OkfError;
use crate::build::jsonl::read_journal;
use crate::bundle::Bundle;
use crate::bundle::log_md;
use crate::bundle::ops::{
    JOURNAL_REL, OPS_DIR, OpKind, Record, RecoveryDirection, Step, append, append_record,
    disk_hash, failpoint, put_file, remove_staging, staged_rel,
};
use crate::bundle::paths::{confined_file, io_err, read_rel};
use crate::bundle::record::{Line, ROLLED_BACK, fold};
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

/// The newest line `op` wrote at `step` in `file`: `None` when absent,
/// `Some(true)` when it is a roll-back compensation, `Some(false)` when it is
/// the step's own line.
fn line_state(b: &Bundle, file: &str, op: &str, step: usize) -> Result<Option<bool>, OkfError> {
    let Some(bytes) = read_rel(b.root(), file)? else {
        return Ok(None);
    };
    let mut state = None;
    for l in bytes.split(|c| *c == b'\n') {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(l)
            && v.get("op").and_then(|x| x.as_str()) == Some(op)
            && v.get("step").and_then(|x| x.as_u64()) == Some(step as u64)
        {
            state = Some(v.get(ROLLBACK_KEY).is_some());
        }
    }
    Ok(state)
}

/// The key that marks a roll-back compensation line.
const ROLLBACK_KEY: &str = "rollback";

/// The staged bytes of `path` on `side`.
fn staged(b: &Bundle, op: &str, side: &str, path: &str) -> Result<Vec<u8>, OkfError> {
    let rel = staged_rel(op, side, path);
    read_rel(b.root(), &rel)?.ok_or_else(|| OkfError::Io {
        path: PathBuf::from(rel),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "staged file missing"),
    })
}

/// The compensation line that undoes a verification-record line `op`
/// appended at `step` (#10746 S5 r2).
///
/// What: the event's newest state before `op` (lines of other operations
/// only), or, when `op` introduced the event, a `rolled_back` line that
/// `record::fold` drops, so the record reads exactly as before `op`. The line
/// carries `op`, `step` and the roll-back key, so a later roll forward knows
/// to append the step's own line again. `None` for a line that is not a
/// record line.
fn compensation(
    b: &Bundle,
    file: &str,
    op: &str,
    step: usize,
    line: &str,
) -> Result<Option<String>, OkfError> {
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
    let restore = prior.unwrap_or(Line {
        invalidated_at: None,
        invalidated_by_hash: None,
        reason: Some(ROLLED_BACK.to_string()),
        ..appended
    });
    let mut v = restore.value()?;
    if let serde_json::Value::Object(m) = &mut v {
        m.insert("op".into(), op.into());
        m.insert("step".into(), step.into());
        m.insert(ROLLBACK_KEY.into(), true.into());
    }
    Ok(Some(v.to_string()))
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
                        // Absent, or undone by an interrupted roll back:
                        // the step's own line goes in (again).
                        if line_state(b, file, &p.op, i)? != Some(false) {
                            append(b, file, line)?;
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
                        if line_state(b, file, &p.op, i)? == Some(false)
                            && let Some(restore) = compensation(b, file, &p.op, i, line)?
                        {
                            append(b, file, &restore)?;
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

/// What a handle opened without the §4.4 lock does with the journal: it
/// reads it and changes nothing (#10746 S5 r2).
///
/// Why: without the lock an incomplete intent may be a live writer's op in
/// flight, so neither recovery nor staging cleanup is safe. What: each
/// incomplete operation's file-step paths are marked on this handle (a write
/// to them returns `RecoveryPending`), and each is reported: a
/// `recovery_conflict` when a file step is at neither pre- nor post-image,
/// else `interrupted_operation`. `Bundle::recover_interrupted` recovers.
/// Test: `concurrent_open_never_recovers_a_live_op`,
/// `c109_out_of_band_edit_after_crash_is_a_recovery_conflict`.
pub(crate) fn scan_interrupted(b: &mut Bundle) -> Result<Vec<Finding>, OkfError> {
    let mut findings = Vec::new();
    if !confined_file(b.root(), OPS_DIR)?.is_dir() {
        return Ok(findings);
    }
    let (pending, _) = read_ops(b)?;
    for p in &pending {
        let paths: Vec<String> = file_steps(p).map(|(path, _, _)| path.clone()).collect();
        b.mark_conflict(&p.op, &paths);
        let other = others(b, p)?;
        if other.is_empty() {
            findings.push(Finding::new(
                Severity::Warning,
                FindingKind::InterruptedOperation,
                paths.first().cloned().unwrap_or_default(),
                format!(
                    "operation {} is incomplete; Bundle::recover_interrupted finishes it, \
                     and writes to its paths return RecoveryPending until then",
                    p.op
                ),
                "SPEC-OKF-01 §4.11",
            ));
        } else {
            findings.push(conflict_finding(&p.op, &other));
        }
    }
    Ok(findings)
}

/// Full recovery (§4.11 "Recovery on open"); the caller is the only writer.
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
    // cleanup did not run.
    let entries = std::fs::read_dir(&ops).map_err(|e| io_err(&ops, e))?;
    for e in entries {
        let e = e.map_err(|e| io_err(&ops, e))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if e.path().is_dir() && !kept.contains(&name) {
            remove_staging(b, &name)?;
        }
    }
    finish_batches(b)?;
    Ok(findings)
}

/// Write every leftover ingest log batch. A batch whose `log.md` write is
/// refused with `RecoveryPending` stays for a later recovery (§4.11 rule 2);
/// any other error fails.
fn finish_batches(b: &mut Bundle) -> Result<(), OkfError> {
    let ops = confined_file(b.root(), OPS_DIR)?;
    let mut runs = Vec::new();
    for e in std::fs::read_dir(&ops).map_err(|e| io_err(&ops, e))? {
        let name = e.map_err(|e| io_err(&ops, e))?.file_name();
        let name = name.to_string_lossy();
        if let Some(run) = name
            .strip_prefix("log-")
            .and_then(|r| r.strip_suffix(".jsonl"))
        {
            runs.push(run.to_string());
        }
    }
    runs.sort();
    let (_, done) = read_ops(b)?;
    for run in runs {
        match log_md::LogBatch::resume(&run).flush(b, &done) {
            Ok(_) | Err(OkfError::RecoveryPending { .. }) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

impl Bundle {
    /// Recover every interrupted operation (SPEC-OKF-01 §4.11 "Recovery on
    /// open").
    ///
    /// Why: #10746 S5 r2 — without the §4.4 lock, `open` cannot tell a
    /// crashed operation from a live writer's, so it only reports them. What:
    /// rolls each incomplete operation whose files are all at pre- or
    /// post-image in its kind's direction, reports the others as
    /// `recovery_conflict`, removes staging with no live intent, and writes
    /// leftover ingest log batches. The caller must be the only writer.
    /// Test: `c106_crash_at_every_boundary_rolls_forward_byte_identical`,
    /// `c108_crash_during_recovery_and_torn_intent`.
    pub fn recover_interrupted(&mut self) -> Result<Vec<Finding>, OkfError> {
        // #10746 S5: with the §4.4 lock, "only writer" is enforced here.
        let _lock = self.write_lock()?;
        recover_on_open(self)
    }

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
        // #10746 S5: one writer at a time (§4.4).
        let _lock = self.write_lock()?;
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
        // #10746 S5 r2: an ingest batch refused while this op held `log.md`
        // is written now (§4.11 rule 2).
        finish_batches(self)?;
        Ok(RecoveryReport {
            op_id: p.op.clone(),
            kind: p.kind,
            direction: Some(dir),
            conflicts: other.into_iter().map(PathBuf::from).collect(),
        })
    }
}
