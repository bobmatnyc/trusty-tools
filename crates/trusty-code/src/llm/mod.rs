//! Native OpenRouter chat-completions client for trusty-code.
//!
//! Why: trusty-code agents need to invoke LLMs via the OpenRouter API without
//! depending on third-party Rust SDK crates (`async-openai`, etc.) that pin us
//! to specific provider contracts.  A thin native client gives us full control
//! over the wire format, headers, and error handling.
//! What: This module exports `LlmClient`, `LlmClientConfig`, all request/response
//! types (`ChatRequest`, `ChatResponse`, `ChatMessage`, `ToolDefinition`, …),
//! and `LlmError`.  The API key is injected at construction time via
//! `LlmClientConfig::new` or `LlmClientConfig::from_env`; library helpers never
//! read `std::env` directly.
//! Test: `cargo test -p trusty-code` covers all unit tests (serialisation,
//! deserialisation, error mapping).  `cargo test -p trusty-code --
//! --include-ignored` additionally runs the live `live_openrouter_call` test.

mod client;
mod client_trait;
mod error;
mod message;
mod request;
mod response;
mod tool_call_extractor;
mod usage;

// ── Public API re-exports ─────────────────────────────────────────────────────

pub use client::{LlmClient, LlmClientConfig};
pub use client_trait::LlmClientTrait;
pub use error::LlmError;
pub use message::ChatMessage;
pub use request::{ChatRequest, FunctionCall, FunctionDefinition, ToolCall, ToolDefinition};
pub use response::{AssistantMessage, ChatChoice, ChatResponse};
pub use tool_call_extractor::{
    DEFAULT_MAX_REPAIR_ATTEMPTS, ExtractedToolCall, ExtractionStrategy, SchemaViolation,
    ToolCallExtractError, ToolCallExtractor, extract_with_repair, strategy_order_for,
};
pub use usage::UsageBlock;
