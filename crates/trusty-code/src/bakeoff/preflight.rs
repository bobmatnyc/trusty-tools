//! Bundle loading and the five rejections #5441's preflight owes.
//!
//! Why: #5441 asks for "a lightweight preflight that rejects stale runner
//! paths, missing provenance, mock-only evidence, incomplete L1-L3 coverage,
//! and results produced by a different Trusty Code build". Those five are
//! stated as properties of retained evidence, so all five are decidable
//! offline from the bundle plus a handful of pins — no model, no runner, no
//! network.
//! What: [`load_bundle`] reads `<root>/L1..L3`, and [`preflight`] applies the
//! per-level rules (artifacts present and non-empty, metadata parses and agrees
//! with its own `tcode_report.json`, evidence is real, provenance is complete,
//! neither checkout was dirty) and the cross-level rules (one build, one
//! runner, one challenge revision, one set of source digests) plus the caller's
//! [`Pins`]. Reading is deliberately tolerant of extra files and extra JSON
//! keys, and intolerant of absent ones.
//! Test: `bakeoff::tests::*` — one test per rule.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::bakeoff::metadata::LevelMetadata;
use crate::bakeoff::{GateReport, LEVELS, REQUIRED_ARTIFACTS, Rule, Violation};

/// Identities the caller is asserting the evidence must match.
///
/// Why: cross-level agreement proves the three levels used ONE build and ONE
/// runner, but not that it was the FROZEN candidate. Only the operator knows
/// which commit was frozen, so they pin it; an unpinned field is simply not
/// checked rather than guessed at.
/// What: three optional expected values, compared verbatim against every
/// level's metadata.
/// Test: `bakeoff::tests::a_pinned_commit_mismatch_is_a_build_mismatch`,
/// `bakeoff::tests::a_pinned_runner_revision_mismatch_is_a_stale_runner`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Pins {
    /// The frozen candidate's git commit.
    pub commit: Option<String>,
    /// SHA-256 of the binary built from that commit.
    pub binary_sha256: Option<String>,
    /// The bake-off runner revision the invocation was defined against.
    pub runner_revision: Option<String>,
}

impl Pins {
    /// Build a pin set from the three optional expectations.
    ///
    /// Why: [`Pins`] is `#[non_exhaustive]`, so the `tcode` binary — a separate
    /// crate from this library — cannot use a struct literal. A constructor
    /// keeps the CLI wiring to one call and leaves the struct free to grow.
    /// What: moves the three arguments in, in the order the CLI flags are
    /// declared.
    /// Test: `bakeoff::tests::a_matching_pin_set_passes`.
    pub fn new(
        commit: Option<String>,
        binary_sha256: Option<String>,
        runner_revision: Option<String>,
    ) -> Self {
        Self {
            commit,
            binary_sha256,
            runner_revision,
        }
    }
}

/// One level's readable evidence.
///
/// Why: the comparison stage needs the same parsed metadata the preflight read,
/// and re-reading it would let the two stages disagree about what is on disk.
/// What: the level directory, its parsed `metadata.json`, and its raw
/// `tcode_report.json` value.
/// Test: `bakeoff::tests::a_complete_bundle_passes_preflight`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LevelEvidence {
    /// `<bundle>/L<n>`.
    pub dir: PathBuf,
    /// The parsed retained metadata.
    pub metadata: LevelMetadata,
    /// The level's raw run report, as parsed JSON.
    pub report: serde_json::Value,
}

/// A retained bake-off bundle, as far as it could be read.
///
/// Why: a bundle that fails preflight is still worth carrying around — the
/// report names which levels were readable, and a caller comparing against a
/// baseline needs whatever parsed.
/// What: the root path, the levels that parsed, and the violations produced
/// while reading.
/// Test: `bakeoff::tests::a_missing_level_is_incomplete_coverage`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Bundle {
    /// The bundle root the caller named.
    pub root: PathBuf,
    /// Levels that parsed, keyed by level number.
    pub levels: BTreeMap<u8, LevelEvidence>,
    /// Findings recorded while reading the bundle.
    pub read_violations: Vec<Violation>,
}

