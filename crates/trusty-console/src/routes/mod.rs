//! HTTP route handlers extracted from `server.rs` to keep that file under the
//! 500-SLOC production cap.
//!
//! Why: P2 (#1222) adds the trusty-mpm Sessions surface — a dozen new route
//! handlers. Adding them inline to `server.rs` would push it well over the cap.
//! Grouping the session/supervisor/auto-resume handlers here keeps `server.rs`
//! focused on the router, app state, and the pre-existing metrics/SPA handlers.
//! What: re-exports the `sessions` submodule's handlers and the shared
//! `McpHandleError` → HTTP response mapping helper.
//! Test: each submodule carries its own `#[cfg(test)]` tests; the route wiring
//! is exercised by `server.rs`'s integration tests.

pub mod sessions;
