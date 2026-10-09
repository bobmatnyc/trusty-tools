//! The two trusty-search reads the trace pass makes, and their failure taxonomy
//! (#6166).
//!
//! Why not `crate::integrations::search_client`: that client targets
//! `ReviewConfig::search_url` — the review pipeline's configured address — while
//! the report pass must address the daemon the audit actually indexed, which is
//! whatever `SearchTransport::resolve_advertised` resolves (every
//! `TRUSTY_DATA_DIR`-isolated instance included). It also needs two things
//! that trait does not carry: `call_chain`, whose body is `text/plain` rather
//! than JSON, and a `path_prefix`-scoped search. Address resolution and the
//! proxy-free client builder are both `trusty-common`'s, so nothing here is a
//! second implementation of either.
//!
//! What: [`TraceSource`] is the seam — [`HttpTraceSource`] talks to the daemon
//! over its socket or, until #9214 phase C, HTTP (see
//! [`crate::integrations::search_transport`]),
//! and a stub stands in for it under test. [`TraceError`] separates the three
//! failures that read identically at the socket but mean different things to a
//! reader: the daemon is not there, the index is not registered, the symbol is
//! not in the graph.
//!
//! Test: `trace_client_tests.rs`.

use async_trait::async_trait;
use serde::Deserialize;

use serde_json::json;
use trusty_common::search_rpc::METHOD_HEALTH;

use crate::integrations::search_client::{IndexInfo, SearchClientError};
use crate::integrations::search_transport::{
    METHOD_CALL_CHAIN, METHOD_QUERY, SearchTransport, call_socket,
};
use crate::report::index_registry::fetch_registered_indexes_via;

/// Why one trusty-search read could not answer.
///
/// Why: all three arrive as a failed HTTP call, and collapsing them would put
/// "trusty-search is down" and "this symbol is not in the graph" behind one
/// no-trace line. The remedies differ — start the daemon, index the checkout,
/// or accept that the symbol is genuinely absent — so the reader gets which.
/// What: `Unreachable` is a transport failure; `IndexAbsent` and `SymbolAbsent`
/// are the daemon's two distinct 404s, told apart by its error body; `Api` is
/// anything else it answered.
/// Test: `trace_client_tests::the_two_404_bodies_are_told_apart`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TraceError {
    /// The daemon did not answer at all.
    #[error("trusty-search is not reachable ({0})")]
    Unreachable(String),
    /// The daemon answered, and does not hold this index.
    #[error("index {0} is not registered with trusty-search")]
    IndexAbsent(String),
    /// The daemon holds the index, and its symbol graph has no such entry point.
    #[error("symbol {0} is not in the symbol graph")]
    SymbolAbsent(String),
    /// Any other non-2xx answer.
    #[error("trusty-search answered {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body, truncated by the caller.
        body: String,
    },
}

/// The `[ENTRY]` node of a call-chain report — the anchor, and nothing else.
///
/// Why: leg 1 keeps ONLY this node. The endpoint's edges resolve callees and
/// callers by bare symbol name, so on this workspace `UsearchStore::save`'s
/// callee list names `write` in `trusty-agents`, `size` in `trusty-progress`,
/// and `rename` in `trusty-code` — 254 of 321 callee edges cross a crate
/// boundary and 16 land in non-Rust files. Tracked as #6167; until it is fixed
/// an edge is noise a due-diligence reader would have to check by hand.
/// What: the symbol as the graph spells it, the file and line it was found at,
/// and the declaration signature the report renders.
/// Test: `trace_client_tests::the_entry_node_parses_out_of_a_call_chain_report`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallChainEntry {
    /// The symbol name the graph answered with.
    pub symbol: String,
    /// Repository-relative file holding the declaration.
    pub file: String,
    /// 1-based declaration line.
    pub line: u64,
    /// The declaration signature, or empty when the report carried none.
    pub signature: String,
}

/// One usage site of the traced symbol inside the finding's own file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceUsage {
    /// Repository-relative file.
    pub file: String,
    /// 1-based start line of the chunk.
    pub line: u64,
    /// Byte-bounded snippet of the chunk.
    pub snippet: String,
}

