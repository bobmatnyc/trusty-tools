//! `tm hook --pm-guard` with a stdin payload it cannot read (#7975).
//!
//! Why: the guard used to ALLOW whenever the `PreToolUse` payload failed to
//! read or parse. A code-critic saw destructive-command payloads piped through
//! a shell arrive as empty stdin and pass. The guard is registered on
//! `PreToolUse` only, and Claude Code always sends that event a JSON object on
//! stdin, so an unreadable payload is never a legitimate "nothing to guard".
//! What: drives the real binary with an empty stdin, a stdin whose read returns
//! `Err`, truncated or non-object JSON, and a stdin held open past the read
//! timeout. Round 2 adds the payloads that PARSED but name no classifiable tool
//! call — a non-string `tool_name`, a `Bash` call with no `tool_input` — which
//! used to ALLOW globally with no audit record. Each must print a `deny`. Three
//! controls sit beside them: a valid destructive payload still denies, a
//! megabyte `tool_input` is judged on its content rather than on the read
//! deadline, and a parsed payload naming no guarded operation still allows.
//! Test: `cargo test -p trusty-mpm --test tm_hook_pm_guard_stdin_7975`.

mod common;

use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// Nothing listens on port 1, so the deny path's audit POST fails fast.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// A `tm hook --pm-guard` command with every operator escape hatch stripped,
/// run outside any checkout so no directory-keyed rule decides the case.
fn guard_command(cwd: &std::path::Path) -> Command {
    let mut command = common::tm_command_in(common::tm_spawn_home());
    command
        .args(["--url", UNREACHABLE_DAEMON, "hook", "--pm-guard"])
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Run the guard with `stdin` as its standard input and return stdout.
fn run_with_stdin(stdin: Stdio) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = guard_command(dir.path())
        .stdin(stdin)
        .output()
        .expect("run tm hook --pm-guard");
    assert!(
        output.status.success(),
        "the guard reports its decision on stdout with exit 0: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
}

/// Run the guard with `bytes` written to a piped stdin that is then closed.
fn run_with_bytes(bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = guard_command(dir.path())
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn tm hook --pm-guard");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(bytes)
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for the guard");
    assert!(
        output.status.success(),
        "the guard reports its decision on stdout with exit 0: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
}

/// The `permissionDecision` the guard printed, or `None` for an ALLOW.
fn decision_of(stdout: &str) -> Option<(String, String)> {
    let line = stdout.trim();
    if line.is_empty() {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("the guard's stdout must be one JSON object ({e}): {stdout}"));
    let out = &parsed["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PreToolUse", "got: {stdout}");
    Some((
        out["permissionDecision"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        out["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    ))
}

/// Assert a deny whose reason names the stdin payload failure.
fn assert_payload_failure_denied(stdout: &str, case: &str) {
    let Some((decision, reason)) = decision_of(stdout) else {
        panic!("{case}: an unreadable payload must DENY, but the guard allowed (empty stdout)");
    };
    assert_eq!(decision, "deny", "{case}: got {stdout}");
    assert!(
        reason.contains("stdin payload"),
        "{case}: the reason must name the payload failure, got: {reason}"
    );
}

#[test]
fn pm_guard_denies_an_empty_stdin_payload() {
    assert_payload_failure_denied(&run_with_bytes(b""), "empty stdin");
    assert_payload_failure_denied(&run_with_bytes(b" \n\t"), "whitespace-only stdin");
    assert_payload_failure_denied(&run_with_stdin(Stdio::null()), "stdin at /dev/null");
}

#[test]
fn pm_guard_denies_a_stdin_read_error() {
    // Invalid UTF-8 makes the string read itself return `Err(InvalidData)`.
    assert_payload_failure_denied(
        &run_with_bytes(b"{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"rm -rf /\xff\"}}"),
        "non-UTF-8 stdin",
    );
    // A directory as fd 0: `read(2)` returns `EISDIR`.
    let dir = tempfile::tempdir().expect("tempdir");
    let as_stdin = std::fs::File::open(dir.path()).expect("open a directory for reading");
    assert_payload_failure_denied(&run_with_stdin(Stdio::from(as_stdin)), "directory as stdin");
}

#[test]
fn pm_guard_denies_truncated_or_non_object_json() {
    for (case, bytes) in [
        (
            "truncated destructive payload",
            br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf /"#
                .as_slice(),
        ),
        ("plain text", b"not json at all".as_slice()),
        ("a JSON array", b"[]".as_slice()),
        ("a JSON string", br#""PreToolUse""#.as_slice()),
    ] {
        assert_payload_failure_denied(&run_with_bytes(bytes), case);
    }
}

#[test]
fn pm_guard_denies_a_stdin_that_stays_open_past_the_read_timeout() {
    // The delivery-race shape: part of a payload is written and the writer
    // never closes the pipe. The guard's bounded read times out, and the guard
    // must EXIT with its decision while stdin is still open: Claude Code
    // discards the output of a hook it cancels at its timeout, and a cancelled
    // `PreToolUse` command hook does not block the call.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = guard_command(dir.path())
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn tm hook --pm-guard");
    let mut stdin = child.stdin.take().expect("child stdin");
    stdin
        .write_all(br#"{"hook_event_name":"PreToolUse","tool_name":"Bash""#)
        .expect("write stdin");
    stdin.flush().expect("flush stdin");
    let mut child_stdout = child.stdout.take().expect("child stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = child_stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    // Well past the 5 s guard read deadline (`PM_GUARD_STDIN_TIMEOUT`, widened
    // from the 500 ms advisory budget in round 2), short of the 10 s hook
    // timeout Claude Code is told to allow.
    let stdout = rx.recv_timeout(std::time::Duration::from_secs(9));
    // Close stdin only now, so a guard that waits for EOF is still released.
    drop(stdin);
    let status = child.wait().expect("wait for the guard");
    let stdout = stdout.expect("the guard must exit while stdin is still open");
    assert!(status.success(), "status={status:?}");
    assert_payload_failure_denied(&stdout, "stdin held open");
}

#[test]
fn pm_guard_still_denies_a_valid_destructive_payload() {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "rm -rf /"},
    })
    .to_string();
    let stdout = run_with_bytes(payload.as_bytes());
    let (decision, _) = decision_of(&stdout).expect("`rm -rf /` must be denied");
    assert_eq!(decision, "deny", "got: {stdout}");
}

/// A `PreToolUse` payload for `rm -rf /root` with `extra_targets` more paths.
///
/// Why (#7975 round 2): `/root` is denied by the ABSOLUTE destructive-delete
/// rule (#4031), which runs before every exemption and before the per-turn
/// file-change budget — so the verdict is a property of the command alone, and
/// padding the argument list is the one way to grow the payload past a megabyte
/// without changing what it asks for.
/// Test: `pm_guard_reads_a_megabyte_tool_input_and_decides_on_its_content`.
fn destructive_payload(extra_targets: usize) -> String {
    let mut command = String::from("rm -rf /root");
    for i in 0..extra_targets {
        command.push_str(&format!(" /tmp/scratch-{i}"));
    }
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string()
}

#[test]
fn pm_guard_reads_a_megabyte_tool_input_and_decides_on_its_content() {
    // Round 2, code-critic HIGH: the fail-closed read reused the 500 ms
    // ADVISORY budget, so a payload that is merely large — or a host that is
    // merely loaded — would have been denied on the DEADLINE rather than on
    // what it asks for, for every tool, since the guard is registered with
    // `matcher: ""`. A megabyte `tool_input` must reach the rules intact.
    // The proof is a twin: the same command padded past a megabyte must draw
    // the same verdict, word for word, as its short form.
    let short = run_with_bytes(destructive_payload(0).as_bytes());
    let (short_decision, short_reason) =
        decision_of(&short).expect("`rm -rf /root` must be denied");
    assert_eq!(short_decision, "deny", "got: {short}");

    let payload = destructive_payload(120_000);
    assert!(
        payload.len() > 1024 * 1024,
        "the padded payload must exceed a megabyte, is {} bytes",
        payload.len()
    );
    let started = std::time::Instant::now();
    let large = run_with_bytes(payload.as_bytes());
    let elapsed = started.elapsed();

    let (decision, reason) = decision_of(&large).expect("a megabyte payload must still be judged");
    assert_eq!(decision, "deny", "got: {large}");
    assert_eq!(
        reason, short_reason,
        "a megabyte payload must draw the same content-based verdict as its short twin"
    );
    assert!(
        !reason.contains("stdin payload"),
        "the deny must come from the command, not from the read: {reason}"
    );
    // Spawn, read, classify, and the audit POST to a dead port together must
    // finish well inside the 10 s the guard hook is registered with.
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "the guard took {elapsed:?} on a {}-byte payload",
        payload.len()
    );
}

#[test]
fn pm_guard_denies_a_payload_whose_tool_name_is_not_a_string() {
    // Round 2, code-critic MEDIUM: a `tool_name` the guard cannot read used to
    // ALLOW globally, with no audit record, bypassing every ABSOLUTE rule.
    for (case, payload) in [
        (
            "numeric tool_name",
            r#"{"hook_event_name":"PreToolUse","tool_name":42}"#,
        ),
        ("absent tool_name", r#"{"hook_event_name":"PreToolUse"}"#),
        (
            "null tool_name",
            r#"{"hook_event_name":"PreToolUse","tool_name":null,"tool_input":{}}"#,
        ),
    ] {
        let stdout = run_with_bytes(payload.as_bytes());
        let Some((decision, reason)) = decision_of(&stdout) else {
            panic!("{case}: an unclassifiable payload must DENY, but the guard allowed");
        };
        assert_eq!(decision, "deny", "{case}: got {stdout}");
        assert!(reason.contains("`tool_name`"), "{case}: got {reason}");
    }
}

#[test]
fn pm_guard_denies_a_bash_payload_with_no_tool_input() {
    // Same finding, the other half: `tool_name: "Bash"` with no `tool_input`
    // classified against an empty command, which every Bash rule allows — a
    // global ALLOW reached through a delivery fault.
    for (case, payload) in [
        (
            "absent tool_input",
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#,
        ),
        (
            "string tool_input",
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":"rm -rf /"}"#,
        ),
    ] {
        let stdout = run_with_bytes(payload.as_bytes());
        let Some((decision, reason)) = decision_of(&stdout) else {
            panic!("{case}: a Bash payload with no tool_input must DENY, but the guard allowed");
        };
        assert_eq!(decision, "deny", "{case}: got {stdout}");
        assert!(reason.contains("`tool_input`"), "{case}: got {reason}");
    }
}

#[test]
fn pm_guard_allows_a_parsed_payload_naming_no_guarded_operation() {
    // Surrounding whitespace is not a failure: the payload parses.
    let payload = format!(
        "\n  {}  \n",
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Glob",
            "tool_input": {"pattern": "**/*.md"},
        })
    );
    assert_eq!(decision_of(&run_with_bytes(payload.as_bytes())), None);
}
