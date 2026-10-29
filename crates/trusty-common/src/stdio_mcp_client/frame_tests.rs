//! #10360: response matching in `StdioMcpClient::request` — only a response
//! (`result` or `error`, no `method`) to the in-flight id is a call's result,
//! and ids never repeat across a respawn.

use serde_json::json;

use super::StdioMcpClient;

/// Extracts the numeric `id` of the request line just read into `$l`.
#[cfg(unix)]
const READ_ID: &str = r#"id=$(printf '%s' "$l" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')"#;

/// Why: #10360 — a server request such as `roots/list` carries an id of its
/// own; when it equals the in-flight id it must not become the call's result.
/// What: the mock answers `initialize`, then on `tools/call` (id 2) first
/// sends a `roots/list` request with id 2, then the real response.
/// Test: This test.
#[tokio::test]
#[cfg(unix)]
async fn server_request_with_matching_id_is_not_returned_as_result() {
    let script = r#"read -r l
printf '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"mock","version":"1"}}}\n'
read -r l
read -r l
printf '{"jsonrpc":"2.0","id":2,"method":"roots/list"}\n'
printf '{"jsonrpc":"2.0","id":2,"result":{"answer":"tool"}}\n'
sleep 5"#;
    let mut client = StdioMcpClient::spawn("sh", &["-c", script], "test-client")
        .await
        .expect("spawn mock");
    client.initialize().await.expect("initialize");
    let result = client
        .call_tool("t", json!({}))
        .await
        .expect("the response, not the server request, is the result");
    assert_eq!(result, json!({ "answer": "tool" }));
}

/// Why: #10360 — `respawn` restarted ids at 1, so a frame carrying an id
/// from the earlier process lifetime matched a request of the new one.
/// What: generation 1 answers `initialize` (id 1) and `tools/call` (id 2),
/// then exits. Generation 2 answers `initialize`, then emits a stale id-2
/// response before answering each later request with `fresh`. The call after
/// the respawn must return `fresh`, which requires an id other than 2.
/// Test: This test.
#[tokio::test]
#[cfg(unix)]
async fn stale_frame_after_respawn_is_not_returned_as_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("generation-1-ran");
    let marker = marker.to_str().expect("utf-8 temp path");
    let script = format!(
        r#"if [ -e "$1" ]; then
  read -r l; {READ_ID}
  printf '{{"jsonrpc":"2.0","id":%s,"result":{{"serverInfo":{{"name":"mock","version":"2"}}}}}}\n' "$id"
  read -r l
  printf '{{"jsonrpc":"2.0","id":2,"result":{{"stale":true}}}}\n'
  while read -r l; do {READ_ID}
    printf '{{"jsonrpc":"2.0","id":%s,"result":{{"fresh":true}}}}\n' "$id"
  done
else
  : > "$1"
  read -r l
  printf '{{"jsonrpc":"2.0","id":1,"result":{{"serverInfo":{{"name":"mock","version":"1"}}}}}}\n'
  read -r l
  read -r l
  printf '{{"jsonrpc":"2.0","id":2,"result":{{"first":true}}}}\n'
fi"#
    );
    let mut client = StdioMcpClient::spawn("sh", &["-c", &script, "sh", marker], "test-client")
        .await
        .expect("spawn mock");
    client.initialize().await.expect("initialize");
    let first = client.call_tool("t", json!({})).await.expect("first call");
    assert_eq!(first, json!({ "first": true }));
    for _ in 0..100 {
        if !client.is_alive() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!client.is_alive(), "generation 1 should have exited");

    let second = client
        .call_tool("t", json!({}))
        .await
        .expect("call after respawn");
    assert_eq!(second, json!({ "fresh": true }), "stale frame was misread");
}

/// Why: #10360 — the response test behind `request`'s skip loop.
/// What: a table of frames against expected id 2; only a `result` or `error`
/// frame with id 2 and no `method` is accepted.
/// Test: This test.
#[test]
fn is_response_to_accepts_only_responses_with_the_id() {
    use super::client::is_response_to;
    let id = json!(2);
    let cases = [
        (json!({"jsonrpc":"2.0","id":2,"result":{}}), true),
        (json!({"jsonrpc":"2.0","id":2,"error":{"code":-1}}), true),
        (json!({"jsonrpc":"2.0","id":2,"method":"roots/list"}), false),
        (
            json!({"jsonrpc":"2.0","id":2,"method":"x","result":{}}),
            false,
        ),
        (json!({"jsonrpc":"2.0","id":2}), false),
        (json!({"jsonrpc":"2.0","id":3,"result":{}}), false),
        (
            json!({"jsonrpc":"2.0","method":"notifications/progress"}),
            false,
        ),
    ];
    for (frame, want) in cases {
        assert_eq!(is_response_to(&frame, &id), want, "{frame}");
    }
}

