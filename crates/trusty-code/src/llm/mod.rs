//! Native LLM clients for trusty-code: OpenRouter (HTTP) and AWS Bedrock
//! (Converse API).
//!
//! Why: trusty-code agents need to invoke LLMs via OpenRouter's chat-
//! completions endpoint OR, for organisations running on AWS (#1021), via
//! Bedrock's Converse API — without pulling in a third-party SDK
//! (`async-openai`, etc.) that pins us to one provider's contract. Both
//! transports speak the same provider-neutral `ChatRequest`/`ChatResponse`
//! shape so `AgentLoop` and every other caller never branches on the
//! backend.
//! What: This module exports `LlmClient` (OpenRouter HTTP), `BedrockChatClient`
//! (Bedrock Converse), `DispatchingLlmClient` (routes each request to the
//! right transport by model slug via `crate::provider::provider_for`),
//! `LlmClientConfig`, all request/response types (`ChatRequest`,
//! `ChatResponse`, `ChatMessage`, `ToolDefinition`, …), and `LlmError`. The
//! OpenRouter API key is injected at construction time via
//! `LlmClientConfig::new`/`from_env`; Bedrock uses the standard AWS
//! credential chain (no key). Library helpers never read `std::env` directly
//! except at these documented construction seams.
//! Test: `cargo test -p trusty-code` covers all unit tests (serialisation,
//! deserialisation, error mapping, Bedrock message/tool-choice/response
//! conversion). `cargo test -p trusty-code -- --include-ignored` additionally
//! runs the live `live_openrouter_call`/`live_bedrock_call` tests.

mod bedrock;
mod client;
mod client_trait;
mod dispatch;
mod error;
mod message;
mod request;
mod response;
mod tool_call_extractor;
mod usage;

// ── Public API re-exports ─────────────────────────────────────────────────────

pub use bedrock::BedrockChatClient;
pub use client::{LlmClient, LlmClientConfig};
pub use client_trait::LlmClientTrait;
pub use dispatch::DispatchingLlmClient;
pub use error::LlmError;
pub use message::ChatMessage;
pub use request::{
    CacheControl, ChatRequest, FunctionCall, FunctionDefinition, RequestUsageConfig, ToolCall,
    ToolDefinition,
};
pub use response::{AssistantMessage, ChatChoice, ChatResponse};
pub use tool_call_extractor::{
    DEFAULT_MAX_REPAIR_ATTEMPTS, ExtractedToolCall, ExtractionStrategy, SchemaViolation,
    ToolCallExtractError, ToolCallExtractor, extract_with_repair, strategy_order_for,
};
pub use usage::UsageBlock;
