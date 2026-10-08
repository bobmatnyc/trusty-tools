//! Tests for [`super::CliCommand`] (#7519): stdin-only delivery, refusals
//! before spawn, withheld stderr, verdicts, a missing CLI, the timeout, and
//! the output cap.
//!
//! Every child is `/bin/sh <script>`, the script written to a temp dir and
//! named by absolute path. No test reads or changes `PATH` or any other
//! process-global environment. Running the script through `/bin/sh` rather
//! than exec'ing it avoids `ETXTBSY` when another test thread forks while a
//! script is still open for writing.
//! Test: itself.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::classify::classify;
use super::{CliCommand, CliSpec, Verdict};
use crate::api::{SecretValue, SecretsError};

const SH: &str = "/bin/sh";
const CANARY: &str = "sk-canary-7519-0123456789abcdef";
const TOKEN: &str = "ops-token-canary-7519-fedcba9876543210";

const SPEC: CliSpec = CliSpec::new("testcli", "testcli", Duration::from_secs(10))
    .with_hints(
        "install testcli from example.invalid",
        "run `testcli signin` first",
    )
    .with_markers(
        &["item not found", "isn't an item"],
        &["not currently signed in", "session expired"],
    );

/// Logs argv, env and stdin to `@LOG@/*.log`, then prints a fixed stdout.
const RECORDER: &str = "printf '%s\\n' \"$@\" > '@LOG@/argv.log'\n\
                        env > '@LOG@/env.log'\n\
                        cat > '@LOG@/stdin.log'\n\
                        printf 'printed-value'\n";

/// A shell script in its own temp dir; `@LOG@` in the body is that dir.
struct Shim {
    dir: TempDir,
    script: PathBuf,
}

impl Shim {
    fn new(body: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("shim.sh");
        let body = body.replace("@LOG@", &dir.path().display().to_string());
        std::fs::write(&script, body).unwrap();
        Self { dir, script }
    }

    fn command(&self) -> CliCommand {
        CliCommand::new(SPEC).program(SH).arg(&self.script)
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }

    fn logged_nothing(&self) -> bool {
        ["argv.log", "env.log", "stdin.log"]
            .iter()
            .all(|name| !self.dir.path().join(name).exists())
    }

    /// The pid a script wrote to `@LOG@/pid`.
    fn logged_pid(&self) -> libc::pid_t {
        let pid: libc::pid_t = self.log("pid").trim().parse().unwrap();
        assert!(pid > 1);
        pid
    }
}

/// Whether `pid` is gone within ~2 s. A killed grandchild is reparented and
/// reaped, which takes a moment.
fn gone_soon(pid: libc::pid_t) -> bool {
    (0..200).any(|_| {
        // SAFETY: signal 0 sends nothing; `kill` only checks that `pid` exists.
        let rc = unsafe { libc::kill(pid, 0) };
        let gone = rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if !gone {
            std::thread::sleep(Duration::from_millis(10));
        }
        gone
    })
}

/// Why: DOC-74 §8.2, #7519 A1 and A10 — the value reaches the child on
/// stdin only, and a token only through its environment. The stdin and
/// argv logs holding what they should is the positive control.
/// Test: itself.
#[test]
fn runner_value_reaches_the_child_on_stdin_only() {
    let shim = Shim::new(RECORDER);
    let run = shim
        .command()
        .args(["--vault", "trusty/acme/web"])
        .env("TESTCLI_TOKEN", TOKEN)
        .run_with_stdin(&SecretValue::new(CANARY))
        .unwrap();
    assert_eq!(run.verdict, Verdict::Ok);
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stdout.expose(), "printed-value");
    assert_eq!(shim.log("stdin.log"), CANARY);

    let (argv, env) = (shim.log("argv.log"), shim.log("env.log"));
    assert!(argv.contains("trusty/acme/web"), "{argv}");
    assert!(!argv.contains(CANARY) && !env.contains(CANARY));
    assert!(env.contains(&format!("TESTCLI_TOKEN={TOKEN}")));
    assert!(!argv.contains(TOKEN), "{argv}");
    assert!(!format!("{run:?}").contains("printed-value"));
}

/// Why: #7519 A1 — a value in argv or the overlay is refused before the
/// spawn, so the shim never runs and logs nothing.
/// Test: itself.
#[test]
fn runner_refuses_the_value_in_argv_before_spawn() {
    let shim = Shim::new(RECORDER);
    let value = SecretValue::new(CANARY);
    let in_argv = shim.command().arg(format!("--title={CANARY}"));
    let in_env = shim.command().env("TESTCLI_EXTRA", CANARY);
    for command in [in_argv, in_env] {
        let err = command.run_with_stdin(&value).unwrap_err();
        assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains(CANARY), "{shown}");
    }
    assert!(shim.logged_nothing());
}

