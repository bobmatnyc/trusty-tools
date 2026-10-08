//! `questions.jsonl`: the question ledger.
//!
//! Why: a reply must resolve exactly the question it answers, and the ledger
//! must survive a restart of the bot (#9448 rulings 4 and 6).
//! What: an append-only log of `reserve`, `open` and `resolve` records,
//! replayed into memory by [`Ledger::open`]. An id is reserved on disk before
//! the Chat call, so an id whose send outcome is unknown is never reused.
//! A question resolves once; a later reply leaves the first answer standing.
//! A torn final line (no trailing newline, from a crash mid-append) is moved
//! to `questions.jsonl.torn` and cut from the ledger; any other bad line
//! fails the open.
//! Test: `ledger_survives_restart`, `reply_resolves_exactly_that_question`,
//! `torn_final_ledger_line_is_quarantined_and_the_channel_opens`,
//! `malformed_ledger_line_before_the_last_still_fails_the_open`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gchat::error::StateError;
use crate::gchat::state::{append_bytes, append_line, json_error_class, now_rfc3339};

/// The answer recorded for a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The reply text.
    pub text: String,
    /// The sender's email.
    pub sender: String,
    /// The reply's Chat message name.
    pub message_name: String,
    /// When it was recorded (RFC 3339).
    pub answered_at: String,
}

/// One posted question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// Question id; `[Q-<id>]` in the posted text.
    pub id: u64,
    /// The route it was sent on.
    pub route: String,
    /// The recipient it was sent to.
    pub recipient: String,
    /// The DM space it was posted in.
    pub space: String,
    /// The thread key it was posted with.
    pub thread_key: String,
    /// The thread name `create_message` returned, if any.
    pub thread_name: Option<String>,
    /// The posted message's name.
    pub message_name: String,
    /// When it was posted (RFC 3339).
    pub opened_at: String,
    /// The answer, once a reply resolved it.
    #[serde(default)]
    pub answer: Option<Answer>,
}

impl Question {
    /// True until a reply resolves it.
    pub fn is_open(&self) -> bool {
        self.answer.is_none()
    }
}

/// What [`Ledger::resolve`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The question was open and now holds the answer.
    Resolved,
    /// The question already had an answer; nothing changed.
    AlreadyResolved,
    /// No question has this id.
    Unknown,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Record {
    Reserve { id: u64, ts: String },
    Open(Question),
    Resolve { id: u64, answer: Answer },
}

/// The replayed ledger.
#[derive(Debug)]
pub struct Ledger {
    path: PathBuf,
    questions: BTreeMap<u64, Question>,
    next_id: u64,
    quarantined: Option<usize>,
}

