//! The daemon's durable question table, `channels/questions.jsonl` (#8433 P2).
//!
//! Why: an inbound Slack or Telegram answer may resolve exactly one question,
//! once, and only from the recipient and conversation it was asked in. The
//! record must survive a daemon restart. This is a Fail-Open Check site: no
//! failure here may let a question resolve twice or from the wrong party.
//! What: an append-only JSONL log of `reserve`, `open`, `resolve` and
//! `unparsed` (#10127) records, replayed into memory by
//! [`QuestionTable::open_at`]. The shape follows the crate-private gchat
//! ledger in trusty-channels; the code is this module's own. One `std::sync::Mutex` covers each operation's check, append and
//! apply. A lifetime `flock` on `questions.jsonl.lock` refuses a second table
//! on the same path. Any append error poisons the table until it is reopened,
//! and the replay then decides. A poisoned or dropped table releases its
//! `flock` explicitly, so the reopen never waits on a stale holder (#8433).
//! Ids are the policy's `u64` [`QuestionId`]; P5 writes the decimal form
//! into the taint record's `question_id` string.
//! Test: `question_table_tests.rs`.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trusty_channels::policy::{Channel, QuestionId, QuestionState};

// #8433: the record format and replay rules live in `replay`.
mod replay;
use replay::{Book, LineOut, Record, decode, replay_line, validate};

// #9815: the open-time seal check on the log and its lock.
mod seal;

// #9923: the inbound event claim log reuses this table's flock and seal.
pub mod claims;

// #10127: the answer side moved out at the SLOC cap; it gains `unparsed`.
mod answer;
pub use answer::{Answer, ResolveOutcome, Unparsed, UnparsedOutcome};

// #10127: the option-label rules, shared by `ask_owner` and the replay.
mod options;
pub use options::{MAX_OPTION_BYTES, MAX_OPTIONS, MIN_OPTIONS, validate_options};

/// The record format version every line carries.
pub const FORMAT_VERSION: u32 = 1;

/// The question table under `home`:
/// `~/.trusty-tools/trusty-mpm/channels/questions.jsonl`.
// #9815: `tm hook --pm-guard` seals this file, its `.lock` and `.torn`.
pub fn questions_path(home: &Path) -> PathBuf {
    trusty_common::crate_config::crate_config_dir_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    )
    .join("channels")
    .join("questions.jsonl")
}

/// The lifetime lock beside the table log at `path`.
pub fn lock_path(path: &Path) -> PathBuf {
    path.with_extension("jsonl.lock")
}

