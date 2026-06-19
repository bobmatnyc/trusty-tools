//! Live end-to-end test for `tm meta run --demo` (#1053 follow-up to #1049/#1051).
//!
//! Why: the metaharness demo is inherently integration-level — it launches a
//! REAL `claude` CLI session inside tmux, lets it write `hello_metaharness.txt`,
//! polls for the session to exit, and verifies the artifact. None of that can run
//! in CI (no `claude`, no OAuth, no tmux guarantees), so this test is
//! `#[ignore]`-gated; the CI-runnable coverage is the pure poll/verify unit tests
//! in the `tm` binary's `commands::meta::{launch,verify}` modules. This file is
//! the documented local-validation entry point for the POC success criterion.
//! What: runs the built `tm` binary (`CARGO_BIN_EXE_tm`) as
//! `meta run --demo --project <tmp> --timeout-secs <n>` against a throwaway dir
//! and asserts the process exits 0 (the #1051 acceptance criterion) and the
//! artifact exists with the expected marker.
//! Test: this file (run with `cargo test -p trusty-mpm --test meta_demo_e2e -- --include-ignored`).

use std::process::Command;

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
/// Test: this test (ignored in CI).
#[test]
#[ignore = "live: requires claude CLI (OAuth), tmux, and the installed framework — #1053"]
fn meta_run_demo_writes_and_verifies_artifact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path();

    let bin = env!("CARGO_BIN_EXE_tm");
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
