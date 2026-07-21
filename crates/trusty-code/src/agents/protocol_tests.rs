//! Tests for the `agents.*` JSON-RPC methods (issue #3449; base-agent
//! filtering issue #3465 follow-up).
//!
//! Why: split out of `protocol.rs` itself (issue #610's 500-SLOC production
//! cap, enforced by `scripts/check_line_cap.sh`) once the base-agent filter
//! additions tipped that file 1 line over — mirrors the identical
//! `workstreams/protocol.rs` + `protocol_tests.rs` split already established
//! in this crate: `#[path = "protocol_tests.rs"] mod tests;` in `protocol.rs`
//! keeps the module logically nested exactly as before (still `tests` from
//! any `super::`/doc-reference call site — only which file backs it moved),
//! while this file's `_tests.rs` suffix classifies it as a TEST file (1500
//! SLOC cap) instead of counting against the production cap.
//! What: every test from `protocol.rs`'s former inline `mod tests` block,
//! unchanged — `validate_agent_name` guards, `agents_list`'s embedded/disk
//! union and base-agent exclusion, `agents_create`/`agents_delete` and their
//! error mappings, `write_new_file`'s atomic-create/TOCTOU-race coverage, and
//! `register`'s wiring smoke test.
//! Test: this file — self-describing.

use super::*;

fn ctx() -> ConnectionContext {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    ConnectionContext::new(tx)
}

fn state(dir: &Path, project_bound: bool) -> AgentsCatalogState {
    AgentsCatalogState::new(dir.to_path_buf(), project_bound)
}

#[test]
fn validate_name_rejects_empty() {
    assert!(validate_agent_name("").is_err());
}

#[test]
fn validate_name_rejects_uppercase() {
    assert!(validate_agent_name("Engineer").is_err());
}

#[test]
fn validate_name_rejects_path_traversal() {
    assert!(validate_agent_name("../../etc/passwd").is_err());
    assert!(validate_agent_name("a/b").is_err());
    assert!(validate_agent_name("..").is_err());
}

#[test]
fn validate_name_rejects_too_long() {
    let long = "a".repeat(MAX_AGENT_NAME_LEN + 1);
    assert!(validate_agent_name(&long).is_err());
}

#[test]
fn validate_name_accepts_lowercase_alnum_hyphen() {
    assert!(validate_agent_name("my-custom-agent-2").is_ok());
}

#[tokio::test]
async fn list_returns_embedded_when_disk_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let s = state(tmp.path(), false);
    let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
    let agents = result["agents"].as_array().expect("array");
    assert_eq!(agents.len(), crate::assets::DEFAULT_AGENTS.len());
    assert!(agents.iter().all(|a| a["tier"] == "embedded"));
}

#[tokio::test]
async fn list_disk_override_wins_and_suppresses_embedded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("engineer.md"),
        "---\nname: engineer\ndescription: DISK OVERRIDE\n---\n\nBody.\n",
    )
    .expect("write");
    let s = state(tmp.path(), true);
    let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
    let agents = result["agents"].as_array().expect("array");
    let engineer_entries: Vec<_> = agents.iter().filter(|a| a["name"] == "engineer").collect();
    assert_eq!(engineer_entries.len(), 1, "must appear exactly once");
    assert_eq!(engineer_entries[0]["tier"], "project");
    assert_eq!(engineer_entries[0]["description"], "DISK OVERRIDE");
}

#[tokio::test]
async fn list_disk_only_entries_are_additive() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("my-custom.md"),
        "---\nname: my-custom\ndescription: Custom\n---\n\nBody.\n",
    )
    .expect("write");
    let s = state(tmp.path(), true);
    let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
    let agents = result["agents"].as_array().expect("array");
    assert_eq!(agents.len(), crate::assets::DEFAULT_AGENTS.len() + 1);
    assert!(
        agents
            .iter()
            .any(|a| a["name"] == "my-custom" && a["tier"] == "project")
    );
}

