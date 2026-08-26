//! The monitor dashboard's client for the trusty-memory daemon.
//!
//! Why: the unified monitor dashboard needs a typed, testable transport to the
//! trusty-memory daemon's read-only surface. Until #6286 that was its own
//! `reqwest` client against `/health` and `/api/v1/*`; the daemon retired that
//! listener, so the calls now go through [`crate::memory_rpc`] and this module
//! is the projection layer on top of it.
//! What: [`MemoryClient`] wraps a socket path; a `fetch_all` helper folds the
//! status and palace calls into [`MemoryData`](crate::monitor::dashboard::MemoryData).
//! [`ActivityFeed`] is the live event stream, and what a caller reads to know
//! it has stopped and polling must take over.
//! Test: `cargo test -p trusty-common --features monitor-tui`.

mod client;
// The live activity stream that replaces the 2-second poll (#6286).
mod feed;
mod parsers;
#[cfg(test)]
mod tests;
mod types;

pub use client::MemoryClient;
pub use feed::ActivityFeed;
pub use parsers::{
    creator_label, parse_drawers, parse_dream_stats, parse_memory_details, parse_memory_event,
    parse_palace_detail, parse_recall_hits,
};
pub use types::{
    DrawerInfo, DreamStats, MemoryDetail, MemoryEvent, NO_CREATOR_LABEL, RecallHit,
    resolve_memory_socket,
};
