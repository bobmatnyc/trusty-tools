//! Shared frontmatter key/value line parser — re-exported from
//! `trusty-agents-common`.
//!
//! Why: this module's implementation (`parse_kv_line`, `parse_list_value`)
//! moved to `trusty-agents-common::agents::frontmatter` (#2892) alongside
//! `agent_builder`, which depends on it. This shim keeps the existing
//! trusty-mpm call site (`crate::core::delegation_authority` imports
//! `super::frontmatter::parse_kv_line`) compiling unchanged.
//! What: a blanket re-export of the shared crate's public `frontmatter` API.
//! Test: `cargo test -p trusty-agents-common agents::frontmatter` — the
//! tests moved with the implementation.

pub use trusty_agents_common::agents::frontmatter::*;
