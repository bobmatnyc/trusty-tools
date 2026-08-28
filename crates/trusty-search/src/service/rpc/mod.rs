//! The JSON-RPC methods the daemon serves on its Unix socket (#6285).
//!
//! Why: `service::socket` owns the transport — bind, peer check, framing,
//! accept loop — and nothing about which methods exist. This module is the
//! other half, split by route family so the slices that move the remaining
//! `service::server` surface across can each add one file rather than editing
//! one.
//!
//! What: a module list, plus the one encoding helper more than one family needs.
//! Every family registers itself into the router `socket::build_router`
//! assembles.
//!
//! Test: each family's own `*_tests.rs`.

/// One HTTP refusal becomes one JSON-RPC error frame.
pub mod error;

/// Registration for the read families: indexes, status, config, chunks, graph,
/// and call chain (#6285 slice 2).
pub mod reads;

/// Registration for the query families: hybrid search and its fan-out, grep and
/// its fan-out, code-to-code similarity, and typeahead (#6285 slice 3).
pub mod queries;

/// Registration for the write family: index create, delete, relocate, the two
/// per-file writes, the reindex trigger, and contributed-graph ingest
/// (#6285 slice 4).
pub mod writes;

/// Registration for the streaming families: the daemon status stream and one
/// index's reindex progress stream (#6285 slice 5).
pub mod streams;

/// Per-method admission and deadline lane pins for the whole socket surface
/// (#6285 slice 5, discharging the slice-4 review's deferred MEDIUM).
#[cfg(test)]
#[path = "lanes_tests.rs"]
mod lanes_tests;

/// A typed report as the JSON axum would have written for it.
///
/// Why: `serde_json::to_value`, which the router applies to a typed response,
/// WIDENS an `f32` field to `f64` — a typeahead hit's `score` of `0.001f32`
/// becomes `0.0010000000474974513` — while axum serialises the struct straight
/// to text and writes `0.001`. Both name the same `f32`, but they are different
/// digits on the wire, and this surface's whole contract is that the two
/// transports answer identically. HTTP's encoding is the one eleven crates read
/// today, so the socket matches it rather than the reverse.
/// What: serialise to text, then parse — the same two steps HTTP takes, in the
/// same order, so the `f32` shortest-representation is chosen once.
///
/// Only a family whose report is a TYPED struct needs this. A core that already
/// returns `serde_json::Value` holds the widened `f64` on BOTH transports, so
/// the two agree already. Slice 4 reaches it from `writes` as well as `queries`
/// — `IngestGraphResponse` carries no `f32` TODAY, and routing it through here
/// is what keeps that a detail of the struct rather than a thing a reader has to
/// check before adding a field.
///
/// # Errors
///
/// Never in practice: `T` is a report this daemon just built, and a report that
/// cannot serialise cannot be sent over HTTP either. Reported as an internal
/// error rather than unwrapped, because a panic here would take the connection
/// down instead of answering the caller.
/// Test: `typeahead_over_the_socket_matches_the_http_body`,
/// `grep_over_the_socket_matches_the_http_body`,
/// `graph_ingest_over_the_socket_matches_the_http_body`.
pub(crate) fn as_http_body<T: serde::Serialize>(
    report: T,
) -> Result<serde_json::Value, trusty_common::uds::server::RpcError> {
    use trusty_common::uds::server::{RpcError, CODE_INTERNAL_ERROR};

    let text = serde_json::to_string(&report).map_err(|e| {
        tracing::error!(error = %e, "rpc: a report could not be serialised");
        RpcError::new(
            CODE_INTERNAL_ERROR,
            format!("could not serialise the report: {e}"),
        )
    })?;
    serde_json::from_str(&text).map_err(|e| {
        tracing::error!(error = %e, "rpc: a serialised report could not be re-read");
        RpcError::new(
            CODE_INTERNAL_ERROR,
            format!("could not re-read the report: {e}"),
        )
    })
}
