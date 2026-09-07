//! Which trusty-analyze RPC method one `/api/analyze/…` request becomes (#6155).
//!
//! Why: trusty-analyze answers framed JSON-RPC on a Unix socket and the SPA
//! speaks HTTP paths. A socket has no paths, so the translation has to be
//! written down, and writing it down is what makes the mapped surface
//! reviewable: a request this table does not name is refused with `501` rather
//! than forwarded somewhere approximate.
//!
//! What: [`map_request`] turns a method, a path, a query string and a body into
//! one [`Call`]. The table covers every endpoint the dashboard at
//! `/tools/analyze/` calls — `crates/trusty-console/ui-analyze/src/lib/api.js`,
//! all of it, and nothing else.
//!
//! ## The path shapes are the SPA's, unchanged
//!
//! Each row's left-hand side is the path trusty-analyze's retired axum router
//! answered, minus the `/api/analyze/` prefix the console mounts this bridge at.
//! Keeping them means the SPA needed no rewrite: `base.js` reads the injected
//! `window.__ANALYZE_BASE__` and every `api.js` path resolves against it.
//!
//! ## One row renames a parameter, and it is the only one
//!
//! `api.js` sends `?top_k=` to `complexity_hotspots`, whose request type spells
//! that field `top_n` (`analysis::HotspotsRequest`). The retired axum route did
//! the rename in its query extractor; this table does it here, and
//! `the_hotspot_top_k_query_becomes_the_daemons_top_n` is what holds it. Every
//! other query key already matches its `params` field.
//!
//! ## No stream row exists, deliberately
//!
//! The SPA used to open an `EventSource` on `/sse`. #6287 deleted that route,
//! the `AnalyzerEvent` broadcast behind it, and put no streaming RPC method in
//! their place — `service::rpc::METHODS` has none. Bridging it would mean adding
//! a daemon-side endpoint, so the subscription was removed from the SPA instead
//! (`ui-analyze/src/lib/state.svelte.js`) and this table has no `Call::Stream`.
//!
//! ## Query values are coerced, and that is visible when it is wrong
//!
//! A query string is all text; the RPC params are typed. `top_k=20` has to
//! become `20` or the call answers `invalid_params`. [`query_json`] coerces the
//! two boolean literals and any integer and leaves everything else a string. A
//! string-valued parameter whose value is literally `true` or a bare integer
//! would be coerced wrongly — and would then be REFUSED by the daemon's own
//! deserialiser rather than silently mis-read, which is why the coercion is safe
//! to make blind.
//!
//! Test: `maps_every_endpoint_the_spa_calls`, `refuses_an_unmapped_path`,
//! `query_json_coerces_integers`, `query_json_is_an_empty_object_for_no_query`.

use axum::http::Method;
use serde_json::{Map, Value, json};

use super::{
    METHOD_CLUSTERS, METHOD_COMPLEXITY_HOTSPOTS, METHOD_FACTS_DELETE, METHOD_FACTS_LIST,
    METHOD_FACTS_UPSERT, METHOD_HEALTH, METHOD_LIST_INDEXES, METHOD_QUALITY,
    METHOD_REFACTOR_SUGGESTIONS, METHOD_SMELLS,
};

/// What one mapped request asks the daemon for.
///
/// Why an enum with one variant rather than a bare `(method, params)` pair: the
/// search and memory bridges both carry a `Stream` arm, and the three modules
/// are read side by side. Keeping the shape means an analyze stream — if the
/// daemon ever grows one — is a variant, not a signature change through
/// `routes.rs`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Call {
    /// One request, one answer. Every trusty-analyze method is this.
    Unary {
        /// The RPC method name.
        method: &'static str,
        /// The `params` object.
        params: Value,
    },
}

