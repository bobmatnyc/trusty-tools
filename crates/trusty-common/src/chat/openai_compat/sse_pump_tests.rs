//! Failure-path tests for the shared OpenAI-compatible SSE pump (issue #3757).
//!
//! Why: before #3757 every one of these cases arrived at the consumer as a
//! clean `ChatEvent::Done`, so a partial or empty answer rendered as a complete
//! one. Each test below pins one of those cases to an explicit
//! `ChatEvent::Error`.
//! What: the pure decoders (`error_message_from_frame`, `is_event_stream`,
//! `handle_line`) are exercised directly; the transport-level guards
//! (truncated EOF, non-SSE body) go through a raw-TCP mock server, matching the
//! pattern the module's existing round-trip tests use.
//! Test: this file.

use super::super::providers::OllamaProvider;
use super::super::wire::ChatRequestWire;
use super::*;
use crate::chat::{ChatEvent, ChatProvider, SamplingParams};
use tokio::sync::mpsc;

// ── Pure decoders ─────────────────────────────────────────────────────────────

#[test]
fn error_message_reads_numeric_code() {
    let v = serde_json::json!({"message": "rate limited", "code": 429});
    assert_eq!(error_message_from_frame(&v), "429: rate limited");
}

#[test]
fn error_message_reads_string_code() {
    let v = serde_json::json!({"message": "no credits", "code": "insufficient_quota"});
    assert_eq!(
        error_message_from_frame(&v),
        "insufficient_quota: no credits"
    );
}

#[test]
fn error_message_falls_back_without_code() {
    assert_eq!(
        error_message_from_frame(&serde_json::json!({"message": "boom"})),
        "boom"
    );
    assert_eq!(
        error_message_from_frame(&serde_json::json!({})),
        "provider streaming error"
    );
    assert_eq!(
        error_message_from_frame(&serde_json::json!("upstream exploded")),
        "upstream exploded"
    );
}

#[test]
fn content_type_guard_matches_sse() {
    use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};

    let mut sse = HeaderMap::new();
    sse.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    assert!(is_event_stream(&sse));

    let mut json = HeaderMap::new();
    json.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    assert!(!is_event_stream(&json));

    assert!(!is_event_stream(&HeaderMap::new()));
}

/// An in-band error frame must terminate the stream with `Error`, not `Done`.
#[tokio::test]
async fn error_frame_emits_error_event() {
    let (tx, mut rx) = mpsc::channel::<ChatEvent>(8);
    let mut acc = ToolCallAccumulator::default();

    let flow = handle_line(
        "data: {\"error\":{\"message\":\"rate limited\",\"code\":429}}\n",
        &mut acc,
        &tx,
    )
    .await;

    assert_eq!(
        flow,
        Flow::Failed("429: rate limited".to_string()),
        "an error frame must report FAILED, not a plain stop — the caller has \
         to know to return Err"
    );
    drop(tx);
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    assert_eq!(events.len(), 1, "expected exactly one event: {events:?}");
    match &events[0] {
        ChatEvent::Error(msg) => assert_eq!(msg, "429: rate limited"),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// A malformed frame that is NOT an error stays best-effort-skipped, so one bad
/// keep-alive cannot kill an otherwise healthy stream.
#[tokio::test]
async fn malformed_non_error_frame_is_skipped() {
    let (tx, mut rx) = mpsc::channel::<ChatEvent>(8);
    let mut acc = ToolCallAccumulator::default();

    assert_eq!(
        handle_line("data: {not json\n", &mut acc, &tx).await,
        Flow::Continue
    );
    assert_eq!(
        handle_line(": keep-alive\n", &mut acc, &tx).await,
        Flow::Continue
    );
    assert_eq!(handle_line("\n", &mut acc, &tx).await, Flow::Continue);

    drop(tx);
    assert!(rx.recv().await.is_none(), "no events expected");
}

// ── Transport-level guards ────────────────────────────────────────────────────

/// Serve one HTTP response over a throwaway TCP listener and stream it through
/// an `OllamaProvider`, returning every event the pump produced plus the pump's
/// own `Result`.
async fn pump_response(headers: &str, body: &str) -> (Vec<ChatEvent>, anyhow::Result<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let response = format!("HTTP/1.1 200 OK\r\n{headers}\r\nConnection: close\r\n\r\n{body}");

    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let provider = OllamaProvider::new(base, "test-model");
    let (tx, mut rx) = mpsc::channel::<ChatEvent>(16);
    let handle = tokio::spawn(async move {
        provider
            .chat_stream(
                vec![crate::ChatMessage {
                    role: "user".into(),
                    content: "hi".into(),
                    tool_call_id: None,
                    tool_calls: None,
                }],
                vec![],
                tx,
            )
            .await
    });

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    (events, handle.await.expect("pump task panicked"))
}

/// The headline #3757 case: content arrives, then the socket dies mid-frame.
/// The partial text must NOT be capped with a `Done` that makes it look whole.
#[tokio::test]
async fn truncated_stream_errors_instead_of_done() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial answ\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"er that never fin",
    );
    let (events, result) = pump_response("Content-Type: text/event-stream", body).await;

    assert!(
        result.is_err(),
        "a truncated stream must return Err, got {result:?}"
    );
    assert!(
        matches!(events.first(), Some(ChatEvent::Delta(d)) if d == "partial answ"),
        "the complete frame must still be delivered: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, ChatEvent::Done)),
        "a truncated stream must not emit Done: {events:?}"
    );
    match events.last() {
        Some(ChatEvent::Error(msg)) => assert!(
            msg.contains("incomplete SSE frame"),
            "unexpected error text: {msg}"
        ),
        other => panic!("expected a terminal Error, got {other:?}"),
    }
}

