//! Re-export shim — claude-mpm session parsing now lives in `trusty-common` (PR1, #1762).
//!
//! Why: the ClaudeMpmSession type and its loaders were extracted to
//! `trusty_common::catchup::mpm_session` so trusty-code can share them.
//! This shim re-exports the public surface so all existing
//! `crate::core::claude_mpm_session::*` call sites compile without change.
//! What: pub-use `ClaudeMpmSession`, `load_latest_claude_mpm_session`,
//! `load_all_claude_mpm_sessions`.
//! Test: existing tests now live in `trusty_common::catchup::mpm_session` —
//! run via `cargo test -p trusty-common --features catchup`.
// CUTOVER BRIDGE — remove post-migration (#1762)

pub use trusty_common::catchup::mpm_session::{
    ClaudeMpmSession, load_all_claude_mpm_sessions, load_latest_claude_mpm_session,
};
