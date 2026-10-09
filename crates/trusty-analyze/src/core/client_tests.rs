//! Tests for `core::client` — the trusty-search socket client (#9214).

use super::*;
use crate::core::fake_search::FakeSearchSocket;
use std::collections::HashMap;

#[test]
fn index_summary_deserializes_with_and_without_root_path() {
    // With root_path present.
    let json = r#"{"id":"abc","root_path":"/home/user/proj"}"#;
    let s: IndexSummary = serde_json::from_str(json).expect("deserialize with root_path");
    assert_eq!(s.id, "abc");
    assert_eq!(s.root_path.as_deref(), Some("/home/user/proj"));

    // Without root_path (serde default = None).
    let json2 = r#"{"id":"xyz"}"#;
    let s2: IndexSummary = serde_json::from_str(json2).expect("deserialize without root_path");
    assert_eq!(s2.id, "xyz");
    assert!(s2.root_path.is_none());
}

#[test]
fn index_details_deserializes_root_path() {
    // The `search.indexes.list {details: true}` result shape.
    let json = r#"{"indexes":[{"id":"idx1","root_path":"/src/myapp"},{"id":"idx2"}]}"#;
    #[derive(serde::Deserialize)]
    struct Listing {
        indexes: Vec<IndexSummary>,
    }
    let listing: Listing = serde_json::from_str(json).expect("parse listing");
    assert_eq!(listing.indexes.len(), 2);
    assert_eq!(listing.indexes[0].id, "idx1");
    assert_eq!(listing.indexes[0].root_path.as_deref(), Some("/src/myapp"));
    assert_eq!(listing.indexes[1].id, "idx2");
    assert!(listing.indexes[1].root_path.is_none());
}

#[test]
fn index_status_deserializes_root_path() {
    // The `search.index.status` result is a richer object; only root_path is
    // needed here.
    #[derive(serde::Deserialize)]
    struct StatusBody {
        #[serde(default)]
        root_path: Option<String>,
    }
    let json_with = r#"{"index_id":"myproj","root_path":"/home/user/myproj","chunk_count":42}"#;
    let s: StatusBody = serde_json::from_str(json_with).expect("parse status with root_path");
    assert_eq!(s.root_path.as_deref(), Some("/home/user/myproj"));

    let json_without = r#"{"index_id":"myproj","chunk_count":0}"#;
    let s2: StatusBody =
        serde_json::from_str(json_without).expect("parse status without root_path");
    assert!(s2.root_path.is_none());
}

/// #9214: a socket nobody is serving is an error that names the path.
///
/// Why: the HTTP client could fall back to a default; the socket client must
/// not, and an operator reading the error has to know which path was dialled.
/// What: calls `list_indexes` and `health` against a path inside an empty
/// tempdir and asserts both errors carry the path.
/// Test: this function IS the test.
#[tokio::test]
async fn missing_socket_is_an_error_naming_its_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let client = TrustySearchClient::new(&socket);
    let shown = socket.display().to_string();

    let err = client
        .list_indexes()
        .await
        .expect_err("a missing socket is an error, never an empty listing");
    assert!(format!("{err:#}").contains(&shown), "got: {err:#}");

    let err = client
        .health()
        .await
        .expect_err("a missing socket is an error, not a verdict");
    assert!(format!("{err:#}").contains(&shown), "got: {err:#}");
}

/// #9214: each client call names its socket method and the params the HTTP
/// route took as path and query.
#[tokio::test]
async fn every_call_names_its_socket_method() {
    let search = FakeSearchSocket::serve(|method, _| match method {
        "search.health" => Ok(crate::core::fake_search::healthy()),
        "search.indexes.list" => Ok(serde_json::json!({
            "indexes": [{"id": "a", "root_path": "/src/a"}]
        })),
        "search.index.status" => Ok(serde_json::json!({"root_path": "/src/a"})),
        other => Err((-32601, format!("unexpected {other}"))),
    });
    let client = TrustySearchClient::new(search.path());

    assert!(client.health().await.expect("health"));
    let details = client.index_details().await.expect("details");
    assert_eq!(details[0].root_path.as_deref(), Some("/src/a"));
    let root = client.index_status_root_path("a").await.expect("status");
    assert_eq!(root.as_deref(), Some("/src/a"));

    let calls = search.calls();
    let methods: Vec<&str> = calls.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods,
        vec![
            "search.health",
            "search.indexes.list",
            "search.index.status"
        ]
    );
    assert_eq!(calls[1].1["details"], true);
    assert_eq!(calls[2].1["index_id"], "a");
}

