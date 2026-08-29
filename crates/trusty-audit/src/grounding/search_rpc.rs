//! The one way this crate calls the running `trusty-search` daemon (#6285).
//!
//! Why: three legs of grounding reached trusty-search over loopback HTTP — the
//! readiness probe in [`super::daemons`], the index-root backstop in
//! [`super::index`], and the per-dimension evidence queries in
//! [`super::evidence`]. ADR-0032 retires that listener: trusty-search binds one
//! hardened Unix socket at the path [`trusty_common::daemon_socket_path`]
//! derives, and speaks the framed JSON-RPC envelope [`trusty_common::uds`]
//! defines. There is no address to discover, no port to fall back to, and
//! nothing for a stale `http_addr` left by an older daemon to disagree with.
//!
//! What: [`search_socket`] derives the path both ends compute, and [`call`]
//! writes one frame and reads one back. Every trusty-search call in this crate
//! goes through here — a second dial or a second path resolver would be two
//! answers to one question.
//!
//! ## The three outcomes stay apart
//!
//! Every leg above is fail-open, and fail-open only works when "the daemon is
//! gone" is distinguishable from "the daemon answered, and the answer was no".
//! [`SearchRpcFailure`] keeps them apart: [`SearchRpcFailure::Unreachable`] is
//! an absence of information, [`SearchRpcFailure::Refused`] is a verdict the
//! daemon reached, and [`SearchRpcFailure::Malformed`] is a daemon that
//! answered something this crate cannot read. None of the three is ever a
//! success — an error frame that read as one would let a dead index render as a
//! clean bill of health.
//!
//! ## The method names are literals, deliberately
//!
//! trusty-audit has no Cargo edge on trusty-search, so the names cannot be
//! imported; they are pinned by `trusty_search::service::socket::METHODS`,
//! which the daemon's own `rpc_router_registers_every_documented_method`
//! compares its router against. A name that drifted answers `method_not_found`,
//! which arrives here as a [`SearchRpcFailure::Refused`] and reaches the
//! operator as a named gap.
//!
//! Test: `search_rpc_tests`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// Environment variable that pins the daemon's socket path explicitly.
///
/// Why: it replaces `TRUSTY_DATA_DIR` as the way a rig points this crate at a
/// daemon it started, without redirecting every other trusty-* client in the
/// same process. Same shape as [`super::daemons::ENV_ANALYZE_SOCKET`] and
/// trusty-mpm's `TRUSTY_SEARCH_SOCKET` — the same variable name, so an operator
/// pins one daemon for both crates with one export.
pub const ENV_SEARCH_SOCKET: &str = "TRUSTY_SEARCH_SOCKET";

/// Liveness, index count and the daemon's version.
pub const METHOD_HEALTH: &str = "search.health";

/// One index's stages, capabilities and footprint — `GET /indexes/{id}/status`.
pub const METHOD_INDEX_STATUS: &str = "search.index.status";

/// Hybrid search within one index — `POST /indexes/{id}/search`.
pub const METHOD_QUERY: &str = "search.query";

/// Response-frame budget for a query, in bytes.
///
/// Why not the shared [`trusty_common::uds::rpc::MAX_FRAME_BYTES`] default of
/// 8 MiB: a discovery pass asks for up to 64 chunks per query, and the HTTP
/// route it replaces had no response limit at all. Slice 5.5 raised the
/// daemon's own accept budget to 64 MiB (`trusty_search::service::socket::
/// MAX_FRAME_BYTES`) precisely so both ends could carry a bulk payload; a
/// client left at the control-plane default would refuse a frame the daemon was
/// willing to send, and the evidence leg would degrade for a reason that is not
/// the daemon's.
///
/// Test: `search_rpc_tests::the_query_budget_matches_the_daemons_accept_budget`.
pub const QUERY_MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// Why one call to the daemon produced no result.
///
/// Why a typed error rather than a formatted string: see the module docs — the
/// three cases are three different facts and each leg here reports them
/// differently.
/// What: `Display` renders one line, safe to show the recipient, in each case.
/// Test: `search_rpc_tests::each_failure_renders_one_line`.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum SearchRpcFailure {
    /// The socket could not be dialled, or the exchange failed. An absence of
    /// information, never a verdict.
    #[error("trusty-search did not answer {method} on {at} ({cause})")]
    Unreachable {
        /// The method that was attempted.
        method: String,
        /// The socket that was dialled.
        at: String,
        /// What the transport reported.
        cause: String,
    },
    /// The daemon answered, and the answer was an error frame.
    #[error("trusty-search refused {method} on {at} ({code}: {message})")]
    Refused {
        /// The method that was called.
        method: String,
        /// The socket that was dialled.
        at: String,
        /// The daemon's own JSON-RPC error code.
        code: i64,
        /// The daemon's own message.
        message: String,
    },
    /// The daemon answered something this crate cannot read.
    #[error("trusty-search answered {method} on {at} with an unreadable body ({cause})")]
    Malformed {
        /// The method that was called.
        method: String,
        /// The socket that was dialled.
        at: String,
        /// What could not be read.
        cause: String,
    },
}

