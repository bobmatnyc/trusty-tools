//! Replay of `questions.jsonl`: the record format and the one state
//! transition (#8433 P2).
//!
//! Why: the live table and the replay must apply the same rules, so a log a
//! correct table never writes fails the open (#8433 r6 G5).
//! What: [`Record`] and its line wrappers, [`Book`] (the replayed questions)
//! with [`Book::apply`], [`decode`], [`validate`] and the served-channel serde.
//! Test: `question_table_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use trusty_channels::policy::{Channel, QuestionId};

use super::{Answer, Ask, FORMAT_VERSION, Question, QuestionTableError};

/// The channels the daemon serves (gchat-mcp serves gchat); same set as the
/// policy snapshot's.
pub(super) const SERVED: [Channel; 2] = [Channel::Slack, Channel::Telegram];

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum Record {
    Reserve { id: QuestionId, ts: String },
    Open { question: Question },
    Resolve { id: QuestionId, answer: Answer },
}

/// One log line as written.
#[derive(Serialize)]
pub(super) struct LineOut<'a> {
    pub(super) v: u32,
    #[serde(flatten)]
    pub(super) record: &'a Record,
}

/// One log line as read.
#[derive(Deserialize)]
struct LineIn {
    v: u32,
    #[serde(flatten)]
    record: Record,
}

/// The replayed questions; [`Book::apply`] is the one state transition.
#[derive(Debug)]
pub(super) struct Book {
    pub(super) questions: BTreeMap<QuestionId, Entry>,
    pub(super) reserved: BTreeSet<QuestionId>,
    pub(super) next_id: QuestionId,
}

#[derive(Debug)]
pub(super) struct Entry {
    pub(super) question: Question,
    pub(super) answer: Option<Answer>,
}

impl Book {
    pub(super) fn new() -> Self {
        Self {
            questions: BTreeMap::new(),
            reserved: BTreeSet::new(),
            next_id: 1,
        }
    }

    /// Apply one record, or say which replay rule it breaks.
    ///
    /// Why: #8433 r6 G5: a log a correct table never writes (a reused id, an
    /// open never reserved, a resolve for an unknown or answered question)
    /// is tampering or a bug; it fails the open rather than being skipped.
    /// What: the same checks run live and on replay.
    /// Test: `resolve_for_unknown_id_in_log_fails_the_open`,
    /// `unknown_channel_or_gchat_row_fails_the_open`.
    pub(super) fn apply(&mut self, record: Record) -> Result<(), &'static str> {
        match record {
            Record::Reserve { id, .. } => {
                if id < self.next_id {
                    return Err("reserve reuses an id");
                }
                self.next_id = id.checked_add(1).ok_or("question id overflow")?;
                self.reserved.insert(id);
            }
            Record::Open { question } => {
                validate(&question.ask)?;
                if !self.reserved.remove(&question.id) {
                    return Err("open for an id not reserved");
                }
                let entry = Entry {
                    question,
                    answer: None,
                };
                self.questions.insert(entry.question.id, entry);
            }
            Record::Resolve { id, answer } => match self.questions.get_mut(&id) {
                None => return Err("resolve for an unknown question"),
                Some(entry) if entry.answer.is_some() => {
                    return Err("second resolve for one question");
                }
                Some(entry) => entry.answer = Some(answer),
            },
        }
        Ok(())
    }
}

/// Apply a decoded line on replay, mapping either failure to `Corrupt`.
pub(super) fn replay_line(
    book: &mut Book,
    decoded: Result<Record, &'static str>,
    path: &Path,
    line: usize,
) -> Result<(), QuestionTableError> {
    decoded
        .and_then(|record| book.apply(record))
        .map_err(|reason| QuestionTableError::Corrupt {
            path: path.to_path_buf(),
            line,
            reason,
        })
}

/// A question the daemon may hold: a served channel, no empty field, and a
/// managed session id P5 can build a taint path from.
pub(super) fn validate(ask: &Ask) -> Result<(), &'static str> {
    if !SERVED.contains(&ask.channel) {
        return Err("channel not served by the daemon");
    }
    let fields = [
        &ask.route,
        &ask.recipient,
        &ask.conversation.dm_channel_id,
        &ask.conversation.thread_ts,
        &ask.message_ts,
    ];
    if fields.iter().any(|f| f.is_empty()) {
        return Err("a question field is empty");
    }
    crate::core::channels_taint::sanitize_id(&ask.asker_managed_session)
        .map_err(|_| "asker managed session id is not a plain id")?;
    Ok(())
}

/// Decode one log line; the error is a content-free class.
pub(super) fn decode(line: &[u8]) -> Result<Record, &'static str> {
    let text = std::str::from_utf8(line).map_err(|_| "invalid UTF-8")?;
    let line: LineIn = serde_json::from_str(text).map_err(|e| match e.classify() {
        serde_json::error::Category::Io => "io error",
        serde_json::error::Category::Syntax => "invalid JSON",
        serde_json::error::Category::Data => "unexpected JSON shape",
        serde_json::error::Category::Eof => "truncated JSON",
    })?;
    if line.v != FORMAT_VERSION {
        return Err("unknown record version");
    }
    Ok(line.record)
}

/// Serde for [`Ask::channel`]: the schema spelling of a served channel; any
/// other name, `gchat` included, fails the decode (#8433 r6 G5).
pub(super) mod served_channel {
    use serde::{Deserialize as _, Deserializer, Serializer};
    use trusty_channels::policy::Channel;

    pub(crate) fn serialize<S: Serializer>(channel: &Channel, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(channel.as_str())
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Channel, D::Error> {
        let name = String::deserialize(d)?;
        super::SERVED
            .into_iter()
            .find(|c| c.as_str() == name)
            .ok_or_else(|| serde::de::Error::custom("channel not served by the daemon"))
    }
}
