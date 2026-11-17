//! Tests for [`super`]: the pure converters moved from trusty-agents'
//! `tools/okg/gapi.rs` (#10746 S13) and the page-at-a-time listings against
//! a `wiremock` server through [`BaseClient::with_loopback_root`].

use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn enc(s: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
}

async fn client(server: &MockServer) -> BaseClient {
    BaseClient::for_test_with_token("a")
        .with_loopback_root(&server.uri())
        .unwrap()
}

/// Why: the window recombines with the query here, and it must be stable.
/// What: asserts both bounds are appended in a fixed order.
/// Test: self-contained.
#[test]
fn gmail_query_appends_window() {
    assert_eq!(
        gmail_query("in:sent", Some("2026/01/01"), Some("2026/07/01")),
        "in:sent after:2026/01/01 before:2026/07/01"
    );
    assert_eq!(
        gmail_query("in:sent", Some("2024/01/01"), None),
        "in:sent after:2024/01/01",
        "widening backwards only changes the after: term"
    );
}

/// Why: a selection with no window must not emit stray operators.
/// What: asserts empty/absent bounds are omitted and whitespace is trimmed.
/// Test: self-contained.
#[test]
fn gmail_query_without_window() {
    assert_eq!(gmail_query("in:sent", None, None), "in:sent");
    assert_eq!(gmail_query("  in:sent  ", Some("  "), None), "in:sent");
    assert_eq!(
        gmail_query("", Some("2026/01/01"), None),
        "after:2026/01/01"
    );
}

/// Why: an HTML-only newsletter must still yield text, but plain text wins.
/// What: asserts the preference and the empty case.
/// Test: self-contained.
#[test]
fn decode_body_prefers_plain_text() {
    let both = json!({
        "parts": [
            { "mimeType": "text/html", "body": { "data": enc("<p>html</p>") } },
            { "mimeType": "text/plain", "body": { "data": enc("plain") } },
        ]
    });
    assert_eq!(decode_body(&both).as_deref(), Some("plain"));
    let html_only = json!({
        "parts": [{ "mimeType": "text/html", "body": { "data": enc("<p>html</p>") } }]
    });
    assert_eq!(decode_body(&html_only).as_deref(), Some("<p>html</p>"));
    assert!(decode_body(&json!({})).is_none());
}

/// Why: real Gmail nests text parts inside `multipart/mixed` wrappers.
/// What: asserts a doubly-nested plain part is found.
/// Test: self-contained.
#[test]
fn decode_body_walks_nested_parts() {
    let nested = json!({
        "mimeType": "multipart/mixed",
        "parts": [{
            "mimeType": "multipart/alternative",
            "parts": [{ "mimeType": "text/plain", "body": { "data": enc("deep text") } }]
        }]
    });
    assert_eq!(decode_body(&nested).as_deref(), Some("deep text"));
}

/// Why: an attachment is listed, never read as the body (SPEC-OKF-01 3k A),
/// and the import keeps the plain and HTML bodies apart (#10746 S13).
/// What: a text attachment with inline data is not the body; both bodies,
/// the header lookup and the attachment list come out.
/// Test: self-contained.
#[test]
fn message_parts_decode_headers_bodies_and_attachments() {
    let msg = json!({
        "payload": {
            "headers": [{ "name": "subject", "value": "Hi" }],
            "mimeType": "multipart/mixed",
            "parts": [
                { "mimeType": "text/plain", "filename": "notes.txt",
                  "body": { "data": enc("attached"), "size": 8 } },
                { "mimeType": "multipart/alternative", "parts": [
                    { "mimeType": "text/plain", "body": { "data": enc("body") } },
                    { "mimeType": "text/html", "body": { "data": enc("<b>body</b>") } },
                ]},
                { "mimeType": "application/pdf", "filename": "a.pdf",
                  "body": { "attachmentId": "x", "size": 99 } },
            ]
        }
    });
    assert_eq!(message_header(&msg, "Subject").as_deref(), Some("Hi"));
    assert_eq!(message_header(&msg, "Date"), None);
    let (plain, html) = message_bodies(&msg["payload"]);
    assert_eq!(plain.as_deref(), Some("body"));
    assert_eq!(html.as_deref(), Some("<b>body</b>"));
    let names: Vec<_> = message_attachments(&msg["payload"])
        .into_iter()
        .map(|a| (a.name, a.mime, a.size))
        .collect();
    assert_eq!(
        names,
        vec![
            ("notes.txt".into(), "text/plain".into(), 8),
            ("a.pdf".into(), "application/pdf".into(), 99)
        ]
    );
}

