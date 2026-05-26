//! `trusty-analyzer` — sidecar code-analysis daemon for trusty-search.
//!
//! Library entrypoint. The previous multi-crate workspace was collapsed into
//! a single crate so the daemon can be published to crates.io as one package.
//! Each former crate is now a top-level module that re-exports its public API
//! at the same path it used to have, modulo the crate→module prefix change.

pub mod core;
pub mod embedder;
pub mod lang;
pub mod mcp;
// Why (issue #249): the `service` module is the axum HTTP daemon surface and
// only compiles when the `http-server` feature is enabled. Gating it keeps
// library consumers that only need the analysis core / dispatcher free of the
// axum + tower-http stack.
#[cfg(feature = "http-server")]
pub mod service;
pub mod types;
