//! Coverage for the memory-file → drawer mapping and the import loop.
//!
//! Why: the derivation must reproduce the established mapping exactly (issue
//! #4837), and the import loop's two safety properties — a dry run writes
//! nothing, and a re-run duplicates nothing — are the whole reason the command
//! can be pointed at a live palace.
//! What: pure parser tests plus loop tests driven against a stub JSON-RPC
//! daemon that implements just `memory_list` and `memory_remember`.
//! Test: this file.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::parse::{describe, parse_memory_file, split_frontmatter, wikilink_targets};
use super::{ImportOptions, ImportStatus, is_import_of, run_import};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

// NOTE: these fixtures are raw strings deliberately — a `\`-continued literal
// swallows the *leading whitespace* of the next line, which would silently
// flatten the nested `metadata:` block and the block-scalar indentation these
// tests exist to cover.
const SAMPLE: &str = r"---
name: admin-merge-only-on-green
description: Admin-merge bypasses bot-approval ONLY — never a red CI gate;
metadata:
  node_type: memory
  type: feedback
  originSessionId: 381b2909-670c-4896-ba84-637f2f2ae75c
  modified: 2026-07-27T20:55:10.605Z
---

Body line one, see [[gate-merge-commands-with-and]].

Also [[critic-verdict-must-post-to-pr.md]] and [[repo-owner-admin-merge|the owner rule]].
";

fn write_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, body) in files {
        std::fs::write(dir.path().join(name), body).expect("write fixture");
    }
    dir
}

// ---------------------------------------------------------------------------
// Parsing / derivation
// ---------------------------------------------------------------------------

#[test]
fn derives_text_and_tags() {
    let parsed = parse_memory_file(SAMPLE).unwrap().unwrap();
    assert_eq!(parsed.name, "admin-merge-only-on-green");
    assert_eq!(parsed.kind, "feedback");
    // Description leads the text, a period is appended after `;`, and the body
    // follows after exactly one blank line with its markdown intact.
    assert!(parsed.text.starts_with(
        "Admin-merge bypasses bot-approval ONLY — never a red CI gate;.\n\nBody line one,"
    ));
    assert!(parsed.text.ends_with("the owner rule]]."));
    assert_eq!(
        parsed.tags,
        vec![
            "admin-merge-only-on-green".to_string(),
            "critic-verdict-must-post-to-pr".to_string(),
            "feedback".to_string(),
            "gate-merge-commands-with-and".to_string(),
            "repo-owner-admin-merge".to_string(),
        ]
    );
}

#[test]
fn appends_period_only_when_needed() {
    assert_eq!(describe("ends in a word"), "ends in a word.");
    assert_eq!(describe("ends in a stop."), "ends in a stop.");
    assert_eq!(describe("ends in a colon:"), "ends in a colon:");
    assert_eq!(describe("\"a quoted headline\""), "\"a quoted headline\"");
    assert_eq!(describe("   padded   "), "padded.");
    assert_eq!(describe(""), "");
}

#[test]
fn keeps_raw_quotes_in_description() {
    // The established mapping stores the description's raw scalar text: many
    // descriptions are quoted headlines whose quotes are prose, not YAML.
    let src = "---\nname: n\ndescription: \"VERIFIED: it was patched\"\nmetadata:\n  type: user\n---\nbody\n";
    let parsed = parse_memory_file(src).unwrap().unwrap();
    assert_eq!(parsed.text, "\"VERIFIED: it was patched\"\n\nbody");
}

#[test]
fn folds_block_scalar_description() {
    let src = r"---
name: folded-note
description: >
  first fragment
  second fragment

  after a blank line
metadata:
  type: project
---
body
";
    let parsed = parse_memory_file(src).unwrap().unwrap();
    assert_eq!(
        parsed.text,
        "first fragment second fragment\nafter a blank line.\n\nbody"
    );
    assert_eq!(parsed.kind, "project");
}

#[test]
fn reads_literal_block_scalar_description() {
    let src = r"---
name: literal-note
description: |-
  line one
  line two.
metadata:
  type: reference
---
body
";
    let parsed = parse_memory_file(src).unwrap().unwrap();
    assert_eq!(parsed.text, "line one\nline two.\n\nbody");
}

#[test]
fn reads_nested_metadata_type() {
    let parsed = parse_memory_file(SAMPLE).unwrap().unwrap();
    assert_eq!(parsed.kind, "feedback");
    assert!(parsed.tags.contains(&"feedback".to_string()));
}

#[test]
fn top_level_type_is_not_mistaken_for_metadata_type() {
    // A stray top-level `type:` must not populate `metadata.type`.
    let src = "---\nname: n\ntype: bogus\ndescription: d.\n---\nbody\n";
    let parsed = parse_memory_file(src).unwrap().unwrap();
    assert_eq!(parsed.kind, "");
    assert_eq!(parsed.tags, vec!["n".to_string()]);
}

