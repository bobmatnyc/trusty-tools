//! Service discovery module for `tm services`.
//!
//! Why: agents need a canonical, scriptable way to answer "is trusty-search
//! running?" and "what port is it on?" without resorting to lsof/curl/ps.
//! This module provides the manifest schema, discovery engine, and the types
//! consumed by the `tm services` CLI subcommands.
//! What: re-exports `ServicesManifest`, `ServiceDecl`, `PortDiscovery`,
//! `ManifestValidationError`, `Discoverer`, `ServiceStatus`, and `HealthState`.
//! Test: unit tests live in `manifest.rs` and `discoverer.rs`; the integration
//! smoke test is in `tests/services_integration.rs`.

pub mod discoverer;
pub mod manifest;

// #9543: trusty-search found by socket health, never by TCP.
#[cfg(test)]
mod uds_search_tests;

pub use discoverer::{
    CACHE_TTL, Discoverer, HEALTH_PROBE_TIMEOUT, HealthResult, HealthState, ServiceStatus,
};
pub use manifest::{
    ManifestValidationError, PortDiscovery, ServiceDecl, ServicesManifest, expand_tilde,
    expand_tilde_owned,
};
