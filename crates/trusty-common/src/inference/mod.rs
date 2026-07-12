//! Unified inference provider adapter layer (epic #2400).
//!
//! Why: `trusty-code`, `trusty-review`, and three internal `trusty-agents`
//! layers each hand-rolled an LLM client, a credential lookup, and an
//! `.env.local` loader with subtly different precedence rules. Epic #2400
//! centralises all of that in `trusty-common` so every consumer shares one
//! implementation instead of six.
//!
//! What: Wave 1 ticket #2401 shipped the credential resolution layer:
//! [`credentials::KeyStore`] trait, three backends (`MemoryKeyStore`,
//! `FileKeyStore`, `KeyringStore`), the `resolve_key` precedence chain (process
//! env > `.env.local` > secure store), and the shared `redact_secret` helper.
//! Wave 1 ticket #2402 (behind the `inference-client` feature) adds the
//! inference foundation on top of it: the [`types`] request/response model, the
//! [`InferenceError`] error surface, the [`InferenceAdapter`] trait, the
//! capability [`registry`] (context windows incl. the #2330 haiku fix, pricing,
//! caching, tool dialects), the two-stage [`configurator`] (`provider_for` +
//! [`Configurator`]), and the [`test_support`] doubles. Concrete provider HTTP
//! adapters land in #2403/#2407.
//!
//! Test: see `credentials` submodule and its `*_tests` sibling files; the
//! `inference-client` surface is covered by each submodule's inline tests and
//! `crates/trusty-common/tests/inference_foundation.rs`.

pub mod credentials;

#[cfg(feature = "inference-client")]
pub mod adapter;
#[cfg(feature = "inference-client")]
pub mod configurator;
#[cfg(feature = "inference-client")]
pub mod error;
#[cfg(feature = "inference-client")]
pub mod providers;
#[cfg(feature = "inference-client")]
pub mod registry;
#[cfg(feature = "inference-client")]
pub mod test_support;
#[cfg(feature = "inference-client")]
pub mod types;

// Flat re-exports of the most-used surface so consumers write
// `trusty_common::inference::{InferenceAdapter, ChatRequest, …}` rather than
// reaching into each submodule.
#[cfg(feature = "inference-client")]
pub use adapter::InferenceAdapter;
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
pub use types::{
    AssistantMessage, CacheControl, ChatChoice, ChatMessage, ChatRequest, ChatResponse,
    FunctionCall, FunctionDefinition, RequestUsageConfig, SecretString, StopReason, ToolCall,
    ToolChoice, ToolDefinition, Usage, openai_tool_choice,
};
