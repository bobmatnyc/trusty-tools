//! Unit tests for the `install` module.
//!
//! Why: Keeping tests in a sibling file rather than inline in `install.rs` lets
//! the definition file stay under the 500-line production cap (CLAUDE.md /
//! `scripts/check_line_cap.sh`) while retaining full coverage — mirrors the
//! `cli.rs` / `cli_tests.rs` split.
//!
//! What: Covers the `InstallReport` JSON shaping and `all_ok` derivation
//! (including the #2560 verify-tail fold and the #3527 supervisor-bootstrap
//! gating predicate), the `--dry-run` preview report, and the unknown-member /
//! dry-run exit-code paths of `run`. The network / `cargo install` path and
//! the confirmation gate's TTY plumbing are side-effecting and covered
//! elsewhere (see `install.rs`'s module doc).
//!
//! Test: `cargo test -p trusty-installer` runs all tests in this file.

use super::*;
// Not re-exported by `install.rs` itself (only used internally by
// `install_report.rs`'s own `print_dry_run`), so pulled in directly here.
use super::install_report::build_dry_run_report;
use crate::commands::stable_set::ManageStrategy;

/// Build an `InstallOutcome` with the pre-#2566 default service state
/// (not attempted — `service_ok: true`, empty detail) and the pre-#3554
/// default shadow state (clear — `shadow_ok: true`, empty detail), so tests
/// that only care about the binary-install dimension don't have to repeat
/// those fields at every call site.
fn outcome(member: &str, ok: bool, detail: &str) -> InstallOutcome {
    InstallOutcome {
        member: member.to_owned(),
        ok,
        detail: detail.to_owned(),
        service_ok: true,
        service_detail: String::new(),
        shadow_ok: true,
        shadow_detail: String::new(),
        required: true,
        integrity_ok: true,
    }
}

/// Like `outcome`, but marks the member OPTIONAL (graceful-degrade,
/// demo-critical fix) so tests can build a failed-but-non-fatal outcome.
fn optional_outcome(member: &str, ok: bool, detail: &str) -> InstallOutcome {
    InstallOutcome {
        required: false,
        ..outcome(member, ok, detail)
    }
}

/// Why: #5518 — the graceful-degrade rule says an OPTIONAL member's install
/// failure never fails the run. Applied to a checksum mismatch that would mean
/// `tctl install` sees a tampered artifact and still exits 0, which is the
/// original defect re-appearing one layer above `download::Outcome`.
/// What: An optional member that failed verification; asserts `all_ok` is false
/// and the exit code is 2, unlike the routine optional failure above.
/// Test: This is the test.
#[test]
fn report_all_ok_reflects_an_optional_members_checksum_mismatch() {
    let tampered = InstallReport::build(vec![
        outcome("trusty-search", true, "installed"),
        InstallOutcome {
            integrity_ok: false,
            ..optional_outcome("tga", false, "SECURITY: checksum mismatch for tga 2.18.0")
        },
    ]);
    assert!(
        !tampered.all_ok,
        "an optional member's failed checksum must still fail the run"
    );
    assert_eq!(tampered.exit_code(), 2);

    // The graceful degrade itself is unchanged: a routine optional failure
    // still exits 0, so this is not just "every optional failure now fails".
    let degraded = InstallReport::build(vec![
        outcome("trusty-search", true, "installed"),
        optional_outcome("tga", false, "no prebuilt for this platform"),
    ]);
    assert!(degraded.all_ok);
    assert_eq!(degraded.exit_code(), 0);
}

/// Why: `install_all` decides between "skipped (optional, no prebuilt for this
/// platform)" and a loud failure by asking this predicate. If it missed a
/// mismatch wrapped in caller context, the tamper signal would be rendered as a
/// routine skip (#5518).
/// What: Wraps a `ChecksumMismatch` in two layers of context; asserts it is
/// still recognised.
/// Test: This is the test.
#[test]
fn is_integrity_failure_spots_a_checksum_mismatch_through_context() {
    let mismatch = crate::download::ChecksumMismatch {
        crate_name: "tga".to_owned(),
        version: "2.18.0".to_owned(),
        archive: "tga-2.18.0.tar.gz".to_owned(),
        url: "https://example.invalid/tga.tar.gz".to_owned(),
        expected: "a".repeat(64),
        actual: "b".repeat(64),
    };
    let wrapped = anyhow::Error::new(mismatch).context("installing tga");
    assert!(is_integrity_failure(&wrapped));
}

/// Why: The predicate must not classify every failure as an integrity failure,
/// or the graceful degrade it guards disappears.
/// What: A plain error; asserts it is NOT an integrity failure.
/// Test: This is the test.
#[test]
fn is_integrity_failure_ignores_a_routine_failure() {
    let e = anyhow::anyhow!("no Rust toolchain found on PATH (cargo not available)");
    assert!(!is_integrity_failure(&e));
}

/// Why: The JSON envelope is a public contract; pin its shape.
/// What: Builds a report and asserts the serialised keys/values.
/// Test: This is the test.
#[test]
fn report_serialises() {
    let report = InstallReport::build(vec![outcome("trusty-search", true, "installed")]);
    let v = serde_json::to_value(&report).expect("serialises");
    assert_eq!(v["command"], "install");
    assert_eq!(v["all_ok"], true);
    assert_eq!(v["members"][0]["member"], "trusty-search");
    assert_eq!(v["members"][0]["service_ok"], true);
}

