//! The tolerant tail read of an instance's ask turn log (#11747).
//!
//! Why: `ta`'s own `TurnLog::read` fails on the first bad line, which is right
//! for the engine that replays the log and wrong for a page that only shows
//! history: one torn line would hide every turn. The console skips a bad line
//! instead, and says how many it skipped, so the skip is never silent.
//! What: [`read_tail`] over `<home>/stores/ask-turns.jsonl`, keeping the last
//! `limit` good records, oldest first (file order).
//! Test: `tests::a_bad_line_is_counted_not_fatal`, `tests::the_tail_is_the_newest_records`,
//! and the turns cases in `tests/assistants_routes.rs`.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The `stores/` directory inside an instance home (`assistants/home.rs`).
pub const STORES_DIR: &str = "stores";

/// The turn log's file name (`ask/turn_log.rs` `TURN_LOG_FILE`).
/// Test: `tests::the_mirrored_constants_match_the_source`.
pub const TURN_LOG_FILE: &str = "ask-turns.jsonl";

/// The record version `ta` writes (`ask/turn_log.rs` `TURN_RECORD_VERSION`).
/// Test: `tests::the_mirrored_constants_match_the_source`.
pub const TURN_RECORD_VERSION: u32 = 1;

/// One line of the log, as `ta` writes it (`ask/turn_log.rs` `TurnRecord`).
/// Test: `tests::the_turn_record_fields_match_the_source`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnRecord {
    v: u32,
    at: String,
    question: String,
    answer: String,
}

/// One turn as the console serves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Turn {
    /// RFC 3339 time the answer was recorded.
    pub at: String,
    /// The user's question.
    pub question: String,
    /// The assistant's answer.
    pub answer: String,
}

/// The last turns of one log, with the counts that make a skip visible.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnTail {
    /// At most `limit` records, oldest first.
    pub turns: Vec<Turn>,
    /// Good records in the whole log.
    pub total: usize,
    /// Lines that were not a good version-1 record.
    pub skipped: usize,
}

/// `<home>/stores/ask-turns.jsonl`.
pub fn turn_log_path(home: &Path) -> PathBuf {
    home.join(STORES_DIR).join(TURN_LOG_FILE)
}

/// The last `limit` good records of the log at `path`.
///
/// What: a missing file is an empty tail. Each `\n`-separated line must parse
/// as a version-1 record with exactly `v`, `at`, `question` and `answer`;
/// anything else — blank, torn, not UTF-8, not JSON, another version — adds
/// one to `skipped`. The empty segment after the final newline is not a line.
/// The read takes no lock, so a line `ta` is appending at that moment can be
/// counted once as skipped.
///
/// # Errors
///
/// An I/O failure other than the file's absence.
/// Test: `tests::a_bad_line_is_counted_not_fatal`, `tests::the_tail_is_the_newest_records`.
pub fn read_tail(path: &Path, limit: usize) -> std::io::Result<TurnTail> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(TurnTail::default()),
        Err(e) => return Err(e),
    };
    let mut tail = TurnTail::default();
    if raw.is_empty() {
        return Ok(tail);
    }
    let body = raw.strip_suffix(b"\n").unwrap_or(&raw);
    let mut kept: VecDeque<Turn> = VecDeque::with_capacity(limit.min(1024));
    for line in body.split(|b| *b == b'\n') {
        let record = std::str::from_utf8(line)
            .ok()
            .and_then(|text| serde_json::from_str::<TurnRecord>(text).ok())
            .filter(|r| r.v == TURN_RECORD_VERSION);
        let Some(record) = record else {
            tail.skipped += 1;
            continue;
        };
        tail.total += 1;
        if limit == 0 {
            continue;
        }
        if kept.len() == limit {
            kept.pop_front();
        }
        kept.push_back(Turn {
            at: record.at,
            question: record.question,
            answer: record.answer,
        });
    }
    tail.turns = kept.into();
    Ok(tail)
}
