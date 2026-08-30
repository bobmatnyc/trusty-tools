//! HTTP route handlers extracted from `server.rs` to keep that file under the
//! 500-SLOC production cap.
//!
//! Why: P2 (#1222) adds the trusty-mpm Sessions surface — a dozen new route
//! handlers. Adding them inline to `server.rs` would push it well over the cap.
//! Grouping the session/supervisor/auto-resume handlers here keeps `server.rs`
//! focused on the router, app state, and the pre-existing metrics/SPA handlers.
//! What: re-exports the `sessions` submodule's handlers and the shared
//! `McpHandleError` → HTTP response mapping helper, the `config` submodule (#1220
//! Config tab — `/api/console/config/mpm`), plus the `origin_guard` same-origin
//! middleware that protects the destructive write routes, plus the `deletes`
//! submodule (#6360 — `DELETE` a trusty-memory palace or a trusty-search index
//! by calling the owning daemon's own teardown), the `cleanup` submodule
//! (#6371 — prune stale index registrations in one batch, compact a palace),
//! and the `verdict` submodule both of those report through.
//! Test: each submodule carries its own `#[cfg(test)]` tests; the route wiring
//! is exercised by `server.rs`'s integration tests.

// #6380: the delete-time census re-check the batch prune applies to every id.
pub mod census_guard;
pub mod cleanup;
pub mod config;
pub mod deletes;
pub mod memory_rpc;
pub mod origin_guard;
pub mod sessions;
pub mod verdict;

use std::time::Duration;

/// The service id the poller cache keys trusty-search's base URL under.
pub(crate) const SEARCH_SERVICE_ID: &str = "trusty-search";

/// The daemon whose socket carries `palace_delete` and `palace_compact`.
pub(crate) const MEMORY_SERVICE: &str = "trusty-memory";

/// How long one operator-driven daemon action may take, end to end.
///
/// Why not the 3s the health probe uses: tearing down a palace drops a redb
/// store and an HNSW graph, deregistering an index waits for in-flight writers
/// to quiesce, and compacting a palace walks its whole vector index. All three
/// are disk work bounded by corpus size, not by a round trip.
pub(crate) const ACTION_TIMEOUT: Duration = Duration::from_secs(30);
