//! Test-only mock [`LlmProvider`] for the context-engine compaction tests.
//!
//! Why: §7.3 compaction is dependency-injected through the [`LlmProvider`] trait
//! specifically so tests run with NO real network and NO real model. The
//! acceptance tests must also assert *which model* the compaction call asked for
//! (Haiku default vs. `compaction_model` override) and that the evicted content /
//! goal+session ids survive into the summary. This mock captures every request it
//! receives and returns a caller-supplied canned summary, satisfying both needs
//! deterministically.
//! What: [`MockProvider`] implements [`LlmProvider`]; it records each
//! [`LlmRequest`] into a shared `Vec` and can either echo a fixed reply or build
//! the reply from the request (so a test can assert the evicted rounds were
//! actually delivered to the model). It is compiled only under `#[cfg(test)]`.
//! Test: used by `compaction_tests.rs` and `engine_tests.rs`.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::core::sm::providers::{LlmProvider, LlmRequest, LlmResponse, SmLlmError};

/// How a [`MockProvider`] produces its reply text.
///
/// Why: different tests need different reply behaviours — a fixed canned summary
/// (assert it lands in `compressed_context`) versus a reply derived from the
/// request (assert the evicted rounds / ids reached the model and survive).
/// What: `Fixed` returns the same text every call; `Echo` returns a prefix plus
/// the concatenated user-message contents of the request.
/// Test: both variants exercised in `compaction_tests.rs` / `engine_tests.rs`.
#[derive(Debug, Clone)]
pub enum MockReply {
    /// Always return this exact text.
    Fixed(String),
    /// Return `prefix` followed by the request's concatenated user-message text.
    Echo { prefix: String },
}

/// A recording, deterministic mock LLM provider for tests.
///
/// Why: lets the engine/compaction tests inject a provider that (a) needs no
/// credentials or network and (b) exposes the exact requests it was asked to
/// complete, so a test can assert the resolved model id, temperature, and that
/// the evicted content was delivered.
/// What: holds the configured [`MockReply`] and a shared, lock-guarded log of
/// every [`LlmRequest`]. Clones share the same log (the inner `Arc<Mutex<…>>`),
/// so a clone handed to the engine still records into the handle the test holds.
/// Test: `mock_provider_records_requests` and the fold/engine tests.
#[derive(Clone)]
pub struct MockProvider {
    reply: MockReply,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl MockProvider {
    /// Build a mock that always returns `text`.
    ///
    /// Why: the "evicted content survives" test injects a known summary so it can
    /// assert that exact text lands in `compressed_context`.
    /// What: constructs a [`MockProvider`] with a `Fixed` reply and an empty log.
    /// Test: `fold_rounds_returns_mock_summary`.
    pub fn fixed(text: impl Into<String>) -> Self {
        Self {
            reply: MockReply::Fixed(text.into()),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Build a mock that echoes the request's user content after `prefix`.
    ///
    /// Why: the GOLDEN test asserts that goal/session ids present in the evicted
    /// rounds survive compaction; an echoing mock guarantees the ids the test put
    /// in the rounds reappear in the returned summary iff they were delivered.
    /// What: constructs a [`MockProvider`] with an `Echo` reply.
    /// Test: `golden_ids_survive_compaction` (engine tests).
    pub fn echo(prefix: impl Into<String>) -> Self {
        Self {
            reply: MockReply::Echo {
                prefix: prefix.into(),
            },
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The requests this mock has received so far (cloned snapshot).
    ///
    /// Why: tests assert which model/temperature the compaction call used by
    /// inspecting the captured requests.
    /// What: returns a clone of the recorded [`LlmRequest`] log.
    /// Test: `mock_provider_records_requests`, the model-override engine tests.
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.requests
            .lock()
            .expect("mock lock not poisoned")
            .clone()
    }

    /// The model id of the most recent request, if any.
    ///
    /// Why: the "Haiku default / override honoured" acceptance tests only need the
    /// last request's model; this is a terse accessor for that.
    /// What: returns the `model` of the last recorded request.
    /// Test: `default_compaction_uses_summary_model`,
    /// `compaction_model_override_is_honored`.
    pub fn last_model(&self) -> Option<String> {
        self.requests
            .lock()
            .expect("mock lock not poisoned")
            .last()
            .map(|r| r.model.clone())
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    /// Static name for logs; the mock is always `"mock"`.
    ///
    /// Why: satisfies the trait; the value is irrelevant to assertions.
    /// What: returns `"mock"`.
    /// Test: trivially covered by use.
    fn name(&self) -> &str {
        "mock"
    }

    /// Record the request and return the configured canned/echoed reply.
    ///
    /// Why: capturing the request is what lets tests assert model/temperature and
    /// delivered content; returning a deterministic reply avoids any real LLM.
    /// What: pushes `req` into the shared log, then builds the response text per
    /// the [`MockReply`] mode and returns it with zeroed telemetry.
    /// Test: `mock_provider_records_requests` and the fold/engine tests.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, SmLlmError> {
        let text = match &self.reply {
            MockReply::Fixed(t) => t.clone(),
            MockReply::Echo { prefix } => {
                let body: String = req
                    .messages
                    .iter()
                    .filter(|m| m.role == "user")
                    .map(|m| m.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{prefix}{body}")
            }
        };
        let model = req.model.clone();
        self.requests
            .lock()
            .expect("mock lock not poisoned")
            .push(req);
        Ok(LlmResponse {
            text,
            model,
            input_tokens: 0,
            output_tokens: 0,
            latency_ms: 0,
            cost_usd: 0.0,
        })
    }
}
