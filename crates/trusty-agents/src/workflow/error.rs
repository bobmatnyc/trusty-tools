//! Typed error enum for workflow engine failures.
//!
//! Why: Callers (the CLI and higher-level tests) benefit from matchable
//! variants rather than opaque `anyhow::Error` strings, especially for
//! distinguishing bad config from runtime phase failures.
//! What: `WorkflowError` with variants for phase failure, missing agent,
//! and invalid config. Each carries enough context for useful messages.
//! Test: Tests construct each variant and assert the `Display` output.

use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum WorkflowError {
    #[error("phase '{phase}' failed: {source}")]
    PhaseFailed {
        phase: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("agent '{name}' not found")]
    AgentNotFound { name: String },

    #[error("workflow config invalid: {0}")]
    ConfigInvalid(String),

    #[error("workflow file not found: {path}")]
    WorkflowNotFound { path: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// #33: A tool-calling agent produced two consecutive plain-text
    /// responses without ever calling a tool. The tool-discipline retry
    /// mechanism has given up rather than looping forever.
    #[error(
        "agent '{agent}' stalled: produced plain text for {consecutive_turns} consecutive turns without a tool call"
    )]
    AgentLoopStalled {
        agent: String,
        consecutive_turns: u32,
    },
}
