//! Unit tests for [`super`] (the #7975 hook stdin reader).

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::ReadBuf;

use super::*;

/// A reader whose every read returns `Err`, standing in for a broken stdin.
struct FailingReader;

impl AsyncRead for FailingReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::other("injected read failure")))
    }
}

async fn read_bytes(bytes: &'static [u8]) -> Result<serde_json::Value, HookStdinError> {
    read_hook_stdin(bytes, HOOK_STDIN_TIMEOUT).await
}

#[tokio::test]
async fn read_hook_stdin_reports_empty_input() {
    for bytes in [b"".as_slice(), b" \n\t ".as_slice()] {
        let result = read_bytes(bytes).await;
        assert!(
            matches!(result, Err(HookStdinError::Empty)),
            "{bytes:?}: {result:?}"
        );
    }
}

#[tokio::test]
async fn read_hook_stdin_reports_a_read_error() {
    let result = read_hook_stdin(FailingReader, HOOK_STDIN_TIMEOUT).await;
    assert!(
        matches!(&result, Err(HookStdinError::Read(e)) if e.to_string() == "injected read failure"),
        "{result:?}"
    );
}

#[tokio::test]
async fn read_hook_stdin_reports_a_timeout() {
    // The writer half stays alive and never writes, so the read never ends.
    let (_writer, reader) = tokio::io::duplex(64);
    let limit = Duration::from_millis(20);
    let result = read_hook_stdin(reader, limit).await;
    assert!(
        matches!(result, Err(HookStdinError::TimedOut(d)) if d == limit),
        "{result:?}"
    );
}

#[tokio::test]
async fn read_hook_stdin_reports_truncated_and_non_object_json() {
    let truncated: &'static [u8] =
        br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf /"#;
    let result = read_bytes(truncated).await;
    assert!(
        matches!(result, Err(HookStdinError::Parse(_))),
        "{result:?}"
    );
    for bytes in [b"[]".as_slice(), b"42".as_slice(), br#""x""#.as_slice()] {
        let result = read_bytes(bytes).await;
        assert!(
            matches!(result, Err(HookStdinError::NotAnObject)),
            "{bytes:?}: {result:?}"
        );
    }
}

#[tokio::test]
async fn read_hook_stdin_returns_a_parsed_object() {
    let payload = read_bytes(b"\n {\"tool_name\":\"Glob\"} \n")
        .await
        .expect("a JSON object with surrounding whitespace parses");
    assert_eq!(payload["tool_name"], "Glob");
}

#[test]
fn unreadable_payload_deny_reason_names_the_failure() {
    let parse_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    for err in [
        HookStdinError::Empty,
        HookStdinError::Read(std::io::Error::other("boom")),
        HookStdinError::TimedOut(HOOK_STDIN_TIMEOUT),
        HookStdinError::Parse(parse_error),
        HookStdinError::NotAnObject,
    ] {
        let reason = unreadable_payload_deny_reason(&err);
        assert!(reason.contains("stdin payload"), "{reason}");
        assert!(reason.contains(&err.to_string()), "{reason}");
    }
}

/// A reader that hands out `total` bytes in `chunks` slices, sleeping `gap`
/// between them, standing in for a loaded host's slow pipe writer.
///
/// Why (#7975 round 2): the guard-budget widening has to be provable without
/// depending on the host actually being loaded. A writer that spans the 500 ms
/// advisory budget but not the 5 s guard budget makes the difference
/// deterministic.
fn slow_reader(total: usize, chunks: usize, gap: Duration) -> impl tokio::io::AsyncRead + Unpin {
    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let body = "x".repeat(total);
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let payload = format!(r#"{{"tool_name":"Write","tool_input":{{"content":"{body}"}}}}"#);
        let bytes = payload.into_bytes();
        let step = bytes.len().div_ceil(chunks);
        for chunk in bytes.chunks(step) {
            if writer.write_all(chunk).await.is_err() {
                return;
            }
            tokio::time::sleep(gap).await;
        }
        let _ = writer.shutdown().await;
    });
    reader
}

