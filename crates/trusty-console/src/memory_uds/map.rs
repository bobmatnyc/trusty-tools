//! Which trusty-memory RPC method one `/api/memory/…` request becomes (#6155).
//!
//! Why: trusty-memory answers framed JSON-RPC on a Unix socket and the SPA
//! speaks HTTP paths. A socket has no paths, so the translation has to be
//! written down, and writing it down is what makes the mapped surface
//! reviewable: a request this table does not name is refused with `501` rather
//! than forwarded somewhere approximate.
//!
//! What: [`map_request`] turns a method, a path and a query string into one
//! [`Call`]. The table covers every endpoint the dashboard at `/tools/memory/`
//! calls — `crates/trusty-console/ui-memory/src/lib/api.js` plus the one
//! `EventSource` in `lib/components/ActivityFeed.svelte`.
//!
//! ## The path shapes are the SPA's, unchanged
//!
//! Each row's left-hand side is the path trusty-memory's retired axum router
//! answered, minus the `/api/memory/` prefix the console mounts this bridge at.
//! Keeping them means the SPA needed no rewrite: `base.js` reads the injected
//! `window.__MEMORY_BASE__` and every `api.js` path resolves against it.
//!
//! ## Query values are coerced, and that is visible when it is wrong
//!
//! A query string is all text; the RPC params are typed. `limit=200` has to
//! become `200` or the call answers `invalid_params`. [`query_json`] coerces the
//! two boolean literals and any integer and leaves everything else a string. A
//! string-valued parameter whose value is literally `true` or a bare integer
//! would be coerced wrongly — and would then be REFUSED by the daemon's own
//! deserialiser rather than silently mis-read, which is why the coercion is safe
//! to make blind.
//!
//! ## No row carries a request body
//!
//! The two writes the dashboard makes — `POST /api/v1/dream/run` and
//! `POST /api/v1/admin/stop` — take no arguments, so [`map_request`] never reads
//! one and the handler never buffers one. A row that needs a body is the change
//! that adds the parameter back.
//!
//! Test: `maps_every_endpoint_the_spa_calls`, `refuses_an_unmapped_path`,
//! `query_json_coerces_integers`, `query_json_is_an_empty_object_for_no_query`.

use axum::http::Method;
use serde_json::{Map, Value, json};

use super::{
    METHOD_ACTIVITY, METHOD_ACTIVITY_STREAM, METHOD_ADMIN_STOP, METHOD_CONFIG, METHOD_DRAWERS_LIST,
    METHOD_DREAM_RUN, METHOD_DREAM_STATUS, METHOD_HEALTH, METHOD_KG_ALL, METHOD_KG_COUNT,
    METHOD_KG_GRAPH, METHOD_KG_GRAPH_NEIGHBORS, METHOD_KG_GRAPH_SEED, METHOD_KG_QUERY_TOOL,
    METHOD_KG_SUBJECTS_WITH_COUNTS, METHOD_LOGS_TAIL, METHOD_PALACE_GET, METHOD_PALACES_LIST,
    METHOD_STATUS,
};

/// What one mapped request asks the daemon for.
///
/// Why the two are separate variants rather than a flag: a method is streaming
/// or unary and never both (`trusty_common::uds::server` registers them in
/// different tables), and the HTTP side answers them differently — one JSON body
/// against a `text/event-stream` that stays open. Deciding which from the PATH
/// rather than from the caller's `Accept` header means no caller can talk its
/// way into an open-ended connection.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Call {
    /// One request, one answer.
    Unary {
        /// The RPC method name.
        method: &'static str,
        /// The `params` object.
        params: Value,
    },
    /// One request, many frames, bridged to Server-Sent Events.
    Stream {
        /// The RPC method name.
        method: &'static str,
        /// The `params` object.
        params: Value,
    },
}

