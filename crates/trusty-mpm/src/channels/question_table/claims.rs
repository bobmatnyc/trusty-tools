//! The durable inbound event claim log, `channels/inbound_claims.jsonl`
//! (#9923, #8433 P6).
//!
//! Why: Slack Socket Mode redelivers an event whose envelope was not acked in
//! time, with the same `event_id`. An answer must act at most once, across a
//! daemon restart too, so the daemon claims each event id durably BEFORE it
//! acks the envelope and before any effect (Q3, Q4). This is a Fail-Open
//! Check site: a claim that cannot be written acks nothing and acts on
//! nothing; no arm logs and goes on.
//! What: an append-only JSONL log of `claimed` and `done` records, a sibling
//! of the question table, reusing its `Flock`, `seal::check_path`, torn-line
//! quarantine and replay rules. [`ClaimStore::open_at`] prunes claims older
//! than [`RETENTION`] (Q5) by writing the kept lines to `.prune` and renaming
//! it over the log, so a failed prune deletes nothing. A crash between claim
//! and effect leaves `claimed` with no `done`: the answer is final and lost
//! (Q4), and [`claimed_never_done`] lists it for `tm doctor`.
//! Test: `inbound_claims_tests.rs`.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    FORMAT_VERSION, Flock, QuestionTableError, append_once, cut, io_err, lock_path, seal, sync_dir,
    take_lock, torn_path,
};

/// How long a claim is kept before an open prunes it (#9923 Q5).
pub const RETENTION: chrono::TimeDelta = chrono::TimeDelta::days(7);

/// The claim log under `home`:
/// `~/.trusty-tools/trusty-mpm/channels/inbound_claims.jsonl`.
// #9923: `tm hook --pm-guard` seals this file, its `.lock`, `.torn` and `.prune`.
pub fn claims_path(home: &Path) -> PathBuf {
    super::questions_path(home).with_file_name("inbound_claims.jsonl")
}

/// Where a prune writes the kept lines before it renames them over `path`.
pub fn prune_path(path: &Path) -> PathBuf {
    path.with_extension("jsonl.prune")
}

/// A claim-log operation that could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    /// The lock, seal, file or replay step failed, as the question table
    /// reports it.
    #[error(transparent)]
    Store(#[from] QuestionTableError),
    /// An append failed or the lock was poisoned; reopen the log.
    #[error("inbound claim log is poisoned; reopen it")]
    Poisoned,
    /// The event id is not a plain Slack id, so it is never written.
    #[error("event id is empty, too long or not alphanumeric")]
    InvalidEventId,
    /// The daemon has no home to hold the log under.
    #[error("no home directory to hold the inbound claim log")]
    NoHome,
}

/// What [`ClaimStore::claim`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// The id was new and is now durably claimed: ack, then act.
    Fresh,
    /// The id was claimed before (a retry, or a redelivery after restart):
    /// ack and do nothing.
    Duplicate,
}

/// One log line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Claimed {
        event_id: String,
        at: String,
    },
    Done {
        event_id: String,
        at: String,
        outcome: String,
    },
}

/// One log line as written and read.
#[derive(Serialize, Deserialize)]
struct Line {
    v: u32,
    #[serde(flatten)]
    record: Record,
}

/// One claimed id: when, and the settle's outcome once done.
#[derive(Debug, Clone)]
struct Entry {
    at: DateTime<Utc>,
    done: Option<String>,
}

/// The replayed claims, keyed by event id.
type Book = BTreeMap<String, Entry>;

/// Test seam: which step fails.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimFault {
    /// Every append fails before its bytes reach the file.
    Append,
    /// The prune's write of `.prune` fails.
    Prune,
}

#[derive(Debug)]
struct State {
    file: File,
    book: Book,
    poisoned: bool,
    #[cfg(test)]
    fault: Option<ClaimFault>,
}

/// The daemon's claim log over one `inbound_claims.jsonl`.
///
/// Why: see the module doc.
/// What: one mutex covers each claim's check, append and apply; an append
/// error poisons the store until it is reopened. A lifetime `flock` on
/// `.lock` refuses a second store on the path. Methods block on one fsync.
/// Test: `inbound_claims_tests.rs`.
#[derive(Debug)]
pub struct ClaimStore {
    path: PathBuf,
    state: Mutex<State>,
    flock: Flock,
}

impl ClaimStore {
    /// Take the lock, replay, and prune claims past [`RETENTION`].
    ///
    /// Why: a restarted daemon must still refuse a redelivered event, and
    /// two daemons must never share one log (Q3, Q5).
    /// What: [`Self::open_at_time`] at the current time.
    /// Test: `claim_survives_daemon_restart_so_a_retry_after_restart_is_not_redelivered`.
    pub fn open_at(path: &Path) -> Result<Self, ClaimError> {
        Self::open_inner(path, Utc::now(), None)
    }