impl Ledger {
    /// Replay `path`, or start empty when it does not exist.
    ///
    /// Why: a restarted bot must see the questions still open, and one
    /// crash mid-append must not stop every later open (#9448 review).
    /// What: every newline-terminated line must decode, or the open fails
    /// naming the line, rather than silently forgetting questions. An
    /// unterminated final line that decodes is kept and given its newline;
    /// one that does not is appended to `questions.jsonl.torn` and cut from
    /// the ledger, and [`Ledger::quarantined_bytes`] reports its length. The
    /// caller must hold the state directory's lock.
    /// Test: `ledger_survives_restart`,
    /// `torn_final_ledger_line_is_quarantined_and_the_channel_opens`,
    /// `malformed_ledger_line_before_the_last_still_fails_the_open`.
    pub(crate) fn open(path: &Path) -> Result<Self, StateError> {
        let mut ledger = Self {
            path: path.to_path_buf(),
            questions: BTreeMap::new(),
            next_id: 1,
            quarantined: None,
        };
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ledger),
            Err(e) => return Err(StateError::io(path, &e)),
        };
        let body_len = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let (body, tail) = bytes.split_at(body_len);
        for (i, line) in body.split_inclusive(|b| *b == b'\n').enumerate() {
            if line.trim_ascii().is_empty() {
                continue;
            }
            let record = decode(line).map_err(|reason| StateError::Corrupt {
                path: path.to_path_buf(),
                line: i + 1,
                reason: reason.to_string(),
            })?;
            ledger.apply(record);
        }
        if !tail.trim_ascii().is_empty() {
            match decode(tail) {
                Ok(record) => {
                    ledger.apply(record);
                    append_bytes(path, b"\n")?;
                }
                Err(_) => {
                    let torn = path.with_extension("jsonl.torn");
                    append_bytes(&torn, &[tail, b"\n"].concat())?;
                    truncate(path, body_len)?;
                    ledger.quarantined = Some(tail.len());
                }
            }
        }
        Ok(ledger)
    }

    /// The byte length of a torn final line [`Ledger::open`] moved aside.
    pub fn quarantined_bytes(&self) -> Option<usize> {
        self.quarantined
    }

    fn apply(&mut self, record: Record) {
        match record {
            Record::Reserve { id, .. } => self.next_id = self.next_id.max(id.saturating_add(1)),
            Record::Open(q) => {
                self.next_id = self.next_id.max(q.id.saturating_add(1));
                self.questions.insert(q.id, q);
            }
            Record::Resolve { id, answer } => {
                if let Some(q) = self.questions.get_mut(&id).filter(|q| q.is_open()) {
                    q.answer = Some(answer);
                }
            }
        }
    }

    /// Reserve the next question id on disk.
    pub fn reserve(&mut self) -> Result<u64, StateError> {
        let id = self.next_id;
        append_line(
            &self.path,
            &Record::Reserve {
                id,
                ts: now_rfc3339(),
            },
        )?;
        self.next_id = id.saturating_add(1);
        Ok(id)
    }

    /// Record a posted question as open.
    pub fn record_open(&mut self, question: Question) -> Result<(), StateError> {
        let record = Record::Open(question);
        append_line(&self.path, &record)?;
        self.apply(record);
        Ok(())
    }

    /// Resolve question `id` with `answer`, once.
    pub fn resolve(&mut self, id: u64, answer: Answer) -> Result<ResolveOutcome, StateError> {
        match self.questions.get(&id) {
            None => return Ok(ResolveOutcome::Unknown),
            Some(q) if !q.is_open() => return Ok(ResolveOutcome::AlreadyResolved),
            Some(_) => {}
        }
        let record = Record::Resolve { id, answer };
        append_line(&self.path, &record)?;
        self.apply(record);
        Ok(ResolveOutcome::Resolved)
    }

    /// The question with this id.
    pub fn get(&self, id: u64) -> Option<&Question> {
        self.questions.get(&id)
    }

    /// Every question, oldest first.
    pub fn questions(&self) -> impl Iterator<Item = &Question> {
        self.questions.values()
    }

    /// Questions of `route` whose stored thread name is `thread_name`.
    pub fn by_thread<'a>(
        &'a self,
        route: &'a str,
        thread_name: &'a str,
    ) -> impl Iterator<Item = &'a Question> + 'a {
        self.questions
            .values()
            .filter(move |q| q.route == route && q.thread_name.as_deref() == Some(thread_name))
    }
}

/// Decode one ledger line; the error is a content-free class.
fn decode(line: &[u8]) -> Result<Record, &'static str> {
    let text = std::str::from_utf8(line).map_err(|_| "invalid UTF-8")?;
    serde_json::from_str(text).map_err(|e| json_error_class(&e))
}

fn truncate(path: &Path, len: usize) -> Result<(), StateError> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| StateError::io(path, &e))?;
    file.set_len(len as u64)
        .and_then(|()| file.sync_all())
        .map_err(|e| StateError::io(path, &e))
}

/// The question ids named by `[Q-<n>]` tokens in `text`, deduplicated, in
/// order of first appearance.
///
/// Why: in a DM, Chat may not thread a reply, so the visible token is the
/// fallback binding (ruling 6).
/// What: matches `[Q-` + ASCII digits + `]`, case-sensitive; a token whose
/// number overflows `u64` is ignored.
/// Test: `question_tokens_parse_only_well_formed_ids`.
pub fn question_tokens(text: &str) -> Vec<u64> {
    let mut ids = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[Q-") {
        rest = &rest[start + 3..];
        let digits: &str = &rest[..rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len())];
        if !digits.is_empty() && rest[digits.len()..].starts_with(']') {
            if let Ok(id) = digits.parse::<u64>() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

/// The visible token for question `id`.
pub fn question_token(id: u64) -> String {
    format!("[Q-{id}]")
}