/// Turn one `/api/memory/…` request into the call it stands for.
///
/// Why the `Err` is a sentence rather than a code: it is rendered into the `501`
/// body an operator reads, and naming the unmapped path is what makes the gap
/// actionable.
///
/// What: `path` is the sub-path with no leading slash — what axum's `{*path}`
/// captures. No row carries a request body, so none is read; see the module
/// docs.
///
/// # Errors
///
/// A path and method pair this table does not name, and a `kg` read with no
/// `subject`.
///
/// Test: `maps_every_endpoint_the_spa_calls`, `refuses_an_unmapped_path`,
/// `a_kg_read_without_a_subject_is_refused`.
pub(crate) fn map_request(
    method: &Method,
    path: &str,
    query: Option<&str>,
) -> Result<Call, String> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let q = query_json(query);

    // Every `params` here is an OBJECT, never `null`, even for a method that
    // takes no arguments: a derived `Deserialize` refuses `null` outright, so a
    // no-argument call sent as `null` answers `invalid_params`. trusty-memory's
    // `NoParams` accepts either; `{}` is what works for both.
    let unary = |method: &'static str, params: Value| Ok(Call::Unary { method, params });
    let stream = |method: &'static str, params: Value| Ok(Call::Stream { method, params });

    match (method, segments.as_slice()) {
        (&Method::GET, ["health"]) => unary(METHOD_HEALTH, json!({})),

        // ---- the daemon as a whole -------------------------------------------
        (&Method::GET, ["api", "v1", "status"]) => unary(METHOD_STATUS, json!({})),
        (&Method::GET, ["api", "v1", "config"]) => unary(METHOD_CONFIG, json!({})),
        (&Method::GET, ["api", "v1", "logs", "tail"]) => unary(METHOD_LOGS_TAIL, q),
        (&Method::GET, ["api", "v1", "activity"]) => unary(METHOD_ACTIVITY, q),
        (&Method::GET, ["api", "v1", "dream", "status"]) => unary(METHOD_DREAM_STATUS, json!({})),
        (&Method::POST, ["api", "v1", "dream", "run"]) => unary(METHOD_DREAM_RUN, json!({})),
        (&Method::POST, ["api", "v1", "admin", "stop"]) => unary(METHOD_ADMIN_STOP, json!({})),

        // ---- palaces ---------------------------------------------------------
        (&Method::GET, ["api", "v1", "palaces"]) => unary(METHOD_PALACES_LIST, json!({})),
        (&Method::GET, ["api", "v1", "palaces", id]) => {
            unary(METHOD_PALACE_GET, json!({ "palace_id": id }))
        }
        (&Method::GET, ["api", "v1", "palaces", id, "drawers"]) => {
            unary(METHOD_DRAWERS_LIST, with_palace(id, q))
        }

        // ---- one palace's knowledge graph ------------------------------------
        //
        // `kg` with a `subject` query is the one row that does NOT reach a
        // `memory.*` method: see `METHOD_KG_QUERY_TOOL`. Its params are the
        // tool's (`palace`), not the folded methods' (`palace_id`).
        (&Method::GET, ["api", "v1", "palaces", id, "kg"]) => {
            let subject = query_json(query)
                .get("subject")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    format!("GET /api/v1/palaces/{id}/kg needs a `subject` query parameter")
                })?;
            unary(
                METHOD_KG_QUERY_TOOL,
                json!({ "palace": id, "subject": subject }),
            )
        }
        (&Method::GET, ["api", "v1", "palaces", id, "kg", "subjects_with_counts"]) => {
            unary(METHOD_KG_SUBJECTS_WITH_COUNTS, with_palace(id, q))
        }
        (&Method::GET, ["api", "v1", "palaces", id, "kg", "all"]) => {
            unary(METHOD_KG_ALL, with_palace(id, q))
        }
        (&Method::GET, ["api", "v1", "palaces", id, "kg", "count"]) => {
            unary(METHOD_KG_COUNT, json!({ "palace_id": id }))
        }
        (&Method::GET, ["api", "v1", "palaces", id, "kg", "graph"]) => {
            unary(METHOD_KG_GRAPH, json!({ "palace_id": id }))
        }
        (&Method::GET, ["api", "v1", "palaces", id, "kg", "graph", "seed"]) => {
            unary(METHOD_KG_GRAPH_SEED, with_palace(id, q))
        }
        (&Method::GET, ["api", "v1", "palaces", id, "kg", "graph", "neighbors"]) => {
            unary(METHOD_KG_GRAPH_NEIGHBORS, with_palace(id, q))
        }

        // ---- the live feed ---------------------------------------------------
        //
        // `/sse` was the daemon's broadcast route; `memory.activity_stream` is
        // what #6286 replaced it with, and `ActivityFeed.svelte` still opens the
        // old path, so this row is the whole of the rename.
        (&Method::GET, ["sse"]) => stream(METHOD_ACTIVITY_STREAM, json!({})),

        _ => Err(format!(
            "no trusty-memory socket method serves {method} /{}. The console reaches \
             trusty-memory over its Unix socket (ADR-0032, #6286); only the endpoints the \
             dashboard uses are mapped.",
            path.trim_matches('/')
        )),
    }
}

/// Merge a palace id into the query-derived params object.
///
/// Why: `GET /api/v1/palaces/{id}/kg/all?limit=50&offset=0` carries half its
/// arguments in the path and half in the query, and the RPC method takes one
/// flat object.
/// Test: `maps_every_endpoint_the_spa_calls`.
fn with_palace(id: &str, mut params: Value) -> Value {
    if let Some(obj) = params.as_object_mut() {
        obj.insert("palace_id".to_string(), Value::String(id.to_string()));
    }
    params
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