/// Read `<root>/L1`, `<root>/L2` and `<root>/L3`.
///
/// Why: separating reading from judging lets the baseline bundle be loaded
/// with the same code that loads the candidate, without subjecting a
/// previously-accepted baseline to a fresh preflight it was never gated on.
/// What: for each of [`LEVELS`], requires the directory and every entry of
/// [`REQUIRED_ARTIFACTS`] to exist and be non-empty, then parses
/// `metadata.json` and `tcode_report.json`. A level that fails any of those
/// records a violation and is omitted from `levels`.
/// Test: `bakeoff::tests::a_missing_level_is_incomplete_coverage`,
/// `bakeoff::tests::an_empty_artifact_is_rejected`,
/// `bakeoff::tests::malformed_metadata_is_rejected`.
pub fn load_bundle(root: &Path) -> Bundle {
    let mut levels = BTreeMap::new();
    let mut read_violations = Vec::new();

    for level in LEVELS {
        let dir = root.join(format!("L{level}"));
        if !dir.is_dir() {
            read_violations.push(Violation::level(
                Rule::IncompleteCoverage,
                level,
                format!("no level directory at {}", dir.display()),
            ));
            continue;
        }

        let mut artifacts_ok = true;
        for artifact in REQUIRED_ARTIFACTS {
            let path = dir.join(artifact);
            match std::fs::metadata(&path) {
                Ok(meta) if meta.len() > 0 => {}
                Ok(_) => {
                    artifacts_ok = false;
                    read_violations.push(Violation::level(
                        Rule::MissingArtifact,
                        level,
                        format!("{artifact} is empty"),
                    ));
                }
                Err(e) => {
                    artifacts_ok = false;
                    read_violations.push(Violation::level(
                        Rule::MissingArtifact,
                        level,
                        format!("{artifact} unreadable: {e}"),
                    ));
                }
            }
        }
        if !artifacts_ok {
            continue;
        }

        let metadata = match read_json::<LevelMetadata>(&dir.join("metadata.json")) {
            Ok(m) => m,
            Err(detail) => {
                read_violations.push(Violation::level(Rule::MalformedMetadata, level, detail));
                continue;
            }
        };
        let report = match read_json::<serde_json::Value>(&dir.join("tcode_report.json")) {
            Ok(r) => r,
            Err(detail) => {
                read_violations.push(Violation::level(Rule::MalformedMetadata, level, detail));
                continue;
            }
        };

        levels.insert(
            level,
            LevelEvidence {
                dir,
                metadata,
                report,
            },
        );
    }

    Bundle {
        root: root.to_path_buf(),
        levels,
        read_violations,
    }
}

/// Apply every preflight rule to a candidate bundle.
///
/// Why: this is the "lightweight preflight" #5441 asks for, and it runs before
/// any baseline comparison — comparing numbers produced by an unidentifiable
/// build would only launder the problem into a delta table.
/// What: replays the read violations, then per level checks the declared level
/// number, evidence mode, verifier check count, provenance completeness,
/// checkout cleanliness, the metadata/report build and status agreement, and
/// the caller's [`Pins`]; then checks that all readable levels agree on build,
/// runner, challenge revision, and source digests.
/// Test: `bakeoff::tests::a_complete_bundle_passes_preflight` and the
/// per-rule rejection tests beside it.
pub fn preflight(bundle: &Bundle, pins: &Pins) -> GateReport {
    let mut report = GateReport {
        levels: bundle.levels.keys().copied().collect(),
        violations: bundle.read_violations.clone(),
        notes: Vec::new(),
    };

    for (&level, evidence) in &bundle.levels {
        check_level(level, evidence, pins, &mut report);
    }
    check_cross_level(bundle, &mut report);

    if report.levels.len() == LEVELS.len() {
        report.notes.push(format!(
            "read {} level(s) from {}",
            LEVELS.len(),
            bundle.root.display()
        ));
    }
    report
}

/// Every rule that a single level can violate on its own.
///
/// Why: keeps [`preflight`] readable as the list of stages it runs, with the
/// per-level detail one hop away.
/// What: pushes violations onto `report` for the level's declared number,
/// evidence mode, verifier coverage, provenance, checkout cleanliness,
/// metadata/report agreement, and pin mismatches.
/// Test: the per-rule rejection tests in `bakeoff::tests`.
fn check_level(level: u8, evidence: &LevelEvidence, pins: &Pins, report: &mut GateReport) {
    let meta = &evidence.metadata;

    if meta.level != level {
        report.violations.push(Violation::level(
            Rule::MalformedMetadata,
            level,
            format!(
                "metadata declares level {} but sits in L{level}",
                meta.level
            ),
        ));
    }

    if !meta.evidence_mode.is_real() {
        report.violations.push(Violation::level(
            Rule::MockEvidence,
            level,
            format!(
                "evidence_mode is {:?}; only real runs satisfy the exit gate",
                meta.evidence_mode
            ),
        ));
    }
    if meta.verifier.checks_total == 0 {
        report.violations.push(Violation::level(
            Rule::MockEvidence,
            level,
            "verifier recorded 0 checks — no correctness evidence".to_string(),
        ));
    }

    let gaps = meta.provenance_gaps();
    if !gaps.is_empty() {
        report.violations.push(Violation::level(
            Rule::MissingProvenance,
            level,
            format!("unusable provenance field(s): {}", gaps.join(", ")),
        ));
    }

    if meta.runner.dirty {
        report.violations.push(Violation::level(
            Rule::DirtyCheckout,
            level,
            format!(
                "runner checkout at revision {} was dirty; the recorded revision does not identify what ran",
                meta.runner.revision
            ),
        ));
    }
    if meta.build.dirty {
        report.violations.push(Violation::level(
            Rule::DirtyCheckout,
            level,
            format!(
                "candidate checkout at commit {} was dirty; the recorded commit does not identify the binary",
                meta.build.commit
            ),
        ));
    }

    check_report_agreement(level, evidence, report);

    if let Some(expected) = &pins.commit
        && &meta.build.commit != expected
    {
        report.violations.push(Violation::level(
            Rule::BuildMismatch,
            level,
            format!(
                "build.commit is {} but the pinned candidate is {expected}",
                meta.build.commit
            ),
        ));
    }
    if let Some(expected) = &pins.binary_sha256
        && &meta.build.binary_sha256 != expected
    {
        report.violations.push(Violation::level(
            Rule::BuildMismatch,
            level,
            format!(
                "build.binary_sha256 is {} but the pinned candidate binary is {expected}",
                meta.build.binary_sha256
            ),
        ));
    }
    if let Some(expected) = &pins.runner_revision
        && &meta.runner.revision != expected
    {
        report.violations.push(Violation::level(
            Rule::StaleRunner,
            level,
            format!(
                "runner.revision is {} but the pinned runner is {expected}",
                meta.runner.revision
            ),
        ));
    }
}

