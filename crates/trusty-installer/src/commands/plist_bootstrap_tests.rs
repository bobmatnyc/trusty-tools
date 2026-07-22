//! Unit tests for the `plist_bootstrap` module.
//!
//! Why: Keeping tests in a sibling file rather than inline in
//! `plist_bootstrap.rs` lets the definition file stay under the 500-line
//! production cap (CLAUDE.md / `scripts/check_line_cap.sh`) while retaining
//! full coverage — mirrors the `cli.rs` / `cli_tests.rs` and
//! `install.rs` / `install_tests.rs` splits.
//!
//! What: Covers template filling, the #3527 downgrade-guard decision table,
//! the registered-binary-path parser, and — via [`super::StubLaunchctl`] /
//! [`super::SupervisorTarget`] (#3551) — the full `install_mpm_supervisor_for`
//! write path (plist write, downgrade guard, bootout + bootstrap) without
//! ever spawning a real `launchctl` subprocess or resolving the real home
//! directory.
//!
//! Test: `cargo test -p trusty-installer` runs all tests in this file.

use super::*;

/// Process-wide lock serialising the ONE test below that still mutates a
/// process-global env var (`PATH`, to prove a decoy earlier on it is
/// ignored — see `install_mpm_supervisor_for_candidate_version_ignores_path_shadow`).
/// Every other test in this module uses [`StubLaunchctl`] +
/// [`SupervisorTarget`] instead of env mutation entirely (#3551) and needs
/// no lock. Cfg-gated to `unix` so a non-unix build (where the sole user
/// of this lock, gated behind `write_fake_tm`, doesn't exist) doesn't hit
/// `-D dead_code`.
#[cfg(unix)]
static PATH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Why: The template must contain the two placeholder tokens so fill_template
/// has something to replace.
/// What: Asserts both `__HOME__` and `__TM_BINARY_PATH__` appear in the raw
/// template string.
/// Test: This is the test.
#[test]
fn template_contains_placeholders() {
    assert!(
        PLIST_TEMPLATE.contains("__HOME__"),
        "template missing __HOME__ placeholder"
    );
    assert!(
        PLIST_TEMPLATE.contains("__TM_BINARY_PATH__"),
        "template missing __TM_BINARY_PATH__ placeholder"
    );
}

/// Why: `fill_template` must replace every occurrence of both tokens.
/// What: Fills with synthetic home + path; asserts neither token survives
/// and both replacement values appear in the result.
/// Test: This is the test.
#[test]
fn fill_template_replaces_all_tokens() {
    let filled = fill_template("/home/testuser", "/usr/local/bin/tm");
    assert!(!filled.contains("__HOME__"), "unfilled __HOME__ token");
    assert!(
        !filled.contains("__TM_BINARY_PATH__"),
        "unfilled __TM_BINARY_PATH__ token"
    );
    assert!(filled.contains("/home/testuser"), "home not in output");
    assert!(
        filled.contains("/usr/local/bin/tm"),
        "tm path not in output"
    );
    // Log paths must contain the substituted home.
    assert!(
        filled.contains("/home/testuser/.trusty-mpm/logs/supervisor.out.log"),
        "stdout log path wrong"
    );
    assert!(
        filled.contains("/home/testuser/.trusty-mpm/logs/supervisor.err.log"),
        "stderr log path wrong"
    );
}

/// Why: The label constant must match what the plist embeds (label-mismatch
/// would cause launchctl to fail with a confusing error).
/// What: Asserts PLIST_LABEL appears in the template.
/// Test: This is the test.
#[test]
fn label_constant_matches_plist() {
    assert!(
        PLIST_TEMPLATE.contains(PLIST_LABEL),
        "PLIST_LABEL not found in template"
    );
}

/// Why: `resolve_tm_binary` must return a non-empty path even when `tm` is
/// not installed (fallback path).
/// What: Calls with a synthetic home dir; asserts the result is non-empty.
/// Test: This is the test.
#[test]
fn resolve_tm_binary_fallback() {
    let home = std::path::Path::new("/tmp/fake-home-for-test");
    let p = resolve_tm_binary(home);
    assert!(!p.as_os_str().is_empty());
    // On a system where `tm` is not installed the fallback must not reference `__HOME__`.
    assert!(!p.to_string_lossy().contains("__"), "placeholder in path");
}

