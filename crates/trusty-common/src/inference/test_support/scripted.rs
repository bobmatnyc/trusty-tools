//! [`ScriptedAdapter`] — the deterministic test-only inference adapter.
//!
//! Why: the configurator seam, the agent loop, and any consumer of
//! [`InferenceAdapter`] must be unit-testable WITHOUT real HTTP or credentials.
//! This is the in-memory double that makes that possible — and, in this
//! foundation ticket, the only [`InferenceAdapter`] implementation, so the
//! configurator can be exercised end-to-end before real adapters land in #2403.
//! It is shipped as public test support (not `cfg(test)`) so downstream crates'
//! integration tests can reuse it, mirroring the crate's `embedder-test-support`
//! precedent.
//! What: [`ScriptedAdapter`] answers [`InferenceAdapter::chat`] from a FIFO queue
//! of scripted outcomes; when the queue is empty it either echoes the last user
//! message (echo mode) or errors (strict mode). Capability introspection is
//! driven by the [`ProviderCapabilities`] it is constructed with.
//! Test: `crates/trusty-common/tests/inference_foundation.rs` drives it through
//! the trait and the configurator; inline `tests` cover echo + queue behaviour.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::inference::adapter::InferenceAdapter;
use crate::inference::error::InferenceError;
use crate::inference::registry::ProviderCapabilities;
use crate::inference::types::{
    AssistantMessage, ChatChoice, ChatRequest, ChatResponse, UsageBlock,
};

/// A deterministic [`InferenceAdapter`] backed by a scripted response queue.
///
/// Why: gives tests full control over what `chat` returns — a canned success, a
/// specific error, or an automatic echo — with zero network or credential
/// dependency.
/// What: holds a `name`, a [`ProviderCapabilities`] descriptor (drives the
/// introspection methods), an `echo` flag, and a `Mutex`-guarded FIFO of
/// scripted outcomes. `chat` pops the front; on an empty queue it echoes (echo
/// mode) or returns [`InferenceError::Provider`] (strict mode).
/// Test: `echo_reflects_last_user_message`, `queue_is_fifo`, `strict_mode_exhaustion_errors`.
pub struct ScriptedAdapter {
    name: String,
    capabilities: ProviderCapabilities,
    echo: bool,
    queue: Mutex<Vec<Result<ChatResponse, InferenceError>>>,
}

impl ScriptedAdapter {
    /// Construct a strict adapter: an empty queue errors on exhaustion.
    ///
    /// Why: tests that assert an exact sequence of responses want a hard failure
    /// (not a silent echo) if the code under test calls `chat` more than scripted.
    /// What: `echo = false`, empty queue, cloning `capabilities` into the adapter.
    /// Test: `strict_mode_exhaustion_errors`.
    pub fn new(name: impl Into<String>, capabilities: &ProviderCapabilities) -> Self {
        Self {
            name: name.into(),
            capabilities: *capabilities,
            echo: false,
            queue: Mutex::new(Vec::new()),
        }
    }

    /// Construct an echo adapter: an empty queue reflects the last user message.
    ///
    /// Why: the simplest possible "it works" double for wiring tests — a full
    /// request→response→usage cycle without scripting each turn.
    /// What: `echo = true`, empty queue.
    /// Test: `echo_reflects_last_user_message`.
    pub fn echo(name: impl Into<String>, capabilities: &ProviderCapabilities) -> Self {
        Self {
            name: name.into(),
            capabilities: *capabilities,
            echo: true,
            queue: Mutex::new(Vec::new()),
        }
    }

    /// Queue a scripted successful response (FIFO).
    ///
    /// Why: builder-style scripting for the exact-sequence tests.
    /// What: pushes `response` to the back of the queue; returns `self` for
    /// chaining.
    /// Test: `queue_is_fifo`.
    pub fn with_response(self, response: ChatResponse) -> Self {
        self.push(Ok(response));
        self
    }

    /// Queue a scripted error (FIFO).
    ///
    /// Why: lets tests drive the caller's retry/alarm handling deterministically.
    /// What: pushes `error` to the back of the queue; returns `self`.
    /// Test: exercised via the integration suite.
    pub fn with_error(self, error: InferenceError) -> Self {
        self.push(Err(error));
        self
    }

