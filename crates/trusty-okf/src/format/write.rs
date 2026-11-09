//! Strict, atomic, raw-preserving concept file write (SPEC-OKF-01 §2.2,
//! §2.2.1, §2.11, §4.4).
//!
//! Why: a write that breaks OKF §11 must be refused before any byte moves,
//! a write that changes nothing must write nothing, and a write must never
//! leave a half-written file, follow a link out of the bundle, or replace
//! bytes it did not read.
//! What: [`check_write`] applies the [`WriteMode`] to the values that will be
//! rendered; `prepare_write_with` (crate-private; `Bundle::put` is the
//! public write) checks, refuses a stale or new-over-existing write, renders
//! with a re-parse postcondition and compares; the bundle writes the rendered
//! bytes through the operation journal or `bundle::paths::atomic_write`.
//! Test: `c03_strict_write_refuses_missing_type_and_changes_no_file`,
//! `c114_unchanged_put_writes_zero_bytes_and_reads_malformed`,
//! `c115_foreign_paths_read_only_and_write_is_invalid_id`,
//! `a1_put_never_replaces_an_existing_file_with_a_new_concept`,
//! `a1_put_refuses_a_stale_write`, `a2_strict_check_runs_on_rendered_values`.

use std::path::{Path, PathBuf};

use crate::OkfError;
use crate::bundle::paths::confined_file;
use crate::format::concept::{Concept, parse_concept_at, render_concept};
use crate::format::fields::Frontmatter;
use crate::format::hash::bytes_hash;
use crate::format::id::{ConceptId, ConceptRef};
use crate::format::validate::{Finding, Severity, conformance_findings};

/// How strictly a write applies findings (SPEC-OKF-01 §2.2, §3 (f) B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteMode {
    /// Never refuse; report every finding.
    Lenient,
    /// Refuse on an error finding (an OKF §11 breach). The default.
    #[default]
    StrictErrors,
    /// Refuse on an error or a warning finding.
    StrictWarnings,
}

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The file did not exist and was created.
    Created,
    /// The file existed and its bytes changed.
    Updated,
    /// The rendered bytes equal the file's bytes; nothing was written.
    Unchanged,
}

/// Apply `mode` to a concept's findings.
///
/// What: the findings are the §11 conformance check plus the typed view's
/// read warnings, both computed from `c.values()`, the values the render
/// writes (#10746 A2), never from a cached view. Under `StrictErrors` any
/// error refuses; under `StrictWarnings` any error or warning refuses. A
/// refusal is [`OkfError::Conformance`] holding the blocking findings.
/// Otherwise returns every finding.
/// Test: `c03_strict_write_refuses_missing_type_and_changes_no_file`,
/// `a2_strict_check_runs_on_rendered_values`.
pub fn check_write(c: &Concept, mode: WriteMode) -> Result<Vec<Finding>, OkfError> {
    let path = c.path();
    let (view, read) = Frontmatter::from_mapping(c.values(), &path);
    let mut findings = conformance_findings(&view, &path);
    findings.extend(read);
    let blocks = |f: &Finding| match mode {
        WriteMode::Lenient => false,
        WriteMode::StrictErrors => f.severity == Severity::Error,
        WriteMode::StrictWarnings => f.severity != Severity::Info,
    };
    let blocking: Vec<Finding> = findings.iter().filter(|&f| blocks(f)).cloned().collect();
    if blocking.is_empty() {
        Ok(findings)
    } else {
        Err(OkfError::Conformance(blocking))
    }
}

/// Render `c` and prove the text reads back as exactly `c`'s values and body.
///
/// Why: #10746 A2 — what is checked must be what is written. A render that
/// would emit a duplicate key, drop a non-string key or lose a value fails
/// the re-parse comparison, so it is refused instead of written.
/// What: renders, re-parses under the same ID, and compares values and body.
/// A mismatch or a parse failure is [`OkfError::Parse`].
/// Test: `a2_render_never_emits_duplicate_keys`.
pub(crate) fn render_checked(c: &Concept) -> Result<String, OkfError> {
    let rendered = render_concept(c);
    let refuse = |message: String| OkfError::Parse {
        path: PathBuf::from(c.path()),
        line: None,
        message,
    };
    let (back, _) = parse_concept_at(c.id.clone(), &rendered)
        .map_err(|e| refuse(format!("rendered text does not read back: {e}")))?;
    if back.values() != c.values() || back.body != c.body {
        return Err(refuse(
            "rendered text does not read back as the checked values; write refused \
             (SPEC-OKF-01 §2.2.1)"
                .to_string(),
        ));
    }
    Ok(rendered)
}

/// The bytes on disk at `target`, or `None` when it does not exist.
pub(crate) fn read_existing(target: &Path) -> Result<Option<Vec<u8>>, OkfError> {
    match std::fs::read(target) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_err(target, source)),
    }
}

