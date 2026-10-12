//! #9171 A3b-2, Architect ruling Q2: `read_events` holds its offset at the
//! last newline, so an event whose line is still being written is read whole
//! on the next cycle instead of being skipped as a fragment. A recorded
//! divergence from the prototype, which moves the offset past the partial
//! line and loses the event.

use std::fs::{self, OpenOptions};
use std::io::Write;

use trusty_architect::poller::read_events;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FIRST: &str =
    r#"{"type": "permission_prompt", "tmux_session": "tm-a", "tmux_pane": "%1", "message": "one"}"#;
const SECOND: &str =
    r#"{"type": "agent_needs_input", "tmux_session": "tm-b", "tmux_pane": "%2", "message": "two"}"#;

fn append(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// A whole line, then half of the next: the first read yields the whole
/// line and stops the offset after its newline; once the writer finishes
/// the second line, the next read yields it. Red against an offset that
/// moves to the end of the file.
#[test]
fn read_events_holds_offset_at_last_newline() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("events.jsonl");
    fs::write(&path, "")?;
    let mut offset = None;
    assert!(read_events(&path, &mut offset, 0.0)?.is_empty());
    assert_eq!(offset, Some(0));

    let (head, tail) = SECOND.split_at(SECOND.len() / 2);
    append(&path, &format!("{FIRST}\n{head}"))?;
    let got = read_events(&path, &mut offset, 1.0)?;
    let mut failures = Vec::new();
    if got.iter().map(|a| a.detail.as_str()).collect::<Vec<_>>() != ["one"] {
        failures.push(format!("first read: {got:?}"));
    }
    let held = (FIRST.len() + 1) as u64;
    if offset != Some(held) {
        failures.push(format!(
            "offset after a partial line: {offset:?}, want {held}"
        ));
    }

    append(&path, &format!("{tail}\n"))?;
    let got = read_events(&path, &mut offset, 2.0)?;
    if got.iter().map(|a| a.detail.as_str()).collect::<Vec<_>>() != ["two"] {
        failures.push(format!("the completed line is read whole: {got:?}"));
    }
    let end = fs::metadata(&path)?.len();
    if offset != Some(end) {
        failures.push(format!(
            "offset after the completed line: {offset:?}, want {end}"
        ));
    }
    assert!(failures.is_empty(), "{failures:#?}");
    Ok(())
}

/// The first run skips the backlog, but not a line still being written: the
/// offset stops after the last newline, and the finished line is read next.
#[test]
fn first_run_skips_the_backlog_but_holds_a_partial_last_line() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("events.jsonl");
    let (head, tail) = SECOND.split_at(10);
    fs::write(&path, format!("{FIRST}\n{head}"))?;
    let mut offset = None;
    let got = read_events(&path, &mut offset, 0.0)?;
    assert!(got.is_empty(), "the backlog is skipped: {got:?}");
    assert_eq!(offset, Some((FIRST.len() + 1) as u64));
    append(&path, &format!("{tail}\n"))?;
    let got = read_events(&path, &mut offset, 1.0)?;
    assert_eq!(
        got.iter().map(|a| a.kind.as_str()).collect::<Vec<_>>(),
        ["agent_needs_input"]
    );
    Ok(())
}

/// A chunk with no newline at all moves nothing.
#[test]
fn a_chunk_without_a_newline_leaves_the_offset() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("events.jsonl");
    fs::write(&path, format!("{FIRST}\n"))?;
    let mut offset = None;
    read_events(&path, &mut offset, 0.0)?;
    let start = offset;
    append(&path, &SECOND[..20])?;
    assert!(read_events(&path, &mut offset, 1.0)?.is_empty());
    assert_eq!(offset, start);
    Ok(())
}
