//! Typed errors for one ask (#11650).
//!
//! Why: the Fail-Open Check. Every way an ask can stop short is a variant, so
//! no caller can mistake a cancelled, capped or failed run for an answer, and
//! the turn log only advances on `Ok`.
//! What: [`AskError`]. The loop's partial output is dropped, never carried.
//! Test: `super::tests::engine_tests` — one test per reachable arm.

use std::path::PathBuf;

use thiserror::Error;
use trusty_code::agent_loop::AgentLoopError;

use crate::assistants::AssistantError;

/// A failure to answer one question for one instance.
///
/// Why/What/Test: see this module's doc comment.
#[derive(Debug, Error)]
pub enum AskError {
    /// The question was empty or whitespace only.
    #[error("the question is empty")]
    EmptyQuestion,

    /// `instructions.md` is missing, unreadable, too large, not UTF-8 or
    /// empty. The ask never runs without instructions.
    #[error("cannot read the instance's instructions: {0}")]
    Instructions(#[source] AssistantError),

    /// `config.toml` could not be read or parsed.
    #[error("cannot read the instance's config: {0}")]
    Config(#[source] AssistantError),

    /// An `[ask]` setting in `config.toml` is out of range.
    #[error("`[ask] {key}` in `{}` is invalid: {reason}", path.display())]
    InvalidSetting {
        path: PathBuf,
        key: &'static str,
        reason: String,
    },

    /// A turn-log line is not a valid turn record.
    #[error("turn log `{}` line {line} is corrupt: {reason}", path.display())]
    CorruptTurnLog {
        path: PathBuf,
        line: usize,
        reason: String,
    },

    /// The turn log could not be read or appended to.
    #[error("turn log `{}` could not be used: {source}", path.display())]
    TurnLogIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The production inference client could not be built.
    #[error("cannot build the inference client: {0}")]
    Client(String),

    /// The cancel flag was set; the loop stopped at a turn boundary.
    #[error("the ask was cancelled")]
    Cancelled,

    /// The loop used `max_turns` without a final answer.
    #[error("no answer within {max_turns} turns")]
    TurnCapExceeded { max_turns: u32 },

    /// The loop ran past `timeout_secs`.
    #[error("no answer within {timeout_secs}s")]
    Timeout { timeout_secs: u64 },

    /// The model finished with no answer text.
    #[error("the model returned an empty answer")]
    EmptyAnswer,

    /// Any other loop failure, including an inference error.
    #[error("the agent loop failed: {0}")]
    Loop(#[source] AgentLoopError),
}

impl From<AgentLoopError> for AskError {
    /// Map a loop failure to its variant, dropping any partial output.
    /// Test: `super::tests::engine_tests::cancel_stops_at_the_next_turn_boundary`.
    fn from(e: AgentLoopError) -> Self {
        match e {
            AgentLoopError::Cancelled { .. } => Self::Cancelled,
            AgentLoopError::TurnCapExceeded { max_turns, .. } => {
                Self::TurnCapExceeded { max_turns }
            }
            AgentLoopError::Timeout { timeout_secs, .. } => Self::Timeout { timeout_secs },
            other => Self::Loop(other),
        }
    }
}
