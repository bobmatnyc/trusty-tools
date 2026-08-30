//! Which trusty-search RPC method one `/api/search/…` request becomes (#6285).
//!
//! Why: the console served this prefix with a generic reverse proxy — any
//! method, any path, forwarded verbatim to a base URL. A socket has no paths, so
//! the translation has to be written down, and writing it down is what makes the
//! mapped surface reviewable: a request this table does not name is refused with
//! `501` rather than forwarded somewhere approximate.
//!
//! What: [`map_request`] turns a method, a path and a query string into one
//! [`Call`]. The table covers every endpoint the two SPAs served from this
//! binary actually call — the trusty-search dashboard at `/tools/search/`
//! (`crates/trusty-console/ui-search/src/lib/api.js` plus the two `EventSource`s) and
//! the console's own cleanup flow (`ui/src/cleanupFlow.js`).
//!
//! ## Two endpoints the SPA calls have no method to map to
//!
//! `POST /chat` and `POST /admin/stop` are HTTP-only on trusty-search: slice 5.5
//! left them out on the grounds that "chat serves the embedded `/ui` alone", and
//! #6384 moved that `/ui` here — so the console-served SPA IS the consumer that
//! reasoning said did not exist. Both answer `501` naming the gap. The same
//! finding the #6285 consumer-map correction recorded for trusty-mpm's TUI stop
//! key: the retire slice needs an owner decision on `admin.stop`, and the chat
//! lane needs one too.
//!
//! ## Query values are coerced, and that is visible when it is wrong
//!
//! A query string is all text; the RPC params are typed. `details=true` has to
//! become `true` and `n=200` has to become `200`, or every call answers
//! `invalid_params`. [`query_json`] coerces the two literals and any integer and
//! leaves everything else a string. A string-valued parameter whose value is
//! literally `true` or a bare integer would be coerced wrongly — and would then
//! be REFUSED by the daemon's own deserialiser rather than silently mis-read,
//! which is why the coercion is safe to make blind.
//!
//! Test: `maps_every_endpoint_the_spa_calls`, `refuses_an_unmapped_path`,
//! `query_json_coerces_bools_and_integers`, `body_json_reads_an_empty_body_as_absent`.

use axum::http::Method;
use serde_json::{Map, Value, json};

use super::{
    METHOD_CONFIG_GET, METHOD_CONFIG_SET, METHOD_HEALTH, METHOD_INDEX_CONFIG_GET,
    METHOD_INDEX_CONFIG_SET, METHOD_INDEX_CREATE, METHOD_INDEX_DELETE, METHOD_INDEX_REINDEX,
    METHOD_INDEX_REINDEX_STREAM, METHOD_INDEX_STATUS, METHOD_INDEXES_LIST, METHOD_LOGS_TAIL,
    METHOD_QUERY, METHOD_QUERY_ALL, METHOD_REGISTRY_ORPHANS, METHOD_STATUS_STREAM,
};

/// What one mapped request asks the daemon for.
///
/// Why the two are separate types rather than a flag: a method is streaming or
/// unary and never both (`trusty_common::uds::server` registers them in
/// different tables), and the HTTP side answers them differently — one JSON body
/// against a `text/event-stream` that stays open. Deciding which from the PATH
/// rather than from the caller's `Accept` header is also strictly stronger than
/// what #6155 could do over HTTP: there, `Accept: text/event-stream` was a claim
/// the proxy had to re-check against the upstream's `Content-Type` before
/// granting a deadline-free read. Here the transport is fixed by this table, so
/// no caller can talk its way into an open-ended connection.
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

