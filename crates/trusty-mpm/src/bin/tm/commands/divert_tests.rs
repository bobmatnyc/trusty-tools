//! Tests for `tm divert bulk-read` (#6887).
//!
//! Why: these cover what the command decides around the worker call — how files
//! are gathered and bounded, and what the diversion ledger records. The worker
//! call itself is covered in `divert_worker_tests.rs`.
//! What: the `#[cfg(test)]` module `divert.rs` includes.

use super::*;

/// Build a reply with the shape a real child returns.
fn reply() -> WorkerReply {
    WorkerReply {
        text: "two functions".to_string(),
        model: "claude-haiku-4-5".to_string(),
        input_tokens: 10,
        output_tokens: 412,
        cache_read_tokens: 9094,
        cache_creation_tokens: 21922,
        cost_usd: 0.019422,
    }
}

/// Why: the hook's block reason quotes [`FALLTHROUGH_MARKER`] verbatim so the
/// agent has a literal to match. If the two drift, a fall-through leaves the
/// agent with a string it was told to look for and cannot find, and no next
/// move.
/// What: asserts the hook's reason contains the marker this module prints.
#[test]
fn fallthrough_marker_matches_the_hook_reason() {
    let reason = crate::commands::divert_check::block_reason("a.rs", 900);
    assert!(
        reason.contains(FALLTHROUGH_MARKER),
        "the block reason must quote the fall-through marker verbatim: {reason}"
    );
}

/// Why (#6887 acceptance criterion 2): the block reason is the agent's ONLY
/// channel back, so it must name the exact replacement command. A reason that
/// only says "too big" strands the agent.
/// What: asserts the reason names `tm divert bulk-read` and the file.
#[test]
fn block_reason_names_the_worker_command() {
    let reason = crate::commands::divert_check::block_reason("src/big.rs", 1200);
    assert!(
        reason.contains("tm divert bulk-read src/big.rs"),
        "{reason}"
    );
    assert!(
        reason.contains("1200"),
        "the reason must state the line count"
    );
}

/// Why: the worker must be told which bytes came from which file, or its answer
/// silently describes the wrong one. A named file that cannot be read is a hard
/// error for the same reason — a skipped file yields a confident answer about
/// content that was never sent.
/// What: each file becomes one `(path, content)` pair; a missing file errors.
#[test]
fn read_sources_labels_each_file() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn a() {}\n").unwrap();
    std::fs::write(&b, "fn b() {}\n").unwrap();

    let sources = read_sources(&[a.clone(), b.clone()]).expect("both files read");
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].0, a.display().to_string());
    assert_eq!(sources[0].1, "fn a() {}\n");
    assert_eq!(sources[1].0, b.display().to_string());

    let missing = tmp.path().join("nope.rs");
    let err = read_sources(&[missing]).expect_err("a missing file must not be skipped");
    assert!(err.to_string().contains("cannot read"), "{err}");
}

/// Why: an unbounded blob would fail API-side as a transport error, which reads
/// as a worker outage rather than as "these files are too big". Truncating
/// explicitly keeps the diagnosis local.
/// What: content past the budget is cut, marked, and stops the loop.
#[test]
fn read_sources_truncates_past_the_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let big = tmp.path().join("big.rs");
    let tail = tmp.path().join("tail.rs");
    std::fs::write(&big, "x".repeat(MAX_CONTENT_BYTES + 4096)).unwrap();
    std::fs::write(&tail, "fn never_sent() {}").unwrap();

    let sources = read_sources(&[big, tail]).expect("truncation is not an error");
    assert_eq!(sources.len(), 1, "the loop stops once the budget is spent");
    assert!(sources[0].1.contains("truncated: content budget reached"));
    assert!(sources[0].1.len() <= MAX_CONTENT_BYTES + 64);
}

/// Why (#6887 acceptance criterion 6): this line IS the record — #6873's usage
/// ledger is not merged — so it has to carry the running count AND the child's
/// own token and cost numbers. A line missing any of them cannot answer "what
/// did diversion save".
/// What: asserts the greppable marker and every field a reader needs, including
/// the cache counters that dominate a Claude Code child's prompt spend.
#[test]
fn diversion_line_carries_the_count_and_the_child_usage() {
    let line = diversion_line(3, 2, &reply());
    assert!(line.starts_with(DIVERSION_LOG_MARKER), "{line}");
    for field in [
        "count=3",
        "files=2",
        "model=claude-haiku-4-5",
        "input_tokens=10",
        "output_tokens=412",
        "cache_read_tokens=9094",
        "cache_creation_tokens=21922",
        "cost_usd=0.019422",
    ] {
        assert!(line.contains(field), "missing {field} in: {line}");
    }
}

/// Why (#6887 acceptance criterion 6): "count per session" cannot come from a
/// process that exits after one diversion, so it has to be durable. The ledger
/// file is the count — one line per diversion — and two sessions must not share
/// one counter.
/// What: three diversions on one session count 1, 2, 3; a second session starts
/// at 1 again; the file holds one line per diversion.
#[test]
fn record_diversion_counts_per_session() {
    let tmp = tempfile::tempdir().unwrap();
    let reply = reply();

    for expected in 1..=3 {
        let count = record_diversion(tmp.path(), "sess-a", 1, &reply).expect("ledger write");
        assert_eq!(count, expected, "the count must advance per diversion");
    }
    assert_eq!(
        record_diversion(tmp.path(), "sess-b", 1, &reply).expect("ledger write"),
        1,
        "a different session must not inherit another's count"
    );

    let log = std::fs::read_to_string(tmp.path().join("divert").join("sess-a.log")).unwrap();
    assert_eq!(log.lines().count(), 3);
    assert!(log.lines().next().unwrap().contains("count=1"));
    assert!(log.lines().last().unwrap().contains("count=3"));

    // A session id carrying a path separator must not escape the ledger dir.
    record_diversion(tmp.path(), "../escape", 1, &reply).expect("ledger write");
    assert!(tmp.path().join("divert").join(".._escape.log").exists());
}