#[tokio::test]
async fn read_hook_stdin_reads_a_slow_megabyte_inside_the_guard_budget() {
    // 1 MiB of `tool_input` spread over ~1.2 s: past the 500 ms advisory
    // budget, well inside the 5 s guard budget. The guard must decide on the
    // payload's content, never on the deadline.
    const MEGABYTE: usize = 1024 * 1024;
    let gap = Duration::from_millis(100);
    let started = std::time::Instant::now();
    let payload = read_hook_stdin(slow_reader(MEGABYTE, 12, gap), PM_GUARD_STDIN_TIMEOUT)
        .await
        .expect("a slow megabyte must read inside the guard budget");
    let elapsed = started.elapsed();
    assert_eq!(payload["tool_name"], "Write");
    assert!(
        payload["tool_input"]["content"].as_str().unwrap().len() >= MEGABYTE,
        "the whole tool_input must survive the read"
    );
    assert!(
        elapsed > HOOK_STDIN_TIMEOUT,
        "the fixture must outlast the advisory budget to be worth anything: {elapsed:?}"
    );

    // Red-first proof for the widening: the same payload under the old 500 ms
    // constant times out, which under #7975's fail-closed read is a DENY.
    let result = read_hook_stdin(slow_reader(MEGABYTE, 12, gap), HOOK_STDIN_TIMEOUT).await;
    assert!(
        matches!(result, Err(HookStdinError::TimedOut(d)) if d == HOOK_STDIN_TIMEOUT),
        "{result:?}"
    );
}

#[test]
fn hook_stdin_only_the_advisory_read_uses_the_short_budget() {
    // The two budgets must stay distinct, and the advisory one must stay the
    // shorter: a hook that only skips work can afford to give up sooner than
    // one that denies the call.
    assert!(HOOK_STDIN_TIMEOUT < PM_GUARD_STDIN_TIMEOUT);
    assert_eq!(HOOK_STDIN_TIMEOUT, Duration::from_millis(500));
}

#[test]
fn guard_read_budget_leaves_the_audit_post_inside_the_hook_timeout() {
    // A deny's worst case is THREE bounds. `main.rs` resolves the daemon URL
    // through the console-gateway probe before it dispatches anything, and the
    // registered guard command carries no `--url`, so that probe is a fixed
    // prefix on every invocation — not an occasional one. All three are the
    // real constants, not copies, so lowering any one of them trips this.
    let worst_case = trusty_mpm::core::discovery::GATEWAY_PROBE_TIMEOUT
        + PM_GUARD_STDIN_TIMEOUT
        + crate::commands::pm_guard::AUDIT_POST_TIMEOUT;
    assert!(
        worst_case < REGISTERED_HOOK_TIMEOUT,
        "{worst_case:?} must fit inside {REGISTERED_HOOK_TIMEOUT:?}"
    );
    // And it must leave room for exec and classification, not merely squeak in.
    assert!(
        REGISTERED_HOOK_TIMEOUT - worst_case >= Duration::from_secs(2),
        "only {:?} of headroom left",
        REGISTERED_HOOK_TIMEOUT - worst_case
    );
}

#[test]
fn unclassifiable_payload_detail_names_the_bad_field() {
    for (case, payload) in [
        (
            "no tool_name",
            serde_json::json!({"hook_event_name": "PreToolUse"}),
        ),
        ("numeric tool_name", serde_json::json!({"tool_name": 42})),
        ("null tool_name", serde_json::json!({"tool_name": null})),
    ] {
        let detail = unclassifiable_payload_detail(&payload);
        assert_eq!(
            detail,
            Some("`tool_name` is missing or is not a string"),
            "{case}"
        );
    }
    for (case, payload) in [
        ("no tool_input", serde_json::json!({"tool_name": "Bash"})),
        (
            "string tool_input",
            serde_json::json!({"tool_name": "Bash", "tool_input": "rm -rf /"}),
        ),
    ] {
        let detail = unclassifiable_payload_detail(&payload);
        assert_eq!(
            detail,
            Some("`tool_input` is missing or is not an object"),
            "{case}"
        );
    }
    assert_eq!(
        unclassifiable_payload_detail(&serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {},
        })),
        None,
        "an empty tool_input object is the documented shape for a no-argument tool"
    );
}

#[test]
fn unclassifiable_payload_deny_reason_names_the_failure() {
    for detail in [
        "`tool_name` is missing or is not a string",
        "`tool_input` is missing or is not an object",
    ] {
        let reason = unclassifiable_payload_deny_reason(detail);
        assert!(reason.contains(detail), "{reason}");
        assert!(reason.contains("#7975"), "{reason}");
    }
}
