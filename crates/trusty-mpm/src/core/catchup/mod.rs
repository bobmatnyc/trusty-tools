//! Re-export shim — catch-up engine now lives in `trusty-common` (PR1, #1762).
//!
//! Why: the orchestrator was extracted to `trusty_common::catchup` so both
//! trusty-mpm and trusty-code can share it. This shim re-exports the public
//! surface so all existing `crate::core::catchup::*` call sites compile
//! without change.
//! What: pub-use the entire `trusty_common::catchup` surface. Sub-modules
//! (`git`, `palace`, `state`, `session_finder`, `mpm_session`, `mpm_registry`)
//! are also re-exported by name so sub-module qualified paths still resolve.
//! Test: `cargo test -p trusty-mpm` — exercises re-exported types.
// CUTOVER BRIDGE — remove post-migration (#1762)

pub use trusty_common::catchup::*;
pub use trusty_common::catchup::{git, mpm_registry, mpm_session, palace, session_finder, state};