/// Why: `all_ok` must be false if any member's BINARY install failed.
/// What: Mixes an ok and a failed outcome; asserts `all_ok = false`.
/// Test: This is the test.
#[test]
fn report_all_ok() {
    let report = InstallReport::build(vec![outcome("a", true, ""), outcome("b", false, "boom")]);
    assert!(!report.all_ok);
}

/// Why: THE demo-critical fix — an OPTIONAL member (e.g. `trusty-analyze` on
/// a platform with no prebuilt and no Rust toolchain) failing to install must
/// NOT fail the overall run; only a REQUIRED member's failure may.
/// What: a required-ok + optional-failed report is still `all_ok`/exit 0; a
/// required-failed + optional-ok report is not.
/// Test: This is the test.
#[test]
fn report_all_ok_ignores_optional_failure() {
    let degraded = InstallReport::build(vec![
        outcome("trusty-search", true, "installed"),
        optional_outcome(
            "trusty-analyze",
            false,
            "no prebuilt for aarch64-apple-darwin",
        ),
    ]);
    assert!(
        degraded.all_ok,
        "an optional member's failure must not fail the overall run"
    );
    assert_eq!(degraded.exit_code(), 0);

    let genuinely_failed = InstallReport::build(vec![
        outcome("trusty-search", false, "network error"),
        optional_outcome("trusty-analyze", true, "installed"),
    ]);
    assert!(
        !genuinely_failed.all_ok,
        "a required member's failure must still fail the overall run"
    );
    assert_eq!(genuinely_failed.exit_code(), 2);
}

/// Why: an install selecting ONLY optional members (e.g. `tctl install tga`)
/// must still be able to report success when that member installs fine —
/// `all_ok` over an empty required-subset must not vacuously fail.
/// What: a single optional, successful outcome yields `all_ok: true`.
/// Test: This is the test.
#[test]
fn report_all_ok_true_for_optional_only_success() {
    let report = InstallReport::build(vec![optional_outcome("tga", true, "installed")]);
    assert!(report.all_ok);
    assert_eq!(report.exit_code(), 0);
}

/// Why (#5806): the error arm the test above never covered, and a fail-open.
/// `filter(required).all(…)` over an empty iterator is vacuously `true`, so an
/// all-OPTIONAL selection whose install genuinely FAILED reported
/// `all_ok: true` and exit 0 — nothing installed, success reported, no signal
/// until much later. This has been reachable via `tctl install tga`,
/// `trusty-analyze`, and `trusty-console` all along; adding trusty-installer
/// as an OPTIONAL member would have opened a fourth door.
/// What: one optional, FAILED outcome must yield `all_ok: false` / exit 2, and
/// so must each of the two other failure dimensions (service, shadow) on the
/// same all-optional selection.
/// Test: This is the test.
#[test]
fn all_optional_selection_does_not_fail_open() {
    let failed = InstallReport::build(vec![optional_outcome("tga", false, "network error")]);
    assert!(
        !failed.all_ok,
        "an all-optional selection that FAILED must not report success"
    );
    assert_eq!(failed.exit_code(), 2);

    let mut service_failed = optional_outcome("tga", true, "installed");
    service_failed.service_ok = false;
    assert!(!InstallReport::build(vec![service_failed]).all_ok);

    let mut shadowed = optional_outcome("tga", true, "installed");
    shadowed.shadow_ok = false;
    assert!(!InstallReport::build(vec![shadowed]).all_ok);
}

/// Why (#5806): the acceptance case for the fail-open fix. `tctl install
/// trusty-installer` names exactly one member, and that member is OPTIONAL, so
/// a failed self-install hit the vacuous-truth path above and exited 0 — the
/// worst possible place for it, since "reported success, did nothing" on the
/// installer means the operator keeps running the stale binary believing it
/// was replaced.
/// What: a lone failed trusty-installer outcome exits 2; the same outcome
/// alongside a healthy REQUIRED member still degrades gracefully to exit 0,
/// which is the graceful-degrade property this fix must not break.
/// Test: This is the test.
#[test]
fn lone_optional_installer_failure_exits_nonzero() {
    let alone = InstallReport::build(vec![optional_outcome(
        "trusty-installer",
        false,
        "permission denied writing to bin dir",
    )]);
    assert!(!alone.all_ok);
    assert_eq!(alone.exit_code(), 2);

    let bulk = InstallReport::build(vec![
        outcome("trusty-search", true, "installed"),
        optional_outcome("trusty-installer", false, "no prebuilt for this platform"),
    ]);
    assert!(
        bulk.all_ok,
        "a bulk install runs from a working installer; failing to refresh it \
         must not fail the whole stack"
    );
    assert_eq!(bulk.exit_code(), 0);
}

/// Why (#5806): `build(vec![])` was vacuously `all_ok: true` — `.all()` over an
/// empty iterator, the same shape the required-filter fix removes one line up.
/// `run` returns early on an empty selection so nothing reaches it today, but
/// "installed nothing" is not evidence of a successful install, and leaving the
/// shape inside the function being fixed for it is how it comes back.
/// What: asserts an empty report is not `all_ok` and exits 2.
/// Test: This is the test.
#[test]
fn empty_report_is_not_all_ok() {
    let empty = InstallReport::build(Vec::new());
    assert!(
        !empty.all_ok,
        "a report over zero members must not claim every member installed"
    );
    assert_eq!(empty.exit_code(), 2);
}

