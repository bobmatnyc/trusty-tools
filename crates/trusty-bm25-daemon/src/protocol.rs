//! JSON-RPC 2.0 envelope types for the BM25 daemon.
//!
//! Why: Both the daemon and the in-process `Bm25Client` (in `trusty-common`)
//! need to agree on the wire format. Centralising the shapes here keeps the
//! protocol versioned in one place and lets the client crate copy the wire
//! shape without taking a dependency on this binary.
//!
//! What: pure serde types describing the four RPC methods (`index`,
//! `search`, `delete`, `rebuild`), their results, and the JSON-RPC error
//! envelope. Numeric error codes follow the JSON-RPC 2.0 spec where
//! applicable (-32600 invalid request, -32601 method not found, -32700
//! parse error, -32603 internal error).
//!
//! Test: round-trip serde tests live in this module; integration coverage in
//! `tests/bm25_daemon.rs`.

use serde::{Deserialize, Serialize};

/// JSON-RPC method: insert / update a document by `doc_id`.
///
/// Why: keep the method-string literal in one place so both the daemon's
/// dispatch arm and the client cannot drift.
/// What: a `&'static str` constant exposed to the rest of the crate.
/// Test: covered by the integration test which sends this exact method name.
pub const METHOD_INDEX: &str = "index";

/// JSON-RPC method: search the index, returning top-K hits sorted by score.
pub const METHOD_SEARCH: &str = "search";

/// JSON-RPC method: remove a document by `doc_id`.
///
/// Why: reserved for the dream subprocess. The hot request path
/// (`Bm25Client`) deliberately treats this as an out-of-band capability —
/// drawer deletion is owned by the dream cycle, not the recall hot path.
pub const METHOD_DELETE: &str = "delete";

/// JSON-RPC method: drop every document and start from an empty index.
///
/// Why: reserved for the dream subprocess. Same access-discipline note as
/// [`METHOD_DELETE`] — the hot path must never invoke this.
pub const METHOD_REBUILD: &str = "rebuild";

/// JSON-RPC method: report how much corpus this daemon is actually serving.
///
/// Why: without it the protocol can answer "here are your hits" but not "is
/// there anything to hit". A palace indexed to 20% of its drawers and a palace
/// indexed to 100% return the same empty hit list for a query that misses, so
/// a caller cannot tell a genuine no-match from a corpus that was never
/// backfilled — the fail-open shape where partial coverage reads as working
/// coverage. `stats` is what makes those two states distinguishable.
/// What: takes no params; returns [`StatsResult`].
/// Test: `stats_result_round_trips`, `dispatch_request_handles_stats`.
pub const METHOD_STATS: &str = "stats";

/// JSON-RPC method: report which of the caller's `doc_id`s this daemon does
/// NOT hold.
///
/// Why: `stats` reports a COUNT, and a count cannot answer "does the daemon
/// hold a document for every drawer this palace has". The two diverge as soon
/// as the corpus contains a document the palace no longer has — a drawer that
/// was forgotten, or a doc id that predates a rename — because `doc_count`
/// then measures a different set than the caller is asking about. A caller
/// that compares `doc_count >= my_drawer_count` reads "covered" off a corpus
/// that shares no ids with it at all. This method compares the actual sets.
/// What: params `{"doc_ids": [..]}`; returns [`MissingDocsResult`] naming the
/// subset the daemon has never seen. Extra documents the daemon holds are not
/// reported — they do not affect coverage of the ids asked about.
/// Test: `missing_docs_result_round_trips`, `dispatch_request_handles_missing_docs`.
pub const METHOD_MISSING_DOCS: &str = "missing_docs";

/// JSON-RPC version string. The protocol mandates this exact value.
pub const JSONRPC_VERSION: &str = "2.0";

// ── Error codes ────────────────────────────────────────────────────────────

/// JSON-RPC: malformed request envelope (e.g. missing required fields).
pub const ERR_INVALID_REQUEST: i32 = -32600;