#[test]
fn missing_frontmatter_is_none() {
    assert!(
        parse_memory_file("# Memory Index\n\nno frontmatter here\n")
            .unwrap()
            .is_none()
    );
    assert!(split_frontmatter("plain text").is_none());
}

#[test]
fn unterminated_frontmatter_is_none() {
    assert!(
        parse_memory_file("---\nname: x\nnever closed\n")
            .unwrap()
            .is_none()
    );
}

#[test]
fn frontmatter_without_name_is_error() {
    let err = parse_memory_file("---\ndescription: d\n---\nbody\n").unwrap_err();
    assert!(err.to_string().contains("no `name`"), "{err}");
}

#[test]
fn extracts_and_normalises_wikilink_targets() {
    let body = "see [[plain]], [[with-alias|Alias Text]], [[anchored#section]], \
                [[trailing.md]], [[]], [[unclosed";
    assert_eq!(
        wikilink_targets(body),
        vec![
            "plain".to_string(),
            "with-alias".to_string(),
            "anchored".to_string(),
            "trailing".to_string(),
        ]
    );
}

#[test]
fn wikilink_scan_is_utf8_safe() {
    // Multi-byte characters around the link must not panic the byte scanner.
    let body = "— café — [[naïve-slug]] — 日本語 —";
    assert_eq!(wikilink_targets(body), vec!["naïve-slug".to_string()]);
}

#[test]
fn headline_match_identifies_own_drawer() {
    assert!(is_import_of("head.\n\nbody", "head.\n\ndifferent body"));
    assert!(!is_import_of("other head.\n\nbody", "head.\n\nbody"));
    // Empty headline falls back to whole-text equality.
    assert!(is_import_of("\n\nbody", "\n\nbody"));
    assert!(!is_import_of("\n\nbody", "\n\nother"));
}

// ---------------------------------------------------------------------------
// Stub trusty-memory JSON-RPC daemon
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StubState {
    /// Every `memory_remember` params object, in call order.
    writes: Vec<Value>,
    /// tag → drawer ids carrying it.
    by_tag: HashMap<String, Vec<String>>,
    /// drawer id → stored content.
    content: HashMap<String, String>,
}

type Stub = Arc<Mutex<StubState>>;

async fn rpc(
    axum::extract::State(state): axum::extract::State<Stub>,
    axum::Json(body): axum::Json<Value>,
) -> axum::Json<Value> {
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let params = body.get("params").cloned().unwrap_or(Value::Null);
    let mut st = state.lock().expect("stub lock");
    let result = match method {
        "memory_list" => {
            let tag = params.get("tag").and_then(Value::as_str).unwrap_or("");
            let drawers: Vec<Value> = st
                .by_tag
                .get(tag)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|id| {
                    json!({
                        "drawer_id": id,
                        "content": st.content.get(id).cloned().unwrap_or_default(),
                    })
                })
                .collect();
            json!({ "palace": "stub", "drawers": drawers })
        }
        "memory_remember" => {
            st.writes.push(params.clone());
            let id = format!("drawer-{}", st.writes.len());
            let content = params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            st.content.insert(id.clone(), content);
            if let Some(tags) = params.get("tags").and_then(Value::as_array) {
                for tag in tags.iter().filter_map(Value::as_str) {
                    st.by_tag
                        .entry(tag.to_string())
                        .or_default()
                        .push(id.clone());
                }
            }
            json!({ "drawer_id": id, "status": "stored" })
        }
        other => json!({ "error": format!("unexpected method {other}") }),
    };
    axum::Json(json!({ "jsonrpc": "2.0", "id": 1, "result": result }))
}

/// Start the stub on an ephemeral loopback port; returns its base URL + state.
async fn start_stub() -> (String, Stub) {
    let state: Stub = Arc::new(Mutex::new(StubState::default()));
    let app = axum::Router::new()
        .route("/rpc", axum::routing::post(rpc))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state)
}

