//! End-to-end scan-floor gate for the `publish-guard` BINARY (issue #4618).
//!
//! Why: `main`'s unit tests cover `scan_floor_violation` as a pure predicate,
//! which proves the arithmetic but not the wiring. Deleting the
//! `if let Some(msg) = scan_floor_violation(checked)` block from `main` leaves
//! those unit tests green and silently restores the exact vacuous pass #4618
//! exists to eliminate — `checked 0 publishable crate(s) — 0 drifted` exiting 0.
//! Only invoking the real binary can catch that, so this is the Rust
//! counterpart to `scripts/check_scan_floor_selftest.sh`.
//! What: runs the built binary against a root holding an empty `crates/` and
//! asserts a non-zero exit whose stderr names the floor.
//! Test: this file is the test.
//!
//! Deliberately offline: every fixture crate would make `check_crate` reach
//! crates.io, so the fixture holds ZERO crates. That is also the exact
//! condition under test, and `main` reaches the floor before any network call.
//! The floor's upper bound is covered by `main`'s `floor_accepts_real_scan`
//! unit test rather than a live run.

use std::process::Command;

/// A run that discovered no crates must exit non-zero naming the scan floor.
///
/// Guards the CALL SITE, not the predicate: this goes red if the
/// `scan_floor_violation` check is removed from `main`.
#[test]
fn binary_refuses_a_vacuous_scan() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(root.path().join("crates")).expect("crates");

    let out = Command::new(env!("CARGO_BIN_EXE_publish-guard"))
        .arg("--root")
        .arg(root.path())
        .output()
        .expect("publish-guard runs");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The run must have completed normally all the way to the summary — that is
    // what proves the non-zero exit below is the FLOOR and not an early abort
    // (a missing `crates/` dir, for instance, fails much earlier).
    assert!(
        stdout.contains("checked 0 publishable crate(s)"),
        "expected a completed run reporting a zero scan\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !out.status.success(),
        "a run that checked no crates exited 0 — the #4618 defect is back\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("SCAN FLOOR"),
        "the failure must name the scan floor\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("OK — no version-parity drift detected"),
        "the vacuous run must never reach the OK line\nstdout: {stdout}"
    );
}
