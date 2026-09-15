//! Live end-to-end test for `tm meta run --demo` (#1053 follow-up to #1049/#1051).
//!
//! Why: the metaharness demo is inherently integration-level — it launches a
//! REAL `claude` CLI session inside tmux, lets it write `hello_metaharness.txt`,
//! polls for the session to exit, and verifies the artifact. None of that can run
//! in CI (no `claude`, no OAuth, no tmux guarantees), so this test is
//! `#[ignore]`-gated; the CI-runnable coverage is the pure poll/verify unit tests
//! in the `tm` binary's `commands::meta::{launch,verify}` modules. This file is
//! the documented local-validation entry point for the POC success criterion.
//! What: runs the built `tm` binary (via `common::tm_bin`) as
//! `meta run --demo --project <tmp> --timeout-secs <n>` against a throwaway dir
//! and asserts the process exits 0 (the #1051 acceptance criterion) and the
//! artifact exists with the expected marker.
//! Test: this file (run with `TRUSTY_MPM_META_DEMO_E2E=1 cargo test -p
//! trusty-mpm --test meta_demo_e2e -- --include-ignored`).

mod common;

use std::process::Command;

/// Declares that THIS host has the live prerequisites (#7998).
///
/// Why: `#[ignore]` keeps the test out of a default run, but `--include-ignored`
/// is the local baseline gate, and there it launched a real `claude` session on
/// every host — timing out at 187-220s and failing on any machine without a
/// logged-in CLI. A timeout reports as a defect in the code under test; a
/// missing prerequisite is not one. `#[ignore]` alone cannot tell them apart,
/// because it cannot see the environment.
/// What: unset (or not `1`) ⇒ print why and return, so `--include-ignored` is
/// fast and honest; set to `1` ⇒ the full live run below, unchanged.
/// Test: `meta_run_demo_writes_and_verifies_artifact` — the only reader.
const LIVE_ENV: &str = "TRUSTY_MPM_META_DEMO_E2E";

/// `tm meta run --demo` boots a real claude session, writes the artifact, and
/// the command verifies it and exits 0.
///
/// Why: this is the epic #1045 success criterion realised end-to-end. It proves
/// the launch (#1049) + poll/verify (#1051) wiring drives a real session to a
/// checkable result.
/// What: spawns the `tm` binary against a tempdir with a generous timeout; on a
/// clean run the process exits 0 and `hello_metaharness.txt` contains the
/// `metaharness OK` marker. Requires `claude` (logged in), `tmux`, and the
/// trusty-mpm framework installed locally.
/// Test: this test (ignored in CI; opt in with [`LIVE_ENV`]).
#[test]
#[ignore = "live: requires claude CLI (OAuth), tmux, and the installed framework — set TRUSTY_MPM_META_DEMO_E2E=1 (#1053, #7998)"]
fn meta_run_demo_writes_and_verifies_artifact() {
    if std::env::var(LIVE_ENV).as_deref() != Ok("1") {
        eprintln!(
            "skipping meta_run_demo_writes_and_verifies_artifact: set {LIVE_ENV}=1 to run it. \
             It launches a REAL claude session (logged-in CLI + tmux + the installed framework) \
             and takes minutes; without those it only times out (#7998)."
        );
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path();

    // #7568: deliberately NOT confined to a scratch `$HOME`. This is a live,
    // `#[ignore]`d run against the framework the operator actually installed —
    // a scratch home would make it untestable rather than hermetic.
    let bin = common::tm_bin();
    let output = Command::new(bin)
        .args([
            "meta",
            "run",
            "--demo",
            "--no-provision",
            "--project",
            &project.to_string_lossy(),
            "--timeout-secs",
            "180",
        ])
        .output()
        .expect("failed to run tm meta run --demo");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("--- tm meta run --demo stdout ---\n{stdout}");
    eprintln!("--- tm meta run --demo stderr ---\n{stderr}");

    assert!(
        output.status.success(),
        "meta run --demo must exit 0 on success; status: {:?}",
        output.status
    );

    let artifact = project.join("hello_metaharness.txt");
    let body = std::fs::read_to_string(&artifact).expect("artifact must exist after a passing run");
    assert!(
        body.contains("metaharness OK"),
        "artifact must contain the expected marker, got: {body}"
    );
}
