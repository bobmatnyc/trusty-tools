//! Unit tests for the OKG builder tool surface.
//!
//! Why: the ledger/idempotency invariants themselves are proved in
//! `trusty-kb::okg`; what needs covering HERE is the agent-facing layer — the
//! tool schemas, the path-confinement gate on a model-supplied `root`, the
//! register-then-ingest wiring, and the pure Gmail/Drive adapters (which cannot
//! be exercised end-to-end because `trusty-gworkspace` exposes no injectable
//! HTTP transport outside its own crate).
//! What: schema/name pins, a real docstore round-trip against a temp tree, a
//! confinement rejection, and table-driven coverage of every pure adapter.
//! Test: `cargo test -p trusty-agents okg`.

use serde_json::{Value, json};

use super::*;
use crate::test_env::HOME_LOCK;

/// Point the OKG root resolution at a temp knowledge dir for one test.
///
/// Mutating `KB_KNOWLEDGE_DIR` is process-global, so every test here holds
/// `HOME_LOCK` (the crate's existing serializing mutex) and restores the prior
/// value on drop.
struct KnowledgeDirGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prior: Option<std::ffi::OsString>,
    dir: tempfile::TempDir,
}

impl KnowledgeDirGuard {
    fn new() -> Self {
        let lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let prior = std::env::var_os("KB_KNOWLEDGE_DIR");
        // SAFETY: guarded by HOME_LOCK — no other test mutates the env
        // concurrently, and the prior value is restored in Drop.
        unsafe { std::env::set_var("KB_KNOWLEDGE_DIR", dir.path()) };
        Self {
            _lock: lock,
            prior,
            dir,
        }
    }

    /// The tree root under the temp knowledge dir.
    fn root(&self) -> std::path::PathBuf {
        self.dir.path().join("test-kb")
    }
}

impl Drop for KnowledgeDirGuard {
    fn drop(&mut self) {
        // SAFETY: still holding HOME_LOCK for the lifetime of this guard.
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var("KB_KNOWLEDGE_DIR", v),
                None => std::env::remove_var("KB_KNOWLEDGE_DIR"),
            }
        }
    }
}

/// Parse a successful tool result's JSON payload.
fn payload(result: &crate::tools::traits::ToolResult) -> Value {
    assert!(!result.is_error(), "tool errored: {}", result.content());
    serde_json::from_str(result.content()).expect("tool result must be JSON")
}

// ───────────────────────── registration + schemas ─────────────────────────

/// Why: the registered name set is the contract the persona `[tools].allow`
/// globs and the assistant-tier registry both match against.
/// What: pins the four names and their order.
/// Test: self-contained.
#[test]
fn okg_tools_exposes_expected_names() {
    let tools = okg_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        vec![
            "okg_ingest_docstore",
            "okg_ingest_gmail",
            "okg_ingest_drive",
            "okg_sources"
        ]
    );
}

/// Why: a malformed schema is silently dropped by the OpenAI tool builder, so
/// the model would never see the tool at all.
/// What: asserts every tool declares a function schema with a matching name, an
/// object parameter block, and the shared `agent`/`root` properties.
/// Test: self-contained.
#[test]
fn every_tool_schema_is_well_formed() {
    for tool in okg_tools() {
        let s = tool.schema();
        let f = &s["function"];
        assert_eq!(f["name"], tool.name(), "schema name must match name()");
        assert!(
            f["description"].as_str().is_some_and(|d| d.len() > 40),
            "{} needs a usable description",
            tool.name()
        );
        assert_eq!(f["parameters"]["type"], "object");
        let props = &f["parameters"]["properties"];
        assert!(
            props["agent"].is_object(),
            "{} must accept `agent`",
            tool.name()
        );
        assert!(
            props["root"].is_object(),
            "{} must accept `root`",
            tool.name()
        );
        assert_eq!(f["parameters"]["additionalProperties"], false);
    }
}

/// Why: `root` is model-supplied. Without the confinement gate an ingest could
/// be aimed at any directory on disk — the same hole trusty-kb closed for
/// `kb_convert_tree`.
/// What: asserts a root outside the knowledge dir is rejected, and one inside
/// resolves.
/// Test: self-contained.
#[test]
fn resolve_store_rejects_out_of_tree_root() {
    let guard = KnowledgeDirGuard::new();
    let outside = tempfile::tempdir().unwrap();
    let err = resolve_store(&json!({ "root": outside.path().to_string_lossy() }))
        .expect_err("out-of-tree root must be rejected");
    assert!(
        err.to_string().contains("outside the knowledge directory"),
        "unexpected error: {err}"
    );

    let inside = resolve_store(&json!({ "root": guard.root().to_string_lossy() }))
        .expect("in-tree root must resolve");
    assert_eq!(inside.root, guard.root());

    // An agent name always maps under the knowledge dir, so it is always safe.
    let mapped = resolve_store(&json!({ "agent": "Some Assistant" })).expect("agent maps");
    assert_eq!(mapped.root, guard.dir.path().join("some-assistant"));
}

