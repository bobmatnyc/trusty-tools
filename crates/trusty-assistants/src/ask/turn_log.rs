//! The per-instance ask turn log (#11650).
//!
//! Why: trusty-code's `Transcript` is not serialisable, so context across
//! asks lives in a log this crate owns, replayed into a fresh transcript.
//! What: [`TurnLog`] over `<home>/stores/ask-turns.jsonl`, one [`TurnRecord`]
//! per line. [`TurnLog::read`] fails on the first bad line, naming it;
//! [`TurnLog::load`] does the same except that it moves a corrupt LAST line to
//! a quarantine file and truncates it off (#11650);
//! [`TurnLog::append`] adds one line. The engine loads with `load` and
//! appends only after an answer exists.
//! Test: `super::tests::turn_log_tests` — the whole module.

use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::AskError;
use crate::assistants::AssistantHome;

/// File name of the turn log inside the home's `stores/` directory.
pub const TURN_LOG_FILE: &str = "ask-turns.jsonl";

/// Record format version written by this build.
pub const TURN_RECORD_VERSION: u32 = 1;

/// One answered question.
/// Test: `super::tests::turn_log_tests::appended_turns_read_back_in_order`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnRecord {
    /// Record format version; only [`TURN_RECORD_VERSION`] is accepted.
    pub v: u32,
    /// RFC 3339 time the answer was recorded.
    pub at: String,
    /// The user's question.
    pub question: String,
    /// The assistant's final answer.
    pub answer: String,
}

impl TurnRecord {
    /// A record stamped with the current time.
    pub fn now(question: &str, answer: &str) -> Self {
        Self {
            v: TURN_RECORD_VERSION,
            at: chrono::Utc::now().to_rfc3339(),
            question: question.to_string(),
            answer: answer.to_string(),
        }
    }
}

/// One instance's turn log.
/// Test: `super::tests::turn_log_tests` — the whole module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnLog {
    path: PathBuf,
}

impl TurnLog {
    /// The turn log of `home`. Touches no filesystem.
    pub fn for_home(home: &AssistantHome) -> Self {
        Self {
            path: home.stores_dir().join(TURN_LOG_FILE),
        }
    }

    /// The log file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every recorded turn, oldest first; empty when the file is absent.
    ///
    /// What: strict. Each line must parse as a version-1 [`TurnRecord`]. A
    /// blank line, a partial line, a non-UTF-8 line or an unknown version is
    /// [`AskError::CorruptTurnLog`] with its 1-based line number; nothing after
    /// it is dropped silently. Never changes the file; the engine uses
    /// [`TurnLog::load`].
    /// Test: `super::tests::turn_log_tests::a_corrupt_line_is_a_typed_error`,
    /// `super::tests::turn_log_tests::an_absent_log_is_empty`.
    pub fn read(&self) -> Result<Vec<TurnRecord>, AskError> {
        let raw = match std::fs::read(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(self.io(source)),
        };
        match parse(&raw) {
            (turns, None) => Ok(turns),
            (_, Some(bad)) => Err(self.corrupt(bad)),
        }
    }

    /// Every recorded turn, after moving a corrupt LAST line out of the way.
    ///
    /// Why: #11650 — a crash mid-append leaves a partial last line, and a
    /// strict read then refused every later ask with no way out.
    /// What: takes an exclusive lock on the log, then parses it. When only the
    /// final line is bad (unterminated, blank, not UTF-8 or not JSON), its
    /// bytes are written to a new `ask-turns.jsonl.corrupt-<time>` file and
    /// synced, then the log is truncated to the last good line and synced,
    /// and a warning names the line and the quarantine file. The returned
    /// [`Recovery`] says the same. A bad line before the last one, or a record
    /// of an unknown version anywhere, is still [`AskError::CorruptTurnLog`]
    /// and the file is not touched. A failed quarantine write is an error and
    /// the log is not truncated.
    /// Test: `super::tests::turn_log_tests::a_corrupt_last_line_is_quarantined`,
    /// `super::tests::turn_log_tests::mid_file_corruption_is_not_recovered`,
    /// `super::tests::engine_tests::an_ask_recovers_from_a_corrupt_last_line`.
    pub fn load(&self) -> Result<LoadedLog, AskError> {
        let mut file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(LoadedLog::default()),
            Err(source) => return Err(self.io(source)),
        };
        file.lock().map_err(|e| self.io(e))?;
        let mut raw = Vec::new();
        file.read_to_end(&mut raw).map_err(|e| self.io(e))?;
        let (turns, bad) = parse(&raw);
        let Some(bad) = bad else {
            return Ok(LoadedLog {
                turns,
                recovered: None,
            });
        };
        if !bad.last || bad.version {
            return Err(self.corrupt(bad));
        }
        let quarantine = self.quarantine_path();
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&quarantine)
            .map_err(|e| self.io(e))?;
        out.write_all(&raw[bad.offset..])
            .and_then(|()| out.sync_all())
            .map_err(|e| self.io(e))?;
        file.set_len(bad.offset as u64)
            .and_then(|()| file.sync_all())
            .map_err(|e| self.io(e))?;
        tracing::warn!(
            path = %self.path.display(),
            line = bad.line,
            reason = %bad.reason,
            quarantine = %quarantine.display(),
            "ask: the turn log's last line was corrupt; moved it aside and truncated the log",
        );
        Ok(LoadedLog {
            turns,
            recovered: Some(Recovery {
                line: bad.line,
                reason: bad.reason,
                quarantine,
                bytes: raw.len() - bad.offset,
            }),
        })
    }

    /// `<log>.corrupt-<UTC time>`, a new file beside the log.
    fn quarantine_path(&self) -> PathBuf {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".corrupt-{stamp}"));
        PathBuf::from(name)
    }

    fn io(&self, source: std::io::Error) -> AskError {
        AskError::TurnLogIo {
            path: self.path.clone(),
            source,
        }
    }

    fn corrupt(&self, bad: BadLine) -> AskError {
        AskError::CorruptTurnLog {
            path: self.path.clone(),
            line: bad.line,
            reason: bad.reason,
        }
    }

    /// Append `record` as one line, creating `stores/` and the file if needed.
    ///
    /// What: the line is written with a single `write_all` on an append-only
    /// handle under an exclusive lock, then synced. Earlier lines are never touched.
    /// Test: `super::tests::turn_log_tests::appended_turns_read_back_in_order`.
    pub fn append(&self, record: &TurnRecord) -> Result<(), AskError> {
        let io = |source| AskError::TurnLogIo {
            path: self.path.clone(),
            source,
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let mut line = serde_json::to_string(record)
            .map_err(|e| io(std::io::Error::new(ErrorKind::InvalidData, e)))?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(io)?;
        // #11650: the same lock `load` takes, so a recovery never truncates a
        // line another ask is writing.
        file.lock().map_err(io)?;
        file.write_all(line.as_bytes()).map_err(io)?;
        file.sync_data().map_err(io)
    }
}

