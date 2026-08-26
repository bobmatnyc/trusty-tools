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
use super::{DEDUP_CANDIDATE_LIMIT, ImportOptions, ImportStatus, has_headline_of, run_import};

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
    // #4851: the newline-spanning target exercises normalise_link_target's
    // multi-line rejection, the one branch this test's pointer claimed but
    // never reached.
    let body = "see [[plain]], [[with-alias|Alias Text]], [[anchored#section]], \
                [[trailing.md]], [[]], [[multi\nline]], [[unclosed";
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
    assert!(has_headline_of("head.\n\nbody", "head.\n\ndifferent body"));
    assert!(!has_headline_of("other head.\n\nbody", "head.\n\nbody"));
    // Empty headline falls back to whole-text equality.
    assert!(has_headline_of("\n\nbody", "\n\nbody"));
    assert!(!has_headline_of("\n\nbody", "\n\nother"));
}

#[test]
fn plain_multiline_description_is_error() {
    // A plain (unindicated) multi-line scalar used to import with the
    // description silently truncated to its first line.
    let src = "---\nname: n\ndescription: first half\n  second half\nmetadata:\n  type: user\n---\nbody\n";
    let err = parse_memory_file(src).unwrap_err();
    assert!(err.to_string().contains("plain multi-line scalar"), "{err}");
}

#[test]
fn colonless_frontmatter_line_is_error() {
    let err = parse_memory_file("---\nname: n\nstray line\n---\nbody\n").unwrap_err();
    assert!(err.to_string().contains("no `key:` separator"), "{err}");
}

#[test]
fn type_below_metadata_child_depth_is_ignored() {
    // Only `metadata:`'s direct child `type:` is the field we want.
    let src = "---\nname: n\nmetadata:\n  nested:\n    type: deep\n---\nbody\n";
    let parsed = parse_memory_file(src).unwrap().unwrap();
    assert_eq!(parsed.kind, "");
    assert_eq!(parsed.tags, vec!["n".to_string()]);
}

#[test]
fn dedent_survives_multibyte_indentation() {
    // `trim_start` strips U+00A0 too, so a block mixing it with ASCII spaces
    // puts the block indent (a byte count) inside a multi-byte character.
    let src = "---\nname: n\ndescription: |\n  ascii indent\n \u{a0}nbsp indent\n---\nbody\n";
    let parsed = parse_memory_file(src).unwrap().unwrap();
    assert!(parsed.text.starts_with("ascii indent"), "{:?}", parsed.text);
}

// ---------------------------------------------------------------------------
// Stub trusty-memory JSON-RPC daemon
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StubState {
    /// Every `memory_remember` params object, in call order.
    writes: Vec<Value>,
    /// Every `memory_list` params object, in call order.
    lists: Vec<Value>,
    /// tag → drawer ids carrying it.
    by_tag: HashMap<String, Vec<String>>,
    /// drawer id → stored content.
    content: HashMap<String, String>,
}

type Stub = Arc<Mutex<StubState>>;

fn rpc(state: &Stub, method: &str, params: Value) -> Value {
    let mut st = state.lock().expect("stub lock");
    match method {
        "memory_list" => {
            st.lists.push(params.clone());
            let tag = params.get("tag").and_then(Value::as_str).unwrap_or("");
            // The real handler filters by tag and *then* truncates to `limit`
            // (`handle_memory_list` in trusty-memory) — the stub must do the
            // same or the truncation boundary is untestable.
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            let drawers: Vec<Value> = st
                .by_tag
                .get(tag)
                .cloned()
                .unwrap_or_default()
                .iter()
                .take(limit)
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
    }
}

/// Start the stub on a temp Unix socket (#6286); returns the daemon + state.
async fn start_stub() -> (crate::uds_mock::MockMemoryDaemon, Stub) {
    let state: Stub = Arc::new(Mutex::new(StubState::default()));
    let served = Arc::clone(&state);
    let daemon = crate::uds_mock::spawn(move |method: &str, params: Value| {
        let state = Arc::clone(&served);
        let method = method.to_string();
        Box::pin(async move { Ok(rpc(&state, &method, params)) })
    })
    .await;
    (daemon, state)
}

fn opts(dir: &std::path::Path, socket: &std::path::Path, dry_run: bool) -> ImportOptions {
    ImportOptions {
        dir: dir.to_path_buf(),
        palace: "stub".to_string(),
        dry_run,
        allow_secret_like: true,
        memory_socket: Some(socket.to_path_buf()),
    }
}

// ---------------------------------------------------------------------------
// Import loop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dry_run_writes_nothing() {
    let dir = write_dir(&[("admin-merge-only-on-green.md", SAMPLE)]);
    let (daemon, state) = start_stub().await;

    let report = run_import(&opts(dir.path(), daemon.socket(), true))
        .await
        .unwrap();

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
    let (daemon, state) = start_stub().await;

    let first = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();
    assert_eq!(first.created, 1);
    assert_eq!(first.files[0].status, ImportStatus::Created);
    let drawer_id = first.files[0]
        .drawer_id
        .clone()
        .expect("drawer id reported");

    let second = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();
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
    let (daemon, state) = start_stub().await;
    run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();

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
    let (daemon, state) = start_stub().await;

    let report = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();

    assert_eq!(report.created, 2, "{:#?}", report.files);
    assert_eq!(report.failed, 0);
    assert_eq!(state.lock().unwrap().writes.len(), 2);
}