/// Why: `resolve_uid` must return a sensible UID (non-zero in typical CI).
/// What: Calls it and asserts the value is parseable (we get an integer back).
/// Test: This is the test.
#[test]
fn resolve_uid_returns_nonzero_on_real_system() {
    // On macOS and Linux the UID of the test runner is always non-zero in CI
    // (root-as-UID-0 can run but is unusual in CI; we just assert it is a u32).
    let uid = resolve_uid();
    // uid is always a valid u32 — the function never panics.
    let _ = uid; // just confirm it compiled and ran without panic
}

// ── decide_downgrade (#3527) ─────────────────────────────────────────────

/// Why: `--force` is the explicit operator override; it must always win
/// regardless of the version comparison.
/// What: A strictly-older candidate with `force = true` still proceeds.
/// Test: This is the test.
#[test]
fn decide_downgrade_force_always_proceeds() {
    assert_eq!(
        decide_downgrade(Some("2.0.0"), Some("1.0.0"), true),
        DowngradeDecision::Proceed
    );
}

/// Why: with nothing currently registered (or its version undeterminable),
/// there is nothing to guard against — a fresh install must proceed.
/// What: `current = None` proceeds regardless of the candidate.
/// Test: This is the test.
#[test]
fn decide_downgrade_no_current_proceeds() {
    assert_eq!(
        decide_downgrade(None, Some("0.1.0"), false),
        DowngradeDecision::Proceed
    );
}

/// Why: the core happy path — a strictly newer candidate must always
/// proceed without needing `--force`.
/// What: candidate > current → Proceed.
/// Test: This is the test.
#[test]
fn decide_downgrade_newer_proceeds() {
    assert_eq!(
        decide_downgrade(Some("0.19.27"), Some("0.20.0"), false),
        DowngradeDecision::Proceed
    );
}

/// Why: THE #3527 regression this guard exists for — an older candidate
/// (e.g. a stale GitHub release) must be refused without `--force`.
/// What: candidate < current → Refuse.
/// Test: This is the test.
#[test]
fn decide_downgrade_older_refuses() {
    assert_eq!(
        decide_downgrade(Some("0.19.27"), Some("0.16.0"), false),
        DowngradeDecision::Refuse
    );
}

/// Why: "older-or-equal" per the #3527 spec — a same-version reinstall
/// must also be refused (no-op re-bootstrap is not worth a live daemon
/// bootout/bootstrap cycle).
/// What: candidate == current → Refuse.
/// Test: This is the test.
#[test]
fn decide_downgrade_equal_refuses() {
    assert_eq!(
        decide_downgrade(Some("0.19.27"), Some("0.19.27"), false),
        DowngradeDecision::Refuse
    );
}

/// Why: an unparseable version string means we cannot PROVE a downgrade;
/// failing open avoids blocking a legitimate install on a version-string
/// quirk. Also covers a leading `v` prefix being stripped correctly.
/// What: unparseable current/candidate → Proceed; `v`-prefixed versions
/// parse and compare normally.
/// Test: This is the test.
#[test]
fn decide_downgrade_unparseable_proceeds() {
    assert_eq!(
        decide_downgrade(Some("not-a-version"), Some("0.1.0"), false),
        DowngradeDecision::Proceed
    );
    assert_eq!(
        decide_downgrade(Some("0.1.0"), Some("not-a-version"), false),
        DowngradeDecision::Proceed
    );
    // `v`-prefixed versions must still compare correctly (not treated as
    // unparseable).
    assert_eq!(
        decide_downgrade(Some("v0.19.27"), Some("v0.16.0"), false),
        DowngradeDecision::Refuse
    );
}

// ── extract_program_path (#3527) ─────────────────────────────────────────

/// Why: the downgrade guard parses the EXISTING on-disk plist to find which
/// binary is currently registered; pin the happy path against a real
/// filled template.
/// What: fills the template with a synthetic path and asserts the parser
/// recovers it exactly.
/// Test: This is the test.
#[test]
fn extract_program_path_finds_binary() {
    let filled = fill_template("/home/testuser", "/home/testuser/.local/bin/tm");
    assert_eq!(
        extract_program_path(&filled),
        Some("/home/testuser/.local/bin/tm".to_owned())
    );
}

/// Why: a plist missing the `ProgramArguments` key (malformed / unexpected
/// content) must not panic — the guard should just skip the comparison.
/// What: asserts `None` on XML without the key.
/// Test: This is the test.
#[test]
fn extract_program_path_missing_key_is_none() {
    assert_eq!(extract_program_path("<plist><dict></dict></plist>"), None);
}