/// JSON-RPC: requested method does not exist.
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;

/// JSON-RPC: payload could not be parsed as JSON at all.
pub const ERR_PARSE: i32 = -32700;

/// JSON-RPC: server-side failure while executing the method.
pub const ERR_INTERNAL: i32 = -32603;

/// Inbound JSON-RPC request envelope.
///
/// Why: the daemon reads one of these per newline-framed payload. Using a
/// dedicated struct lets serde reject malformed messages with a precise error
/// rather than parsing into a free-form `Value` and post-validating.
/// What: standard JSON-RPC 2.0 request fields. `id` is stored as
/// `serde_json::Value` so the daemon can echo the caller's id verbatim
/// (clients sometimes use strings, numbers, or `null`).
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
}

/// Parameters for the `index` method.
///
/// Why: typed params make decoding explicit — a missing `doc_id` or `text`
/// field is rejected at parse time rather than at the call boundary.
/// What: both fields are required; empty strings are accepted (an empty
/// text indexes a doc with zero tokens, which never matches a query).
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexParams {
    pub doc_id: String,
    pub text: String,
}

/// Successful result for the `index` method.
///
/// Why: callers want to distinguish a successful append from a corpus-cap
/// drop without parsing a free-form `serde_json::Value`.
/// What: a single bool. Updates of an existing `doc_id` always set
/// `indexed: true` because they don't grow the corpus.
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexResult {
    pub indexed: bool,
}

/// Parameters for the `search` method.
///
/// Why: callers always supply both a query and a top-K cap; making both
/// required at the type level avoids surprising defaults.
/// What: `query` is the human-supplied text; `top_k` is the maximum number
/// of hits to return (the worker clamps to ≥ 1).
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchParams {
    pub query: String,
    pub top_k: usize,
}

/// Parameters for the `delete` method — reserved for the dream subprocess.
///
/// Why: removal must be addressable by the same `doc_id` callers used when
/// indexing. The request hot path never sends this; only out-of-band
/// maintenance does.
/// What: single field, `doc_id: String`.
/// Test: covered by `dispatch_request_handles_delete` in `server.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteParams {
    pub doc_id: String,
}

/// Successful result for the `delete` method.
///
/// Why: lets the caller (the dream subprocess) reconcile its own state when
/// the daemon already lacked the requested id.
/// What: `deleted: false` when the id wasn't present; otherwise `true`.
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteResult {
    pub deleted: bool,
}

/// Successful result for the `rebuild` method.
///
/// Why: callers want to know how many docs survived a rebuild for
/// observability and to assert against an external source of truth.
/// What: the live document count after the rebuild finishes (zero on a
/// from-scratch rebuild, which is the only mode currently supported).
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebuildResult {
    pub doc_count: usize,
}

/// Successful result for the `stats` method.
///
/// Why: two numbers answer two different questions. `doc_count` answers "is
/// this palace indexed, and to what extent" — a backfiller compares it against
/// the palace's drawer count to decide between skip, repair, and full run.
/// `total_text_bytes` answers "how much memory is this daemon's corpus worth"
/// — [`crate::index::PalaceBm25Index`] holds every document's full text in a
/// `BTreeMap`, so per-daemon RSS scales with this figure and a supervisor
/// enforcing an RSS cap wants it visible rather than inferred.
/// What: `doc_count` is the live document count (post-delete, post-rebuild);
/// `total_text_bytes` is the summed byte length of the retained document text.
/// Test: `stats_result_round_trips`, `dispatch_request_handles_stats`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResult {
    pub doc_count: usize,
    pub total_text_bytes: u64,
}

/// Params for the `missing_docs` method.
///
/// Why: the caller supplies the set it cares about rather than asking the
/// daemon to enumerate its whole corpus, so the answer is O(caller's ids) on
/// the wire and stays correct no matter how many stale documents the daemon
/// is still holding.
/// What: the doc ids to test for presence. An empty list is legal and yields
/// an empty answer.
/// Test: `missing_docs_result_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingDocsParams {
    pub doc_ids: Vec<String>,
}