/// The two reads the trace pass makes, behind a seam.
///
/// Why: every one of the five fail-closed branches is an error arm, and an
/// error arm nothing exercises is an error arm that quietly stops firing. The
/// stub in `trace_tests.rs` returns each error in turn, so a change that drops one
/// fails a test rather than a live engagement.
/// What: `reachable` is the one-shot health probe; `entry_node` is the anchor
/// read; `usages` is the `path_prefix`-scoped search.
/// Test: `trace_tests.rs` (every arm), `trace_client_tests.rs` (the HTTP shapes).
#[async_trait]
pub trait TraceSource: Send + Sync {
    /// Whether the daemon answers `/health`.
    async fn reachable(&self) -> bool;

    /// The daemon's registered indexes, `root_path` included (#6677).
    ///
    /// Why: the pass addresses an index the audit registered, and the id it
    /// registered under need not be the one this checkout derives to. The
    /// registry is what tells them apart, and
    /// [`crate::report::index_registry::resolve_report_index`] is what reads it.
    /// What: the default is an empty list, which resolves to the derived id —
    /// the behaviour every stub had before #6677.
    /// Test: `trace_tests::an_index_registered_at_the_checkout_root_is_traced_against`.
    async fn registered_indexes(&self) -> Vec<IndexInfo> {
        Vec::new()
    }

    /// The `[ENTRY]` node for `symbol`, with every edge discarded.
    ///
    /// # Errors
    ///
    /// [`TraceError`] — see its variants.
    async fn entry_node(&self, index_id: &str, symbol: &str) -> Result<CallChainEntry, TraceError>;

    /// Usage sites of `symbol` restricted to `path_prefix`.
    ///
    /// # Errors
    ///
    /// [`TraceError`] — see its variants.
    async fn usages(
        &self,
        index_id: &str,
        symbol: &str,
        path_prefix: &str,
        limit: usize,
        snippet_bytes: usize,
    ) -> Result<Vec<TraceUsage>, TraceError>;
}

/// Whole-request bound for one trace read.
///
/// Longer than `trusty_common::http_client::LOOPBACK_REQUEST_TIMEOUT`'s 5s
/// because a call-chain render walks the whole symbol graph, and shorter than
/// anything a reader would call a hang.
const TRACE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// How much of an error body is quoted back into a no-trace line.
const MAX_ERROR_BODY: usize = 200;

/// Bound for the socket leg's liveness probe (#9214).
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The live [`TraceSource`].
pub struct HttpTraceSource {
    /// HTTP base URL, no trailing slash; empty on the socket leg.
    base_url: String, // #9214 phase C: delete
    /// Proxy-free loopback client.
    http: reqwest::Client, // #9214 phase C: delete
    /// #9214: which leg every read takes.
    transport: SearchTransport,
}

impl HttpTraceSource {
    /// Build a source against the daemon `trusty-search` itself advertises.
    ///
    /// Why: a fixed address misses every `TRUSTY_DATA_DIR`-isolated daemon.
    /// What: #9214 — [`SearchTransport::resolve_advertised`] picks the socket
    /// (`<TRUSTY_DATA_DIR>/trusty-search.sock` for an isolated instance), with
    /// no TCP fallback when its file is missing; then builds the shared
    /// proxy-free loopback client with [`TRACE_TIMEOUT`].
    /// `None` when the TLS backend will not initialise, which the caller
    /// reports as an unreachable daemon.
    /// Test: `search_transport_tests::missing_socket_fails_closed_without_tcp_on_the_advertised_leg`,
    /// `search_transport_tests::trace_entry_node_and_usages_go_over_the_socket`,
    /// `search_transport_tests::trusty_data_dir_isolates_the_default_socket`.
    #[must_use]
    pub fn resolved() -> Option<Self> {
        Self::with_transport(SearchTransport::resolve_advertised())
    }

    /// Build a source against an explicit base URL (the HTTP leg).
    #[must_use]
    pub fn at(base_url: impl Into<String>) -> Option<Self> {
        let url = base_url.into().trim_end_matches('/').to_string();
        Self::with_transport(SearchTransport::Http(url)) // #9214 phase C: delete
    }

    /// Build a source over an already-resolved transport (#9214).
    #[must_use]
    pub fn with_transport(transport: SearchTransport) -> Option<Self> {
        let http = trusty_common::http_client::loopback_client_builder() // #9214 phase C: delete
            .timeout(TRACE_TIMEOUT)
            .build()
            .ok()?;
        let base_url = match &transport {
            SearchTransport::Http(url) => url.clone(), // #9214 phase C: delete
            SearchTransport::Socket(_) => String::new(),
        };
        Some(Self {
            base_url,
            http,
            transport,
        })
    }