fn opts(dir: &std::path::Path, url: &str, dry_run: bool) -> ImportOptions {
    ImportOptions {
        dir: dir.to_path_buf(),
        palace: "stub".to_string(),
        dry_run,
        allow_secret_like: true,
        memory_url: Some(url.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Import loop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dry_run_writes_nothing() {
    let dir = write_dir(&[("admin-merge-only-on-green.md", SAMPLE)]);
    let (url, state) = start_stub().await;

    let report = run_import(&opts(dir.path(), &url, true)).await.unwrap();

    assert!(report.dry_run);
    assert_eq!(report.total, 1);
    assert_eq!(report.created, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(report.files[0].status, ImportStatus::WouldCreate);
    assert!(report.files[0].drawer_id.is_none());
    assert!(
        state.lock().unwrap().writes.is_empty(),
        "dry run must issue no memory_remember calls"
    );
}

#[tokio::test]
async fn import_is_idempotent() {
    let dir = write_dir(&[("admin-merge-only-on-green.md", SAMPLE)]);
    let (url, state) = start_stub().await;

    let first = run_import(&opts(dir.path(), &url, false)).await.unwrap();
    assert_eq!(first.created, 1);
    assert_eq!(first.files[0].status, ImportStatus::Created);
    let drawer_id = first.files[0]
        .drawer_id
        .clone()
        .expect("drawer id reported");

    let second = run_import(&opts(dir.path(), &url, false)).await.unwrap();
    assert_eq!(second.created, 0);
    assert_eq!(second.skipped, 1);
    assert_eq!(second.files[0].status, ImportStatus::Skipped);
    assert_eq!(
        second.files[0].drawer_id.as_deref(),
        Some(drawer_id.as_str())
    );

    assert_eq!(
        state.lock().unwrap().writes.len(),
        1,
        "re-running must not write a duplicate drawer"
    );
}

#[tokio::test]
async fn write_payload_matches_the_established_mapping() {
    let dir = write_dir(&[("admin-merge-only-on-green.md", SAMPLE)]);
    let (url, state) = start_stub().await;
    run_import(&opts(dir.path(), &url, false)).await.unwrap();

    let writes = state.lock().unwrap().writes.clone();
    let sent = &writes[0];
    assert_eq!(sent["palace"], "stub");
    assert_eq!(sent["force"], true);
    assert_eq!(sent["allow_secret_like"], true);
    assert!(
        sent["text"]
            .as_str()
            .unwrap()
            .starts_with("Admin-merge bypasses bot-approval ONLY — never a red CI gate;.\n\n")
    );
    assert_eq!(sent["tags"][0], "admin-merge-only-on-green");
}

#[tokio::test]
async fn linking_drawer_does_not_block_its_target() {
    // `admin-merge-only-on-green.md` links to `[[gate-merge-commands-with-and]]`,
    // so its drawer carries that slug as a tag. Importing the linked file must
    // still create its own drawer — the tag alone must not read as "present".
    let target = "---\nname: gate-merge-commands-with-and\ndescription: Gate merges with &&.\nmetadata:\n  type: feedback\n---\nGate body.\n";
    let dir = write_dir(&[
        ("admin-merge-only-on-green.md", SAMPLE),
        ("gate-merge-commands-with-and.md", target),
    ]);
    let (url, state) = start_stub().await;

    let report = run_import(&opts(dir.path(), &url, false)).await.unwrap();

    assert_eq!(report.created, 2, "{:#?}", report.files);
    assert_eq!(report.failed, 0);
    assert_eq!(state.lock().unwrap().writes.len(), 2);
}

#[tokio::test]
async fn non_memory_files_are_skipped_and_non_markdown_ignored() {
    let dir = write_dir(&[
        ("MEMORY.md", "# Memory Index\n\n- a list of links\n"),
        ("notes.txt", "not markdown at all"),
    ]);
    let (url, state) = start_stub().await;

    let report = run_import(&opts(dir.path(), &url, false)).await.unwrap();

    assert_eq!(report.total, 1, "only the .md file is considered");
    assert_eq!(report.skipped, 1);
    assert_eq!(report.files[0].file, "MEMORY.md");
    assert_eq!(report.files[0].status, ImportStatus::Skipped);
    assert!(state.lock().unwrap().writes.is_empty());
}

#[tokio::test]
async fn unparseable_file_is_reported_not_fatal() {
    let dir = write_dir(&[
        (
            "aaa-broken.md",
            "---\ndescription: no name here\n---\nbody\n",
        ),
        ("zzz-good.md", SAMPLE),
    ]);
    let (url, _state) = start_stub().await;

    let report = run_import(&opts(dir.path(), &url, false)).await.unwrap();

    assert_eq!(report.total, 2);
    assert_eq!(report.failed, 1);
    assert_eq!(report.created, 1);
    assert_eq!(report.files[0].status, ImportStatus::Failed);
    assert!(
        report.files[0]
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("parse failed")
    );
    // The good file, sorted after the broken one, still landed.
    assert_eq!(report.files[1].status, ImportStatus::Created);
}

#[test]
fn report_serialises_with_per_file_status_and_drawer_id() {
    let report = super::ImportReport {
        palace: "p".into(),
        dir: "/d".into(),
        dry_run: false,
        total: 1,
        created: 1,
        skipped: 0,
        failed: 0,
        files: vec![super::FileResult {
            file: "a.md".into(),
            name: Some("a".into()),
            status: ImportStatus::Created,
            drawer_id: Some("uuid-1".into()),
            tags: vec!["a".into()],
            detail: None,
        }],
    };
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["files"][0]["status"], "created");
    assert_eq!(json["files"][0]["drawer_id"], "uuid-1");
    assert!(json["files"][0].get("detail").is_none());
}
