//! The build script's UI-build policy (#8094).
//!
//! Why: `cargo install --path crates/trusty-agents` reported success while
//! embedding `<p>trusty-agents: UI not built.</p>`, because `build.rs` turned
//! every `pnpm build` failure into the placeholder page. A build script's own
//! `#[cfg(test)]` module is never compiled by `cargo test`, so this target
//! `include!`s the same source `build.rs` does and tests it directly.
//! What: one test per arm of the two decisions — the abort path with a real
//! failing command, the spawn-error path, the success path, and the two
//! legitimate placeholder opt-outs.
//! Test: this file.

include!("../build_ui_policy.rs");

/// A `pnpm build` that RUNS and fails aborts the cargo build, and says so.
///
/// The command really runs and really fails, so the `ExitStatus` under test is
/// one the OS produced rather than one a mock asserted into existence.
#[test]
fn a_failing_pnpm_build_aborts_the_cargo_build() {
    let status = std::process::Command::new("sh")
        .args(["-c", "echo ERR_PNPM_NO_MATCHING_VERSION >&2; exit 1"])
        .status();
    assert!(
        status.as_ref().is_ok_and(|s| !s.success()),
        "the fixture command must run and fail: {status:?}"
    );

    let reason = classify_pnpm_step("pnpm build", status)
        .expect_err("a failed pnpm build must not be degraded into a placeholder");
    assert!(reason.contains("pnpm build failed"), "{reason}");
    assert!(reason.contains("SKIP_UI_BUILD=1"), "{reason}");
    assert!(reason.contains("placeholder"), "{reason}");
}

/// A pnpm step that cannot be spawned aborts too — never a silent placeholder.
#[test]
fn an_unspawnable_pnpm_step_aborts_the_cargo_build() {
    let status = std::process::Command::new("trusty-agents-no-such-program-8094").status();
    let reason = classify_pnpm_step("pnpm install", status)
        .expect_err("an unspawnable step must not be degraded into a placeholder");
    assert!(
        reason.contains("pnpm install could not be spawned"),
        "{reason}"
    );
}

/// A zero exit carries the build on.
#[test]
fn a_successful_pnpm_step_carries_on() {
    let status = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .status();
    assert_eq!(classify_pnpm_step("pnpm build", status), Ok(()));
}

/// `SKIP_UI_BUILD=1` still embeds the placeholder, and the warning names it.
#[test]
fn skip_ui_build_names_the_opt_out() {
    let reason = placeholder_reason(true, true).expect("SKIP_UI_BUILD=1 is the deliberate opt-out");
    assert!(reason.contains("SKIP_UI_BUILD=1"), "{reason}");
}

/// A host with no JS toolchain still builds (#112), and the warning says why.
#[test]
fn a_missing_pnpm_names_pnpm() {
    let reason = placeholder_reason(false, false).expect("no pnpm means no UI build is possible");
    assert!(reason.contains("pnpm not found"), "{reason}");
}

/// With the opt-out unset and pnpm present, nothing licenses the placeholder.
#[test]
fn a_usable_toolchain_has_no_placeholder_reason() {
    assert_eq!(placeholder_reason(false, true), None);
}