/// Turn one `/api/analyze/…` request into the call it stands for.
///
/// Why the `Err` is a sentence rather than a code: it is rendered into the `501`
/// body an operator reads, and naming the unmapped path is what makes the gap
/// actionable.
///
/// What: `path` is the sub-path with no leading slash — what axum's `{*path}`
/// captures, already percent-decoded. `body` is the raw request body, read only
/// by the one row that carries one (`POST /facts`).
///
/// # Errors
///
/// A path and method pair this table does not name, a fact id that is not a
/// number, and a body that is not JSON.
///
/// Test: `maps_every_endpoint_the_spa_calls`, `refuses_an_unmapped_path`,
/// `a_fact_delete_with_a_non_numeric_id_is_refused`.
pub(crate) fn map_request(
    method: &Method,
    path: &str,
    query: Option<&str>,
    body: &[u8],
) -> Result<Call, String> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let q = query_json(query);

    // Every `params` here is an OBJECT, never `null`, even for a method that
    // takes no arguments: a derived `Deserialize` refuses `null` outright, so a
    // no-argument call sent as `null` answers `invalid_params`.
    // trusty-analyze's `NoParams` accepts either; `{}` is what works for both.
    let unary = |method: &'static str, params: Value| Ok(Call::Unary { method, params });

    match (method, segments.as_slice()) {
        (&Method::GET, ["health"]) => unary(METHOD_HEALTH, json!({})),

        // ---- the index roster -------------------------------------------------
        (&Method::GET, ["indexes"]) => unary(METHOD_LIST_INDEXES, json!({})),

        // ---- one index's analysis ---------------------------------------------
        //
        // `top_k` is the SPA's spelling and `top_n` the daemon's; see the module
        // docs. Nothing else on this row is renamed.
        (&Method::GET, ["indexes", id, "complexity_hotspots"]) => {
            let mut params = with_index(id, q);
            if let Some(obj) = params.as_object_mut()
                && let Some(top_k) = obj.remove("top_k")
            {
                obj.insert("top_n".to_string(), top_k);
            }
            unary(METHOD_COMPLEXITY_HOTSPOTS, params)
        }
        // `?category=` rides along unread: `analysis::SmellsRequest` has no such
        // field and `Smells.svelte` filters by category in the browser
        // (`filtered = category ? flat.filter(…)`). serde ignores it, so the row
        // passes the query through rather than stripping one key by name.
        (&Method::GET, ["indexes", id, "smells"]) => unary(METHOD_SMELLS, with_index(id, q)),
        (&Method::GET, ["indexes", id, "quality"]) => {
            unary(METHOD_QUALITY, json!({ "index_id": id }))
        }
        (&Method::GET, ["indexes", id, "refactor-suggestions"]) => {
            unary(METHOD_REFACTOR_SUGGESTIONS, with_index(id, q))
        }
        (&Method::GET, ["indexes", id, "clusters"]) => unary(METHOD_CLUSTERS, with_index(id, q)),

        // ---- the fact store ---------------------------------------------------
        //
        // `analyze.facts_list` is the one method whose every field is optional,
        // so an empty query is a legitimate "list everything" call.
        (&Method::GET, ["facts"]) => unary(METHOD_FACTS_LIST, q),
        (&Method::POST, ["facts"]) => unary(METHOD_FACTS_UPSERT, body_json(body)?),
        // `facts::DeleteFactRequest::id` is a `u64`, and a path segment is text.
        // Parsing here rather than passing the string through means a bad id is
        // a `501` naming itself instead of an `invalid_params` from the daemon.
        (&Method::DELETE, ["facts", id]) => {
            let parsed: u64 = id.parse().map_err(|_| {
                format!("DELETE /facts/{id} needs a numeric fact id; `{id}` is not one")
            })?;
            unary(METHOD_FACTS_DELETE, json!({ "id": parsed }))
        }

        _ => Err(format!(
            "no trusty-analyze socket method serves {method} /{}. The console reaches \
             trusty-analyze over its Unix socket (ADR-0032, #6287); only the endpoints the \
             dashboard uses are mapped.",
            path.trim_matches('/')
        )),
    }
}

/// Merge an index id into the query-derived params object.
///
/// Why: `GET /indexes/{id}/clusters?k=8&method=bow` carries half its arguments
/// in the path and half in the query, and the RPC method takes one flat object.
/// Test: `maps_every_endpoint_the_spa_calls`.
fn with_index(id: &str, mut params: Value) -> Value {
    if let Some(obj) = params.as_object_mut() {
        obj.insert("index_id".to_string(), Value::String(id.to_string()));
    }
    params
}

/// Parse a request body as JSON, reading an empty body as absent.
///
/// Why `Null` and not `{}` for an empty body: `facts::UpsertFactRequest`
/// requires four fields, so an empty POST must be refused. `null` is what its
/// derived `Deserialize` refuses, which is the correct refusal to inherit —
/// naming the missing field rather than inventing one here.
///
/// # Errors
///
/// A non-empty body that is not JSON.
///
/// Test: `body_json_reads_an_empty_body_as_absent`,
/// `refuses_a_body_that_is_not_json`.
fn body_json(body: &[u8]) -> Result<Value, String> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(body).map_err(|e| format!("request body is not JSON: {e}"))
}

/// Turn a query string into the typed JSON object the RPC params expect.
///
/// Why coercion rather than passing strings through: see the module docs — the
/// daemon's params are typed and a query string is not. Why it is safe to do
/// blind: a wrongly-coerced value is refused by the daemon's own deserialiser
/// with `invalid_params`, which this bridge surfaces as `400`. Nothing is read
/// as a different valid value.
/// What: `true`/`false` become booleans, anything parsing as an `i64` becomes a
/// number, everything else stays a string. A repeated key keeps its LAST value,
/// matching what `serde_urlencoded` does for a non-sequence field.
/// Test: `query_json_coerces_integers`,
/// `query_json_is_an_empty_object_for_no_query`.
fn query_json(query: Option<&str>) -> Value {
    let mut out = Map::new();
    let Some(raw) = query.filter(|q| !q.is_empty()) else {
        return Value::Object(out);
    };
    // Parsing through `Url` rather than splitting by hand: percent-decoding and
    // `+`-as-space are its job, and the crate already depends on it.
    let Ok(url) = reqwest::Url::parse(&format!("http://console/?{raw}")) else {
        return Value::Object(out);
    };
    for (key, value) in url.query_pairs() {
        let coerced = match value.as_ref() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            other => match other.parse::<i64>() {
                Ok(n) => Value::from(n),
                Err(_) => Value::String(other.to_string()),
            },
        };
        out.insert(key.into_owned(), coerced);
    }
    Value::Object(out)
}