/// Successful result for the `missing_docs` method.
///
/// Why: `missing.is_empty()` is a set statement — "the daemon holds a document
/// for every id you named" — which is the claim a backfiller actually needs
/// and the one a count cannot make. `checked` is echoed back so a caller can
/// tell a genuinely-empty answer from a request that was silently truncated.
/// What: `missing` is the subset of the requested ids the daemon does not
/// hold, in request order; `checked` is how many ids were examined.
/// Test: `missing_docs_result_round_trips`, `dispatch_request_handles_missing_docs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingDocsResult {
    pub missing: Vec<String>,
    pub checked: usize,
}

/// One hit returned by the `search` method.
///
/// Why: typed payload makes the wire shape explicit and lets clients
/// destructure into `(doc_id, score)` without poking at `serde_json::Value`.
/// What: opaque string id (whatever the caller passed to `index`) paired
/// with a non-negative BM25 score.
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub doc_id: String,
    pub score: f32,
}

/// Successful result for the `search` method.
///
/// Why: wrapping the array in a named struct gives us forward-compat room
/// (e.g. a `total_matched` counter in a future revision).
/// What: `hits` is sorted by `score` descending, capped at the request's
/// `top_k`.
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
}

/// JSON-RPC 2.0 error object.
///
/// Why: structured errors let clients distinguish "method not found" from
/// "internal failure" without string-matching on `message`.
/// What: `code` follows the JSON-RPC 2.0 numeric scheme (see the `ERR_*`
/// constants). `message` is a short human-readable description.
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

/// Outbound JSON-RPC response envelope.
///
/// Why: exactly one of `result` / `error` is populated, mirroring the JSON-RPC
/// 2.0 spec. Using `skip_serializing_if` keeps the wire output clean.
/// What: `id` echoes the request id verbatim. On parse failure (no id
/// recoverable) we emit `id = null`.
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    pub id: serde_json::Value,
}

impl RpcResponse {
    /// Build a success response.
    ///
    /// Why: keeps the boilerplate of "jsonrpc=2.0, error=None" off every call
    /// site so dispatch code reads as a straight value translation.
    /// What: wraps `result` as `Some(serde_json::Value)` and the supplied id.
    /// Test: covered by the integration test's success path.
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Build an error response.
    ///
    /// Why: same DRY motivation as [`Self::ok`].
    /// What: wraps `error` as `Some(RpcError)` with the supplied code/message
    /// and the caller's id (or `null` when no id could be recovered).
    /// Test: covered by the integration test's error paths.
    pub fn err(id: serde_json::Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
            id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_request_round_trips() {
        let raw = r#"{"jsonrpc":"2.0","method":"index","params":{"doc_id":"d1","text":"hello world"},"id":"abc"}"#;
        let parsed: RpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.method, "index");
        let params: IndexParams = serde_json::from_value(parsed.params.unwrap()).unwrap();
        assert_eq!(params.doc_id, "d1");
        assert_eq!(params.text, "hello world");
    }

