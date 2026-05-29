//! Unit tests for the `memories` command: CLI parsing + export/import round-trips.
//!
//! Why: Parsing must stay backward-compatible, and the JSONL round-trip is the
//! contract teammates rely on for cross-machine sharing.
//! What: `parse_args` cases plus store-backed export/import/auto-import tests
//! using a tempdir project root and stub embeddings.
//! Test: This module is itself the test coverage.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::json;
use tempfile::tempdir;

use super::{
    Command, ExportRecord, SHARED_MEMORIES_FILENAME, auto_import_if_changed, export_session,
    import_file, parse_args,
};
use crate::memory::store::{MemoryStore, Segment};
use crate::memory::{RedbUsearchStore, embed::ALL_MINI_LM_L6_V2_DIM};

#[test]
fn parses_export_with_session_and_output() {
    let cmd = parse_args(&["export", "--session", "sess-1", "--output", "/tmp/x.jsonl"]).unwrap();
    assert_eq!(
        cmd,
        Command::Export {
            session: Some("sess-1".to_string()),
            output: Some(PathBuf::from("/tmp/x.jsonl")),
            segment: None,
        }
    );
}

#[test]
fn parses_export_with_no_args() {
    let cmd = parse_args(&["export"]).unwrap();
    assert_eq!(
        cmd,
        Command::Export {
            session: None,
            output: None,
            segment: None,
        }
    );
}

#[test]
fn parses_export_with_segment_filter() {
    let cmd = parse_args(&["export", "--segment", "brief"]).unwrap();
    assert_eq!(
        cmd,
        Command::Export {
            session: None,
            output: None,
            segment: Some("brief".to_string()),
        }
    );
}

#[test]
fn rejects_invalid_export_segment() {
    // Unknown segment names must error before we open any stores.
    assert!(parse_args(&["export", "--segment", "bogus"]).is_err());
}

#[test]
fn parses_import_from_committed() {
    let cmd = parse_args(&["import", "--from-committed"]).unwrap();
    assert_eq!(
        cmd,
        Command::Import {
            input: None,
            from_committed: true,
        }
    );
}

#[test]
fn parses_list_with_scope() {
    let cmd = parse_args(&["list", "--scope", "imported"]).unwrap();
    assert_eq!(
        cmd,
        Command::List {
            scope: "imported".to_string()
        }
    );
}

#[test]
fn parses_list_default_scope_is_session() {
    let cmd = parse_args(&["list"]).unwrap();
    assert_eq!(
        cmd,
        Command::List {
            scope: "session".to_string()
        }
    );
}

#[test]
fn rejects_invalid_scope() {
    assert!(parse_args(&["list", "--scope", "bogus"]).is_err());
}

#[test]
fn rejects_unknown_action() {
    assert!(parse_args(&["foo"]).is_err());
}

/// Helper: insert a memory into a session's store with the given session_id
/// stamped into the payload, using a deterministic stub embedding.
async fn insert_test_memory(project_root: &Path, session_id: &str, id: &str, content: &str) {
    let session_dir = project_root
        .join(".open-mpm")
        .join("state")
        .join("sessions")
        .join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();
    let store = RedbUsearchStore::open(&session_dir, 16).unwrap();
    let mut v = vec![0.0f32; 16];
    for (i, b) in content.bytes().take(16).enumerate() {
        v[i] = (b as f32) / 255.0;
    }
    let payload = json!({
        "content": content,
        "session_id": session_id,
        "tag": "memories/session",
        "created_at": Utc::now().to_rfc3339(),
    });
    store
        .insert(Segment::AgentMemory, id, &v, payload)
        .await
        .unwrap();
}

