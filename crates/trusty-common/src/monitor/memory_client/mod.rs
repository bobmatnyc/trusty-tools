//! HTTP client for the trusty-memory daemon.
//!
//! Why: the unified monitor dashboard needs a typed, testable transport to the
//! trusty-memory daemon's read-only endpoints (`/health`, `/api/v1/status`,
//! `/api/v1/palaces`). Keeping it in its own module mirrors `search_client` and
//! isolates the wire shapes the memory panel renders.
//! What: [`MemoryClient`] wraps a base URL and a pooled `reqwest::Client`; a
//! `fetch_all` helper folds the status and palace calls into [`MemoryData`].
//! Test: `cargo test -p trusty-common --features monitor-tui` covers URL
//! resolution and base-URL storage; live endpoints are covered by the daemon's
//! own suite.

mod client;
mod parsers;
#[cfg(test)]
mod tests;
mod types;

pub use client::MemoryClient;
pub use parsers::{
    creator_label, parse_drawers, parse_dream_stats, parse_memory_details, parse_memory_event,
    parse_palaces, parse_recall_hits,
};
pub use types::{
    DEFAULT_MEMORY_URL, DrawerInfo, DreamStats, MemoryDetail, MemoryEvent, NO_CREATOR_LABEL,
    RecallHit, normalize_url, resolve_memory_url,
};