    /// [`Self::open_at`] judged at `now`.
    ///
    /// What: the home-write fence; the directory 0700; the `flock` on
    /// `.lock` (held: `Locked`); the seal on the log, `.torn` and `.prune`
    /// (`Unsealed`); then every newline-terminated line must decode and
    /// apply, or the open fails `Corrupt`. An undecodable final line is
    /// appended to `.torn` before the log is cut. Then the prune: when any
    /// claim is older than [`RETENTION`], the rest are written to `.prune`
    /// (0600, `O_EXCL`, synced) and renamed over the log; any failure fails
    /// the open and leaves the log as it was.
    /// Test: `prune_drops_only_expired_claims`,
    /// `prune_failure_fails_the_open_and_deletes_no_unexpired_claim`,
    /// `corrupt_claim_line_fails_the_open`, `unsealed_claim_log_refuses_to_open`.
    pub fn open_at_time(path: &Path, now: DateTime<Utc>) -> Result<Self, ClaimError> {
        Self::open_inner(path, now, None)
    }

    /// [`Self::open_at_time`] with a fault injected.
    #[cfg(test)]
    pub(crate) fn open_with_fault(
        path: &Path,
        now: DateTime<Utc>,
        fault: ClaimFault,
    ) -> Result<Self, ClaimError> {
        Self::open_inner(path, now, Some(fault))
    }

    fn open_inner(
        path: &Path,
        now: DateTime<Utc>,
        #[cfg_attr(not(test), allow(unused_variables))] fault: Option<FaultArg>,
    ) -> Result<Self, ClaimError> {
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
        let flock = take_lock(&lock_path(path))?;
        // #9923 Q3: a log changed outside the trust-anchor seal never opens.
        seal::check_path(path)?;
        seal::check_path(&prune_path(path))?;
        let mut book = replay(path)?;
        let mut keep = book.clone();
        keep.retain(|_, e| now.signed_duration_since(e.at) <= RETENTION);
        if keep.len() != book.len() {
            #[cfg(test)]
            let failing = fault == Some(ClaimFault::Prune);
            #[cfg(not(test))]
            let failing = false;
            prune(path, dir, &keep, failing)?;
            book = keep;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(io_err(path))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(io_err(path))?;
        file.sync_all().map_err(io_err(path))?;
        sync_dir(dir);
        let state = State {
            file,
            book,
            poisoned: false,
            #[cfg(test)]
            fault: fault.filter(|f| *f == ClaimFault::Append),
        };
        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(state),
            flock,
        })
    }

    /// Claim `event_id` durably, once.
    ///
    /// Why: the claim is the dedup; the caller acks only after it returns
    /// `Ok`, and acts only on [`Claim::Fresh`] (#9923 Q4).
    /// What: an id already in the book is `Duplicate` and writes nothing;
    /// else one `claimed` line is written and synced, then applied. An
    /// invalid id, a poisoned store and an append error are `Err`; the
    /// append error poisons the store.
    /// Test: `event_id_claimed_by_daemon_before_any_effect`,
    /// `claim_store_write_failure_acks_nothing_and_acts_on_nothing`.
    pub fn claim(&self, event_id: &str) -> Result<Claim, ClaimError> {
        check_event_id(event_id)?;
        let mut state = self.lock()?;
        if state.book.contains_key(event_id) {
            return Ok(Claim::Duplicate);
        }
        let at = Utc::now();
        let record = Record::Claimed {
            event_id: event_id.to_owned(),
            at: stamp(at),
        };
        self.commit(&mut state, &record)?;
        state
            .book
            .insert(event_id.to_owned(), Entry { at, done: None });
        Ok(Claim::Fresh)
    }

    /// Record that the effect for claimed `event_id` ended with `outcome`.
    ///
    /// What: one `done` line. An id already done is `Ok` and writes nothing;
    /// an unclaimed id is `InvalidEventId`. An append error poisons the
    /// store. The claim stands either way, so a retry stays dropped.
    /// Test: `claimed_never_done_is_listed_for_doctor`.
    pub fn settle(&self, event_id: &str, outcome: &str) -> Result<(), ClaimError> {
        check_event_id(event_id)?;
        let mut state = self.lock()?;
        match state.book.get(event_id) {
            None => return Err(ClaimError::InvalidEventId),
            Some(entry) if entry.done.is_some() => return Ok(()),
            Some(_) => {}
        }
        let outcome = code_of(outcome);
        let record = Record::Done {
            event_id: event_id.to_owned(),
            at: stamp(Utc::now()),
            outcome: outcome.clone(),
        };
        self.commit(&mut state, &record)?;
        if let Some(entry) = state.book.get_mut(event_id) {
            entry.done = Some(outcome);
        }
        Ok(())
    }

    /// True once an append failed or the lock was poisoned.
    pub fn is_poisoned(&self) -> bool {
        self.state.lock().map_or_else(
            |_| {
                self.flock.release("claim log mutex poisoned");
                true
            },
            |state| state.poisoned,
        )
    }

    /// The lock, refusing when poisoned either way.
    fn lock(&self) -> Result<MutexGuard<'_, State>, ClaimError> {
        let state = self.state.lock().map_err(|_| {
            self.flock.release("claim log mutex poisoned");
            ClaimError::Poisoned
        })?;
        if state.poisoned {
            return Err(ClaimError::Poisoned);
        }
        Ok(state)
    }

    /// Append `record` and sync it; an error poisons the store and frees
    /// its flock, so the holder's reopen replays what reached the disk.
    fn commit(&self, state: &mut State, record: &Record) -> Result<(), ClaimError> {
        if let Err(source) = append(state, record) {
            state.poisoned = true;
            self.flock.release("claim log poisoned");
            tracing::error!(
                path = %self.path.display(),
                error = %source,
                "inbound claim append failed; poisoned until reopened"
            );
            return Err(io_err(&self.path)(source).into());
        }
        Ok(())
    }
}

