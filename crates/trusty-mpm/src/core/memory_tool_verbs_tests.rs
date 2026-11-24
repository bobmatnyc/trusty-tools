//! Coverage for the MCP-only memory tools as `tm memory` verbs (#11625).
//!
//! Why: the argument keys must be trusty-memory's own schema keys, a write
//! must refuse to run without a palace, and a dead socket must be an error
//! naming that socket.
//! What: pure checks on [`MemoryTool`] plus [`super::run_tool`] against a
//! socket nothing serves. The live wire path is in
//! `tests/memory_tool_verbs_socket.rs`.
//! Test: this file.

use serde_json::{Value, json};

use super::{MemoryTool, PalaceScope};
use crate::core::memory_verbs::{MemoryVerbError, MemoryVerbOptions};

/// A `task add` with only its required key.
fn task_add() -> MemoryTool {
    MemoryTool::TaskAdd {
        content: "ship it".into(),
        room: None,
        tags: vec![],
    }
}

/// Why: a wrong method name or scope sends the call to the wrong handler or
/// drops a write's palace.
/// Test: itself.
#[test]
fn every_tool_names_its_method_and_scope() {
    let cases: Vec<(MemoryTool, &str, PalaceScope)> = vec![
        (
            MemoryTool::RecallDeep {
                query: "q".into(),
                top_k: None,
                room: None,
                min_score: None,
            },
            "memory_recall_deep",
            PalaceScope::Read,
        ),
        (
            MemoryTool::RecallAll {
                query: "q".into(),
                top_k: None,
                deep: false,
                scope: None,
            },
            "memory_recall_all",
            PalaceScope::None,
        ),
        (
            MemoryTool::List {
                room: None,
                wing: None,
                tag: None,
                limit: None,
            },
            "memory_list",
            PalaceScope::Read,
        ),
        (
            MemoryTool::PromptContext { query: None },
            "get_prompt_context",
            PalaceScope::None,
        ),
        (task_add(), "task_add", PalaceScope::Write),
        (
            MemoryTool::TaskList {
                include_completed: false,
            },
            "task_list",
            PalaceScope::Read,
        ),
        (
            MemoryTool::TaskComplete {
                drawer_id: "d".into(),
            },
            "task_complete",
            PalaceScope::Write,
        ),
        (
            MemoryTool::KgAssert {
                subject: "s".into(),
                predicate: "p".into(),
                object: "o".into(),
                confidence: None,
                provenance: None,
            },
            "kg_assert",
            PalaceScope::Write,
        ),
        (
            MemoryTool::KgQuery {
                subject: "s".into(),
            },
            "kg_query",
            PalaceScope::Read,
        ),
        (MemoryTool::KgGaps, "kg_gaps", PalaceScope::Read),
    ];
    for (tool, method, scope) in cases {
        assert_eq!(tool.method(), method, "{tool:?}");
        assert_eq!(tool.scope(), scope, "{tool:?}");
    }
}

/// Why: an omitted flag must leave the daemon default in force, and every key
/// sent must be one the tool's schema declares.
/// Test: itself.
#[test]
fn arguments_carry_only_the_schema_keys_supplied() {
    let bare = Value::Object(task_add().arguments());
    assert_eq!(bare, json!({ "content": "ship it" }));

    let all = MemoryTool::RecallAll {
        query: "q".into(),
        top_k: Some(3),
        deep: true,
        scope: Some("all".into()),
    };
    assert_eq!(
        Value::Object(all.arguments()),
        json!({ "q": "q", "top_k": 3, "deep": true, "scope": "all" })
    );

    let kg = MemoryTool::KgAssert {
        subject: "s".into(),
        predicate: "p".into(),
        object: "o".into(),
        confidence: Some(0.5),
        provenance: Some("test".into()),
    };
    assert_eq!(
        Value::Object(kg.arguments()),
        json!({ "subject": "s", "predicate": "p", "object": "o",
                "confidence": 0.5, "provenance": "test" })
    );
    assert!(MemoryTool::KgGaps.arguments().is_empty());
}

/// Why: a write with no palace has nowhere to land; it must fail before any
/// call, while a read falls through to the daemon's palace index (#6318).
/// Test: itself.
#[tokio::test]
async fn a_write_tool_without_a_resolvable_palace_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".trusty-tools")).expect("pin dir");
    std::fs::write(
        dir.path().join(".trusty-tools/trusty-memory.yaml"),
        "this: [is not: a pin\n",
    )
    .expect("pin file");
    let opts = MemoryVerbOptions {
        palace: None,
        socket: Some(dir.path().join("absent.sock")),
        cwd: Some(dir.path().to_path_buf()),
    };
    let err = super::run_tool(&task_add(), &opts)
        .await
        .expect_err("a write needs a palace");
    assert!(matches!(err, MemoryVerbError::Palace { .. }), "{err:?}");
}

/// Why: daemon down is the condition these verbs exist for; the error must
/// name the socket, never degrade to an empty result.
/// Test: itself.
#[tokio::test]
async fn a_dead_socket_is_an_error_naming_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let opts = MemoryVerbOptions {
        palace: Some("p".into()),
        socket: Some(socket.clone()),
        cwd: Some(dir.path().to_path_buf()),
    };
    let err = super::run_tool(&MemoryTool::KgGaps, &opts)
        .await
        .expect_err("nothing is serving that socket");
    assert!(matches!(err, MemoryVerbError::Call { .. }), "{err:?}");
    assert!(
        err.to_string().contains(&socket.display().to_string()),
        "{err}"
    );
}
