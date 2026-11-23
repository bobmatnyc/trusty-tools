//! The per-instance ask turn log (#11650).
//!
//! Why: trusty-code's `Transcript` is not serialisable, so context across
//! asks lives in a log this crate owns, replayed into a fresh transcript.
//! What: [`TurnLog`] over `<home>/stores/ask-turns.jsonl`, one [`TurnRecord`]
//! per line. [`TurnLog::read`] fails on the first bad line, naming it;
//! [`TurnLog::append`] adds one line and never rewrites the file. The engine
//! appends only after an answer exists.
//! Test: `super::tests::turn_log_tests` — the whole module.

use std::io::{ErrorKind, Write};
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
    /// What: each line must parse as a version-1 [`TurnRecord`]. A blank line,
    /// a partial line or an unknown version is [`AskError::CorruptTurnLog`]
    /// with its 1-based line number; nothing after it is dropped silently.
    /// Test: `super::tests::turn_log_tests::a_corrupt_line_is_a_typed_error`,
    /// `super::tests::turn_log_tests::an_absent_log_is_empty`.
    pub fn read(&self) -> Result<Vec<TurnRecord>, AskError> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(AskError::TurnLogIo {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let corrupt = |line: usize, reason: String| AskError::CorruptTurnLog {
            path: self.path.clone(),
            line,
            reason,
        };
        if !raw.is_empty() && !raw.ends_with('\n') {
            return Err(corrupt(
                raw.lines().count(),
                "the last line is unterminated".into(),
            ));
        }
        raw.lines()
            .enumerate()
            .map(|(i, line)| {
                let record: TurnRecord =
                    serde_json::from_str(line).map_err(|e| corrupt(i + 1, e.to_string()))?;
                if record.v != TURN_RECORD_VERSION {
                    return Err(corrupt(
                        i + 1,
                        format!("unknown record version {}", record.v),
                    ));
                }
                Ok(record)
            })
            .collect()
    }

    /// Append `record` as one line, creating `stores/` and the file if needed.
    ///
    /// What: the line is written with a single `write_all` on an append-only
    /// handle, then synced. Earlier lines are never touched.
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
        file.write_all(line.as_bytes()).map_err(io)?;
        file.sync_data().map_err(io)
    }
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
