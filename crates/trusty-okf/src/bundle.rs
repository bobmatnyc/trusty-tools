//! Bundle operations: open, get, put, verify, move, `index.md` regeneration,
//! `log.md` appends, path confinement and graph read (SPEC-OKF-01 §2.3,
//! §2.10, §2.11, §4.3, §4.6).
//!
//! Why: every write to a bundle must stay under its root, keep the bytes it
//! does not own, refuse to replace bytes it never read, and keep the
//! verification record honest. One type, opened from a root path, carries
//! all of it, so no entry point derives a path from anything else (B3).
//! What: [`Bundle::open`] canonicalises an existing root and reads every
//! concept leniently, returning findings instead of failing; [`Bundle::get`]
//! reads one concept. The writes live in submodules: [`put`] (`put`,
//! `verify`, §2.10), [`moves`] (`move_concept`), [`index_md`]
//! (`regenerate_indexes`), [`log_md`], [`record`] (the verification record
//! and axis), [`paths`] (§2.11), [`graph`] (graph read). Slice S3 of #10746;
//! `create`, `supersede`, `deprecate`, `adopt`, `migrate`, recovery and the
//! operation journal come with S5 and S6.
//! Test: `c01_bundle_open_is_ok_on_the_google_bundles`,
//! `c54_open_on_a_missing_root_is_an_error`,
//! `c115_bundle_open_reads_foreign_files_and_writes_never_touch_them`,
//! `c36_out_of_band_edit_reads_unverified_with_a_warning`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::OkfError;
use crate::format::concept::{Concept, parse_concept, parse_concept_at};
use crate::format::hash::content_hash;
use crate::format::id::{ConceptId, ConceptRef};
use crate::format::validate::{Finding, FindingKind, Severity};

pub mod graph;
pub mod index_md;
pub mod log_md;
pub mod moves;
pub mod paths;
pub mod put;
pub mod record;
pub(crate) mod walk;

pub use index_md::IndexReport;
pub use moves::{MoveReport, UnmovedLink};
pub use put::{HumanConfirmation, PutOutcome};
pub use record::{VerificationEvent, VerificationState};

/// One OKF bundle, opened from its root directory.
#[derive(Debug, Clone)]
pub struct Bundle {
    /// The canonical root directory.
    root: PathBuf,
}

impl Bundle {
    /// Open a bundle leniently (SPEC-OKF-01 §2.2, §4.3).
    ///
    /// Why: a read never fails on a finding (§4.4); a non-conformant bundle
    /// still opens and reports what is wrong (OKF §12).
    /// What: the root must exist and be a directory (§2.11 rule 1), else
    /// the I/O error or `PathEscape`. The walk never follows links. Each file
    /// adds its findings: an info per foreign path, a warning per symbolic
    /// link, an error per unreadable or unparseable file, the read warnings
    /// of each concept, a warning per fold-key group of two or more files
    /// (§2.11), and a warning per concept whose recorded verification names
    /// another content hash (§2.10 "Out-of-band edits").
    /// Test: `c01_bundle_open_is_ok_on_the_google_bundles`,
    /// `c54_open_on_a_missing_root_is_an_error`,
    /// `c115_bundle_open_reads_foreign_files_and_writes_never_touch_them`,
    /// `c36_out_of_band_edit_reads_unverified_with_a_warning`.
    pub fn open(root: &Path) -> Result<(Bundle, Vec<Finding>), OkfError> {
        let bundle = Bundle {
            root: paths::canonical_root(root)?,
        };
        let scan = walk::scan(&bundle.root)?;
        let mut findings = scan.findings;
        let mut folds: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in &scan.files {
            if matches!(entry.r, ConceptRef::Id(_)) {
                folds
                    .entry(paths::fold_key(&entry.rel))
                    .or_default()
                    .push(entry.rel.clone());
            }
            let text = match std::fs::read_to_string(bundle.root.join(&entry.rel)) {
                Ok(t) => t,
                Err(e) => {
                    findings.push(Finding::new(
                        Severity::Error,
                        FindingKind::UnreadableFile,
                        entry.rel.clone(),
                        format!("cannot read as text: {e}"),
                        "SPEC-OKF-01 §4.4",
                    ));
                    continue;
                }
            };
            match parse_concept_at(entry.r.clone(), &text) {
                Ok((c, read)) => {
                    findings.extend(read);
                    if let ConceptRef::Id(id) = &entry.r {
                        findings.extend(bundle.mismatch_finding(id, &c)?);
                    }
                }
                Err(e) => findings.push(Finding::parse_error(entry.rel.clone(), e.to_string())),
            }
        }
        for paths_in_group in folds.values().filter(|g| g.len() > 1) {
            findings.push(Finding::new(
                Severity::Warning,
                FindingKind::FoldCollision,
                paths_in_group[0].clone(),
                format!(
                    "files share one fold key and may address one file on a \
                     case-insensitive file system: {}",
                    paths_in_group.join(", ")
                ),
                "SPEC-OKF-01 §2.11",
            ));
        }
        Ok((bundle, findings))
    }

    /// The canonical root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read one concept (SPEC-OKF-01 §4.3).
    ///
    /// What: the path is confined (no link crossed); a missing file is
    /// [`OkfError::NotFound`]; unparseable frontmatter is
    /// [`OkfError::Parse`]. Read findings are dropped; `open` reports them.
    /// Test: `c31_body_change_moves_verification_to_the_record`.
    pub fn get(&self, id: &ConceptId) -> Result<Concept, OkfError> {
        let path = paths::confined_file(&self.root, &id.file_name())?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(OkfError::NotFound(id.clone()));
            }
            Err(e) => return Err(paths::io_err(&path, e)),
        };
        parse_concept(id.clone(), &text).map(|(c, _)| c)
    }

    /// The concept IDs on disk, sorted; foreign files excluded.
    ///
    /// Test: `c09_move_rewrites_every_link_and_logs_one_entry`.
    pub fn concept_ids(&self) -> Result<Vec<ConceptId>, OkfError> {
        Ok(walk::scan(&self.root)?
            .files
            .into_iter()
            .filter_map(|e| match e.r {
                ConceptRef::Id(id) => Some(id),
                ConceptRef::Foreign(_) => None,
            })
            .collect())
    }

    /// The §2.10 out-of-band warning for one read concept, if any.
    fn mismatch_finding(&self, id: &ConceptId, c: &Concept) -> Result<Option<Finding>, OkfError> {
        let events = record::fold(record::read_lines(self, id)?);
        if !record::has_mismatch(&events, &content_hash(c)) {
            return Ok(None);
        }
        let mut f = Finding::new(
            Severity::Warning,
            FindingKind::VerificationMismatch,
            id.file_name(),
            "content changed since its recorded verification; the verification state \
             reads Unverified until the next write or verify"
                .to_string(),
            "SPEC-OKF-01 §2.10",
        );
        f.concept = Some(id.clone());
        Ok(Some(f))
    }
}