/// Why (#5806): the machine verdict was fixed and the human one was not. For an
/// all-optional selection that failed, the footer printed `installed 0/0
/// required component(s)`, an INFO-level "skipped", and then a green VERIFIED —
/// while `build` exited 2. One run, two channels, opposite stories.
/// What: asserts a lone failed OPTIONAL member produces a `0/1 selected`
/// headline, an ERROR line naming the failure, and NO informational "skipped"
/// line — and that the summary agrees with `all_ok` in every case.
/// Test: This is the test.
#[test]
fn all_optional_failure_summary_does_not_read_as_success() {
    let report = InstallReport::build(vec![optional_outcome("tga", false, "network error")]);
    let lines = install_report::summary_lines(&report);

    assert_eq!(lines.headline, "installed 0/1 selected component(s)");
    assert_eq!(lines.errors, vec!["tga: network error".to_owned()]);
    assert!(
        lines.skipped.is_empty(),
        "a failure the exit code gates on must not be downgraded to `skipped`: {:?}",
        lines.skipped
    );
    assert!(!report.all_ok);
    assert_eq!(
        lines.errors.is_empty(),
        report.all_ok,
        "the human summary and `all_ok` must not disagree"
    );
}

/// Why (#5806): the graceful-degrade wording this fix must NOT break. With a
/// REQUIRED member present, an OPTIONAL member's failure still reads as an
/// informational skip rather than a scary error, and the headline still counts
/// the required subset.
/// What: asserts a healthy REQUIRED member beside a failed OPTIONAL one yields
/// `1/1 required`, no errors, and one `skipped` line.
/// Test: This is the test.
#[test]
fn summary_lines_match_the_gating_set() {
    let report = InstallReport::build(vec![
        outcome("trusty-search", true, "installed"),
        optional_outcome("tga", false, "no prebuilt for this platform"),
    ]);
    let lines = install_report::summary_lines(&report);

    assert_eq!(lines.headline, "installed 1/1 required component(s)");
    assert!(lines.errors.is_empty(), "{:?}", lines.errors);
    assert_eq!(lines.skipped.len(), 1);
    assert!(lines.skipped[0].starts_with("tga: skipped"));
    assert!(report.all_ok);
}

/// Why: #5518 — `summary_lines` degrades every non-gating failure to
/// `skipped (no prebuilt for this platform)`, which is the exact sentence a
/// tamper signal must never wear. Left alone it would have re-hidden an
/// optional member's mismatch on the human path while `all_ok` failed the run,
/// putting the two channels back into the disagreement #5806 closed.
/// What: an optional member with `integrity_ok: false` beside a healthy
/// required one; asserts the footer carries an error naming it and no `skipped`
/// line, and that the routine optional failure still reads as a skip.
/// Test: This is the test.
#[test]
fn an_optional_members_checksum_mismatch_is_never_summarised_as_skipped() {
    let report = InstallReport::build(vec![
        outcome("trusty-search", true, "installed"),
        InstallOutcome {
            integrity_ok: false,
            ..optional_outcome("tga", false, "SECURITY: checksum mismatch for tga 2.18.0")
        },
    ]);
    let lines = install_report::summary_lines(&report);

    assert!(
        lines.skipped.is_empty(),
        "a tampered artifact must never be summarised as a skip: {:?}",
        lines.skipped
    );
    assert_eq!(lines.errors.len(), 1, "{:?}", lines.errors);
    assert!(
        lines.errors[0].contains("tga") && lines.errors[0].contains("checksum mismatch"),
        "the error line must name the member and the mismatch: {:?}",
        lines.errors
    );
    assert!(!report.all_ok);

    // The graceful degrade it sits beside is unchanged.
    let degraded = InstallReport::build(vec![
        outcome("trusty-search", true, "installed"),
        optional_outcome("tga", false, "no prebuilt for this platform"),
    ]);
    let degraded_lines = install_report::summary_lines(&degraded);
    assert!(degraded_lines.errors.is_empty());
    assert_eq!(degraded_lines.skipped.len(), 1);
}

