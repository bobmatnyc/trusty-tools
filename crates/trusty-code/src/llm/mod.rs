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
mod error;
mod types;

// ── Public API re-exports ─────────────────────────────────────────────────────

pub use client::{LlmClient, LlmClientConfig};
pub use error::LlmError;
pub use types::{
    AssistantMessage, ChatChoice, ChatMessage, ChatRequest, ChatResponse, FunctionCall,
    FunctionDefinition, ToolCall, ToolDefinition, UsageBlock,
};
