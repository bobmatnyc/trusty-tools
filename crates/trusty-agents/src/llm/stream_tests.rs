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

use anyhow::anyhow;
use async_trait::async_trait;
use tokio::sync::mpsc::{self, Sender};
use trusty_common::ChatMessage;
use trusty_common::chat::{ChatEvent, ChatProvider, ToolCall, ToolDef};

use super::*;

/// Controllable fake `ChatProvider` for exercising `stream_with_provider`'s
/// error branches without a live API key: it replays a fixed script of
/// `ChatEvent`s into `tx`, then returns `ret` (or panics first if `panic`).
struct FakeProvider {
    script: Vec<ChatEvent>,
    ret: Result<(), String>,
    panic: bool,
}

impl FakeProvider {
    fn ok(script: Vec<ChatEvent>) -> Self {
        Self {
            script,
            ret: Ok(()),
            panic: false,
        }
    }
}

#[async_trait]
impl ChatProvider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }
    fn model(&self) -> &str {
        "fake-model"
    }
    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDef>,
        tx: Sender<ChatEvent>,
    ) -> anyhow::Result<()> {
        if self.panic {
            panic!("fake provider panic");
        }
        for ev in &self.script {
            let _ = tx.send(ev.clone()).await;
        }
        self.ret.clone().map_err(|m| anyhow!(m))
    }
}

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

// --- stream_with_provider error-branch coverage (item 5, critic MEDIUM) ---

const FAST: Duration = Duration::ZERO;

#[tokio::test]
async fn stream_with_provider_propagates_error_frame() {
    // A mid-stream Error frame must surface as Err so the caller falls back.
    let provider = FakeProvider::ok(vec![
        ChatEvent::Delta("partial".into()),
        ChatEvent::Error("upstream exploded".into()),
    ]);
    let out = stream_with_provider(provider, vec![], "s1", "agent", FAST).await;
    assert!(out.is_err(), "error frame should fail the stream");
    assert!(out.unwrap_err().to_string().contains("upstream exploded"));
}

#[tokio::test]
async fn stream_with_provider_propagates_provider_err() {
    // The provider itself returning Err (e.g. HTTP 500) must propagate even if
    // some content was already streamed.
    let provider = FakeProvider {
        script: vec![ChatEvent::Delta("hi".into())],
        ret: Err("openrouter HTTP 500".into()),
        panic: false,
    };
    let out = stream_with_provider(provider, vec![], "s1", "agent", FAST).await;
    assert!(out.is_err());
    assert!(out.unwrap_err().to_string().contains("chat_stream failed"));
}

#[tokio::test]
async fn stream_with_provider_guards_empty_stream() {
    // A clean Done with no content and no tool calls is the silent-failure case
    // the guard must convert into Err so the blocking fallback engages.
    let provider = FakeProvider::ok(vec![ChatEvent::Done]);
    let out = stream_with_provider(provider, vec![], "s1", "agent", FAST).await;
    assert!(out.is_err(), "empty stream should be treated as failed");
    assert!(out.unwrap_err().to_string().contains("no content"));
}

#[tokio::test]
async fn stream_with_provider_propagates_panic() {
    let provider = FakeProvider {
        script: vec![],
        ret: Ok(()),
        panic: true,
    };
    let out = stream_with_provider(provider, vec![], "s1", "agent", FAST).await;
    assert!(out.is_err());
    assert!(out.unwrap_err().to_string().contains("panicked"));
}

#[tokio::test]
async fn stream_with_provider_returns_assembled_content() {
    // Positive control: a normal stream returns the assembled text.
    let provider = FakeProvider::ok(vec![
        ChatEvent::Delta("Hello, ".into()),
        ChatEvent::Delta("world".into()),
        ChatEvent::Done,
    ]);
    let out = stream_with_provider(provider, vec![], "s1", "agent", FAST)
        .await
        .expect("stream should succeed");
    assert_eq!(out.content, "Hello, world");
    assert!(out.error.is_none());
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
