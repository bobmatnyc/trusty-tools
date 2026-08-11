//! End-to-end regression tests for issue #4827: `daemon.env` must reach `clap`.
//!
//! Why: `load_daemon_env()` ran AFTER `Cli::try_parse()`, so every variable in
//! the file that backs a `#[arg(long, env = "…")]` was read too late to change
//! the already-computed value and was silently ignored. The reporter's
//! `daemon.env` held `TRUSTY_NO_AUTO_DISCOVER=1` — added for #767 — and
//! auto-discovery ran anyway. The file presented itself as a working
//! configuration mechanism and was not one for that whole class of setting.
//!
//! No in-process test can reach this. Ordering lives in `main()`, and clap
//! resolves an env-sourced value by reading the REAL process environment during
//! the parse — so the only way to observe which came first is to spawn the
//! binary, exactly as `no_auto_discover_env.rs` does for #4823.
//!
//! What: points `TRUSTY_DATA_DIR` at a scratch directory holding a `daemon.env`
//! and a `daemon.lock` naming PID 1. Argument parsing runs first; the daemon
//! then sees a live lockfile and exits 1 with "another daemon is already
//! running" before it binds a port, opens an index, or touches this machine's
//! real daemon. That yields a clean two-valued signal: exit 2 means clap SAW
//! the file's value and rejected it, exit 1 means clap never saw it.
//!
//! Test: `cargo test -p trusty-search --test daemon_env_precedence`

use std::path::Path;
use std::process::Command;

/// Exit status clap uses for an argument-parsing failure.
const CLAP_USAGE_EXIT: i32 = 2;

/// Build the `trusty-search start --foreground` command every test here spawns.
///
/// Why: the daemon enforces a hard 16 GB RAM floor
/// (`commands/start/daemon.rs`) that runs AFTER `load_daemon_env()` but BEFORE
/// the lockfile abort these tests assert on. GitHub-hosted runners in this
/// class report just under the floor — 15989 MB and 15993 MB observed on two
/// independent runners — so without the documented `TRUSTY_SKIP_RAM_CHECK`
/// bypass the process exits at the RAM check and the behaviour under test
/// never runs. Centralised here so a new test cannot reintroduce the gap by
/// spawning the binary without the flag.
/// What: points `TRUSTY_DATA_DIR` at `data_dir` and pins `port`; the caller
/// adds whatever per-test environment the case needs.
/// Test: used by every test in this file.
fn daemon_command(data_dir: &Path, port: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trusty-search"));
    cmd.args(["start", "--foreground", "--port", port])
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("TRUSTY_SKIP_RAM_CHECK", "1");
    cmd
}

/// Run the daemon and return `(exit_code, stdout ++ stderr)`.
fn run_to_completion(cmd: &mut Command) -> (i32, String) {
    let out = cmd.output().expect("spawn trusty-search");
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), combined)
}

/// Run `trusty-search start --foreground` against a scratch data dir seeded
/// with `daemon_env` and return `(exit_code, combined_output)`.
///
/// Why: the child gets its own environment, so the real clap env path is
/// exercised with no shared-state hazard against a parallel test binary.
/// What: writes `daemon.env` and a `daemon.lock` holding PID 1 (init — always
/// alive, so `pid_alive` reports true) into a fresh tempdir, then starts the
/// daemon there. `TRUSTY_NO_AUTO_DISCOVER` is removed from the child env so the
/// file is the only source of the value under test.
/// Test: used by every test in this file.
fn run_start(daemon_env: &str) -> (i32, String) {
    let scratch = tempfile::tempdir().expect("create scratch dir");
    std::fs::write(scratch.path().join("daemon.env"), daemon_env).expect("write daemon.env");
    // PID 1 is always alive, so the lockfile fast-path aborts startup right
    // after argument handling — no port bound, no index opened.
    std::fs::write(scratch.path().join("daemon.lock"), b"1").expect("write daemon.lock");

    run_to_completion(daemon_command(scratch.path(), "17998").env_remove("TRUSTY_NO_AUTO_DISCOVER"))
}

/// Core regression for #4827: a value in `daemon.env` must reach clap.
///
/// Why: an unrecognised `TRUSTY_NO_AUTO_DISCOVER` spelling is a hard parse
/// failure (`no_auto_discover_env::env_var_rejects_an_unrecognised_spelling`
/// pins that). So if clap sees the file's value at all, the process exits 2 and
/// names the argument. Against the pre-fix commit the file was sourced after
/// the parse, clap never saw it, and startup proceeded to the lockfile abort —
/// which is precisely the silent no-op the issue reports.
/// What: seeds `TRUSTY_NO_AUTO_DISCOVER=ture` and asserts the parse rejects it.
/// Test: this function.
#[test]
fn daemon_env_value_reaches_clap() {
    let (code, output) = run_start("TRUSTY_NO_AUTO_DISCOVER=ture\n");
    assert_eq!(
        code, CLAP_USAGE_EXIT,
        "#4827: a daemon.env value backing a clap arg must be visible to the \
         parse; got exit {code} and output:\n{output}"
    );
    assert!(
        output.contains("--no-auto-discover"),
        "the diagnostic must name the argument the file set, got:\n{output}"
    );
}

