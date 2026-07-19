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

    /// #3062 (SPEC-AGENTFW-02 §3.5): `tagent resume <run-id>` was asked for a
    /// run id with no `checkpoint.json` on disk.
    #[error(
        "no checkpoint found for run '{run_id}'; resumable runs: {}",
        if available.is_empty() { "(none)".to_string() } else { available.join(", ") }
    )]
    CheckpointNotFound {
        run_id: String,
        available: Vec<String>,
    },

    /// #3062 (SPEC-AGENTFW-02 §3.5): the checkpoint file exists but is not
    /// parseable JSON (or does not match `CheckpointRecord`'s shape). Fails
    /// closed rather than silently starting a fresh run under the same id.
    #[error("checkpoint at {path} is corrupt: {source}")]
    CheckpointCorrupt {
        path: String,
        #[source]
        source: anyhow::Error,
    },

    /// #3062: the checkpoint parses but was written by an incompatible
    /// journal schema version. See `checkpoint::CHECKPOINT_SCHEMA_VERSION`.
    #[error(
        "checkpoint at {path} has schema_version {found}, expected {expected}; \
         it was written by an incompatible version of trusty-agents"
    )]
    CheckpointSchemaMismatch {
        path: String,
        found: u32,
        expected: u32,
    },

    /// #3062 (SPEC-AGENTFW-02 §3.4): the on-disk workflow JSON's phase list no
    /// longer matches the checkpoint's `phase_names` snapshot. Resume MUST
    /// fail closed rather than run a different pipeline than the one that was
    /// checkpointed.
    #[error(
        "workflow '{workflow}' definition changed since checkpoint '{run_id}' was written: \
         phase list no longer matches (checkpoint: {checkpoint_phases:?}, current: {current_phases:?})"
    )]
    ResumeDefinitionChanged {
        run_id: String,
        workflow: String,
        checkpoint_phases: Vec<String>,
        current_phases: Vec<String>,
    },
}
