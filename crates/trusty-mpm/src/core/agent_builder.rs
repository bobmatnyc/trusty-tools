//! Agent inheritance resolution — re-exported from `trusty-agents-common`.
//!
//! Why: this module's implementation (the `extends:`-chain composer,
//! `compose_agent` / `source_chain`) moved to `trusty-agents-common::agents::builder`
//! (#2892) so `trusty-code` can eventually reuse the same composer instead of
//! forking it, mirroring the precedent set by `ToolExecutor`/`AgentRunner`.
//! This shim keeps every existing trusty-mpm call site
//! (`crate::core::agent_builder::{compose_agent, source_chain, AgentBuildError}`)
//! compiling unchanged.
//! What: a blanket re-export of the shared crate's public `builder` API.
//! Test: `cargo test -p trusty-agents-common agents::builder` — the tests
//! moved with the implementation; this crate exercises the re-exported call
//! sites end-to-end via `cargo test -p trusty-mpm`.

pub use trusty_agents_common::agents::builder::*;
