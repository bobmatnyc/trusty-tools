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
//! never a second opinion on where a binary lives — and [`decide`], the pure
//! warn-or-refuse judgement [`crate::chain::Phase::Preflight`] applies to
//! that list. `check` composes both for a caller that wants one call.
//! [`crate::chain::audit`] is the one production caller; it also reports each
//! missing collector through [`crate::progress::Progress`] as
//! `Operation::Preflight` before Phase 2 clones anything — see
//! `crate::chain::audit_with_preflight`.
//!
//! Test: `collectors_tests`, plus
//! `collectors_real_resolve::missing_and_check_go_through_the_real_resolve_binary`
//! (a separate integration test process — see [`missing_resolved_by`] for why
//! the real-resolver proof cannot live in this file's own test module).

use std::path::PathBuf;

use crate::error::AuditError;
use crate::grounding::{cve, license, secrets};

/// One optional collector a sweep can run, and what goes dark without it.
///
/// Why: a shared row shape rather than three ad hoc checks, so a fourth
/// fail-open collector is one entry in [`ALL`] and nothing else changes. It
/// also carries the same three facts (collector, dimension, install hint)
/// into BOTH the warn-and-continue row and the `--strict-collectors` refusal
/// (`AuditError::MissingOptionalCollectors`) — one struct, so the two cannot
/// drift apart the way a bare binary name once did (#7134 review).
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
/// fail-CLOSED preflight (`crate::run::pins`) and do not belong here — a
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
/// `collectors_tests::missing_is_empty_when_every_binary_resolves`,
/// `collectors_real_resolve::missing_and_check_go_through_the_real_resolve_binary`.
pub fn missing() -> Vec<OptionalCollector> {
    missing_resolved_by(trusty_common::bin_resolve::resolve_binary)
}

/// [`missing`] with the resolver supplied by the caller.
///
/// Why: split out purely for testability, the same reason
/// [`trusty_common::bin_resolve`] itself parameterizes its fallback
/// directory list — a test that REPLACED the real, process-global `PATH`/
/// `HOME` to prove "missing" would race every OTHER test in this binary that
/// also resolves a binary (`crate::run::pins`, `crate::git`, …), since
/// `#[serial_test::serial]` only orders tests against EACH OTHER, never
/// against a plain `#[test]` that never opted in — confirmed the hard way:
/// an earlier version of this module's tests did exactly that and
/// intermittently broke `clone::clone_tests::two_paths_with_one_basename_are_refused_together`
/// with `"git is on PATH for this suite" … NotFound`. Production has exactly
/// one caller ([`missing`], passing the real resolver); every test in THIS
/// module (this file's `#[cfg(test)]` block, part of the crate's shared
/// `--lib` test binary) passes a closure over a fixed set of names instead,
/// so nothing here touches real env at all. The one test that must exercise
/// the real resolver — `collectors_real_resolve::missing_and_check_go_through_the_real_resolve_binary`
/// — lives in its own `tests/collectors_real_resolve.rs` integration file
/// instead, which Cargo always builds as its own OS process; being the only
/// `#[test]` in that process is what makes touching `PATH` there safe with no
/// `#[serial]` needed, not merely the fact that its own change is additive.
fn missing_resolved_by(resolve: impl Fn(&str) -> Option<PathBuf>) -> Vec<OptionalCollector> {
    ALL.into_iter()
        .filter(|c| resolve(c.binary).is_none())
        .collect()
}

/// One warning row for a missing collector: what it is, what goes dark, and
/// how to fix it.
///
/// `pub(crate)`: also the exact text [`crate::chain::audit`] narrates through
/// `Operation::Preflight` for each missing collector, so the row a warning
/// prints and the row in [`crate::chain::ChainReport::collector_gaps`] can
/// never disagree.
pub(crate) fn warning_row(c: &OptionalCollector) -> String {
    format!(
        "{}: `{}` is not installed, so {} will go unassessed for every repository in this \
         sweep (install it with `{}`)",
        c.collector, c.binary, c.dimension, c.install_hint
    )
}

/// The warn-or-refuse judgement over an already-resolved `missing` list.
///
/// Why: pure and resolver-free, unlike [`missing`] — split out so
/// `crate::chain::audit_with_preflight` can take the missing list as a
/// parameter (for progress reporting) and reuse this one decision rather than
/// re-deriving it, and so a test can drive every branch (empty, some missing,
/// strict) with a literal list and no PATH/env involved at all.
///
/// # Postconditions
/// On `Ok`, one warning row per entry in `missing` (empty when `missing` is
/// empty), and the caller is free to proceed — this never blocks by itself.
/// `strict` is the only thing that turns a non-empty `missing` into a
/// refusal.
/// Test: `collectors_tests::decide_warns_and_returns_rows_for_every_gap`,
/// `collectors_tests::decide_strict_refuses_on_a_missing_collector`,
/// `collectors_tests::decide_on_an_empty_list_produces_no_rows_either_way`.
///
/// # Errors
/// [`AuditError::MissingOptionalCollectors`] when `strict` is true and
/// `missing` is non-empty.
pub fn decide(strict: bool, missing: &[OptionalCollector]) -> Result<Vec<String>, AuditError> {
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    if strict {
        return Err(AuditError::MissingOptionalCollectors {
            missing: missing.to_vec(),
        });
    }
    Ok(missing.iter().map(warning_row).collect())
}

/// [`missing`] then [`decide`] — the preflight in one call, for a caller with
/// no progress sink to report through.
///
/// [`crate::chain::audit`] does not use this: it needs the missing list
/// itself (to narrate each gap through `Operation::Preflight` before it
/// decides), so it calls [`missing`] and [`decide`] separately.
/// Test: `collectors_tests::check_composes_missing_and_decide`.
///
/// # Errors
/// See [`decide`].
pub fn check(strict: bool) -> Result<Vec<String>, AuditError> {
    decide(strict, &missing())
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
    fn decide_warns_and_returns_rows_for_every_gap() {
        let missing = missing_resolved_by(fake_resolver(&["gitleaks"]));

        let rows = decide(false, &missing).expect("warns, does not refuse");

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
    fn decide_strict_refuses_on_a_missing_collector() {
        let missing = missing_resolved_by(fake_resolver(&["gitleaks", "cargo-deny"]));

        let err = decide(true, &missing).expect_err("strict refuses");

        match err {
            AuditError::MissingOptionalCollectors { missing } => {
                assert_eq!(missing.len(), 1);
                assert_eq!(missing[0].binary, "cargo-audit");
                assert_eq!(missing[0].collector, "cve-scan");
                assert_eq!(missing[0].dimension, "known dependency CVEs");
                assert_eq!(missing[0].install_hint, "cargo install cargo-audit");
            }
            other => panic!("expected MissingOptionalCollectors, got {other:?}"),
        }
    }

    #[test]
    fn decide_on_an_empty_list_produces_no_rows_either_way() {
        assert!(
            decide(false, &[])
                .expect("nothing to warn about")
                .is_empty()
        );
        assert!(decide(true, &[]).expect("nothing to refuse").is_empty());
    }

    #[test]
    fn check_composes_missing_and_decide() {
        // `check` calls the REAL `missing()`, so this only proves the
        // composition compiles and returns `Ok` either way — which of the
        // three real collectors are installed on the machine running this
        // test is not something this suite controls or asserts on; see
        // `tests/collectors_real_resolve.rs` for the real-resolver proof
        // (its own process, so it cannot race this binary's other tests —
        // see `missing_resolved_by`'s doc comment) and `decide_*` above for
        // the warn/refuse logic itself.
        assert!(check(false).is_ok());
    }
}
