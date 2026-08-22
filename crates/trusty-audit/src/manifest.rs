//! Reading tga's `manifest.toml`.
//!
//! Why: `tga audit` already writes `manifest.toml` into its output directory
//! (DOC-67 §6) — engagement metadata plus one `[[repositories]]` entry per
//! configured repo. #5502 is explicit that `trusty-audit` READS that file rather
//! than keeping a second copy of the same facts: two writers of one schema drift,
//! and the drift would show up as a report naming repositories the run never
//! touched.
//!
//! What: a deliberately partial mirror of the schema — `[report]`'s metadata and
//! the repository list, nothing else. Unknown keys are tolerated by omission,
//! because trusty-review's manifest accepts more keys than tga writes and a
//! stricter reader here would reject a valid file.
//!
//! What this file does NOT carry, and why it matters for what a caller may
//! assume: it is written ONCE, after the whole sweep completes, and records the
//! CONFIGURED repository set — not per-repo or per-phase completion. Run
//! progress is a separate record this crate owns (#5494); it is not here.
//! Test: `super::manifest_tests`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::AuditError;

/// The subset of tga's `manifest.toml` this crate reads.
///
/// Why: see the module docs — a partial mirror, not a second definition of the
/// schema. tga owns the writer (`tga::report::dd_manifest`).
/// What: the `[report]` section plus the `[[repositories]]` list.
/// Test: `super::manifest_tests::reads_a_manifest_tga_would_write`.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct AuditManifest {
    /// Engagement metadata.
    pub report: ReportSection,
    /// The inference identity the run that wrote this manifest used (#6135).
    ///
    /// Absent in every manifest written before the key existed, and in any
    /// hand-written one — which is why a render still resolves its own provider
    /// when it is missing rather than refusing.
    #[serde(default)]
    pub inference: Option<InferenceSection>,
    /// The configured repository set, in config order.
    #[serde(default)]
    pub repositories: Vec<RepositoryEntry>,
}

/// The `[inference]` section: which provider and models rendered this report.
///
/// Why: `crate::inference::write_into_manifest` writes it and
/// `trusty-review` resolves from it (#6135). This crate reads it back for one
/// purpose — stating in `index.md` which models a re-render will actually use,
/// since the manifest outranks anything the re-render itself would inject.
/// What: four identity strings, never a credential.
/// Test: `super::manifest_tests::reads_the_inference_section`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct InferenceSection {
    /// Provider id.
    #[serde(default)]
    pub provider: Option<String>,
    /// Reviewer role model id.
    #[serde(default)]
    pub reviewer: Option<String>,
    /// Verifier role model id.
    #[serde(default)]
    pub verifier: Option<String>,
    /// Summarizer role model id.
    #[serde(default)]
    pub summarizer: Option<String>,
}

/// The `[report]` section: who the engagement is for and what it is called.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ReportSection {
    /// Report title.
    pub title: String,
    /// Analyst producing the report, when set.
    #[serde(default)]
    pub analyst: Option<String>,
    /// Client the report is produced for, when set.
    #[serde(default)]
    pub client: Option<String>,
    /// Areas the run could not assess (DOC-67 §9).
    #[serde(default)]
    pub gaps: Vec<String>,
}

/// One configured repository.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct RepositoryEntry {
    /// Display name.
    pub name: String,
    /// Local checkout path the audit scans.
    pub path: PathBuf,
}

impl AuditManifest {
    /// Filename `tga audit` writes (DOC-67 §6).
    pub const FILE_NAME: &'static str = "manifest.toml";

    /// Parse a manifest from TOML text.
    ///
    /// # Errors
    ///
    /// [`AuditError::Parse`] when the text does not match the schema above.
    pub fn from_toml(text: &str, path: &Path) -> Result<Self, AuditError> {
        toml::from_str(text).map_err(|source| AuditError::Parse {
            path: path.to_path_buf(),
            what: "audit manifest",
            source: Box::new(source),
        })
    }