/// An unparseable disk file shadowing an embedded name must surface as
/// `tier: "broken"` — never as the healthy embedded entry, because
/// `resolve_agent`'s disk-wins rule means dispatch of that name WILL
/// fail (code-critic PR #3465 review, MEDIUM).
#[tokio::test]
async fn list_marks_unparseable_disk_override_as_broken() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // `extends:` naming a nonexistent parent makes `load_md_agent`'s
    // compose step fail — the same failure `resolve_agent` would refuse
    // to dispatch through.
    std::fs::write(
        tmp.path().join("engineer.md"),
        "---\nname: engineer\nextends: nonexistent-parent\n---\n\nBody.\n",
    )
    .expect("write");
    let s = state(tmp.path(), true);
    let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
    let agents = result["agents"].as_array().expect("array");
    let engineer_entries: Vec<_> = agents.iter().filter(|a| a["name"] == "engineer").collect();
    assert_eq!(engineer_entries.len(), 1, "must appear exactly once");
    assert_eq!(engineer_entries[0]["tier"], "broken");
    assert!(
        engineer_entries[0]["description"]
            .as_str()
            .expect("description")
            .contains("unparseable"),
    );
}

/// `agents.list` excludes every `BASE-*` composition-template name —
/// even when a real file exists on disk, as trusty-mpm's own bundle
/// installs (frontmatter `name: base-agent`, `role: base` — see
/// `crates/trusty-mpm/src/assets/agents/BASE-AGENT.md`) — while
/// `resolve_agent` (the dispatch/composition path) still finds it by
/// name, since composing a leaf agent's `extends: base-agent` chain
/// depends on it (issue #3465 follow-up).
///
/// Why: pins the LISTING-only nature of the filter — [`is_base_agent`]
/// must never leak into `resolve_agent`/`load_md_agent`, only into
/// `agents_list`. [`is_base_agent`]'s case-insensitivity (needed
/// because the real bundle's on-disk filename is uppercase
/// `BASE-AGENT.md`) is covered directly by its own doc/the
/// `assets::BASE_AGENT_NAMES` contract; this test uses the lowercase
/// filename so the same file also exercises `resolve_agent`'s exact-name
/// path join without relying on filesystem case-folding, which is not
/// portable (case-sensitive on Linux CI, case-insensitive by default on
/// macOS).
/// What: writes `base-agent.md` and a normal `engineer.md`; asserts the
/// list has no `base-agent` entry but does have `engineer`, then asserts
/// `super::super::resolve_agent` still resolves `base-agent` by name.
/// Test: this test.
#[tokio::test]
async fn list_excludes_base_agents_but_resolve_agent_still_finds_them() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("base-agent.md"),
        "---\nname: base-agent\nrole: base\n---\n\nBase instructions.\n",
    )
    .expect("write");
    std::fs::write(
        tmp.path().join("engineer.md"),
        "---\nname: engineer\n---\n\nBody.\n",
    )
    .expect("write");

    let s = state(tmp.path(), true);
    let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
    let agents = result["agents"].as_array().expect("array");
    assert!(
        !agents
            .iter()
            .any(|a| a["name"].as_str().is_some_and(is_base_agent)),
        "no BASE-* composition template may appear in the listing, got: {agents:?}"
    );
    assert!(
        agents.iter().any(|a| a["name"] == "engineer"),
        "a real dispatchable agent must still be listed, got: {agents:?}"
    );

    let resolved = super::super::resolve_agent(tmp.path(), "base-agent")
        .expect("resolve_agent must still resolve a base template by name");
    assert_eq!(resolved.agent.name, "base-agent");
}

#[tokio::test]
async fn create_writes_file_and_returns_tier() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let s = state(tmp.path(), true);
    let result = agents_create(
        &s,
        json!({"name": "my-agent", "content": "---\nname: my-agent\n---\n\nBody.\n"}),
        ctx(),
    )
    .await
    .expect("create");
    assert_eq!(result["name"], "my-agent");
    assert_eq!(result["tier"], "project");
    assert!(tmp.path().join("my-agent.md").exists());
}

#[tokio::test]
async fn create_rejects_invalid_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let s = state(tmp.path(), true);
    let err = agents_create(&s, json!({"name": "Bad Name!", "content": "x"}), ctx())
        .await
        .expect_err("must reject");
    assert_eq!(err.code, -32003);
}

#[tokio::test]
async fn create_rejects_path_traversal_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let s = state(tmp.path(), true);
    let err = agents_create(&s, json!({"name": "../../evil", "content": "x"}), ctx())
        .await
        .expect_err("must reject");
    assert_eq!(err.code, -32003);
    assert!(!tmp.path().parent().unwrap().join("evil.md").exists());
}

#[tokio::test]
async fn create_rejects_embedded_name_collision() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let s = state(tmp.path(), true);
    let err = agents_create(
        &s,
        json!({"name": "engineer", "content": "---\nname: engineer\n---\n\nBody.\n"}),
        ctx(),
    )
    .await
    .expect_err("must reject embedded collision");
    assert_eq!(err.code, -32001);
    assert!(!tmp.path().join("engineer.md").exists());
}

