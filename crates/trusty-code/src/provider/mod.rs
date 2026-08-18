//! Provider abstraction and per-agent model routing.
//!
//! Why: trusty-code routes each agent to its own model, which may live behind
//! different backends (OpenRouter today, AWS Bedrock later). The agent loop must
//! not branch on the backend; it depends on the [`Provider`] trait and asks a
//! factory for the right implementation given a model slug. This module is that
//! seam (#1021) and the precedence resolver for which slug to use.
//! What: Re-exports the [`Provider`] trait and [`ToolChoice`] enum, the five
//! concrete providers ([`OpenRouterProvider`], [`BedrockProvider`],
//! [`FireworksProvider`], [`TogetherProvider`], [`AtlasCloudProvider`]), the
//! [`provider_for`] factory, the [`resolve_model`] precedence function plus
//! its [`DEFAULT_MODEL`] constant, the [`resolve_max_tokens`] precedence
//! function plus its [`DEFAULT_MAX_TOKENS`] constant, and (#2207) the
//! [`resolve_deadline_secs`] precedence function plus its
//! [`DEFAULT_RUN_DEADLINE_SECS`] constant and [`RUN_DEADLINE_ENV_VAR`] name,
//! and (#2308) the [`resolve_context_window`] precedence function plus its
//! [`DEFAULT_CONTEXT_WINDOW`] constant.
//! Test: submodule `tests` in each file; routing/adapter coverage in
//! `routing.rs` and `adapter.rs`.
//!
//! [`Provider`]: crate::provider::Provider
//! [`ToolChoice`]: crate::provider::ToolChoice
//! [`OpenRouterProvider`]: crate::provider::OpenRouterProvider
//! [`BedrockProvider`]: crate::provider::BedrockProvider
//! [`FireworksProvider`]: crate::provider::FireworksProvider
//! [`TogetherProvider`]: crate::provider::TogetherProvider
//! [`AtlasCloudProvider`]: crate::provider::AtlasCloudProvider
//! [`provider_for`]: crate::provider::provider_for
//! [`resolve_model`]: crate::provider::resolve_model
//! [`DEFAULT_MODEL`]: crate::provider::DEFAULT_MODEL
//! [`resolve_max_tokens`]: crate::provider::resolve_max_tokens
//! [`DEFAULT_MAX_TOKENS`]: crate::provider::DEFAULT_MAX_TOKENS
//! [`resolve_deadline_secs`]: crate::provider::resolve_deadline_secs
//! [`DEFAULT_RUN_DEADLINE_SECS`]: crate::provider::DEFAULT_RUN_DEADLINE_SECS
//! [`RUN_DEADLINE_ENV_VAR`]: crate::provider::RUN_DEADLINE_ENV_VAR
//! [`resolve_context_window`]: crate::provider::resolve_context_window
//! [`DEFAULT_CONTEXT_WINDOW`]: crate::provider::DEFAULT_CONTEXT_WINDOW

mod adapter;
mod atlascloud;
mod bedrock;
mod fireworks;
mod openrouter;
mod routing;
mod together;
mod traits;

pub use adapter::provider_for;
pub use atlascloud::AtlasCloudProvider;
pub use bedrock::BedrockProvider;
pub use fireworks::FireworksProvider;
pub use openrouter::OpenRouterProvider;
pub use routing::{
    DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS, DEFAULT_MODEL, DEFAULT_RUN_DEADLINE_SECS,
    RUN_DEADLINE_ENV_VAR, resolve_context_window, resolve_deadline_secs, resolve_max_tokens,
    resolve_model,
};
pub use together::TogetherProvider;
pub use traits::{Provider, ToolChoice};
