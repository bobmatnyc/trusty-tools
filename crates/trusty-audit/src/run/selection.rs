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
use crate::workdir::{self, Area, WorkDir};

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

/// Whether this repository's GitHub issues can be reached, and if not, why.
///
/// Why (#6130): `SelectedRepo::name` is the on-disk identity, and for a
/// local-path target that is `local/<name>` — an owner GitHub does not have.
/// Handing it to tga as `github.repo` asked `api.github.com/repos/local/…` for
/// every work item and 404'd all 3152 of them, which failed the `collect`
/// stage closed and stopped the audit before it could package. The issue
/// identity is therefore its own field, resolved from the source checkout's
/// `origin` remote, and its ABSENCE is recorded rather than inferred.
/// What: [`Present`](Self::Present) carries the `owner/repo` tga queries;
/// [`Absent`](Self::Absent) carries the sentence the report's Gaps section
/// prints. There is deliberately no "unknown" — see
/// [`SelectedRepo::github_leg`] for how a selection file written before this
/// field existed is resolved into one of the two.
/// Test: `super::run_tests::a_local_repo_with_no_github_remote_declares_the_leg_absent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubLeg<'a> {
    /// GitHub issues for this repository live under this `owner/repo`.
    Present(&'a str),
    /// No GitHub identity — the leg is declared absent, for this reason.
    Absent(&'a str),
}

/// What a pre-#6130 selection file's `local/<name>` entry resolves to.
///
/// A selection written before the slug was resolved carries neither field, and
/// its `local/` owner is the only evidence left that the target was a path on
/// disk. Reading that as a declared absence is the conservative half of the
/// fork: the alternative is querying `local/<name>` again.
const LEGACY_LOCAL_ABSENCE: &str = "this repository was selected as a path on disk by an earlier \
run, which recorded no GitHub identity for it";

/// One repository the run was asked to audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SelectedRepo {
    /// Display name, used for the output directory and the log file.
    pub name: String,
    /// Checkout path. Relative paths anchor to the working-directory root.
    pub path: PathBuf,
    /// The `owner/repo` this repository's GitHub issues live under (#6130).
    ///
    /// Set by `crate::clone` from the acquisition source: the request entry
    /// itself for a remote, the source checkout's `origin` remote for a local
    /// path. Read through [`Self::github_leg`], never directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_slug: Option<String>,
    /// Why there is no [`Self::github_slug`], when there is none (#6130).
    ///
    /// Set exactly when the resolution ran and found no GitHub identity, so an
    /// absence carries a sentence the report can print instead of a silence the
    /// report cannot distinguish from a clean result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_absent: Option<String>,
}

impl SelectedRepo {
    /// This repository's GitHub-issue identity, or the reason it has none.
    ///
    /// Why: [`Self::github_slug`] and [`Self::github_absent`] are the wire
    /// format — two optional strings that must agree — and this is the single
    /// place that turns them into the one answer callers act on, so the
    /// agreement is enforced at the read rather than trusted at every write.
    /// What: a set slug wins; a recorded reason is next; and a selection file
    /// written before either field existed falls back on the only evidence it
    /// left — an entry acquired under [`crate::local_repo::LOCAL_OWNER`] was a
    /// path on disk and gets [`LEGACY_LOCAL_ABSENCE`], while any other name IS
    /// the `owner/repo` it was cloned from.
    /// Test: `super::run_tests::a_legacy_selection_still_resolves_both_shapes`.
    pub fn github_leg(&self) -> GithubLeg<'_> {
        if let Some(slug) = self.github_slug.as_deref() {
            return GithubLeg::Present(slug);
        }
        if let Some(reason) = self.github_absent.as_deref() {
            return GithubLeg::Absent(reason);
        }
        if self.name.split('/').next() == Some(crate::local_repo::LOCAL_OWNER) {
            GithubLeg::Absent(LEGACY_LOCAL_ABSENCE)
        } else {
            GithubLeg::Present(&self.name)
        }
    }
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
/// emits values before tables), then publishes through
/// [`workdir::write_atomically`], which owns the temporary-file-and-rename
/// obligation for every state document this crate writes (#5494).
/// Test: `super::run_tests::a_saved_selection_reads_back_whole`,
/// `super::run_tests::racing_writers_never_leave_a_torn_selection`.
///
/// # Errors
///
/// [`AuditError::WorkDir`] when the state area cannot be made, the temporary
/// file cannot be written, or the rename fails.
pub fn save_selection(work: &WorkDir, repos: &[SelectedRepo]) -> Result<(), AuditError> {
    let path = selection_path(work);
    let selection = Selection {
        count: repos.len(),
        repositories: repos.to_vec(),
    };
    let text = toml::to_string_pretty(&selection).map_err(|e| AuditError::WorkDir {
        path: path.clone(),
        source: std::io::Error::other(e),
    })?;
    // #5822: the temp-file-then-rename discipline moved to `workdir` when the
    // target registry became a second state file owing the same guarantee.
    workdir::write_atomically(&path, &text)
}