/// Turn one `/api/search/…` request into the call it stands for.
///
/// Why the `Err` is a sentence rather than a code: it is rendered into the `501`
/// body an operator reads, and "no socket method serves POST /chat" is what
/// makes the gap actionable.
///
/// What: `path` is the sub-path with no leading slash — what axum's `{*path}`
/// captures. `body` is the raw request body, parsed as JSON only for the methods
/// that carry one.
///
/// # Errors
///
/// A path and method pair this table does not name, or a body that is not JSON.
///
/// Test: `maps_every_endpoint_the_spa_calls`, `refuses_an_unmapped_path`,
/// `refuses_a_body_that_is_not_json`.
pub(crate) fn map_request(
    method: &Method,
    path: &str,
    query: Option<&str>,
    body: &[u8],
) -> Result<Call, String> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let q = query_json(query);

    // Every `params` here is an OBJECT, never `null`, even for a method that
    // takes no arguments: trusty-search decodes `params` into a struct whose
    // fields all have serde defaults, and a derived `Deserialize` refuses
    // `null` outright — so a no-argument call sent as `null` answers
    // `invalid_params`. `NoParams` accepts either; `{}` is what works for both.
    let unary = |method: &'static str, params: Value| Ok(Call::Unary { method, params });
    let stream = |method: &'static str, params: Value| Ok(Call::Stream { method, params });

    match (method, segments.as_slice()) {
        (&Method::GET, ["health"]) => unary(METHOD_HEALTH, json!({})),

        // ---- the index roster -----------------------------------------------
        (&Method::GET, ["indexes"]) => unary(METHOD_INDEXES_LIST, q),
        (&Method::POST, ["indexes"]) => unary(METHOD_INDEX_CREATE, body_json(body)?),
        (&Method::DELETE, ["indexes", id]) => unary(METHOD_INDEX_DELETE, with_index(id, q)),

        // ---- one index -------------------------------------------------------
        (&Method::GET, ["indexes", id, "status"]) => {
            unary(METHOD_INDEX_STATUS, json!({ "index_id": id }))
        }
        (&Method::GET, ["indexes", id, "config"]) => {
            unary(METHOD_INDEX_CONFIG_GET, json!({ "index_id": id }))
        }
        (&Method::PATCH, ["indexes", id, "config"]) => unary(
            METHOD_INDEX_CONFIG_SET,
            json!({ "index_id": id, "body": body_json(body)? }),
        ),
        (&Method::POST, ["indexes", id, "search"]) => unary(
            METHOD_QUERY,
            json!({ "index_id": id, "body": body_json(body)? }),
        ),
        // `ReindexParams::body` is `Option`, so an absent body maps to `null`
        // rather than to `{}` — the same "no overrides" the HTTP route reads
        // from an empty request body.
        (&Method::POST, ["indexes", id, "reindex"]) => unary(
            METHOD_INDEX_REINDEX,
            json!({ "index_id": id, "body": body_json(body)? }),
        ),
        (&Method::GET, ["indexes", id, "reindex", "stream"]) => {
            stream(METHOD_INDEX_REINDEX_STREAM, json!({ "index_id": id }))
        }

        // ---- registry-wide ---------------------------------------------------
        (&Method::POST, ["search"]) => unary(METHOD_QUERY_ALL, body_json(body)?),
        (&Method::GET, ["config"]) => unary(METHOD_CONFIG_GET, json!({})),
        (&Method::PATCH, ["config"]) => unary(METHOD_CONFIG_SET, body_json(body)?),
        (&Method::GET, ["logs", "tail"]) => unary(METHOD_LOGS_TAIL, q),
        (&Method::GET, ["registry", "orphans"]) => unary(METHOD_REGISTRY_ORPHANS, json!({})),
        (&Method::GET, ["status", "stream"]) => stream(METHOD_STATUS_STREAM, json!({})),

        _ => Err(format!(
            "no trusty-search socket method serves {method} /{}. The console reaches \
             trusty-search over its Unix socket (ADR-0032, #6285); only the endpoints the \
             dashboard uses are mapped. `POST /chat` and `POST /admin/stop` have no socket \
             method at all and are pending an owner decision on #6285.",
            path.trim_matches('/')
        )),
    }
}

/// Merge an index id into the query-derived params object.
///
/// Why: `DELETE /indexes/{id}?delete_data=true` carries half its arguments in
/// the path and half in the query, and the RPC method takes one flat object.
/// Test: `maps_every_endpoint_the_spa_calls`.
fn with_index(id: &str, mut params: Value) -> Value {
    if let Some(obj) = params.as_object_mut() {
        obj.insert("index_id".to_string(), Value::String(id.to_string()));
    }
    params
}

