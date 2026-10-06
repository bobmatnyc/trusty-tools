//! Tests for [`super::ExternalCliCommand`] (#9311). Unix only: every child is
//! a `sh` script that reports what it received.

use std::error::Error as _;
use std::time::{Duration, Instant};

use super::*;

/// The value every test delivers. Distinctive, so a substring search for it
/// cannot match anything by accident.
const VALUE: &str = "s3cr3t-9311-stdin-only-VALUE";

/// Echoes stdin, then one argv entry per line, then the environment, each
/// section introduced by a marker line.
const REPORT_SCRIPT: &str = r#"cat; printf '\n%s\n' '--argv--'; for a in "$0" "$@"; do printf '%s\n' "$a"; done; printf '%s\n' '--env--'; env"#;

/// Writes its pid to `$1`, then execs a long sleep with stdin closed. Only a
/// kill ends it before the test would time out.
const CLOSE_STDIN_SCRIPT: &str = r#"printf '%s' "$$" > "$1"; exec sleep 30 0<&-"#;

fn secret() -> Secret<String> {
    Secret::new(VALUE.to_string())
}

/// Several pipe buffers' worth, so the write cannot finish into the kernel
/// buffer before the child closes its end.
fn big_secret() -> Secret<Vec<u8>> {
    Secret::new(VALUE.repeat(64 * 1024).into_bytes())
}

fn report_command() -> ExternalCliCommand {
    ExternalCliCommand::new("sh").args([
        "-c",
        REPORT_SCRIPT,
        "probe",
        "--flag",
        "op://vault/item/field",
    ])
}

/// The stdin-only contract, read from a run of [`report_command`].
fn assert_stdin_only(out: &ExternalCliOutput) {
    assert!(out.success, "child failed: code {:?}", out.code);
    let report = out.stdout.expose();
    let (stdin_seen, rest) = report
        .split_once("\n--argv--\n")
        .expect("report has an argv section");
    assert_eq!(stdin_seen, VALUE, "the child must read the value on stdin");
    let (argv, env) = rest
        .split_once("--env--\n")
        .expect("report has an env section");
    assert_eq!(
        argv, "probe\n--flag\nop://vault/item/field\n",
        "argv must carry exactly the caller's arguments"
    );
    assert!(
        !env.contains(VALUE),
        "the value must not reach the child's environment"
    );
    assert!(
        !format!("{out:?}").contains(VALUE),
        "Debug of the output must not render the value"
    );
}

/// Display, Debug, and every `source()` in the chain are free of `needle`.
fn assert_no_value(err: &ExternalCliError, needle: &str) {
    let mut texts = vec![err.to_string(), format!("{err:?}")];
    let mut source = err.source();
    while let Some(s) = source {
        texts.push(s.to_string());
        texts.push(format!("{s:?}"));
        source = s.source();
    }
    for text in texts {
        assert!(
            !text.contains(needle),
            "error text leaked the value: {text}"
        );
    }
}

/// A `StdinWrite` error, returned well before the child's 30 s sleep ends,
/// and the child's pid no longer exists — a zombie would still answer
/// `kill -0`, so this also proves the child was reaped.
fn assert_killed_and_reaped(
    result: Result<ExternalCliOutput, ExternalCliError>,
    started: Instant,
    pid_file: &Path,
) {
    let err = result.expect_err("a closed stdin fails the write");
    assert!(
        matches!(err, ExternalCliError::StdinWrite { .. }),
        "{err:?}"
    );
    assert_no_value(&err, VALUE);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the child must be killed, not waited out"
    );
    let pid = std::fs::read_to_string(pid_file).expect("child wrote its pid");
    let alive = std::process::Command::new("sh")
        .args(["-c", r#"kill -0 "$1""#, "probe", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .expect("run kill -0");
    assert!(
        !alive.success(),
        "pid {pid} still exists: the child was not reaped"
    );
}

fn close_stdin_command(pid_file: &Path) -> ExternalCliCommand {
    ExternalCliCommand::new("sh")
        .args(["-c", CLOSE_STDIN_SCRIPT, "probe"])
        .arg(pid_file)
}

#[test]
fn stdin_value_reaches_the_child_on_stdin_only() {
    let out = report_command()
        .output_with_stdin_blocking(&secret())
        .expect("run");
    assert_stdin_only(&out);
}

#[tokio::test]
async fn async_stdin_value_reaches_the_child_on_stdin_only() {
    let out = report_command()
        .output_with_stdin(&secret())
        .await
        .expect("run");
    assert_stdin_only(&out);
}

#[test]
fn nonzero_exit_error_withholds_stderr_that_echoes_the_value() {
    let out = ExternalCliCommand::new("sh")
        .args(["-c", "cat >&2; exit 7"])
        .output_with_stdin_blocking(&secret())
        .expect("run");
    // Not vacuous: the child really did echo the value to stderr.
    assert!(out.stderr.expose().contains(VALUE));
    assert!(!format!("{out:?}").contains(VALUE));
    let err = out.ok().expect_err("exit 7 is an error");
    assert!(
        matches!(err, ExternalCliError::NonZero { code: Some(7), .. }),
        "{err:?}"
    );
    assert_no_value(&err, VALUE);
}

#[test]
fn missing_binary_fails_closed_without_the_value() {
    for program in ["trusty-9311-no-such-cli", "/nonexistent-9311/bin/cli"] {
        let err = ExternalCliCommand::new(program)
            .arg("read")
            .output_with_stdin_blocking(&secret())
            .expect_err("no such binary");
        assert!(
            matches!(err, ExternalCliError::NotInstalled { .. }),
            "{program}: {err:?}"
        );
        assert_no_value(&err, VALUE);
    }
}

#[tokio::test]
async fn async_missing_binary_fails_closed() {
    let cmd = ExternalCliCommand::new("trusty-9311-no-such-cli");
    let err = cmd
        .output_with_stdin(&secret())
        .await
        .expect_err("no such binary");
    assert!(
        matches!(err, ExternalCliError::NotInstalled { .. }),
        "{err:?}"
    );
    assert_no_value(&err, VALUE);
    let err = cmd.output().await.expect_err("no such binary");
    assert!(
        matches!(err, ExternalCliError::NotInstalled { .. }),
        "{err:?}"
    );
}

#[test]
fn spawn_failure_is_typed_and_carries_no_value() {
    // An existing file without the execute bit: exec fails with EACCES, not
    // ENOENT, so this is `Spawn` and not `NotInstalled`.
    let not_executable = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let err = ExternalCliCommand::new(not_executable)
        .output_with_stdin_blocking(&secret())
        .expect_err("not executable");
    assert!(matches!(err, ExternalCliError::Spawn { .. }), "{err:?}");
    assert_no_value(&err, VALUE);
}

#[test]
fn stdin_write_failure_kills_and_reaps_the_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("pid");
    let started = Instant::now();
    let result = close_stdin_command(&pid_file).output_with_stdin_blocking(&big_secret());
    assert_killed_and_reaped(result, started, &pid_file);
}

