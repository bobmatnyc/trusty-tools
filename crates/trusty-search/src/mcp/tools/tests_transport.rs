//! The bridge's socket transport (#9168): which method each tool calls, what
//! it sends, and what a failure says.
//!
//! Why: the move off HTTP is only proven if every tool reaches its `search.*`
//! method and no failure text points at a URL. A per-tool table is the one
//! place a twenty-first tool, or a tool still dialling HTTP, shows up.
//! What: a recording mock socket daemon answers every method; each tool is
//! dispatched once and the method sequence it produced is compared to the
//! table. Failure tests drive an absent socket and a refusing daemon.
//! Test: this file.

use serde_json::{json, Value};

use super::test_daemon::{
    last_params, methods, recording_daemon, refusal, unreachable_server, Calls, MockDaemon,
};
use super::tests::req;
use super::{error_codes, tool_descriptors};

/// One answer per method, shaped enough for every tool's success path.
async fn answering_daemon() -> (MockDaemon, Calls) {
    recording_daemon(|method, _| {
        Ok(match method {
            "search.call_chain" => Value::String("main\n  calls: helper".into()),
            "search.index.status" => {
                json!({ "search_capabilities": ["vector", "kg"], "chunk_count": 1 })
            }
            "search.health" => json!({ "status": "ok", "version": "9.9.9", "indexes": 1 }),
            _ => json!({ "results": [], "matches": [], "indexes": [], "total": 0 }),
        })
    })
    .await
}

/// Every tool, the arguments that reach its success path, and the socket
/// methods it calls, in order.
fn tool_table() -> Vec<(&'static str, Value, Vec<&'static str>)> {
    let q = |extra: Value| {
        let mut base = json!({ "index_id": "demo", "query": "fn main" });
        for (k, v) in extra.as_object().expect("an object") {
            base[k] = v.clone();
        }
        base
    };
    vec![
        ("search", q(json!({})), vec!["search.query"]),
        ("search_lexical", q(json!({})), vec!["search.query"]),
        (
            "search_semantic",
            q(json!({})),
            vec!["search.index.status", "search.query"],
        ),
        (
            "search_kg",
            q(json!({})),
            vec!["search.index.status", "search.query"],
        ),
        ("search_all", q(json!({})), vec!["search.query"]),
        (
            "search_all",
            json!({ "query": "fn main" }),
            vec!["search.query.all"],
        ),
        (
            "search_similar",
            json!({ "index": "demo", "file": "src/a.rs" }),
            vec!["search.similar"],
        ),
        (
            "index_file",
            json!({ "index_id": "demo", "path": "a.rs", "content": "fn a() {}" }),
            vec!["search.index.file.put"],
        ),
        (
            "remove_file",
            json!({ "index_id": "demo", "path": "a.rs" }),
            vec!["search.index.file.remove"],
        ),
        ("list_indexes", json!({}), vec!["search.indexes.list"]),
        (
            "create_index",
            json!({ "id": "demo", "root_path": "/tmp/demo" }),
            vec!["search.index.create"],
        ),
        (
            "delete_index",
            json!({ "index_id": "demo" }),
            vec!["search.index.delete"],
        ),
        (
            "reindex",
            json!({ "index_id": "demo" }),
            vec!["search.index.reindex"],
        ),
        (
            "index_status",
            json!({ "index_id": "demo" }),
            vec!["search.index.status"],
        ),
        (
            "list_chunks",
            json!({ "index_id": "demo" }),
            vec!["search.chunks.list"],
        ),
        (
            "search_health",
            json!({ "index_id": "demo" }),
            vec!["search.health", "search.index.status"],
        ),
        (
            "chat",
            json!({ "index_id": "demo", "message": "how?" }),
            vec!["search.chat"],
        ),
        (
            "get_call_chain",
            json!({ "index_id": "demo", "entry_point": "main" }),
            vec!["search.call_chain"],
        ),
        (
            "grep",
            json!({ "index_id": "demo", "pattern": "fn" }),
            vec!["search.grep"],
        ),
        ("grep", json!({ "pattern": "fn" }), vec!["search.grep.all"]),
        (
            "console_metrics",
            json!({}),
            vec!["search.health", "search.indexes.list"],
        ),
        (
            "typeahead",
            json!({ "index_id": "demo", "query": "ma" }),
            vec!["search.typeahead"],
        ),
    ]
}