/// EOF on a frame boundary without `[DONE]` is a NORMAL finish — not every
/// provider sends the sentinel, so this must stay a clean `Done`.
#[tokio::test]
async fn clean_eof_without_done_sentinel_still_completes() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"all of it\"}}]}\n\n";
    let (events, result) = pump_response("Content-Type: text/event-stream", body).await;

    assert!(result.is_ok(), "clean EOF must not error: {result:?}");
    assert!(matches!(events.first(), Some(ChatEvent::Delta(d)) if d == "all of it"));
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
}

/// A gateway that strips streaming returns a buffered completion at 200. The
/// answer must be replayed, not silently dropped as zero deltas + `Done`.
#[tokio::test]
async fn non_sse_json_body_is_replayed() {
    let body = r#"{"choices":[{"message":{"content":"buffered answer"}}]}"#;
    let (events, result) = pump_response("Content-Type: application/json", body).await;

    assert!(result.is_ok(), "degrade path must succeed: {result:?}");
    assert!(
        matches!(events.first(), Some(ChatEvent::Delta(d)) if d == "buffered answer"),
        "expected the buffered content replayed: {events:?}"
    );
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
}

/// A non-SSE body with nothing renderable is the silent-empty-answer case.
#[tokio::test]
async fn non_sse_body_without_content_errors() {
    let (events, result) =
        pump_response("Content-Type: text/html", "<html>bad gateway</html>").await;

    assert!(result.is_err(), "unusable body must return Err: {result:?}");
    match events.last() {
        Some(ChatEvent::Error(msg)) => assert!(
            msg.contains("non-SSE"),
            "error should name the non-SSE body: {msg}"
        ),
        other => panic!("expected a terminal Error, got {other:?}"),
    }
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Done)));
}

/// An in-band error object in a buffered body must surface too.
#[tokio::test]
async fn non_sse_error_object_surfaces() {
    let body = r#"{"error":{"message":"no credits","code":"insufficient_quota"}}"#;
    let (events, result) = pump_response("Content-Type: application/json", body).await;

    assert!(result.is_err(), "error body must return Err: {result:?}");
    match events.last() {
        Some(ChatEvent::Error(msg)) => assert_eq!(msg, "insufficient_quota: no credits"),
        other => panic!("expected a terminal Error, got {other:?}"),
    }
}

/// Right frames, wrong `Content-Type`: the guard must not destroy a valid
/// stream from a server that mislabels its SSE body.
#[tokio::test]
async fn mislabelled_sse_body_is_decoded() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"still works\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (events, result) = pump_response("Content-Type: application/json", body).await;

    assert!(
        result.is_ok(),
        "mislabelled SSE must still decode: {result:?}"
    );
    assert!(matches!(events.first(), Some(ChatEvent::Delta(d)) if d == "still works"));
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
}

/// The contract this module documents is that a failure surfaces on BOTH
/// channels. An earlier revision emitted the `Error` event but still returned
/// `Ok(())`, so a consumer that only joins the pump task saw success.
#[tokio::test]
async fn error_frame_returns_err() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        "data: {\"error\":{\"message\":\"rate limited\",\"code\":429}}\n\n",
    );
    let (events, result) = pump_response("Content-Type: text/event-stream", body).await;

    let err = result.expect_err("a mid-stream error frame must return Err");
    assert!(
        err.to_string().contains("429: rate limited"),
        "the Err must carry the provider's message: {err}"
    );
    assert!(
        matches!(events.first(), Some(ChatEvent::Delta(d)) if d == "partial"),
        "content before the error is still delivered: {events:?}"
    );
    match events.last() {
        Some(ChatEvent::Error(msg)) => assert_eq!(msg, "429: rate limited"),
        other => panic!("expected a terminal Error event, got {other:?}"),
    }
    assert!(
        !events.iter().any(|e| matches!(e, ChatEvent::Done)),
        "an errored stream must not also emit Done: {events:?}"
    );
}

