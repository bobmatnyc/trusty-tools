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
//! deadline — its only denied target is its LAST argument, so only a read that
//! consumed the whole payload reaches it — and a parsed payload naming no
//! guarded operation still allows.
//! Test: `cargo test -p trusty-mpm --test tm_hook_pm_guard_stdin_7975`.

mod common;

use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// Nothing listens on port 1, so the deny path's audit POST fails fast.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// A `tm hook --pm-guard` command with every operator escape hatch stripped,
/// run outside any checkout so no directory-keyed rule decides the case.
fn guard_command(cwd: &std::path::Path) -> Command {
    guard_command_against(cwd, UNREACHABLE_DAEMON)
}

/// [`guard_command`] pointed at `url` for its audit POST.
///
/// Why (#7975 round 2): one test needs the audit record itself, which means a
/// reachable sink rather than the dead port the decision tests use.
/// Test: `pm_guard_denies_a_bash_payload_with_no_tool_input`.
fn guard_command_against(cwd: &std::path::Path, url: &str) -> Command {
    let mut command = common::tm_command_in(common::tm_spawn_home());
    command
        .args(["--url", url, "hook", "--pm-guard"])
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
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf8");
    common::assert_pm_guard_refusals_prefixed(&stdout);
    stdout
}

/// The audit POST body the guard sent, captured from a one-shot HTTP sink.
///
/// Why (#7975 round 2, code-critic MEDIUM): on the unclassifiable-payload deny
/// the audit record is the ONLY alarm — nothing else names which tool was
/// refused — so a test has to read it rather than trust it. The decision tests
/// point at a dead port on purpose; this one needs a listener.
/// What: binds an ephemeral port, runs the guard against it with `bytes` on
/// stdin, reads the single request, answers `200`, and returns the parsed JSON
/// body. Raw `TcpListener` rather than a server framework: one request, no
/// routing, and no runtime to start inside a `#[test]`.
/// Test: `pm_guard_denies_a_bash_payload_with_no_tool_input`.
fn audit_record_for(bytes: &[u8]) -> serde_json::Value {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an audit sink");
    let url = format!("http://{}", listener.local_addr().expect("sink address"));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stream.read(&mut chunk) {
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
            if request_is_complete(&raw) {
                break;
            }
        }
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let _ = stream.flush();
        let _ = tx.send(raw);
    });

    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = guard_command_against(dir.path(), &url)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn tm hook --pm-guard");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(bytes)
        .expect("write stdin");
    child.wait_with_output().expect("wait for the guard");

    let raw = rx
        .recv_timeout(std::time::Duration::from_secs(8))
        .expect("the guard must send exactly one audit POST");
    let split = find_header_end(&raw).expect("the captured request must have a header block");
    serde_json::from_slice(&raw[split..]).unwrap_or_else(|e| {
        panic!(
            "the audit body must be JSON ({e}): {}",
            String::from_utf8_lossy(&raw)
        )
    })
}

/// Byte offset just past the request's `\r\n\r\n` header terminator.
fn find_header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Whether `raw` holds a whole request — headers plus `Content-Length` bytes.
fn request_is_complete(raw: &[u8]) -> bool {
    let Some(body_start) = find_header_end(raw) else {
        return false;
    };
    let headers = String::from_utf8_lossy(&raw[..body_start]).to_lowercase();
    let Some(len) = headers
        .split("content-length:")
        .nth(1)
        .and_then(|rest| rest.split(['\r', '\n']).next())
        .and_then(|v| v.trim().parse::<usize>().ok())
    else {
        // No body declared: the headers alone are the whole request.
        return true;
    };
    raw.len() - body_start >= len
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
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf8");
    common::assert_pm_guard_refusals_prefixed(&stdout);
    stdout
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

/// The `timeout` the pm-guard hook is registered with, mirrored from
/// `core::session_launch::settings::pm_guard_hook_value`.
const REGISTERED_HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Wall-clock ceiling for one megabyte guard run in this suite.
///
/// Why (#7975, #8078): the megabyte control used to assert a flat 8 s, which measured
/// the host rather than the guard and flaked at 8.20–9.37 s. A debug `tm` pays
/// a multi-second cold page-in and unoptimized rule evaluation that the release
/// binary Claude Code execs does not, and this suite spawns its children in
/// parallel with the rest of the crate's tests. The verdict assertions in
/// `pm_guard_reads_a_megabyte_tool_input_and_decides_on_its_content` are the
/// property; this is a liveness backstop, sized at six times the registered
/// hook timeout so it still catches an order-of-magnitude regression — a
/// quadratic scan of the payload, or a read that falls back on its deadline —
/// without policing a budget a debug build cannot measure.
/// Test: `pm_guard_reads_a_megabyte_tool_input_and_decides_on_its_content`.
const MEGABYTE_RUN_CEILING: std::time::Duration =
    std::time::Duration::from_secs(6 * REGISTERED_HOOK_TIMEOUT.as_secs());

/// A `PreToolUse` payload for `rm -rf` over `extra_targets` scratch paths,
/// ending in `/root`.
///
/// Why (#7975 round 2): `/root` is denied by the ABSOLUTE destructive-delete
/// rule (#4031), which runs before every exemption and before the per-turn
/// file-change budget — so the verdict is a property of the command alone, and
/// padding the argument list is the one way to grow the payload past a megabyte
/// without changing what it asks for.
/// What: `/root` is the LAST argument, so a guard that stopped reading early
/// would classify a padded command with no denied target at all. #7975: that
/// placement, not a wall clock, is what proves the whole payload was consumed.
/// `extra_targets: 0` yields the bare `rm -rf /root` short twin.
/// Test: `pm_guard_reads_a_megabyte_tool_input_and_decides_on_its_content`.
fn destructive_payload(extra_targets: usize) -> String {
    let mut command = String::from("rm -rf");
    for i in 0..extra_targets {
        command.push_str(&format!(" /tmp/scratch-{i}"));
    }
    command.push_str(" /root");
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
    // the same verdict, word for word, as its short form — and because the
    // padded form's only denied target is its LAST argument, that verdict is
    // reachable only from a read that consumed every byte.
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
        "a megabyte payload must draw the same content-based verdict as its short twin, \
         which only a read of its last argument can reach"
    );
    assert!(
        !reason.contains("stdin payload"),
        "the deny must come from the command, not from the read: {reason}"
    );
    // #7975: the verdict above is the property. This only catches a guard that
    // has become slow by an order of magnitude — see `MEGABYTE_RUN_CEILING`.
    assert!(
        elapsed < MEGABYTE_RUN_CEILING,
        "the guard took {elapsed:?} on a {}-byte payload, past the {MEGABYTE_RUN_CEILING:?} \
         liveness backstop",
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

    // Round 2: the audit record is the only alarm this path raises, and on
    // this arm `tool_name` IS readable — a record that says the guard denied
    // "" cannot tell an operator which tool was refused.
    let record = audit_record_for(
        br#"{"hook_event_name":"PreToolUse","session_id":"s-7975","tool_name":"Bash"}"#,
    );
    assert_eq!(record["payload"]["tool"], "Bash", "got: {record}");
    assert_eq!(
        record["payload"]["pm_guard_decision"], "deny",
        "got: {record}"
    );
    assert_eq!(record["session_id"], "s-7975", "got: {record}");
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