    /// The base URL this source targets.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// The chunk shape `POST /indexes/{id}/search` returns, narrowed to what a
/// usage record needs.
#[derive(Debug, Deserialize)]
struct SearchHit {
    path: String,
    start_line: u64,
    #[serde(default)]
    compact_snippet: String,
    #[serde(default)]
    content: String,
}

/// The envelope those chunks arrive in.
#[derive(Debug, Deserialize)]
struct SearchBody {
    #[serde(default)]
    results: Vec<SearchHit>,
}

#[async_trait]
impl TraceSource for HttpTraceSource {
    async fn reachable(&self) -> bool {
        // #9214: the socket leg asks `search.health`.
        if let Some(socket) = self.transport.socket_path() {
            return call_socket(socket, METHOD_HEALTH, json!({}), PROBE_TIMEOUT)
                .await
                .is_ok();
        }
        // #9214 phase C: delete — the HTTP probe below.
        trusty_common::daemon_guard::probe_once(&format!("{}/health", self.base_url)).await
    }

    /// #6677: read from THIS source's daemon, so a test server and an
    /// auto-ported daemon are both addressed by the same code path.
    async fn registered_indexes(&self) -> Vec<IndexInfo> {
        fetch_registered_indexes_via(self.transport.clone()).await
    }

    async fn entry_node(&self, index_id: &str, symbol: &str) -> Result<CallChainEntry, TraceError> {
        // `direction=outgoing&max_depth=1&include_source=false` is the smallest
        // report that still carries the `[ENTRY]` node. Every edge in it is
        // discarded; asking for fewer is what keeps a symbol like `get` — 2391
        // caller edges on this workspace — from being paid for at all.
        // #9214: `search.call_chain` answers the same text as a bare string.
        if let Some(socket) = self.transport.socket_path() {
            let params = json!({
                "index_id": index_id,
                "entry_point": symbol,
                "direction": "outgoing",
                "max_depth": 1,
                "include_source": false,
            });
            let report = call_socket(socket, METHOD_CALL_CHAIN, params, TRACE_TIMEOUT)
                .await
                .map_err(|e| from_search_error(e, index_id, symbol))?;
            // #9214: a result we cannot read is an error, never a definite
            // "symbol absent" — the HTTP leg's unparseable-body shape.
            let Some(text) = report.as_str() else {
                return Err(TraceError::Api {
                    status: 200,
                    body: "non-string call_chain result".to_string(),
                });
            };
            return parse_entry_node(text)
                .ok_or_else(|| TraceError::SymbolAbsent(symbol.to_string()));
        }
        // #9214 phase C: delete — the HTTP leg, from here to the end of the fn.
        let url = format!("{}/indexes/{index_id}/call_chain", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("entry_point", symbol),
                ("direction", "outgoing"),
                ("max_depth", "1"),
                ("include_source", "false"),
            ])
            .send()
            .await
            .map_err(|e| TraceError::Unreachable(format!("GET {url}: {e}")))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| TraceError::Unreachable(format!("read body of {url}: {e}")))?;
        if status != 200 {
            return Err(classify(status, &body, index_id, symbol));
        }
        parse_entry_node(&body).ok_or_else(|| TraceError::SymbolAbsent(symbol.to_string()))
    }

    async fn usages(
        &self,
        index_id: &str,
        symbol: &str,
        path_prefix: &str,
        limit: usize,
        snippet_bytes: usize,
    ) -> Result<Vec<TraceUsage>, TraceError> {
        let body = json!({
            "text": symbol,
            "top_k": limit,
            "path_prefix": path_prefix,
        });
        // #9214: `search.query` nests the HTTP body under `body`.
        if let Some(socket) = self.transport.socket_path() {
            let params = json!({ "index_id": index_id, "body": body });
            let value = call_socket(socket, METHOD_QUERY, params, TRACE_TIMEOUT)
                .await
                .map_err(|e| from_search_error(e, index_id, symbol))?;
            let parsed: SearchBody =
                serde_json::from_value(value).map_err(|e| TraceError::Api {
                    status: 200,
                    body: format!("unparseable search body: {e}"),
                })?;
            return Ok(to_usages(parsed, limit, snippet_bytes));
        }
        // #9214 phase C: delete — the HTTP leg, from here to the end of the fn.
        let url = format!("{}/indexes/{index_id}/search", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| TraceError::Unreachable(format!("POST {url}: {e}")))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| TraceError::Unreachable(format!("read body of {url}: {e}")))?;
        if status != 200 {
            return Err(classify(status, &body, index_id, symbol));
        }
        let parsed: SearchBody = serde_json::from_str(&body).map_err(|e| TraceError::Api {
            status,
            body: format!("unparseable search body: {e}"),
        })?;
        Ok(to_usages(parsed, limit, snippet_bytes))
    }
}

