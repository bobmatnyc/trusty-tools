//! The tolerant, bounded tail read of an instance's ask turn log (#11747).
//!
//! Why: `ta`'s own `TurnLog::read` fails on the first bad line, which is right
//! for the engine that replays the log and wrong for a page that only shows
//! history: one torn line would hide every turn. The console skips a bad line
//! instead, and says how many it skipped, so the skip is never silent. The log
//! is append-only and the route is reachable over the tailnet, so a request
//! reads at most [`TAIL_WINDOW_BYTES`] from the end, never the whole file.
//! What: [`read_tail`] over `<home>/stores/ask-turns.jsonl`, keeping the last
//! `limit` good records of the window, oldest first (file order).
//! Test: `tests::a_bad_line_is_counted_not_fatal`, `tests::the_tail_is_the_newest_records`,
//! `tests::the_window_drops_its_first_partial_line`, and the turns cases in
//! `tests/assistants_routes.rs`.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
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

/// Most bytes one request reads from the end of a turn log: 4 MiB.
///
/// Why: #11747 review — reading the whole append-only log per request grows
/// memory and I/O with the log. At [`super::routes::MAX_TURN_LIMIT`] turns this
/// leaves about 20 KiB per turn.
/// Test: `turns_reads_only_the_tail_window` (`tests/assistants_routes.rs`).
pub const TAIL_WINDOW_BYTES: u64 = 4 * 1024 * 1024;

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
    /// Good records inside the window read.
    pub total: usize,
    /// Bad lines inside the window read. The partial line a truncated window
    /// starts in is dropped and not counted.
    pub skipped: usize,
    /// The file was longer than the window, so older lines were not read.
    pub truncated: bool,
}

/// `<home>/stores/ask-turns.jsonl`.
pub fn turn_log_path(home: &Path) -> PathBuf {
    home.join(STORES_DIR).join(TURN_LOG_FILE)
}

/// The last `limit` good records within the last [`TAIL_WINDOW_BYTES`].
///
/// What: see [`read_tail_window`].
///
/// # Errors
///
/// See [`read_tail_window`].
/// Test: `tests::the_tail_is_the_newest_records`, `turns_reads_only_the_tail_window`
/// (`tests/assistants_routes.rs`).
pub fn read_tail(path: &Path, limit: usize) -> std::io::Result<TurnTail> {
    read_tail_window(path, limit, TAIL_WINDOW_BYTES)
}

/// The last `limit` good records within the last `window` bytes of `path`.
///
/// What: a missing file is an empty tail. The file is opened without blocking
/// (`O_NONBLOCK` on Unix, so a FIFO cannot hang the open) and the OPENED handle
/// must be a regular file; anything else — a directory, FIFO or device, also
/// through a symlink — is an `InvalidData` error and nothing is read. Then at
/// most `window` bytes are read from the end; when that cuts the file, the
/// first, partial line is dropped uncounted and `truncated` is set. Each
/// `\n`-separated line must parse as a version-1 record with exactly `v`,
/// `at`, `question` and `answer`; anything else adds one to `skipped`. The
/// read takes no lock, so a line `ta` is appending at that moment can be
/// counted once as skipped. Memory is bounded by `window + 1` bytes.
///
/// # Errors
///
/// An I/O failure other than the file's absence, or a non-regular file.
/// Test: `tests::a_bad_line_is_counted_not_fatal`,
/// `tests::the_window_drops_its_first_partial_line`.
pub fn read_tail_window(path: &Path, limit: usize, window: u64) -> std::io::Result<TurnTail> {
    let mut file = match open_nonblocking(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(TurnTail::default()),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        // #11747: a FIFO or device would block or never end; refuse it.
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "the turn log is not a regular file",
        ));
    }
    let start = meta.len().saturating_sub(window);
    let truncated = start > 0;
    // Read from one byte early, so a window that starts exactly on a line
    // boundary keeps that line: the extra byte is then the `\n` dropped below.
    let from = start.saturating_sub(1);
    file.seek(SeekFrom::Start(from))?;
    let mut raw = Vec::new();
    file.take(window + (start - from)).read_to_end(&mut raw)?;
    let body: &[u8] = if truncated {
        match raw.iter().position(|b| *b == b'\n') {
            Some(i) => &raw[i + 1..],
            None => &[],
        }
    } else {
        &raw
    };
    let mut tail = parse_lines(body, limit);
    tail.truncated = truncated;
    Ok(tail)
}

/// Open `path` for reading without blocking on a FIFO.
fn open_nonblocking(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

/// Parse whole lines; the empty segment after a final newline is not a line.
fn parse_lines(raw: &[u8], limit: usize) -> TurnTail {
    let mut tail = TurnTail::default();
    if raw.is_empty() {
        return tail;
    }
    let body = raw.strip_suffix(b"\n").unwrap_or(raw);
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
    tail
}