/// Where [`QuestionTable::open_at`] moves a torn final line of `path`.
pub fn torn_path(path: &Path) -> PathBuf {
    path.with_extension("jsonl.torn")
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
    /// The log or its lock is not in the state the daemon leaves it in, so a
    /// writer outside the trust-anchor seal changed it (#9815).
    #[error("{}: not sealed ({reason}); the question table refuses to open", path.display())]
    Unsealed {
        /// The log or lock.
        path: PathBuf,
        /// A content-free reason.
        reason: &'static str,
    },
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
    #[serde(with = "replay::served_channel")]
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
    /// The option labels, numbered from 1; empty for a free-text question.
    // #10127: a line written before options existed reads as empty, and an
    // empty set is not written, so a pre-P2 daemon still reads such a line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
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

/// The live table: the log handle, the replayed book and the poison flag.
#[derive(Debug)]
struct State {
    file: File,
    book: Book,
    poisoned: bool,
    #[cfg(test)]
    fault: Option<Fault>,
    #[cfg(test)]
    unlock_fails: bool,
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
#[derive(Debug)]
pub struct QuestionTable {
    path: PathBuf,
    state: Mutex<State>,
    quarantined: Option<usize>,
    flock: Flock,
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
    /// file is created 0600, and its directory fsynced. #9815: an existing log,
    /// lock or torn file that is not sealed ([`seal::check_path`]) refuses the
    /// open with `Unsealed`; the torn-file append follows no symlink and does
    /// not wait on a FIFO.
    /// Test: `table_survives_restart`, `torn_final_line_quarantined`,
    /// `final_line_missing_only_newline_is_kept`,
    /// `malformed_middle_line_fails_the_open`,
    /// `second_table_on_same_path_is_refused_while_first_lives`,
    /// `file_is_0600_and_dir_0700`, `home_write_fence_is_called`,
    /// `symlinked_log_refuses_to_open`, `symlinked_lock_refuses_to_open`,
    /// `torn_file_symlink_is_not_followed`,
    /// `fifo_at_the_torn_file_refuses_to_open_without_blocking`.
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
        let lock = take_lock(&lock_path(path))?;
        // #9815: a log changed outside the trust-anchor seal never opens.
        seal::check_path(path)?;
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
                replay_line(&mut book, decode(line), path, lines)?;
            }
        }
        let mut quarantined = None;
        let mut add_newline = false;
        if !tail.trim_ascii().is_empty() {
            match decode(tail) {
                Ok(record) => {
                    replay_line(&mut book, Ok(record), path, lines + 1)?;
                    add_newline = true;
                }
                Err(_) => {
                    // #8433: copy the fragment aside BEFORE cutting it, so a
                    // failed copy fails the open and loses nothing.
                    let torn = torn_path(path);
                    // #9815: a torn file changed outside the seal (a FIFO, a
                    // link) is never written; it would block or redirect.
                    seal::check_path(&torn)?;
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
            #[cfg(test)]
            unlock_fails: false,
        };
        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(state),
            quarantined,
            flock: lock,
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

    /// The question posted in `conversation`, open or answered.
    ///
    /// Why: #9873 Q2: an owner's thread reply carries no `[Q-n]` token, so
    /// the conversation is the binding; text is never trusted to bind.
    /// What: the one opened question whose conversation equals it. `None`
    /// for no match, for two (fail closed), and on a poisoned table. A
    /// reserved-only id has no conversation, so it is never found.
    /// Test: `conversation_lookup_finds_the_question_posted_there`,
    /// `conversation_lookup_on_a_poisoned_table_is_none`.
    pub fn find_by_conversation(&self, conversation: &Conversation) -> Option<QuestionId> {
        let state = self.lock().ok()?;
        let mut found = state
            .book
            .questions
            .values()
            .filter(|e| e.question.ask.conversation == *conversation);
        let id = found.next()?.question.id;
        found.next().is_none().then_some(id)
    }

    /// The first answer to question `id`; `None` when unanswered, unknown or
    /// poisoned. #9873: a second answer reports who won, and when.
    pub fn answer(&self, id: QuestionId) -> Option<Answer> {
        let state = self.lock().ok()?;
        state.book.questions.get(&id)?.answer.clone()
    }

    /// True once an append failed or the lock was poisoned.
    pub fn is_poisoned(&self) -> bool {
        // #8433: a poisoned mutex refuses for good, so its flock can go.
        self.state.lock().map_or_else(
            |_| {
                self.flock.release("mutex poisoned");
                true
            },
            |state| state.poisoned,
        )
    }

    /// The byte length of a torn final line [`Self::open_at`] moved aside.
    pub fn quarantined_bytes(&self) -> Option<usize> {
        self.quarantined
    }

    /// The lock, refusing when poisoned either way.
    fn lock(&self) -> Result<MutexGuard<'_, State>, QuestionTableError> {
        // #8433 r6: a poisoned mutex refuses; its state is never recovered
        // with `into_inner`, because a panic may have left it mid-update.
        let state = self.state.lock().map_err(|_| {
            // #8433: a poisoned mutex refuses for good, so its flock can go.
            self.flock.release("mutex poisoned");
            QuestionTableError::Poisoned
        })?;
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
            self.poison(state);
            tracing::error!(
                path = %self.path.display(),
                error = %source,
                "question table append failed; poisoned until reopened"
            );
            return Err(io_err(&self.path)(source));
        }
        state.book.apply(record).map_err(|reason| {
            self.poison(state);
            QuestionTableError::Corrupt {
                path: self.path.clone(),
                line: 0,
                reason,
            }
        })
    }

    /// Poison `state`, then release the table's flock.
    ///
    /// Why: #8433: the daemon replaces a poisoned table while stale `Arc`
    /// holders still run; with the flock held, its reopen saw `Locked`.
    /// What: sets the poison flag first, so every later operation refuses
    /// (`lock` checks it) and the reopened table is the only writer; then
    /// unlocks. A failed unlock is logged and the table stays poisoned: the
    /// reopen then sees `Locked` until the last holder drops it.
    /// Test: `poisoned_table_releases_its_flock_while_arc_held`,
    /// `failed_unlock_at_poison_keeps_the_table_refusing_and_locked`.
    fn poison(&self, state: &mut State) {
        state.poisoned = true;
        #[cfg(test)]
        if state.unlock_fails {
            let injected = std::io::Error::other("injected unlock failure");
            self.flock.settle(Err(injected), "poisoned");
            return;
        }
        self.flock.release("poisoned");
    }
}