/// Why (#5806): `summary_lines_match_the_gating_set` pins ONE selection shape,
/// so the agreement between the footer and `all_ok` held there by construction
/// rather than by rule. The defect this change closes was the two channels
/// deriving the gating partition separately — pin the agreement across every
/// shape the partition can take, so re-deriving it anywhere fails a test rather
/// than shipping.
/// What: over eight selections — all-required, all-optional and mixed, each
/// healthy and each failing, plus the service-failure dimension — asserts
/// `summary_lines(&r).errors.is_empty() == r.all_ok`, and that the table
/// exercises both verdicts rather than passing vacuously.
///
/// The empty selection is excluded deliberately: `build(vec![])` is `all_ok:
/// false` with no member able to fail, which `empty_report_is_not_all_ok`
/// covers and `summary_lines`'s postcondition documents as the one exception.
/// Test: This is the test.
#[test]
fn summary_errors_agree_with_all_ok_across_selection_shapes() {
    let service_failed = |member: &str, required: bool| {
        let mut o = if required {
            outcome(member, true, "installed")
        } else {
            optional_outcome(member, true, "installed")
        };
        o.service_ok = false;
        o.service_detail = "launchd bootstrap failed".to_owned();
        o
    };

    let cases: Vec<(&str, Vec<InstallOutcome>)> = vec![
        (
            "all required, healthy",
            vec![outcome("a", true, "installed"), outcome("b", true, "ok")],
        ),
        (
            "all required, one binary failure",
            vec![outcome("a", true, "installed"), outcome("b", false, "boom")],
        ),
        (
            "all required, one service failure",
            vec![outcome("a", true, "installed"), service_failed("b", true)],
        ),
        (
            "all optional, healthy",
            vec![
                optional_outcome("a", true, "installed"),
                optional_outcome("b", true, "installed"),
            ],
        ),
        (
            "all optional, one binary failure",
            vec![
                optional_outcome("a", true, "installed"),
                optional_outcome("b", false, "boom"),
            ],
        ),
        (
            "all optional, lone service failure",
            vec![service_failed("a", false)],
        ),
        (
            "mixed, optional failure degrades gracefully",
            vec![
                outcome("a", true, "installed"),
                optional_outcome("b", false, "no prebuilt for this platform"),
            ],
        ),
        (
            "mixed, required failure gates",
            vec![
                outcome("a", false, "boom"),
                optional_outcome("b", true, "installed"),
            ],
        ),
        // #5518: the one shape where a NON-gating member decides the verdict.
        // `build` fails the run for it, so the footer must carry an error line
        // or the two channels disagree again.
        (
            "mixed, optional member's checksum mismatch",
            vec![
                outcome("a", true, "installed"),
                InstallOutcome {
                    integrity_ok: false,
                    ..optional_outcome("b", false, "SECURITY: checksum mismatch for b 1.0.0")
                },
            ],
        ),
    ];

    let (mut verdicts_ok, mut verdicts_failed) = (0, 0);
    for (shape, members) in cases {
        let report = InstallReport::build(members);
        let lines = install_report::summary_lines(&report);
        assert_eq!(
            lines.errors.is_empty(),
            report.all_ok,
            "{shape}: the human footer and `all_ok` disagree — errors {:?}, all_ok {}",
            lines.errors,
            report.all_ok
        );
        if report.all_ok {
            verdicts_ok += 1;
        } else {
            verdicts_failed += 1;
        }
    }
    assert!(
        verdicts_ok > 0 && verdicts_failed > 0,
        "the table must exercise both verdicts, not pass vacuously: \
         {verdicts_ok} ok / {verdicts_failed} failed"
    );
}

/// Why: #2566 review — a binary can install cleanly (`ok: true`) while its
/// SERVICE bootstrap genuinely fails; the report must not claim
/// `all_ok: true` in that case (the exact failure class #2557 existed
/// because of: a "successfully installed" daemon that never actually
/// works). A `Skipped` service outcome (opt-out, non-service member, or
/// plist already present) must NOT be treated as a failure.
/// What: one member with `ok: true, service_ok: false` → `all_ok == false`;
/// a second report with `ok: true, service_ok: true` (skip case) →
/// `all_ok == true`.
/// Test: this is the test.
#[test]
fn report_all_ok_reflects_service_failure() {
    let failed = InstallReport::build(vec![InstallOutcome {
        member: "trusty-search".to_owned(),
        ok: true,
        detail: "installed".to_owned(),
        service_ok: false,
        service_detail: "service bootstrap failed (non-fatal): boom".to_owned(),
        shadow_ok: true,
        shadow_detail: String::new(),
        required: true,
        integrity_ok: true,
    }]);
    assert!(
        !failed.all_ok,
        "a genuine service bootstrap failure must flip all_ok to false"
    );
    assert_eq!(failed.exit_code(), 2);

    let skipped = InstallReport::build(vec![InstallOutcome {
        member: "trusty-search".to_owned(),
        ok: true,
        detail: "installed".to_owned(),
        service_ok: true,
        service_detail: "launchd plist already present — left untouched".to_owned(),
        shadow_ok: true,
        shadow_detail: String::new(),
        required: true,
        integrity_ok: true,
    }]);
    assert!(
        skipped.all_ok,
        "a skipped (not failed) service bootstrap must not flip all_ok"
    );
    assert_eq!(skipped.exit_code(), 0);
}

/// Why: #3554 — a just-installed binary that is provably PATH-shadowed by a
/// stale copy is a genuine "looks installed, actually not live" failure; the
/// report must not claim `all_ok: true` in that case, mirroring the
/// service-bootstrap-failure precedent above.
/// What: one member with `ok: true, shadow_ok: false` → `all_ok == false`
/// and exit code 2; a `shadow_ok: true` (clear) member → `all_ok == true`.
/// Test: this is the test.
#[test]
fn report_all_ok_reflects_shadow_failure() {
    let shadowed = InstallReport::build(vec![InstallOutcome {
        member: "trusty-mpm".to_owned(),
        ok: true,
        detail: "installed".to_owned(),
        service_ok: true,
        service_detail: String::new(),
        shadow_ok: false,
        shadow_detail: "PATH SHADOWED: installed trusty-mpm 0.19.29 to /x/.local/bin/tm, but \
                         the shell resolves `tm` to /x/.cargo/bin/tm (0.19.26)"
            .to_owned(),
        required: true,
        integrity_ok: true,
    }]);
    assert!(
        !shadowed.all_ok,
        "a genuine PATH-shadow condition must flip all_ok to false"
    );
    assert_eq!(shadowed.exit_code(), 2);

    let clear = InstallReport::build(vec![outcome("trusty-mpm", true, "installed")]);
    assert!(
        clear.all_ok,
        "a clear (non-shadowed) install must not flip all_ok"
    );
    assert_eq!(clear.exit_code(), 0);
}

