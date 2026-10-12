//! The daemon's durable question table, `channels/questions.jsonl` (#8433 P2).
//!
//! Why: an inbound Slack or Telegram answer may resolve exactly one question,
//! once, and only from the recipient and conversation it was asked in. The
//! record must survive a daemon restart. This is a Fail-Open Check site: no
//! failure here may let a question resolve twice or from the wrong party.
//! What: an append-only JSONL log of `reserve`, `open` and `resolve` records,
//! replayed into memory by [`QuestionTable::open_at`]. The shape follows the
//! crate-private gchat ledger in trusty-channels; the code is this module's
//! own. One `std::sync::Mutex` covers each operation's check, append and
//! apply. A lifetime `flock` on `questions.jsonl.lock` refuses a second table
//! on the same path. Any append error poisons the table until it is reopened,
//! and the replay then decides. Ids are the policy's `u64` [`QuestionId`];
//! P5 writes the decimal form into the taint record's `question_id` string.
//! Test: `question_table_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trusty_channels::policy::{Channel, QuestionId, QuestionState};

/// The record format version every line carries.
pub const FORMAT_VERSION: u32 = 1;

/// The channels the daemon serves (gchat-mcp serves gchat); same set as the
/// policy snapshot's.
const SERVED: [Channel; 2] = [Channel::Slack, Channel::Telegram];

/// The question table under `home`:
/// `~/.trusty-tools/trusty-mpm/channels/questions.jsonl`.
// See #9815: the trust anchor seals this file and its `.lock` (P2a, G7).
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
    /// Another table holds this path's lock (a second daemon).
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
        /// The 1-based line; 0 for a live operation.
        line: usize,
        /// A content-free reason.
        reason: &'static str,
    },
    /// An append failed or the lock was poisoned; reopen the table.
    #[error("question table is poisoned; reopen it")]
    Poisoned,
    /// The id was never reserved, or is already open.
    #[error("question {0} is not reserved and unopened")]
    NotReserved(QuestionId),
    /// The question is not one the daemon may hold.
    #[error("invalid question: {0}")]
    Invalid(&'static str),
    /// The daemon has no home to hold the table under.
    #[error("no home directory to hold the question table")]
    NoHome,
}

/// Where a question was asked: the DM channel id and the posted thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// The DM channel (Slack `D…`) or private chat id.
    pub dm_channel_id: String,
    /// The thread the question was posted as.
    pub thread_ts: String,
}

/// What the asker supplies to open a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    /// The channel it was sent on; Slack or Telegram only.
    #[serde(with = "served_channel")]
    pub channel: Channel,
    /// The route it was sent on.
    pub route: String,
    /// The recipient, in the channel's compared form.
    pub recipient: String,
    /// The asker's managed session id; P5 taints it before delivery.
    pub asker_managed_session: String,
    /// Where it was posted; an answer must come from here.
    pub conversation: Conversation,
    /// The posted message's timestamp.
    pub message_ts: String,
}

/// One open or answered question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// The id [`QuestionTable::reserve`] issued.
    pub id: QuestionId,
    /// When it was opened (RFC 3339).
    pub opened_at: String,
    /// What was asked, of whom and where.
    #[serde(flatten)]
    pub ask: Ask,
}

/// The first answer recorded for a question. Holds no answer text, only its
/// digest and length; P5 delivers the text from its own buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// Who answered.
    pub answerer: String,
    /// When it was recorded (RFC 3339, milliseconds).
    pub answered_at: String,
    /// Lowercase hex SHA-256 of the answer text.
    pub text_sha256: String,
    /// The answer text's length in bytes.
    pub text_len: usize,
}

impl Answer {
    /// The record of `answerer` answering `text` now.
    fn new(answerer: &str, text: &str) -> Self {
        Self {
            answerer: answerer.to_owned(),
            answered_at: now(),
            text_sha256: sha256_hex(text),
            text_len: text.len(),
        }
    }
}

/// What [`QuestionTable::resolve`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The question was open and now holds this answer, durably.
    Resolved,
    /// The question already had an answer; nothing changed.
    AlreadyResolved {
        /// The answer that won.
        first: Answer,
    },
    /// The answerer or conversation is not the question's; nothing changed.
    NotRecipient,
    /// No open or answered question has this id (a reserved-only id too).
    Unknown,
    /// The table is poisoned; nothing changed in memory. Reopen it.
    Poisoned,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Record {
    Reserve { id: QuestionId, ts: String },
    Open { question: Question },
    Resolve { id: QuestionId, answer: Answer },
}

/// One log line as written.
#[derive(Serialize)]
struct LineOut<'a> {
    v: u32,
    #[serde(flatten)]
    record: &'a Record,
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
struct Book {
    questions: BTreeMap<QuestionId, Entry>,
    reserved: BTreeSet<QuestionId>,
    next_id: QuestionId,
}