    /// Read and parse the manifest at `path`.
    ///
    /// # Errors
    ///
    /// [`AuditError::Read`] when the file cannot be read, [`AuditError::Parse`]
    /// when its contents do not match the schema.
    pub fn load(path: &Path) -> Result<Self, AuditError> {
        let text = std::fs::read_to_string(path).map_err(|source| AuditError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml(&text, path)
    }

    /// Load the manifest if it exists, reporting `Ok(None)` when it does not.
    ///
    /// Why: the guided flow runs before any sweep has produced a manifest, so
    /// "absent" is an ordinary state to report, not a failure. An unreadable or
    /// malformed file is still an error — the distinction is between "no run has
    /// happened" and "a run happened and left something we cannot read".
    /// What: `NotFound` becomes `Ok(None)`; every other error propagates.
    /// Test: `super::manifest_tests::an_absent_manifest_is_not_an_error`.
    ///
    /// # Errors
    ///
    /// [`AuditError::Read`] for any read failure other than not-found, and
    /// [`AuditError::Parse`] for a malformed file.
    pub fn load_if_present(path: &Path) -> Result<Option<Self>, AuditError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text, path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(AuditError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    /// Shaped after what `tga::report::dd_manifest::DdManifest::to_toml` emits.
    const SAMPLE: &str = r#"
[report]
title = "Acme — Technical Due Diligence"
analyst = "Trusty"
client = "Acme"
gaps = ["Two repositories could not be cloned."]

[[repositories]]
name = "acme-api"
path = "/work/repos/acme-api"

[[repositories]]
name = "acme-web"
path = "/work/repos/acme-web"
"#;

    #[test]
    fn reads_a_manifest_tga_would_write() {
        let manifest =
            AuditManifest::from_toml(SAMPLE, Path::new("manifest.toml")).expect("parses");
        assert_eq!(manifest.report.title, "Acme — Technical Due Diligence");
        assert_eq!(manifest.report.client.as_deref(), Some("Acme"));
        assert_eq!(manifest.report.gaps.len(), 1);
        assert_eq!(
            manifest
                .repositories
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["acme-api", "acme-web"]
        );
    }

    /// Why: #6135 — the section a sweep writes is what a re-render resolves
    /// from, and `index.md` states it, so this reader has to see it.
    /// What: a manifest carrying the section, and one without.
    /// Test: this test itself.
    #[test]
    fn reads_the_inference_section() {
        let text = format!(
            "{SAMPLE}\n[inference]\nprovider = \"openrouter\"\n\
             reviewer = \"anthropic/claude-opus-4.8\"\n\
             verifier = \"anthropic/claude-haiku-4.5\"\n\
             summarizer = \"anthropic/claude-haiku-4.5\"\n"
        );
        let manifest = AuditManifest::from_toml(&text, Path::new("manifest.toml")).expect("parses");
        let inference = manifest.inference.expect("declared");
        assert_eq!(inference.provider.as_deref(), Some("openrouter"));
        assert_eq!(
            inference.reviewer.as_deref(),
            Some("anthropic/claude-opus-4.8")
        );

        let older = AuditManifest::from_toml(SAMPLE, Path::new("manifest.toml")).expect("parses");
        assert!(
            older.inference.is_none(),
            "a manifest written before the key existed still loads"
        );
    }

    #[test]
    fn optional_metadata_may_be_absent() {
        let text = "[report]\ntitle = \"Untitled\"\n";
        let manifest = AuditManifest::from_toml(text, Path::new("manifest.toml")).expect("parses");
        assert!(manifest.report.analyst.is_none());
        assert!(manifest.repositories.is_empty());
    }

    #[test]
    fn keys_this_reader_ignores_do_not_break_the_load() {
        let text = format!("{SAMPLE}\n[report.extra]\nfuture_key = 1\n");
        AuditManifest::from_toml(&text, Path::new("manifest.toml"))
            .expect("a richer manifest still loads");
    }

    #[test]
    fn an_absent_manifest_is_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("manifest.toml");
        assert!(
            AuditManifest::load_if_present(&missing)
                .expect("absent is fine")
                .is_none()
        );
    }

    #[test]
    fn a_malformed_manifest_is_still_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, "this is not toml = = =").expect("write");
        let err = AuditManifest::load_if_present(&path).expect_err("malformed must fail");
        assert!(matches!(err, AuditError::Parse { .. }));
    }
}