/// Why: #3527 — trusty-mpm's supervisor bootstrap must respect the SAME
/// `--no-service` / `TCTL_NO_SERVICE_BOOTSTRAP` opt-out as every other
/// daemon, even though its `ManageStrategy::OwnVerb` makes
/// `plans_service_bootstrap` always `false` for it.
/// What: asserts the predicate is `true` only when `service_enabled` AND
/// the member is trusty-mpm; `false` for a disabled flag or a different
/// member.
/// Test: This is the test.
#[test]
fn plans_mpm_supervisor_bootstrap_respects_flag() {
    let mpm = stable_member_for_test("trusty-mpm", "trusty-mpm", ManageStrategy::OwnVerb);
    let search = stable_member_for_test("trusty-search", "trusty-search", ManageStrategy::Launchd);

    assert!(plans_mpm_supervisor_bootstrap(&mpm, true));
    assert!(
        !plans_mpm_supervisor_bootstrap(&mpm, false),
        "--no-service / TCTL_NO_SERVICE_BOOTSTRAP must disable the supervisor bootstrap too"
    );
    assert!(
        !plans_mpm_supervisor_bootstrap(&search, true),
        "the predicate must only ever apply to trusty-mpm"
    );
}

/// Why: #2560 — the folded report's `all_ok` (and hence exit code) must
/// reflect a verify-tail failure even though the binary + service install
/// itself succeeded — never silently report success over a down daemon.
/// What: an all-ok binary install report folded with a `verified: false`
/// verify tail flips to `all_ok == false`; folded with `verified: true` it
/// stays `true`.
/// Test: This is the test.
#[test]
fn with_verify_failure_flips_all_ok() {
    let base = InstallReport::build(vec![outcome("trusty-search", true, "installed")]);
    assert!(base.all_ok);

    let verify_failed = crate::commands::verify_tail::VerifyTailReport {
        command: "install.verify",
        ensure_ok: true,
        members: Vec::new(),
        verified: false,
    };
    let folded = base.clone().with_verify(verify_failed);
    assert!(
        !folded.all_ok,
        "a verify-tail failure must flip all_ok to false"
    );
    assert_eq!(folded.exit_code(), 2);
}

/// Why: the converse of the above — a verified success must NOT disturb an
/// already-ok report.
/// What: folding a `verified: true` verify tail leaves `all_ok == true`.
/// Test: This is the test.
#[test]
fn with_verify_success_preserves_all_ok() {
    let base = InstallReport::build(vec![outcome("trusty-search", true, "installed")]);
    let verify_ok = crate::commands::verify_tail::VerifyTailReport {
        command: "install.verify",
        ensure_ok: true,
        members: Vec::new(),
        verified: true,
    };
    let folded = base.with_verify(verify_ok);
    assert!(folded.all_ok);
    assert_eq!(folded.exit_code(), 0);
}

/// Why: The exit code must track the verdict for automation.
/// What: Asserts 0 for all-ok and 2 for a failure.
/// Test: This is the test.
#[test]
fn exit_code_reflects_all_ok() {
    let ok = InstallReport::build(vec![outcome("a", true, "")]);
    assert_eq!(ok.exit_code(), 0);
    let bad = InstallReport::build(vec![outcome("a", false, "x")]);
    assert_eq!(bad.exit_code(), 2);
}

// #4964: `cargo_bin_dir_from_env`'s two tests moved to `trusty-common` as
// `bin_resolve::tests::canonical_bin_dir_from_*` — the local copy of the rule
// they covered was one of five, and is now the shared
// `trusty_common::bin_resolve::canonical_bin_dir`. The coverage is unchanged
// (same three cases, plus a fourth for the no-home case); it just lives beside
// the one implementation.

/// Why (#4964): `binary_size` used to join a bare binary NAME onto the cargo
/// bin dir while `install_one` writes to `install_dir` — so on the prebuilt
/// branch it stat'ed a stale copy, or nothing at all, and the component
/// table's size column described a file this run never touched. Taking the
/// concrete path removes the second directory entirely.
/// What: the reported size is the byte length of the file AT THE GIVEN PATH,
/// in a directory that is provably not any cargo bin dir.
/// Test: This is the test.
#[test]
fn binary_size_reads_the_concrete_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("trusty-search");
    std::fs::write(&path, b"0123456789").expect("write");
    assert_eq!(binary_size(&path), 10);
}

/// Why: the size column must still render for a binary that cannot be stat'ed
/// (a permissions failure, a race with an external mover) rather than aborting
/// the install report.
/// What: a missing path reports 0.
/// Test: This is the test.
#[test]
fn binary_size_is_zero_for_a_missing_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(binary_size(&dir.path().join("absent-4964")), 0);
}

// ── select_prebuilt_bin_path / cargo_fallback_bin_path (#3554 review — MEDIUM) ──
//
// These pin the exact glue that two of the three original #3554 bugs lived
// in: picking the CONCRETE just-installed path rather than re-deriving it by
// name. Extracted as pure functions specifically because `install_one` can't
// be exercised in a unit test without a live network round-trip
// (`download::try_install_prebuilt` hits GitHub Releases).

