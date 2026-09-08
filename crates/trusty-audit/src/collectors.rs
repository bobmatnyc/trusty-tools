//! Preflight for OPTIONAL collector binaries, before a sweep starts.
//!
//! Why (#7134): a 59-repository, 5h03m client engagement ran with zero
//! secrets-scan coverage because `gitleaks` was not installed on the
//! operator's machine. That gap was disclosed correctly — each collector
//! fails open and names itself in a `[report].gaps` line
//! ([`crate::grounding::secrets`], [`crate::grounding::cve`],
//! [`crate::grounding::license`]) — but only after an hours-long sweep had
//! already cloned and collected every repository. Nothing looked BEFORE the
//! sweep started.
//!
//! What: [`missing`], which resolves every optional collector's binary
//! through [`trusty_common::bin_resolve::resolve_binary`] — the same
//! resolver each collector already calls at collection time (see each
//! module's `BINARY` constant), so this reports exactly what will go dark,
//! never a second opinion on where a binary lives — and [`check`], which
//! turns that list into a warning by default (the sweep still runs; each
//! repository's own `[report].gaps` line is unchanged) or a refusal under
//! `--strict-collectors`, run in [`crate::chain::Phase::Preflight`] before
//! [`crate::clone::clone_all`] clones anything.
//!
//! Test: `collectors_tests`.

use std::path::PathBuf;

use crate::error::AuditError;
use crate::grounding::{cve, license, secrets};

/// One optional collector a sweep can run, and what goes dark without it.
///
/// Why: a shared row shape rather than three ad hoc checks, so a fourth
/// fail-open collector is one entry in [`ALL`] and nothing else changes.
/// What: the collector's report name, the binary it resolves on `PATH`, the
/// evidence dimension that is not covered without it, and how to install it.
/// Test: `collectors_tests::all_names_every_fail_open_collector`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionalCollector {
    /// The collector's name in a `[report].gaps` line, e.g. `secrets-scan`.
    pub collector: &'static str,
    /// The binary this collector resolves on `PATH`, e.g. `gitleaks`.
    pub binary: &'static str,
    /// The evidence dimension that goes unassessed without it, one clause.
    pub dimension: &'static str,
    /// How an operator installs it, e.g. `brew install gitleaks`.
    pub install_hint: &'static str,
}

/// Every optional collector a sweep can run, in report order.
///
/// Why: one row per collector that resolves its binary through
/// [`trusty_common::bin_resolve::resolve_binary`] and fails open with a
/// named gap line when it is missing. `git`, `tga`, `trusty-search`,
/// `trusty-analyze` and `trusty-review` are REQUIRED tools with their own
/// fail-CLOSED preflight ([`crate::run::pins`]) and do not belong here — a
/// missing required tool already refuses the run before this module would
/// ever run.
pub const ALL: [OptionalCollector; 3] = [
    OptionalCollector {
        collector: secrets::COLLECTOR,
        binary: secrets::BINARY,
        dimension: "secret leakage in the working tree",
        install_hint: secrets::INSTALL_COMMAND,
    },
    OptionalCollector {
        collector: cve::COLLECTOR,
        binary: cve::BINARY,
        dimension: "known dependency CVEs",
        install_hint: cve::INSTALL_COMMAND,
    },
    OptionalCollector {
        collector: license::COLLECTOR,
        binary: license::BINARY,
        dimension: "dependency license review",
        install_hint: license::INSTALL_COMMAND,
    },
];

/// Every optional collector in [`ALL`] whose binary is not on this machine.
///
/// What: filters [`ALL`] by [`trusty_common::bin_resolve::resolve_binary`],
/// the exact resolver each collector calls at collection time — this reuses
/// that lookup rather than adding a second one, so a preflight pass and a
/// mid-sweep pass can never disagree about what is installed.
/// Test: `collectors_tests::missing_finds_a_binary_absent_from_a_fake_resolver`,
/// `collectors_tests::missing_is_empty_when_every_binary_resolves`.
pub fn missing() -> Vec<OptionalCollector> {
    missing_resolved_by(trusty_common::bin_resolve::resolve_binary)
}

/// [`missing`] with the resolver supplied by the caller.
///
/// Why: split out purely for testability, the same reason
/// [`trusty_common::bin_resolve`] itself parameterizes its fallback
/// directory list — a test that mutated the real, process-global `PATH`/
/// `HOME` to prove "missing" would race every OTHER test in this binary that
/// also resolves a binary (`crate::run::pins`, `crate::git`, …), since
/// `#[serial_test::serial]` only orders tests against EACH OTHER, never
/// against a plain `#[test]` that never opted in. Production has exactly one
/// caller ([`missing`], passing the real resolver); tests pass a closure over
/// a fixed set of names instead, so nothing here ever touches real env.
fn missing_resolved_by(resolve: impl Fn(&str) -> Option<PathBuf>) -> Vec<OptionalCollector> {
    ALL.into_iter()
        .filter(|c| resolve(c.binary).is_none())
        .collect()
}

