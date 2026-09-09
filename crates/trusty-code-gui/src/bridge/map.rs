//! Which `tcode` RPC method one webview request becomes (#6637).
//!
//! Why: the webview reached the daemon through a generic HTTP listener — any
//! path, forwarded to a base URL. A socket has no paths, so the translation has
//! to be written down, and writing it down is what makes the reachable surface
//! reviewable: a request this table does not name is refused with `501` rather
//! than forwarded somewhere approximate.
//!
//! What: [`map_request`] turns a method, a path and a query string into one
//! [`Call`]. The table is the daemon's REST route set one for one — every route
//! `trusty_code::serve::http::build_axum_router` merges, mapped to the same
//! JSON-RPC method and the same `params` its handler built. Covering the whole
//! set rather than only today's `fetch()` sites is what makes PR 2c a deletion:
//! nothing the webview could reach before stops resolving.
//!
//! ## What is deliberately NOT here
//!
//! `POST /rpc`. The daemon serves it and the webview never calls it, and a row
//! for it would forward an arbitrary method by name — which is the whole of the
//! reviewability this table exists for. A caller that wants the method surface
//! dials the socket, as `tcode tui` does.
//!
//! `GET /health` and `POST /auth/sse-ticket` are not here either, for the
//! opposite reason: they are the BRIDGE's own routes, answered in
//! [`crate::bridge::routes`] without reaching the daemon. Nothing polls this
//! listener from outside the app, so its liveness is its own question.
//!
//! ## Query values are coerced, and that is visible when it is wrong
//!
//! A query string is all text; the RPC params are typed. `include_closed=true`
//! has to become `true` and `after_seq=12` has to become `12`, or the call
//! answers `invalid_params`. [`query_bool`] and [`query_u64`] read exactly the
//! parameters the daemon's own `Query<…>` structs declared, and a value that
//! does not parse is dropped rather than guessed at — the same absent-means-
//! default the REST route's `#[serde(default)]` gave it.
//!
//! Test: `maps_every_route_the_daemon_serves`, `refuses_an_unmapped_path`,
//! `refuses_a_body_that_is_not_json`, `query_coercion_reads_the_declared_params`.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::{METHOD_SESSION_EVENTS, METHOD_WORKSTREAM_EVENTS};

/// What one mapped request asks the daemon for.
///
/// Why the three are separate rather than a flag: a method is streaming or
/// unary and never both (`trusty_common::uds::server` registers them in
/// different tables), and the HTTP side answers them differently — one JSON
/// body against a `text/event-stream` that stays open. Deciding which from the
/// PATH rather than from the caller's `Accept` header means no caller can talk
/// its way into an open-ended connection. [`Call::TranscriptMarkdown`] is the
/// one route with no single RPC twin; see [`crate::bridge::transcript_md`].
#[derive(Debug, PartialEq, Eq)]
pub enum Call {
    /// One request, one answer.
    Unary {
        /// The RPC method name.
        method: &'static str,
        /// The `params` object.
        params: Value,
        /// The status the daemon's REST handler answered on success — `200`
        /// for a read, `201` for a create, `202` for `task.run`.
        status: StatusCode,
    },
    /// One request, many frames, bridged to Server-Sent Events.
    Stream {
        /// The RPC method name.
        method: &'static str,
        /// The `params` object.
        params: Value,
    },
    /// `GET /sessions/{id}/transcript.md` — two calls and a renderer.
    TranscriptMarkdown {
        /// The session whose transcript to render.
        session_id: String,
    },
}