/// A daemon that answers `search.health` with an error is down, not a failed
/// call — the socket form of the old non-2xx `/health`.
#[tokio::test]
async fn health_error_answer_reads_as_down() {
    let search = FakeSearchSocket::serve(|_, _| Err((-32603, "not ready".to_string())));
    let up = TrustySearchClient::new(search.path())
        .health()
        .await
        .expect("an answered error is a verdict, not a transport failure");
    assert!(!up);
}

/// Serve a cursor-aware fake trusty-search socket.
///
/// Why: a stub that answers every request with the same body cannot
/// exercise a cursor walk at all — the loop's continuation, its
/// no-progress guard, and the post-walk shortfall check all stay dark
/// while the tests still pass. Keying the response on `after` is what
/// makes a multi-page walk observable.
/// What: `pages` maps the `after` cursor a request carries (`""` for the
/// first page) to the result to answer it with. An unmapped cursor is a
/// test bug, so it answers an error rather than an empty page that would
/// look like a clean end of corpus.
/// Test: used by every `get_chunks` test below.
fn spawn_chunks_stub(pages: Vec<(&'static str, serde_json::Value)>) -> FakeSearchSocket {
    let pages: HashMap<String, serde_json::Value> =
        pages.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    FakeSearchSocket::serve(move |method, params| {
        if method != "search.chunks.list" {
            return Err((-32601, format!("unexpected {method}")));
        }
        let cursor = params["after"].as_str().unwrap_or_default();
        pages
            .get(cursor)
            .cloned()
            .ok_or_else(|| (-32603, format!("stub has no page for cursor {cursor:?}")))
    })
}

/// One page body: `chunks` for the given ids, plus `total` and the cursor
/// to follow (`None` ends the walk).
fn page(total: usize, ids: &[&str], next_cursor: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "total": total,
        "chunks": ids.iter().map(|i| chunk_json(i)).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
    })
}

fn chunk_json(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "file": "src/lib.rs", "start_line": 1, "end_line": 2,
        "content": "fn f() {}", "score": 0.0, "match_reason": "enumerate"
    })
}

/// #6043: the exact response the live daemon served for the `trusty-tools`
/// index — `total: 50929` beside zero rows and `next_cursor: null`.
///
/// Why: the walk reads `next_cursor: null` as "corpus exhausted", so before
/// this fix `get_chunks` returned `Ok(vec![])` and
/// `complexity_distribution` scored an empty corpus and published
/// `total: 0, skipped_non_code: 0` — a confident, wrong measurement of a
/// 50,929-chunk index.
/// What: asserts the shortfall against the server's own `total` is an error
/// naming both numbers.
/// Test: this function IS the test.
#[tokio::test]
async fn short_export_against_reported_total_is_an_error() {
    let search = spawn_chunks_stub(vec![(
        "",
        serde_json::json!({
            "index_id": "trusty-tools", "total": 50929,
            "chunks": [], "next_cursor": null
        }),
    )]);
    let err = TrustySearchClient::new(search.path())
        .get_chunks("trusty-tools")
        .await
        .expect_err("a zero-row export against total: 50929 must not pass as the corpus");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("50929") && msg.contains("incomplete"),
        "the error must name the shortfall, got: {msg}"
    );
}

/// A response with no `total` makes no completeness claim, so there is
/// nothing to check it against — the export is returned as-is.
#[tokio::test]
async fn export_without_total_makes_no_completeness_claim() {
    let search = spawn_chunks_stub(vec![(
        "",
        serde_json::json!({ "chunks": [chunk_json("a:1:2")] }),
    )]);
    let chunks = TrustySearchClient::new(search.path())
        .get_chunks("idx")
        .await
        .expect("no total means no claim to verify");
    assert_eq!(chunks.len(), 1);
}