impl SearchRpcFailure {
    /// True when the daemon ANSWERED and refused, rather than not answering.
    ///
    /// Why: a future caller that must not act on silence needs this
    /// distinction, and a string comparison against [`std::fmt::Display`]
    /// would be a second contract. `root_matches` (see [`super::index`]) does not call
    /// this — it discards the `Result` and stays fail-open on every variant
    /// by design, so today this is exercised only by its own tests.
    /// Test: `search_rpc_tests::a_dead_socket_is_unreachable_not_a_refusal`.
    #[must_use]
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Refused { .. })
    }
}

/// The socket the trusty-search daemon binds: [`ENV_SEARCH_SOCKET`], else the
/// derived default.
///
/// Why derived rather than read from a discovery file: the daemon computes the
/// same path from the same call (`trusty_search::service::socket::socket_path`),
/// so there is nothing published between them for a stale write to contradict.
/// A resolution failure yields an empty path, which every caller then reports as
/// unreachable — the same outcome a wrong path would produce, without the guess.
/// This mirrors [`super::daemons::analyze_socket`] exactly.
/// Test: `search_rpc_tests::an_absent_or_empty_override_falls_back_to_the_default_socket`.
#[must_use]
pub fn search_socket() -> PathBuf {
    socket_from_override(std::env::var(ENV_SEARCH_SOCKET).ok().as_deref())
}

/// The override rule itself: a non-empty value wins, everything else defaults.
///
/// Split out so the rule is asserted without any test reading or writing the
/// process environment — `set_var` is `unsafe` in edition 2024 and unsound under
/// the parallel harness.
fn socket_from_override(value: Option<&str>) -> PathBuf {
    match value.filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => trusty_common::daemon_socket_path("trusty-search").unwrap_or_default(),
    }
}

