//! The operation journal: multi-file writes, atomic as a set (SPEC-OKF-01
//! §4.11).
//!
//! Why: each file write is atomic, but `move_concept`, a content-changing
//! `put`, `verify` and an ingest run's `log.md` write touch several files. A
//! crash between two of them left a mixed state with no recovery rule.
//! What: an [`OpPlan`] lists ordered steps: a file step (a path, its
//! pre-image and post-image bytes, `None` for absent) or an append step (a
//! journal file and one JSON line). [`run`] stages every post-image under
//! `_sources/ops/<op-id>/new/` and every pre-image under `old/` (suffix
//! `.okf-staged`, synced), appends the intent record to
//! `_sources/ops/journal.jsonl`, runs each step after a pre-image check,
//! appends the completion record and removes the staging directory. A
//! pre-image mismatch stops before the step, writes no completion and
//! returns `RecoveryPending`. `bundle::recover` finishes or reverses an
//! interrupted operation. The op ID is a hash of the steps, so an
//! interrupted and an uninterrupted run name the same op.
//! Test: `c106_crash_at_every_boundary_rolls_forward_byte_identical`,
//! `c126_out_of_band_edit_mid_move_stops_with_recovery_conflict`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::paths::{append_line, atomic_write, confined_file, io_err, read_rel};
use crate::format::hash::bytes_hash;
use crate::format::validate::{Finding, FindingKind, Severity};
use crate::state::revisions::run_id;

/// The staging and journal directory (§4.11 "Location").
pub(crate) const OPS_DIR: &str = "_sources/ops";
/// The operation journal.
pub(crate) const JOURNAL_REL: &str = "_sources/ops/journal.jsonl";
/// The suffix every staged file carries (§4.11 "Staged names").
pub(crate) const STAGED: &str = ".okf-staged";

/// The kind of a journaled operation (SPEC-OKF-01 §4.2).
///
/// #10746 S5: `Correct` covers every `put` that writes more than one file;
/// `Ingest` is an ingest run's single deferred `log.md` write (not in the
/// §4.2 sketch). #10746 S6 writes `Supersede`, `Deprecate`, `Migrate` and
/// `Rebuild`; `Deprecate` (not in the §4.2 sketch) is `deprecate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum OpKind {
    /// `move_concept`.
    Move,
    /// `supersede` (3c A).
    Supersede,
    /// A content-changing or logged `put`.
    Correct,
    /// `verify`.
    Verify,
    /// `migrate`; the one kind recovery rolls back.
    Migrate,
    /// The `rebuild` swap.
    Rebuild,
    /// An ingest run's `log.md` write.
    Ingest,
    /// `deprecate`: the concept and `log.md`.
    Deprecate,
}

/// Which way recovery takes an operation (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryDirection {
    /// Every file to its post-image; missing lines appended.
    RollForward,
    /// Every file to its pre-image.
    RollBack,
}

/// One journaled step (§4.11 "Steps").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub(crate) enum Step {
    /// A file reaches `post` from `pre` (byte hashes; `None` is absent).
    File {
        path: String,
        pre: Option<String>,
        post: Option<String>,
    },
    /// One exact line appended to a journal file.
    Append { file: String, line: String },
}

/// One operation-journal record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "rec", rename_all = "snake_case")]
pub(crate) enum Record {
    /// Written before the first write to any bundle file.
    Intent {
        op: String,
        kind: OpKind,
        now: String,
        actor: String,
        steps: Vec<Step>,
    },
    /// Written after the last step, or by recovery with its direction.
    Complete {
        op: String,
        direction: Option<RecoveryDirection>,
    },
}

/// A planned step, before staging.
#[derive(Debug, Clone)]
enum Planned {
    File {
        path: String,
        pre: Option<Vec<u8>>,
        post: Option<Vec<u8>>,
    },
    Append {
        file: String,
        line: Value,
    },
}

/// An operation to run: kind, `now`, actor and ordered steps.
#[derive(Debug, Clone)]
pub(crate) struct OpPlan {
    kind: OpKind,
    now: String,
    actor: String,
    fixed_id: Option<String>,
    steps: Vec<Planned>,
}

impl OpPlan {
    /// An empty plan.
    pub(crate) fn new(kind: OpKind, now: &str, actor: &str) -> Self {
        Self {
            kind,
            now: now.to_string(),
            actor: actor.to_string(),
            fixed_id: None,
            steps: Vec::new(),
        }
    }

