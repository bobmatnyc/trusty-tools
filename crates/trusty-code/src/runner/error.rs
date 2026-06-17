//! Error type for the in-process agent runner.
//!
//! Why: The runner is library code, so per the crate convention it surfaces a
//! structured `thiserror` enum rather than `unwrap()` or bare `anyhow` strings.
//! Each variant names a distinct failure the caller (the delegate tool, the
//! orchestrator) may want to react to differently — a missing/invalid agent
//! config is the caller's mistake, whereas an `AgentLoop` failure is downstream.
//! What: `RunnerError` enumerates config-load, unknown-agent, and loop-failure
//! cases. It converts to `anyhow::Error` automatically so it slots into the
//! `AgentRunner` trait's `anyhow::Result` return type via `?`.
//! Test: `runner::tests::unknown_agent_errors` and the config-load tests assert
//! on these variants' `Display` text.

use std::path::PathBuf;

use crate::agent_loop::AgentLoopError;

/// Failure modes of an in-process agent run.
///
/// Why: A typed error lets call sites distinguish "you asked for an agent that
/// does not exist" (recoverable, surface to the model) from "the agent's loop
/// blew up" (propagate). Bundling them in one enum keeps the runner's signature
/// a single `Result<_, RunnerError>`.
/// What: `UnknownAgent` and `ConfigLoad` cover resolving the agent's TOML;
/// `Loop` wraps an `AgentLoopError` from the multi-turn loop.
/// Test: `runner::tests::*` exercise `UnknownAgent` and `Loop` paths.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// No agent config was found for the requested name in the config directory.
    #[error("unknown agent '{name}': no config found in {dir}")]
    UnknownAgent {
        /// The requested agent slug.
        name: String,
        /// The directory that was searched for `<name>.toml`.
        dir: PathBuf,
    },

    /// The agent's config file exists but could not be read or parsed.
    #[error("failed to load agent config for '{name}': {source}")]
    ConfigLoad {
        /// The requested agent slug.
        name: String,
        /// The underlying load/parse error.
        #[source]
        source: anyhow::Error,
    },

    /// The multi-turn agent loop returned an error (turn cap, timeout, LLM, …).
    #[error("agent '{name}' loop failed: {source}")]
    Loop {
        /// The agent slug whose loop failed.
        name: String,
        /// The underlying loop error.
        #[source]
        source: AgentLoopError,
    },
}
