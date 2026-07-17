//! Ownership manifest for deployed agent files — re-exported from
//! `trusty-agents-common`.
//!
//! Why: this module's implementation (`AgentManifest`, `atomic_write`,
//! `checksum`, …) moved to `trusty-agents-common::agents::manifest` (#2892) so
//! `trusty-code` can eventually reuse the same ownership ledger instead of
//! forking it, mirroring the precedent set by `ToolExecutor`/`AgentRunner`.
//! This shim keeps every existing trusty-mpm call site
//! (`crate::core::agent_manifest::{AgentManifest, atomic_write, checksum, …}`)
//! compiling unchanged.
//! What: a blanket re-export of the shared crate's public `manifest` API.
//! Test: `cargo test -p trusty-agents-common agents::manifest` — the tests
//! moved with the implementation; this crate exercises the re-exported call
//! sites end-to-end via `cargo test -p trusty-mpm`.

pub use trusty_agents_common::agents::manifest::*;
