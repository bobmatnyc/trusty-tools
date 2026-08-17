//! Regression coverage for #2272's bounded, in-turn inference retry policy.
//!
//! Why: transient provider failures must not restart a delegated engineer from
//! scratch, while retrying a stream after visible text would duplicate output.
//! What: scripted blocking and streaming adapters pin the three-attempt budget,
//! retry classifier, pre-exposure stream boundary, and enclosing run deadline.
//! Test: this module is the contract surface.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::stream;
use trusty_common::inference::{
    ChatStream, ChatStreamEvent, StopReason, StreamCompletion, ToolCallDelta, Usage,
};

use super::*;

enum BlockingAttempt {
    Error(InferenceError),
    Response(ChatResponse),
}

struct BlockingRetryLlm {
    attempts: Mutex<VecDeque<BlockingAttempt>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<ChatRequest>>,
    call_times: Mutex<Vec<tokio::time::Instant>>,
    first_call_delay: Option<Duration>,
}

impl BlockingRetryLlm {
    fn new(attempts: Vec<BlockingAttempt>) -> Self {
        Self {
            attempts: Mutex::new(attempts.into()),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            call_times: Mutex::new(Vec::new()),
            first_call_delay: None,
        }
    }

    fn with_first_call_delay(mut self, delay: Duration) -> Self {
        self.first_call_delay = Some(delay);
        self
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn call_times(&self) -> Vec<tokio::time::Instant> {
        self.call_times.lock().expect("call-time lock").clone()
    }
}

#[async_trait]
impl InferenceAdapter for BlockingRetryLlm {
    crate::llm::mock_adapter_identity!("mock-blocking-retry");

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.call_times
            .lock()
            .expect("call-time lock")
            .push(tokio::time::Instant::now());
        self.requests
            .lock()
            .expect("request lock")
            .push(request.clone());
        if call == 0
            && let Some(delay) = self.first_call_delay
        {
            tokio::time::sleep(delay).await;
        }
        match self
            .attempts
            .lock()
            .expect("attempt lock")
            .pop_front()
            .expect("scripted blocking attempt")
        {
            BlockingAttempt::Error(error) => Err(error),
            BlockingAttempt::Response(response) => Ok(response),
        }
    }
}

enum StreamAttempt {
    HandshakeError(InferenceError),
    Events(Vec<Result<ChatStreamEvent, InferenceError>>),
}

struct StreamingRetryLlm {
    attempts: Mutex<VecDeque<StreamAttempt>>,
    calls: AtomicUsize,
}

