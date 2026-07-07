//! Provider abstraction and per-agent model routing.
//!
//! Why: trusty-code routes each agent to its own model, which may live behind
//! different backends (OpenRouter today, AWS Bedrock later). The agent loop must
//! not branch on the backend; it depends on the [`Provider`] trait and asks a
//! factory for the right implementation given a model slug. This module is that
//! seam (#1021) and the precedence resolver for which slug to use.
//! What: Re-exports the [`Provider`] trait and [`ToolChoice`] enum, the two
//! concrete providers ([`OpenRouterProvider`], [`BedrockProvider`]), the
//! [`provider_for`] factory, the [`resolve_model`] precedence function plus
//! its [`DEFAULT_MODEL`] constant, the [`resolve_max_tokens`] precedence
//! function plus its [`DEFAULT_MAX_TOKENS`] constant, and (#2207) the
//! [`resolve_deadline_secs`] precedence function plus its
//! [`DEFAULT_RUN_DEADLINE_SECS`] constant and [`RUN_DEADLINE_ENV_VAR`] name.
//! Test: submodule `tests` in each file; routing/adapter coverage in
//! `routing.rs` and `adapter.rs`.

mod adapter;
mod bedrock;
mod openrouter;
mod routing;
mod traits;

pub use adapter::provider_for;
pub use bedrock::BedrockProvider;
pub use openrouter::OpenRouterProvider;
pub use routing::{
    DEFAULT_MAX_TOKENS, DEFAULT_MODEL, DEFAULT_RUN_DEADLINE_SECS, RUN_DEADLINE_ENV_VAR,
    resolve_deadline_secs, resolve_max_tokens, resolve_model,
};
pub use traits::{Provider, ToolChoice};
