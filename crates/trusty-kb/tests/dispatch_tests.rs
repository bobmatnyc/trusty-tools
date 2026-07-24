//! End-to-end tests driving every tool through the MCP dispatch seam
//! (`trusty_kb::mcp::dispatch_with`) against temp-dir roots — the "test every
//! tool through the dispatcher" requirement from the build brief. These exercise
//! the JSON-RPC envelope + per-call root resolution that the per-module unit
//! tests (which call `KbStore` directly) do not.

use serde_json::{Value, json};
use trusty_common::mcp::Request;
use trusty_kb::mcp::{ServerConfig, dispatch_with};
use trusty_kb::roots::Roots;

/// A ServerConfig whose knowledge dir + default root live under a fresh tempdir.
fn config() -> (tempfile::TempDir, ServerConfig, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let kdir = tmp.path().to_path_buf();
    let root = kdir.join("bob-kb");
    let cfg = ServerConfig::new(Roots::new(kdir, root.clone()));
    (tmp, cfg, root)
}

fn req(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(1)),
        method: method.into(),
        params: Some(params),
    }
}

/// Call a tool and return (isError, parsed-text-or-raw).
async fn call(cfg: &ServerConfig, name: &str, args: Value) -> (bool, Value) {
    let params = json!({ "name": name, "arguments": args });
    let resp = dispatch_with(cfg.clone(), req("tools/call", params)).await;
    let result = resp.result.expect("tools/call result");
    let is_error = result["isError"].as_bool().unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let parsed = serde_json::from_str(text).unwrap_or(Value::String(text.to_string()));
    (is_error, parsed)
}

/// Why: ensure_structure + status are the tree-bootstrap path a client runs
/// first; both must work through the dispatcher against an explicit root.
/// What: ensures structure, then asserts status reports six known collections
/// and the root index.
#[tokio::test]
async fn ensure_then_status() {
    let (_t, cfg, root) = config();
    let r = root.to_string_lossy().to_string();
    let (err, ensure) = call(&cfg, "kb_ensure_structure", json!({ "root": r })).await;
    assert!(!err);
    assert_eq!(ensure["created_dirs"].as_array().unwrap().len(), 6);

    let (err, status) = call(&cfg, "kb_status", json!({ "root": r })).await;
    assert!(!err);
    assert!(status["has_root_index"].as_bool().unwrap());
    assert!(status["collections"].as_array().unwrap().len() >= 6);
}

/// Why: byte-stable put is the primary determinism guarantee — verified through
/// the dispatcher, reading the file off disk.
/// What: puts the same entity twice via dispatch; asserts the written file is
/// byte-identical and carries the expected type/title.
#[tokio::test]
async fn put_is_byte_identical_through_dispatch() {
    let (_t, cfg, root) = config();
    let r = root.to_string_lossy().to_string();
    let args = json!({
        "root": r, "collection": "people", "name": "Ada Lovelace",
        "frontmatter": { "aliases": ["Ada"], "description": "First programmer" },
        "body": "Mathematician."
    });
    let (err, out) = call(&cfg, "kb_put_entity", args.clone()).await;
    assert!(!err, "{out}");
    let path = root.join("people/ada-lovelace.md");
    let first = std::fs::read_to_string(&path).unwrap();
    let (err2, _) = call(&cfg, "kb_put_entity", args).await;
    assert!(!err2);
    let second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(first, second, "repeat put must be byte-identical");
    assert!(first.contains("type: Person"));
    assert!(first.contains("title: Ada Lovelace"));
}

/// Why: merge must union + preserve unknown keys through the dispatcher.
/// What: creates then merges; asserts a custom key survives and get_entity
/// returns the merged frontmatter.
#[tokio::test]
async fn merge_preserves_keys_through_dispatch() {
    let (_t, cfg, root) = config();
    let r = root.to_string_lossy().to_string();
    call(
        &cfg,
        "kb_put_entity",
        json!({
            "root": r, "collection": "people", "name": "Grace",
            "frontmatter": { "custom_key": "keepme" }
        }),
    )
    .await;
    call(
        &cfg,
        "kb_put_entity",
        json!({
            "root": r, "collection": "people", "name": "Grace", "merge": true,
            "frontmatter": { "description": "Compiler pioneer" }
        }),
    )
    .await;
    let (err, got) = call(
        &cfg,
        "kb_get_entity",
        json!({
            "root": r, "collection": "people", "name": "Grace"
        }),
    )
    .await;
    assert!(!err);
    assert_eq!(got["frontmatter"]["custom_key"], "keepme");
    assert_eq!(got["frontmatter"]["description"], "Compiler pioneer");
}