    /// Push an outcome onto the back of the queue.
    ///
    /// Why: shared by the builder methods; centralises the lock handling.
    /// What: locks the queue and appends; a poisoned lock is recovered
    /// (into_inner) rather than propagated — test-support code must not panic a
    /// caller over a poisoned test mutex.
    fn push(&self, outcome: Result<ChatResponse, InferenceError>) {
        let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        q.push(outcome);
    }

    /// Build an echo response reflecting the last user message in `request`.
    ///
    /// Why: the echo-mode fallback needs a plausible, deterministic response
    /// carrying real content and a non-zero [`crate::inference::Usage`].
    /// What: assistant content is the last `user`-role message's text (or an
    /// empty string); usage is a crude word-count so cost/usage plumbing has
    /// non-zero data to flow.
    fn echo_response(request: &ChatRequest) -> ChatResponse {
        let content = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let prompt_tokens = request
            .messages
            .iter()
            .filter_map(|m| m.content.as_ref())
            .map(|c| c.split_whitespace().count() as u32)
            .sum();
        let completion_tokens = content.split_whitespace().count() as u32;
        ChatResponse {
            id: "scripted-echo".into(),
            model: request.model.clone(),
            choices: vec![ChatChoice {
                message: AssistantMessage {
                    content: Some(content),
                    tool_calls: Vec::new(),
                },
                finish_reason: Some("stop".into()),
            }],
            usage: UsageBlock {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
                ..Default::default()
            },
        }
    }
}

#[async_trait]
impl InferenceAdapter for ScriptedAdapter {
    /// The configured adapter name.
    fn name(&self) -> &str {
        &self.name
    }

    /// The capabilities this adapter was constructed with.
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// Pop the next scripted outcome, or echo/error on exhaustion.
    ///
    /// Why: deterministic, network-free `chat` for tests.
    /// What: returns the front of the queue (FIFO) when non-empty; otherwise
    /// echoes the last user message (echo mode) or returns
    /// [`InferenceError::Provider`] (strict mode).
    /// Test: `echo_reflects_last_user_message`, `queue_is_fifo`,
    /// `strict_mode_exhaustion_errors`.
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        if q.is_empty() {
            drop(q);
            return if self.echo {
                Ok(Self::echo_response(request))
            } else {
                Err(InferenceError::Provider(
                    "scripted adapter queue exhausted".into(),
                ))
            };
        }
        q.remove(0)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::registry::{ProviderId, capabilities};
    use crate::inference::types::ChatMessage;

    fn caps() -> &'static ProviderCapabilities {
        capabilities(ProviderId::OpenRouter)
    }

    /// Why: echo mode must reflect the last user turn and report non-zero usage.
    /// Test: itself.
    #[tokio::test]
    async fn echo_reflects_last_user_message() {
        let adapter = ScriptedAdapter::echo("scripted", caps());
        let req = ChatRequest::new(
            "x/y",
            vec![ChatMessage::system("sys"), ChatMessage::user("hello world")],
        );
        let resp = adapter.chat(&req).await.expect("echo");
        assert_eq!(resp.first_text().as_deref(), Some("hello world"));
        assert!(resp.usage().total_tokens() > 0);
    }

    /// Why: scripted responses must be served in FIFO order before any echo.
    /// Test: itself.
    #[tokio::test]
    async fn queue_is_fifo() {
        let first = ScriptedAdapter::echo_response(&ChatRequest::new(
            "m",
            vec![ChatMessage::user("first")],
        ));
        let adapter = ScriptedAdapter::echo("scripted", caps()).with_response(first);
        let req = ChatRequest::new("m", vec![ChatMessage::user("second")]);
        // Queue front (scripted "first") comes before the echo of "second".
        assert_eq!(
            adapter.chat(&req).await.expect("q").first_text().as_deref(),
            Some("first")
        );
        assert_eq!(
            adapter
                .chat(&req)
                .await
                .expect("echo")
                .first_text()
                .as_deref(),
            Some("second")
        );
    }

    /// Why: strict mode must error on exhaustion, never silently echo.
    /// Test: itself.
    #[tokio::test]
    async fn strict_mode_exhaustion_errors() {
        let adapter = ScriptedAdapter::new("scripted", caps());
        let req = ChatRequest::new("m", vec![ChatMessage::user("x")]);
        let err = adapter.chat(&req).await.expect_err("exhausted");
        assert!(matches!(err, InferenceError::Provider(_)));
    }
}