    /// Use `id` as the op ID instead of the step hash (an ingest run's log).
    pub(crate) fn with_id(mut self, id: String) -> Self {
        self.fixed_id = Some(id);
        self
    }

    /// Add a file step.
    pub(crate) fn file(&mut self, path: &str, pre: Option<Vec<u8>>, post: Option<Vec<u8>>) {
        self.steps.push(Planned::File {
            path: path.to_string(),
            pre,
            post,
        });
    }

    /// Add an append step. A line for a file that is already a file step of
    /// this plan joins that step's post-image instead, so no file is both a
    /// file step and an append target (recovery classifies file bytes).
    pub(crate) fn append(&mut self, file: &str, line: Value) {
        for s in &mut self.steps {
            if let Planned::File {
                path,
                post: Some(post),
                ..
            } = s
                && path == file
            {
                post.extend_from_slice(line.to_string().as_bytes());
                post.push(b'\n');
                return;
            }
        }
        self.steps.push(Planned::Append {
            file: file.to_string(),
            line,
        });
    }

    /// Every file-step path.
    pub(crate) fn file_paths(&self) -> Vec<String> {
        self.steps
            .iter()
            .filter_map(|s| match s {
                Planned::File { path, .. } => Some(path.clone()),
                Planned::Append { .. } => None,
            })
            .collect()
    }

    /// The op ID: fixed, or `<kind>-<hash of now, actor and steps>`.
    /// #10746 S6: a migration names its backups by it before `run`.
    pub(crate) fn op_id(&self) -> String {
        if let Some(id) = &self.fixed_id {
            return id.clone();
        }
        let h = |b: &Option<Vec<u8>>| b.as_deref().map(bytes_hash).unwrap_or_default();
        let mut parts = vec![self.now.clone(), self.actor.clone()];
        for s in &self.steps {
            match s {
                Planned::File { path, pre, post } => {
                    parts.extend([path.clone(), h(pre), h(post)]);
                }
                Planned::Append { file, line } => parts.extend([file.clone(), line.to_string()]),
            }
        }
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        run_id(&format!("{:?}", self.kind).to_lowercase(), &refs)
    }
}

/// The staged path of `rel` under `side` (`new`, `old` or `conflict`).
pub(crate) fn staged_rel(op: &str, side: &str, rel: &str) -> String {
    format!("{OPS_DIR}/{op}/{side}/{rel}{STAGED}")
}

/// Byte hash of the file at `rel`, or `None` when absent.
pub(crate) fn disk_hash(b: &Bundle, rel: &str) -> Result<Option<String>, OkfError> {
    io_seam(&format!("read:{rel}"))?;
    Ok(read_rel(b.root(), rel)?.map(|x| bytes_hash(&x)))
}

/// Append one line to a journal file of an operation (synced).
pub(crate) fn append(b: &Bundle, file: &str, line: &str) -> Result<(), OkfError> {
    io_seam(file)?;
    append_line(b.root(), file, line)
}

/// An I/O-error seam: under `cfg(test)` a hook may fail the named file
/// access with a real error (not a crash). Compiled to nothing otherwise.
pub(crate) fn io_seam(name: &str) -> Result<(), OkfError> {
    #[cfg(test)]
    if fail::io_hit(name) {
        return Err(OkfError::Io {
            path: PathBuf::from(name),
            source: std::io::Error::other("injected i/o error"),
        });
    }
    let _ = name;
    Ok(())
}

/// #10746 S9: a read seam between resolving a source path and reading its
/// bytes. Under `cfg(test)` a hook may swap the file at `path`, as a racing
/// writer would. Compiled to nothing otherwise.
pub(crate) fn read_seam(path: &std::path::Path) {
    #[cfg(test)]
    fail::before_read(path);
    let _ = path;
}

/// #10746 S9 fix round: a seam after a source path's directories are
/// resolved and before its leaf is opened. Under `cfg(test)` a hook may
/// swap a parent directory, as a racing writer would.
pub(crate) fn walk_seam(path: &std::path::Path) {
    #[cfg(test)]
    fail::before_walk(path);
    let _ = path;
}

/// Bring `rel` to `bytes` (`None` removes it).
///
/// #10746: a removal is the source path of a move whose post-image already
/// lives at the target, or a file the operation itself created and a roll
/// back reverses; never a concept's only copy.
pub(crate) fn put_file(b: &Bundle, rel: &str, bytes: Option<&[u8]>) -> Result<(), OkfError> {
    let path = confined_file(b.root(), rel)?;
    io_seam(rel)?;
    #[cfg(test)]
    fail::count_write(rel);
    match bytes {
        Some(bytes) => atomic_write(&path, bytes),
        None => {
            let removed = std::fs::remove_file(&path); // okf: temp-cleanup (move source, §4.11)
            match removed {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io_err(&path, e)),
            }
        }
    }
}