/// The table's lifetime `flock`, released explicitly (#8433).
///
/// Why: a flock belongs to the open file description. A child forked
/// before it execs shares that description, and a poisoned table's stale
/// `Arc` holders keep the descriptor open, so closing it alone left the
/// lock held for those windows.
/// What: [`Self::release`] calls `File::unlock` (`LOCK_UN` on the shared
/// description), which ends the lock for every copy. `Drop` releases it, so
/// a dropped table and an `open_at` that fails after the lock both free it.
/// A live table never releases it, so a second opener is still refused.
/// Test: `dropped_table_releases_its_flock_despite_a_forked_child`,
/// `second_table_on_same_path_is_refused_while_first_lives`.
#[derive(Debug)]
struct Flock {
    file: File,
    path: PathBuf,
}

impl Flock {
    /// Unlock; a failure is logged, never returned.
    fn release(&self, why: &'static str) {
        self.settle(self.file.unlock(), why);
    }

    /// Log a failed unlock. The lock then holds until every copy of the
    /// descriptor closes: the old behaviour, which refuses a reopen
    /// (`Locked`) and never lets two tables write.
    fn settle(&self, unlocked: std::io::Result<()>, why: &'static str) {
        if let Err(error) = unlocked {
            tracing::error!(
                path = %self.path.display(),
                %error,
                why,
                "question table flock release failed; it holds until its descriptor closes"
            );
        }
    }
}

impl Drop for Flock {
    fn drop(&mut self) {
        // #8433: the descriptor closes next, which releases too, unless a
        // forked child holds a copy; the unlock covers that copy.
        self.release("dropped");
    }
}

/// The answered questions in the log at `path`, with their askers, read
/// without the table's lock.
///
/// Why: #9873 Q4a: `tm doctor` lists a question resolved with no delivery
/// record while the daemon holds the lock, so it cannot open the table.
/// What: replays every newline-terminated line under the live rules; an
/// unterminated final line (an append in flight, or a torn one the next
/// open judges) is skipped. An absent log has none; a read or replay
/// error is an `Err`.
/// Test: `resolved_question_without_a_delivery_line_is_a_doctor_fail`.
pub fn answered_in_log(path: &Path) -> Result<Vec<(QuestionId, String)>, QuestionTableError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(path)(e)),
    };
    let mut book = Book::new();
    for (n, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if line.ends_with(b"\n") && !line.trim_ascii().is_empty() {
            replay_line(&mut book, decode(line), path, n + 1)?;
        }
    }
    Ok(book
        .questions
        .values()
        .filter(|e| e.answer.is_some())
        .map(|e| (e.question.id, e.question.ask.asker_managed_session.clone()))
        .collect())
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

/// Open (creating, 0600) and exclusively `flock` `path` without waiting.
fn take_lock(path: &Path) -> Result<Flock, QuestionTableError> {
    // #9815: a lock changed outside the trust-anchor seal is never taken.
    seal::check_path(path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(io_err(path))?;
    match file.try_lock() {
        Ok(()) => Ok(Flock {
            file,
            path: path.to_path_buf(),
        }),
        Err(std::fs::TryLockError::WouldBlock) => Err(QuestionTableError::Locked {
            path: path.to_path_buf(),
        }),
        Err(std::fs::TryLockError::Error(e)) => Err(io_err(path)(e)),
    }
}

/// Append `bytes` to `path` (creating, 0600) with one write, then sync.
/// Test: `torn_line_append_on_a_fifo_fails_fast`,
/// `torn_file_symlink_is_not_followed`.
fn append_once(path: &Path, bytes: &[u8]) -> Result<(), QuestionTableError> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        // #9815: a symlink planted here would aim the daemon's write elsewhere,
        // and a FIFO swapped in after the seal check would block the open
        // (and the daemon's table slot) until a reader came; fail fast.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
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

#[cfg(test)]
#[path = "question_table_tests.rs"]
mod tests;
