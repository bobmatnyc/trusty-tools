//! `poll-state.json`: the poller's state across cycles.
//!
//! Why: dedup, the idle clocks, the events offset and the builder band live
//! here. The prototype writes it with `json.dump` to a fixed `.tmp` file and
//! `os.replace`, with no fsync, and treats an unreadable file as empty. Rust
//! owns the format (Architect ruling Q7) but must keep every key it does not
//! know, so a rollback to Python keeps its state (#9171 A3a).
//! What: [`PollState`] types the keys A2/A3 code reads and keeps every other
//! key in [`PollState::other`] unchanged. The `wake_*` keys are typed in
//! A3b-2; a stored value of the wrong type loads as
//! [`StoredCount::Unreadable`] (or no `wake_sent_at`) instead of failing the
//! whole file, so an owed wake is never reset by a parse error. [`save_state`] creates a uniquely
//! named sibling (`<path>.<pid>.<n>.tmp`, `create_new`), so two writers never
//! share a temporary file, writes and syncs it, renames it over `path` and
//! syncs the directory. A kill at any point leaves either the old file or the
//! new one, never a truncated one; a save that fails removes its temporary
//! file, while a SIGKILL can leave one behind. [`load_state`] returns an empty state for a missing file and
//! an empty state plus the error for an unreadable or corrupt one.
//! Test: `a_save_killed_between_steps_leaves_a_state_that_parses`,
//! `state_survives_sigkill_mid_save`, `unknown_state_keys_round_trip`,
//! `concurrent_saves_use_distinct_temporary_files`,
//! `a_failed_save_leaves_no_temporary_file`.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::classify::{DedupState, PaneState, Seen};
use crate::error::StateError;
use crate::pyjson::to_python_json;

/// The poller's persisted state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PollState {
    /// Dedup keys (`session|pane|kind|fingerprint`) in insertion order.
    #[serde(
        default,
        skip_serializing_if = "DedupState::is_empty",
        serialize_with = "ser_dedup",
        deserialize_with = "de_dedup"
    )]
    pub alerts: DedupState,
    /// Per-pane state keyed `session|pane`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub panes: BTreeMap<String, PaneState>,
    /// The watched sessions of the last cycle and their pane ids.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sessions: BTreeMap<String, String>,
    /// Bytes of `events.jsonl` already read; `None` before the first read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events_offset: Option<u64>,
    /// The stored builder band (`stop`/`hold`/`open`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<String>,
    /// Alerts owed a wake: carried until a wake reports `sent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_pending: Option<StoredCount>,
    /// How many of the owed alerts triage only logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_logged: Option<StoredCount>,
    /// When the last wake was sent (the watchdog's silent clock). A value
    /// that is not a number reads as unset.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_lenient_time"
    )]
    pub wake_sent_at: Option<f64>,
    /// Every other key, kept as read (wake counters, rc, usage, ...).
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// A count in `poll-state.json`, or the value found when it is not one.
///
/// Why: `wake_pending` is a wake owed. A typed `u64` would turn a stray value
/// into a corrupt file and an empty state, dropping the debt (#9171 A3b-1
/// critic HIGH). What: a non-negative integer reads as [`StoredCount::Count`];
/// anything else keeps its JSON in [`StoredCount::Unreadable`] and writes it
/// back unchanged.
/// Test: `wake_fields_are_typed_and_unreadable_counts_survive_load`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StoredCount {
    /// A non-negative integer.
    Count(u64),
    /// Any other JSON value, kept as read.
    Unreadable(Value),
}

impl StoredCount {
    /// The count, or `unreadable` (logged) when the stored value is not one.
    pub fn or_unreadable(&self, key: &str, unreadable: u64) -> u64 {
        match self {
            Self::Count(n) => *n,
            Self::Unreadable(v) => {
                tracing::warn!(key, value = %v, unreadable, "poll-state counter is not a count");
                unreadable
            }
        }
    }
}

// #9171 A3b-2: a wake_sent_at of the wrong type must not fail the file.
fn de_lenient_time<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let v = Value::deserialize(d)?;
    if !v.is_null() && !v.is_number() {
        tracing::warn!(value = %v, "poll-state wake_sent_at is not a number; read as unset");
    }
    Ok(v.as_f64())
}

fn ser_dedup<S: Serializer>(d: &DedupState, s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut map = s.serialize_map(Some(d.entries().len()))?;
    for (k, v) in d.entries() {
        map.serialize_entry(k, v)?;
    }
    map.end()
}