/// Same as insert_test_memory but uses ALL_MINI_LM_L6_V2_DIM to match the
/// dimension export_session expects.
async fn insert_test_memory_real_dim(
    project_root: &Path,
    session_id: &str,
    id: &str,
    content: &str,
) {
    let session_dir = project_root
        .join(".open-mpm")
        .join("state")
        .join("sessions")
        .join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();
    let store = RedbUsearchStore::open(&session_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
    let mut v = vec![0.0f32; ALL_MINI_LM_L6_V2_DIM];
    for (i, b) in content.bytes().take(ALL_MINI_LM_L6_V2_DIM).enumerate() {
        v[i] = (b as f32) / 255.0;
    }
    let payload = json!({
        "content": content,
        "session_id": session_id,
        "tag": "memories/session",
        "created_at": Utc::now().to_rfc3339(),
    });
    store
        .insert(Segment::AgentMemory, id, &v, payload)
        .await
        .unwrap();
}

#[tokio::test]
async fn memory_export_produces_jsonl_with_machine_id() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    insert_test_memory_real_dim(&root, "sess-export", "fact-1", "Hello world.").await;

    let out = root.join("shared.jsonl");
    let n = export_session(&root, "sess-export", &out, None)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert!(out.exists());

    let body = std::fs::read_to_string(&out).unwrap();
    let line = body.lines().next().unwrap();
    let rec: ExportRecord = serde_json::from_str(line).unwrap();
    assert_eq!(rec.id, "fact-1");
    assert_eq!(rec.content, "Hello world.");
    assert_eq!(rec.session_id.as_deref(), Some("sess-export"));
    assert!(!rec.machine_id.is_empty(), "machine_id must be populated");
    assert_eq!(rec.embedding.len(), ALL_MINI_LM_L6_V2_DIM);
}

#[tokio::test]
async fn memory_import_from_jsonl_marks_imported_true() {
    // Hand-craft a JSONL file with one record and import it; verify the
    // resulting payload in the imported store has imported=true.
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join(".open-mpm")).unwrap();
    let in_path = root.join(".open-mpm").join(SHARED_MEMORIES_FILENAME);
    let rec = ExportRecord {
        id: "fact-x".to_string(),
        content: "Imported content.".to_string(),
        tag: Some("memories/session".to_string()),
        session_id: Some("remote-sess".to_string()),
        machine_id: "teammate-laptop".to_string(),
        exported_at: Utc::now().to_rfc3339(),
        embedding: vec![0.1f32; ALL_MINI_LM_L6_V2_DIM],
        payload: json!({"content": "Imported content.", "session_id": "remote-sess"}),
        segment: None,
    };
    std::fs::write(
        &in_path,
        format!("{}\n", serde_json::to_string(&rec).unwrap()),
    )
    .unwrap();

    let n = import_file(&root, &in_path).await.unwrap();
    assert_eq!(n, 1);

    // Inspect the imported store.
    let imported_dir = root
        .join(".open-mpm")
        .join("state")
        .join("sessions")
        .join("imported");
    let store = RedbUsearchStore::open(&imported_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
    let key = "imported:teammate-laptop:fact-x";
    let payload = store
        .get(Segment::AgentMemory, key)
        .await
        .unwrap()
        .expect("imported payload should exist");
    assert_eq!(
        payload.get("imported").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        payload.get("machine_id").and_then(|v| v.as_str()),
        Some("teammate-laptop")
    );
    assert_eq!(
        payload.get("session_id").and_then(|v| v.as_str()),
        Some("remote-sess")
    );
}

#[tokio::test]
async fn auto_import_skips_when_hash_unchanged() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join(".open-mpm")).unwrap();
    let in_path = root.join(".open-mpm").join(SHARED_MEMORIES_FILENAME);

    let rec = ExportRecord {
        id: "fact-y".to_string(),
        content: "stable".to_string(),
        tag: None,
        session_id: Some("rs".to_string()),
        machine_id: "m1".to_string(),
        exported_at: Utc::now().to_rfc3339(),
        embedding: vec![0.0f32; ALL_MINI_LM_L6_V2_DIM],
        payload: json!({"content": "stable"}),
        segment: None,
    };
    std::fs::write(
        &in_path,
        format!("{}\n", serde_json::to_string(&rec).unwrap()),
    )
    .unwrap();

    let n1 = auto_import_if_changed(&root).await.unwrap();
    assert_eq!(n1, 1);
    // Second call with same file contents should be a no-op (returns 0).
    let n2 = auto_import_if_changed(&root).await.unwrap();
    assert_eq!(n2, 0, "unchanged file should not re-import");
}

#[tokio::test]
async fn list_segment_round_trips_records() {
    // Sanity: list_segment returns what we inserted.
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    insert_test_memory(&root, "sx", "a", "alpha").await;
    insert_test_memory(&root, "sx", "b", "beta").await;

    let session_dir = root
        .join(".open-mpm")
        .join("state")
        .join("sessions")
        .join("sx");
    let store = RedbUsearchStore::open(&session_dir, 16).unwrap();
    let recs = store.list_segment(Segment::AgentMemory).await.unwrap();
    let ids: Vec<&str> = recs.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(ids.contains(&"a"));
    assert!(ids.contains(&"b"));
}

