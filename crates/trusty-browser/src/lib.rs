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
//! request journal and every page op ([`pages`], [`pages_url`]). Phase 5 adds
//! the `trusty-browser-mcp` stdio server: [`mcp_cli`] (flags and start-up),
//! [`mcp_server`] (JSON-RPC lines), [`mcp_tools`] (tool table and results),
//! [`mcp_args`] (FastMCP/pydantic argument rules), [`mcp_session`]
//! (`agent.sock` client), [`mcp_upload`] and [`mcp_mime`] (upload root).
//! Phase 4 adds the embedded extension and its publication ([`assets`]), the
//! Chrome host registration ([`install_host`]), the launchd job ([`launchd`]),
//! the CLI ([`cli`], [`cli_operator`]) and [`keychain_migrate`].

pub mod assets;
pub mod audit;
pub mod broker;
mod broker_agent;
mod broker_enroll;
mod broker_service;
pub mod casefold;
pub mod cli;
pub mod cli_operator;
pub mod codesign;
pub mod entropy;
pub mod frame;
pub mod grants;
pub mod install_host;
pub mod keychain_migrate;
pub mod launchd;
pub mod leases;
mod legacy;
pub mod mcp_args;
pub mod mcp_cli;
pub mod mcp_mime;
pub mod mcp_server;
pub mod mcp_session;
pub mod mcp_tools;
pub mod mcp_upload;
pub mod native_host;
pub mod pages;
pub mod pages_capture;
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
