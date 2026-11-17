//! Trusty Browser: broker, native host and MCP server for Chrome profiles.
//!
//! Why: the Python 0.7.0 package is being ported to Rust
//! (`docs/plans/trusty-browser-rust-port.md`); every byte Python defines on
//! the wire or on disk must stay identical so no profile has to re-pair.
//! What: phase 1 shipped the library layer: [`pyjson`] (Python-exact JSON),
//! [`frame`], [`refusal`], [`proof`], [`audit`], [`registry`] and [`vault`].
//! Phase 2 adds the [`broker`] (four sockets, [`peer`] and [`peer_darwin`]
//! authentication, the pairing handshake, enrollment, grant and operator
//! ops) and the [`native_host`] relay. Leases, page ops and the MCP server
//! arrive in later phases.

pub mod audit;
pub mod broker;
mod broker_agent;
mod broker_enroll;
mod broker_service;
pub mod casefold;
pub mod entropy;
pub mod frame;
pub mod grants;
mod legacy;
pub mod native_host;
pub mod pairing;
pub mod peer;
pub mod peer_conn;
pub mod peer_darwin;
mod private;
pub mod proof;
pub mod pyjson;
pub mod refusal;
pub mod registry;
pub mod vault;
pub mod wire;

/// The Python package version this crate reproduces.
pub const PYTHON_PARITY_VERSION: &str = "0.7.0";
