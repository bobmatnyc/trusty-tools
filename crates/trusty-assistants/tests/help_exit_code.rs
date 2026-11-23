//! Integration test (#7538): `ta --help` exits 0 and prints on stdout.
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

use tempfile::TempDir;

mod common;

/// Run `ta <flag>` in `dir` with an isolated `$HOME` and no ambient
/// project hint (#4826), returning `(exit_code, stdout, stderr)`.
///
/// The isolation mirrors `tests/version_provenance.rs`: this argv still runs
/// the normal startup path, which deploys the bundled agent roster under
/// `$HOME/.trusty-agents/`, and a test must never write that into the
/// developer's real home.
fn help_output(dir: &TempDir, flag: &str) -> (Option<i32>, String, String) {
    // #11632: `common::ta` sets the isolated `$HOME` and clears the hints.
    let out = common::ta(dir)
        .arg(flag)
        .output()
        .unwrap_or_else(|e| panic!("spawn `ta {flag}`: {e}"));
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
        let (code, stdout, stderr) = help_output(&tmp, flag);

        assert_eq!(code, Some(0), "`ta {flag}` must exit 0; stderr:\n{stderr}");
        assert!(
            stdout.contains("Usage:"),
            "`ta {flag}` must print the usage text on stdout; got:\n{stdout}"
        );
        assert!(
            !stderr.contains("Error:"),
            "`ta {flag}` must not present help as an error; stderr:\n{stderr}"
        );
    }
}

/// Why (#11742): `ta ask --help` and `ta handoff --help` failed as usage
/// errors, so a new user had no way to read a verb's flags. Every verb's
/// `--help` and `-h` must print that verb's usage on stdout and exit 0.
/// Test: itself.
#[test]
fn subcommand_help_exits_zero_and_prints_usage() {
    let cases: &[(&[&str], &str)] = &[
        (&["ask"], "ta ask <instance> <question…>"),
        (&["handoff"], "ta handoff <instance> --to tm|tc"),
        (&["handoff", "izzie", "--to", "tm"], "ta handoff <instance>"),
        (&["config", "profile"], "ta config profile"),
        (&["config"], "ta config"),
        (&["list"], "ta list"),
        (&["mcp-serve"], "ta mcp-serve"),
        (&["ask-daemon"], "ta ask-daemon <instance>"),
    ];
    for (verb, usage) in cases {
        for flag in ["--help", "-h"] {
            let tmp = tempfile::TempDir::new().expect("tempdir");
            let out = common::ta(&tmp)
                .args(*verb)
                .arg(flag)
                .stdin(std::process::Stdio::null())
                .output()
                .expect("spawn ta");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let argv = format!("ta {} {flag}", verb.join(" "));
            assert_eq!(out.status.code(), Some(0), "`{argv}`; stderr:\n{stderr}");
            assert!(stdout.contains("Usage:"), "`{argv}`; stdout:\n{stdout}");
            assert!(stdout.contains(usage), "`{argv}`; stdout:\n{stdout}");
        }
    }
}