/// Why: 403/404 come back as a SUCCESS value carrying an `error` key.
/// What: asserts detection, message extraction, and the clean case.
/// Test: self-contained.
#[test]
fn api_error_detects_soft_failure() {
    let soft = json!({ "error": { "message": "Insufficient Permission" }, "status": 403 });
    assert_eq!(
        api_error(&soft).as_deref(),
        Some("Insufficient Permission (status 403)")
    );
    assert!(api_error(&json!({ "messages": [] })).is_none());
    assert!(api_error(&json!({ "error": "raw string" })).is_some());
}

/// Why: the list URLs are built here, so the encoder must handle a real
/// Gmail query's spaces, colons, and slashes.
/// What: asserts the RFC 3986 unreserved set survives.
/// Test: self-contained.
#[test]
fn pct_encode_escapes_reserved() {
    assert_eq!(
        pct_encode("in:sent after:2026/01/01"),
        "in%3Asent%20after%3A2026%2F01%2F01"
    );
    assert_eq!(pct_encode("a-b_c.d~e9"), "a-b_c.d~e9");
    assert_eq!(pct_encode("café"), "caf%C3%A9");
}

/// Why: one page per call must return its token unfollowed, and the
/// ceiling listing must follow every token (#10746 S13).
/// What: two pages behind a loopback root; one page call returns the
/// token, the ceiling call stops at its bound.
/// Test: self-contained.
#[tokio::test(flavor = "current_thread")]
async fn gmail_list_ids_follows_pages_through_a_loopback_root() {
    let server = MockServer::start().await;
    let at = "/gmail.googleapis.com/gmail/v1/users/me/messages";
    Mock::given(method("GET"))
        .and(path(at))
        .and(query_param("q", "in:sent"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m1", "threadId": "t1"}, {"id": "m2", "threadId": "t1"}],
            "nextPageToken": "P2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(at))
        .and(query_param("pageToken", "P2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messages": [{"id": "m3"}]})))
        .mount(&server)
        .await;
    let c = client(&server).await;
    let page = gmail_list_page(&c, Some("a"), "in:sent", None, 2)
        .await
        .unwrap();
    assert_eq!(page.next.as_deref(), Some("P2"));
    assert_eq!(page.messages[0], ("m1".into(), Some("t1".into())));
    let all = gmail_list_ids(&c, Some("a"), "in:sent", 10).await.unwrap();
    assert_eq!(all, vec!["m1", "m2", "m3"]);
    let cut = gmail_list_ids(&c, Some("a"), "in:sent", 2).await.unwrap();
    assert_eq!(cut, vec!["m1", "m2"]);
    // A soft error is an error, never an empty page.
    let none = BaseClient::for_test_with_token("a")
        .with_loopback_root(&server.uri())
        .unwrap();
    Mock::given(method("GET"))
        .and(path(at))
        .and(query_param("q", "denied"))
        .respond_with(ResponseTemplate::new(403).set_body_string("no"))
        .mount(&server)
        .await;
    assert!(
        gmail_list_page(&none, Some("a"), "denied", None, 5)
            .await
            .is_err()
    );
}