/// The claim fault an open may carry; `()`-like outside tests.
#[cfg(test)]
type FaultArg = ClaimFault;
#[cfg(not(test))]
type FaultArg = std::convert::Infallible;

/// The daemon's slot for its open claim store.
///
/// Why: "no claim store, no ack" is decided in one place, and a poisoned
/// store must be replaced by a reopen whose replay decides (Q3).
/// What: [`Self::get`] returns the held store unless poisoned, else opens
/// `path` again; an open error is returned and not cached.
/// Test: `poisoned_claim_store_is_reopened_by_its_hold`.
#[derive(Debug, Default)]
pub struct ClaimHold {
    slot: Mutex<Option<Arc<ClaimStore>>>,
}

impl ClaimHold {
    /// The store over `path`, opening it when none is held. Blocking.
    pub fn get(&self, path: &Path) -> Result<Arc<ClaimStore>, ClaimError> {
        let mut slot = self.slot.lock().map_err(|_| ClaimError::Poisoned)?;
        if let Some(store) = slot.as_ref().filter(|s| !s.is_poisoned()) {
            return Ok(Arc::clone(store));
        }
        *slot = None;
        let store = Arc::new(ClaimStore::open_at(path)?);
        *slot = Some(Arc::clone(&store));
        Ok(store)
    }
}

/// The event ids claimed at least `min_age` before `now` with no `done`
/// line, read without the store's lock.
///
/// Why: #9923 Q4: a crash between claim and effect loses the answer; the
/// operator must see it. `tm doctor` cannot take the daemon's lock.
/// What: replays newline-terminated lines only (an in-flight append is
/// skipped); an absent log has none; a read or replay error is `Err`.
/// Test: `claimed_never_done_is_listed_for_doctor`.
pub fn claimed_never_done(
    path: &Path,
    now: DateTime<Utc>,
    min_age: chrono::TimeDelta,
) -> Result<Vec<String>, QuestionTableError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(path)(e)),
    };
    let mut book = Book::new();
    for (n, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if line.ends_with(b"\n") && !line.trim_ascii().is_empty() {
            apply_line(&mut book, line, path, n + 1)?;
        }
    }
    Ok(book
        .into_iter()
        .filter(|(_, e)| e.done.is_none() && now.signed_duration_since(e.at) >= min_age)
        .map(|(id, _)| id)
        .collect())
}

/// Replay the log at `path`, quarantining an undecodable final line.
fn replay(path: &Path) -> Result<Book, QuestionTableError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(io_err(path)(e)),
    };
    let mut book = Book::new();
    let body_len = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let (body, tail) = bytes.split_at(body_len);
    let mut lines = 0;
    for line in body.split_inclusive(|b| *b == b'\n') {
        lines += 1;
        if !line.trim_ascii().is_empty() {
            apply_line(&mut book, line, path, lines)?;
        }
    }
    if !tail.trim_ascii().is_empty() {
        // #9923: the question table's rule: a torn claim never applies, and
        // is copied aside before the cut so a failed copy loses nothing.
        let torn = torn_path(path);
        seal::check_path(&torn)?;
        append_once(&torn, &[tail, b"\n"].concat())?;
        cut(path, body_len)?;
    }
    Ok(book)
}

