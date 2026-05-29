//! Tests for `seed_mcp_connections`, `seed_all`, and the pure helper functions
//! (`parse_skill_frontmatter`, `render_mcp_description`).
//!
//! Why: Keeps the doc/skill seeder tests (`seed_tests.rs`) separate from the
//! MCP + aggregate path so each file stays under the 500-line cap.
//! What: Stubbed-embedder integration tests plus pure-function unit tests.
//! Test: This file is itself the test coverage.
#![allow(clippy::await_holding_lock)]

use tempfile::tempdir;

use super::StubEmbedderShared;
use crate::init::seed::{parse_skill_frontmatter, render_mcp_description};
use crate::init::{MCP_SEED_SESSION_ID, ProjectInitializer};
use crate::memory::store::MemoryStore;
use crate::test_env::HOME_LOCK;

#[tokio::test]
async fn seed_mcp_connections_indexes_servers() {
    use crate::memory::redb_usearch::RedbUsearchStore;
    use crate::memory::store::Segment;

    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_home = std::env::var_os("HOME");
    let home_sandbox = tempdir().unwrap();
    // SAFETY: HOME_LOCK is held for the duration of this test.
    unsafe {
        std::env::set_var("HOME", home_sandbox.path());
    }

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let omd = root.join(".open-mpm").join("state");
    tokio::fs::create_dir_all(&omd).await.unwrap();

    // Fixture .mcp.json with two servers.
    let mcp_json = serde_json::json!({
        "mcpServers": {
            "kuzu-memory": {
                "type": "stdio",
                "command": "kuzu-memory",
                "args": ["mcp"],
                "env": {"KUZU_MEMORY_DB": "/tmp/db"}
            },
            "vector-search": {
                "type": "stdio",
                "command": "uv",
                "args": ["run", "mcp-vector-search", "mcp"],
                "description": "Semantic code search"
            }
        }
    });
    tokio::fs::write(
        root.join(".mcp.json"),
        serde_json::to_string_pretty(&mcp_json).unwrap(),
    )
    .await
    .unwrap();

    let dim = 16;
    let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
    let embedder = StubEmbedderShared { dim };

    let init = ProjectInitializer::new(&root, &omd);
    let n = init.seed_mcp_connections(&store, &embedder).await.unwrap();
    assert_eq!(n, 2, "expected two MCP servers seeded, got {n}");

    let payload = store
        .get(Segment::AgentMemory, "mcp:kuzu-memory")
        .await
        .unwrap()
        .expect("kuzu-memory MCP entry");
    assert_eq!(
        payload.get("tag").and_then(|v| v.as_str()),
        Some("configuration/mcp")
    );
    assert_eq!(
        payload.get("session_id").and_then(|v| v.as_str()),
        Some(MCP_SEED_SESSION_ID)
    );
    assert_eq!(
        payload.get("server_name").and_then(|v| v.as_str()),
        Some("kuzu-memory")
    );
    assert_eq!(
        payload.get("command").and_then(|v| v.as_str()),
        Some("kuzu-memory")
    );
    let content = payload.get("content").and_then(|v| v.as_str()).unwrap();
    assert!(content.contains("MCP Server: kuzu-memory"));
    assert!(content.contains("Command: kuzu-memory mcp"));
    assert!(content.contains("Environment: KUZU_MEMORY_DB"));

    let payload2 = store
        .get(Segment::AgentMemory, "mcp:vector-search")
        .await
        .unwrap()
        .expect("vector-search MCP entry");
    let content2 = payload2.get("content").and_then(|v| v.as_str()).unwrap();
    assert!(content2.contains("Description: Semantic code search"));

    // Re-seed should be a no-op.
    let n2 = init.seed_mcp_connections(&store, &embedder).await.unwrap();
    assert_eq!(n2, 0);

    assert!(omd.join("mcp_seeded.json").exists());

    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[tokio::test]
async fn seed_all_runs_all_three_seeders() {
    use crate::memory::redb_usearch::RedbUsearchStore;

    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_home = std::env::var_os("HOME");
    let home_sandbox = tempdir().unwrap();
    // SAFETY: HOME_LOCK is held for the duration of this test.
    unsafe {
        std::env::set_var("HOME", home_sandbox.path());
    }

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let omd = root.join(".open-mpm").join("state");
    tokio::fs::create_dir_all(&omd).await.unwrap();
    // docs
    tokio::fs::create_dir_all(root.join("docs/user"))
        .await
        .unwrap();
    tokio::fs::write(root.join("docs/user/quickstart.md"), "# Quickstart\n")
        .await
        .unwrap();
    // skills
    tokio::fs::create_dir_all(root.join(".open-mpm/skills"))
        .await
        .unwrap();
    tokio::fs::write(
        root.join(".open-mpm/skills/x.md"),
        "---\nname: x\ntags: [a]\n---\n\nbody\n",
    )
    .await
    .unwrap();
    // mcp
    tokio::fs::write(
        root.join(".mcp.json"),
        r#"{"mcpServers":{"s":{"command":"c","args":[]}}}"#,
    )
    .await
    .unwrap();

    let dim = 16;
    let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
    let embedder = StubEmbedderShared { dim };
    let init = ProjectInitializer::new(&root, &omd);
    let (docs, skills, mcp) = init.seed_all(&store, &embedder).await;
    assert_eq!(docs, 1);
    assert_eq!(skills, 1);
    assert_eq!(mcp, 1);

    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn parse_skill_frontmatter_extracts_fields() {
    let content = "---\nname: foo\ntags: [a, b, c]\nsummary: A summary\n---\n\nbody\n";
    let (name, tags, summary) = parse_skill_frontmatter(content).unwrap();
    assert_eq!(name, "foo");
    assert_eq!(tags, vec!["a", "b", "c"]);
    assert_eq!(summary, "A summary");

    // Missing frontmatter returns None.
    assert!(parse_skill_frontmatter("# Just markdown\n").is_none());

    // description falls back into the summary slot.
    let content2 = "---\nname: bar\ntags: [x]\ndescription: Desc here\n---\n\n";
    let (_, _, s) = parse_skill_frontmatter(content2).unwrap();
    assert_eq!(s, "Desc here");
}

#[test]
fn render_mcp_description_includes_all_fields() {
    let s = render_mcp_description(
        "myserver",
        "node",
        &["server.js".to_string(), "--port=3000".to_string()],
        "Helpful tool",
        &["API_KEY".to_string(), "DB_URL".to_string()],
    );
    assert!(s.contains("MCP Server: myserver"));
    assert!(s.contains("Command: node server.js --port=3000"));
    assert!(s.contains("Description: Helpful tool"));
    assert!(s.contains("Environment: API_KEY, DB_URL"));
}
