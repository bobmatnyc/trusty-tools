//! Integration test (#4260): `tagent --version` names the commit that built
//! the binary, and says the same thing every time it is asked.
//!
//! Why: the version string used to end in `build #N`, a counter read from
//! `.trusty-agents/state/build.json` and incremented by the `--version` path
//! itself — so the SAME installed binary reported `build #2435` then
//! `build #2440` across successive runs. A number that changes per invocation
//! cannot identify a build, and it read exactly like one, so a stale binary
//! sat unnoticed and was reported as a code bug. This drives the REAL built
//! binary rather than calling `build_info::version_string()` in-process,
//! because the defect lived in the CLI path (which counter it printed and the
//! disk write it performed), not in the formatter.
//! What: runs `--version` three times from an isolated `$HOME`/cwd and asserts
//! the three stdouts are byte-identical, that the string carries the crate
//! version and the compile-time short SHA, and that no `build #` counter
//! survives anywhere in it.
//! Test: this file IS the test.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_tagent");

/// Short git SHA baked in by `build.rs` for THIS compilation of the crate.
///
/// Why: `cargo:rustc-env` reaches every target in the package, so the test
/// binary and the `tagent` binary are stamped with the same value — the
/// assertion compares the binary's claim against the build's own record
/// instead of re-shelling out to git.
const GIT_HASH: &str = env!("GIT_COMMIT_HASH");

/// Run `tagent --version` in `dir` with an isolated `$HOME` and no ambient
/// project hint (#4826), returning stdout.
fn version_stdout(dir: &Path) -> String {
    let out = Command::new(BIN)
        .arg("--version")
        .current_dir(dir)
        .env("HOME", dir)
        .env_remove("TAGENT_PROJECT_DIR")
        .env_remove("OPEN_MPM_PROJECT_DIR")
        .output()
        .expect("spawn `tagent --version`");
    assert!(
        out.status.success(),
        "`--version` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Why: this is the #4260 defect itself — the same binary must not describe
/// itself differently on the second and third ask.
/// Test: itself.
#[test]
fn version_output_is_identical_across_invocations() {
    let tmp = tempfile::TempDir::new().unwrap();
    let first = version_stdout(tmp.path());
    let second = version_stdout(tmp.path());
    let third = version_stdout(tmp.path());

    assert_eq!(
        first, second,
        "`--version` must be stable across invocations of the same binary"
    );
    assert_eq!(
        second, third,
        "`--version` must be stable across invocations of the same binary"
    );
}

/// Why: stability alone is satisfied by printing nothing useful. The string
/// must also identify WHICH source produced the binary — the question the
/// 2026-07-28 stale-build incident could not answer without a `strings` dump.
/// Test: itself.
#[test]
fn version_output_names_the_commit_that_built_it() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out = version_stdout(tmp.path());

    assert!(
        out.contains(env!("CARGO_PKG_VERSION")),
        "`--version` must carry the crate version: {out}"
    );
    assert!(
        out.contains(GIT_HASH),
        "`--version` must carry the build's short SHA ({GIT_HASH}): {out}"
    );
}

/// Why: the counter is the misleading half of #4260 — a `build #N` token in
/// `--version` is a build identity claim the number cannot honour.
/// Test: itself.
#[test]
fn version_output_carries_no_per_invocation_counter() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out = version_stdout(tmp.path());

    assert!(
        !out.contains("build #"),
        "`--version` must not present a per-invocation counter: {out}"
    );
}

/// Why: `--version` is run by scripts and CI; it has no business creating a
/// state directory or mutating a counter file to answer a read-only question.
/// Test: itself.
#[test]
fn version_output_writes_nothing_to_the_working_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _ = version_stdout(tmp.path());

    let entries: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert!(
        entries.is_empty(),
        "`--version` must not write to the working directory, found: {entries:?}"
    );
}