/// Why: #7519 P3 — Keeper's stdin carries the value base64-encoded, so the
/// raw value is not a substring of stdin; a value `hide` names is still
/// refused in argv and the overlay before the spawn. Red without the
/// `hidden_leaks` check in `refuse_leaks`.
/// Test: itself.
#[test]
fn runner_refuses_a_hidden_secret_in_argv_before_spawn() {
    let shim = Shim::new(RECORDER);
    let hidden = SecretValue::new(CANARY);
    let encoded = SecretValue::new("record-update password=$BASE64:c2stY2FuYXJ5\n");
    let in_argv = shim.command().hide(&hidden).arg(format!("--x={CANARY}"));
    let in_env = shim.command().hide(&hidden).env("TESTCLI_EXTRA", CANARY);
    for command in [in_argv, in_env] {
        let err = command.run_with_stdin(&encoded).unwrap_err();
        assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
        assert!(!format!("{err} {err:?}").contains(CANARY));
    }
    assert!(shim.logged_nothing());
    // The same command without the secret in argv runs.
    let run = shim
        .command()
        .hide(&hidden)
        .arg("--x=y")
        .run_with_stdin(&encoded)
        .unwrap();
    assert_eq!(run.verdict, Verdict::Ok);
}

/// Why: #7519 A10 — an overlay token in argv would be readable through
/// `ps`; it is refused before the spawn on both runners.
/// Test: itself.
#[test]
fn runner_refuses_a_token_in_argv_before_spawn() {
    let shim = Shim::new(RECORDER);
    let command = shim
        .command()
        .env("TESTCLI_TOKEN", TOKEN)
        .arg(format!("--token={TOKEN}"));
    let errors = [
        command.run().unwrap_err(),
        command
            .run_with_stdin(&SecretValue::new(CANARY))
            .unwrap_err(),
    ];
    for err in errors {
        assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains(TOKEN) && !shown.contains(CANARY), "{shown}");
    }
    assert!(shim.logged_nothing());
}

/// Why: #7519 A2/A3 — a CLI that echoes its stdin to stderr and fails must
/// leak nothing, and a value-bearing run is never read as a miss even when
/// the echo carries a missing or locked marker.
/// Test: itself.
#[test]
fn runner_echoed_stdin_never_reaches_an_error() {
    let shim = Shim::new("cat >&2\nexit 1\n");
    let value = format!("{CANARY} item not found; not currently signed in");
    let run = shim
        .command()
        .run_with_stdin(&SecretValue::new(value))
        .unwrap();
    assert_eq!(run.verdict, Verdict::Other);
    assert_eq!(run.code, Some(1));
    assert!(!format!("{run:?}").contains(CANARY));
    let err = run.into_value().unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(CANARY), "{shown}");
}

/// Why: #7519 A3 — each marker maps to its verdict whatever its case, a
/// locked marker beats a missing one, unknown stderr is `Other`, and
/// `into_value` keeps a miss `Ok(None)` and every failure an `Err`.
/// Test: itself.
#[test]
fn runner_stderr_markers_map_to_verdicts() {
    let shim = Shim::new("printf '%s' \"$1\" >&2\nexit \"$2\"\n");
    let cases = [
        ("ERROR: Item Not Found in vault", "1", Verdict::Missing),
        ("[ERROR] \"x\" isn't an item", "1", Verdict::Missing),
        ("You are NOT CURRENTLY SIGNED IN", "1", Verdict::Locked),
        ("item not found; session expired", "1", Verdict::Locked),
        ("connection reset by peer", "1", Verdict::Other),
        ("a warning on success", "0", Verdict::Ok),
    ];
    for (stderr, code, verdict) in cases {
        let run = shim.command().args([stderr, code]).run().unwrap();
        assert_eq!(run.verdict, verdict, "{stderr}");
        assert!(!format!("{run:?}").contains(stderr), "{stderr}");
        match (verdict, run.into_value()) {
            (Verdict::Ok, Ok(Some(value))) => assert_eq!(value.expose(), ""),
            (Verdict::Missing, Ok(None)) => {}
            (Verdict::Locked, Err(err @ SecretsError::BackendLocked { .. })) => {
                assert!(err.to_string().contains(SPEC.locked_hint), "{err}");
                assert!(!err.to_string().contains(stderr), "{err}");
            }
            (Verdict::Other, Err(err @ SecretsError::Backend { .. })) => {
                assert!(!err.to_string().contains(stderr), "{err}");
            }
            (verdict, other) => panic!("{stderr}: {verdict:?} gave {other:?}"),
        }
    }
}