fn de_dedup<'de, D: Deserializer<'de>>(d: D) -> Result<DedupState, D::Error> {
    struct Ordered;
    impl<'de> serde::de::Visitor<'de> for Ordered {
        type Value = DedupState;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a map of dedup keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<DedupState, A::Error> {
            let mut entries = Vec::new();
            while let Some((k, v)) = m.next_entry::<String, Seen>()? {
                entries.push((k, v));
            }
            Ok(DedupState::from_entries(entries))
        }
    }
    d.deserialize_map(Ordered)
}

/// A point inside [`save_state_with`] where a kill can land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStep {
    /// The bytes are in the temporary file, not yet synced.
    Written,
    /// The temporary file is synced, not yet renamed.
    Synced,
    /// The temporary file is renamed over `path`; the directory is not yet synced.
    Renamed,
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> StateError + '_ {
    move |source| StateError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Distinguishes the temporary files of saves in one process.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// How many names to try before giving up on a directory full of stale files.
const TMP_ATTEMPTS: u32 = 32;

/// Creates `<path>.<pid>.<n>.tmp` with `create_new`, so it never opens a file
/// another writer (or a stale one from a reused pid) already holds.
fn create_tmp(path: &Path) -> io::Result<(File, PathBuf)> {
    for _ in 0..TMP_ATTEMPTS {
        let mut name = path.as_os_str().to_owned();
        let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        name.push(format!(".{}.{n}.tmp", std::process::id()));
        let tmp = PathBuf::from(name);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((f, tmp)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unused temporary file name",
    ))
}

/// Writes `state` to `path` atomically (unique tmp + fsync + rename + dir fsync).
///
/// # Errors
/// [`StateError::Io`] when any step fails. A failure before the rename
/// leaves the old state at `path`; a failure of the directory sync after it
/// leaves the new state there, possibly not yet durable (#9171 A3b-1, #9803
/// critic carry-over).
pub fn save_state(path: &Path, state: &PollState) -> Result<(), StateError> {
    save_state_with(path, state, &mut |_| Ok(()))
}

/// [`save_state`] with `at` called after each [`SaveStep`]; an error from
/// `at` stops the save there, as a kill would. The test seam for crash safety.
///
/// # Errors
/// As [`save_state`], plus whatever `at` returns.
pub fn save_state_with(
    path: &Path,
    state: &PollState,
    at: &mut dyn FnMut(SaveStep) -> io::Result<()>,
) -> Result<(), StateError> {
    let text = to_python_json(state, true).map_err(|e| StateError::Io {
        path: path.to_owned(),
        source: io::Error::other(e),
    })?;
    let (mut f, tmp) = create_tmp(path).map_err(io_err(path))?;
    let saved = write_and_rename(&mut f, &tmp, path, text.as_bytes(), at);
    if saved.is_err() {
        // Best effort: after a rename the file is already gone (NotFound).
        drop(f);
        let _ = fs::remove_file(&tmp);
    }
    saved
}

/// The steps of [`save_state_with`] after the temporary file exists.
fn write_and_rename(
    f: &mut File,
    tmp: &Path,
    path: &Path,
    text: &[u8],
    at: &mut dyn FnMut(SaveStep) -> io::Result<()>,
) -> Result<(), StateError> {
    f.write_all(text).map_err(io_err(tmp))?;
    at(SaveStep::Written).map_err(io_err(tmp))?;
    f.sync_all().map_err(io_err(tmp))?;
    at(SaveStep::Synced).map_err(io_err(tmp))?;
    fs::rename(tmp, path).map_err(io_err(path))?;
    at(SaveStep::Renamed).map_err(io_err(path))?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(io_err(dir))?;
    }
    Ok(())
}

/// Reads `path`. A missing file is an empty state and no error; an unreadable
/// or corrupt file (bytes that are not UTF-8 count as corrupt) is an empty
/// state and the error, for the caller to log.
pub fn load_state(path: &Path) -> (PollState, Option<StateError>) {
    // #9171: read bytes, not a String. Bytes that are not UTF-8 are a corrupt
    // file (copied, then reset), not an I/O error that blocks saves for good.
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (PollState::default(), None),
        Err(source) => {
            let err = StateError::Io {
                path: path.to_owned(),
                source,
            };
            return (PollState::default(), Some(err));
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(state) => (state, None),
        Err(source) => {
            let err = StateError::Corrupt {
                path: path.to_owned(),
                source,
            };
            (PollState::default(), Some(err))
        }
    }
}
