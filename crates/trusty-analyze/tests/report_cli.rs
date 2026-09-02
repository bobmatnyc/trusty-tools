//! `trusty-analyze report` exists and is wired to the pipeline (#6669).
//!
//! Why: the flag-mapping unit tests in `commands::report` prove the arguments
//! reach a request, but not that the SUBCOMMAND exists on the built binary —
//! a feature-gated verb that failed to compile into the bin would pass every
//! one of them. This runs the real binary.
//! What: asserts `report --help` documents `--template` and `--code-only`, and
//! that a run against a manifest that is not there fails by naming the path
//! rather than hanging or panicking. It deliberately does not run a full
//! render: that always calls inference (#5454) and takes minutes, so the
//! exit-0-writes-a-file proof is the live smoke run recorded on the PR.
//! Test: this file. Compiled only under `--features review`, like the verb.
#![cfg(feature = "review")]

use std::process::Command;

/// The binary under test, built by cargo for this integration target.
const BIN: &str = env!("CARGO_BIN_EXE_trusty-analyze");

/// Why: a verb that does not compile into the binary is invisible to every
/// unit test of its argument mapping.
/// What: `report --help` exits 0 and documents both new flags.
/// Test: this test itself.
#[test]
fn the_report_verb_is_on_the_binary() {
    let out = Command::new(BIN)
        .args(["report", "--help"])
        .output()
        .expect("run trusty-analyze report --help");
    assert!(
        out.status.success(),
        "report --help must exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let help = String::from_utf8_lossy(&out.stdout);
    for flag in ["--manifest", "--template", "--code-only", "--out"] {
        assert!(help.contains(flag), "help must document {flag}:\n{help}");
    }
}

/// Why: an operator who mistypes a manifest path must see the path, not a
/// stack trace or a silent hang — this is the first thing the pipeline checks.
/// What: a missing manifest exits non-zero and names the path.
/// Test: this test itself.
#[test]
fn a_missing_manifest_is_refused_by_name() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing = dir.path().join("no-such-manifest.toml");
    let out = Command::new(BIN)
        .args([
            "report",
            "--manifest",
            missing.to_str().expect("utf-8 path"),
            "--template",
            "cast",
            "--code-only",
        ])
        .output()
        .expect("run trusty-analyze report");
    assert!(!out.status.success(), "a missing manifest must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-such-manifest.toml"),
        "the refusal must name the path it tried:\n{stderr}"
    );
}