#[tokio::test]
async fn create_rejects_existing_disk_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("dup.md"), "---\nname: dup\n---\n\nBody.\n").expect("write");
    let s = state(tmp.path(), true);
    let err = agents_create(
        &s,
        json!({"name": "dup", "content": "---\nname: dup\n---\n\nNew.\n"}),
        ctx(),
    )
    .await
    .expect_err("must reject existing file");
    // `-32009 already_exists` — NOT `-32008`, which is the workstreams'
    // `active_conflict` slot (code-critic PR #3465 review, LOW).
    assert_eq!(err.code, -32009);
    assert_eq!(err.data.as_ref().unwrap()["error_type"], "already_exists");
}

/// The conflict path must go through `create_new` (`O_CREAT|O_EXCL`) —
/// meaning a losing create can NEVER have truncated or overwritten the
/// existing file's content, even transiently (code-critic PR #3465
/// review, HIGH 1: the prior `exists()`-then-`write` shape let the
/// second of two racing creates silently clobber the first).
#[tokio::test]
async fn create_conflict_does_not_clobber_existing_content() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let original = "---\nname: dup\ndescription: ORIGINAL\n---\n\nBody.\n";
    std::fs::write(tmp.path().join("dup.md"), original).expect("write");
    let s = state(tmp.path(), true);

    let err = agents_create(&s, json!({"name": "dup", "content": "CLOBBER"}), ctx())
        .await
        .expect_err("must conflict");
    assert_eq!(err.code, -32009);

    let on_disk = std::fs::read_to_string(tmp.path().join("dup.md")).expect("read");
    assert_eq!(on_disk, original, "existing content must be untouched");
}

/// `write_new_file` is the shared atomic-create seam — exercise the io
/// `AlreadyExists` mapping directly, without a handler in the way.
#[test]
fn write_new_file_maps_already_exists_io_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("f.md");
    write_new_file(&path, "first", || "conflict".to_string()).expect("first create");
    let err =
        write_new_file(&path, "second", || "conflict".to_string()).expect_err("second create");
    assert_eq!(err.code, -32009);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "first",
        "loser must not touch the winner's content"
    );
}

/// Genuine concurrency: N threads race `write_new_file` against the
/// SAME path at the same time. Exactly one must win; every other must
/// get `AlreadyExists` (mapped to `-32009`) — never a torn/mixed write
/// (code-critic PR #3465 review, HIGH 1/HIGH 2: the original
/// `exists()`-then-`write` shape had no such guarantee under real
/// concurrency, only under the sequential re-creation this module's
/// other tests exercise).
#[test]
fn write_new_file_concurrent_create_exactly_one_wins() {
    use std::sync::{Arc, Barrier};

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = Arc::new(tmp.path().join("race.md"));
    const N: usize = 16;
    let barrier = Arc::new(Barrier::new(N));

    let handles: Vec<_> = (0..N)
        .map(|i| {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                write_new_file(&path, &format!("writer-{i}"), || "conflict".to_string()).map(|_| i)
            })
        })
        .collect();

    let results: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    let winners: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
    let losers: Vec<_> = results.iter().filter(|r| r.is_err()).collect();

    assert_eq!(winners.len(), 1, "exactly one racing create must win");
    assert_eq!(losers.len(), N - 1, "every other racer must lose");
    for loser in &losers {
        let err = loser.as_ref().unwrap_err();
        assert_eq!(
            err.code, -32009,
            "loser must get already_exists, not a torn write"
        );
    }

    // The file on disk must be exactly the winner's untouched content —
    // never a mix, truncation, or empty file from a lost race.
    let winner_idx = *winners[0].as_ref().unwrap();
    let on_disk = std::fs::read_to_string(path.as_path()).expect("read");
    assert_eq!(on_disk, format!("writer-{winner_idx}"));
}

#[tokio::test]
async fn delete_removes_disk_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("my-agent.md"),
        "---\nname: my-agent\n---\n\nBody.\n",
    )
    .expect("write");
    let s = state(tmp.path(), true);
    agents_delete(&s, json!({"name": "my-agent"}), ctx())
        .await
        .expect("delete");
    assert!(!tmp.path().join("my-agent.md").exists());
}