// ───────────────────────── docstore round-trip ─────────────────────────

/// Why: the end-to-end agent path must deliver the same guarantees the engine
/// promises — first run ingests, second run is inert, and a NEW source appends
/// without disturbing the first.
/// What: drives `okg_ingest_docstore` twice over one corpus, then registers a
/// second corpus and re-checks the first through `okg_sources`.
/// Test: self-contained.
#[tokio::test]
async fn docstore_tool_ingests_and_is_idempotent() {
    let guard = KnowledgeDirGuard::new();
    let corpus = guard.dir.path().join("corpus-a");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(corpus.join("note.md"), "a first note").unwrap();
    std::fs::write(corpus.join("other.txt"), "a second note").unwrap();

    let tool = OkgIngestDocstoreTool::new();
    let args = json!({
        "root": guard.root().to_string_lossy(),
        "source_id": "corpus-a",
        "path": corpus.to_string_lossy(),
        "collection": "notes",
    });

    let first = payload(&tool.execute(args.clone()).await);
    assert_eq!(first["source"]["created"], true);
    assert_eq!(first["ingest"]["ingested"], 2);
    assert_eq!(first["ingest"]["skipped"], 0);

    // Re-run with only the source_id — the registered path is reused.
    let rerun = payload(
        &tool
            .execute(json!({
                "root": guard.root().to_string_lossy(),
                "source_id": "corpus-a",
            }))
            .await,
    );
    assert_eq!(rerun["source"]["created"], false);
    assert_eq!(rerun["ingest"]["ingested"], 0, "a re-run must add nothing");
    assert_eq!(rerun["ingest"]["skipped"], 2);

    // A second, unrelated doc store appends.
    let corpus_b = guard.dir.path().join("corpus-b");
    std::fs::create_dir_all(&corpus_b).unwrap();
    std::fs::write(corpus_b.join("extra.md"), "third note").unwrap();
    let added = payload(
        &tool
            .execute(json!({
                "root": guard.root().to_string_lossy(),
                "source_id": "corpus-b",
                "path": corpus_b.to_string_lossy(),
                "collection": "notes",
            }))
            .await,
    );
    assert_eq!(added["ingest"]["ingested"], 1);

    let listed = payload(
        &OkgSourcesTool::new()
            .execute(json!({ "root": guard.root().to_string_lossy() }))
            .await,
    );
    assert_eq!(listed["count"], 2);
    let sources = listed["sources"].as_array().unwrap();
    assert_eq!(sources[0]["id"], "corpus-a");
    assert_eq!(
        sources[0]["watermark"]["items"], 2,
        "the first source is untouched by the second: {listed}"
    );
    assert_eq!(sources[1]["id"], "corpus-b");
    assert_eq!(sources[1]["kind"], "docstore");
}

/// Why: a first registration without a path, or a kind mismatch, must fail with
/// an actionable message rather than silently doing nothing.
/// What: asserts both error paths.
/// Test: self-contained.
#[tokio::test]
async fn docstore_tool_reports_actionable_errors() {
    let guard = KnowledgeDirGuard::new();
    let root = guard.root().to_string_lossy().to_string();

    let missing_path = OkgIngestDocstoreTool::new()
        .execute(json!({ "root": &root, "source_id": "nope" }))
        .await;
    assert!(missing_path.is_error());
    assert!(
        missing_path.content().contains("'path' is required"),
        "got: {}",
        missing_path.content()
    );

    let missing_id = OkgIngestDocstoreTool::new()
        .execute(json!({ "root": &root }))
        .await;
    assert!(missing_id.is_error());
    assert!(missing_id.content().contains("source_id"));

    // A gmail source id cannot be re-used for a doc store.
    OkgIngestGmailTool::new();
    let store = resolve_store(&json!({ "root": &root })).unwrap();
    store
        .okg_register_source(trusty_kb::okg::registry::SourceSpec::new(
            "mail",
            None,
            trusty_kb::okg::registry::Locator::Gmail {
                query: "in:sent".into(),
                after: None,
                before: None,
            },
            "t0",
        ))
        .unwrap();
    let mismatch = OkgIngestDocstoreTool::new()
        .execute(json!({ "root": &root, "source_id": "mail" }))
        .await;
    assert!(mismatch.is_error());
    assert!(
        mismatch.content().contains("not a doc store"),
        "got: {}",
        mismatch.content()
    );
}