/// Each of the twenty tools reaches its `search.*` method and only that.
///
/// Why (#9168): the acceptance is "every MCP tool maps to an existing socket
/// method". A tool missing from the table, or one calling a method the table
/// does not name, fails here.
#[tokio::test]
async fn every_tool_reaches_its_socket_method() {
    let advertised: Vec<String> = tool_descriptors()
        .as_array()
        .expect("descriptors")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect();
    let table = tool_table();
    for name in &advertised {
        assert!(
            table.iter().any(|(t, _, _)| t == name),
            "{name} has no row in the socket-method table"
        );
    }
    assert_eq!(
        advertised.len(),
        20,
        "the tool count is part of the contract"
    );

    for (tool, args, expected) in table {
        let (daemon, calls) = answering_daemon().await;
        let resp = daemon.server().dispatch(req(tool, args.clone())).await;
        assert!(resp.error.is_none(), "{tool} {args}: {:?}", resp.error);
        assert_eq!(methods(&calls), expected, "{tool} {args}");
    }
}

/// Query-string fields travel as typed JSON beside `index_id` (#9168).
///
/// Why: the daemon's socket methods decode `limit`, `max_depth` and
/// `include_source` as numbers and booleans; a string `"100"` would be
/// refused as invalid params where HTTP's query string accepted it.
#[tokio::test]
async fn query_string_fields_travel_as_typed_json() {
    let (daemon, calls) = answering_daemon().await;
    let server = daemon.server();
    for (tool, args) in [
        (
            "list_chunks",
            json!({ "index_id": "demo", "after": "src/a b.rs:1:9", "limit": 5 }),
        ),
        (
            "typeahead",
            json!({ "index_id": "demo", "query": "ma", "limit": 3, "mode": "blended" }),
        ),
        (
            "get_call_chain",
            json!({ "index_id": "demo", "entry_point": "main", "max_depth": 2, "include_source": false }),
        ),
    ] {
        let resp = server.dispatch(req(tool, args)).await;
        assert!(resp.error.is_none(), "{tool}: {:?}", resp.error);
    }
    let chunks = last_params(&calls, "search.chunks.list").expect("chunks");
    assert_eq!(
        chunks,
        json!({ "index_id": "demo", "offset": 0, "limit": 5, "after": "src/a b.rs:1:9" })
    );
    let typeahead = last_params(&calls, "search.typeahead").expect("typeahead");
    assert_eq!(
        typeahead,
        json!({ "index_id": "demo", "q": "ma", "limit": 3, "mode": "blended" })
    );
    let chain = last_params(&calls, "search.call_chain").expect("call chain");
    assert_eq!(chain["max_depth"], json!(2));
    assert_eq!(chain["include_source"], json!(false));
}

/// An absent socket is a transport error that names the socket and the start
/// command, never a URL.
#[tokio::test]
async fn an_unreachable_daemon_error_names_the_socket() {
    let server = unreachable_server();
    let socket = server.daemon().socket().display().to_string();
    let resp = server
        .dispatch(req("search", json!({ "index_id": "demo", "query": "q" })))
        .await;
    let err = resp.error.expect("an absent socket is an error");
    assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    assert!(err.message.contains(&socket), "{}", err.message);
    assert!(
        err.message.contains("trusty-search start"),
        "{}",
        err.message
    );
    assert!(!err.message.contains("://"), "no URL: {}", err.message);
}

/// A daemon refusal names the method and the socket, not a URL.
#[tokio::test]
async fn a_refusal_names_the_socket_not_a_url() {
    let (daemon, _calls) =
        recording_daemon(|_, _| Err(refusal(500, &json!({ "error": "corpus open failed" })))).await;
    let socket = daemon.client.socket().display().to_string();
    let resp = daemon
        .server()
        .dispatch(req("index_status", json!({ "index_id": "demo" })))
        .await;
    let err = resp.error.expect("a refusal is an error");
    assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    assert!(
        err.message.contains("search.index.status"),
        "{}",
        err.message
    );
    assert!(
        err.message.contains("corpus open failed"),
        "{}",
        err.message
    );
    assert!(err.message.contains(&socket), "{}", err.message);
    assert!(!err.message.contains("://"), "no URL: {}", err.message);
}

/// An invalid-params refusal stays a parameter error with the daemon's words,
/// on a tool other than search.
#[tokio::test]
async fn invalid_params_refusal_stays_a_parameter_error() {
    let (daemon, _calls) =
        recording_daemon(|_, _| Err(refusal(400, &json!({ "error": "limit out of range" })))).await;
    let resp = daemon
        .server()
        .dispatch(req("list_chunks", json!({ "index_id": "demo" })))
        .await;
    let err = resp.error.expect("refused");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert_eq!(err.message, "limit out of range");
}
