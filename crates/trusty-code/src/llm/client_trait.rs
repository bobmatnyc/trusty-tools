//! Object-safe trait abstraction over the concrete inference transports.
//!
//! Why: The multi-turn agent loop must be unit-testable without issuing real
//! HTTP requests. The production transports (`OpenAiCompatClient`,
//! `DispatchingLlmClient`, `BedrockChatClient`) perform network I/O in their
//! inherent `chat` methods, so the loop depends on this trait instead —
//! production wires in a real transport, tests wire in a scripted mock. Defining
//! the seam here keeps the transports free of test-only machinery.
//! What: Declares `LlmClientTrait` with a single `chat` method mirroring the
//! transports' `chat`. Each concrete transport implements it in its own module
//! (`client`, `dispatch`, `bedrock`).
//! Test: `client_trait::tests::transport_implements_trait` proves
//! `OpenAiCompatClient` satisfies the trait via a `dyn` coercion; the loop's
//! mock (in `agent_loop::tests`) exercises the trait independently of the
//! network.

use async_trait::async_trait;

use super::{ChatRequest, ChatResponse, LlmError};

/// Object-safe interface for issuing a single chat-completions call.
///
/// Why: Lets `AgentLoop` accept an `Arc<dyn LlmClientTrait>` so the network
/// client can be swapped for a deterministic stub in tests. `Send + Sync` are
/// required because the loop runs under `tokio` and may be shared across tasks.
/// What: A one-method async trait whose signature matches `LlmClient::chat`
/// exactly, so the concrete client implements it by trivial delegation.
/// Test: `llm_client_implements_trait`.
#[async_trait]
pub trait LlmClientTrait: Send + Sync {
    /// Issue one chat-completions request and return the parsed response.
    ///
    /// Why: A single-method surface keeps the mock trivial and the loop's
    /// dependency minimal.
    /// What: Mirrors `LlmClient::chat`: posts `req` and yields a `ChatResponse`
    /// or an `LlmError`.
    /// Test: Exercised by the real client in `llm_client_implements_trait` and
    /// by the scripted mock in `agent_loop::tests`.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError>;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::llm::OpenAiCompatClient;

    /// The production `OpenAiCompatClient` can be coerced to
    /// `Arc<dyn LlmClientTrait>`.
    ///
    /// Why: The agent loop stores its client as `Arc<dyn LlmClientTrait>`; this
    /// guards that the production transport actually satisfies the trait (object
    /// safety + impl presence) without making a network call. Construction is
    /// credential-free (#2245), so this needs no key.
    /// What: Build a client and coerce it to the trait object.
    /// Test: this test.
    #[test]
    fn transport_implements_trait() {
        let _erased: Arc<dyn LlmClientTrait> = Arc::new(OpenAiCompatClient::new());
    }
}