#[tokio::test]
async fn delete_missing_name_returns_not_found() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let s = state(tmp.path(), true);
    let err = agents_delete(&s, json!({"name": "totally-bogus"}), ctx())
        .await
        .expect_err("must 404");
    assert_eq!(err.code, -32002);
}

#[tokio::test]
async fn delete_embedded_name_returns_permission_denied() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let s = state(tmp.path(), true);
    let err = agents_delete(&s, json!({"name": "engineer"}), ctx())
        .await
        .expect_err("must 403");
    assert_eq!(err.code, -32001);
}

#[tokio::test]
async fn register_wires_all_three_methods() {
    use trusty_common::mcp::Request;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut router = Router::new();
    register(&mut router, state(tmp.path(), true));

    let req = Request {
        jsonrpc: Some("2.0".to_string()),
        id: Some(Value::from(1)),
        method: "agents.list".to_string(),
        params: None,
    };
    let resp = router.dispatch(req, &ctx()).await;
    assert!(resp.result.is_some(), "agents.list must be wired");
    assert!(resp.result.unwrap()["agents"].is_array());
}

/// `agents.list` includes a `plugin`-tier entry, namespaced
/// `<plugin>:<name>`, for every agent discovered under
/// `<project_root>/.claude/plugins/*/agents/` (issue #3539).
///
/// Why: this is the acceptance criterion for #3539's namespacing decision
/// at the catalog-listing level — `project_root_two_levels_up` must
/// correctly recover the project root from `state.dir` (a real
/// `.claude/agents` directory, unlike this file's other tests which pass a
/// bare tempdir root as `dir` for brevity — plugin discovery specifically
/// needs the real on-disk shape).
/// Test: this test.
#[tokio::test]
async fn list_includes_namespaced_plugin_agents() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let agents_dir = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).expect("mkdir");
    let plugin_agents_dir = tmp
        .path()
        .join(".claude")
        .join("plugins")
        .join("my-plugin")
        .join("agents");
    std::fs::create_dir_all(&plugin_agents_dir).expect("mkdir");
    std::fs::write(
        plugin_agents_dir.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Plugin reviewer\n---\n\nBody.\n",
    )
    .expect("write");

    let s = state(&agents_dir, true);
    let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
    let agents = result["agents"].as_array().expect("array");
    let plugin_entry = agents
        .iter()
        .find(|a| a["name"] == "my-plugin:reviewer")
        .unwrap_or_else(|| panic!("expected a namespaced plugin entry, got: {agents:?}"));
    assert_eq!(plugin_entry["tier"], "plugin");
    assert_eq!(plugin_entry["description"], "Plugin reviewer");
}

/// A plugin shipping an agent whose LOCAL name matches an existing project
/// agent's name does NOT override the project entry — both appear, the
/// project one unnamespaced and unchanged, the plugin one namespaced
/// (issue #3539's additive-only guarantee).
///
/// Why: this is the explicit collision test #3539 calls for — namespacing
/// makes the "additive only, never overrides project/bundled" contract
/// structural (the keys literally cannot collide), and this test pins that
/// outcome rather than just trusting the mechanism.
/// Test: this test.
#[tokio::test]
async fn list_plugin_agent_does_not_override_project_agent_of_same_local_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let agents_dir = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).expect("mkdir");
    std::fs::write(
        agents_dir.join("engineer.md"),
        "---\nname: engineer\ndescription: PROJECT engineer\n---\n\nProject body.\n",
    )
    .expect("write");
    let plugin_agents_dir = tmp
        .path()
        .join(".claude")
        .join("plugins")
        .join("my-plugin")
        .join("agents");
    std::fs::create_dir_all(&plugin_agents_dir).expect("mkdir");
    std::fs::write(
        plugin_agents_dir.join("engineer.md"),
        "---\nname: engineer\ndescription: PLUGIN engineer\n---\n\nPlugin body.\n",
    )
    .expect("write");

    let s = state(&agents_dir, true);
    let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
    let agents = result["agents"].as_array().expect("array");

    let project_entry = agents
        .iter()
        .find(|a| a["name"] == "engineer")
        .expect("project 'engineer' must still be present, untouched");
    assert_eq!(project_entry["tier"], "project");
    assert_eq!(project_entry["description"], "PROJECT engineer");

    let plugin_entry = agents
        .iter()
        .find(|a| a["name"] == "my-plugin:engineer")
        .expect("namespaced plugin entry must be present alongside it");
    assert_eq!(plugin_entry["tier"], "plugin");
    assert_eq!(plugin_entry["description"], "PLUGIN engineer");
}
