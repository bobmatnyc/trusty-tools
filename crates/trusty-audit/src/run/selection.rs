//! The selection file: what the next sweep is asked to audit.
//!
//! Why: split out of `super` when it crossed the 500-SLOC production cap
//! (#5823). It earns its own file rather than an arbitrary cut: the selection
//! is a CONTRACT between producers this crate does not own — the repository
//! picker (#5497) and `crate::clone` (#5215) — and the sweep that consumes it.
//! Its format, its two write obligations, and the reader that enforces them are
//! one subject, and nothing else in `super` touches them.
//!
//! What: [`SELECTION_FILE`] and its schema, [`load_selection`] (which refuses an
//! absent, empty or truncated selection rather than sweeping a subset), and
//! [`save_selection`] (which is the only writer, so the atomic-rename and
//! `count`-first obligations are one decision instead of a note each producer
//! re-reads).
//! Test: `super::run_tests`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::AuditError;
use crate::workdir::{Area, WorkDir};

/// File under `state/` naming the repositories the run should audit.
///
/// Why: repository selection is separate work (#5487, #5497) and repository
/// cloning is separate again (#5215). #5555 does not implement either — it
/// defines the file both will write, so neither has to redesign this module's
/// input. The shape is deliberately the same `{ name, path }` pair as tga's own
/// manifest, because that is what a selection is.
///
/// ```toml
/// # <work-dir>/state/selected-repos.toml
/// count = 2                   # how many entries follow — REQUIRED
///
/// [[repositories]]
/// name = "acme-api"
/// path = "repos/acme-api"     # relative paths anchor to the work-dir root
///
/// [[repositories]]
/// name = "acme-web"
/// path = "repos/acme-web"
/// ```
///
/// ## Two obligations on whoever writes it
///
/// 1. **Write to a temporary file in the same directory and rename it into
///    place.** A rename is atomic; a direct write is not, and a producer that
///    crashes part-way through one leaves syntactically valid TOML holding a
///    prefix of the entries.
/// 2. **Declare `count` first**, before the `[[repositories]]` tables. TOML
///    requires top-level keys to precede tables anyway, so a truncated file
///    keeps the count and loses entries — which is exactly the direction that
///    makes the mismatch detectable. A `count` that disagrees with the number of
///    entries is [`AuditError::TruncatedSelection`], not a smaller selection.
///
/// Obligation 2 is what makes obligation 1 checkable rather than a request. A
/// sweep that silently audits three of five repositories and reports
/// `AllSucceeded` is the same fail-open shape as a sweep over none.
pub const SELECTION_FILE: &str = "selected-repos.toml";

/// One repository the run was asked to audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SelectedRepo {
    /// Display name, used for the output directory and the log file.
    pub name: String,
    /// Checkout path. Relative paths anchor to the working-directory root.
    pub path: PathBuf,
}

/// The `state/selected-repos.toml` document.
///
/// `count` is required and is the truncation check — see [`SELECTION_FILE`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Selection {
    pub(crate) count: usize,
    #[serde(default)]
    pub(crate) repositories: Vec<SelectedRepo>,
}

/// Where the repository selection is read from.
pub fn selection_path(work: &WorkDir) -> PathBuf {
    work.path(Area::State).join(SELECTION_FILE)
}

/// Read the repository selection.
///
/// Why: the input contract #5487/#5215 fill. Absent and empty are the same
/// state — nothing was selected — and both are a refusal rather than a
/// zero-repository success, because a sweep that audits nothing and exits 0 is
/// the fail-open shape this module exists to avoid.
///
/// A THIRD state is a refusal too: a file whose `count` does not match the
/// entries it carries. That is the truncated-write case a producer crashing
/// mid-write leaves behind, and it is indistinguishable from a smaller
/// selection unless the count says otherwise.
/// What: parses `state/`[`SELECTION_FILE`] and checks the count.
/// Test: `super::run_tests::an_absent_selection_is_a_refusal`,
/// `super::run_tests::a_truncated_selection_is_refused`.
///
/// # Errors
///
/// [`AuditError::NoRepositoriesSelected`] when the file is absent or lists
/// nothing, [`AuditError::TruncatedSelection`] when `count` disagrees with the
/// entries, [`AuditError::Read`] when it exists but cannot be read, and
/// [`AuditError::Parse`] when it does not match the schema — including when
/// `count` is absent, since a file without it cannot be checked at all.
pub fn load_selection(work: &WorkDir) -> Result<Vec<SelectedRepo>, AuditError> {
    let path = selection_path(work);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AuditError::NoRepositoriesSelected { path });
        }
        Err(source) => return Err(AuditError::Read { path, source }),
    };
    let selection: Selection = toml::from_str(&text).map_err(|source| AuditError::Parse {
        path: path.clone(),
        what: "repository selection",
        source: Box::new(source),
    })?;
    if selection.repositories.is_empty() {
        return Err(AuditError::NoRepositoriesSelected { path });
    }
    // #5555: a prefix of a crashed write parses cleanly; only the count catches it.
    if selection.count != selection.repositories.len() {
        return Err(AuditError::TruncatedSelection {
            path,
            declared: selection.count,
            found: selection.repositories.len(),
        });
    }
    Ok(selection.repositories)
}

/// Record the repositories the next sweep should audit.
///
/// Why: [`SELECTION_FILE`] states two obligations on whoever writes it, and
/// #5556 found there was no writer at all — `taudit clone` acquired the
/// checkouts, `taudit run` then refused with "nothing to audit", and every
/// per-stage test passed throughout. The writer lives beside the reader so the
/// atomic-rename and `count`-first obligations are one decision rather than a
/// note each producer re-reads; #5497's picker writes through here too.
/// What: renders `count` ahead of the entries (serde field order, and `toml`
/// emits values before tables), writes to a uniquely-named temporary file in
/// the same directory, and renames it into place. The unique name is what lets
/// two writers race without either reading the other's half-written file.
/// Test: `super::run_tests::a_saved_selection_reads_back_whole`,
/// `super::run_tests::racing_writers_never_leave_a_torn_selection`.
///
/// # Errors
///
/// [`AuditError::WorkDir`] when the state area cannot be made, the temporary
/// file cannot be written, or the rename fails.
pub fn save_selection(work: &WorkDir, repos: &[SelectedRepo]) -> Result<(), AuditError> {
    let path = selection_path(work);
    let dir = work.path(Area::State);
    std::fs::create_dir_all(&dir).map_err(|source| AuditError::WorkDir { path: dir, source })?;

    let selection = Selection {
        count: repos.len(),
        repositories: repos.to_vec(),
    };
    let text = toml::to_string_pretty(&selection).map_err(|e| AuditError::WorkDir {
        path: path.clone(),
        source: std::io::Error::other(e),
    })?;

    let temp = path.with_file_name(format!("{SELECTION_FILE}.{}.tmp", writer_tag()));
    std::fs::write(&temp, text).map_err(|source| AuditError::WorkDir {
        path: temp.clone(),
        source,
    })?;
    std::fs::rename(&temp, &path).map_err(|source| {
        let _ = std::fs::remove_file(&temp);
        AuditError::WorkDir { path, source }
    })
}

/// A suffix no two concurrent writers share: process, plus thread within it.
fn writer_tag() -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut hasher);
    format!("{}-{}", std::process::id(), hasher.finish())
}
