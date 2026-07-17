//! Agent deployment — re-exported from `trusty-agents-common`.
//!
//! Why: this module's implementation (`deploy_agents`, `deploy_agents_filtered`,
//! `DeployResult`, `is_agent_file`) moved to
//! `trusty-agents-common::agents::deployer` (#2892) so `trusty-code` can
//! eventually reuse the same compose/ownership/atomic-write deploy pipeline
//! instead of forking it, mirroring the precedent set by
//! `ToolExecutor`/`AgentRunner`. This shim keeps every existing trusty-mpm
//! call site (`crate::core::agent_deployer::{deploy_agents, DeployResult, …}`)
//! compiling unchanged.
//! What: a blanket re-export of the shared crate's public `deployer` API.
//! Test: `cargo test -p trusty-agents-common agents::deployer` — the tests
//! moved with the implementation; this crate exercises the re-exported call
//! sites end-to-end via `cargo test -p trusty-mpm`.

pub use trusty_agents_common::agents::deployer::*;
