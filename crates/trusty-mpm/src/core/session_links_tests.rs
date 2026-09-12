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

/// A `Write` that records the size of every call it receives.
///
/// Why: the interleave defect is invisible in the resulting bytes — one write of
/// `id\n` and two writes of `id` then `\n` produce an identical file. The only
/// observable is the call boundary, which is what an `O_APPEND` descriptor
/// interleaves on. Mirrors `core::savings_tests`'s `CountingSink` (#7579).
struct CountingSink {
    writes: Vec<usize>,
    bytes: Vec<u8>,
}

impl std::io::Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writes.push(buf.len());
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Why (critic HIGH 2, the #7579 shape): the daemon's `SessionStart`
/// correlation is this store's caller and it runs concurrently, so an id and
/// its newline reaching the `O_APPEND` descriptor as two writes lets a second
/// writer land its own id mid-line. A spliced line is a session id that never
/// existed, which `linked_claude_ids` would hand the statusline as a sibling to
/// fold. #7579 put ten such lines in the operator's savings ledger.
/// FAILS BEFORE THIS CHANGE: `writeln!(file, "{claude_id}")` records two writes
/// — measured `[36, 1]` for a UUID — where this asserts one.
/// Test: itself.
#[test]
fn an_id_and_its_newline_leave_in_one_write() {
    let mut sink = CountingSink {
        writes: Vec::new(),
        bytes: Vec::new(),
    };

    write_link_line(&mut sink, "3544c9e5-f90d-421f-9fb4-7a92d5c329ff").expect("write");

    assert_eq!(
        sink.writes.len(),
        1,
        "the id and its newline must reach the descriptor together, got {:?}",
        sink.writes
    );
    let written = String::from_utf8(sink.bytes).expect("utf-8");
    assert!(
        written.ends_with('\n'),
        "the single write must carry the terminator: {written:?}"
    );
    assert_eq!(
        written.matches('\n').count(),
        1,
        "exactly one terminator: {written:?}"
    );
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