/// Parse a request body as JSON, reading an empty body as absent.
///
/// Why `Null` and not `{}` for an empty body: the only methods that take one are
/// the ones whose params wrap it, and `ReindexParams::body` is an `Option` that
/// must read an empty POST as "no overrides". A method whose body is required
/// refuses `null` itself, which is the correct refusal to inherit.
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
/// Test: `query_json_coerces_bools_and_integers`,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unary(method: &Method, path: &str, query: Option<&str>, body: &str) -> Call {
        map_request(method, path, query, body.as_bytes()).expect("mapped")
    }

    /// Why: this table is the whole contract between the SPA and the socket. If
    /// one row is wrong the dashboard loses a feature silently — the SPA renders
    /// an error toast, not a crash — so every endpoint the bundle calls is
    /// asserted here against the method it must reach.
    /// What: one case per `api.js` entry plus the two `EventSource`s and the
    /// console's own orphan census.
    /// Test: this is the test.
    #[test]
    fn maps_every_endpoint_the_spa_calls() {
        assert_eq!(
            unary(&Method::GET, "health", None, ""),
            Call::Unary {
                method: METHOD_HEALTH,
                params: json!({})
            }
        );
        assert_eq!(
            unary(&Method::GET, "indexes", Some("details=true"), ""),
            Call::Unary {
                method: METHOD_INDEXES_LIST,
                params: json!({ "details": true })
            }
        );
        assert_eq!(
            unary(
                &Method::POST,
                "indexes",
                None,
                r#"{"id":"a","root_path":"/r"}"#
            ),
            Call::Unary {
                method: METHOD_INDEX_CREATE,
                params: json!({ "id": "a", "root_path": "/r" })
            }
        );
        assert_eq!(
            unary(&Method::DELETE, "indexes/a", Some("delete_data=true"), ""),
            Call::Unary {
                method: METHOD_INDEX_DELETE,
                params: json!({ "index_id": "a", "delete_data": true })
            }
        );
        assert_eq!(
            unary(&Method::GET, "indexes/a/status", None, ""),
            Call::Unary {
                method: METHOD_INDEX_STATUS,
                params: json!({ "index_id": "a" })
            }
        );
        assert_eq!(
            unary(&Method::GET, "indexes/a/config", None, ""),
            Call::Unary {
                method: METHOD_INDEX_CONFIG_GET,
                params: json!({ "index_id": "a" })
            }
        );
        assert_eq!(
            unary(
                &Method::PATCH,
                "indexes/a/config",
                None,
                r#"{"include_docs":false}"#
            ),
            Call::Unary {
                method: METHOD_INDEX_CONFIG_SET,
                params: json!({ "index_id": "a", "body": { "include_docs": false } })
            }
        );
        assert_eq!(
            unary(
                &Method::POST,
                "indexes/a/search",
                None,
                r#"{"text":"q","top_k":10}"#
            ),
            Call::Unary {
                method: METHOD_QUERY,
                params: json!({ "index_id": "a", "body": { "text": "q", "top_k": 10 } })
            }
        );
        assert_eq!(
            unary(&Method::POST, "indexes/a/reindex", None, "{}"),
            Call::Unary {
                method: METHOD_INDEX_REINDEX,
                params: json!({ "index_id": "a", "body": {} })
            }
        );
        assert_eq!(
            unary(&Method::POST, "search", None, r#"{"query":"q","top_k":5}"#),
            Call::Unary {
                method: METHOD_QUERY_ALL,
                params: json!({ "query": "q", "top_k": 5 })
            }
        );
        assert_eq!(
            unary(&Method::GET, "config", None, ""),
            Call::Unary {
                method: METHOD_CONFIG_GET,
                params: json!({})
            }
        );
        assert_eq!(
            unary(&Method::PATCH, "config", None, r#"{"max_rss_mb":100}"#),
            Call::Unary {
                method: METHOD_CONFIG_SET,
                params: json!({ "max_rss_mb": 100 })
            }
        );
        assert_eq!(
            unary(&Method::GET, "logs/tail", Some("n=200"), ""),
            Call::Unary {
                method: METHOD_LOGS_TAIL,
                params: json!({ "n": 200 })
            }
        );
        assert_eq!(
            unary(&Method::GET, "registry/orphans", None, ""),
            Call::Unary {
                method: METHOD_REGISTRY_ORPHANS,
                params: json!({})
            }
        );
        assert_eq!(
            unary(&Method::GET, "status/stream", None, ""),
            Call::Stream {
                method: METHOD_STATUS_STREAM,
                params: json!({})
            }
        );
        assert_eq!(
            unary(&Method::GET, "indexes/a/reindex/stream", None, ""),
            Call::Stream {
                method: METHOD_INDEX_REINDEX_STREAM,
                params: json!({ "index_id": "a" })
            }
        );
    }

    /// Why: an unmapped request must be refused with a sentence an operator can
    /// act on, never forwarded on a guess. `POST /chat` is the live instance —
    /// the SPA calls it and no socket method serves it.
    /// Test: this is the test.
    #[test]
    fn refuses_an_unmapped_path() {
        for (method, path) in [
            (Method::POST, "chat"),
            (Method::POST, "admin/stop"),
            (Method::POST, "upgrade"),
            (Method::GET, "metrics"),
            (Method::GET, "indexes/a/communities"),
        ] {
            let err = map_request(&method, path, None, b"").expect_err("unmapped");
            assert!(err.contains(path), "the refusal must name the path: {err}");
        }
    }

    /// Why: a leading slash on the captured path must not change the mapping —
    /// axum's `{*path}` has produced both shapes across versions.
    /// Test: this is the test.
    #[test]
    fn maps_the_same_with_or_without_a_leading_slash() {
        assert_eq!(
            unary(&Method::GET, "/health", None, ""),
            unary(&Method::GET, "health", None, "")
        );
    }

    /// Why: an empty POST body is "no overrides", not a malformed request — the
    /// SPA's reindex call sends `{}` but a hand-rolled `curl -X POST` sends
    /// nothing, and both must reach the daemon.
    /// Test: this is the test.
    #[test]
    fn body_json_reads_an_empty_body_as_absent() {
        assert_eq!(body_json(b"").expect("empty"), Value::Null);
        assert_eq!(body_json(b"  \n").expect("whitespace"), Value::Null);
        assert_eq!(
            unary(&Method::POST, "indexes/a/reindex", None, ""),
            Call::Unary {
                method: METHOD_INDEX_REINDEX,
                params: json!({ "index_id": "a", "body": null })
            }
        );
    }

    /// Why: a body that is not JSON must be refused HERE, with the parse error,
    /// rather than sent on to answer an opaque `invalid_params`.
    /// Test: this is the test.
    #[test]
    fn refuses_a_body_that_is_not_json() {
        let err = map_request(&Method::POST, "search", None, b"not json").expect_err("refused");
        assert!(err.contains("not JSON"), "{err}");
    }

    /// Why: `details=true` reaching the daemon as the STRING `"true"` answers
    /// `invalid_params`, which is how every index listing would fail.
    /// Test: this is the test.
    #[test]
    fn query_json_coerces_bools_and_integers() {
        let v = query_json(Some(
            "details=true&force=false&n=200&format=json&repo=own%2Frepo",
        ));
        assert_eq!(v["details"], json!(true));
        assert_eq!(v["force"], json!(false));
        assert_eq!(v["n"], json!(200));
        assert_eq!(v["format"], json!("json"));
        assert_eq!(v["repo"], json!("own/repo"));
    }

    /// Why: a method whose params struct has all-default fields still refuses
    /// `null`, so "no query" has to be `{}`.
    /// Test: this is the test.
    #[test]
    fn query_json_is_an_empty_object_for_no_query() {
        assert_eq!(query_json(None), json!({}));
        assert_eq!(query_json(Some("")), json!({}));
    }
}
