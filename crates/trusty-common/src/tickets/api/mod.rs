//! Ticketing API surface.
//!
//! Why: Groups the canonical types, config loader, backend trait, and the
//! per-backend implementations under one `crate::api::*` namespace.
//! What: Re-exports the most commonly used items at module level.
//! Test: covered by the submodule tests.

pub mod backends;
pub mod client;
pub mod config;
pub mod models;
