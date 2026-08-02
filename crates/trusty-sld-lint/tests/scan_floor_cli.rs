//! End-to-end scan-floor gate for the `sld-lint` BINARY (issue #4618).
//!
//! Why: `main`'s unit tests cover `scan_floor_violation` as a pure predicate,
//! which proves the arithmetic but not the wiring. Deleting the
//! `if let Some(msg) = scan_floor_violation(...)` block from `main` leaves those
//! unit tests green and silently restores the exact vacuous pass #4618 exists to
//! eliminate — a run that discovered nothing exiting 0. Only invoking the real
//! binary can catch that, so this is the Rust counterpart to
//! `scripts/check_scan_floor_selftest.sh`, which does the same for the shell
//! gates.
//! What: runs the built binary against a root whose scan set is empty and
//! asserts a non-zero exit whose stderr names the floor; then runs it against
//! the real workspace so a floor that fires on a healthy tree also fails here.
//! Test: this file is the test.

use std::path::PathBuf;
use std::process::Command;

/// The workspace root, relative to this crate's manifest dir.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root resolves")
}

/// A root the linter can scan successfully but which contains nothing to scan.
///
/// The spec catalog must exist or the run aborts with `LintError::Catalog`
/// before ever reaching the floor — which would make this test pass for the
/// wrong reason.
fn empty_but_valid_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("docs/specs")).expect("docs/specs");
    std::fs::create_dir_all(dir.path().join("crates")).expect("crates");
    std::fs::write(dir.path().join("docs/specs/README.md"), "# Spec catalog\n")
        .expect("write catalog");
    dir
}

/// A run that discovered nothing must exit non-zero naming the scan floor.
///
/// Guards the CALL SITE, not the predicate: this goes red if the
/// `scan_floor_violation` check is removed from `main`.
#[test]
fn binary_refuses_a_vacuous_scan() {
    let root = empty_but_valid_root();
    let out = Command::new(env!("CARGO_BIN_EXE_sld-lint"))
        .arg("--root")
        .arg(root.path())
        .output()
        .expect("sld-lint runs");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The run must have completed normally all the way to the summary — that is
    // what proves the non-zero exit below is the FLOOR and not an early abort.
    assert!(
        stdout.contains("scanned 0 spec doc(s) + 0 code file(s)"),
        "expected a completed run reporting a zero scan\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !out.status.success(),
        "a run that scanned nothing exited 0 — the #4618 defect is back\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("SCAN FLOOR"),
        "the failure must name the scan floor\nstderr: {stderr}"
    );
}

/// The floor must not fire on the real tree — otherwise the test above would
/// pass even with a floor set so high the gate can never go green.
#[test]
fn binary_accepts_the_real_tree() {
    let out = Command::new(env!("CARGO_BIN_EXE_sld-lint"))
        .arg("--root")
        .arg(workspace_root())
        .output()
        .expect("sld-lint runs");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("SCAN FLOOR"),
        "the declared minimums are above what the real tree scans\nstderr: {stderr}"
    );
}