/// Why: the adapter hands the walk back as a cursor between requests, so a
/// walk rebuilt from its serialised form must continue where it stopped.
/// What: a root folder with one subfolder and two pages; the walk is
/// serialised after every page; folders are never returned, the subfolder
/// is listed once, and `drive_list_files` gives the same files.
/// Test: self-contained.
#[tokio::test(flavor = "current_thread")]
async fn drive_walk_resumes_from_its_serialised_state() {
    let server = MockServer::start().await;
    let at = "/www.googleapis.com/drive/v3/files";
    let q = |f: &str| format!("'{f}' in parents and trashed = false");
    Mock::given(method("GET"))
        .and(path(at))
        .and(query_param("q", q("R").as_str()))
        .and(query_param_is_missing("pageToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "files": [{"id": "S", "mimeType": DRIVE_FOLDER_MIME}, {"id": "a"}],
            "nextPageToken": "R2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(at))
        .and(query_param("pageToken", "R2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "files": [{"id": "b"}, {"id": "S", "mimeType": DRIVE_FOLDER_MIME}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(at))
        .and(query_param("q", q("S").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": [{"id": "c"}]})))
        .mount(&server)
        .await;
    // #10746: the walk checks its root before the first page.
    Mock::given(method("GET"))
        .and(path(format!("{at}/R")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "R", "mimeType": DRIVE_FOLDER_MIME})),
        )
        .mount(&server)
        .await;
    let c = client(&server).await;
    let mut walk = DriveWalk::new("R", true);
    let mut ids = Vec::new();
    while !walk.is_done() {
        let text = serde_json::to_string(&walk).unwrap();
        walk = serde_json::from_str(&text).unwrap();
        let files = walk.next_page(&c, Some("a"), 10).await.unwrap().unwrap();
        ids.extend(files.iter().map(|f| f["id"].as_str().unwrap().to_string()));
    }
    assert_eq!(ids, vec!["a", "b", "c"]);
    assert!(walk.next_page(&c, Some("a"), 10).await.unwrap().is_none());
    let flat: Vec<_> = drive_list_files(&c, Some("a"), "R", true, 100)
        .await
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(flat, ids);
    let shallow = drive_list_files(&c, Some("a"), "R", false, 100)
        .await
        .unwrap();
    assert_eq!(shallow.len(), 2);
}

/// Why: #10746 review — Drive lists an unknown or unshared folder as 200
/// and no files, so a walk must refuse a root that is not a live folder
/// before it lists anything.
/// What: a 404 root, a trashed folder and a Doc each fail the first
/// `next_page`, and no `files.list` call is made.
/// Test: self-contained.
#[tokio::test(flavor = "current_thread")]
async fn drive_walk_refuses_a_root_that_is_not_a_live_folder() {
    let server = MockServer::start().await;
    let at = "/www.googleapis.com/drive/v3/files";
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": []})))
        .expect(0)
        .mount(&server)
        .await;
    for (id, meta) in [
        (
            "T",
            json!({"id": "T", "mimeType": DRIVE_FOLDER_MIME, "trashed": true}),
        ),
        (
            "D",
            json!({"id": "D", "mimeType": "application/vnd.google-apps.document"}),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{at}/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(meta))
            .mount(&server)
            .await;
    }
    let c = client(&server).await;
    for id in ["gone", "T", "D"] {
        let mut walk = DriveWalk::new(id, true);
        assert!(walk.next_page(&c, Some("a"), 10).await.is_err(), "{id}");
    }
}

/// Why: #10879 — the root check runs for every `DriveWalk`, including
/// trusty-agents' okg drive ingest, whose default folder is the `root`
/// alias; Drive answers `files.get("root")` with My Drive's real ID.
/// What: `files/root` answers a live folder under another ID; the walk
/// passes the check and lists `'root' in parents`.
/// Test: self-contained.
#[tokio::test(flavor = "current_thread")]
async fn drive_walk_accepts_the_root_alias() {
    let server = MockServer::start().await;
    let at = "/www.googleapis.com/drive/v3/files";
    Mock::given(method("GET"))
        .and(path(format!("{at}/root")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "0AMyDrive", "mimeType": DRIVE_FOLDER_MIME})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(at))
        .and(query_param("q", "'root' in parents and trashed = false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": [{"id": "f"}]})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server).await;
    let mut walk = DriveWalk::new("root", false);
    let files = walk.next_page(&c, Some("a"), 10).await.unwrap().unwrap();
    assert_eq!(files, vec![json!({"id": "f"})]);
    assert!(walk.is_done());
}

/// Why: #10746 review — a folder ID goes into the Drive query inside a
/// string literal; a quote in it must not end the literal and add a
/// clause that lists another folder's files.
/// What: an ID with a quote and an `or` clause, and an ID ending in a
/// backslash, stay inside one literal (Drive query syntax escapes `\` and
/// `'` with a backslash); a plain ID is unchanged.
/// Test: self-contained.
#[test]
fn parents_query_cannot_be_widened() {
    assert_eq!(parents_query("F1"), "'F1' in parents and trashed = false");
    assert_eq!(
        parents_query("x' in parents or 'y"),
        r"'x\' in parents or \'y' in parents and trashed = false"
    );
    assert_eq!(
        parents_query(r"a\"),
        r"'a\\' in parents and trashed = false"
    );
}