/// Why: the classification rule on its own — case-insensitive markers,
/// locked first, stdin runs never classified, no markers means `Other`.
/// Test: itself.
#[test]
fn classify_marker_table() {
    let bare = CliSpec::new("bare", "bare", Duration::ZERO);
    let cases: [(bool, &[u8], bool, &CliSpec, Verdict); 8] = [
        (true, b"item not found", false, &SPEC, Verdict::Ok),
        (true, b"", true, &SPEC, Verdict::Ok),
        (false, b"ITEM NOT FOUND", false, &SPEC, Verdict::Missing),
        (false, b"Session Expired", false, &SPEC, Verdict::Locked),
        (
            false,
            b"item not found, session expired",
            false,
            &SPEC,
            Verdict::Locked,
        ),
        (false, b"item not found", true, &SPEC, Verdict::Other),
        (false, b"", false, &SPEC, Verdict::Other),
        (false, b"item not found", false, &bare, Verdict::Other),
    ];
    for (success, stderr, had_stdin, spec, expected) in cases {
        assert_eq!(
            classify(success, stderr, had_stdin, spec),
            expected,
            "{}",
            String::from_utf8_lossy(stderr)
        );
    }
}

/// Why: #7519 A4 — a missing CLI is a typed error naming the program and
/// the fix, on both runners, and never a generic failure.
/// Test: itself.
#[test]
fn runner_missing_program_is_cli_not_installed() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("no-such-cli");
    let command = CliCommand::new(SPEC).program(&missing);
    let errors = [
        command.run().unwrap_err(),
        command
            .run_with_stdin(&SecretValue::new(CANARY))
            .unwrap_err(),
    ];
    for err in errors {
        let shown = format!("{err} {err:?}");
        assert!(shown.contains(SPEC.install_hint), "{shown}");
        assert!(!shown.contains(CANARY), "{shown}");
        match err {
            SecretsError::CliNotInstalled { program, hint } => {
                assert_eq!(program, missing.to_string_lossy());
                assert_eq!(hint, SPEC.install_hint);
            }
            other => panic!("expected CliNotInstalled: {other:?}"),
        }
    }
}

/// Why: #7519 — a hung CLI must not hold a request, and killing the child
/// alone would leave its grandchildren running. The timeout kills the whole
/// process group and returns promptly.
/// Test: itself.
#[test]
fn runner_timeout_kills_the_process_group() {
    let shim = Shim::new("sleep 60 &\necho $! > '@LOG@/pid'\nwait\n");
    let started = Instant::now();
    let err = shim
        .command()
        .timeout(Duration::from_millis(200))
        .run()
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    assert!(
        matches!(&err, SecretsError::Backend { reason, .. } if reason.contains("timeout")),
        "{err:?}"
    );

    let pid: libc::pid_t = shim.log("pid").trim().parse().unwrap();
    assert!(pid > 1);
    // The killed grandchild is reparented and reaped; wait for that briefly.
    let gone = (0..200).any(|_| {
        // SAFETY: signal 0 sends nothing; `kill` only checks that `pid` exists.
        let rc = unsafe { libc::kill(pid, 0) };
        let gone = rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if !gone {
            std::thread::sleep(Duration::from_millis(10));
        }
        gone
    });
    assert!(gone, "grandchild {pid} outlived the timeout");
}

/// Why: #7524 P2-M1 — once a server request's deadline has passed, no CLI
/// call starts, so nothing can land after the server has answered.
/// Test: itself.
#[test]
fn runner_refuses_to_spawn_after_the_request_deadline() {
    let shim = Shim::new(RECORDER);
    let err = crate::store::deadline::within(Instant::now(), || {
        shim.command().run_with_stdin(&SecretValue::new(CANARY))
    })
    .unwrap_err();
    assert!(
        matches!(&err, SecretsError::DeadlineExceeded { backend, .. } if backend == "testcli"),
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains(CANARY));
    assert!(shim.logged_nothing(), "a CLI started after the deadline");
}

/// Why: #7524 P2-M1 — a call the request deadline cuts short is killed at
/// that deadline, not at its own longer timeout, with its whole process
/// group, and the error says so.
/// Test: itself.
#[test]
fn runner_kills_the_cli_at_the_request_deadline() {
    let shim = Shim::new("sleep 60 &\necho $! > '@LOG@/pid'\nwait\n");
    let started = Instant::now();
    let err = crate::store::deadline::within(started + Duration::from_millis(300), || {
        shim.command().timeout(Duration::from_secs(30)).run()
    })
    .unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    assert!(
        matches!(err, SecretsError::DeadlineExceeded { .. }),
        "{err:?}"
    );
    assert!(
        gone_soon(shim.logged_pid()),
        "the grandchild outlived the deadline"
    );

    // Without a request deadline the run's own timeout still applies.
    let err = shim
        .command()
        .timeout(Duration::from_millis(200))
        .run()
        .unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
}