/// Why: the common case — `paths` (as `download::try_install_prebuilt`
/// actually returns them) already live under `install_dir`; the selector
/// must pick the entry matching `binary` by exact file-name equality.
/// What: three siblings under one dir; selecting `"tm"` returns exactly
/// `install_dir.join("tm")`.
/// Test: This is the test.
#[test]
fn select_prebuilt_bin_path_matches_by_filename() {
    let install_dir = std::path::PathBuf::from("/home/x/.local/bin");
    let paths = vec![
        install_dir.join("trusty-search"),
        install_dir.join("tm"),
        install_dir.join("trusty-mpm"),
    ];
    assert_eq!(
        select_prebuilt_bin_path(&paths, "tm", &install_dir),
        install_dir.join("tm")
    );
}

/// Why: `paths` not containing the requested binary at all must not panic —
/// the defensive fallback must still resolve to a path INSIDE `install_dir`
/// (never `None`, never a path elsewhere).
/// What: `paths` has no entry named `tm`; selecting `tm` falls back to
/// `install_dir.join("tm")`.
/// Test: This is the test.
#[test]
fn select_prebuilt_bin_path_falls_back_when_no_match() {
    let install_dir = std::path::PathBuf::from("/home/x/.local/bin");
    let paths = vec![install_dir.join("trusty-search")];
    assert_eq!(
        select_prebuilt_bin_path(&paths, "tm", &install_dir),
        install_dir.join("tm"),
        "defensive fallback must still point inside install_dir"
    );
}

/// Why: the selector matches by exact file-name EQUALITY, never substring —
/// `tm` must not accidentally match `trusty-mpm` (a real sibling in the same
/// tarball placement) just because one contains a similar sequence.
/// What: `paths` contains only `trusty-mpm`; selecting `tm` must NOT return
/// it — it falls back to `install_dir.join("tm")` instead.
/// Test: This is the test.
#[test]
fn select_prebuilt_bin_path_does_not_substring_match() {
    let install_dir = std::path::PathBuf::from("/home/x/.local/bin");
    let paths = vec![install_dir.join("trusty-mpm")];
    assert_eq!(
        select_prebuilt_bin_path(&paths, "tm", &install_dir),
        install_dir.join("tm"),
        "must not substring-match trusty-mpm for binary name tm"
    );
}

/// Why: the cargo-install fallback path must resolve to a concrete path
/// ending in the requested binary name, regardless of which directory
/// `cargo_bin_dir()` (or the `install_dir` fallback) resolves to on the test
/// host.
/// What: the returned path's file name is exactly `binary`.
/// Test: This is the test.
#[test]
fn cargo_fallback_bin_path_joins_binary_onto_cargo_bin_dir() {
    let install_dir = std::path::PathBuf::from("/home/x/.local/bin");
    let p = cargo_fallback_bin_path("tm", &install_dir);
    assert_eq!(p.file_name().and_then(|f| f.to_str()), Some("tm"));
}

/// Why (#3846 code-critic MEDIUM): the pre-fix "existed_before" check
/// resolved a SINGLE fixed directory (`install_dir`, where the prebuilt path
/// writes) before `install_one` knew which `Outcome` branch would fire. That
/// silently produced the WRONG "fresh install" verdict whenever the
/// `cargo install` fallback branch actually landed the binary in a
/// DIFFERENT directory (the cargo bin dir) — e.g. a REINSTALL where a prior
/// `cargo install` run already placed the binary there, but nothing ever
/// sat at the preferred dir.
/// What: Simulates exactly that scenario with two tempdirs standing in for
/// the preferred dir and the cargo bin dir: a file exists ONLY at the cargo
/// bin path. Asserts `existed_before_at_both` reports `false` for the
/// preferred path and `true` for the cargo bin path — i.e. the fallback
/// branch's guidance would correctly select the reinstall variant instead
/// of wrongly reporting a fresh install.
/// Test: This is the test.
#[test]
fn existed_before_at_both_distinguishes_preferred_from_cargo_bin_dir() {
    let preferred_dir = tempfile::tempdir().expect("tempdir");
    let cargo_dir = tempfile::tempdir().expect("tempdir");
    let binary = "trusty-search";

    // Nothing was ever placed at the preferred dir; a prior `cargo install`
    // already placed the binary at the cargo bin dir.
    std::fs::write(cargo_dir.path().join(binary), b"stub").expect("write stub binary");

    let (existed_preferred, existed_cargo) = existed_before_at_both(
        &preferred_dir.path().join(binary),
        &cargo_dir.path().join(binary),
    );
    assert!(
        !existed_preferred,
        "nothing was ever placed in the preferred dir"
    );
    assert!(
        existed_cargo,
        "a binary already sat at the cargo bin dir before this run"
    );
}

/// Why: An unknown member must be a clean error (exit 3), not a silent skip.
/// What: Calls `run` with a bogus member in `--json` mode; asserts exit 3.
/// Test: This is the test.
#[test]
fn run_unknown_member_is_error() {
    let code = run(
        &["not-a-real-tool".to_owned()],
        true,
        true,
        false,
        false,
        false,
        true,
    );
    assert_eq!(code, 3);
}

/// Why: #2112 — `--dry-run` must preview and exit 0 WITHOUT installing,
/// regardless of `--yes`. A `--json` invocation keeps the test hermetic
/// (no interactive picker, no TTY-dependent branch).
/// What: Calls `run` with `dry_run: true`, `yes: false`; asserts exit 0.
/// Test: This is the test.
#[test]
fn run_dry_run_exits_zero() {
    let code = run(&["tga".to_owned()], false, true, false, true, false, true);
    assert_eq!(code, 0);
}

