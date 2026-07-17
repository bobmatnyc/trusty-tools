//! Read-only projection of a deployed agent's frontmatter — re-exported from
//! `trusty-agents-common`.
//!
//! Why: this module's implementation (`AgentMetadata`, `agent_metadata_from_str`,
//! `read_agent_metadata`) moved to `trusty-agents-common::agents::metadata`
//! (#2892) alongside `agent_deployer`, which depends on it
//! (`deploy_agents_filtered` populates `DeployResult::declared_skills` via
//! `agent_metadata_from_str`). This shim keeps every existing trusty-mpm call
//! site (`tm doctor`'s dangling-skill check, `tm agent list`/`show`)
//! compiling unchanged.
//! What: a blanket re-export of the shared crate's public `metadata` API.
//! Test: `cargo test -p trusty-agents-common agents::metadata` — the tests
//! moved with the implementation.

pub use trusty_agents_common::agents::metadata::*;