// ───────────────────────── pure Gmail adapters ─────────────────────────

/// Why: the window lives structurally in the registry so it can be widened
/// later; this is where it recombines into a query, and it must be stable.
/// What: asserts both bounds are appended in a fixed order.
/// Test: self-contained.
#[test]
fn gmail_query_appends_window() {
    assert_eq!(
        gapi::gmail_query("in:sent", Some("2026/01/01"), Some("2026/07/01")),
        "in:sent after:2026/01/01 before:2026/07/01"
    );
    assert_eq!(
        gapi::gmail_query("in:sent", Some("2024/01/01"), None),
        "in:sent after:2024/01/01",
        "widening backwards only changes the after: term"
    );
}

/// Why: a source with no window must not emit stray operators.
/// What: asserts empty/absent bounds are omitted and whitespace is trimmed.
/// Test: self-contained.
#[test]
fn gmail_query_without_window() {
    assert_eq!(gapi::gmail_query("in:sent", None, None), "in:sent");
    assert_eq!(
        gapi::gmail_query("  in:sent  ", Some("  "), None),
        "in:sent"
    );
    assert_eq!(
        gapi::gmail_query("", Some("2026/01/01"), None),
        "after:2026/01/01"
    );
}

/// Why: the message id doubles as the fingerprint — that is precisely what
/// makes "pull exactly once, ever" hold when a widened window re-lists a
/// message already ingested.
/// What: asserts headers, decoded body, timestamp, and the id==fingerprint
/// invariant.
/// Test: self-contained.
#[test]
fn message_to_item_extracts_headers_and_body() {
    let body = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        "Here it is — thanks!",
    );
    let msg = json!({
        "id": "19f919e2b2d113b2",
        "threadId": "t-1",
        "internalDate": "1784854489000",
        "snippet": "fallback snippet",
        "payload": {
            "mimeType": "multipart/alternative",
            "headers": [
                { "name": "From", "value": "bob@example.com" },
                { "name": "To", "value": "abbie@hftp.org" },
                { "name": "Subject", "value": "Invitation to Serve" },
            ],
            "parts": [ { "mimeType": "text/plain", "body": { "data": body } } ]
        }
    });

    let item = gapi::message_to_item(&msg).expect("item");
    assert_eq!(item.item_id, "19f919e2b2d113b2");
    assert_eq!(
        item.fingerprint, item.item_id,
        "a Gmail message is immutable, so its id IS its fingerprint"
    );
    assert_eq!(item.title, "Invitation to Serve");
    assert_eq!(item.body, "Here it is — thanks!");
    assert_eq!(
        item.fields.get("from").map(String::as_str),
        Some("bob@example.com")
    );
    assert_eq!(
        item.fields.get("thread_id").map(String::as_str),
        Some("t-1")
    );
    let ts = item.timestamp.expect("internalDate decoded");
    assert!(ts.starts_with("2026-07-"), "timestamp was {ts}");
    assert!(
        item.name.starts_with("2026-07-"),
        "name is date-prefixed: {}",
        item.name
    );
}

/// Why: a message with no decodable body must still become an entity rather
/// than being dropped, and one with no id must be rejected rather than
/// producing an unkeyable ledger row.
/// What: asserts the snippet fallback and the `None` case.
/// Test: self-contained.
#[test]
fn message_to_item_degrades_without_body_or_id() {
    let no_body = json!({ "id": "m1", "snippet": "only a snippet", "payload": {} });
    let item = gapi::message_to_item(&no_body).expect("item");
    assert_eq!(item.body, "only a snippet");
    assert_eq!(item.title, "(no subject)");
    assert_eq!(item.name, "undated (no subject)");

    assert!(gapi::message_to_item(&json!({ "snippet": "x" })).is_none());
}

/// Why: an HTML-only newsletter must still yield text, but plain text wins when
/// both are present.
/// What: asserts the preference and the nested walk.
/// Test: self-contained.
#[test]
fn decode_body_prefers_plain_text() {
    let enc =
        |s: &str| base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, s);
    let both = json!({
        "parts": [
            { "mimeType": "text/html", "body": { "data": enc("<p>html</p>") } },
            { "mimeType": "text/plain", "body": { "data": enc("plain") } },
        ]
    });
    assert_eq!(gapi::decode_body(&both).as_deref(), Some("plain"));

    let html_only = json!({
        "parts": [{ "mimeType": "text/html", "body": { "data": enc("<p>html</p>") } }]
    });
    assert_eq!(
        gapi::decode_body(&html_only).as_deref(),
        Some("<p>html</p>")
    );

    assert!(gapi::decode_body(&json!({})).is_none());
}