/// Remove an operation's staging files: `new/` and `old/`, then the
/// directory when nothing else is left. A `conflict/` copy stays, so an
/// out-of-band edit survives recovery (§4.11 rule 2, criterion 109).
pub(crate) fn remove_staging(b: &Bundle, op: &str) -> Result<(), OkfError> {
    io_seam(&format!("remove-staging:{op}"))?;
    let dir = confined_file(b.root(), &format!("{OPS_DIR}/{op}"))?;
    for side in ["new", "old"] {
        let sub = dir.join(side);
        let removed = std::fs::remove_dir_all(&sub); // okf: temp-cleanup (staging)
        match removed {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&sub, e)),
        }
    }
    let empty = match std::fs::read_dir(&dir) {
        Ok(mut it) => it.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(&dir, e)),
    };
    if empty {
        std::fs::remove_dir(&dir).map_err(|e| io_err(&dir, e))?; // okf: temp-cleanup (empty staging dir)
    }
    Ok(())
}

/// Append one operation-journal record, synced.
pub(crate) fn append_record(b: &Bundle, r: &Record) -> Result<(), OkfError> {
    let json = serde_json::to_string(r).map_err(|e| OkfError::Parse {
        path: PathBuf::from(JOURNAL_REL),
        line: None,
        message: e.to_string(),
    })?;
    append(b, JOURNAL_REL, &json)
}

/// A crash seam: under `cfg(test)` a hook may stop the operation here, as a
/// process death would. Compiled to nothing outside tests.
pub(crate) fn failpoint(name: &str) -> Result<(), OkfError> {
    #[cfg(test)]
    if fail::hit(name) {
        return Err(OkfError::Io {
            path: PathBuf::from(name),
            source: std::io::Error::other("injected crash"),
        });
    }
    let _ = name;
    Ok(())
}

/// Run a plan (see the module doc). Returns the findings of the run: a
/// `staging_left` warning when the staging directory could not be removed
/// after the completion record (the write itself succeeded).
///
/// #10746 S5 r2: every failure after the intent goes through `abort` (roll
/// back now) or, once every step ran, marks the operation conflicted and
/// returns `RecoveryPending`, so this handle never writes over an
/// operation that a later `recover` would replay.
/// Test: `c106_crash_at_every_boundary_rolls_forward_byte_identical`,
/// `precheck_read_error_aborts_the_operation`,
/// `completion_failure_marks_conflict_and_staging_failure_is_a_finding`.
pub(crate) fn run(b: &mut Bundle, plan: OpPlan) -> Result<Vec<Finding>, OkfError> {
    run_checked(b, plan, &|_| Ok(()))
}