/// A final frame missing only its trailing newline is COMPLETE, not truncated.
/// Failing it would flip a working stream to an error and cost the caller a
/// second full blocking LLM call.
#[tokio::test]
async fn unterminated_done_sentinel_completes_cleanly() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"complete\"}}]}\n\n",
        "data: [DONE]",
    );
    let (events, result) = pump_response("Content-Type: text/event-stream", body).await;

    assert!(
        result.is_ok(),
        "an unterminated [DONE] is a clean finish: {result:?}"
    );
    assert!(matches!(events.first(), Some(ChatEvent::Delta(d)) if d == "complete"));
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
}

/// Same for a complete JSON frame whose trailing newline never arrived.
#[tokio::test]
async fn unterminated_complete_frame_is_not_a_truncation() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"first \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"second\"}}]}",
    );
    let (events, result) = pump_response("Content-Type: text/event-stream", body).await;

    assert!(
        result.is_ok(),
        "a whole final frame is not a truncation: {result:?}"
    );
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Delta(d) => Some(d.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["first ", "second"], "both frames must arrive");
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
}

/// `is_complete_frame` is the truncation discriminator; pin its edges.
#[test]
fn complete_frame_detection_edges() {
    assert!(is_complete_frame("data: [DONE]"));
    assert!(is_complete_frame("data: {\"choices\":[]}"));
    assert!(
        is_complete_frame(": keep-alive"),
        "a comment has no payload"
    );
    assert!(is_complete_frame(""));
    assert!(
        !is_complete_frame("data: {\"choices\":[{\"delta\":{\"content\":\"cut"),
        "a severed JSON payload is a truncation"
    );
    assert!(
        !is_complete_frame("dat"),
        "a severed field name is a truncation"
    );
}

/// A provider that always includes an `error` key sends `"error": null` on
/// healthy frames — that must not be read as a failure.
#[tokio::test]
async fn null_error_key_is_not_a_failure() {
    let body = concat!(
        "data: {\"error\":null,\"choices\":[{\"delta\":{\"content\":\"healthy\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (events, result) = pump_response("Content-Type: text/event-stream", body).await;

    assert!(
        result.is_ok(),
        "a null error key must not fail the stream: {result:?}"
    );
    assert!(matches!(events.first(), Some(ChatEvent::Delta(d)) if d == "healthy"));
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
}

/// The same null guard on the buffered (non-SSE) body path.
#[tokio::test]
async fn null_error_key_in_buffered_body_is_not_a_failure() {
    let body = r#"{"error":null,"choices":[{"message":{"content":"buffered"}}]}"#;
    let (events, result) = pump_response("Content-Type: application/json", body).await;

    assert!(result.is_ok(), "null error in a buffered body: {result:?}");
    assert!(matches!(events.first(), Some(ChatEvent::Delta(d)) if d == "buffered"));
}

// ── #3758: request-body parity ────────────────────────────────────────────────

/// The streaming request must carry the same sampling knobs the blocking path
/// sends, and must omit them entirely when the caller supplies none.
#[test]
fn sampling_params_serialize_into_request_body() {
    let messages: Vec<crate::ChatMessage> = Vec::new();
    let sampling = SamplingParams {
        temperature: Some(0.2),
        max_tokens: Some(4096),
        stop: vec!["```\n\n".to_string()],
    };
    let body = ChatRequestWire {
        model: "m",
        messages: &messages,
        stream: true,
        tools: None,
        temperature: sampling.temperature,
        max_tokens: sampling.max_tokens,
        stop: sampling.stop_slice(),
    };
    let v = serde_json::to_value(&body).unwrap();
    // `temperature` is f32 — the same width the blocking (async-openai) builder
    // uses — so it widens to the identical JSON literal on both paths. Compare
    // with a tolerance rather than pinning the f32→f64 expansion.
    let temperature = v["temperature"].as_f64().expect("temperature must be sent");
    assert!(
        (temperature - 0.2).abs() < 1e-6,
        "temperature must round-trip: {temperature}"
    );
    assert_eq!(v["max_tokens"], 4096);
    assert_eq!(v["stop"][0], "```\n\n");
    assert_eq!(v["stream"], true);
}

#[test]
fn default_sampling_omits_fields() {
    let sampling = SamplingParams::default();
    assert!(
        sampling.stop_slice().is_none(),
        "empty stop must be omitted"
    );

    let messages: Vec<crate::ChatMessage> = Vec::new();
    let body = ChatRequestWire {
        model: "m",
        messages: &messages,
        stream: true,
        tools: None,
        temperature: sampling.temperature,
        max_tokens: sampling.max_tokens,
        stop: sampling.stop_slice(),
    };
    let v = serde_json::to_value(&body).unwrap();
    let obj = v.as_object().unwrap();
    // Byte-identical to the pre-#3758 body: no sampling keys at all.
    assert!(!obj.contains_key("temperature"), "got {v}");
    assert!(!obj.contains_key("max_tokens"), "got {v}");
    assert!(!obj.contains_key("stop"), "got {v}");
    assert!(!obj.contains_key("tools"), "got {v}");
}
