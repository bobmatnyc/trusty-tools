//! Normalized inference request/response model (issue #2402, epic #2400).
//!
//! Why: six bespoke LLM clients each modelled messages, tools, usage, and the
//! request/response envelope slightly differently. This module is the single
//! set of wire+in-memory types every [`super::adapter::InferenceAdapter`]
//! speaks, shaped to absorb what tcode's OpenRouter + Bedrock clients already
//! emit so #2406 migrates without a wire-format change.
//! What: re-exports the message, tool, request, response, usage, and secret
//! types from their focused submodules (one concept per file to stay well under
//! the 500-SLOC cap).
//! Test: each submodule's inline `tests`; cross-type round-trips in
//! `crates/trusty-common/tests/inference_foundation.rs`.

mod message;
mod request;
mod response;
mod secret;
mod tool;
mod usage;

pub use message::ChatMessage;
pub use request::ChatRequest;
pub use response::{AssistantMessage, ChatChoice, ChatResponse, StopReason};
pub use secret::SecretString;
pub use tool::{
    CacheControl, FunctionCall, FunctionDefinition, RequestUsageConfig, ToolCall, ToolChoice,
    ToolDefinition, openai_tool_choice,
};
pub use usage::{PromptTokensDetails, Usage, UsageBlock};