    #[test]
    fn search_request_round_trips() {
        let raw = r#"{"jsonrpc":"2.0","method":"search","params":{"query":"cargo test","top_k":10},"id":1}"#;
        let parsed: RpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.method, "search");
        let params: SearchParams = serde_json::from_value(parsed.params.unwrap()).unwrap();
        assert_eq!(params.query, "cargo test");
        assert_eq!(params.top_k, 10);
    }

    #[test]
    fn search_result_round_trips() {
        let res = SearchResult {
            hits: vec![
                SearchHit {
                    doc_id: "a".into(),
                    score: 1.5,
                },
                SearchHit {
                    doc_id: "b".into(),
                    score: 0.5,
                },
            ],
        };
        let s = serde_json::to_string(&res).unwrap();
        let back: SearchResult = serde_json::from_str(&s).unwrap();
        assert_eq!(back.hits.len(), 2);
        assert_eq!(back.hits[0].doc_id, "a");
    }

    #[test]
    fn index_result_serialises_with_indexed_flag() {
        let r = IndexResult { indexed: true };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"indexed\":true"));
    }

    #[test]
    fn delete_result_round_trips() {
        let r = DeleteResult { deleted: false };
        let s = serde_json::to_string(&r).unwrap();
        let back: DeleteResult = serde_json::from_str(&s).unwrap();
        assert!(!back.deleted);
    }

    #[test]
    fn rebuild_result_round_trips() {
        let r = RebuildResult { doc_count: 42 };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"doc_count\":42"));
        let back: RebuildResult = serde_json::from_str(&s).unwrap();
        assert_eq!(back.doc_count, 42);
    }

    #[test]
    fn ok_response_serialises_without_error_field() {
        let resp = RpcResponse::ok(
            serde_json::json!("abc"),
            serde_json::json!({"hits":[{"doc_id":"d1","score":1.0}]}),
        );
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"result\""));
        assert!(!s.contains("\"error\""));
        assert!(s.contains("\"id\":\"abc\""));
    }

    #[test]
    fn err_response_serialises_without_result_field() {
        let resp = RpcResponse::err(serde_json::Value::Null, ERR_INTERNAL, "boom");
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"error\""));
        assert!(!s.contains("\"result\""));
        assert!(s.contains("\"code\":-32603"));
    }

    #[test]
    fn missing_params_decodes_as_none() {
        let raw = r#"{"jsonrpc":"2.0","method":"index","id":1}"#;
        let parsed: RpcRequest = serde_json::from_str(raw).unwrap();
        assert!(parsed.params.is_none());
    }

    #[test]
    fn delete_request_round_trips() {
        let raw = r#"{"jsonrpc":"2.0","method":"delete","params":{"doc_id":"d1"},"id":3}"#;
        let parsed: RpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.method, "delete");
        let params: DeleteParams = serde_json::from_value(parsed.params.unwrap()).unwrap();
        assert_eq!(params.doc_id, "d1");
    }

    #[test]
    fn rebuild_request_round_trips_with_empty_params() {
        let raw = r#"{"jsonrpc":"2.0","method":"rebuild","params":{},"id":4}"#;
        let parsed: RpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.method, "rebuild");
    }

    #[test]
    fn stats_result_round_trips() {
        let r = StatsResult {
            doc_count: 1311,
            total_text_bytes: 4_194_304,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"doc_count\":1311"));
        let back: StatsResult = serde_json::from_str(&s).unwrap();
        assert_eq!(back.doc_count, 1311);
        assert_eq!(back.total_text_bytes, 4_194_304);
    }

    #[test]
    fn stats_request_round_trips_without_params() {
        let raw = r#"{"jsonrpc":"2.0","method":"stats","id":5}"#;
        let parsed: RpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.method, METHOD_STATS);
        assert!(parsed.params.is_none());
    }

    /// Why: `missing` is what a caller reads coverage off, so the wire shape
    /// has to survive a round trip exactly — a dropped or renamed field would
    /// decode as "nothing missing", which is the fail-open direction.
    /// Test: this test itself.
    #[test]
    fn missing_docs_result_round_trips() {
        let params = MissingDocsParams {
            doc_ids: vec!["a".into(), "b".into()],
        };
        let raw = serde_json::to_string(&params).unwrap();
        let back: MissingDocsParams = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.doc_ids, vec!["a".to_string(), "b".to_string()]);

        let result = MissingDocsResult {
            missing: vec!["b".into()],
            checked: 2,
        };
        let raw = serde_json::to_string(&result).unwrap();
        assert!(raw.contains("\"missing\":[\"b\"]"), "got: {raw}");
        let back: MissingDocsResult = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.missing, vec!["b".to_string()]);
        assert_eq!(back.checked, 2);
    }
}