/// Call one method on the daemon at `socket` and return its `result`.
///
/// # Errors
///
/// [`SearchRpcFailure`], one variant per outcome that is not a result frame.
///
/// Test: `search_rpc_tests::a_dead_socket_is_unreachable_not_a_refusal`.
pub async fn call(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, SearchRpcFailure> {
    call_capped(
        socket,
        method,
        params,
        timeout,
        trusty_common::uds::rpc::MAX_FRAME_BYTES,
    )
    .await
}

/// [`call`] with an explicit response-frame budget — see [`QUERY_MAX_FRAME_BYTES`].
///
/// # Errors
///
/// The same set [`call`] returns; a response over `max_frame_bytes` arrives as
/// [`SearchRpcFailure::Unreachable`], because a refused frame is a transport
/// outcome rather than a verdict the daemon reached.
///
/// Test: `search_rpc_tests::an_error_frame_is_a_refusal_carrying_the_daemons_code`.
pub async fn call_capped(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
    max_frame_bytes: u64,
) -> Result<Value, SearchRpcFailure> {
    let at = socket.display().to_string();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let response: trusty_common::uds::server::RpcResponse =
        trusty_common::uds::send_framed_request_capped(socket, &request, timeout, max_frame_bytes)
            .await
            .map_err(|e| SearchRpcFailure::Unreachable {
                method: method.to_owned(),
                at: at.clone(),
                cause: e.to_string(),
            })?;
    if let Some(error) = response.error {
        return Err(SearchRpcFailure::Refused {
            method: method.to_owned(),
            at,
            code: error.code,
            message: error.message,
        });
    }
    response.result.ok_or(SearchRpcFailure::Malformed {
        method: method.to_owned(),
        at,
        cause: "neither a result nor an error".to_owned(),
    })
}

#[cfg(test)]
mod search_rpc_tests {
    use super::*;

    /// #6285: the names this crate cannot import. A drift here answers
    /// `method_not_found` against a daemon that is perfectly healthy.
    #[test]
    fn the_method_names_match_the_daemons_registered_spelling() {
        assert_eq!(METHOD_HEALTH, "search.health");
        assert_eq!(METHOD_INDEX_STATUS, "search.index.status");
        assert_eq!(METHOD_QUERY, "search.query");
        assert_eq!(ENV_SEARCH_SOCKET, "TRUSTY_SEARCH_SOCKET");
    }

    /// The query budget is the daemon's accept budget, not the shared default —
    /// a smaller one would refuse a frame the daemon was willing to send.
    #[test]
    fn the_query_budget_matches_the_daemons_accept_budget() {
        assert_eq!(QUERY_MAX_FRAME_BYTES, 64 * 1024 * 1024);
        const { assert!(QUERY_MAX_FRAME_BYTES > trusty_common::uds::rpc::MAX_FRAME_BYTES) };
    }

    #[test]
    fn an_absent_or_empty_override_falls_back_to_the_default_socket() {
        let derived = trusty_common::daemon_socket_path("trusty-search").unwrap_or_default();
        assert_eq!(socket_from_override(None), derived);
        assert_eq!(socket_from_override(Some("")), derived);
        assert_eq!(
            socket_from_override(Some("/tmp/pinned-search.sock")),
            PathBuf::from("/tmp/pinned-search.sock")
        );
    }

    /// The fail-open distinction the module exists for: nothing listening is an
    /// absence of information, never a verdict a caller may act on.
    #[tokio::test]
    async fn a_dead_socket_is_unreachable_not_a_refusal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("absent.sock");
        let err = call(
            &socket,
            METHOD_HEALTH,
            serde_json::json!({}),
            Duration::from_secs(2),
        )
        .await
        .expect_err("nothing is listening");
        assert!(!err.is_refusal(), "{err}");
        assert!(matches!(err, SearchRpcFailure::Unreachable { .. }), "{err}");
    }

    /// The other side of it: a daemon that answers an error frame reached a
    /// verdict, and that must never read as a result.
    #[tokio::test]
    async fn an_error_frame_is_a_refusal_carrying_the_daemons_code() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("refusing.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move {
            while let Ok((mut conn, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut sink = Vec::new();
                let _ = conn.read_to_end(&mut sink).await;
                let _ = conn
                    .write_all(
                        br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"no such index"}}"#,
                    )
                    .await;
                let _ = conn.write_all(b"\n").await;
                let _ = conn.flush().await;
            }
        });

        let err = call(
            &socket,
            METHOD_INDEX_STATUS,
            serde_json::json!({"index_id": "acme-api"}),
            Duration::from_secs(5),
        )
        .await
        .expect_err("the daemon refused");
        assert!(err.is_refusal(), "{err}");
        let SearchRpcFailure::Refused { code, message, .. } = &err else {
            panic!("{err}");
        };
        assert_eq!(*code, -32004);
        assert_eq!(message, "no such index");
    }

    /// A result frame is the ONLY success, and it comes back verbatim.
    #[tokio::test]
    async fn a_result_frame_comes_back_as_the_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("answering.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move {
            while let Ok((mut conn, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut sink = Vec::new();
                let _ = conn.read_to_end(&mut sink).await;
                let _ = conn
                    .write_all(br#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","indexes":3}}"#)
                    .await;
                let _ = conn.write_all(b"\n").await;
                let _ = conn.flush().await;
            }
        });

        let result = call(
            &socket,
            METHOD_HEALTH,
            serde_json::json!({}),
            Duration::from_secs(5),
        )
        .await
        .expect("the daemon answered");
        assert_eq!(result["status"], "ok");
        assert_eq!(result["indexes"], 3);
    }

    /// Every rendering stays one line: these reach the recipient's report.
    #[test]
    fn each_failure_renders_one_line() {
        for failure in [
            SearchRpcFailure::Unreachable {
                method: METHOD_HEALTH.to_owned(),
                at: "/tmp/s.sock".to_owned(),
                cause: "connection refused".to_owned(),
            },
            SearchRpcFailure::Refused {
                method: METHOD_QUERY.to_owned(),
                at: "/tmp/s.sock".to_owned(),
                code: -32601,
                message: "no such method".to_owned(),
            },
            SearchRpcFailure::Malformed {
                method: METHOD_INDEX_STATUS.to_owned(),
                at: "/tmp/s.sock".to_owned(),
                cause: "neither a result nor an error".to_owned(),
            },
        ] {
            let line = failure.to_string();
            assert_eq!(line.lines().count(), 1, "must stay one line: {line}");
            assert!(line.contains("trusty-search"), "{line}");
            assert!(line.contains("/tmp/s.sock"), "{line}");
        }
    }
}
