//! Re-export shim — claude-mpm registry reader now lives in `trusty-common` (PR1, #1762).
//!
//! Why: the registry discovery was extracted to
//! `trusty_common::catchup::mpm_registry` so trusty-code can share it.
//! This shim re-exports the public surface so all existing
//! `crate::core::claude_mpm_registry::*` call sites compile without change.
//! What: pub-use `discover_claude_mpm_projects`, `default_registry_path`.
//! Test: existing tests now live in `trusty_common::catchup::mpm_registry` —
//! run via `cargo test -p trusty-common --features catchup`.
// CUTOVER BRIDGE — remove post-migration (#1762)

pub use trusty_common::catchup::mpm_registry::{
    default_registry_path, discover_claude_mpm_projects,
};