/// Why: the inverse-edge reconciler must materialise the mirror edge, observable
/// through the dispatcher.
/// What: creates Grace, then Ada who `knows [[Grace]]`; asserts Grace gains a
/// `knows` edge back to Ada.
#[tokio::test]
async fn reconciler_materialises_inverse_through_dispatch() {
    let (_t, cfg, root) = config();
    let r = root.to_string_lossy().to_string();
    call(
        &cfg,
        "kb_put_entity",
        json!({
            "root": r, "collection": "people", "name": "Grace"
        }),
    )
    .await;
    call(
        &cfg,
        "kb_put_entity",
        json!({
            "root": r, "collection": "people", "name": "Ada",
            "frontmatter": { "knows": "[[Grace]]" }
        }),
    )
    .await;
    let (_e, grace) = call(
        &cfg,
        "kb_get_entity",
        json!({
            "root": r, "collection": "people", "name": "Grace"
        }),
    )
    .await;
    let knows = grace["frontmatter"]["knows"].as_array().unwrap();
    assert!(knows.iter().any(|v| v.as_str().unwrap().contains("Ada")));
}

/// Why: validate must surface each lint class through the dispatcher.
/// What: puts an entity with a dangling link; asserts a dangling_link finding.
#[tokio::test]
async fn validate_reports_dangling_through_dispatch() {
    let (_t, cfg, root) = config();
    let r = root.to_string_lossy().to_string();
    call(
        &cfg,
        "kb_put_entity",
        json!({
            "root": r, "collection": "people", "name": "Ada",
            "frontmatter": { "knows": "[[Ghost]]" }
        }),
    )
    .await;
    let (err, findings) = call(&cfg, "kb_validate", json!({ "root": r })).await;
    assert!(!err);
    let kinds: Vec<&str> = findings
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"dangling_link"));
}

/// Why: report_only is the REQUIRED default and must not write.
/// What: seeds a loose note, runs convert with no mode; asserts the source file
/// is byte-unchanged and applied=false.
#[tokio::test]
async fn convert_report_only_is_side_effect_free() {
    let (_t, cfg, root) = config();
    std::fs::create_dir_all(root.join("notes")).unwrap();
    std::fs::write(root.join("notes/hello.md"), "# Hi\n\nbody\n").unwrap();
    let before = std::fs::read_to_string(root.join("notes/hello.md")).unwrap();
    let (err, report) = call(
        &cfg,
        "kb_convert_tree",
        json!({
            "source_root": root.to_string_lossy()
        }),
    )
    .await;
    assert!(!err);
    assert_eq!(report["applied"], false);
    let after = std::fs::read_to_string(root.join("notes/hello.md")).unwrap();
    assert_eq!(before, after);
}

/// Why: convert in_place must be idempotent (second pass = no-op).
/// What: runs in_place twice; asserts the second report has no Move actions and
/// the mapped entity gained a Person type.
#[tokio::test]
async fn convert_in_place_idempotent_through_dispatch() {
    let (_t, cfg, root) = config();
    std::fs::create_dir_all(root.join("people")).unwrap();
    std::fs::write(root.join("people/ada-lovelace.md"), "First programmer.\n").unwrap();
    let sr = root.to_string_lossy().to_string();
    call(
        &cfg,
        "kb_convert_tree",
        json!({ "source_root": sr, "mode": "in_place" }),
    )
    .await;
    let after_first = std::fs::read_to_string(root.join("people/ada-lovelace.md")).unwrap();
    let (_e, report) = call(
        &cfg,
        "kb_convert_tree",
        json!({ "source_root": sr, "mode": "in_place" }),
    )
    .await;
    let after_second = std::fs::read_to_string(root.join("people/ada-lovelace.md")).unwrap();
    assert_eq!(
        after_first, after_second,
        "second convert pass must be byte-stable"
    );
    let moves = report["plan"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["action"] == "move")
        .count();
    assert_eq!(moves, 0);
    assert!(after_first.contains("type: Person"));
}

/// Why: one service instance must enumerate every assistant's tree.
/// What: creates two agent trees via the `agent` arg, then kb_list_trees;
/// asserts both appear with entity counts.
#[tokio::test]
async fn list_trees_enumerates_all_assistants() {
    let (_t, cfg, _root) = config();
    call(
        &cfg,
        "kb_put_entity",
        json!({
            "agent": "izzie", "collection": "people", "name": "Ada"
        }),
    )
    .await;
    call(
        &cfg,
        "kb_put_entity",
        json!({
            "agent": "cto-bot", "collection": "projects", "name": "Search"
        }),
    )
    .await;
    let (err, trees) = call(&cfg, "kb_list_trees", json!({})).await;
    assert!(!err);
    let names: Vec<&str> = trees
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"izzie"));
    assert!(names.contains(&"cto-bot"));
    let izzie = trees
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "izzie")
        .unwrap();
    assert_eq!(izzie["entity_count"], 1);
}