/// Why: real Gmail nests text parts inside `multipart/mixed` wrappers; a
/// non-recursive walk would return an empty body for most mail with attachments.
/// What: asserts a doubly-nested plain part is found.
/// Test: self-contained.
#[test]
fn decode_body_walks_nested_parts() {
    let enc =
        |s: &str| base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, s);
    let nested = json!({
        "mimeType": "multipart/mixed",
        "parts": [{
            "mimeType": "multipart/alternative",
            "parts": [{ "mimeType": "text/plain", "body": { "data": enc("deep text") } }]
        }]
    });
    assert_eq!(gapi::decode_body(&nested).as_deref(), Some("deep text"));
}

// ───────────────────────── pure Drive adapters ─────────────────────────

/// Why: unlike Gmail, Drive content mutates under a stable id — so a new
/// `version` MUST produce a new fingerprint or an edited doc would never
/// re-ingest.
/// What: asserts the version-derived fingerprint and that it moves with the
/// version while the item id stays put.
/// Test: self-contained.
#[test]
fn drive_item_fingerprints_on_version() {
    let file = |version: i64| {
        json!({
            "id": "1AbC",
            "name": "Board Deck",
            "mimeType": "application/vnd.google-apps.document",
            "modifiedTime": "2026-07-01T10:00:00.000Z",
            "version": version,
        })
    };
    let v1 = gapi::drive_item(&file(7), "content".into()).expect("item");
    let v2 = gapi::drive_item(&file(8), "content".into()).expect("item");

    assert_eq!(v1.fingerprint, "version:7");
    assert_ne!(v1.fingerprint, v2.fingerprint, "a new revision re-ingests");
    assert_eq!(v1.item_id, v2.item_id, "identity is the file id");
    assert_eq!(v1.title, "Board Deck");
    assert_eq!(v1.timestamp.as_deref(), Some("2026-07-01T10:00:00.000Z"));
    assert_eq!(
        v1.fields.get("url").map(String::as_str),
        Some("https://drive.google.com/file/d/1AbC/view")
    );
}

/// Why: not every Drive response carries `version`; falling back to
/// `modifiedTime` keeps change detection working, and having neither must fail
/// OPEN (re-ingest) rather than pretend the file is unchanged forever.
/// What: asserts both fallbacks.
/// Test: self-contained.
#[test]
fn drive_item_falls_back_to_mtime() {
    let mtime_only = json!({ "id": "x", "name": "n", "modifiedTime": "2026-07-01T10:00:00.000Z" });
    let item = gapi::drive_item(&mtime_only, String::new()).expect("item");
    assert_eq!(item.fingerprint, "modified:2026-07-01T10:00:00.000Z");

    let bare = json!({ "id": "x", "name": "n" });
    let item = gapi::drive_item(&bare, String::new()).expect("item");
    assert_eq!(
        item.fingerprint, "unversioned:x",
        "no revision signal must not look like 'unchanged'"
    );
    assert!(item.timestamp.is_none());

    assert!(gapi::drive_item(&json!({ "name": "no id" }), String::new()).is_none());
}

// ───────────────────────── shared HTTP helpers ─────────────────────────

/// Why: 403/404 come back from `BaseClient` as a SUCCESS value carrying an
/// `error` key. Treating that as an empty page would report "0 new items" for a
/// permission failure — a silent fallback this codebase forbids.
/// What: asserts detection, message extraction, and the clean-response case.
/// Test: self-contained.
#[test]
fn api_error_detects_soft_failure() {
    let soft = json!({ "error": { "message": "Insufficient Permission" }, "status": 403 });
    assert_eq!(
        gapi::api_error(&soft).as_deref(),
        Some("Insufficient Permission (status 403)")
    );
    assert!(gapi::api_error(&json!({ "messages": [] })).is_none());
    assert!(gapi::api_error(&json!({ "error": "raw string" })).is_some());
}

/// Why: we build our own URLs to get pagination, so the encoder must handle a
/// real Gmail query's spaces, colons, and slashes.
/// What: asserts the RFC 3986 unreserved set survives and everything else is
/// escaped.
/// Test: self-contained.
#[test]
fn pct_encode_escapes_reserved() {
    assert_eq!(
        gapi::pct_encode("in:sent after:2026/01/01"),
        "in%3Asent%20after%3A2026%2F01%2F01"
    );
    assert_eq!(gapi::pct_encode("a-b_c.d~e9"), "a-b_c.d~e9");
    assert_eq!(gapi::pct_encode("café"), "caf%C3%A9");
}