/// What [`TurnLog::load`] read, and any recovery it made.
#[derive(Debug, Default)]
pub struct LoadedLog {
    /// Every good turn, oldest first.
    pub turns: Vec<TurnRecord>,
    /// Set when a corrupt last line was moved aside.
    pub recovered: Option<Recovery>,
}

/// A corrupt last line that [`TurnLog::load`] moved aside.
/// Test: `super::tests::turn_log_tests::a_corrupt_last_line_is_quarantined`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovery {
    /// 1-based line number of the corrupt line.
    pub line: usize,
    /// Why the line did not parse.
    pub reason: String,
    /// The file now holding the line's bytes.
    pub quarantine: PathBuf,
    /// How many bytes were moved.
    pub bytes: usize,
}

/// The first line that is not a good record.
struct BadLine {
    /// 1-based line number.
    line: usize,
    /// Byte offset where the line starts; everything before it is good.
    offset: usize,
    reason: String,
    /// The line is the file's last one.
    last: bool,
    /// The line parsed but names an unknown record version.
    version: bool,
}

/// Parse `raw` line by line, stopping at the first bad line.
fn parse(raw: &[u8]) -> (Vec<TurnRecord>, Option<BadLine>) {
    let mut turns = Vec::new();
    let mut offset = 0;
    while offset < raw.len() {
        let line = turns.len() + 1;
        let bad = |reason: String, last: bool, version: bool| BadLine {
            line,
            offset,
            reason,
            last,
            version,
        };
        let Some(len) = raw[offset..].iter().position(|b| *b == b'\n') else {
            return (
                turns,
                Some(bad("the last line is unterminated".into(), true, false)),
            );
        };
        let next = offset + len + 1;
        let last = next == raw.len();
        let text = match std::str::from_utf8(&raw[offset..offset + len]) {
            Ok(text) => text,
            Err(e) => return (turns, Some(bad(e.to_string(), last, false))),
        };
        match serde_json::from_str::<TurnRecord>(text) {
            Ok(record) if record.v == TURN_RECORD_VERSION => turns.push(record),
            Ok(record) => {
                let reason = format!("unknown record version {}", record.v);
                return (turns, Some(bad(reason, last, true)));
            }
            Err(e) => return (turns, Some(bad(e.to_string(), last, false))),
        }
        offset = next;
    }
    (turns, None)
}

/// The newest turns that fit the replay limits, oldest first.
///
/// Why: #11650 — every ask used to replay the whole log, so requests grew
/// with the log until the provider refused them on context length.
/// What: walks back from the newest turn and keeps turns while both limits
/// hold: at most `max_turns` turns and at most `max_chars` characters of
/// question plus answer text. It stops at the first turn that does not fit,
/// so the window is always a contiguous suffix of the log; older turns stay
/// in the file and are only left out of this request.
/// Test: `super::tests::turn_log_tests::the_replay_window_is_the_newest_suffix`,
/// `super::tests::engine_tests::replay_is_bounded_by_the_turn_and_char_limits`.
pub fn replay_window(turns: &[TurnRecord], max_turns: usize, max_chars: usize) -> &[TurnRecord] {
    let mut chars = 0usize;
    let mut kept = 0usize;
    for turn in turns.iter().rev() {
        if kept == max_turns {
            break;
        }
        let size = turn.question.chars().count() + turn.answer.chars().count();
        match chars.checked_add(size) {
            Some(total) if total <= max_chars => chars = total,
            _ => break,
        }
        kept += 1;
    }
    &turns[turns.len() - kept..]
}
