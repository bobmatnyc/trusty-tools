//! SSE-decoder + stream-adapter unit tests for [`super`] (epic #3696, Gap B).
//!
//! Why: the streaming parser is the load-bearing piece of the demo — it must be
//! correct across the real-world nasties (chunks that split mid-token or
//! mid-codepoint, keep-alive comments, a usage-only final frame, in-band error
//! chunks, EOF without `[DONE]`). These tests pin each against recorded/synthetic
//! OpenAI/OpenRouter chunk fixtures with NO live network.
//! What: drives [`super::SseDecoder`] directly with byte slices and drives
//! [`super::decode_event_stream`]/[`super::buffered_stream`] with in-memory
//! streams, asserting the exact ordered event output.
//! Test: this file is the test module.

use super::*;
use futures_util::StreamExt;

/// Collect every event a decoder produces from feeding one full byte buffer,
/// then flushing EOF. Unwraps each event (panicking on an `Err`) — error paths
/// use `feed` directly and inspect the `Result`.
fn drive(bytes: &[u8]) -> Vec<ChatStreamEvent> {
    let mut dec = SseDecoder::new();
    let mut out: Vec<ChatStreamEvent> = dec
        .feed(bytes)
        .into_iter()
        .map(|r| r.expect("no error expected"))
        .collect();
    if let Some(ev) = dec.finish() {
        out.push(ev);
    }
    out
}

/// A canonical OpenAI/OpenRouter content stream: a role frame, two content
/// frames, a finish frame, then `[DONE]`.
const HAPPY: &str = "\
data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

/// Why: the base case — content fragments must surface in order, followed by
/// exactly one terminal `Done` carrying the finish reason.
/// Test: itself.
#[test]
fn decode_yields_deltas_then_done() {
    let events = drive(HAPPY.as_bytes());
    assert_eq!(
        events,
        vec![
            ChatStreamEvent::Delta("Hel".into()),
            ChatStreamEvent::Delta("lo".into()),
            ChatStreamEvent::Done(StreamCompletion {
                finish_reason: Some(StopReason::Stop),
                usage: Usage::default(),
            }),
        ]
    );
}

/// Why: the socket splits at arbitrary byte boundaries; feeding the SAME stream
/// one byte at a time must produce the IDENTICAL event sequence as feeding it
/// whole. This is the split-mid-token guarantee.
/// Test: itself.
#[test]
fn decode_handles_split_chunks() {
    let mut dec = SseDecoder::new();
    let mut events: Vec<ChatStreamEvent> = Vec::new();
    for b in HAPPY.as_bytes() {
        for r in dec.feed(&[*b]) {
            events.push(r.expect("no error"));
        }
    }
    if let Some(ev) = dec.finish() {
        events.push(ev);
    }
    assert_eq!(events, drive(HAPPY.as_bytes()));
}

/// Why: a multibyte UTF-8 codepoint can be split across two socket chunks; the
/// byte-buffering decoder must reassemble it rather than dropping the frame (the
/// legacy per-chunk `from_utf8` bug). The euro sign `€` is 3 bytes (E2 82 AC).
/// Test: itself.
#[test]
fn decode_partial_utf8_across_chunks() {
    let frame = "data: {\"choices\":[{\"delta\":{\"content\":\"€\"}}]}\n\n";
    let bytes = frame.as_bytes();
    // Split at every interior byte offset; each split must still decode "€".
    for cut in 1..bytes.len() {
        let mut dec = SseDecoder::new();
        let mut events: Vec<ChatStreamEvent> = Vec::new();
        for r in dec.feed(&bytes[..cut]) {
            events.push(r.expect("no error"));
        }
        for r in dec.feed(&bytes[cut..]) {
            events.push(r.expect("no error"));
        }
        assert_eq!(
            events,
            vec![ChatStreamEvent::Delta("€".into())],
            "split at byte {cut} lost the codepoint"
        );
    }
}

/// Why: SSE keep-alives arrive as `:`-prefixed comment lines and blank lines;
/// they must be silently ignored, not parsed or errored.
/// Test: itself.
#[test]
fn decode_tolerates_keepalives() {
    let stream = "\
: OPENROUTER PROCESSING\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
: ping\n\n\
data: [DONE]\n\n";
    let events = drive(stream.as_bytes());
    assert_eq!(
        events,
        vec![
            ChatStreamEvent::Delta("hi".into()),
            ChatStreamEvent::Done(StreamCompletion {
                finish_reason: None,
                usage: Usage::default(),
            }),
        ]
    );
}

/// Why: with `stream_options.include_usage`, the provider sends a usage-only
/// frame (empty choices) right before `[DONE]`; that usage must land in the
/// terminal event.
/// Test: itself.
#[test]
fn decode_carries_usage_in_terminal() {
    let stream = "\
data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":3,\"cost\":0.002}}\n\n\
data: [DONE]\n\n";
    let events = drive(stream.as_bytes());
    let ChatStreamEvent::Done(done) = events.last().expect("has terminal") else {
        panic!("last event must be Done, got {:?}", events.last());
    };
    assert_eq!(done.usage.prompt_tokens, 11);
    assert_eq!(done.usage.completion_tokens, 3);
    assert_eq!(done.usage.cost_usd, Some(0.002));
}

