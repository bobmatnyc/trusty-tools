//! Runtime execution layer: the in-process agent runner (issue #1029).
//!
//! Why: The PM's `delegate_to_agent` tool needs a concrete `AgentRunner` that
//! actually executes a sub-agent. The M1 model-comparison harness runs that
//! sub-agent *in-process* — same process as the PM — so a delegation cycle is
//! cheap, deterministic to observe, and lets the sub-agent's token usage accrue
//! onto the same shared LLM client (and thus the PM's transcript). This module
//! is that runner plus its DI seams.
//! What: Re-exports `InProcessAgentRunner` (implements `crate::tools::AgentRunner`),
//! the `RegistryFactory` trait (how a sub-agent's tools are assembled),
//! `InProcessRunnerConfig` (default loop budget), `RunnerError` (structured
//! library error), and the `agent_config_exists` helper.
//! Test: `runner::tests::*` cover the full delegation path offline via a
//! scripted `LlmClientTrait` mock.

mod error;
mod in_process;

#[cfg(test)]
mod tests;

pub use error::RunnerError;
pub use in_process::{
    InProcessAgentRunner, InProcessRunnerConfig, RegistryFactory, agent_config_exists,
};
