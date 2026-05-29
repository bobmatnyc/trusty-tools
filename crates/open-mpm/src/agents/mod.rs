//! Agent configuration loader.
//!
//! Why: Sub-agents (and the PM itself) are defined declaratively in TOML so
//! model, prompt, and LLM parameters can evolve without code changes. This
//! module root stitches together the config type definitions (`config`,
//! `params`), the model-resolution + ctrl-default logic (`model`), and the
//! disk loaders (`loader`) into the public `crate::agents` surface.
//! What: Re-exports `AgentConfig` and every nested config type plus the
//! `resolve_model` helper so external call sites keep referencing
//! `crate::agents::*` unchanged after the file split (#358).
//! Test: `AgentConfig::load` on bundled `pm.toml` / `python-engineer.toml`
//! returns Ok with expected `agent.name` / `agent.model`; see `tests.rs`.

pub mod claude_code_runner;
pub mod claude_mpm_loader;
mod config;
pub mod context_filter;
pub mod harness_protocol;
pub mod in_process_runner;
mod loader;
mod model;
mod params;
pub mod persona;
pub mod prompt_builder;
pub mod registry;

#[cfg(test)]
mod tests;

// Re-export the config data shapes so `crate::agents::<Type>` keeps resolving
// for every external consumer after the #358 file split.
pub use config::{
    AgentCapabilities, AgentConfig, AgentInfo, NativeToolsConfig, RunnerKind, SystemPrompt,
    TicketingTomlConfig, ToolsConfig,
};
pub use params::{
    AgentCompressConfig, AgentPluginsConfig, LlmParams, RbacConfig, RunnerConfig,
    SessionCompressionConfig, ToolChoice,
};

// Model resolution: `resolve_model` and `ModelSource` are part of the public
// surface; `agent_model_env` is crate-internal (claude_code_runner uses it).
pub(crate) use model::agent_model_env;
pub use model::{FALLBACK_MODEL, ModelSource, resolve_model};

// `agent_env_suffix` and `agent_config_path` are exercised only by the unit
// tests; surface them at crate scope (under `#[cfg(test)]`) so the nested
// `tests` submodules can reach them via `crate::agents::*`.
#[cfg(test)]
pub(crate) use loader::agent_config_path;
#[cfg(test)]
pub(crate) use model::agent_env_suffix;