#[derive(Debug)]
struct Entry {
    question: Question,
    answer: Option<Answer>,
}

impl Book {
    fn new() -> Self {
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
    fn apply(&mut self, record: Record) -> Result<(), &'static str> {
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

/// The live table: the log handle, the replayed book and the poison flag.
struct State {
    file: File,
    book: Book,
    poisoned: bool,
    #[cfg(test)]
    fault: Option<Fault>,
}

/// Test seam: an append fails before or after its bytes reach the file.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    BeforeWrite,
    AfterWrite,
}

/// The daemon's question table over one `questions.jsonl`.
///
/// Why: P4 (`ask_owner`) reserves and opens; P5 (inbound) resolves. Both
/// need one compare-and-set that a restart, a crash or a second daemon
/// cannot break.
/// What: see the module doc. Every method takes the one mutex; a poisoned
/// mutex or a poisoned table refuses. `Sync`, so the daemon shares it in an
/// `Arc`; methods block on one fsync, so async callers use `spawn_blocking`.
/// Test: `question_table_tests.rs`.
pub struct QuestionTable {
    path: PathBuf,
    state: Mutex<State>,
    quarantined: Option<usize>,
    _lock: File,
}

impl std::fmt::Debug for QuestionTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionTable")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl QuestionTable {
    /// Take `path`'s lock and replay it, creating it when absent.
    ///
    /// Why: a restarted daemon must see the questions still open, a crash
    /// mid-append must not close the channel, and two daemons must never
    /// share one table (#8433 r6 G6).
    /// What: checks the #8545 home-write fence, creates the directory 0700,
    /// takes a lifetime `flock` on `<path>.lock` (held: [`QuestionTableError::Locked`]),
    /// then replays. Every newline-terminated line must decode and pass
    /// [`Book::apply`], or the open fails with `Corrupt`. An unterminated
    /// final line that decodes is kept and given its newline; one that does
    /// not is appended to `questions.jsonl.torn` before the log is cut. A new
    /// file is created 0600, and its directory fsynced; an existing one is
    /// set to 0600.
    /// Test: `table_survives_restart`, `torn_final_line_quarantined`,
    /// `final_line_missing_only_newline_is_kept`,
    /// `malformed_middle_line_fails_the_open`,
    /// `second_table_on_same_path_is_refused_while_first_lives`,
    /// `file_is_0600_and_dir_0700`, `home_write_fence_is_called`.
    pub fn open_at(path: &Path) -> Result<Self, QuestionTableError> {
        // #8433: a test must never write the operator's home (#8545); the
        // lock and torn files are siblings of `path`.
        crate::core::home_write_fence::check(path);
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(io_err(dir))?;
        let lock = take_lock(&path.with_extension("jsonl.lock"))?;
        let (bytes, created) = match std::fs::read(path) {
            Ok(bytes) => (bytes, false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), true),
            Err(e) => return Err(io_err(path)(e)),
        };
        let mut book = Book::new();
        let body_len = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let (body, tail) = bytes.split_at(body_len);
        let mut lines = 0;
        for line in body.split_inclusive(|b| *b == b'\n') {
            lines += 1;
            if !line.trim_ascii().is_empty() {
                replay(&mut book, decode(line), path, lines)?;
            }
        }
        let mut quarantined = None;
        let mut add_newline = false;
        if !tail.trim_ascii().is_empty() {
            match decode(tail) {
                Ok(record) => {
                    replay(&mut book, Ok(record), path, lines + 1)?;
                    add_newline = true;
                }
                Err(_) => {
                    // #8433: copy the fragment aside BEFORE cutting it, so a
                    // failed copy fails the open and loses nothing.
                    let torn = path.with_extension("jsonl.torn");
                    append_once(&torn, &[tail, b"\n"].concat())?;
                    cut(path, body_len)?;
                    quarantined = Some(tail.len());
                }
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(io_err(path))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(io_err(path))?;
        if created {
            file.sync_all().map_err(io_err(path))?;
            sync_dir(dir);
        }
        if add_newline {
            file.write_all(b"\n")
                .and_then(|()| file.sync_data())
                .map_err(io_err(path))?;
        }
        let state = State {
            file,
            book,
            poisoned: false,
            #[cfg(test)]
            fault: None,
        };
        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(state),
            quarantined,
            _lock: lock,
        })
    }

    /// Reserve the next question id, durably, before anything is sent.
    ///
    /// Why: an id whose send outcome is unknown must never be reissued.
    /// What: appends `reserve` under the lock; the id is never reused, even
    /// after a restart.
    /// Test: `concurrent_reserve_issues_unique_ids`,
    /// `reserved_never_opened_id_is_not_reused_after_restart`.
    pub fn reserve(&self) -> Result<QuestionId, QuestionTableError> {
        let mut state = self.lock()?;
        let id = state.book.next_id;
        self.commit(&mut state, Record::Reserve { id, ts: now() })?;
        Ok(id)
    }

    /// Open reserved question `id` once its send succeeded.
    ///
    /// Why: P4 opens a question only after the send is acknowledged; a reply
    /// to a reserved-only id is [`ResolveOutcome::Unknown`] and drops.
    /// What: refuses an invalid ask, an id not reserved or already open, and
    /// a poisoned table; else appends `open` and returns the question.
    /// Test: `id_reserved_before_send_is_never_reused`,
    /// `open_refuses_unreserved_or_invalid_question`.
    pub fn open(&self, id: QuestionId, ask: Ask) -> Result<Question, QuestionTableError> {
        validate(&ask).map_err(QuestionTableError::Invalid)?;
        let mut state = self.lock()?;
        if !state.book.reserved.contains(&id) {
            return Err(QuestionTableError::NotReserved(id));
        }
        let question = Question {
            id,
            opened_at: now(),
            ask,
        };
        let record = Record::Open {
            question: question.clone(),
        };
        self.commit(&mut state, record)?;
        Ok(question)
    }

    /// Resolve question `id` with `answer` from `answerer` in `conversation`,
    /// once.
    ///
    /// Why: the arbiter for P5. `check_inbound` matches route names only,
    /// and its `answered` copy is taken before this call, so the recipient,
    /// the conversation and "still open" are judged here, in the same lock
    /// hold as the append (#8433 r6 item 4).
    /// What: `Unknown` for no open or answered question; `NotRecipient`
    /// unless `answerer` and `conversation` equal the question's;
    /// `AlreadyResolved` with the first answer; else appends `resolve`,
    /// syncs it and returns `Resolved`. An append error poisons the table
    /// and returns `Poisoned`.
    /// Test: `concurrent_answers_resolve_exactly_once_n_rounds`,
    /// `first_answer_wins_second_gets_already_answered_by_x_at_t`,
    /// `answerer_must_be_the_questions_recipient`,
    /// `answer_from_other_conversation_is_refused`,
    /// `resolve_is_durable_before_it_returns`.
    pub fn resolve(
        &self,
        id: QuestionId,
        answerer: &str,
        conversation: &Conversation,
        answer: &str,
    ) -> ResolveOutcome {
        let Ok(mut state) = self.lock() else {
            return ResolveOutcome::Poisoned;
        };
        let Some(entry) = state.book.questions.get(&id) else {
            return ResolveOutcome::Unknown;
        };
        // #8433 r6: recipient and conversation are checked inside the CAS.
        let ask = &entry.question.ask;
        if ask.recipient != answerer || ask.conversation != *conversation {
            return ResolveOutcome::NotRecipient;
        }
        if let Some(first) = &entry.answer {
            return ResolveOutcome::AlreadyResolved {
                first: first.clone(),
            };
        }
        let record = Record::Resolve {
            id,
            answer: Answer::new(answerer, answer),
        };
        match self.commit(&mut state, record) {
            Ok(()) => ResolveOutcome::Resolved,
            Err(_) => ResolveOutcome::Poisoned,
        }
    }

    /// The policy's view of question `id`, for `check_inbound`.
    ///
    /// Why: P5 passes this to `PolicySnapshot::check_inbound` as a
    /// pre-filter; [`Self::resolve`] stays the arbiter.
    /// What: `None` for an unknown or reserved-only id, and on a poisoned
    /// table (the policy then drops the message).
    /// Test: `question_state_adapter_matches_policy`.
    pub fn state(&self, id: QuestionId) -> Option<QuestionState> {
        let state = self.lock().ok()?;
        let entry = state.book.questions.get(&id)?;
        Some(QuestionState {
            id,
            channel: entry.question.ask.channel,
            route: entry.question.ask.route.clone(),
            answered: entry.answer.is_some(),
        })
    }

    /// Question `id` as opened; `None` when unknown or poisoned.
    pub fn get(&self, id: QuestionId) -> Option<Question> {
        let state = self.lock().ok()?;
        state.book.questions.get(&id).map(|e| e.question.clone())
    }

    /// True once an append failed or the lock was poisoned.
    pub fn is_poisoned(&self) -> bool {
        self.state.lock().map_or(true, |state| state.poisoned)
    }

    /// The byte length of a torn final line [`Self::open_at`] moved aside.
    pub fn quarantined_bytes(&self) -> Option<usize> {
        self.quarantined
    }

    /// The lock, refusing when poisoned either way.
    fn lock(&self) -> Result<MutexGuard<'_, State>, QuestionTableError> {
        // #8433 r6: a poisoned mutex refuses; its state is never recovered
        // with `into_inner`, because a panic may have left it mid-update.
        let state = self
            .state
            .lock()
            .map_err(|_| QuestionTableError::Poisoned)?;
        if state.poisoned {
            return Err(QuestionTableError::Poisoned);
        }
        Ok(state)
    }

    /// Append `record`, then apply it; any error poisons the table.
    ///
    /// Why: #8433 r6 G1: after a failed append the bytes may be on disk
    /// (a `sync_data` failure, a partial write), so memory can no longer
    /// say what a restart will replay. Only a reopen may decide.
    /// What: one `write_all` and `sync_data` of the whole line; an error sets
    /// the poison flag and returns `Io`.
    /// Test: `append_error_poisons_table_until_reopen`,
    /// `failed_resolve_leaves_question_open_in_memory_only_if_log_agrees`.
    fn commit(&self, state: &mut State, record: Record) -> Result<(), QuestionTableError> {
        if let Err(source) = append(state, &record) {
            state.poisoned = true;
            tracing::error!(
                path = %self.path.display(),
                error = %source,
                "question table append failed; poisoned until reopened"
            );
            return Err(io_err(&self.path)(source));
        }
        state.book.apply(record).map_err(|reason| {
            state.poisoned = true;
            QuestionTableError::Corrupt {
                path: self.path.clone(),
                line: 0,
                reason,
            }
        })
    }
}