/// Cross-check `metadata.json` against the level's own `tcode_report.json`.
///
/// Why: the metadata is written by the external runner and the report by the
/// binary under test. Requiring them to agree is what makes the metadata
/// evidence rather than an unverified assertion — a runner that stamped the
/// wrong commit, or reran only some levels against a rebuilt binary, is caught
/// here and nowhere else.
/// What: compares `build.version`/`commit`/`commit_date` and `status` against
/// the report's own values, ignoring report keys the gate does not read.
/// Test: `bakeoff::tests::report_build_mismatch_is_rejected`,
/// `bakeoff::tests::metadata_status_must_match_the_run_report`.
fn check_report_agreement(level: u8, evidence: &LevelEvidence, report: &mut GateReport) {
    let meta = &evidence.metadata;
    let reported = evidence.report.get("build");
    for (field, expected) in [
        ("version", &meta.build.version),
        ("commit", &meta.build.commit),
        ("commit_date", &meta.build.commit_date),
    ] {
        let actual = reported.and_then(|b| b.get(field)).and_then(|v| v.as_str());
        if actual != Some(expected.as_str()) {
            report.violations.push(Violation::level(
                Rule::BuildMismatch,
                level,
                format!(
                    "tcode_report.json build.{field} is {} but metadata says {expected}",
                    actual.unwrap_or("absent")
                ),
            ));
        }
    }

    let reported_status = evidence.report.get("status").and_then(|v| v.as_str());
    if reported_status != Some(meta.run.status.as_str()) {
        report.violations.push(Violation::level(
            Rule::MalformedMetadata,
            level,
            format!(
                "tcode_report.json status is {} but metadata says {}",
                reported_status.unwrap_or("absent"),
                meta.run.status
            ),
        ));
    }
}

/// Require every readable level to describe the same experiment.
///
/// Why: three levels run against three different builds, runners, or challenge
/// revisions are three experiments, and comparing their aggregate against a
/// baseline is meaningless. This is the "results produced by a different Trusty
/// Code build" rejection in its cross-level form — the one an operator is most
/// likely to hit by rerunning a single failed level after a rebuild.
/// What: compares each level's build identity, runner path/revision, challenge
/// revision, and source digests against the lowest-numbered readable level.
/// Test: `bakeoff::tests::build_drift_across_levels_is_rejected`,
/// `bakeoff::tests::source_digest_drift_across_levels_is_rejected`.
fn check_cross_level(bundle: &Bundle, report: &mut GateReport) {
    let mut iter = bundle.levels.iter();
    let Some((&first_level, first)) = iter.next() else {
        return;
    };
    let reference = &first.metadata;

    for (&level, evidence) in iter {
        let meta = &evidence.metadata;
        if meta.build_identity() != reference.build_identity() {
            report.violations.push(Violation::bundle(
                Rule::BuildMismatch,
                format!(
                    "L{level} was produced by build {} but L{first_level} by {}",
                    meta.build_identity(),
                    reference.build_identity()
                ),
            ));
        }
        if meta.runner.path != reference.runner.path
            || meta.runner.revision != reference.runner.revision
        {
            report.violations.push(Violation::bundle(
                Rule::StaleRunner,
                format!(
                    "L{level} used runner {}@{} but L{first_level} used {}@{}",
                    meta.runner.path,
                    meta.runner.revision,
                    reference.runner.path,
                    reference.runner.revision
                ),
            ));
        }
        if meta.challenge_revision != reference.challenge_revision {
            report.violations.push(Violation::bundle(
                Rule::StaleRunner,
                format!(
                    "L{level} ran challenge revision {} but L{first_level} ran {}",
                    meta.challenge_revision, reference.challenge_revision
                ),
            ));
        }
        if meta.source_digests != reference.source_digests {
            report.violations.push(Violation::bundle(
                Rule::StaleRunner,
                format!(
                    "L{level} consumed different instruction/agent/skill sources than L{first_level}"
                ),
            ));
        }
    }
}

/// Read and parse one JSON file, reporting the path in any error.
///
/// Why: a bare serde error names a line and column but not the file, and this
/// gate reads twelve JSON files across two bundles.
/// What: returns the parsed value, or a one-line human detail on any I/O or
/// parse failure.
/// Test: `bakeoff::tests::malformed_metadata_is_rejected`.
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("{} unreadable: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("{} is not valid: {e}", path.display()))
}