/// [`run`], with `check` run after the last step and before the completion
/// record (#10746 S6: the §2.9 migration audit). A failed check rolls the
/// operation back and returns the check's error.
/// Test: `c24_audit_restores_every_file_when_a_sources_entry_is_dropped`,
/// `c120_body_byte_change_restores_and_a_corrupt_blob_is_restore_failed`.
pub(crate) fn run_checked(
    b: &mut Bundle,
    plan: OpPlan,
    check: &dyn Fn(&Bundle) -> Result<(), OkfError>,
) -> Result<Vec<Finding>, OkfError> {
    // Every path is confined (no link, no escape) before any byte moves, so
    // a refused path stops the operation with no file changed.
    for s in &plan.steps {
        let (Planned::File { path, .. } | Planned::Append { file: path, .. }) = s;
        confined_file(b.root(), path)?;
    }
    let op = plan.op_id();
    let mut steps = Vec::with_capacity(plan.steps.len());
    let mut lines: Vec<Option<String>> = Vec::with_capacity(plan.steps.len());
    for (i, s) in plan.steps.iter().enumerate() {
        match s {
            Planned::File { path, pre, post } => {
                for (side, bytes) in [("new", post), ("old", pre)] {
                    // #10746 S6: a migration's pre-images are its
                    // content-addressed backups, written before `run`
                    // (§4.11 "Before the intent", §2.9 step 2).
                    if side == "old" && plan.kind == OpKind::Migrate {
                        continue;
                    }
                    if let Some(bytes) = bytes {
                        put_file(b, &staged_rel(&op, side, path), Some(bytes))?;
                    }
                }
                let h = |x: &Option<Vec<u8>>| x.as_deref().map(bytes_hash);
                steps.push(Step::File {
                    path: path.clone(),
                    pre: h(pre),
                    post: h(post),
                });
                lines.push(None);
            }
            Planned::Append { file, line } => {
                let mut line = line.clone();
                if let Value::Object(m) = &mut line {
                    m.insert("op".into(), Value::String(op.clone()));
                    m.insert("step".into(), Value::from(i));
                    if m.get("run").and_then(Value::as_str) == Some("") {
                        m.insert("run".into(), Value::String(op.clone()));
                    }
                }
                steps.push(Step::Append {
                    file: file.clone(),
                    line: line.to_string(),
                });
                lines.push(Some(line.to_string()));
            }
        }
    }
    failpoint("staged")?;
    let intent = Record::Intent {
        op: op.clone(),
        kind: plan.kind,
        now: plan.now.clone(),
        actor: plan.actor.clone(),
        steps: steps.clone(),
    };
    append_record(b, &intent)?;
    failpoint("intent")?;
    for (i, (planned, line)) in plan.steps.iter().zip(&lines).enumerate() {
        match (planned, line) {
            (Planned::File { path, pre, post }, _) => {
                let disk = match disk_hash(b, path) {
                    Ok(h) => h,
                    Err(e) => return Err(abort(b, &op, plan.kind, &steps, e)),
                };
                if disk != pre.as_deref().map(bytes_hash) {
                    let paths = plan.file_paths();
                    b.mark_conflict(&op, &paths);
                    return Err(OkfError::RecoveryPending {
                        op_id: op,
                        paths: paths.into_iter().map(PathBuf::from).collect(),
                    });
                }
                if let Err(e) = put_file(b, path, post.as_deref()) {
                    return Err(abort(b, &op, plan.kind, &steps, e));
                }
            }
            (Planned::Append { file, .. }, Some(line)) => {
                if let Err(e) = append(b, file, line) {
                    return Err(abort(b, &op, plan.kind, &steps, e));
                }
            }
            (Planned::Append { file, .. }, None) => {
                return Err(OkfError::Parse {
                    path: PathBuf::from(file),
                    line: None,
                    message: "append step has no finalized line".to_string(),
                });
            }
        }
        failpoint(&format!("step-{i}"))?;
    }
    if let Err(e) = check(b) {
        return Err(abort(b, &op, plan.kind, &steps, e));
    }
    failpoint("before-complete")?;
    let complete = Record::Complete {
        op: op.clone(),
        direction: None,
    };
    if append_record(b, &complete).is_err() {
        // Every step ran but the journal does not say so: a later recover
        // would replay it, so this handle stops writing to its paths.
        return Err(pending(b, &op, &plan.file_paths()));
    }
    let mut findings = Vec::new();
    if let Err(e) = remove_staging(b, &op) {
        findings.push(Finding::new(
            Severity::Warning,
            FindingKind::StagingLeft,
            format!("{OPS_DIR}/{op}"),
            format!("operation {op} completed; its staging was not removed: {e}"),
            "SPEC-OKF-01 §4.11",
        ));
    }
    Ok(findings)
}

/// Mark `op` conflicted on this handle and build its `RecoveryPending`.
fn pending(b: &mut Bundle, op: &str, paths: &[String]) -> OkfError {
    b.mark_conflict(op, paths);
    OkfError::RecoveryPending {
        op_id: op.to_string(),
        paths: paths.iter().map(PathBuf::from).collect(),
    }
}

/// A step failed in-process (not a crash): roll the operation back now, so
/// the caller's error leaves every bundle file as it was (§4.4 "a refused
/// write changes no file"). Returns `e`. When the roll back fails too, the
/// operation stays incomplete, its paths are marked conflicted on this
/// bundle, and the error is `RecoveryPending`, never `e` alone: the caller
/// learns the set is not back at its pre-image.
fn abort(b: &mut Bundle, op: &str, kind: OpKind, steps: &[Step], e: OkfError) -> OkfError {
    match crate::bundle::recover::roll_back_now(b, op, kind, steps) {
        Ok(()) => e,
        // #10746 S6: a migration restore whose hash check failed keeps the
        // operation open (no completion record) and adds `restore_failed`
        // to the audit's loss list (§2.9 step 4).
        Err(OkfError::MigrationLoss(failed)) => {
            let paths: Vec<String> = steps
                .iter()
                .filter_map(|s| match s {
                    Step::File { path, .. } => Some(path.clone()),
                    Step::Append { .. } => None,
                })
                .collect();
            b.mark_conflict(op, &paths);
            let mut items = match e {
                OkfError::MigrationLoss(items) => items,
                other => vec![crate::bundle::audit::LossItem::new(
                    "",
                    "step_failed",
                    &other.to_string(),
                )],
            };
            items.extend(failed);
            OkfError::MigrationLoss(items)
        }
        Err(_) => {
            let paths: Vec<String> = steps
                .iter()
                .filter_map(|s| match s {
                    Step::File { path, .. } => Some(path.clone()),
                    Step::Append { .. } => None,
                })
                .collect();
            pending(b, op, &paths)
        }
    }
}