/// Refuse a write that would replace bytes the caller never read (#10746 A1).
///
/// What: a new concept (no read hash) over an existing file is
/// `Conflict { reason: "exists" }`; a read concept whose file now holds other
/// bytes, or no longer exists, is `Conflict { reason: "stale_write" }`.
/// Test: `a1_put_never_replaces_an_existing_file_with_a_new_concept`,
/// `a1_put_refuses_a_stale_write`.
pub(crate) fn check_not_stale(
    id: &ConceptId,
    c: &Concept,
    existing: Option<&[u8]>,
) -> Result<(), OkfError> {
    let conflict = |reason: &str| OkfError::Conflict {
        id: id.clone(),
        reason: reason.to_string(),
    };
    match (existing, c.read_hash()) {
        (None, None) => Ok(()),
        (Some(_), None) => Err(conflict("exists")),
        (Some(bytes), Some(h)) if bytes_hash(bytes) == h => Ok(()),
        _ => Err(conflict("stale_write")),
    }
}

/// A concept write that passed every refusal and has not touched a file.
///
/// #10746: split from the old `write_concept` so `Bundle::put` and
/// `Bundle::verify` can append the verification record between the checks
/// and the rename (§2.10 "no verification event is lost").
#[derive(Debug)]
pub(crate) struct PreparedWrite {
    rendered: String,
    /// What the write would do.
    pub(crate) outcome: WriteOutcome,
}

impl PreparedWrite {
    /// #10746 S5: the rendered bytes, the post-image of a journaled step.
    pub(crate) fn bytes(&self) -> &[u8] {
        self.rendered.as_bytes()
    }
}

/// Check and render a concept write under `root` without writing a byte,
/// against `existing`, the bytes the target holds (#10746 S5: the caller
/// reads them, so a move can plan the moved concept at its new path).
///
/// Why: the shared core of `Bundle::put` and `Bundle::verify` — strict
/// refusal, raw-preserving render, zero-byte no-op and atomic replace (§2.2,
/// §2.2.1, §4.4). Crate-private since S3 (#10746): a public write here would
/// bypass §2.10 and the fold rule.
/// What: in order, each step refusing with no file changed:
/// 1. a foreign concept is refused with [`OkfError::InvalidId`];
/// 2. [`check_write`] applies `mode`;
/// 3. a stored block that is not patchable is refused with
///    [`OkfError::Parse`];
/// 4. the target is confined (`bundle::paths::confined_file`);
/// 5. a stale or new-over-existing write against `existing` is refused
///    ([`check_not_stale`]);
/// 6. the render must read back as the checked values ([`render_checked`]).
///
/// Equal bytes on disk give [`WriteOutcome::Unchanged`]. The caller runs
/// `PreparedWrite::bytes` and writes them through the operation journal or
/// `atomic_write`.
/// Test: `c03_strict_write_refuses_missing_type_and_changes_no_file`,
/// `c114_unchanged_put_writes_zero_bytes_and_reads_malformed`,
/// `c115_foreign_paths_read_only_and_write_is_invalid_id`.
pub(crate) fn prepare_write_with(
    root: &Path,
    c: &Concept,
    mode: WriteMode,
    existing: Option<&[u8]>,
) -> Result<(PreparedWrite, Vec<Finding>), OkfError> {
    let id = match &c.id {
        ConceptRef::Id(id) => id.clone(),
        ConceptRef::Foreign(p) => return Err(foreign_refusal(p)),
    };
    let findings = check_write(c, mode)?;
    if !c.raw().text.is_empty() && !c.raw().patchable {
        return Err(unpatchable(c));
    }
    confined_file(root, &id.file_name())?;
    check_not_stale(&id, c, existing)?;
    let rendered = render_checked(c)?;
    let outcome = match existing {
        Some(b) if b == rendered.as_bytes() => WriteOutcome::Unchanged,
        Some(_) => WriteOutcome::Updated,
        None => WriteOutcome::Created,
    };
    let prepared = PreparedWrite { rendered, outcome };
    Ok((prepared, findings))
}

/// The refusal for a stored block that cannot be patched (§2.2.1).
fn unpatchable(c: &Concept) -> OkfError {
    OkfError::Parse {
        path: PathBuf::from(c.path()),
        line: None,
        message: "frontmatter cannot be split into top-level entries; \
                  a raw-preserving write is refused (SPEC-OKF-01 §2.2.1)"
            .to_string(),
    }
}

/// The refusal for a write that targets a foreign file.
pub(crate) fn foreign_refusal(p: &Path) -> OkfError {
    let shown = p.to_string_lossy().into_owned();
    let stem = shown.strip_suffix(".md").unwrap_or(&shown);
    match ConceptId::parse(stem) {
        Err(e) => e,
        Ok(_) => OkfError::InvalidId {
            input: shown,
            rule: 0,
        },
    }
}

fn io_err(path: &Path, source: std::io::Error) -> OkfError {
    OkfError::Io {
        path: path.to_path_buf(),
        source,
    }
}
