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

/// A root with FULL file discovery but a reference grammar that matches nothing.
///
/// This is the #5440 shape, and the reason [`binary_refuses_a_vacuous_scan`]
/// cannot catch it: that test drives an EMPTY root, so the file counts and the
/// reference counts fall to zero together and either floor would fire. Here the
/// walk finds every file at full strength while zero references resolve —
/// exactly what renaming the `# Spec References` block marker in
/// `trusty_common::sld::inline` produces against the real tree.
///
/// The drift is modelled in the FIXTURE rather than by mutating the parser: each
/// code file declares its references under `# Specification References` and each
/// spec doc under a `specification_refs:` frontmatter key, so both grammars miss
/// and neither emits a diagnostic. The result is a run that reports 0 errors
/// over a tree it never checked.
fn discovered_but_unparseable_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("docs/specs")).expect("docs/specs");
    std::fs::write(root.join("docs/specs/README.md"), "# Spec catalog\n").expect("write catalog");

    // Comfortably above MIN_SPEC_DOCS (20): real spec documents that each
    // declare a reference under a frontmatter key the reader does not know.
    for i in 0..25 {
        let body = format!(
            "---\nspecification_refs:\n  - id: SPEC-X-{i:02}~draft\n    \
             path: docs/specs/x.md\n    anchor: SPEC-X-{i:02}~draft\n---\n\n# DOC-{i} — X\n\nbody\n"
        );
        std::fs::write(root.join(format!("docs/specs/spec-{i:02}.md")), body).expect("write spec");
    }

    // Comfortably above MIN_CODE_FILES (200): real source files that each
    // declare a reference under a marker the inline scanner does not match.
    std::fs::create_dir_all(root.join("crates/demo/src")).expect("crates/demo/src");
    for i in 0..210 {
        let body = format!(
            "//! # Specification References\n//!\n//! - [`SPEC-X-{i:02}~draft`]\
             (docs/specs/x.md#SPEC-X-{i:02}~draft)\n\npub fn unit_{i}() {{}}\n"
        );
        std::fs::write(root.join(format!("crates/demo/src/m{i:03}.rs")), body).expect("write code");
    }
    dir
}

/// Full discovery with zero references resolved must fail, not report clean.
///
/// Pre-fix this exits 0: the floors only knew `spec_docs` and `code_files`,
/// both of which are healthy here, so the gate passed over a tree where not one
/// reference was checked (#5440-followup).
#[test]
fn binary_refuses_full_discovery_with_zero_references() {
    let root = discovered_but_unparseable_root();
    let out = Command::new(env!("CARGO_BIN_EXE_sld-lint"))
        .arg("--root")
        .arg(root.path())
        .output()
        .expect("sld-lint runs");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Discovery is at full strength AND the run found nothing wrong — the two
    // facts that together make this a vacuous pass rather than an early abort.
    assert!(
        stdout.contains("scanned 25 spec doc(s) + 210 code file(s)"),
        "expected healthy discovery counts\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("resolved 0 frontmatter + 0 inline reference(s)"),
        "expected the summary to PRINT the zeroed reference counts\nstdout: {stdout}"
    );
    assert!(
        stdout.contains("0 error(s)"),
        "the drifted grammar must emit no diagnostics — that is what makes the \
         pass vacuous\nstdout: {stdout}"
    );
    assert!(
        !out.status.success(),
        "a run that walked 235 files and resolved 0 references exited 0 — a floor \
         that counts discovery instead of work is back\nstdout: {stdout}\nstderr: {stderr}"
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