/// One warning row for a missing collector: what it is, what goes dark, and
/// how to fix it.
fn warning_row(c: &OptionalCollector) -> String {
    format!(
        "{}: `{}` is not installed, so {} will go unassessed for every repository in this \
         sweep (install it with `{}`)",
        c.collector, c.binary, c.dimension, c.install_hint
    )
}

/// The preflight itself: warn-and-continue by default, refuse under
/// `--strict-collectors`.
///
/// # Postconditions
/// On `Ok`, one warning row per missing collector (empty when every optional
/// collector is present), and the caller is free to proceed — this never
/// blocks by itself. `strict` is the only thing that turns a non-empty
/// [`missing`] into a refusal.
///
/// What: calls [`missing`] once and either returns its rows as warnings or,
/// when `strict` is set and the list is non-empty, refuses.
/// Test: `collectors_tests::default_warns_and_returns_rows_for_every_gap`,
/// `collectors_tests::strict_refuses_on_a_missing_collector`,
/// `collectors_tests::all_present_produces_no_rows_either_way`.
///
/// # Errors
/// [`AuditError::MissingOptionalCollectors`] when `strict` is true and at
/// least one optional collector's binary is missing.
pub fn check(strict: bool) -> Result<Vec<String>, AuditError> {
    check_resolved_by(strict, trusty_common::bin_resolve::resolve_binary)
}

/// [`check`] with the resolver supplied by the caller — see
/// [`missing_resolved_by`] for why this seam exists.
fn check_resolved_by(
    strict: bool,
    resolve: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Vec<String>, AuditError> {
    let missing = missing_resolved_by(resolve);
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    if strict {
        return Err(AuditError::MissingOptionalCollectors {
            missing: missing.iter().map(|c| c.binary).collect(),
        });
    }
    Ok(missing.iter().map(warning_row).collect())
}

#[cfg(test)]
mod collectors_tests {
    use super::*;

    /// A resolver that reports every name in `present` as found (at a stand-in
    /// path) and everything else as missing — no process env touched at all,
    /// so these tests are safe under any `--test-threads` and need no
    /// `#[serial]` coordination with the rest of the crate's test binary.
    fn fake_resolver(present: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |name| present.contains(&name).then(|| PathBuf::from(name))
    }

    #[test]
    fn all_names_every_fail_open_collector() {
        let binaries: Vec<&str> = ALL.iter().map(|c| c.binary).collect();
        assert_eq!(binaries, ["gitleaks", "cargo-audit", "cargo-deny"]);
    }

    #[test]
    fn missing_finds_a_binary_absent_from_a_fake_resolver() {
        let missing = missing_resolved_by(fake_resolver(&["gitleaks"]));

        let names: Vec<&str> = missing.iter().map(|c| c.binary).collect();
        assert_eq!(names, ["cargo-audit", "cargo-deny"]);
    }

    #[test]
    fn missing_is_empty_when_every_binary_resolves() {
        let missing =
            missing_resolved_by(fake_resolver(&["gitleaks", "cargo-audit", "cargo-deny"]));

        assert!(missing.is_empty(), "expected no gaps, got {missing:?}");
    }

    #[test]
    fn default_warns_and_returns_rows_for_every_gap() {
        let rows =
            check_resolved_by(false, fake_resolver(&["gitleaks"])).expect("warns, does not refuse");

        assert_eq!(rows.len(), 2);
        let cargo_audit_row = rows
            .iter()
            .find(|r| r.contains("cargo-audit"))
            .expect("a row naming cargo-audit");
        assert!(cargo_audit_row.contains("known dependency CVEs"));
        assert!(cargo_audit_row.contains("cargo install cargo-audit"));
        assert!(rows.iter().any(|r| r.contains("cargo-deny")));
    }

    #[test]
    fn strict_refuses_on_a_missing_collector() {
        let err = check_resolved_by(true, fake_resolver(&["gitleaks", "cargo-deny"]))
            .expect_err("strict refuses");

        match err {
            AuditError::MissingOptionalCollectors { missing } => {
                assert_eq!(missing, vec!["cargo-audit"]);
            }
            other => panic!("expected MissingOptionalCollectors, got {other:?}"),
        }
    }

    #[test]
    fn all_present_produces_no_rows_either_way() {
        let all_present = fake_resolver(&["gitleaks", "cargo-audit", "cargo-deny"]);

        assert!(
            check_resolved_by(false, &all_present)
                .expect("nothing to warn about")
                .is_empty()
        );
        assert!(
            check_resolved_by(true, &all_present)
                .expect("nothing to refuse")
                .is_empty()
        );
    }
}
