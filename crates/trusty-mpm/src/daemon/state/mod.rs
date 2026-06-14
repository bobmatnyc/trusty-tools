//! Shared daemon state.
//!
//! Why: the HTTP API, the MCP server, the hook relay, and the dashboard feed
//! all read and mutate the same picture of the world — managed sessions, their
//! delegation trees, per-agent circuit breakers, recent hook events, and
//! per-session memory usage. A single `Arc`-shared, lock-guarded state keeps
//! them consistent and is the daemon's composition root for dependency
//! injection into request handlers.
//! What: [`DaemonState`] holds `DashMap`s keyed by `SessionId`/agent name plus
//! a bounded ring buffer of recent [`HookEventRecord`]s; methods provide the
//! typed mutations the rest of the daemon needs.
//! Test: `cargo test -p trusty-mpm-daemon` exercises registration, the hook
//! ring-buffer bound, and memory-pressure classification.

mod core;
mod overseer;
mod resources;
mod sessions;

#[cfg(test)]
mod tests;

pub use core::{
    DaemonState, EVENT_CHANNEL_CAPACITY, HOOK_HISTORY_LIMIT, PAIR_CODE_TTL, ReapResult,
};
