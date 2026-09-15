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
