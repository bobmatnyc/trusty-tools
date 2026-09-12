//! Tests for the managed-session link store (#7617).
//!
//! Why: this store is what stops a Claude Code restart reading as a savings
//! disappearance, so its append-once, fail-open and path-safety behaviours are
//! the contract the `💸` segment leans on.
//! Test: this file IS the test module.

use super::*;

/// Why: the repeated `SessionStart` a resume raises must not grow the file.
/// Test: itself.
#[test]
fn a_link_is_recorded_once() {
    let root = tempfile::tempdir().expect("root");

    assert!(record_link(root.path(), "managed-1", "claude-a"));
    assert!(
        !record_link(root.path(), "managed-1", "claude-a"),
        "re-recording the same id must be a no-op"
    );
    assert_eq!(
        linked_claude_ids(root.path(), "claude-a"),
        vec!["claude-a".to_string()]
    );
}

/// Why (#7617): this is the case the issue reports — one managed session
/// carrying three Claude ids in an evening, each restart hiding the previous
/// one's savings rows.
/// Test: itself.
#[test]
fn a_second_id_joins_the_same_managed_session() {
    let root = tempfile::tempdir().expect("root");
    record_link(root.path(), "managed-1", "claude-a");
    record_link(root.path(), "managed-1", "claude-b");

    let from_new = linked_claude_ids(root.path(), "claude-b");

    assert_eq!(
        from_new,
        vec!["claude-a".to_string(), "claude-b".to_string()]
    );
}

/// Why: the queried id is part of its own set, so a caller folds one list
/// rather than remembering to add the id back.
/// Test: itself.
#[test]
fn siblings_include_the_queried_id() {
    let root = tempfile::tempdir().expect("root");
    record_link(root.path(), "managed-1", "claude-a");

    assert!(
        linked_claude_ids(root.path(), "claude-a").contains(&"claude-a".to_string()),
        "the queried id must be in its own sibling set"
    );
}

/// Why (Fail-Open Check): an id nothing links, and a store that does not exist,
/// must both answer "no siblings" rather than guessing — the segment then shows
/// its explicit empty state instead of another session's figure.
/// Test: itself.
#[test]
fn an_unlinked_id_has_no_siblings() {
    let root = tempfile::tempdir().expect("root");
    record_link(root.path(), "managed-1", "claude-a");

    assert!(linked_claude_ids(root.path(), "claude-z").is_empty());
    assert!(linked_claude_ids(root.path(), "").is_empty());
}

/// Test: itself.
#[test]
fn an_absent_store_has_no_siblings() {
    let root = tempfile::tempdir().expect("root");

    assert!(linked_claude_ids(root.path(), "claude-a").is_empty());
}

/// Why: the managed id becomes a filename verbatim, so a traversing or
/// separator-bearing id must be refused rather than resolved out of the store.
/// Test: itself.
#[test]
fn a_traversing_managed_id_is_refused() {
    let root = tempfile::tempdir().expect("root");

    for bad in ["..", ".", "", "../escape", "a/b", ".hidden"] {
        assert!(
            !record_link(root.path(), bad, "claude-a"),
            "{bad:?} must be refused as a link-file name"
        );
    }
    assert!(linked_claude_ids(root.path(), "claude-a").is_empty());
}
