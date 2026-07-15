//! Integration test for `tm hook` idle-parking detection (issue #2610).
//!
//! Why: `core::idle_parking`'s unit tests cover the pure detection logic, but
//! none exercises the actual Stop/SubagentStop → read-transcript → warn path
//! end to end through the real binary. This is the mechanical back-stop that
//! flags a delegated agent that ended its turn to "wait" for a self-spawned
//! monitor that can never re-wake it; it must fire on real turn-end payloads and
//! stay silent otherwise. This file closes that gap.
//! What: runs the built `tm` binary as `tm --url <unreachable> hook` with a
//! `SubagentStop` (or other) stdin payload that names a temp transcript file,
//! then asserts on the process stderr (the loud IDLE-PARKING WARNING) and a
//! clean `exit 0` (fail-open — the best-effort daemon POST to the unreachable
//! URL must never fail the hook). The `session_id`/`transcript_path` field names
//! match the live Claude Code hooks reference
//! (<https://code.claude.com/docs/en/hooks>).
//! Test: `cargo test -p trusty-mpm --test tm_hook_idle_parking`.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run `tm hook` with the given stdin JSON, returning `(exit_ok, stderr)`.
fn run_hook(stdin_json: &str) -> (bool, String) {
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut child = Command::new(bin)
        .args(["--url", "http://127.0.0.1:1", "hook"])
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `tm hook`");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait for tm hook");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Write a single-line JSONL transcript with one assistant message and return
/// the temp file (kept alive by the caller so the path stays valid).
fn transcript_with_assistant(text: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().expect("create temp transcript");
    let line = serde_json::json!({
        "type": "assistant",
        "message": { "content": [{ "type": "text", "text": text }] }
    });
    writeln!(f, "{line}").expect("write transcript line");
    f.flush().expect("flush transcript");
    f
}

fn payload(event: &str, transcript_path: &std::path::Path) -> String {
    serde_json::json!({
        "hook_event_name": event,
        "session_id": "sess-2610",
        "transcript_path": transcript_path.to_string_lossy(),
    })
    .to_string()
}

#[test]
fn hook_flags_idle_parking_final_message_on_subagent_stop() {
    let tf = transcript_with_assistant(
        "I've started a background polling monitor for the checks and will report back once green.",
    );
    let (ok, stderr) = run_hook(&payload("SubagentStop", tf.path()));

    assert!(ok, "hook must exit 0 (fail-open), stderr={stderr}");
    assert!(
        stderr.contains("IDLE-PARKING WARNING (#2610)"),
        "expected idle-parking warning on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("session=sess-2610"),
        "warning must identify the session, got: {stderr:?}"
    );
}

#[test]
fn hook_flags_idle_parking_on_main_stop() {
    let tf = transcript_with_assistant("Standing by for the CI run to complete.");
    let (ok, stderr) = run_hook(&payload("Stop", tf.path()));

    assert!(ok, "hook must exit 0 (fail-open), stderr={stderr}");
    assert!(
        stderr.contains("IDLE-PARKING WARNING (#2610)"),
        "expected idle-parking warning on stderr for Stop, got: {stderr:?}"
    );
}

#[test]
fn hook_stays_silent_for_clean_final_message() {
    let tf = transcript_with_assistant(
        "I ran `gh pr checks 2606 --watch --fail-fast` in the foreground; all checks passed. Done.",
    );
    let (ok, stderr) = run_hook(&payload("SubagentStop", tf.path()));

    assert!(ok, "hook must exit 0, stderr={stderr}");
    assert!(
        !stderr.contains("IDLE-PARKING WARNING"),
        "clean final message must NOT be flagged, got: {stderr:?}"
    );
}

#[test]
fn hook_does_not_scan_non_turn_end_events() {
    // A PreToolUse/PostToolUse event must never trigger the transcript scan,
    // even if the transcript happens to contain parking language.
    let tf = transcript_with_assistant("standing by for the monitor to report back");
    let (ok, stderr) = run_hook(&payload("PostToolUse", tf.path()));

    assert!(ok, "hook must exit 0, stderr={stderr}");
    assert!(
        !stderr.contains("IDLE-PARKING WARNING"),
        "non-turn-end event must not scan for parking, got: {stderr:?}"
    );
}

/// Code-critic finding (PR #2620): `read_transcript_tail`'s caller wraps the
/// whole read in `tokio::time::timeout`, but that only bounds the `.await` —
/// the underlying blocking `open()` syscall (run on a `spawn_blocking` worker)
/// still runs to completion. `open()` on a FIFO with no writer blocks
/// indefinitely at the kernel level, which would leak a stuck blocking-pool
/// thread even though the hook process itself still exits promptly. The fix
/// `stat()`s the path first (which never blocks on a FIFO) and refuses
/// anything that is not a regular file before ever calling `open()`.
///
/// This proves the FIFO case end to end: `tm hook` must return promptly (well
/// under the test's generous bound) with a clean `exit 0` and no idle-parking
/// warning — not hang waiting on the FIFO to be opened for writing (which
/// never happens, since nothing else opens it).
///
/// `mkfifo` is a POSIX-only primitive, hence `#[cfg(unix)]`; the equivalent
/// Windows named-pipe API has different open semantics and is out of scope —
/// this test skips cleanly (does not compile / run at all) on non-unix.
#[cfg(unix)]
#[test]
fn rejects_fifo_without_blocking() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let fifo_path = dir.path().join("transcript.fifo");

    let mkfifo_status = Command::new("mkfifo")
        .arg(&fifo_path)
        .status()
        .expect("failed to spawn mkfifo");
    assert!(mkfifo_status.success(), "mkfifo must succeed");

    let bin = env!("CARGO_BIN_EXE_tm");
    let mut child = Command::new(bin)
        .args(["--url", "http://127.0.0.1:1", "hook"])
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `tm hook`");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(payload("SubagentStop", &fifo_path).as_bytes())
        .expect("write stdin");

    // Bounded poll instead of `wait_with_output()`: if the `stat`-first guard
    // regresses and `open()` is reached, this must fail LOUDLY (assertion) —
    // it must never hang the test suite waiting on a FIFO nothing will write to.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tm hook did not exit within the bound — the stat-before-open FIFO \
             guard has regressed and open() is blocking on the FIFO"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    let output = child.wait_with_output().expect("collect output after exit");
    assert!(
        status.success(),
        "tm hook must exit 0 even for an unreadable transcript path (fail-open), \
         status={status:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("IDLE-PARKING WARNING"),
        "a FIFO transcript path must never be scanned, got: {stderr:?}"
    );
}