/// A complete single-page export satisfies the reported total, and the first
/// page selects cursor mode with `after: ""`.
#[tokio::test]
async fn cursor_walk_accepts_a_complete_export() {
    let search = spawn_chunks_stub(vec![("", page(2, &["a:1:2", "b:1:2"], None))]);
    let chunks = TrustySearchClient::new(search.path())
        .get_chunks("idx")
        .await
        .expect("a complete export is accepted");
    assert_eq!(chunks.len(), 2);
    let calls = search.calls();
    assert_eq!(calls[0].1["index_id"], "idx");
    assert_eq!(calls[0].1["after"], "");
    assert_eq!(calls[0].1["limit"], CHUNK_PAGE_LIMIT);
}

/// The walk follows `next_cursor` across every page and stops on the page
/// that offers none.
///
/// Why: a corpus larger than one page is the ordinary case — the live
/// `trusty-tools` index is 51 pages at the 1000-row server cap — and none
/// of the loop's continuation was exercised while the stub answered every
/// request identically. A walk that silently stopped after page one would
/// have looked correct in every other test here.
/// What: three pages chained by cursor, asserting all rows arrive exactly
/// once and in order.
/// Test: this function IS the test.
#[tokio::test]
async fn cursor_walk_collects_every_page() {
    let search = spawn_chunks_stub(vec![
        ("", page(5, &["a:1:2", "b:1:2"], Some("b:1:2"))),
        ("b:1:2", page(5, &["c:1:2", "d:1:2"], Some("d:1:2"))),
        ("d:1:2", page(5, &["e:1:2"], None)),
    ]);
    let chunks = TrustySearchClient::new(search.path())
        .get_chunks("idx")
        .await
        .expect("a three-page walk completes");
    let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["a:1:2", "b:1:2", "c:1:2", "d:1:2", "e:1:2"],
        "every page's rows arrive exactly once, in walk order"
    );
}

/// A multi-page walk that dies partway is a shortfall, not a short corpus.
///
/// Why: `chunks_after` turns a mid-walk redb read failure into an empty
/// page with `next_cursor: null`, which is bit-identical to a clean end of
/// corpus. Page one succeeding is what makes this the insidious shape — the
/// client has rows in hand and no reason to doubt them.
/// What: page one returns 2 of 5 rows, page two returns the empty
/// end-of-corpus shape. Asserts the walk refuses rather than returning 2.
/// Test: this function IS the test.
#[tokio::test]
async fn walk_truncated_after_the_first_page_is_an_error() {
    let search = spawn_chunks_stub(vec![
        ("", page(5, &["a:1:2", "b:1:2"], Some("b:1:2"))),
        ("b:1:2", page(5, &[], None)),
    ]);
    let err = TrustySearchClient::new(search.path())
        .get_chunks("idx")
        .await
        .expect_err("2 of 5 rows must not pass as the whole corpus");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("5 chunks") && msg.contains("returned 2"),
        "the error must name both numbers, got: {msg}"
    );
}

/// A server that keeps handing back the cursor it was given must not spin.
///
/// Why: the walk advances only because the cursor is exclusive and strictly
/// increasing. A daemon that repeats it — a bug, or a corpus mutating under
/// the walk — would otherwise loop forever inside a request handler.
/// What: a page whose `next_cursor` equals the cursor that fetched it. The
/// guard breaks the loop and the shortfall check reports it, so the test
/// terminating at all is half the assertion.
/// Test: this function IS the test.
#[tokio::test]
async fn repeated_cursor_stops_the_walk_instead_of_spinning() {
    let search = spawn_chunks_stub(vec![
        ("", page(9, &["a:1:2"], Some("a:1:2"))),
        ("a:1:2", page(9, &["a:1:2"], Some("a:1:2"))),
    ]);
    let err = TrustySearchClient::new(search.path())
        .get_chunks("idx")
        .await
        .expect_err("a non-advancing cursor is a failed walk, not a complete one");
    assert!(format!("{err:#}").contains("incomplete"), "got: {err:#}");
}
