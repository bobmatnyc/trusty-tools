//! The resume token: an event's bus `seq` and its `EventId`, as one string.
//!
//! Why: since #9075 a resume needs both halves. The bus refuses
//! `since_seq` alone, because a crash can hand the same `seq` to a second
//! event, and the id is what tells the two apart. An SSE reader carries one
//! opaque `Last-Event-ID`, so DOC-73 §4.4's "maps exactly onto `since_seq`"
//! no longer holds: the id must ride in the same string (#9076).
//! What: [`ResumeToken`] renders as `<seq>:<uuid>` and parses back. A token
//! that does not parse is an error, never a silent "start live", so a
//! corrupted `Last-Event-ID` cannot drop a reader's backlog unseen.
//! Test: `super::tests::token_round_trips_through_its_string`,
//! `super::tests::token_rejects_malformed_strings`.

use std::fmt;
use std::str::FromStr;

use trusty_common::control_bus::{EventId, HarnessEvent};

/// Where a reader stands: the last event it holds.
///
/// Why: the two values `events.subscribe` needs to resume (`since_seq`,
/// `since_id`), kept together so neither travels without the other.
/// What: `seq` is the bus-assigned sequence number, `id` the producer-minted
/// id of the event at that `seq`. `Display` and `FromStr` use `<seq>:<uuid>`.
/// Test: `super::tests::token_round_trips_through_its_string`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResumeToken {
    /// The bus `seq` of the held event.
    pub seq: u64,
    /// The id of the held event.
    pub id: EventId,
}

impl ResumeToken {
    /// The token naming `event`.
    pub fn of(event: &HarnessEvent) -> Self {
        Self {
            seq: event.seq,
            id: event.id,
        }
    }
}

impl fmt::Display for ResumeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.seq, self.id)
    }
}

/// Why a string is not a [`ResumeToken`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResumeTokenError {
    /// No `:` between the seq and the id.
    #[error("resume token {0:?} is not <seq>:<uuid>")]
    Shape(String),
    /// The part before `:` is not an unsigned integer.
    #[error("resume token {0:?} has no numeric seq")]
    Seq(String),
    /// The part after `:` is not a UUID.
    #[error("resume token {0:?} has no valid event id")]
    Id(String),
}

impl FromStr for ResumeToken {
    type Err = ResumeTokenError;

    /// Parse `<seq>:<uuid>`.
    ///
    /// # Errors
    ///
    /// [`ResumeTokenError`] naming the first half that does not parse.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let (seq, id) = raw
            .trim()
            .split_once(':')
            .ok_or_else(|| ResumeTokenError::Shape(raw.to_string()))?;
        let seq = seq
            .parse::<u64>()
            .map_err(|_| ResumeTokenError::Seq(raw.to_string()))?;
        // `EventId` is a transparent UUID, so its own serde form is the parser.
        let id = serde_json::from_value(serde_json::Value::String(id.to_string()))
            .map_err(|_| ResumeTokenError::Id(raw.to_string()))?;
        Ok(Self { seq, id })
    }
}
