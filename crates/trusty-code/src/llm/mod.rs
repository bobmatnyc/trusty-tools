//! LLM transports for trusty-code: the shared OpenAI-compatible adapter
//! (OpenRouter / Fireworks) and AWS Bedrock (Converse API).
//!
//! Why: trusty-code agents need to invoke LLMs via OpenAI-compatible endpoints
//! (OpenRouter, and — since #2406 — Fireworks) OR, for organisations running on
//! AWS (#1021), via Bedrock's Converse API. Both transports speak the same
//! provider-neutral `ChatRequest`/`ChatResponse` shape so `AgentLoop` and every
//! other caller never branches on the backend. The OpenAI-compatible HTTP
//! mechanics now live in the shared `trusty_common::inference` adapter layer
//! (epic #2400) rather than a bespoke tcode client — `OpenAiCompatClient`
//! (`client`) is the thin consumer of that core, bridged to tcode's wire types
//! by `convert`.
//! What: This module exports `OpenAiCompatClient` (shared OpenRouter/Fireworks
//! transport), `BedrockChatClient` (Bedrock Converse), `DispatchingLlmClient`
//! (routes each request to the right transport by model slug via
//! `crate::provider::provider_for`), all request/response types (`ChatRequest`,
//! `ChatResponse`, `ChatMessage`, `ToolDefinition`, …), `LlmError`, and (#2264)
//! the opt-in `debug_capture` module — `DebugCaptureSink` +
//! `wrap_with_debug_capture` — that dumps the FULL wire-level request/response
//! of every round-trip to JSONL when `TCODE_DEBUG_TRANSCRIPT` is set.
//! OpenRouter/Fireworks credentials are resolved via the shared 3-tier chain
//! (process env > `.env.local` > secure store); Bedrock uses the standard AWS
//! credential chain (no key). Library helpers never read `std::env` for secrets
//! directly — resolution is centralised in the shared resolver.
//! Test: `cargo test -p trusty-code` covers all unit tests (serialisation,
//! deserialisation, type conversion + error mapping, Bedrock message/tool-choice/
//! response conversion, debug-capture) plus the offline black-box HTTP round-trip
//! in `tests/inference_shared_adapter_e2e.rs`. `cargo test -p trusty-code --
//! --include-ignored` additionally runs the live
//! `live_openrouter_call`/`live_fireworks_call`/`live_bedrock_call` tests.

mod bedrock;
mod client;
mod client_trait;
mod convert;
mod debug_capture;
mod dispatch;
mod error;
mod message;
mod request;
mod response;
mod tool_call_extractor;
mod usage;

// ── Public API re-exports ─────────────────────────────────────────────────────

pub use bedrock::BedrockChatClient;
pub use client::OpenAiCompatClient;
pub use client_trait::LlmClientTrait;
pub use debug_capture::{DebugCaptureSink, wrap_with_debug_capture};
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
