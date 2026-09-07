//! Test doubles shared across the profiling submodules' test files.
//!
//! Why: both model passes decide what to send by asking their adapter
//! (#5588), and `trusty_common::inference::test_support::ScriptedAdapter`
//! discards the request — so neither call site can be proven from its response
//! alone. Both need the same double, and a second copy of it would drift.
//! What: [`RecordingAdapter`], an [`InferenceAdapter`] that keeps every request
//! it was handed and answers with one canned [`ChatResponse`].
//! Test: used by `period_reviewer_picks_the_delivery_the_adapter_supports`
//! (`batch_reviewer_tests.rs`) and
//! `synthesizer_picks_the_delivery_the_adapter_supports`
//! (`synthesizer_tests.rs`).

use std::sync::Mutex;

use async_trait::async_trait;
use trusty_common::inference::{
    ChatRequest, ChatResponse, InferenceAdapter, InferenceError, ProviderCapabilities,
};

/// An adapter that keeps every request it was handed and answers `response`.
///
/// Why: the capability an adapter reports is what each profiling pass reads to
/// choose between `ChatRequest.response_schema` and the prose fallback, so the
/// only way to prove a call site asked rather than hard-coded one delivery is to
/// inspect the request it actually sent.
/// What: constructed with a capability seed and one canned response; `chat`
/// records the request and returns a clone of that response, however many times
/// it is called. [`Self::only_request`] reads the single recorded request back.
/// Test: as the module doc.
pub struct RecordingAdapter {
    capabilities: &'static ProviderCapabilities,
    seen: Mutex<Vec<ChatRequest>>,
    response: ChatResponse,
}

impl RecordingAdapter {
    /// Build a recorder over `capabilities` answering `response` every time.
    pub fn new(capabilities: &'static ProviderCapabilities, response: ChatResponse) -> Self {
        Self {
            capabilities,
            seen: Mutex::new(Vec::new()),
            response,
        }
    }

    /// The single request this adapter was sent.
    ///
    /// Panics when the count is not exactly one — a pass that called twice, or
    /// not at all, is a different failure than the delivery being wrong, and the
    /// assertion should say which.
    pub fn only_request(&self) -> ChatRequest {
        let seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            seen.len(),
            1,
            "expected exactly one call, got {}",
            seen.len()
        );
        seen[0].clone()
    }

    /// The system turn of [`Self::only_request`], or an empty string.
    pub fn only_system_turn(&self) -> String {
        self.only_request().messages[0]
            .content
            .clone()
            .unwrap_or_default()
    }
}

#[async_trait]
impl InferenceAdapter for RecordingAdapter {
    fn name(&self) -> &str {
        "recording"
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        self.capabilities
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(request.clone());
        Ok(self.response.clone())
    }
}