/// Why: #7519 A10 — `Debug` reaches logs; an overlay may carry a token.
/// Test: itself.
#[test]
fn runner_env_overlay_debug_shows_keys_only() {
    let command = CliCommand::new(SPEC).env("OP_SERVICE_ACCOUNT_TOKEN", TOKEN);
    let debug = format!("{command:?}");
    assert!(debug.contains("OP_SERVICE_ACCOUNT_TOKEN"), "{debug}");
    assert!(!debug.contains(TOKEN), "{debug}");
}

/// Why: #7519 — each stream is capped at 1 MiB. A stdout over the cap
/// would be a truncated value, so it is an error; a stderr over the cap is
/// cut, and the run still finishes.
/// Test: itself.
#[test]
fn runner_stdout_over_the_cap_is_an_error() {
    let flood = "dd if=/dev/zero bs=1024 count=1100 2>/dev/null";
    let err = Shim::new(flood).command().run().unwrap_err();
    assert!(
        matches!(&err, SecretsError::Backend { reason, .. } if reason.contains("1 MiB")),
        "{err:?}"
    );

    let shim = Shim::new(&format!("{{ {flood}; }} >&2\nprintf ok\n"));
    let run = shim.command().run().unwrap();
    assert_eq!(run.verdict, Verdict::Ok);
    assert_eq!(run.stdout.expose(), "ok");
}

/// Why: #7519 — a child that exits without reading all of stdin breaks the
/// pipe; that is the child's answer, classified by its exit status, not a
/// write failure.
/// Test: itself.
#[test]
fn runner_early_exit_on_stdin_is_classified() {
    let shim = Shim::new("exit 3\n");
    let value = SecretValue::new("v".repeat(4 * 1024 * 1024));
    let run = shim.command().run_with_stdin(&value).unwrap();
    assert_eq!(run.verdict, Verdict::Other);
    assert_eq!(run.code, Some(3));
}

/// Why: #7519 — a child that closes stdin before reading the whole value
/// and exits 0 must not read as stored, and its process group is killed so
/// no grandchild outlives the run. The grandchild holds no pipe, so only
/// the group kill can reach it.
/// Test: itself.
#[test]
fn runner_stdin_closed_early_is_never_ok_and_kills_the_group() {
    let shim = Shim::new(
        "sleep 60 </dev/null >/dev/null 2>&1 &\n\
         echo $! > '@LOG@/pid'\n\
         exec 0<&-\n\
         exit 0\n",
    );
    let value = SecretValue::new("v".repeat(4 * 1024 * 1024));
    let started = Instant::now();
    let run = shim.command().run_with_stdin(&value).unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    assert_ne!(
        run.verdict,
        Verdict::Ok,
        "a partly read value read as stored"
    );
    assert!(run.into_value().is_err());
    let pid = shim.logged_pid();
    assert!(
        gone_soon(pid),
        "grandchild {pid} outlived a short stdin write"
    );
}

/// Why: #7519 — once the leader exits and is reaped, a grandchild still
/// holding stdout would keep the run waiting; the runner kills the group at
/// the deadline, so the call returns and the grandchild is gone.
/// Test: itself.
#[test]
fn runner_grandchild_holding_a_pipe_is_killed_after_the_leader_exits() {
    let shim = Shim::new("sleep 60 &\necho $! > '@LOG@/pid'\nexit 0\n");
    let started = Instant::now();
    let err = shim
        .command()
        .timeout(Duration::from_millis(300))
        .run()
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    assert!(
        matches!(&err, SecretsError::Backend { reason, .. } if reason.contains("timeout")),
        "{err:?}"
    );
    let pid = shim.logged_pid();
    assert!(
        gone_soon(pid),
        "grandchild {pid} outlived its reaped leader"
    );
}

/// Why: #7519 — stderr cut at the 1 MiB cap is not read for meaning: a
/// failed run whose stderr overflowed is `Other`, never a miss or locked,
/// even with a marker inside the kept bytes.
/// Test: itself.
#[test]
fn runner_stderr_over_the_cap_on_failure_is_other() {
    let shim = Shim::new(
        "printf '%s\\n' \"$1\" >&2\n\
         { dd if=/dev/zero bs=1024 count=1100 2>/dev/null; } >&2\n\
         exit 1\n",
    );
    for marker in ["item not found", "not currently signed in"] {
        let run = shim.command().arg(marker).run().unwrap();
        assert_eq!(run.verdict, Verdict::Other, "{marker}");
        assert_eq!(run.code, Some(1), "{marker}");
    }
}