/// Insert a record into a specific segment with full-dim embedding.
async fn insert_into_segment(
    project_root: &Path,
    session_id: &str,
    segment: Segment,
    id: &str,
    content: &str,
) {
    let session_dir = project_root
        .join(".open-mpm")
        .join("state")
        .join("sessions")
        .join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();
    let store = RedbUsearchStore::open(&session_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
    let mut v = vec![0.0f32; ALL_MINI_LM_L6_V2_DIM];
    for (i, b) in content.bytes().take(ALL_MINI_LM_L6_V2_DIM).enumerate() {
        v[i] = (b as f32) / 255.0;
    }
    let payload = json!({
        "content": content,
        "session_id": session_id,
    });
    store.insert(segment, id, &v, payload).await.unwrap();
}

#[tokio::test]
async fn memory_export_filters_by_segment() {
    // Insert records into two different segments; export with a filter
    // and verify only the matching tier shows up.
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    insert_into_segment(&root, "sf", Segment::Brief, "b1", "active goal").await;
    insert_into_segment(&root, "sf", Segment::History, "h1", "decision X").await;

    let out = root.join("brief-only.jsonl");
    let n = export_session(&root, "sf", &out, Some(Segment::Brief))
        .await
        .unwrap();
    assert_eq!(n, 1, "only the Brief record should be exported");

    let body = std::fs::read_to_string(&out).unwrap();
    let line = body.lines().next().unwrap();
    let rec: ExportRecord = serde_json::from_str(line).unwrap();
    assert_eq!(rec.id, "b1");
    assert_eq!(rec.segment.as_deref(), Some("brief"));
}

#[tokio::test]
async fn memory_import_routes_by_segment() {
    // A JSONL record stamped with segment=context must end up in the
    // imported store's Context segment, NOT AgentMemory.
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join(".open-mpm")).unwrap();
    let in_path = root.join(".open-mpm").join(SHARED_MEMORIES_FILENAME);
    let rec = ExportRecord {
        id: "ctx-1".to_string(),
        content: "uses tokio runtime".to_string(),
        tag: None,
        session_id: Some("remote".to_string()),
        machine_id: "host-a".to_string(),
        exported_at: Utc::now().to_rfc3339(),
        embedding: vec![0.2f32; ALL_MINI_LM_L6_V2_DIM],
        payload: json!({"content": "uses tokio runtime"}),
        segment: Some("context".to_string()),
    };
    std::fs::write(
        &in_path,
        format!("{}\n", serde_json::to_string(&rec).unwrap()),
    )
    .unwrap();

    let n = import_file(&root, &in_path).await.unwrap();
    assert_eq!(n, 1);

    let imported_dir = root
        .join(".open-mpm")
        .join("state")
        .join("sessions")
        .join("imported");
    let store = RedbUsearchStore::open(&imported_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
    let key = "imported:host-a:ctx-1";
    // Should land in Context, not AgentMemory.
    assert!(store.get(Segment::Context, key).await.unwrap().is_some());
    assert!(
        store
            .get(Segment::AgentMemory, key)
            .await
            .unwrap()
            .is_none(),
        "context-tagged record must not leak into AgentMemory"
    );
}

#[tokio::test]
async fn memory_import_without_segment_defaults_to_agent_memory() {
    // Backward-compat: legacy JSONL exports (no `segment` field) must
    // continue to import into AgentMemory.
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join(".open-mpm")).unwrap();
    let in_path = root.join(".open-mpm").join(SHARED_MEMORIES_FILENAME);
    let rec = ExportRecord {
        id: "legacy-1".to_string(),
        content: "legacy fact".to_string(),
        tag: None,
        session_id: None,
        machine_id: "old-host".to_string(),
        exported_at: Utc::now().to_rfc3339(),
        embedding: vec![0.0f32; ALL_MINI_LM_L6_V2_DIM],
        payload: json!({"content": "legacy fact"}),
        segment: None,
    };
    std::fs::write(
        &in_path,
        format!("{}\n", serde_json::to_string(&rec).unwrap()),
    )
    .unwrap();

    let n = import_file(&root, &in_path).await.unwrap();
    assert_eq!(n, 1);

    let imported_dir = root
        .join(".open-mpm")
        .join("state")
        .join("sessions")
        .join("imported");
    let store = RedbUsearchStore::open(&imported_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
    let key = "imported:old-host:legacy-1";
    assert!(
        store
            .get(Segment::AgentMemory, key)
            .await
            .unwrap()
            .is_some(),
        "legacy record should default to AgentMemory"
    );
}
