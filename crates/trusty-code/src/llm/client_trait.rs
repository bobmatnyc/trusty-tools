//! Object-safe trait abstraction over the concrete `LlmClient`.
//!
//! Why: The multi-turn agent loop must be unit-testable without issuing real
//! HTTP requests. The concrete `LlmClient` performs network I/O in its inherent
//! `chat` method, so the loop depends on this trait instead — production wires in
//! the real `LlmClient`, tests wire in a scripted mock. Defining the seam here
//! keeps `LlmClient` itself free of test-only machinery.
//! What: Declares `LlmClientTrait` with a single `chat` method mirroring
//! `LlmClient::chat`, and provides the blanket-free impl for `LlmClient` that
//! delegates to the existing inherent method.
//! Test: `client_trait::tests::llm_client_implements_trait` proves `LlmClient`
//! satisfies the trait via a `dyn` coercion; the loop's mock (in
//! `agent_loop::tests`) exercises the trait independently of the network.

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

#[async_trait]
impl LlmClientTrait for super::LlmClient {
    /// Delegate to the inherent `LlmClient::chat`.
    ///
    /// Why: Keeps a single implementation of the HTTP mechanics; the trait is a
    /// thin dispatch seam only.
    /// What: Forwards `req` to the inherent method.
    /// Test: `llm_client_implements_trait`.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        super::LlmClient::chat(self, req).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::llm::{LlmClient, LlmClientConfig};

    /// `LlmClient` can be coerced to `Arc<dyn LlmClientTrait>`.
    ///
    /// Why: The agent loop stores its client as `Arc<dyn LlmClientTrait>`; this
    /// guards that the production client actually satisfies the trait (object
    /// safety + impl presence) without making a network call.
    /// What: Build a client with a fake key and coerce it to the trait object.
    /// Test: this test.
    #[test]
    fn llm_client_implements_trait() {
        let cfg = LlmClientConfig::new("sk-or-test").expect("config");
        let client = LlmClient::from_config(cfg).expect("client");
        let _erased: Arc<dyn LlmClientTrait> = Arc::new(client);
    }
}