// #2112 review (HIGH): a `run_non_tty_no_yes_refuses` test previously
// called the real `run()` relying on the test harness's ambient stdin not
// being a TTY. That is unsound: on a developer's interactive terminal
// `is_tty()` returns true → `InstallGate::NeedsPrompt` → `prompt_yes_no`
// blocks forever on `stdin().read_line()`, hanging the test suite. The
// #2112 regression this was meant to guard — non-TTY + no `--yes` must
// REFUSE — is already exhaustively covered, TTY-independent, by
// `super::install_gate::tests::decide_non_tty_refuses` (mirrors the
// established `upgrade.rs`/`update_engine.rs` split: `resolve_and_apply`
// is side-effecting and validated manually, `decide_apply` is the unit-
// tested pure gate). No `run()`-level replacement is added here.

/// Why: The `--dry-run` JSON envelope is a public contract; pin its shape
/// and the `service_bootstrap` derivation (enabled AND launchd-managed).
/// What: Builds a report over a launchd daemon + a non-daemon with service
/// bootstrap enabled; asserts per-member `service_bootstrap` values.
/// Test: This is the test.
#[test]
fn dry_run_report_shape() {
    let members = vec![
        stable_member_for_test("trusty-search", "trusty-search", ManageStrategy::Launchd),
        stable_member_for_test("tga", "tga", ManageStrategy::None),
    ];
    let dir = std::path::Path::new("/tmp/tctl-test-bin");
    let report = build_dry_run_report(&members, true, dir);
    assert_eq!(report.command, "install");
    assert!(report.dry_run);
    assert!(report.service_bootstrap_enabled);
    assert_eq!(report.members.len(), 2);
    assert!(
        report.members[0].service_bootstrap,
        "launchd member should plan a service bootstrap"
    );
    assert!(
        !report.members[1].service_bootstrap,
        "non-daemon member must not plan a service bootstrap"
    );

    let disabled = build_dry_run_report(&members, false, dir);
    assert!(
        !disabled.members[0].service_bootstrap,
        "disabled service bootstrap must suppress every member"
    );
}

/// Why: #2112 review (MEDIUM) — `--dry-run` always bypasses the
/// interactive picker (`run`'s picker gate requires `!dry_run`), so a
/// no-members `--dry-run` must preview the FULL stable set, not whatever
/// subset a TTY-driven picker might otherwise offer. `run()`'s exit code
/// can't be inspected for the resolved set without capturing stdout, so
/// this pins the composition `run()` itself uses:
/// `select_members_transitive(&[])` (what an empty `members` slice
/// resolves to) fed into `build_dry_run_report`.
/// What: Resolving `&[]` yields every [`stable_set`] member, in order,
/// and the dry-run report carries them all.
/// Test: This is the test.
#[test]
fn dry_run_full_set_when_no_members_named() {
    let resolved = select_members_transitive(&[]);
    let all = super::super::stable_set::stable_set();
    assert_eq!(
        resolved.members.len(),
        all.len(),
        "empty members must resolve to the full stable set"
    );

    let report = build_dry_run_report(
        &resolved.members,
        true,
        std::path::Path::new("/tmp/tctl-test-bin"),
    );
    let names: Vec<&str> = report.members.iter().map(|m| m.member.as_str()).collect();
    let expected: Vec<&str> = all.iter().map(|m| m.crate_name.as_str()).collect();
    assert_eq!(
        names, expected,
        "dry-run preview must list every stable-set member, in order, when none are named"
    );
}

/// Why (#5805): THE acceptance case. `tctl install trusty-installer
/// --dry-run` returned `{"error": "unknown member(s): trusty-installer"}` and
/// exit 3; it must now resolve, preview, and exit 0. `run` is called with
/// `--json` so the interactive picker and the TTY-dependent confirmation gate
/// are both bypassed — `--dry-run` returns before any install side effect, so
/// this test writes nothing to disk.
/// What: asserts exit 0 for both spellings the crate answers to,
/// `trusty-installer` and the `tctl` alias binary.
/// Test: This is the test.
#[test]
fn run_dry_run_accepts_the_installer_itself() {
    for name in ["trusty-installer", "tctl"] {
        let code = run(&[name.to_owned()], false, true, false, true, false, true);
        assert_eq!(
            code, 0,
            "`tctl install {name} --dry-run` must succeed, not report an unknown member"
        );
    }
}

/// Why (#5805): the second half of the acceptance case — the preview must name
/// the destination #5777 canonicalised. Before this change the report had no
/// `install_dir` field at all, so an operator could not tell from a dry run
/// whether the install would land in `$CARGO_HOME/bin` or somewhere a stale
/// copy earlier on PATH would keep shadowing.
/// What: asserts the reported directory is exactly what
/// `download::install_dir_or_fallback()` resolves — the same call
/// `install_one` makes, so the preview cannot name a directory the install
/// would not use — and that it ends in `/bin`.
/// Test: This is the test.
#[test]
fn dry_run_names_the_canonical_install_dir() {
    let members = select_members_transitive(&["trusty-installer".to_owned()]).members;
    let expected = crate::download::install_dir_or_fallback();
    let report = build_dry_run_report(&members, true, &expected);
    assert_eq!(report.install_dir, expected.display().to_string());
    assert!(
        report.install_dir.ends_with("/bin"),
        "the canonical destination is a bin dir: {}",
        report.install_dir
    );
}

