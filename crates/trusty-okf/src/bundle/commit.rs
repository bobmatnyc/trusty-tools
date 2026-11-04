//! How a planned `put` or `verify` reaches the disk (SPEC-OKF-01 §4.4,
//! §4.9 Axis 1, §4.11).
//!
//! Why: a write that touches one file keeps the §4.4 rule (atomic rename)
//! plus a revision-journal line before the rename (criterion 66); a write
//! that touches more than one file — the concept plus `log.md`, or plus the
//! verification record — must be atomic as a set through the operation
//! journal (§4.11).
//! What: [`Bundle::commit_op`] refuses a write to a path of a conflicted
//! operation with `RecoveryPending`, then either appends the revision line
//! and renames (one file), or runs one journaled operation whose steps are,
//! in order: the verification-record lines, the revision line, the concept
//! file and, for a logged write, `log.md`. An ingest run defers its `log.md`
//! entries to a `LogBatch`, so an item that changes no verification is a
//! one-file write.
//! Test: `c66_crash_between_revision_line_and_rename_keeps_previous_saved`,
//! `c106_crash_at_every_boundary_rolls_forward_byte_identical`,
//! `c126_out_of_band_edit_mid_move_stops_with_recovery_conflict`.

use std::path::PathBuf;

use chrono::{DateTime, FixedOffset};

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::log_md::{LogBatch, prepare_entry};
use crate::bundle::ops::{self, OpKind, OpPlan, failpoint, put_file};
use crate::bundle::put::PutOutcome;
use crate::bundle::record::{Line, instant_string, record_rel};
use crate::format::fields::Actor;
use crate::format::hash::{ContentHash, bytes_hash};
use crate::format::id::ConceptId;
use crate::format::validate::Finding;
use crate::state::revisions::{self, REVISIONS_REL, RevisionLine, run_id};

/// Where a write's `log.md` entry goes.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LogMode<'a> {
    /// Into `log.md` in the same operation.
    Entry,
    /// Into an ingest run's batch, written once at the end of the run.
    Defer(&'a LogBatch),
}

/// A checked, rendered write that has touched no file.
#[derive(Debug, Clone)]
pub(crate) struct PutPlan {
    pub id: ConceptId,
    pub outcome: PutOutcome,
    pub findings: Vec<Finding>,
    /// The bytes the target holds now; `None` when absent.
    pub pre: Option<Vec<u8>>,
    /// The rendered bytes.
    pub post: Vec<u8>,
    /// §2.10 content hash of the rendered concept.
    pub content: ContentHash,
    /// Verification-record lines for `id`.
    pub lines: Vec<Line>,
    /// The `log.md` entry text.
    pub log: String,
}

impl PutPlan {
    /// The revision-journal line for this write. An empty `run` is filled
    /// with the op ID by `ops::run`.
    pub(crate) fn revision(
        &self,
        op_kind: &str,
        now: &DateTime<FixedOffset>,
        run: &str,
    ) -> RevisionLine {
        RevisionLine {
            id: self.id.as_str().to_string(),
            content: self.content.as_str().to_string(),
            bytes: bytes_hash(&self.post),
            op_kind: op_kind.to_string(),
            now: instant_string(now),
            run: run.to_string(),
            op: None,
            step: None,
        }
    }
}

fn json(v: &RevisionLine) -> Result<serde_json::Value, OkfError> {
    serde_json::to_value(v).map_err(|e| OkfError::Parse {
        path: REVISIONS_REL.into(),
        line: None,
        message: e.to_string(),
    })
}

impl Bundle {
    /// Record that `op` stopped or was found conflicted over `paths`.
    pub(crate) fn mark_conflict(&mut self, op: &str, paths: &[String]) {
        self.conflicts.retain(|(o, _)| o != op);
        self.conflicts.push((op.to_string(), paths.to_vec()));
    }

    /// Forget `op`'s conflict after it was recovered.
    pub(crate) fn clear_conflict(&mut self, op: &str) {
        self.conflicts.retain(|(o, _)| o != op);
    }

    /// Refuse a write to a path of a conflicted operation (§4.11 rule 2).
    ///
    /// Test: `c109_out_of_band_edit_after_crash_is_a_recovery_conflict`.
    pub(crate) fn check_recovery(&self, paths: &[String]) -> Result<(), OkfError> {
        for (op, held) in &self.conflicts {
            if paths.iter().any(|p| held.contains(p)) {
                return Err(OkfError::RecoveryPending {
                    op_id: op.clone(),
                    paths: held.iter().map(PathBuf::from).collect(),
                });
            }
        }
        Ok(())
    }

    /// Write a planned `put` or `verify` (see the module doc).
    pub(crate) fn commit_op(
        &mut self,
        plan: PutPlan,
        by: &Actor,
        now: &DateTime<FixedOffset>,
        log: LogMode<'_>,
        kind: OpKind,
    ) -> Result<(), OkfError> {
        let file = plan.id.file_name();
        let logged = matches!(log, LogMode::Entry);
        let mut paths = vec![file.clone(), record_rel(&plan.id)];
        if logged {
            paths.push("log.md".to_string());
        }
        self.check_recovery(&paths)?;
        let (op_kind, run) = match (kind, log) {
            (OpKind::Verify, _) => ("verify", String::new()),
            (_, LogMode::Defer(batch)) => ("ingest", batch.run().to_string()),
            _ => ("put", String::new()),
        };
        let mut rev = plan.revision(op_kind, now, &run);
        if let LogMode::Defer(batch) = log {
            batch.record(
                self,
                &file,
                &plan.log,
                now,
                plan.pre.as_deref().map(bytes_hash),
            )?;
        }
        if !logged && plan.lines.is_empty() {
            // One file: the revision line, then the rename (§4.9, crit. 66).
            if rev.run.is_empty() {
                rev.run = run_id("put", &[&rev.id, &rev.now, &rev.bytes]);
            }
            revisions::append(self.root(), &rev)?;
            failpoint("after-revision-line")?;
            return put_file(self, &file, Some(&plan.post));
        }
        let mut op = OpPlan::new(kind, &instant_string(now), &by.to_string());
        for l in &plan.lines {
            op.append(&record_rel(&plan.id), l.value()?);
        }
        op.append(REVISIONS_REL, json(&rev)?);
        op.file(&file, plan.pre.clone(), Some(plan.post.clone()));
        if logged {
            let e = prepare_entry(self.root(), now, &plan.log)?;
            op.file("log.md", e.old, Some(e.text.into_bytes()));
        }
        ops::run(self, op).map(|_| ())
    }
}