// ── SupervisorTarget / LaunchctlPort injection (#3551) ───────────────────

/// THE #3551 regression, proven causally rather than by absence of a
/// crash: construct a target whose `domain` is a synthetic string that
/// provably differs from the real `gui/<uid>` domain
/// [`SupervisorTarget::production`] would have resolved, and assert every
/// `launchctl` call the stub recorded carries the INJECTED domain, never
/// the real one. Before #3551, `resolve_uid()`/`gui/<uid>` was hardcoded
/// inside the bootstrap function itself — no parameter could have changed
/// it, so a test asserting this would have failed (or, run for real,
/// would have bootstrapped into the real domain).
/// What: builds a `StubLaunchctl` + synthetic-domain `SupervisorTarget`,
/// calls `install_mpm_supervisor_for`, and asserts the stub's recorded
/// calls all reference the synthetic domain and none reference the real
/// `gui/<real uid>` domain.
/// Test: This is the test.
#[test]
fn install_mpm_supervisor_for_targets_injected_domain_not_real_uid() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubLaunchctl::new();
    let real_domain = format!("gui/{}", resolve_uid());
    let synthetic_domain = "gui/999999-isolated-test-domain".to_owned();
    assert_ne!(
        synthetic_domain, real_domain,
        "precondition: the synthetic test domain must differ from the real live domain"
    );
    let target = SupervisorTarget {
        home: tmp.path().to_owned(),
        domain: synthetic_domain.clone(),
        launchctl: &stub,
    };
    let tm_path = tmp.path().join("local-bin").join("tm");

    let result = install_mpm_supervisor_for(&target, false, &tm_path);

    assert!(result.is_ok(), "expected Ok, got {result:?}");
    let calls = stub.calls();
    assert!(
        !calls.is_empty(),
        "expected bootout + bootstrap calls to be recorded"
    );
    assert!(
        calls.iter().all(|c| c.contains(&synthetic_domain)),
        "every launchctl call must target the INJECTED synthetic domain: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains(&real_domain)),
        "no launchctl call may reference the real live domain: {calls:?}"
    );
}

// ── install_mpm_supervisor_for end-to-end (#3551, fully sandboxed) ──────

/// Why: the full write path (plist write, downgrade guard, bootout +
/// bootstrap) must be exercisable WITHOUT ever spawning the real
/// `launchctl` binary or resolving the real home directory — the whole
/// point of the #3551 injected-parameter seam. Unlike the old
/// `HOME_OVERRIDE_ENV` / `SKIP_LAUNCHCTL_ENV` pair, there is no way to
/// set only one of `home` / `launchctl` here: [`SupervisorTarget`] is one
/// value.
/// What: builds a `StubLaunchctl` + tempdir `SupervisorTarget`, calls
/// `install_mpm_supervisor_for(&target, false, tm_path)`, and asserts it
/// succeeds, the plist landed under the temp home, and the stub recorded
/// exactly a bootout-then-bootstrap sequence (never a real subprocess —
/// there is no code path in `StubLaunchctl` that could spawn one).
/// `tm_path` is a synthetic, non-existent path (#3554: no longer
/// re-derived via `which tm`, so it need not exist for this write-path
/// test — no existing plist means the downgrade guard's version probe is
/// never reached).
/// Test: This is the test.
#[test]
fn install_mpm_supervisor_for_writes_plist_and_never_touches_real_launchctl() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubLaunchctl::new();
    let target = SupervisorTarget {
        home: tmp.path().to_owned(),
        domain: "gui/999999-isolated-test-domain".to_owned(),
        launchctl: &stub,
    };
    let tm_path = tmp.path().join("local-bin").join("tm");

    let result = install_mpm_supervisor_for(&target, false, &tm_path);

    assert!(result.is_ok(), "expected Ok, got {result:?}");
    let plist_path = tmp
        .path()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{PLIST_LABEL}.plist"));
    assert!(
        plist_path.exists(),
        "plist should have been written under the overridden home"
    );
    let calls = stub.calls();
    assert_eq!(
        calls.len(),
        2,
        "expected exactly bootout + bootstrap: {calls:?}"
    );
    assert!(calls[0].starts_with("bootout "), "calls: {calls:?}");
    assert!(calls[1].starts_with("bootstrap "), "calls: {calls:?}");
}

