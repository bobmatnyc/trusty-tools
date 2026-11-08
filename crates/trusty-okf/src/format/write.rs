//! Strict, atomic, raw-preserving concept file write (SPEC-OKF-01 §2.2,
//! §2.2.1, §2.11, §4.4).
//!
//! Why: a write that breaks OKF §11 must be refused before any byte moves,
//! a write that changes nothing must write nothing, and a write must never
//! leave a half-written file or follow a link out of the bundle.
//! What: [`check_write`] applies the [`WriteMode`]; [`write_concept`] checks,
//! renders, compares and writes through a `create_new` temporary file and a
//! rename. `Bundle::put` (slice S3) adds §2.10 and the operation journal on
//! top of it.
//! Test: `c03_strict_write_refuses_missing_type_and_changes_no_file`,
//! `c114_unchanged_put_writes_zero_bytes_and_reads_malformed`,
//! `c115_foreign_paths_read_only_and_write_is_invalid_id`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::OkfError;
use crate::format::concept::{Concept, render_concept};
use crate::format::fields::Frontmatter;
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
/// read warnings. Under `StrictErrors` any error refuses; under
/// `StrictWarnings` any error or warning refuses. A refusal is
/// [`OkfError::Conformance`] holding the blocking findings. Otherwise
/// returns every finding.
/// Test: `c03_strict_write_refuses_missing_type_and_changes_no_file`.
pub fn check_write(c: &Concept, mode: WriteMode) -> Result<Vec<Finding>, OkfError> {
    let path = c.path();
    let mut findings = conformance_findings(&c.frontmatter, &path);
    findings.extend(Frontmatter::from_mapping(c.values(), &path).1);
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

/// Write a concept under `root`, atomically, only when its bytes change.
///
/// Why: the S2 half of `Bundle::put` — strict refusal, raw-preserving render,
/// zero-byte no-op and atomic replace (§2.2, §2.2.1, §4.4).
/// What: in order, each step refusing with no file changed:
/// 1. a foreign concept is refused with [`OkfError::InvalidId`];
/// 2. [`check_write`] applies `mode`;
/// 3. a stored block that is not patchable is refused with
///    [`OkfError::Parse`];
/// 4. `root` is canonicalised and must be a directory; every existing
///    component below it on the way to the target must not be a symbolic
///    link ([`OkfError::PathEscape`]).
///
/// Then the concept renders; equal bytes on disk return
/// [`WriteOutcome::Unchanged`] and write nothing. Otherwise the bytes go to a
/// `create_new` temporary file in the target's directory, are synced, and the
/// file is renamed over the target. Returns the outcome and every finding.
/// Test: `c03_strict_write_refuses_missing_type_and_changes_no_file`,
/// `c114_unchanged_put_writes_zero_bytes_and_reads_malformed`,
/// `c115_foreign_paths_read_only_and_write_is_invalid_id`.
pub fn write_concept(
    root: &Path,
    c: &Concept,
    mode: WriteMode,
) -> Result<(WriteOutcome, Vec<Finding>), OkfError> {
    let id = match &c.id {
        ConceptRef::Id(id) => id.clone(),
        ConceptRef::Foreign(p) => return Err(foreign_refusal(p)),
    };
    let findings = check_write(c, mode)?;
    if !c.raw.text.is_empty() && !c.raw.patchable {
        return Err(OkfError::Parse {
            path: PathBuf::from(c.path()),
            line: None,
            message: "frontmatter cannot be split into top-level entries; \
                      a raw-preserving write is refused (SPEC-OKF-01 §2.2.1)"
                .to_string(),
        });
    }
    let target = confined_target(root, &id)?;
    let rendered = render_concept(c);
    let existing = match std::fs::read(&target) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(io_err(&target, source)),
    };
    if existing.as_deref() == Some(rendered.as_bytes()) {
        return Ok((WriteOutcome::Unchanged, findings));
    }
    atomic_write(&target, rendered.as_bytes())?;
    let outcome = if existing.is_some() {
        WriteOutcome::Updated
    } else {
        WriteOutcome::Created
    };
    Ok((outcome, findings))
}

/// The refusal for a write that targets a foreign file.
fn foreign_refusal(p: &Path) -> OkfError {
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

/// Canonicalise `root` and check each existing component from it to the
/// target of `id` (§2.11 file system rules 1 and 2).
fn confined_target(root: &Path, id: &ConceptId) -> Result<PathBuf, OkfError> {
    let root = root.canonicalize().map_err(|e| io_err(root, e))?;
    if !root.is_dir() {
        return Err(OkfError::PathEscape(root));
    }
    let rel = PathBuf::from(id.file_name());
    let mut at = root.clone();
    for comp in rel.components() {
        let Component::Normal(part) = comp else {
            return Err(OkfError::PathEscape(rel));
        };
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(OkfError::PathEscape(at)),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(io_err(&at, e)),
        }
    }
    Ok(root.join(rel))
}

/// Write `bytes` to `target` through a `create_new` temporary file in the
/// same directory and a rename.
fn atomic_write(target: &Path, bytes: &[u8]) -> Result<(), OkfError> {
    let dir = target
        .parent()
        .ok_or_else(|| OkfError::PathEscape(target.to_path_buf()))?;
    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Process ID plus a counter: two writes in one process never share a name.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.okf-tmp-{}-{n}", std::process::id()));
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, target));
    if let Err(e) = result {
        // Removes only the temporary file this call created; a concept file
        // is never deleted (criterion 10).
        let _ = std::fs::remove_file(&tmp); // okf: temp-cleanup
        return Err(io_err(target, e));
    }
    Ok(())
}