/// The documented spelling must still boot.
///
/// Why: making the file visible is worthless if the value operators actually
/// write is then rejected — that would trade a silent no-op for a daemon that
/// refuses to start, which is the #4823 failure.
/// What: seeds `TRUSTY_NO_AUTO_DISCOVER=1` and asserts the run gets past
/// argument handling to the lockfile abort.
/// Test: this function.
#[test]
fn daemon_env_documented_spelling_still_boots() {
    let (code, output) = run_start("TRUSTY_NO_AUTO_DISCOVER=1\n");
    assert_ne!(
        code, CLAP_USAGE_EXIT,
        "TRUSTY_NO_AUTO_DISCOVER=1 in daemon.env must parse; got exit {code} \
         and output:\n{output}"
    );
}

/// A malformed line must not take the rest of the file down with it.
///
/// Why (#4827 fail-open): a line with no `=` was dropped in silence, so the
/// settings that DID parse looked identical to a file that parsed cleanly. The
/// parser now reports the bad line at `warn` while still applying the good
/// ones — the surrounding settings must keep working.
/// What: seeds a malformed line ahead of a valid `TRUSTY_NO_AUTO_DISCOVER=ture`
/// and asserts clap still sees the valid one.
/// Test: this function.
#[test]
fn a_malformed_line_does_not_discard_the_rest_of_the_file() {
    let (code, output) = run_start("TRUSTY_MAX_CHUNKS 100000\nTRUSTY_NO_AUTO_DISCOVER=ture\n");
    assert_eq!(
        code, CLAP_USAGE_EXIT,
        "a malformed line must not stop later keys from being applied; got \
         exit {code} and output:\n{output}"
    );
}

/// The process environment must still outrank the file.
///
/// Why: `daemon.env` is a fallback for launchd restarts that carry no shell
/// env. If the file could beat an exported value the precedence documented on
/// `load_daemon_env` ("env > file > default") would be inverted, and an
/// operator's one-off export would stop working.
/// What: exports a valid `TRUSTY_NO_AUTO_DISCOVER=1` while the file holds the
/// invalid `ture`; the export must win, so the parse succeeds.
/// Test: this function.
#[test]
fn process_env_still_beats_daemon_env() {
    let scratch = tempfile::tempdir().expect("create scratch dir");
    std::fs::write(
        scratch.path().join("daemon.env"),
        "TRUSTY_NO_AUTO_DISCOVER=ture\n",
    )
    .expect("write daemon.env");
    std::fs::write(scratch.path().join("daemon.lock"), b"1").expect("write daemon.lock");

    let (code, output) = run_to_completion(
        daemon_command(scratch.path(), "17997").env("TRUSTY_NO_AUTO_DISCOVER", "1"),
    );
    assert_ne!(
        code, CLAP_USAGE_EXIT,
        "an exported value must outrank daemon.env; got exit {code} and \
         output:\n{output}"
    );
}

/// `TRUSTY_DATA_DIR` in `daemon.env` must never be applied before the parse.
///
/// Why: `daemon.env`'s own location is resolved from `TRUSTY_DATA_DIR`. If the
/// early pass applied it, a production `daemon.env` could redirect a
/// `--data-dir /tmp/isolated` run back at production data — a far worse defect
/// than the one being fixed. It stays on the post-parse pass, where the flag
/// still wins.
/// What: seeds a `TRUSTY_DATA_DIR` pointing somewhere else and asserts the
/// daemon still aborts on the scratch dir's lockfile, i.e. the redirect did not
/// happen.
/// Test: this function.
#[test]
fn daemon_env_cannot_redirect_the_data_dir() {
    let scratch = tempfile::tempdir().expect("create scratch dir");
    let decoy = tempfile::tempdir().expect("create decoy dir");
    std::fs::write(
        scratch.path().join("daemon.env"),
        format!("TRUSTY_DATA_DIR={}\n", decoy.path().display()),
    )
    .expect("write daemon.env");
    std::fs::write(scratch.path().join("daemon.lock"), b"1").expect("write daemon.lock");

    let (_code, output) = run_to_completion(&mut daemon_command(scratch.path(), "17996"));
    assert!(
        output.contains("already running"),
        "the scratch dir's lockfile must still be the one consulted — a \
         daemon.env TRUSTY_DATA_DIR must not repoint the run; got:\n{output}"
    );
    assert!(
        !decoy.path().join("daemon.lock").exists(),
        "the decoy data dir must never have been used"
    );
}