/// Why: OpenRouter can report a failure as an in-band `data:` error chunk rather
/// than a non-2xx status; the decoder must surface it as a terminal `Err` (with
/// the reported code mapped to an API error) and stop.
/// Test: itself.
#[test]
fn decode_surfaces_error_chunk() {
    let mut dec = SseDecoder::new();
    let results = dec.feed(
        b"data: {\"error\":{\"message\":\"rate limited\",\"code\":429}}\n\n",
    );
    assert_eq!(results.len(), 1);
    match &results[0] {
        Err(InferenceError::Api { status, body }) => {
            assert_eq!(*status, 429);
            assert_eq!(body, "rate limited");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
    // Post-error input is ignored (no second terminal).
    assert!(dec.feed(b"data: [DONE]\n\n").is_empty());
    assert!(dec.finish().is_none());
}

/// Why: a malformed JSON frame must be skipped best-effort (not abort the whole
/// stream) so one bad frame never drops the rest of the completion.
/// Test: itself.
#[test]
fn decode_skips_malformed_frame() {
    let stream = "\
data: {not valid json\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
data: [DONE]\n\n";
    let events = drive(stream.as_bytes());
    assert_eq!(
        events,
        vec![
            ChatStreamEvent::Delta("ok".into()),
            ChatStreamEvent::Done(StreamCompletion {
                finish_reason: None,
                usage: Usage::default(),
            }),
        ]
    );
}

/// Why: not every provider emits `[DONE]`; a clean EOF must still yield exactly
/// one terminal event so the consumer loop ends normally.
/// Test: itself.
#[test]
fn decode_eof_without_done_still_terminates() {
    let stream = "data: {\"choices\":[{\"delta\":{\"content\":\"bye\"}},{\"delta\":{}}]}\n\n";
    // Note the finish frame is absent; EOF drives the terminal.
    let events = drive(stream.as_bytes());
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], ChatStreamEvent::Delta("bye".into()));
    assert!(matches!(events[1], ChatStreamEvent::Done(_)));
}

/// Why: `[DONE]` yields the terminal event; a subsequent `finish()` at EOF must
/// NOT emit a second one.
/// Test: itself.
#[test]
fn decode_done_then_finish_is_single_terminal() {
    let mut dec = SseDecoder::new();
    let events = dec.feed(b"data: [DONE]\n\n");
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Ok(ChatStreamEvent::Done(_))));
    assert!(dec.finish().is_none());
}

/// Why: tool calls stream across frames — id+name on the first, argument
/// fragments after — and the decoder must forward each fragment with its slot
/// index so the consumer can reassemble the call.
/// Test: itself.
#[test]
fn decode_accumulates_tool_call() {
    let stream = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"loc\\\":\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"SEA\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";
    let events = drive(stream.as_bytes());
    // Three tool-call fragments then the terminal.
    let tool_events: Vec<&ToolCallDelta> = events
        .iter()
        .filter_map(|e| match e {
            ChatStreamEvent::ToolCall(d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(tool_events.len(), 3);
    assert_eq!(tool_events[0].id.as_deref(), Some("call_1"));
    assert_eq!(tool_events[0].name.as_deref(), Some("get_weather"));
    // Reassembled argument text.
    let args: String = tool_events.iter().map(|d| d.arguments.clone()).collect();
    assert_eq!(args, "{\"loc\":\"SEA\"}");
    assert!(matches!(
        events.last(),
        Some(ChatStreamEvent::Done(StreamCompletion {
            finish_reason: Some(StopReason::ToolCalls),
            ..
        }))
    ));
}

/// Why: `decode_event_stream` must drive the decoder over a byte-chunk stream and
/// yield the same events end-to-end. Uses an in-memory stream (no network).
/// Test: itself.
#[tokio::test]
async fn decode_event_stream_end_to_end() {
    // Split the happy stream into three arbitrary byte chunks.
    let bytes = HAPPY.as_bytes();
    let third = bytes.len() / 3;
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![
        Ok(bytes[..third].to_vec()),
        Ok(bytes[third..2 * third].to_vec()),
        Ok(bytes[2 * third..].to_vec()),
    ];
    let byte_stream = futures_util::stream::iter(chunks);
    let events: Vec<ChatStreamEvent> = decode_event_stream(byte_stream)
        .map(|r| r.expect("no error"))
        .collect()
        .await;
    assert_eq!(
        events,
        vec![
            ChatStreamEvent::Delta("Hel".into()),
            ChatStreamEvent::Delta("lo".into()),
            ChatStreamEvent::Done(StreamCompletion {
                finish_reason: Some(StopReason::Stop),
                usage: Usage::default(),
            }),
        ]
    );
}

/// Why: a transport error mid-stream must surface as a terminal `Err` the caller
/// can classify, not be swallowed.
/// Test: itself.
#[tokio::test]
async fn decode_event_stream_surfaces_transport_error() {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![
        Ok(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n".to_vec()),
        Err(std::io::Error::other("connection reset")),
    ];
    let byte_stream = futures_util::stream::iter(chunks);
    let results: Vec<Result<ChatStreamEvent, InferenceError>> =
        decode_event_stream(byte_stream).collect().await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], Ok(ChatStreamEvent::Delta("hi".into())));
    assert!(matches!(results[1], Err(InferenceError::Transport(_))));
}

/// Why: the buffered fallback (the trait default) must replay a completed
/// response as one delta + a terminal event carrying its usage/stop reason.
/// Test: itself.
#[tokio::test]
async fn buffered_stream_replays_response() {
    let fixture = r#"{
      "id": "gen-1",
      "choices": [{"message": {"role": "assistant", "content": "Hello there"}, "finish_reason": "stop"}],
      "usage": {"prompt_tokens": 5, "completion_tokens": 2}
    }"#;
    let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
    let events: Vec<ChatStreamEvent> = buffered_stream(resp)
        .map(|r| r.expect("no error"))
        .collect()
        .await;
    assert_eq!(
        events,
        vec![
            ChatStreamEvent::Delta("Hello there".into()),
            ChatStreamEvent::Done(StreamCompletion {
                finish_reason: Some(StopReason::Stop),
                usage: Usage::new(5, 2, 0, 0),
            }),
        ]
    );
}
