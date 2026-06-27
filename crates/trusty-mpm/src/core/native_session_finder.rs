//! Re-export shim — session finder now lives in `trusty-common` (PR1, #1762).
//!
//! Why: the unified paused-session discovery was extracted to
//! `trusty_common::catchup::session_finder` so trusty-code can share it.
//! This shim re-exports the public surface so all existing
//! `crate::core::native_session_finder::*` call sites compile without change.
//! What: pub-use `PausedSession`, `find_paused_sessions`, `render_resume_context`.
//! Test: existing `trusty_mpm::core::native_session_finder` tests now live in
//! `trusty_common::catchup::session_finder` — run via
//! `cargo test -p trusty-common --features catchup`.
// CUTOVER BRIDGE — remove post-migration (#1762)

pub use trusty_common::catchup::session_finder::{
    PausedSession, find_paused_sessions, render_resume_context,
};
