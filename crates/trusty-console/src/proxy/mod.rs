//! Reverse-proxy module for trusty-console P1.
//!
//! Why: Operators want to reach each daemon's UI and API through the console's
//! single URL (`http://127.0.0.1:7788`) rather than remembering per-daemon
//! ports.  This module implements the `/proxy/{daemon}/{*path}` route that
//! forwards requests to the live daemon base URL resolved from the background
//! health-poll cache.
//! What: Two sub-modules — `routes` contains the axum extractor types and the
//! forwarding handler; this `mod.rs` re-exports the public surface and houses
//! the `KNOWN_DAEMONS` constant.
//! Test: Unit tests for URL construction live in `routes.rs`.

pub mod routes;

pub use routes::proxy_handler;