/// Why (#5805): the crate ships TWO binaries and the preview listed one. An
/// operator reading `trusty-installer -> trusty-installer` would not learn
/// that `tctl` — the name they actually type — is also about to be replaced.
/// What: asserts the installer's preview row lists both binaries, and that no
/// member's row is empty (the shared table falls back to `[crate_name]`).
/// Test: This is the test.
#[test]
fn dry_run_lists_both_installer_binaries() {
    let all = select_members_transitive(&[]).members;
    let report = build_dry_run_report(&all, true, std::path::Path::new("/tmp/tctl-test-bin"));
    let installer = report
        .members
        .iter()
        .find(|m| m.member == "trusty-installer")
        .expect("the installer previews as a member");
    assert!(installer.binaries.contains(&"trusty-installer".to_owned()));
    assert!(
        installer.binaries.contains(&"tctl".to_owned()),
        "the preview must name the `tctl` alias it also replaces: {:?}",
        installer.binaries
    );
    for m in &report.members {
        assert!(!m.binaries.is_empty(), "{} previewed no binary", m.member);
    }
}

/// Why (#5805): membership pulls a member into the daemon-shaped subcommands,
/// and `tctl start trusty-installer` must not try to bootstrap a launchd job
/// for a CLI. `plans_service_bootstrap` is the shared predicate the preview
/// and the real install loop both read, so pinning it here pins both.
/// What: asserts the installer plans no service bootstrap even with service
/// bootstrapping fully enabled.
/// Test: This is the test.
#[test]
fn installer_never_plans_a_service_bootstrap() {
    let members = select_members_transitive(&["trusty-installer".to_owned()]).members;
    let report = build_dry_run_report(&members, true, std::path::Path::new("/tmp/tctl-test-bin"));
    assert!(report.service_bootstrap_enabled);
    assert!(
        !report.members[0].service_bootstrap,
        "a non-daemon must never plan a launchd service bootstrap"
    );
}

/// Test-only `StableMember` builder — its fields are public for reads but
/// `stable_set` only constructs instances via its own invariant-deriving
/// constructor; this builds one directly for the dry-run shaping test.
fn stable_member_for_test(crate_name: &str, binary: &str, manage: ManageStrategy) -> StableMember {
    StableMember {
        crate_name: crate_name.to_owned(),
        binary: binary.to_owned(),
        daemon: manage == ManageStrategy::Launchd,
        manage,
        required: true,
    }
}

/// Why: The live checklist row must stay a single line; a long or multi-line
/// `anyhow` error chain (e.g. the #3554 health-gate mismatch message, which
/// embeds a raw `--version` output) must never wrap the block.
/// What: Asserts a multi-line error keeps only its first line, and a long
/// first line is truncated to 80 chars with a trailing `…`.
/// Test: This is the test.
#[test]
fn short_reason_truncates_long_and_multiline_errors() {
    let multiline = anyhow::anyhow!("first line\nsecond line\nthird line");
    assert_eq!(short_reason(&multiline), "first line");

    let long = anyhow::anyhow!("x".repeat(120));
    let got = short_reason(&long);
    assert_eq!(got.chars().count(), 81); // 80 chars + the `…` marker
    assert!(got.ends_with('…'));

    let short = anyhow::anyhow!("network timeout");
    assert_eq!(short_reason(&short), "network timeout");
}

/// Why (#4470 HIGH-1, the end-to-end proof): the defect was invisible at the
/// `bootstrap_one` layer — every port-guard test passed while `tctl install`
/// still exited 0 reporting `all_ok: true`, because the call site classified
/// failure with an inline `matches!(action, Failed(_))` that never learned
/// about `RefusedForeignPort`. A guard that detects an orphan and then reports
/// success to automation is worse than no guard. This test crosses the layer
/// boundary that hid it, composing the REAL action→`service_ok` mapping with
/// the REAL report builder and exit-code derivation.
///
/// It fails if `RefusedForeignPort` is dropped from `BootstrapAction::
/// is_failure`: `service_ok` flips to `true`, `all_ok` to `true`, exit code
/// to 0.
///
/// What: a REQUIRED member whose service bootstrap was refused must yield
/// `all_ok == false` and a non-zero exit code.
/// Test: This is the test.
#[test]
fn refused_foreign_port_drives_all_ok_false_and_a_nonzero_exit_code() {
    let action = crate::commands::service_bootstrap::BootstrapAction::RefusedForeignPort(
        "port 7878 is held by pid 9931, which launchd does not supervise".to_owned(),
    );
    // The exact mapping `install_all` performs at its call site.
    let service_ok = !action.is_failure();
    assert!(
        !service_ok,
        "precondition: a refusal must classify as a service-bootstrap failure"
    );

    let report = InstallReport::build(vec![InstallOutcome {
        member: "trusty-search".to_owned(),
        ok: true,
        detail: "installed".to_owned(),
        service_ok,
        service_detail: action.note("trusty-search"),
        shadow_ok: true,
        shadow_detail: String::new(),
        required: true,
        integrity_ok: true,
    }]);

    assert!(
        !report.all_ok,
        "a refused bootstrap must never report all_ok: true — the daemon is not running"
    );
    assert_ne!(
        report.exit_code(),
        0,
        "`tctl install` must exit non-zero when the #4470 port guard refused"
    );
}