impl StreamingRetryLlm {
    fn new(attempts: Vec<StreamAttempt>) -> Self {
        Self {
            attempts: Mutex::new(attempts.into()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl InferenceAdapter for StreamingRetryLlm {
    crate::llm::mock_adapter_identity!("mock-streaming-retry");

    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        panic!("stream retry tests must not use the blocking adapter path")
    }

    async fn chat_stream(&self, _request: &ChatRequest) -> Result<ChatStream, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self
            .attempts
            .lock()
            .expect("attempt lock")
            .pop_front()
            .expect("scripted streaming attempt")
        {
            StreamAttempt::HandshakeError(error) => Err(error),
            StreamAttempt::Events(events) => Ok(Box::pin(stream::iter(events))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Message {
    turn_id: String,
    delta: String,
    done: bool,
}

struct RecordingMessageSink {
    messages: Mutex<Vec<Message>>,
}

impl RecordingMessageSink {
    fn new() -> Self {
        Self {
            messages: Mutex::new(Vec::new()),
        }
    }

    fn messages(&self) -> Vec<Message> {
        self.messages.lock().expect("message lock").clone()
    }

    /// Every delta the user could actually have read, in order.
    fn visible_text(&self) -> Vec<String> {
        self.messages()
            .into_iter()
            .filter(|message| !message.delta.is_empty())
            .map(|message| message.delta)
            .collect()
    }
}

#[async_trait]
impl ToolEventSink for RecordingMessageSink {
    async fn tool_started(&self, _: &str, _: &str, _: &str, _: &str, _: &str) {}

    async fn tool_finished(&self, _: &str, _: &str, _: &str, _: &str, _: bool, _: &str) {}

    async fn tool_error(&self, _: &str, _: &str, _: &str, _: &str, _: &str) {}

    async fn agent_message(&self, _: &str, _: &str, turn_id: &str, delta: &str, done: bool) {
        self.messages.lock().expect("message lock").push(Message {
            turn_id: turn_id.to_string(),
            delta: delta.to_string(),
            done,
        });
    }
}

fn response(text: &str) -> ChatResponse {
    serde_json::from_value(stop_response(text)).expect("valid response")
}

fn done() -> ChatStreamEvent {
    ChatStreamEvent::Done(StreamCompletion {
        finish_reason: Some(StopReason::Stop),
        usage: Usage {
            prompt_tokens: 7,
            completion_tokens: 5,
            ..Usage::default()
        },
    })
}

fn empty_registry() -> Arc<ToolRegistry> {
    Arc::new(ToolRegistry::new())
}

#[tokio::test(start_paused = true)]
async fn blocking_retryable_failure_then_success_is_one_logical_turn() {
    let llm = Arc::new(BlockingRetryLlm::new(vec![
        BlockingAttempt::Error(InferenceError::Transport("first".into())),
        BlockingAttempt::Response(response("done")),
    ]));

    let output = AgentLoop::new(AgentLoopConfig::default(), llm.clone(), empty_registry())
        .run("system", "task")
        .await
        .expect("retry should recover");

    assert_eq!(llm.calls(), 2);
    assert_eq!(output.content, "done");
    assert_eq!(output.usage.prompt_tokens, 7);
    assert_eq!(output.usage.completion_tokens, 5);
    let requests = llm.requests.lock().expect("request lock");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        serde_json::to_value(&requests[0]).expect("serialize request"),
        serde_json::to_value(&requests[1]).expect("serialize request")
    );
}

#[tokio::test(start_paused = true)]
async fn blocking_retry_exhaustion_returns_third_original_error() {
    let llm = Arc::new(BlockingRetryLlm::new(vec![
        BlockingAttempt::Error(InferenceError::Transport("first".into())),
        BlockingAttempt::Error(InferenceError::Api {
            status: 429,
            body: "second".into(),
        }),
        BlockingAttempt::Error(InferenceError::Api {
            status: 503,
            body: "third-marker".into(),
        }),
    ]));

    let error = AgentLoop::new(AgentLoopConfig::default(), llm.clone(), empty_registry())
        .run("system", "task")
        .await
        .expect_err("third retryable failure must escape");

    assert_eq!(llm.calls(), 3);
    assert!(matches!(
        error,
        AgentLoopError::Llm(InferenceError::Api { status: 503, body })
            if body == "third-marker"
    ));
}

#[tokio::test(start_paused = true)]
async fn blocking_retry_delays_are_100_then_200_milliseconds() {
    let llm = Arc::new(BlockingRetryLlm::new(vec![
        BlockingAttempt::Error(InferenceError::Transport("first".into())),
        BlockingAttempt::Error(InferenceError::Transport("second".into())),
        BlockingAttempt::Response(response("done")),
    ]));

    AgentLoop::new(AgentLoopConfig::default(), llm.clone(), empty_registry())
        .run("system", "task")
        .await
        .expect("third attempt should recover");

    let call_times = llm.call_times();
    assert_eq!(call_times.len(), 3);
    assert_eq!(call_times[1] - call_times[0], Duration::from_millis(100));
    assert_eq!(call_times[2] - call_times[1], Duration::from_millis(200));
}

#[tokio::test(start_paused = true)]
async fn blocking_non_retryable_error_is_attempted_once() {
    let failures = vec![
        InferenceError::Api {
            status: 400,
            body: "bad request".into(),
        },
        InferenceError::Api {
            status: 401,
            body: "unauthorized".into(),
        },
        InferenceError::MissingConfig("missing model".into()),
        InferenceError::Deserialise {
            message: "bad json".into(),
            body: "not-json".into(),
        },
    ];

    for failure in failures {
        let llm = Arc::new(BlockingRetryLlm::new(vec![BlockingAttempt::Error(failure)]));
        let result = AgentLoop::new(AgentLoopConfig::default(), llm.clone(), empty_registry())
            .run("system", "task")
            .await;
        assert!(matches!(result, Err(AgentLoopError::Llm(_))));
        assert_eq!(llm.calls(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn stream_handshake_retryable_failure_then_success_recovers() {
    let llm = Arc::new(StreamingRetryLlm::new(vec![
        StreamAttempt::HandshakeError(InferenceError::Transport("handshake".into())),
        StreamAttempt::Events(vec![Ok(ChatStreamEvent::Delta("done".into())), Ok(done())]),
    ]));
    let sink = Arc::new(RecordingMessageSink::new());

    let output = AgentLoop::new(AgentLoopConfig::default(), llm.clone(), empty_registry())
        .with_tool_event_sink(sink.clone())
        .run("system", "task")
        .await
        .expect("handshake retry should recover");

    let messages = sink.messages();
    assert_eq!(llm.calls(), 2);
    assert_eq!(output.content, "done");
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.delta.as_str(), message.done))
            .collect::<Vec<_>>(),
        vec![("done", false), ("", true)]
    );
    assert_eq!(messages[0].turn_id, messages[1].turn_id);
}

#[tokio::test(start_paused = true)]
async fn stream_failure_before_exposure_discards_partial_assembly_before_retry() {
    let llm = Arc::new(StreamingRetryLlm::new(vec![
        StreamAttempt::Events(vec![
            Ok(ChatStreamEvent::Delta(String::new())),
            Ok(ChatStreamEvent::ToolCall(ToolCallDelta {
                index: 0,
                id: Some("discarded".into()),
                name: Some("discarded_tool".into()),
                arguments: "{}".into(),
            })),
            Err(InferenceError::Transport("stream body".into())),
        ]),
        StreamAttempt::Events(vec![Ok(ChatStreamEvent::Delta("clean".into())), Ok(done())]),
    ]));
    let sink = Arc::new(RecordingMessageSink::new());

    let output = AgentLoop::new(AgentLoopConfig::default(), llm.clone(), empty_registry())
        .with_tool_event_sink(sink.clone())
        .run("system", "task")
        .await
        .expect("unexposed body failure should recover");

    assert_eq!(llm.calls(), 2);
    assert_eq!(output.content, "clean");
    assert_eq!(sink.visible_text(), vec!["clean".to_string()]);
    assert_eq!(sink.messages().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn stream_failure_after_text_delta_is_not_retried() {
    let llm = Arc::new(StreamingRetryLlm::new(vec![StreamAttempt::Events(vec![
        Ok(ChatStreamEvent::Delta("visible".into())),
        Err(InferenceError::Transport("after exposure".into())),
    ])]));
    let sink = Arc::new(RecordingMessageSink::new());

    let error = AgentLoop::new(AgentLoopConfig::default(), llm.clone(), empty_registry())
        .with_tool_event_sink(sink.clone())
        .run("system", "task")
        .await
        .expect_err("an exposed stream must not retry");

    assert!(matches!(
        error,
        AgentLoopError::Llm(InferenceError::Transport(_))
    ));
    assert_eq!(llm.calls(), 1);
    assert_eq!(
        sink.messages()
            .iter()
            .map(|message| (message.delta.as_str(), message.done))
            .collect::<Vec<_>>(),
        vec![("visible", false)]
    );
}

/// Why (#2272): the exposure flag is reset per attempt, so "already retried
/// once" and "already showed text" are independent facts. A policy that tracked
/// only remaining budget — or that hoisted exposure out of the attempt loop —
/// would still have a retry available here and would replay `visible` a second
/// time. This is the path the original design note did not cover: exposure
/// happening on an attempt AFTER the first, with budget left over.
/// What: attempt 1 fails before showing anything (a legitimate retry), attempt 2
/// shows text and then fails retryably with one attempt still unspent.
/// Test: itself.
#[tokio::test(start_paused = true)]
async fn stream_exposed_on_a_later_attempt_is_not_retried_again() {
    let llm = Arc::new(StreamingRetryLlm::new(vec![
        StreamAttempt::Events(vec![Err(InferenceError::Transport(
            "before exposure".into(),
        ))]),
        StreamAttempt::Events(vec![
            Ok(ChatStreamEvent::Delta("visible".into())),
            Err(InferenceError::Transport("after exposure".into())),
        ]),
        // A third attempt is inside the budget; reaching it is the bug.
        StreamAttempt::Events(vec![
            Ok(ChatStreamEvent::Delta("visible".into())),
            Ok(done()),
        ]),
    ]));
    let sink = Arc::new(RecordingMessageSink::new());

    let error = AgentLoop::new(AgentLoopConfig::default(), llm.clone(), empty_registry())
        .with_tool_event_sink(sink.clone())
        .run("system", "task")
        .await
        .expect_err("an exposed stream must not retry even with budget left");

    assert!(matches!(
        error,
        AgentLoopError::Llm(InferenceError::Transport(_))
    ));
    assert_eq!(llm.calls(), 2);
    assert_eq!(sink.visible_text(), vec!["visible".to_string()]);
}

/// Why (#2272): a tool-only turn emits no `agent_message` at all, so "nothing
/// reached the sink" must not be read as "nothing was assembled". Retrying such
/// a stream is safe precisely because it was invisible — this pins that the
/// retry does happen and that the recovered turn still dispatches its tool once.
/// What: attempt 1 streams a complete tool call and then fails; attempt 2
/// streams a plain text turn. The user sees only the second attempt's text.
/// Test: itself.
#[tokio::test(start_paused = true)]
async fn tool_only_stream_failure_retries_without_emitting_anything() {
    let llm = Arc::new(StreamingRetryLlm::new(vec![
        StreamAttempt::Events(vec![
            Ok(ChatStreamEvent::ToolCall(ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name: Some("never_dispatched".into()),
                arguments: "{}".into(),
            })),
            Err(InferenceError::Api {
                status: 503,
                body: "mid-tool-call".into(),
            }),
        ]),
        StreamAttempt::Events(vec![
            Ok(ChatStreamEvent::Delta("recovered".into())),
            Ok(done()),
        ]),
    ]));
    let sink = Arc::new(RecordingMessageSink::new());

    let output = AgentLoop::new(AgentLoopConfig::default(), llm.clone(), empty_registry())
        .with_tool_event_sink(sink.clone())
        .run("system", "task")
        .await
        .expect("an invisible tool-only stream is safe to retry");

    assert_eq!(llm.calls(), 2);
    assert_eq!(output.content, "recovered");
    assert_eq!(sink.visible_text(), vec!["recovered".to_string()]);
    assert_eq!(
        sink.messages()
            .iter()
            .filter(|message| message.done)
            .count(),
        1,
        "exactly one terminal done: true for the surviving attempt"
    );
}

/// Why (#2272): the backoff sleeps are deliberately NOT deducted from a
/// separate budget — `run_with_transcript` already wraps the whole loop in
/// `tokio::time::timeout`, and that stays the single outer bound. This pins
/// that a retry cannot push a run past its configured deadline.
/// What: the first call burns 950 ms of a 1 s deadline and then fails
/// retryably; the 100 ms backoff crosses the deadline, so `Timeout` wins and
/// the second attempt never issues.
/// Test: itself.
#[tokio::test(start_paused = true)]
async fn retry_backoff_is_inside_run_deadline() {
    let llm = Arc::new(
        BlockingRetryLlm::new(vec![
            BlockingAttempt::Error(InferenceError::Transport("late failure".into())),
            BlockingAttempt::Response(response("must not run")),
        ])
        .with_first_call_delay(Duration::from_millis(950)),
    );
    let config = AgentLoopConfig {
        timeout_secs: 1,
        ..AgentLoopConfig::default()
    };

    let error = AgentLoop::new(config, llm.clone(), empty_registry())
        .run("system", "task")
        .await
        .expect_err("the outer deadline must expire during retry backoff");

    assert!(matches!(
        error,
        AgentLoopError::Timeout {
            timeout_secs: 1,
            ..
        }
    ));
    assert_eq!(llm.calls(), 1);
}
