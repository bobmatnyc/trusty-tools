//! Trusty Browser: broker, native host and MCP server for Chrome profiles.
//!
//! Why: the Python 0.7.0 package is being ported to Rust
//! (`docs/plans/trusty-browser-rust-port.md`). Phase 0 ships only this skeleton
//! and the parity fixtures captured from Python under `tests/fixtures/`.
//! What: no feature logic yet; the frame codec, proof, vault and registry
//! modules arrive in phase 1.

/// The Python package version this crate reproduces.
pub const PYTHON_PARITY_VERSION: &str = "0.7.0";