/// Shape a search body's hits into bounded usage records.
fn to_usages(parsed: SearchBody, limit: usize, snippet_bytes: usize) -> Vec<TraceUsage> {
    parsed
        .results
        .into_iter()
        .take(limit)
        .map(|h| TraceUsage {
            file: h.path,
            line: h.start_line,
            snippet: bound(
                if h.compact_snippet.is_empty() {
                    &h.content
                } else {
                    &h.compact_snippet
                },
                snippet_bytes,
            ),
        })
        .collect()
}

/// Map a socket-leg failure onto the taxonomy (#9214).
///
/// `call_socket` already turned `-32004` into `Api{404}` with the daemon's
/// message, which names `unknown index` or `entry point not found` exactly as
/// the HTTP body did, so [`classify`] splits the two the same way.
/// Test: `search_transport_tests::trace_entry_node_and_usages_go_over_the_socket`.
fn from_search_error(e: SearchClientError, index_id: &str, symbol: &str) -> TraceError {
    match e {
        SearchClientError::Api { status, body } => classify(status, &body, index_id, symbol),
        other => TraceError::Unreachable(other.to_string()),
    }
}

/// Map a non-2xx answer onto the taxonomy.
///
/// Why: the daemon returns 404 for BOTH an unregistered index and an
/// unresolvable entry point, and the only thing separating them is the error
/// string. Guessing collapses two different remedies into one.
/// What: a 404 whose body names an unknown index is [`TraceError::IndexAbsent`];
/// a 404 naming a missing entry point is [`TraceError::SymbolAbsent`]; anything
/// else is [`TraceError::Api`] with a bounded body.
/// Test: `trace_client_tests::the_two_404_bodies_are_told_apart`.
fn classify(status: u16, body: &str, index_id: &str, symbol: &str) -> TraceError {
    if status == 404 {
        if body.contains("unknown index") {
            return TraceError::IndexAbsent(index_id.to_string());
        }
        if body.contains("entry point not found") {
            return TraceError::SymbolAbsent(symbol.to_string());
        }
    }
    TraceError::Api {
        status,
        body: bound(body, MAX_ERROR_BODY),
    }
}

/// Truncate to at most `max` bytes on a char boundary.
fn bound(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Pull the `[ENTRY]` node out of a call-chain report.
///
/// Why: the endpoint answers `text/plain`, so this is the parse. Only the entry
/// node is read — see [`CallChainEntry`] for why the edges below it are dropped.
/// What: finds the ``## `<symbol>` [ENTRY]  <file>:<line>`` header and the
/// `Signature:` line under it. `None` when no entry header is present, which the
/// caller treats as the symbol being absent.
/// Test: `trace_client_tests::{the_entry_node_parses_out_of_a_call_chain_report,
/// a_report_with_no_entry_header_parses_to_nothing}`.
pub fn parse_entry_node(report: &str) -> Option<CallChainEntry> {
    let mut lines = report.lines();
    let header = lines.find(|l| l.trim_start().starts_with("## ") && l.contains("[ENTRY]"))?;
    let symbol = header.split('`').nth(1)?.to_string();
    let (file, line) = header.split_once("[ENTRY]")?.1.trim().rsplit_once(':')?;
    let line: u64 = line.trim().parse().ok()?;
    let signature = lines
        .take_while(|l| !l.trim_start().starts_with("## "))
        .find_map(|l| l.trim_start().strip_prefix("Signature:"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    Some(CallChainEntry {
        symbol,
        file: file.trim().to_string(),
        line,
        signature,
    })
}

#[cfg(test)]
#[path = "trace_client_tests.rs"]
mod tests;