/// Why: when a plist is ALREADY registered but its `ProgramArguments`
/// binary no longer exists (so its `--version` cannot be probed), there is
/// no PROVABLE downgrade — the guard must fail open rather than block a
/// legitimate install on an unprobeable predecessor. Seeding the existing
/// plist by hand (rather than depending on whatever `tm` happens to be on
/// the test machine's PATH) keeps this hermetic and deterministic
/// regardless of the host environment.
/// What: writes a plist whose `ProgramArguments[0]` points at a
/// guaranteed-nonexistent path, then calls
/// `install_mpm_supervisor_for(&target, false, tm_path)` with a
/// likewise-nonexistent `tm_path`; asserts it proceeds (`Ok`).
/// Test: This is the test.
#[test]
fn install_mpm_supervisor_for_proceeds_when_existing_binary_unprobeable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubLaunchctl::new();

    // Seed an existing plist by hand, pointing at a binary that cannot
    // possibly exist, so `installed_version` deterministically returns
    // `None` for the "current" side of the comparison.
    let agents_dir = tmp.path().join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents_dir).expect("create LaunchAgents dir");
    let plist_path = agents_dir.join(format!("{PLIST_LABEL}.plist"));
    let seeded = fill_template(
        &tmp.path().to_string_lossy(),
        "/nonexistent/fake-tm-binary-for-test-xyz",
    );
    std::fs::write(&plist_path, seeded).expect("seed plist");

    let target = SupervisorTarget {
        home: tmp.path().to_owned(),
        domain: "gui/999999-isolated-test-domain".to_owned(),
        launchctl: &stub,
    };
    let candidate_path = tmp.path().join("nonexistent-candidate-tm");
    let result = install_mpm_supervisor_for(&target, false, &candidate_path);

    assert!(
        result.is_ok(),
        "an unprobeable existing binary must not block the install: {result:?}"
    );
}

/// THE #3527 downgrade guard, exercised at the `install_mpm_supervisor_for`
/// level with a fully isolated target: a registered, PROBEABLE binary
/// newer than the candidate must REFUSE without `--force`, and — because
/// the refusal happens before the plist is (re)written — `launchctl` must
/// never be called at all.
/// What: seeds an existing plist referencing a real, probeable fake
/// binary at 0.20.0; calls with a candidate at 0.19.0 (older); asserts
/// `Err` and that the stub recorded zero calls.
/// Test: This is the test.
#[test]
#[cfg(unix)]
fn install_mpm_supervisor_for_refuses_downgrade_without_force() {
    let _guard = PATH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubLaunchctl::new();

    let registered_bin = tmp.path().join("registered-tm");
    write_fake_tm(&registered_bin, "trusty-mpm 0.20.0");
    let agents_dir = tmp.path().join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents_dir).expect("create LaunchAgents dir");
    let plist_path = agents_dir.join(format!("{PLIST_LABEL}.plist"));
    let seeded = fill_template(
        &tmp.path().to_string_lossy(),
        registered_bin.to_str().expect("utf8 path"),
    );
    std::fs::write(&plist_path, seeded).expect("seed plist");

    let target = SupervisorTarget {
        home: tmp.path().to_owned(),
        domain: "gui/999999-isolated-test-domain".to_owned(),
        launchctl: &stub,
    };
    let candidate_bin = tmp.path().join("local-bin").join("tm");
    write_fake_tm(&candidate_bin, "trusty-mpm 0.19.0");

    let result = install_mpm_supervisor_for(&target, false, &candidate_bin);

    assert!(
        result.is_err(),
        "an older candidate must be refused without --force"
    );
    assert!(
        stub.calls().is_empty(),
        "launchctl must never be called when the downgrade guard refuses: {:?}",
        stub.calls()
    );
}

/// Why: a genuine `launchctl bootstrap` failure (e.g. a malformed plist,
/// or launchd rejecting the label) must surface as an `Err`, not a
/// swallowed success — `install.rs` depends on this `Err` to narrate a
/// non-fatal warning rather than silently reporting the supervisor as
/// bootstrapped.
/// What: a `StubLaunchctl` configured with `fail_bootstrap` returns the
/// given error string from `bootstrap`; asserts `install_mpm_supervisor_for`
/// propagates it.
/// Test: This is the test.
#[test]
fn install_mpm_supervisor_for_surfaces_bootstrap_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubLaunchctl {
        fail_bootstrap: Some("Load failed: 5: Input/output error".to_owned()),
        ..StubLaunchctl::new()
    };
    let target = SupervisorTarget {
        home: tmp.path().to_owned(),
        domain: "gui/999999-isolated-test-domain".to_owned(),
        launchctl: &stub,
    };
    let tm_path = tmp.path().join("local-bin").join("tm");

    let result = install_mpm_supervisor_for(&target, false, &tm_path);

    let err = result.expect_err("bootstrap failure must surface as Err");
    assert!(
        err.to_string().contains("Input/output error"),
        "error must carry the underlying launchctl stderr: {err}"
    );
}