/// Write one record line to the table's log and sync it.
fn append(state: &mut State, record: &Record) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(&LineOut {
        v: FORMAT_VERSION,
        record,
    })
    .map_err(std::io::Error::other)?;
    line.push(b'\n');
    #[cfg(test)]
    if state.fault == Some(Fault::BeforeWrite) {
        return Err(std::io::Error::other("injected append failure"));
    }
    state.file.write_all(&line)?;
    #[cfg(test)]
    if state.fault == Some(Fault::AfterWrite) {
        return Err(std::io::Error::other("injected sync_data failure"));
    }
    state.file.sync_data()
}

/// Apply a decoded line on replay, mapping either failure to `Corrupt`.
fn replay(
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
fn validate(ask: &Ask) -> Result<(), &'static str> {
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
fn decode(line: &[u8]) -> Result<Record, &'static str> {
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

/// Open (creating, 0600) and exclusively `flock` `path` without waiting.
fn take_lock(path: &Path) -> Result<File, QuestionTableError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(io_err(path))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(QuestionTableError::Locked {
            path: path.to_path_buf(),
        }),
        Err(std::fs::TryLockError::Error(e)) => Err(io_err(path)(e)),
    }
}

/// Append `bytes` to `path` (creating, 0600) with one write, then sync.
fn append_once(path: &Path, bytes: &[u8]) -> Result<(), QuestionTableError> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_data()))
        .map_err(io_err(path))
}

