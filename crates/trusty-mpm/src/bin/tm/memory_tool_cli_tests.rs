//! Parse tests for the MCP-only memory tool verbs (#11625).
//!
//! Why: `tests.rs` is over the test SLOC cap; each verb gets its parse test
//! here instead.
//! What: one round-trip per verb, asserting the flags land in the variant.
//! Test: this file.

use clap::Parser;

use crate::cli::{Cli, Command, MemoryAction, MemoryKgAction, MemoryTaskAction, MemoryToolAction};

/// Parse `tm memory <args>` into its tool action.
fn tool(args: &[&str]) -> MemoryToolAction {
    let argv = ["trusty-mpm", "memory"].iter().chain(args).copied();
    match Cli::try_parse_from(argv).expect("must parse").command {
        Some(Command::Memory {
            action: MemoryAction::Tool(action),
        }) => action,
        other => panic!("expected Memory/Tool, got {other:?}"),
    }
}

#[test]
fn cli_parses_memory_recall_deep() {
    let action = tool(&[
        "recall-deep",
        "q",
        "--palace",
        "p",
        "--top-k",
        "3",
        "--room",
        "Planning",
        "--min-score",
        "0.4",
        "--json",
        "--memory-socket",
        "/tmp/m.sock",
    ]);
    let MemoryToolAction::RecallDeep {
        query,
        palace,
        top_k,
        room,
        min_score,
        out,
    } = action
    else {
        panic!("expected RecallDeep, got {action:?}");
    };
    assert_eq!(query, "q");
    assert_eq!(palace.as_deref(), Some("p"));
    assert_eq!(top_k, Some(3));
    assert_eq!(room.as_deref(), Some("Planning"));
    assert_eq!(min_score, Some(0.4));
    assert!(out.json);
    assert_eq!(out.memory_socket, Some("/tmp/m.sock".into()));
}

#[test]
fn cli_parses_memory_recall_all() {
    let action = tool(&["recall-all", "q", "--deep", "--scope", "all"]);
    let MemoryToolAction::RecallAll {
        query, deep, scope, ..
    } = action
    else {
        panic!("expected RecallAll, got {action:?}");
    };
    assert_eq!(query, "q");
    assert!(deep);
    assert_eq!(scope.as_deref(), Some("all"));
    // The tool spans every palace, so `--palace` is not offered.
    let argv = ["trusty-mpm", "memory", "recall-all", "q", "--palace", "p"];
    assert!(Cli::try_parse_from(argv).is_err());
}

#[test]
fn cli_parses_memory_list() {
    let action = tool(&["list", "--tag", "t", "--limit", "5", "--palace", "p"]);
    let MemoryToolAction::List {
        palace,
        room,
        tag,
        limit,
        ..
    } = action
    else {
        panic!("expected List, got {action:?}");
    };
    assert_eq!(palace.as_deref(), Some("p"));
    assert!(room.is_none());
    assert_eq!(tag.as_deref(), Some("t"));
    assert_eq!(limit, Some(5));
}

#[test]
fn cli_parses_memory_prompt_context() {
    let action = tool(&["prompt-context", "tga", "--json"]);
    let MemoryToolAction::PromptContext { query, out } = action else {
        panic!("expected PromptContext, got {action:?}");
    };
    assert_eq!(query.as_deref(), Some("tga"));
    assert!(out.json);
}

#[test]
fn cli_parses_memory_task_add() {
    let action = tool(&["task", "add", "ship", "--tag", "a", "--tag", "b"]);
    let MemoryToolAction::Task {
        action: MemoryTaskAction::Add { content, tags, .. },
    } = action
    else {
        panic!("expected Task/Add, got {action:?}");
    };
    assert_eq!(content, "ship");
    assert_eq!(tags, ["a", "b"]);
}

#[test]
fn cli_parses_memory_task_list() {
    let action = tool(&["task", "list", "--include-completed"]);
    let MemoryToolAction::Task {
        action: MemoryTaskAction::List {
            include_completed, ..
        },
    } = action
    else {
        panic!("expected Task/List, got {action:?}");
    };
    assert!(include_completed);
}

#[test]
fn cli_parses_memory_task_complete() {
    let action = tool(&["task", "complete", "d1", "--palace", "p"]);
    let MemoryToolAction::Task {
        action: MemoryTaskAction::Complete {
            drawer_id, palace, ..
        },
    } = action
    else {
        panic!("expected Task/Complete, got {action:?}");
    };
    assert_eq!(drawer_id, "d1");
    assert_eq!(palace.as_deref(), Some("p"));
}

#[test]
fn cli_parses_memory_kg_assert() {
    let action = tool(&[
        "kg",
        "assert",
        "tga",
        "is_alias_for",
        "trusty-git-analytics",
        "--confidence",
        "0.8",
        "--provenance",
        "cli",
    ]);
    let MemoryToolAction::Kg {
        action:
            MemoryKgAction::Assert {
                subject,
                predicate,
                object,
                confidence,
                provenance,
                ..
            },
    } = action
    else {
        panic!("expected Kg/Assert, got {action:?}");
    };
    assert_eq!(
        (subject.as_str(), predicate.as_str(), object.as_str()),
        ("tga", "is_alias_for", "trusty-git-analytics")
    );
    assert_eq!(confidence, Some(0.8));
    assert_eq!(provenance.as_deref(), Some("cli"));
}

#[test]
fn cli_parses_memory_kg_query() {
    let action = tool(&["kg", "query", "tga"]);
    let MemoryToolAction::Kg {
        action: MemoryKgAction::Query { subject, .. },
    } = action
    else {
        panic!("expected Kg/Query, got {action:?}");
    };
    assert_eq!(subject, "tga");
}

#[test]
fn cli_parses_memory_kg_gaps() {
    let action = tool(&["kg", "gaps", "--palace", "p", "--json"]);
    let MemoryToolAction::Kg {
        action: MemoryKgAction::Gaps { palace, out },
    } = action
    else {
        panic!("expected Kg/Gaps, got {action:?}");
    };
    assert_eq!(palace.as_deref(), Some("p"));
    assert!(out.json);
}
