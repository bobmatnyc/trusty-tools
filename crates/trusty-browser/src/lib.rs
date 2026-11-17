//! Trusty Browser: broker, native host and MCP server for Chrome profiles.
//!
//! Why: the Python 0.7.0 package is being ported to Rust
//! (`docs/plans/trusty-browser-rust-port.md`); every byte Python defines on
//! the wire or on disk must stay identical so no profile has to re-pair.
//! What: phase 1 shipped the library layer: [`pyjson`] (Python-exact JSON),
//! [`frame`], [`refusal`], [`proof`], [`audit`], [`registry`] and [`vault`].
//! Phase 2 adds the [`broker`] (four sockets, [`peer`] and [`peer_darwin`]
//! authentication, the pairing handshake, enrollment, grant and operator
//! ops) and the [`native_host`] relay. Phase 3 adds [`leases`], the
//! request journal and every page op ([`pages`], [`pages_url`]). The MCP
//! server arrives in a later phase.

pub mod audit;
pub mod broker;
mod broker_agent;
mod broker_enroll;
mod broker_service;
pub mod casefold;
pub mod entropy;
pub mod frame;
pub mod grants;
pub mod leases;
mod legacy;
pub mod native_host;
pub mod pages;
mod pages_ops;
pub mod pages_url;
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
