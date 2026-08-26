//! Sidecar analysis daemon for trusty-analyzer.
//!
//! Why: Keeps analysis isolated from trusty-search. The daemon fetches chunks
//! from the search daemon over HTTP (`TrustySearchClient::get_chunks`) and
//! computes complexity / smells / quality / facts in-process. It does not
//! talk to trusty-search's redb files directly — the search daemon is the
//! single source of truth for chunk data.
//!
//! What: Thin coordinator module. Declares the submodules and re-exports the
//! public surface so callers import from `service` rather than the internal
//! submodules. The served surface is twenty JSON-RPC methods over a hardened
//! Unix socket — see [`rpc::METHODS`], which is the list, rather than a copy of
//! it here that would go stale.
//!
//! #6287 (ADR-0032) replaced the axum HTTP surface this module used to
//! describe. Three things went with it and are not coming back here: the
//! embedded admin UI (`/ui`), which mounts on `trusty-console` instead; the
//! `/sse` push stream, which had the UI as its only subscriber; and the
//! `http_addr` discovery file, which a derived socket path makes unnecessary.
//!
//! Test: `service::rpc_tests` drives every method over a real socket with a
//! stub search client.

mod diagnostics_dispatch;

pub mod events;
pub(crate) mod handlers;
pub mod rpc;

// Re-export the public API so callers can write `use crate::service::…`
// without knowing which submodule owns each item.
pub use events::AnalyzerAppState;
pub use rpc::{build_router, serve, socket_path, METHODS, METHOD_HEALTH};

/// Re-export so the binary can construct facts via the same path.
pub use crate::types::FactRecord as PublicFactRecord;
