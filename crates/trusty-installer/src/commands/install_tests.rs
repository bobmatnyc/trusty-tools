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

/// Build an `InstallOutcome` with the pre-#2566 default service state
/// (not attempted — `service_ok: true`, empty detail), so tests that only
/// care about the binary-install dimension don't have to repeat the two
/// new fields at every call site.
fn outcome(member: &str, ok: bool, detail: &str) -> InstallOutcome {
    InstallOutcome {
        member: member.to_owned(),
        ok,
        detail: detail.to_owned(),
        service_ok: true,
        service_detail: String::new(),
    }
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
    }]);
    assert!(
        skipped.all_ok,
        "a skipped (not failed) service bootstrap must not flip all_ok"
    );
    assert_eq!(skipped.exit_code(), 0);
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

/// Why: Re-review fix — `binary_size` must resolve under a non-default
/// `CARGO_HOME` (CI installs there). The rule lives in the pure
/// `cargo_bin_dir_from_env`, exercised here WITHOUT mutating the process
/// environment so the test is safe to run in parallel with any other test.
/// What: A non-empty value yields `<that>/bin`.
/// Test: This is the test.
#[test]
fn cargo_bin_dir_from_env_honours_value() {
    assert_eq!(
        cargo_bin_dir_from_env(Some("/tmp/fake-cargo-home")),
        Some(std::path::PathBuf::from("/tmp/fake-cargo-home").join("bin"))
    );
}

/// Why: An empty or absent `CARGO_HOME` must fall back to `~/.cargo/bin`.
/// What: Both `Some("")` and `None` resolve to a path ending in `.cargo/bin`.
/// Test: This is the test.
#[test]
fn cargo_bin_dir_from_env_falls_back() {
    for value in [Some(""), None] {
        if let Some(p) = cargo_bin_dir_from_env(value) {
            assert!(p.ends_with(std::path::Path::new(".cargo").join("bin")));
        }
    }
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
    let report = build_dry_run_report(&members, true);
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

    let disabled = build_dry_run_report(&members, false);
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

    let report = build_dry_run_report(&resolved.members, true);
    let names: Vec<&str> = report.members.iter().map(|m| m.member.as_str()).collect();
    let expected: Vec<&str> = all.iter().map(|m| m.crate_name.as_str()).collect();
    assert_eq!(
        names, expected,
        "dry-run preview must list every stable-set member, in order, when none are named"
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
    }
}