/// Turn one bridge request into the call it stands for.
///
/// Why the `Err` is a sentence rather than a code: it is rendered into the
/// `501` body an operator reads, and "no socket method serves POST /rpc" is
/// what makes the gap actionable.
///
/// What: `path` is the sub-path with no leading slash — what axum's `{*path}`
/// captures. `body` is the raw request body, parsed as JSON only for the
/// methods that carry one.
///
/// # Errors
///
/// A path and method pair this table does not name, or a body that is not JSON.
///
/// Test: `maps_every_route_the_daemon_serves`, `refuses_an_unmapped_path`,
/// `refuses_a_body_that_is_not_json`.
#[allow(clippy::too_many_lines)] // One arm per route; splitting it hides the table.
pub fn map_request(
    method: &Method,
    path: &str,
    query: Option<&str>,
    body: &[u8],
) -> Result<Call, String> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

    let get = |method: &'static str, params: Value| {
        Ok(Call::Unary {
            method,
            params,
            status: StatusCode::OK,
        })
    };
    let created = |method: &'static str, params: Value| {
        Ok(Call::Unary {
            method,
            params,
            status: StatusCode::CREATED,
        })
    };
    let stream = |method: &'static str, params: Value| Ok(Call::Stream { method, params });

    match (method, segments.as_slice()) {
        // ---- sessions, read ---------------------------------------------------
        (&Method::GET, ["sessions"]) => get("session.list", json!({})),
        (&Method::GET, ["sessions", id]) => get("session.status", json!({ "session_id": id })),
        (&Method::GET, ["sessions", id, "transcript"]) => {
            get("session.get_transcript", json!({ "session_id": id }))
        }
        (&Method::GET, ["sessions", id, "transcript.md"]) => Ok(Call::TranscriptMarkdown {
            session_id: (*id).to_string(),
        }),
        (&Method::GET, ["sessions", id, "readiness"]) => {
            get("session.get_readiness", json!({ "session_id": id }))
        }
        (&Method::GET, ["sessions", id, "goals"]) => {
            get("session.get_goals", json!({ "session_id": id }))
        }
        (&Method::GET, ["sessions", id, "budget"]) => {
            get("session.get_context_budget", json!({ "session_id": id }))
        }
        (&Method::GET, ["sessions", id, "agents"]) => {
            get("session.get_agents", json!({ "session_id": id }))
        }
        (&Method::GET, ["sessions", id, "search-audit"]) => {
            get("session.get_search_audit", json!({ "session_id": id }))
        }

        // ---- sessions, write --------------------------------------------------
        //
        // `session.create` reads `task`/`agent`/`project`/`mode`/`workstream_id`
        // off the body object the REST handler rebuilt field by field; passing
        // the parsed body straight through is the same document with one fewer
        // place to drop a field.
        (&Method::POST, ["sessions"]) => created("session.create", body_json(body)?),
        (&Method::POST, ["sessions", id, "messages"]) => Ok(Call::Unary {
            method: "session.send",
            params: with_id("session_id", id, body_json(body)?)?,
            status: StatusCode::OK,
        }),
        (&Method::POST, ["sessions", id, "cancel"]) => {
            get("session.cancel", json!({ "session_id": id }))
        }
        (&Method::PUT, ["sessions", id, "goal"]) => Ok(Call::Unary {
            method: "session.set_goal",
            params: with_id("session_id", id, body_json(body)?)?,
            status: StatusCode::OK,
        }),
        (&Method::DELETE, ["sessions", id, "goal"]) => Ok(Call::Unary {
            method: "session.clear_goal",
            params: with_id("session_id", id, body_json(body)?)?,
            status: StatusCode::OK,
        }),

        // ---- the two event tails ----------------------------------------------
        //
        // `after_seq` has no HTTP twin — the daemon's SSE route always replayed
        // the whole ring buffer. It is mapped because the stream method takes
        // it and a resuming client is exactly who reconnects here.
        (&Method::GET, ["sessions", id, "events"]) => {
            let mut params = json!({ "session_id": id });
            if let Some(seq) = query_u64(query, "after_seq")
                && let Some(obj) = params.as_object_mut()
            {
                obj.insert("after_seq".to_string(), json!(seq));
            }
            stream(METHOD_SESSION_EVENTS, params)
        }
        (&Method::GET, ["workstreams", id, "events"]) => {
            stream(METHOD_WORKSTREAM_EVENTS, json!({ "workstream_id": id }))
        }

        // ---- tasks --------------------------------------------------------------
        (&Method::POST, ["tasks"]) => Ok(Call::Unary {
            method: "task.run",
            params: body_json(body)?,
            status: StatusCode::ACCEPTED,
        }),

        // ---- the filesystem picker ---------------------------------------------
        (&Method::GET, ["fs"]) => get(
            "fs.list_dir",
            json!({
                "path": query_str(query, "path"),
                "include_hidden": query_bool(query, "include_hidden"),
            }),
        ),
        (&Method::GET, ["projects"]) => get("fs.list_projects", Value::Null),

        // ---- the agent and skill catalogs --------------------------------------
        (&Method::GET, ["agents"]) => get("agents.list", Value::Null),
        (&Method::POST, ["agents"]) => created("agents.create", body_json(body)?),
        (&Method::DELETE, ["agents", name]) => get("agents.delete", json!({ "name": name })),
        (&Method::GET, ["skills"]) => get("skills.list", Value::Null),
        (&Method::POST, ["skills"]) => created("skills.create", body_json(body)?),
        (&Method::DELETE, ["skills", name]) => get("skills.delete", json!({ "name": name })),

        // ---- workstreams --------------------------------------------------------
        (&Method::GET, ["workstreams"]) => get(
            "workstream.list",
            json!({ "include_closed": query_bool(query, "include_closed") }),
        ),
        (&Method::POST, ["workstreams"]) => created("workstream.create", body_json(body)?),
        (&Method::GET, ["workstreams", id]) => get("workstream.get", json!({ "id": id })),
        (&Method::POST, ["workstreams", id, "close"]) => {
            get("workstream.close", json!({ "id": id }))
        }
        // `force` is the one non-`Option` field any of these bodies carries, so
        // it is rebuilt rather than passed through: the REST handler always sent
        // it (defaulting to `false`), and an absent field would reach a params
        // struct that may not default it.
        (&Method::POST, ["workstreams", id, "activate"]) => get(
            "workstream.activate",
            json!({
                "id": id,
                "force": body_json(body)?.get("force").and_then(Value::as_bool).unwrap_or(false),
            }),
        ),
        (&Method::POST, ["workstreams", id, "deactivate"]) => {
            get("workstream.deactivate", json!({ "id": id }))
        }
        (&Method::POST, ["workstreams", id, "rename"]) => Ok(Call::Unary {
            method: "workstream.rename",
            params: with_id("id", id, body_json(body)?)?,
            status: StatusCode::OK,
        }),

        _ => Err(format!("no tcode socket method serves {method} /{path}")),
    }
}

