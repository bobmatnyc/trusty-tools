//! End-to-end regression tests for the `TRUSTY_NO_AUTO_DISCOVER` env path
//! (issue #4823).
//!
//! Why: this is the trap the fix exists for, and it is the one path no
//! in-process test can reach. `--no-auto-discover` is declared with
//! `env = "TRUSTY_NO_AUTO_DISCOVER"`, and clap resolves an env-sourced value
//! by reading the *real* process environment during `Cli::parse()`. Before
//! #4823 the arg was a bare `bool`, which under clap means the CLI flag is a
//! presence flag but the env var goes through strict `FromStr<bool>` — so the
//! `=1` spelling documented in the README and the #314 changelog was rejected
//! with `invalid value '1' … [possible values: true, false]` and the daemon
//! refused to boot. Under launchd that becomes a throttle-loop: a plist
//! carrying `TRUSTY_NO_AUTO_DISCOVER=1` in `EnvironmentVariables` produces a
//! service that never comes up.
//!
//! Two pre-existing tests looked like they covered this and did not:
//! `commands::start::tests::no_auto_discover_resolution` asserts `Some("1")`
//! suppresses the scan, but against a locally-defined `should_skip_discovery`
//! helper that clap never calls; `commands::service_unit::cli_tests` drives
//! the real `Cli` but only over argv, deliberately leaving the env path alone
//! because mutating the process env races a parallel test binary. Spawning the
//! binary sidesteps that: the child gets its own environment, so the real
//! clap env path is exercised with no shared-state hazard.
//!
//! What: runs the compiled `trusty-search` binary as `start --foreground`
//! against a deliberately unusable scratch `--data-dir`, so argument parsing
//! completes and the daemon then aborts immediately — no port is bound, no
//! daemon is spawned, and the live daemon on this machine is never touched.
//! That yields a clean two-valued signal: exit 2 means clap *rejected* the
//! env value, any other exit means clap accepted it.
//!
//! Test: `cargo test -p trusty-search --test no_auto_discover_env`

use std::process::Command;

/// Exit status clap uses for an argument-parsing failure.
const CLAP_USAGE_EXIT: i32 = 2;

/// Run `trusty-search start` with `TRUSTY_NO_AUTO_DISCOVER` set to `value`
/// (or removed, when `None`) and return `(exit_code, combined_output)`.
///
/// Why: the only way to drive clap's env path is to give clap a real
/// environment, which means a real child process.
/// What: points `--data-dir` at a regular file inside a fresh scratch
/// directory. Argument parsing runs to completion first; the daemon then fails
/// at `create --data-dir directory` before it opens a socket or writes
/// anything, so the probe is side-effect free and returns immediately.
/// `TRUSTY_DATA_DIR` is removed from the child env because it takes precedence
/// over the flag — an inherited value would silently redirect the probe at the
/// real data directory and let the daemon actually boot.
/// Test: used by every test in this file.
fn run_start(value: Option<&str>) -> (i32, String) {
    let scratch = tempfile::tempdir().expect("create scratch dir");
    let unusable = scratch.path().join("data-dir-is-a-file");
    std::fs::write(&unusable, b"").expect("create unusable data-dir sentinel");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trusty-search"));
    cmd.args(["start", "--foreground", "--port", "17999"])
        .arg("--data-dir")
        .arg(&unusable)
        .env_remove("TRUSTY_DATA_DIR")
        .env_remove("TRUSTY_NO_AUTO_DISCOVER");
    if let Some(v) = value {
        cmd.env("TRUSTY_NO_AUTO_DISCOVER", v);
    }

    let out = cmd.output().expect("spawn trusty-search");
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), combined)
}

/// Why (issue #4823): `TRUSTY_NO_AUTO_DISCOVER=1` is what the reporter's
/// `daemon.env` holds and what every trusty-search document tells operators to
/// write. Copying it into a generated plist made the daemon refuse to boot.
/// This test fails against pre-#4823 behaviour — clap answered
/// `invalid value '1' for '--no-auto-discover'` and exited 2.
/// What: asserts the daemon parses `=1` and proceeds past argument handling.
/// Test: this function.
#[test]
fn env_var_accepts_the_documented_numeric_spelling() {
    let (code, output) = run_start(Some("1"));
    assert_ne!(
        code, CLAP_USAGE_EXIT,
        "TRUSTY_NO_AUTO_DISCOVER=1 must not be an argument-parsing error \
         (issue #4823) — a launchd unit carrying it would throttle-loop a \
         daemon that never boots; got exit {code} and output:\n{output}"
    );
    assert!(
        !output.contains("--no-auto-discover"),
        "no diagnostic should mention --no-auto-discover when the value is \
         valid, got:\n{output}"
    );
}

/// Why: the plist on the reporter's machine was hand-edited to `=true` to work
/// around the rejection, so that spelling must keep working; the remaining
/// spellings are what operators actually type.
/// What: asserts every accepted truthy and falsey spelling parses, plus the
/// unset baseline.
/// Test: this function.
#[test]
fn env_var_accepts_every_documented_spelling() {
    for value in [
        None,
        Some("1"),
        Some("true"),
        Some("TRUE"),
        Some("yes"),
        Some("on"),
        Some("0"),
        Some("false"),
        Some("no"),
        Some("off"),
        Some(""),
    ] {
        let (code, output) = run_start(value);
        assert_ne!(
            code, CLAP_USAGE_EXIT,
            "TRUSTY_NO_AUTO_DISCOVER={value:?} must parse (issue #4823), got \
             exit {code} and output:\n{output}"
        );
    }
}

/// Why: loosening the parser must not degrade into "anything unknown is
/// false". A typo that silently re-enabled the scan would be the same
/// silent-capability-change defect #4823 is about, just harder to notice.
/// What: asserts an unrecognised spelling is still a hard parse failure that
/// names the offending argument.
/// Test: this function.
#[test]
fn env_var_rejects_an_unrecognised_spelling() {
    let (code, output) = run_start(Some("ture"));
    assert_eq!(
        code, CLAP_USAGE_EXIT,
        "an unrecognised TRUSTY_NO_AUTO_DISCOVER value must fail loudly rather \
         than default to false, got exit {code} and output:\n{output}"
    );
    assert!(
        output.contains("--no-auto-discover"),
        "the diagnostic must name the offending argument, got:\n{output}"
    );
}