/// Why: #10360 G1 — a timeout that fires mid-line used to drop the bytes
/// already read, so the next call parsed the line's tail as a frame and
/// failed with "parsing JSON-RPC frame".
/// What: the mock writes the first half of the id-2 reply, waits past the
/// caller's 300 ms bound, writes the tail, then answers the next request.
/// The call after the timeout must get its own result.
/// Test: This test.
#[tokio::test]
#[cfg(unix)]
async fn timeout_mid_line_does_not_corrupt_the_next_call() {
    let script = format!(
        r#"read -r l
printf '{{"jsonrpc":"2.0","id":1,"result":{{"serverInfo":{{"name":"mock","version":"1"}}}}}}\n'
read -r l
read -r l
printf '{{"jsonrpc":"2.0","id":2,"result":'
sleep 1
printf '{{"late":true}}}}\n'
read -r l; {READ_ID}
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"fresh":true}}}}\n' "$id"
sleep 5"#
    );
    let mut client = StdioMcpClient::spawn("sh", &["-c", &script], "test-client")
        .await
        .expect("spawn mock");
    client.initialize().await.expect("initialize");
    let bound = std::time::Duration::from_millis(300);
    let first = client.call_tool_with_timeout("t", json!({}), bound).await;
    assert!(first.is_err(), "the first call must time out: {first:?}");
    let second = client
        .call_tool("t", json!({}))
        .await
        .expect("the call after a mid-line timeout");
    assert_eq!(second, json!({ "fresh": true }));
}

/// Why: #10360 G2 — a whole late reply to a timed-out call N, arriving while
/// call N+1 waits, must be skipped rather than returned as N+1's result.
/// What: the mock answers id 2 only after the 300 ms bound expired, then
/// answers the next request with `fresh`.
/// Test: This test.
#[tokio::test]
#[cfg(unix)]
async fn late_reply_to_timed_out_call_is_skipped() {
    let script = format!(
        r#"read -r l
printf '{{"jsonrpc":"2.0","id":1,"result":{{"serverInfo":{{"name":"mock","version":"1"}}}}}}\n'
read -r l
read -r l
sleep 1
printf '{{"jsonrpc":"2.0","id":2,"result":{{"late":true}}}}\n'
read -r l; {READ_ID}
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"fresh":true}}}}\n' "$id"
sleep 5"#
    );
    let mut client = StdioMcpClient::spawn("sh", &["-c", &script], "test-client")
        .await
        .expect("spawn mock");
    client.initialize().await.expect("initialize");
    let bound = std::time::Duration::from_millis(300);
    let first = client.call_tool_with_timeout("t", json!({}), bound).await;
    assert!(first.is_err(), "the first call must time out: {first:?}");
    let second = client.call_tool("t", json!({})).await.expect("call N+1");
    assert_eq!(second, json!({ "fresh": true }), "late reply was misread");
}

/// Why: #10360 G3 — a frame with the awaited id that carries both `method`
/// and `error` is a server request, not this call's failure.
/// What: on `tools/call` (id 2) the mock sends that frame, then the real
/// response; the call returns the real result.
/// Test: This test.
#[tokio::test]
#[cfg(unix)]
async fn error_frame_with_method_is_not_returned_as_result() {
    let script = r#"read -r l
printf '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"mock","version":"1"}}}\n'
read -r l
read -r l
printf '{"jsonrpc":"2.0","id":2,"method":"x","error":{"code":-1,"message":"m"}}\n'
printf '{"jsonrpc":"2.0","id":2,"result":{"answer":"tool"}}\n'
sleep 5"#;
    let mut client = StdioMcpClient::spawn("sh", &["-c", script], "test-client")
        .await
        .expect("spawn mock");
    client.initialize().await.expect("initialize");
    let result = client.call_tool("t", json!({})).await.expect("real result");
    assert_eq!(result, json!({ "answer": "tool" }));
}

/// Why: #10360 — `line_buf` persists across calls, so a child that streams
/// without a newline must not grow it without bound.
/// What: after the handshake the mock writes 17 MB of `{` with no newline
/// for a while, then ends the line and answers the next request with `fresh`.
/// The first call must fail naming the cap and leave the buffer bounded; the
/// second must discard the oversized line's tail and return `fresh`.
/// Test: This test.
#[tokio::test]
#[cfg(unix)]
async fn oversized_line_errors_and_next_call_skips_its_tail() {
    let script = format!(
        r#"read -r l
printf '{{"jsonrpc":"2.0","id":1,"result":{{"serverInfo":{{"name":"mock","version":"1"}}}}}}\n'
read -r l
read -r l
head -c 17000000 /dev/zero | tr '\0' '{{'
sleep 1
head -c 1000 /dev/zero | tr '\0' '{{'
printf '\n'
read -r l; {READ_ID}
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"fresh":true}}}}\n' "$id"
sleep 5"#
    );
    let mut client = StdioMcpClient::spawn("sh", &["-c", &script], "test-client")
        .await
        .expect("spawn mock");
    client.initialize().await.expect("initialize");
    let first = client.call_tool("t", json!({})).await;
    let err = format!(
        "{:#}",
        first.expect_err("an oversized line must fail the call")
    );
    assert!(err.contains("16777216"), "error must name the cap: {err}");
    assert!(
        client.line_buf.len() <= super::MAX_LINE_BYTES,
        "buffer must stay bounded: {}",
        client.line_buf.len()
    );
    let second = client
        .call_tool("t", json!({}))
        .await
        .expect("the call after an oversized line");
    assert_eq!(second, json!({ "fresh": true }));
}

/// Why: #10360 — `id` and `method` of a skipped frame are server-controlled;
/// logging them whole lets a child flood the log.
/// What: a 10 KB method truncates to 64 characters plus `…(+N)`.
/// Test: This test.
#[test]
fn log_field_truncates_long_values() {
    let long = "m".repeat(10_000);
    let shown = super::client::log_field(&long);
    assert_eq!(shown, format!("{}…(+9936)", "m".repeat(64)));
    assert_eq!(super::client::log_field("ping"), "ping");
}