/// Cut `path` to `len` bytes and sync it.
fn cut(path: &Path, len: usize) -> Result<(), QuestionTableError> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.set_len(len as u64).and_then(|()| f.sync_all()))
        .map_err(io_err(path))
}

/// Fsync `dir` so a new entry in it survives a power loss (#8433 r6 G2).
/// Best effort, as `trusty_common::atomic_file` does: a filesystem that
/// refuses a directory handle has still created the file.
fn sync_dir(dir: &Path) {
    if let Err(error) = File::open(dir).and_then(|d| d.sync_all()) {
        tracing::warn!(dir = %dir.display(), %error, "question table directory fsync failed");
    }
}

/// An `Io` error builder for `path`.
fn io_err(path: &Path) -> impl Fn(std::io::Error) -> QuestionTableError + '_ {
    move |source| QuestionTableError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Lowercase hex SHA-256 of `text`.
fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The current UTC time, RFC 3339 to the millisecond.
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Serde for [`Ask::channel`]: the schema spelling of a served channel; any
/// other name, `gchat` included, fails the decode (#8433 r6 G5).
mod served_channel {
    use serde::{Deserialize as _, Deserializer, Serializer};
    use trusty_channels::policy::Channel;

    pub(super) fn serialize<S: Serializer>(channel: &Channel, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(channel.as_str())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Channel, D::Error> {
        let name = String::deserialize(d)?;
        super::SERVED
            .into_iter()
            .find(|c| c.as_str() == name)
            .ok_or_else(|| serde::de::Error::custom("channel not served by the daemon"))
    }
}

#[cfg(test)]
#[path = "question_table_tests.rs"]
mod tests;