/// A post-image seam: under `cfg(test)` a hook may change the bytes a
/// migration writes for `rel` (#10746 S6: the criterion 24 and 120 audit
/// tests). Compiled to nothing otherwise.
pub(crate) fn mutate_seam(rel: &str, bytes: &mut Vec<u8>) {
    #[cfg(test)]
    fail::mutate(rel, bytes);
    let _ = (rel, bytes);
}

/// Test-only crash hooks and write counters.
#[cfg(test)]
pub(crate) mod fail {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    type Hook = Box<dyn FnMut(&str) -> bool>;
    type Mutate = Box<dyn FnMut(&str, &mut Vec<u8>)>;
    type ReadHook = Box<dyn FnMut(&std::path::Path)>;

    thread_local! {
        static READ: RefCell<Option<ReadHook>> = const { RefCell::new(None) };
        static WALK: RefCell<Option<ReadHook>> = const { RefCell::new(None) };
        static MUTATE: RefCell<Option<Mutate>> = const { RefCell::new(None) };
        static IO: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static WRITES: RefCell<BTreeMap<String, usize>> = const { RefCell::new(BTreeMap::new()) };
    }

    /// Install a hook; it returns true to crash at the named seam.
    pub(crate) fn set(f: impl FnMut(&str) -> bool + 'static) {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    /// Remove every hook.
    pub(crate) fn clear() {
        HOOK.with(|h| *h.borrow_mut() = None);
        IO.with(|h| *h.borrow_mut() = None);
        MUTATE.with(|h| *h.borrow_mut() = None);
        READ.with(|h| *h.borrow_mut() = None);
        WALK.with(|h| *h.borrow_mut() = None);
    }

    /// #10746 S9 fix round: install a hook that runs at [`super::walk_seam`].
    pub(crate) fn set_before_walk(f: impl FnMut(&std::path::Path) + 'static) {
        WALK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    pub(crate) fn before_walk(path: &std::path::Path) {
        WALK.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f(path);
            }
        });
    }

    /// #10746 S9: install a hook that runs at [`super::read_seam`].
    pub(crate) fn set_before_read(f: impl FnMut(&std::path::Path) + 'static) {
        READ.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    pub(crate) fn before_read(path: &std::path::Path) {
        READ.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f(path);
            }
        });
    }

    /// #10746 S6: install a post-image hook for a migration.
    pub(crate) fn set_mutate(f: impl FnMut(&str, &mut Vec<u8>) + 'static) {
        MUTATE.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    pub(crate) fn mutate(rel: &str, bytes: &mut Vec<u8>) {
        MUTATE.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f(rel, bytes);
            }
        });
    }

    /// Install an I/O hook; it returns true to fail the named access.
    pub(crate) fn set_io(f: impl FnMut(&str) -> bool + 'static) {
        IO.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    pub(crate) fn io_hit(name: &str) -> bool {
        IO.with(|h| h.borrow_mut().as_mut().is_some_and(|f| f(name)))
    }

    pub(crate) fn hit(name: &str) -> bool {
        HOOK.with(|h| h.borrow_mut().as_mut().is_some_and(|f| f(name)))
    }

    pub(crate) fn count_write(rel: &str) {
        WRITES.with(|w| *w.borrow_mut().entry(rel.to_string()).or_default() += 1);
    }

    /// Writes to `rel` since the last reset on this thread.
    pub(crate) fn writes(rel: &str) -> usize {
        WRITES.with(|w| w.borrow().get(rel).copied().unwrap_or(0))
    }

    /// Reset the counters.
    pub(crate) fn reset_writes() {
        WRITES.with(|w| w.borrow_mut().clear());
    }
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
