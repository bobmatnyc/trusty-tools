//! Integration test (#7538): `tagent --help` exits 0 and prints on stdout.
//!
//! Why: `--help` exited 1 with the help text on stderr behind an `Error:`
//! prefix, because the top-level dispatch mapped clap's `DisplayHelp` error
//! onto an `anyhow` return like any parse failure. Scripts and QA harnesses
//! gate on that exit code, so a healthy binary read as broken. The defect
//! lived in the process's exit status, which only the REAL built binary can
//! report — an in-process parse test sees the `Err` but not what `main` did
//! with it.
//! What: spawns the built binary with `--help` and `-h` from an isolated
//! `$HOME`/cwd, asserting each exits 0 and writes the usage text to stdout.
//! Test: this file IS the test.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_tagent");

/// Run `tagent <flag>` in `dir` with an isolated `$HOME` and no ambient
/// project hint (#4826), returning `(exit_code, stdout, stderr)`.
///
/// The isolation mirrors `tests/version_provenance.rs`: this argv still runs
/// the normal startup path, which deploys the bundled agent roster under
/// `$HOME/.trusty-agents/`, and a test must never write that into the
/// developer's real home.
fn help_output(dir: &Path, flag: &str) -> (Option<i32>, String, String) {
    let out = Command::new(BIN)
        .arg(flag)
        .current_dir(dir)
        .env("HOME", dir)
        .env_remove("TAGENT_PROJECT_DIR")
        .env_remove("OPEN_MPM_PROJECT_DIR")
        .output()
        .unwrap_or_else(|e| panic!("spawn `tagent {flag}`: {e}"));
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Why: this is the #7538 defect itself — the exit code every script reads.
/// Test: itself.
#[test]
fn help_flag_exits_zero_and_prints_usage_on_stdout() {
    for flag in ["--help", "-h"] {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (code, stdout, stderr) = help_output(tmp.path(), flag);

        assert_eq!(
            code,
            Some(0),
            "`tagent {flag}` must exit 0; stderr:\n{stderr}"
        );
        assert!(
            stdout.contains("Usage:"),
            "`tagent {flag}` must print the usage text on stdout; got:\n{stdout}"
        );
        assert!(
            !stderr.contains("Error:"),
            "`tagent {flag}` must not present help as an error; stderr:\n{stderr}"
        );
    }
}