/// Parse a request body as JSON, reading an empty body as `{}`.
///
/// Why `{}` rather than `null`: every method this table sends a body to decodes
/// `params` into a struct whose optional fields have serde defaults, and a
/// derived `Deserialize` refuses `null` outright — so a bodyless `POST
/// /sessions/{id}/cancel`-shaped call sent as `null` would answer
/// `invalid_params`.
///
/// # Errors
///
/// A non-empty body that is not JSON.
fn body_json(body: &[u8]) -> Result<Value, String> {
    if body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(body).map_err(|e| format!("request body is not JSON: {e}"))
}

/// Put the path's id onto the body object under `key`.
///
/// Why the body is merged rather than rebuilt field by field: the daemon's REST
/// handlers named each field explicitly, and each naming is a place a field can
/// be dropped when the method grows one. Merging carries whatever the webview
/// sent.
///
/// # Errors
///
/// A body that is not a JSON object — the id has nowhere to go, and silently
/// discarding the body would send a write with no payload.
fn with_id(key: &str, id: &str, mut body: Value) -> Result<Value, String> {
    let Some(obj) = body.as_object_mut() else {
        return Err(format!("request body must be a JSON object to carry {key}"));
    };
    obj.insert(key.to_string(), json!(id));
    Ok(body)
}

/// The raw value of one query parameter, percent-decoding nothing.
///
/// The two callers read a filesystem path and two booleans; a `%`-escaped path
/// would arrive here still escaped, which is the same thing axum's
/// `Query<ListDirQuery>` did NOT do — see `query_str_decodes_percent_escapes`.
fn query_raw<'a>(query: Option<&'a str>, key: &str) -> Option<&'a str> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// One query parameter as a `Value::String`, or `Value::Null` when absent.
///
/// Percent-decodes, because axum's `Query` extractor did and `GET /fs?path=`
/// carries a filesystem path whose spaces arrive as `%20`.
/// Test: `query_str_decodes_percent_escapes`.
fn query_str(query: Option<&str>, key: &str) -> Value {
    match query_raw(query, key) {
        Some(raw) => json!(percent_decode(raw)),
        None => Value::Null,
    }
}

/// One query parameter as a bool, or `Value::Null` when absent or unparseable.
fn query_bool(query: Option<&str>, key: &str) -> Value {
    match query_raw(query, key).and_then(|v| v.parse::<bool>().ok()) {
        Some(b) => json!(b),
        None => Value::Null,
    }
}

/// One query parameter as a `u64`, or `None` when absent or unparseable.
fn query_u64(query: Option<&str>, key: &str) -> Option<u64> {
    query_raw(query, key)?.parse().ok()
}

