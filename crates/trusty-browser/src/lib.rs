//! Trusty Browser: broker, native host and MCP server for Chrome profiles.
//!
//! Why: the Python 0.7.0 package is being ported to Rust
//! (`docs/plans/trusty-browser-rust-port.md`); every byte Python defines on
//! the wire or on disk must stay identical so no profile has to re-pair.
//! What: phase 1 ships the library layer: [`pyjson`] (Python-exact JSON),
//! [`frame`], [`refusal`], [`proof`], [`audit`], [`registry`] and [`vault`].
//! Sockets, grants and the MCP server arrive in later phases.

pub mod audit;
pub mod frame;
mod private;
pub mod proof;
pub mod pyjson;
pub mod refusal;
pub mod registry;
pub mod vault;

/// The Python package version this crate reproduces.
pub const PYTHON_PARITY_VERSION: &str = "0.7.0";
