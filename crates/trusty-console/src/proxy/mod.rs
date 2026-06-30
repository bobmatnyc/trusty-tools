//! Reverse-proxy module for trusty-console (#1849 Phase 2).
//!
//! Why: Operators want to reach each daemon's UI and API through the console's
//! single URL (`http://127.0.0.1:7788`) rather than remembering per-daemon
//! ports.  This module implements the primary `/api/{service}/{*path}` route
//! (Phase 2) and the deprecated `/proxy/{daemon}/{*path}` alias (backward
//! compatibility) that both forward requests to the live daemon base URL
//! resolved from the background health-poll cache.
//! What: `routes` contains the axum extractor types and the forwarding handlers.
//! This `mod.rs` re-exports the public surface (`proxy_handler`,
//! `deprecated_proxy_handler`).
//! Test: Unit tests for URL construction and normalization live in `routes.rs`.

pub mod routes;

pub use routes::deprecated_proxy_handler;
pub use routes::proxy_handler;