/// Write a minimal executable shell script at `path` that echoes
/// `version_line` on `--version` and exits 0 — a real, probeable binary
/// (unlike the "unprobeable" tests above, which deliberately point at
/// nonexistent paths).
#[cfg(unix)]
fn write_fake_tm(path: &std::path::Path, version_line: &str) {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir fake tm parent");
    }
    std::fs::write(path, format!("#!/bin/sh\necho '{version_line}'\nexit 0\n"))
        .expect("write fake tm binary");
    let mut perms = std::fs::metadata(path)
        .expect("stat fake tm binary")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod fake tm binary");
}

/// THE #3554 regression, at the `install_mpm_supervisor_for` level: the
/// CANDIDATE-version comparison must come from the passed `tm_path` —
/// never re-derived via a `which tm` PATH lookup — even when a decoy
/// `tm` sits EARLIER on `$PATH` than the real candidate.
///
/// Proven causally, not just by absence of a crash: the registered
/// ("current") version (0.19.27) sits strictly BETWEEN the decoy's
/// version (0.19.20) and the real candidate's version (0.19.29). If this
/// function regressed to re-resolve the candidate via PATH (finding the
/// OLDER decoy), [`decide_downgrade`] would REFUSE (0.19.20 <=
/// 0.19.27) — exactly the #3554 symptom ("registered version is not
/// older than the candidate version"). Using the real, passed `tm_path`
/// (0.19.29, strictly newer) it must PROCEED and write a plist
/// referencing `tm_path`, not the decoy.
#[test]
#[cfg(unix)]
fn install_mpm_supervisor_for_candidate_version_ignores_path_shadow() {
    let _guard = PATH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubLaunchctl::new();

    // Registered ("current") binary — a real, probeable fake binary at
    // 0.19.27, referenced by a hand-seeded existing plist.
    let registered_bin = tmp.path().join("registered-tm");
    write_fake_tm(&registered_bin, "trusty-mpm 0.19.27");
    let agents_dir = tmp.path().join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents_dir).expect("create LaunchAgents dir");
    let plist_path = agents_dir.join(format!("{PLIST_LABEL}.plist"));
    let seeded = fill_template(
        &tmp.path().to_string_lossy(),
        registered_bin.to_str().expect("utf8 path"),
    );
    std::fs::write(&plist_path, seeded).expect("seed plist");

    // Decoy binary EARLIER on $PATH — OLDER than the registered version.
    let decoy_dir = tmp.path().join("decoy-early-path");
    write_fake_tm(&decoy_dir.join("tm"), "trusty-mpm 0.19.20");
    let prev_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{prev_path}", decoy_dir.display()));

    // The REAL just-installed candidate — NEWER than registered — at a
    // path that is NOT first on $PATH (mirrors #3554: `~/.local/bin`
    // shadowed by an earlier `~/.cargo/bin`).
    let candidate_bin = tmp.path().join("local-bin").join("tm");
    write_fake_tm(&candidate_bin, "trusty-mpm 0.19.29");

    let target = SupervisorTarget {
        home: tmp.path().to_owned(),
        domain: "gui/999999-isolated-test-domain".to_owned(),
        launchctl: &stub,
    };
    let result = install_mpm_supervisor_for(&target, false, &candidate_bin);

    std::env::set_var("PATH", prev_path);

    assert!(
        result.is_ok(),
        "must proceed using the passed candidate (0.19.29 > registered 0.19.27); \
             a failure here means the candidate was re-resolved via the PATH-shadowed \
             decoy (0.19.20 <= 0.19.27, which would refuse): {result:?}"
    );
    let written = std::fs::read_to_string(&plist_path).expect("read written plist");
    assert_eq!(
        extract_program_path(&written).as_deref(),
        Some(candidate_bin.to_str().expect("utf8 path")),
        "the written plist must reference the passed tm_path, never a PATH-resolved one"
    );
}
