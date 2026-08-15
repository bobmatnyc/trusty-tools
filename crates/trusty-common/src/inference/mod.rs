//! Unified inference provider adapter layer (epic #2400).
//!
//! Why: `trusty-code`, `trusty-review`, and three internal `trusty-agents`
//! layers each hand-rolled an LLM client, a credential lookup, and an
//! `.env.local` loader with subtly different precedence rules. Epic #2400
//! centralises all of that in `trusty-common` so every consumer shares one
//! implementation instead of six.
//!
//! What: Wave 1 ticket #2401 shipped the credential resolution layer here;
//! #4564 promoted it to [`crate::credentials`] because it was never
//! inference-specific, leaving only a deprecated alias behind.
//! Wave 1 ticket #2402 (behind the `inference-client` feature) adds the
//! inference foundation on top of it: the [`types`] request/response model, the
//! [`InferenceError`] error surface, the [`InferenceAdapter`] trait, the
//! capability [`registry`] (context windows incl. the #2330 haiku fix, pricing,
//! caching, tool dialects), the two-stage [`configurator`] (`provider_for` +
//! [`Configurator`]), and the [`test_support`] doubles. Concrete provider HTTP
//! adapters land in #2403/#2407.
//!
//! Test: the `inference-client` surface is covered by each submodule's inline
//! tests and `crates/trusty-common/tests/inference_foundation.rs`; the
//! deprecated credential alias by
//! `deprecated_inference_alias_still_resolves`.
//!
//! [`types`]: crate::inference::types
//! [`InferenceError`]: crate::inference::InferenceError
//! [`InferenceAdapter`]: crate::inference::InferenceAdapter
//! [`registry`]: crate::inference::registry
//! [`configurator`]: crate::inference::configurator
//! [`Configurator`]: configurator::Configurator
//! [`test_support`]: crate::inference::test_support

/// Deprecated compatibility alias for [`crate::credentials`] (#4564).
///
/// Why: the credential resolver moved out from under `inference::` because it
/// was never inference-specific — four of its ten original registry entries
/// were Slack/Telegram/`claude-code` tokens. This alias keeps
/// `trusty_common::inference::credentials::…` resolving for out-of-tree
/// consumers for one release rather than breaking them at the same moment the
/// registry grows.
/// What: a plain re-export. Every in-tree consumer was moved to
/// `trusty_common::credentials` in the same change, so nothing in this
/// workspace triggers the deprecation.
/// Test: `credentials::registry::tests::deprecated_inference_alias_still_resolves`.
#[deprecated(
    since = "0.28.0",
    note = "moved out of `inference` — use `trusty_common::credentials` instead (#4564)"
)]
pub use crate::credentials;

#[cfg(feature = "inference-client")]
pub mod adapter;
#[cfg(feature = "bedrock-client")]
pub mod bedrock;
#[cfg(feature = "config-cli")]
pub mod config;
#[cfg(feature = "inference-client")]
pub mod configurator;
#[cfg(feature = "inference-client")]
pub mod error;
#[cfg(feature = "inference-client")]
pub mod providers;
#[cfg(feature = "inference-client")]
pub mod registry;
#[cfg(feature = "inference-client")]
pub mod streaming;
#[cfg(feature = "inference-client")]
pub mod test_support;
#[cfg(feature = "inference-client")]
pub mod types;

// Flat re-exports of the most-used surface so consumers write
// `trusty_common::inference::{InferenceAdapter, ChatRequest, …}` rather than
// reaching into each submodule.
#[cfg(feature = "inference-client")]
pub use adapter::InferenceAdapter;
#[cfg(feature = "bedrock-client")]
pub use bedrock::{BedrockAdapter, register_bedrock_factory};
#[cfg(feature = "config-cli")]
pub use config::{ConfigCommand, ConfigKeysCommand};
#[cfg(feature = "inference-client")]
pub use configurator::{AdapterFactory, Configurator, ResolvedProvider, provider_for};
#[cfg(feature = "inference-client")]
pub use error::InferenceError;
#[cfg(feature = "inference-client")]
pub use providers::{OpenAiCompatAdapter, OpenAiCompatConfig, register_default_factories};
#[cfg(feature = "inference-client")]
pub use registry::{
    Pricing, ProviderCapabilities, ProviderId, ToolDialect, capabilities, capabilities_for,
    context_window, pricing,
};
#[cfg(feature = "inference-client")]
pub use streaming::{
    ChatStream, ChatStreamEvent, SseDecoder, StreamAssembly, StreamCompletion, ToolCallDelta,
    buffered_stream, decode_event_stream,
};
// #4425: `PromptTokensDetails`/`UsageBlock` join the flat re-export because a
// consumer that owns a `ChatResponse` owns its wire usage block — trusty-code
// reads it directly for cost accounting and had to reach into `types::usage`.
#[cfg(feature = "inference-client")]
pub use types::{
    AssistantMessage, CacheControl, ChatChoice, ChatMessage, ChatRequest, ChatResponse,
    FunctionCall, FunctionDefinition, PromptTokensDetails, RequestUsageConfig, SecretString,
    StopReason, ToolCall, ToolChoice, ToolDefinition, Usage, UsageBlock, openai_tool_choice,
};
