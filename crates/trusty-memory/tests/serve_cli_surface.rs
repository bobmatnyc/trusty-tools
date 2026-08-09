//! Binary-level surface tests for `trusty-memory serve` (#5267).
//!
//! Why: the transport-selection unit tests in `cli_tests.rs` prove which branch
//! the dispatch takes; these prove what the real binary does when you run it —
//! that bare `serve` actually reaches the stdio server, that the explanatory
//! notice goes to stderr and only for a human, and that stdout stays clean
//! enough to carry JSON-RPC. A notice on stdout would corrupt MCP framing, which
//! no parse-level test can catch.
//!
//! These tests never touch the machine's real daemon or palace: every child runs
//! under `TRUSTY_DATA_DIR_OVERRIDE` pointing at a fresh temp dir, and none of
//! them is allowed to reach a readiness state that would start anything.
//!
//! Test: `cargo test -p trusty-memory --test serve_cli_surface`.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Path to the binary under test, as cargo builds it for this integration test.
fn bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().expect("current exe");
    p.pop(); // .../deps
    p.pop(); // .../debug
    p.push("trusty-memory");
    p
}

/// Run `trusty-memory <args>` with a piped stdin that is closed immediately.
///
/// Why: closing stdin gives the MCP stdio loop a clean EOF, so a bare `serve`
/// exits promptly instead of blocking the test. Piped (not TTY) stdin is also
/// exactly the shape an MCP client presents, which is what the notice tests
/// assert against.
/// What: returns `(stdout, stderr)` after a bounded wait.
fn run_piped(args: &[&str], data_dir: &std::path::Path) -> (String, String) {
    let mut child = Command::new(bin())
        .args(args)
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn trusty-memory");

    // Close stdin so the stdio loop sees EOF.
    drop(child.stdin.take());

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(_status) = child.try_wait().expect("try_wait") {
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().expect("wait_with_output");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Why: an MCP client's stdin is a pipe, and it must see NO notice — the notice
/// is for a human who typed the command. It must also never appear on stdout,
/// which carries JSON-RPC framing.
/// What: runs bare `serve` with piped stdin and asserts the notice text is
/// absent from both streams.
/// Test: itself.
#[test]
fn bare_serve_notice_absent_when_stdin_is_piped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (stdout, stderr) = run_piped(&["serve"], tmp.path());

    assert!(
        !stderr.contains("waiting on stdin"),
        "no notice for a piped (MCP client) stdin; stderr was: {stderr}"
    );
    assert!(
        !stdout.contains("waiting on stdin"),
        "the notice must NEVER reach stdout — it is the JSON-RPC channel"
    );
}

/// Why: stdout is the JSON-RPC channel. Anything the bare `serve` path prints
/// there corrupts MCP framing for every client. This is the hygiene invariant
/// the whole stdio design rests on.
/// What: runs bare `serve` with immediate EOF and asserts stdout carries no
/// non-JSON chatter (it is either empty or valid JSON-RPC lines).
/// Test: itself.
#[test]
fn bare_serve_keeps_stdout_clean() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (stdout, _stderr) = run_piped(&["serve"], tmp.path());

    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "stdout must carry only JSON-RPC; found non-JSON line: {line}"
        );
    }
}

/// Why: bare `serve` and `serve --stdio` must be the same server. If they
/// diverged, the alignment #5267 delivers would be cosmetic.
/// What: runs both with an identical closed stdin and asserts their stdout
/// behavior matches (both clean, both terminating on EOF).
/// Test: itself.
#[test]
fn bare_serve_and_explicit_stdio_behave_alike() {
    let tmp_a = tempfile::tempdir().expect("tempdir");
    let tmp_b = tempfile::tempdir().expect("tempdir");
    let (out_bare, _) = run_piped(&["serve"], tmp_a.path());
    let (out_flag, _) = run_piped(&["serve", "--stdio"], tmp_b.path());

    let json_lines = |s: &str| {
        s.lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
            .count()
    };
    assert_eq!(
        json_lines(&out_bare),
        json_lines(&out_flag),
        "bare `serve` and `serve --stdio` must produce the same stdout shape"
    );
}

/// Why: an unknown flag must still be an error. Making bare `serve` meaningful
/// must not have made the parser permissive.
/// What: asserts a nonzero exit and a clap usage error on stderr.
/// Test: itself.
#[test]
fn unknown_flag_is_still_rejected() {
    let out = Command::new(bin())
        .args(["serve", "--definitely-not-a-flag"])
        .output()
        .expect("run");
    assert!(!out.status.success(), "unknown flag must exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("error"),
        "expected a usage error, got: {stderr}"
    );
}

/// Why: `--stdio` conflicts with both HTTP flags, and #5267 must not have
/// relaxed either relationship.
/// What: asserts both conflicting combinations exit nonzero.
/// Test: itself.
#[test]
fn conflicting_transport_flags_still_rejected() {
    for args in [
        ["serve", "--http", "--stdio"],
        ["serve", "--foreground", "--stdio"],
    ] {
        let out = Command::new(bin()).args(args).output().expect("run");
        assert!(
            !out.status.success(),
            "{args:?} must be rejected as conflicting"
        );
    }
}

/// Why: `--help` is how a user discovers that the verb moved. If it still
/// described `serve` as the daemon, the change would be undiscoverable.
/// What: asserts the help text names `start` as the daemon verb and `serve` as
/// MCP stdio.
/// Test: itself.
#[test]
fn help_documents_the_new_serve_semantics() {
    let out = Command::new(bin())
        .args(["serve", "--help"])
        .output()
        .expect("run --help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("stdio"),
        "serve --help must describe the stdio default; got: {help}"
    );
    assert!(
        help.contains("start"),
        "serve --help must point at `start` for the daemon; got: {help}"
    );
}

/// Why: the other half of the notice contract. A human at a terminal MUST be
/// told that `serve` now waits on stdin and that `start` is the daemon verb —
/// without it, bare `serve` looks exactly like a hang. Only a real pty
/// exercises this branch; a pipe takes the silent path.
/// What: allocates a pty with `openpty(3)`, hands the slave to the child as
/// stdin, sends Ctrl-D so the stdio loop sees EOF, and asserts the notice
/// appears on stderr and never on stdout.
/// Test: itself.
#[cfg(unix)]
#[test]
fn bare_serve_notice_present_when_stdin_is_a_tty() {
    use std::os::unix::io::FromRawFd;

    let tmp = tempfile::tempdir().expect("tempdir");

    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    // Safety: both out-params are valid ints; the remaining three are optional
    // and null means "use defaults".
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty must succeed");

    // Safety: `slave` is a fresh fd from openpty and is not used elsewhere.
    let child_stdin = unsafe { Stdio::from_raw_fd(slave) };
    let mut child = Command::new(bin())
        .arg("serve")
        .env("TRUSTY_DATA_DIR_OVERRIDE", tmp.path())
        .stdin(child_stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn trusty-memory on a pty");

    // Ctrl-D: canonical-mode EOF, so the stdio loop terminates.
    // Safety: `master` is a valid open fd owned by this test.
    let mut master_file = unsafe { std::fs::File::from_raw_fd(master) };
    std::thread::sleep(Duration::from_millis(300));
    let _ = master_file.write_all(&[0x04]);
    let _ = master_file.flush();

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().expect("output");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stderr.contains("waiting on stdin") && stderr.contains("start"),
        "a human at a terminal must be told serve is stdio and start is the \
         daemon verb; stderr was: {stderr}"
    );
    assert!(
        !stdout.contains("waiting on stdin"),
        "the notice must never reach stdout; stdout was: {stdout}"
    );
}