/// Decode and apply one line, or fail `Corrupt` naming it.
fn apply_line(
    book: &mut Book,
    line: &[u8],
    path: &Path,
    n: usize,
) -> Result<(), QuestionTableError> {
    decode(line)
        .and_then(|record| apply(book, record))
        .map_err(|reason| QuestionTableError::Corrupt {
            path: path.to_path_buf(),
            line: n,
            reason,
        })
}

/// One line as a record; the error is a content-free class.
fn decode(line: &[u8]) -> Result<Record, &'static str> {
    let text = std::str::from_utf8(line).map_err(|_| "invalid UTF-8")?;
    let line: Line = serde_json::from_str(text).map_err(|_| "undecodable claim line")?;
    if line.v != FORMAT_VERSION {
        return Err("unknown record version");
    }
    Ok(line.record)
}

/// The one state transition, live and on replay.
fn apply(book: &mut Book, record: Record) -> Result<(), &'static str> {
    match record {
        Record::Claimed { event_id, at } => {
            check_event_id(&event_id).map_err(|_| "invalid event id")?;
            let at = parse_stamp(&at)?;
            if book.insert(event_id, Entry { at, done: None }).is_some() {
                return Err("event id claimed twice");
            }
        }
        Record::Done {
            event_id, outcome, ..
        } => match book.get_mut(&event_id) {
            None => return Err("done for an unclaimed event id"),
            Some(entry) if entry.done.is_some() => return Err("second done for one event id"),
            Some(entry) => entry.done = Some(outcome),
        },
    }
    Ok(())
}

/// Rewrite the log as `keep`: write `.prune` (`O_EXCL`, no symlink, 0600),
/// sync, rename over `path`, sync the directory.
fn prune(path: &Path, dir: &Path, keep: &Book, failing: bool) -> Result<(), QuestionTableError> {
    let tmp = prune_path(path);
    // #9923: a `.prune` the daemon left (a crash mid-prune) is sealed, so it
    // is ours; remove it rather than refuse every later open.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(&tmp)(e)),
    }
    let mut bytes = Vec::new();
    for (event_id, entry) in keep {
        let claimed = Record::Claimed {
            event_id: event_id.clone(),
            at: stamp(entry.at),
        };
        bytes.extend(line_bytes(&claimed).map_err(io_err(&tmp))?);
        if let Some(outcome) = &entry.done {
            let done = Record::Done {
                event_id: event_id.clone(),
                at: stamp(entry.at),
                outcome: outcome.clone(),
            };
            bytes.extend(line_bytes(&done).map_err(io_err(&tmp))?);
        }
    }
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut f| {
            if failing {
                return Err(std::io::Error::other("injected prune failure"));
            }
            f.write_all(&bytes).and_then(|()| f.sync_all())
        });
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        // #9923 Q5: the log is untouched; the leftover `.prune` is removed
        // by the next open.
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err(path)(e));
    }
    sync_dir(dir);
    Ok(())
}

/// Write one record line to the store's log and sync it.
fn append(state: &mut State, record: &Record) -> std::io::Result<()> {
    let line = line_bytes(record)?;
    #[cfg(test)]
    if state.fault == Some(ClaimFault::Append) {
        return Err(std::io::Error::other("injected claim append failure"));
    }
    state.file.write_all(&line)?;
    state.file.sync_data()
}

/// `record` as one newline-terminated line.
fn line_bytes(record: &Record) -> std::io::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(&Line {
        v: FORMAT_VERSION,
        record: record.clone(),
    })
    .map_err(std::io::Error::other)?;
    line.push(b'\n');
    Ok(line)
}

/// A Slack event id: 1 to 64 ASCII letters and digits.
fn check_event_id(event_id: &str) -> Result<(), ClaimError> {
    let plain = !event_id.is_empty()
        && event_id.len() <= 64
        && event_id.bytes().all(|b| b.is_ascii_alphanumeric());
    plain.then_some(()).ok_or(ClaimError::InvalidEventId)
}

/// An outcome code kept to `[a-z0-9_]` and 64 chars.
fn code_of(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .take(64)
        .collect()
}

/// `at` as RFC 3339, milliseconds.
fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A stamp written by [`stamp`].
fn parse_stamp(text: &str) -> Result<DateTime<Utc>, &'static str> {
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| "invalid claim time")
}

#[cfg(test)]
#[path = "../inbound_claims_tests.rs"]
mod tests;
