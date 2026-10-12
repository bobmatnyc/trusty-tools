//! The daemon's durable question table, `channels/questions.jsonl` (#8433 P2).
//!
//! RED STAGE: the interface and its tests only; every operation is
//! unimplemented. The implementation lands in the next commit.
//! Test: `question_table_tests.rs`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trusty_channels::policy::{Channel, QuestionId, QuestionState};

/// The question table under `home`.
pub fn questions_path(home: &Path) -> PathBuf {
    trusty_common::crate_config::crate_config_dir_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    )
    .join("channels")
    .join("questions.jsonl")
}

/// A question-table operation that could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum QuestionTableError {
    /// Another table holds this path's lock.
    #[error("{}: held by another question table", path.display())]
    Locked {
        /// The lock file.
        path: PathBuf,
    },
    /// A filesystem step failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The path the step acted on.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// A log line does not decode or breaks a replay rule.
    #[error("{} line {line}: {reason}", path.display())]
    Corrupt {
        /// The log.
        path: PathBuf,
        /// The 1-based line.
        line: usize,
        /// A content-free reason.
        reason: &'static str,
    },
    /// The table is poisoned.
    #[error("question table is poisoned; reopen it")]
    Poisoned,
    /// The id was never reserved, or is already open.
    #[error("question {0} is not reserved and unopened")]
    NotReserved(QuestionId),
    /// The question is not one the daemon may hold.
    #[error("invalid question: {0}")]
    Invalid(&'static str),
    /// No home directory.
    #[error("no home directory to hold the question table")]
    NoHome,
}

/// Where a question was asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// The DM channel id.
    pub dm_channel_id: String,
    /// The thread the question was posted as.
    pub thread_ts: String,
}

/// What the asker supplies to open a question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The channel.
    pub channel: Channel,
    /// The route.
    pub route: String,
    /// The recipient.
    pub recipient: String,
    /// The asker's managed session id.
    pub asker_managed_session: String,
    /// Where it was posted.
    pub conversation: Conversation,
    /// The posted message's timestamp.
    pub message_ts: String,
}

/// One open or answered question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// The id.
    pub id: QuestionId,
    /// When it was opened.
    pub opened_at: String,
    /// What was asked.
    pub ask: Ask,
}

/// The first answer recorded for a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// Who answered.
    pub answerer: String,
    /// When.
    pub answered_at: String,
    /// Hex SHA-256 of the text.
    pub text_sha256: String,
    /// Text length in bytes.
    pub text_len: usize,
}

/// What [`QuestionTable::resolve`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// Resolved.
    Resolved,
    /// Already resolved.
    AlreadyResolved {
        /// The answer that won.
        first: Answer,
    },
    /// Wrong answerer or conversation.
    NotRecipient,
    /// No such question.
    Unknown,
    /// Poisoned.
    Poisoned,
}

struct State {
    #[allow(dead_code)]
    file: Option<File>,
    #[cfg(test)]
    fault: Option<Fault>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    BeforeWrite,
    AfterWrite,
}

/// The daemon's question table over one `questions.jsonl`.
pub struct QuestionTable {
    state: Mutex<State>,
}

impl std::fmt::Debug for QuestionTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionTable").finish_non_exhaustive()
    }
}

impl QuestionTable {
    /// Take `path`'s lock and replay it.
    pub fn open_at(_path: &Path) -> Result<Self, QuestionTableError> {
        todo!("#8433 P2 red stage")
    }

    /// Reserve the next question id.
    pub fn reserve(&self) -> Result<QuestionId, QuestionTableError> {
        todo!("#8433 P2 red stage")
    }

    /// Open reserved question `id`.
    pub fn open(&self, _id: QuestionId, _ask: Ask) -> Result<Question, QuestionTableError> {
        todo!("#8433 P2 red stage")
    }

    /// Resolve question `id`, once.
    pub fn resolve(
        &self,
        _id: QuestionId,
        _answerer: &str,
        _conversation: &Conversation,
        _answer: &str,
    ) -> ResolveOutcome {
        todo!("#8433 P2 red stage")
    }

    /// The policy's view of question `id`.
    pub fn state(&self, _id: QuestionId) -> Option<QuestionState> {
        todo!("#8433 P2 red stage")
    }

    /// Question `id` as opened.
    pub fn get(&self, _id: QuestionId) -> Option<Question> {
        todo!("#8433 P2 red stage")
    }

    /// True once poisoned.
    pub fn is_poisoned(&self) -> bool {
        todo!("#8433 P2 red stage")
    }

    /// The byte length of a quarantined torn line.
    pub fn quarantined_bytes(&self) -> Option<usize> {
        todo!("#8433 P2 red stage")
    }
}

/// Lowercase hex SHA-256 of `text`.
fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "question_table_tests.rs"]
mod tests;