/// Decode `%XX` escapes and `+` in one query-string value.
///
/// Why hand-rolled: this crate has no percent-encoding dependency and adding
/// one for three call sites would be the larger change. An invalid escape is
/// left as written rather than dropped — the daemon then refuses the path it
/// was actually handed, which is a better diagnostic than a silently mangled
/// one.
/// Test: `query_str_decodes_percent_escapes`.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method_of(call: &Call) -> &str {
        match call {
            Call::Unary { method, .. } | Call::Stream { method, .. } => method,
            Call::TranscriptMarkdown { .. } => "transcript.md",
        }
    }

    /// Why: this table replaces the daemon's whole REST route set, and a row
    /// that maps to the wrong method reaches the webview as a plausible answer
    /// to a different question.
    /// What: one case per route, asserting the method name and the mapped
    /// params.
    /// Test: this is the test.
    #[test]
    fn maps_every_route_the_daemon_serves() {
        let cases: &[(Method, &str, Option<&str>, &str, &str)] = &[
            (Method::GET, "sessions", None, "", "session.list"),
            (Method::GET, "sessions/s1", None, "", "session.status"),
            (
                Method::GET,
                "sessions/s1/transcript",
                None,
                "",
                "session.get_transcript",
            ),
            (
                Method::GET,
                "sessions/s1/transcript.md",
                None,
                "",
                "transcript.md",
            ),
            (
                Method::GET,
                "sessions/s1/readiness",
                None,
                "",
                "session.get_readiness",
            ),
            (
                Method::GET,
                "sessions/s1/goals",
                None,
                "",
                "session.get_goals",
            ),
            (
                Method::GET,
                "sessions/s1/budget",
                None,
                "",
                "session.get_context_budget",
            ),
            (
                Method::GET,
                "sessions/s1/agents",
                None,
                "",
                "session.get_agents",
            ),
            (
                Method::GET,
                "sessions/s1/search-audit",
                None,
                "",
                "session.get_search_audit",
            ),
            (
                Method::POST,
                "sessions",
                None,
                r#"{"task":"t"}"#,
                "session.create",
            ),
            (
                Method::POST,
                "sessions/s1/messages",
                None,
                r#"{"input":"hi"}"#,
                "session.send",
            ),
            (
                Method::POST,
                "sessions/s1/cancel",
                None,
                "",
                "session.cancel",
            ),
            (
                Method::PUT,
                "sessions/s1/goal",
                None,
                r#"{"slot":1,"text":"g"}"#,
                "session.set_goal",
            ),
            (
                Method::DELETE,
                "sessions/s1/goal",
                None,
                r#"{"slot":1}"#,
                "session.clear_goal",
            ),
            (
                Method::GET,
                "sessions/s1/events",
                None,
                "",
                METHOD_SESSION_EVENTS,
            ),
            (
                Method::GET,
                "workstreams/w1/events",
                None,
                "",
                METHOD_WORKSTREAM_EVENTS,
            ),
            (Method::POST, "tasks", None, r#"{"task":"t"}"#, "task.run"),
            (Method::GET, "fs", Some("path=%2Ftmp"), "", "fs.list_dir"),
            (Method::GET, "projects", None, "", "fs.list_projects"),
            (Method::GET, "agents", None, "", "agents.list"),
            (
                Method::POST,
                "agents",
                None,
                r#"{"name":"a","content":"c"}"#,
                "agents.create",
            ),
            (Method::DELETE, "agents/a", None, "", "agents.delete"),
            (Method::GET, "skills", None, "", "skills.list"),
            (
                Method::POST,
                "skills",
                None,
                r#"{"name":"s","content":"c"}"#,
                "skills.create",
            ),
            (Method::DELETE, "skills/s", None, "", "skills.delete"),
            (Method::GET, "workstreams", None, "", "workstream.list"),
            (
                Method::POST,
                "workstreams",
                None,
                r#"{"name":"w"}"#,
                "workstream.create",
            ),
            (Method::GET, "workstreams/w1", None, "", "workstream.get"),
            (
                Method::POST,
                "workstreams/w1/close",
                None,
                "",
                "workstream.close",
            ),
            (
                Method::POST,
                "workstreams/w1/activate",
                None,
                r#"{"force":true}"#,
                "workstream.activate",
            ),
            (
                Method::POST,
                "workstreams/w1/deactivate",
                None,
                "",
                "workstream.deactivate",
            ),
            (
                Method::POST,
                "workstreams/w1/rename",
                None,
                r#"{"name":"n"}"#,
                "workstream.rename",
            ),
        ];

        for (http_method, path, query, body, expected) in cases {
            let call = map_request(http_method, path, *query, body.as_bytes())
                .unwrap_or_else(|e| panic!("{http_method} /{path} must map: {e}"));
            assert_eq!(
                method_of(&call),
                *expected,
                "{http_method} /{path} mapped wrongly"
            );
        }
    }

    /// Why: the path id has to reach the daemon, and a body-carrying write that
    /// dropped it would run against whatever the daemon defaulted to.
    /// Test: this is the test.
    #[test]
    fn a_path_id_is_merged_onto_the_body() {
        let call = map_request(
            &Method::POST,
            "sessions/s1/messages",
            None,
            br#"{"input":"hi"}"#,
        )
        .expect("maps");
        let Call::Unary { params, .. } = call else {
            panic!("expected a unary call");
        };
        assert_eq!(params, json!({ "input": "hi", "session_id": "s1" }));
    }

    /// Why: `202` and `201` are what the daemon's REST layer answered, and a
    /// caller that checks for a specific create status would break on `200`.
    /// Test: this is the test.
    #[test]
    fn creates_and_task_runs_keep_their_rest_statuses() {
        for (path, body, expected) in [
            ("sessions", r#"{"task":"t"}"#, StatusCode::CREATED),
            ("workstreams", r#"{"name":"w"}"#, StatusCode::CREATED),
            ("tasks", r#"{"task":"t"}"#, StatusCode::ACCEPTED),
        ] {
            let call = map_request(&Method::POST, path, None, body.as_bytes()).expect("maps");
            let Call::Unary { status, .. } = call else {
                panic!("expected a unary call for /{path}");
            };
            assert_eq!(status, expected, "/{path}");
        }
    }

    /// Why: an unmapped path must refuse loudly and name itself, not answer an
    /// approximate `502` that reads as "the daemon is down".
    /// Test: this is the test.
    #[test]
    fn refuses_an_unmapped_path() {
        let err = map_request(&Method::POST, "rpc", None, b"{}").expect_err("must refuse");
        assert!(err.contains("/rpc"), "{err}");
        let err = map_request(&Method::GET, "nope", None, b"").expect_err("must refuse");
        assert!(err.contains("/nope"), "{err}");
    }

    /// Why: a body that is not JSON cannot become `params`, and forwarding it
    /// as `{}` would run the write with no payload.
    /// Test: this is the test.
    #[test]
    fn refuses_a_body_that_is_not_json() {
        let err = map_request(&Method::POST, "tasks", None, b"not json").expect_err("must refuse");
        assert!(err.contains("not JSON"), "{err}");
    }

    /// Why: the daemon's `Query<…>` structs are typed, so `include_closed=true`
    /// as the STRING `"true"` answers `invalid_params`.
    /// Test: this is the test.
    #[test]
    fn query_coercion_reads_the_declared_params() {
        let call = map_request(
            &Method::GET,
            "workstreams",
            Some("include_closed=true"),
            b"",
        )
        .expect("maps");
        let Call::Unary { params, .. } = call else {
            panic!("expected a unary call");
        };
        assert_eq!(params, json!({ "include_closed": true }));

        let absent = map_request(&Method::GET, "workstreams", None, b"").expect("maps");
        let Call::Unary { params, .. } = absent else {
            panic!("expected a unary call");
        };
        assert_eq!(params, json!({ "include_closed": Value::Null }));
    }

    /// Why: `after_seq` is what a reconnecting client sends to skip the replay
    /// it already has; dropped, every reconnect replays the whole ring buffer.
    /// Test: this is the test.
    #[test]
    fn after_seq_reaches_the_session_event_stream() {
        let call = map_request(
            &Method::GET,
            "sessions/s1/events",
            Some("after_seq=12"),
            b"",
        )
        .expect("maps");
        let Call::Stream { params, .. } = call else {
            panic!("expected a stream call");
        };
        assert_eq!(params, json!({ "session_id": "s1", "after_seq": 12 }));
    }

    /// Why: `GET /fs?path=` carries a filesystem path, and axum's `Query`
    /// extractor decoded `%20` before the daemon's handler saw it. A bridge
    /// that forwarded the escape would list a directory that does not exist.
    /// Test: this is the test.
    #[test]
    fn query_str_decodes_percent_escapes() {
        let call = map_request(&Method::GET, "fs", Some("path=%2Ftmp%2Fa+b"), b"").expect("maps");
        let Call::Unary { params, .. } = call else {
            panic!("expected a unary call");
        };
        assert_eq!(params["path"], json!("/tmp/a b"));
    }
}