#[tokio::test]
async fn drifted_description_is_not_reimported() {
    // The defect this covers: the dedup check used to require the stored
    // drawer's first line to equal the file's derived headline, so rewording a
    // `description` made the drawer read as absent and the re-run wrote a
    // second one carrying the same slug tag.
    let dir = write_dir(&[("admin-merge-only-on-green.md", SAMPLE)]);
    let (daemon, state) = start_stub().await;

    let first = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();
    assert_eq!(first.created, 1);
    let drawer_id = first.files[0].drawer_id.clone().expect("drawer id");

    let drifted = SAMPLE.replace(
        "description: Admin-merge bypasses bot-approval ONLY — never a red CI gate;",
        "description: Admin-merge bypasses bot approval, never a red CI gate",
    );
    assert_ne!(
        drifted, SAMPLE,
        "the fixture rewrite must change the headline"
    );
    std::fs::write(dir.path().join("admin-merge-only-on-green.md"), &drifted).unwrap();

    let second = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();

    assert_eq!(second.created, 0, "{:#?}", second.files);
    assert_eq!(second.skipped, 1);
    assert_eq!(
        second.files[0].drawer_id.as_deref(),
        Some(drawer_id.as_str()),
        "the skip must point at the drawer already holding this file"
    );
    assert!(
        second.files[0]
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("drifted"),
        "{:#?}",
        second.files[0]
    );
    assert_eq!(
        state.lock().unwrap().writes.len(),
        1,
        "drift must never write a second drawer"
    );
}

#[tokio::test]
async fn drift_behind_a_referrer_is_not_reimported() {
    // The hard case: the drifted file's slug tag is on two drawers — its own,
    // and the referrer that links to it — and neither carries its new
    // headline. The referrer is excluded by re-deriving its wikilinks, which
    // leaves exactly one candidate to recognise as the file's own.
    let target = "---\nname: gate-merge-commands-with-and\ndescription: Gate merges with &&.\nmetadata:\n  type: feedback\n---\nGate body.\n";
    let dir = write_dir(&[
        ("admin-merge-only-on-green.md", SAMPLE),
        ("gate-merge-commands-with-and.md", target),
    ]);
    let (daemon, state) = start_stub().await;
    let first = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();
    assert_eq!(first.created, 2, "{:#?}", first.files);

    let drifted = target.replace(
        "description: Gate merges with &&.",
        "description: Always gate a merge behind `&&`.",
    );
    std::fs::write(dir.path().join("gate-merge-commands-with-and.md"), &drifted).unwrap();

    let second = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();

    assert_eq!(second.created, 0, "{:#?}", second.files);
    assert_eq!(second.failed, 0, "{:#?}", second.files);
    assert_eq!(state.lock().unwrap().writes.len(), 2);
}

#[tokio::test]
async fn truncated_candidate_set_fails_closed() {
    // More drawers share the slug tag than one `memory_list` page returns.
    // `memory_list` has no cursor, so a full page means absence cannot be
    // proven — reporting the file beats writing a possible duplicate.
    let dir = write_dir(&[("admin-merge-only-on-green.md", SAMPLE)]);
    let (daemon, state) = start_stub().await;
    {
        let mut st = state.lock().unwrap();
        let ids: Vec<String> = (0..DEDUP_CANDIDATE_LIMIT + 3)
            .map(|n| format!("filler-{n}"))
            .collect();
        for id in &ids {
            st.content
                .insert(id.clone(), format!("Unrelated {id}.\n\nbody"));
        }
        st.by_tag
            .insert("admin-merge-only-on-green".to_string(), ids);
    }

    let report = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();

    let st = state.lock().unwrap();
    assert_eq!(
        st.lists[0]["limit"].as_u64(),
        Some(DEDUP_CANDIDATE_LIMIT as u64),
        "the lookup must ask for the bounded page it reasons about"
    );
    assert_eq!(report.failed, 1, "{:#?}", report.files);
    assert!(
        report.files[0]
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("truncated"),
        "{:#?}",
        report.files[0]
    );
    assert!(
        st.writes.is_empty(),
        "a truncated candidate set must not write"
    );
}

#[tokio::test]
async fn ambiguous_candidates_fail_closed() {
    // Two drawers carry the slug and neither links to it, so neither can be
    // ruled out as the file's own. Guessing either way risks a duplicate.
    let dir = write_dir(&[("admin-merge-only-on-green.md", SAMPLE)]);
    let (daemon, state) = start_stub().await;
    {
        let mut st = state.lock().unwrap();
        for id in ["twin-a", "twin-b"] {
            st.content.insert(
                id.to_string(),
                format!("Some other headline ({id}).\n\nbody"),
            );
        }
        st.by_tag.insert(
            "admin-merge-only-on-green".to_string(),
            vec!["twin-a".to_string(), "twin-b".to_string()],
        );
    }

    let report = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();

    assert_eq!(report.failed, 1, "{:#?}", report.files);
    assert!(
        report.files[0]
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("refusing to guess"),
        "{:#?}",
        report.files[0]
    );
    assert!(state.lock().unwrap().writes.is_empty());
}

#[tokio::test]
async fn non_memory_files_are_skipped_and_non_markdown_ignored() {
    let dir = write_dir(&[
        ("MEMORY.md", "# Memory Index\n\n- a list of links\n"),
        ("notes.txt", "not markdown at all"),
    ]);
    let (daemon, state) = start_stub().await;

    let report = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();

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
    let (daemon, _state) = start_stub().await;

    let report = run_import(&opts(dir.path(), daemon.socket(), false))
        .await
        .unwrap();

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
