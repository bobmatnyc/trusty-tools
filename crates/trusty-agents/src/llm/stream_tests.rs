//! Unit tests for the token-streaming batching core and capability gate.
//!
//! Why: `drive_delta_stream` is the pure heart of chat streaming; pinning its
//! flush cadence, terminal-marker contract, and assembled-text equality with a
//! mocked `ChatEvent` channel guarantees the GUI's incremental-append behavior
//! stays correct independent of any live provider.
//! What: Drives `drive_delta_stream` with a bounded channel we fill ahead of
//! time, capturing every `emit(text, done)` into a shared `Vec`.
//! Test: this file.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use trusty_common::chat::{ChatEvent, ToolCall};

use super::*;

/// Feed `events` into a fresh channel and drive the stream, returning both the
/// captured `(text, done)` emissions and the assembled result.
async fn run(events: Vec<ChatEvent>, cadence: Duration) -> (Vec<(String, bool)>, StreamAssembly) {
    let (tx, rx) = mpsc::channel::<ChatEvent>(64);
    for ev in events {
        tx.send(ev).await.expect("send mock event");
    }
    drop(tx); // close so the driver's recv loop terminates even without Done

    let captured: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let assembly = drive_delta_stream(rx, cadence, move |text, done| {
        sink.lock().unwrap().push((text, done));
    })
    .await;

    let out = Arc::try_unwrap(captured).unwrap().into_inner().unwrap();
    (out, assembly)
}

#[tokio::test]
async fn drive_delta_stream_flushes_each_delta_at_zero_cadence() {
    let (emitted, assembly) = run(
        vec![
            ChatEvent::Delta("a".into()),
            ChatEvent::Delta("b".into()),
            ChatEvent::Done,
        ],
        Duration::ZERO,
    )
    .await;

    assert_eq!(
        emitted,
        vec![
            ("a".to_string(), false),
            ("b".to_string(), false),
            (String::new(), true),
        ]
    );
    assert_eq!(assembly.content, "ab");
    assert!(assembly.error.is_none());
}

#[tokio::test]
async fn drive_delta_stream_batches_under_cadence() {
    // A cadence longer than the whole test means no intermediate flush fires;
    // all three deltas coalesce into a single residual flush at Done.
    let (emitted, assembly) = run(
        vec![
            ChatEvent::Delta("a".into()),
            ChatEvent::Delta("b".into()),
            ChatEvent::Delta("c".into()),
            ChatEvent::Done,
        ],
        Duration::from_secs(3600),
    )
    .await;

    assert_eq!(
        emitted,
        vec![("abc".to_string(), false), (String::new(), true)]
    );
    assert_eq!(assembly.content, "abc");
}

#[tokio::test]
async fn drive_delta_stream_assembles_full_text() {
    let fragments = ["The ", "quick ", "brown ", "fox"];
    let events: Vec<ChatEvent> = fragments
        .iter()
        .map(|f| ChatEvent::Delta((*f).to_string()))
        .chain(std::iter::once(ChatEvent::Done))
        .collect();

    let (_emitted, assembly) = run(events, Duration::ZERO).await;
    assert_eq!(assembly.content, fragments.concat());
}

#[tokio::test]
async fn drive_delta_stream_surfaces_error() {
    let (emitted, assembly) = run(
        vec![
            ChatEvent::Delta("partial".into()),
            ChatEvent::Error("boom".into()),
            ChatEvent::Done, // must never be reached — Error breaks first
        ],
        Duration::ZERO,
    )
    .await;

    assert_eq!(assembly.error.as_deref(), Some("boom"));
    assert_eq!(assembly.content, "partial");
    // Even on error we still emit the terminal marker so the GUI finalizes.
    assert_eq!(emitted.last(), Some(&(String::new(), true)));
}

#[tokio::test]
async fn drive_delta_stream_collects_tool_calls() {
    let call = ToolCall {
        id: "call_1".into(),
        name: "get_weather".into(),
        arguments: "{}".into(),
    };
    let (_emitted, assembly) = run(
        vec![
            ChatEvent::Delta("hi".into()),
            ChatEvent::ToolCall(call.clone()),
            ChatEvent::Done,
        ],
        Duration::ZERO,
    )
    .await;

    assert_eq!(assembly.tool_calls.len(), 1);
    assert_eq!(assembly.tool_calls[0].name, "get_weather");
    assert_eq!(assembly.content, "hi");
}

#[test]
fn build_messages_shapes_roles() {
    let history = vec![ConversationTurn {
        user: "hello".into(),
        assistant: "hi there".into(),
    }];
    let messages = build_messages("SYS", &history, "how are you?");

    let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
    assert_eq!(messages[0].content, "SYS");
    assert_eq!(messages[3].content, "how are you?");
}

#[test]
fn streaming_supported_gates_on_provider() {
    // Bedrock routes through the AWS SDK, never the OpenRouter provider —
    // false regardless of the (default-on) kill switch.
    assert!(!streaming_supported(
        "bedrock/anthropic.claude-3-sonnet",
        false
    ));
    // Fireworks has its own base URL / credential.
    assert!(!streaming_supported("fireworks/llama-v3", false));
}

#[test]
fn streaming_supported_respects_anthropic_direct() {
    // Anthropic-direct bypasses the OpenRouter client entirely.
    assert!(!streaming_supported("anthropic/claude-3.5-sonnet", true));
}

#[test]
fn parse_streaming_flag_table() {
    assert!(parse_streaming_flag(None));
    assert!(parse_streaming_flag(Some("1")));
    assert!(parse_streaming_flag(Some("true")));
    assert!(parse_streaming_flag(Some("anything")));
    assert!(!parse_streaming_flag(Some("0")));
    assert!(!parse_streaming_flag(Some("false")));
    assert!(!parse_streaming_flag(Some(" OFF ")));
    assert!(!parse_streaming_flag(Some("No")));
}