#[tokio::test]
async fn async_stdin_write_failure_kills_and_reaps_the_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("pid");
    let started = Instant::now();
    let result = close_stdin_command(&pid_file)
        .output_with_stdin(&big_secret())
        .await;
    assert_killed_and_reaped(result, started, &pid_file);
}

#[test]
fn value_in_the_command_line_is_refused_before_spawn() {
    // The program does not exist: reaching spawn would yield `NotInstalled`.
    let base = || ExternalCliCommand::new("trusty-9311-no-such-cli");
    let cases = [
        ("argv", base().arg(format!("password={VALUE}"))),
        ("env overlay", base().env("PASSWORD", VALUE)),
    ];
    for (name, cmd) in cases {
        let err = cmd.output_with_stdin_blocking(&secret()).expect_err(name);
        assert!(
            matches!(err, ExternalCliError::ValueInCommandLine { .. }),
            "{name}: {err:?}"
        );
        assert_no_value(&err, VALUE);
    }
}

#[tokio::test]
async fn async_value_in_the_command_line_is_refused_before_spawn() {
    // A real program that leaves a marker file when it runs: the marker's
    // absence proves no child was spawned.
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let base = || {
        ExternalCliCommand::new("sh")
            .args(["-c", r#"touch "$1""#, "probe"])
            .arg(&marker)
    };
    let cases = [
        ("argv", base().arg(format!("password={VALUE}"))),
        ("env overlay", base().env("PASSWORD", VALUE)),
    ];
    for (name, cmd) in cases {
        let err = cmd.output_with_stdin(&secret()).await.expect_err(name);
        assert!(
            matches!(err, ExternalCliError::ValueInCommandLine { .. }),
            "{name}: {err:?}"
        );
        assert_no_value(&err, VALUE);
        assert!(!marker.exists(), "{name}: a child was spawned");
    }
}

#[tokio::test]
async fn async_dropped_future_kills_the_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("pid");
    let cmd = ExternalCliCommand::new("sh")
        .args(["-c", r#"printf '%s' "$$" > "$1"; exec sleep 30"#, "probe"])
        .arg(&pid_file);
    let secret = secret();
    // Boxed so `drop(run)` drops the future itself, not a pinned reference.
    let mut run = Box::pin(cmd.output_with_stdin(&secret));
    // A caller's timeout drops the future; drop it only once the child has
    // written its pid, so the kill cannot race the child's start.
    let started = Instant::now();
    let pid = loop {
        let timed_out = tokio::time::timeout(Duration::from_millis(500), &mut run).await;
        assert!(timed_out.is_err(), "the child must still be running");
        let pid = std::fs::read_to_string(&pid_file).unwrap_or_default();
        if !pid.is_empty() {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the child never wrote its pid"
        );
    };
    drop(run);
    // tokio reaps a killed orphan asynchronously, and a zombie still answers
    // `kill -0`, so poll until the pid is gone.
    let deadline = Instant::now() + Duration::from_secs(5);
    let gone = loop {
        let alive = tokio::process::Command::new("sh")
            .args(["-c", r#"kill -0 "$1""#, "probe", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .await
            .expect("run kill -0");
        if !alive.success() || Instant::now() >= deadline {
            break !alive.success();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        gone,
        "pid {pid} still exists 5 s after the future was dropped"
    );
}

#[test]
fn output_without_stdin_sees_eof_and_wraps_stdout() {
    // `cat` on a null stdin ends at once. The value rides the env overlay
    // only to stand in for `op read` printing a secret on stdout; not argv,
    // which Debug of the output renders.
    let out = ExternalCliCommand::new("sh")
        .args(["-c", r#"cat; printf '%s' "$VALUE_9311""#])
        .env("VALUE_9311", VALUE)
        .output_blocking()
        .expect("run");
    assert!(out.success);
    assert_eq!(out.stdout.expose(), VALUE);
    assert!(!format!("{out:?}").contains(VALUE));
}

#[test]
fn command_debug_renders_env_keys_not_values() {
    let cmd = ExternalCliCommand::new("op")
        .arg("read")
        .env("OP_SESSION_x", "session-token-9311")
        .env_remove("OP_ACCOUNT");
    let rendered = format!("{cmd:?}");
    assert!(rendered.contains("OP_SESSION_x=<set>"), "{rendered}");
    assert!(rendered.contains("OP_ACCOUNT=<removed>"), "{rendered}");
    assert!(!rendered.contains("session-token-9311"), "{rendered}");
}
