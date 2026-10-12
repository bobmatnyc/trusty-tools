//! `poll-state.json`: the poller's state across cycles.
//!
//! Why: dedup, the idle clocks, the events offset and the builder band live
//! here. The prototype writes it with `json.dump` to a `.tmp` file and
//! `os.replace`, with no fsync, and treats an unreadable file as empty. Rust
//! owns the format (Architect ruling Q7) but must keep every key it does not
//! know, so a rollback to Python keeps its state (#9171 A3a).
//! What: [`PollState`] types the keys A2/A3a code reads and keeps every other
//! key in [`PollState::other`] unchanged. [`save_state`] writes
//! `<path>.tmp`, syncs it, renames it over `path` and syncs the directory, so
//! a kill at any point leaves either the old file or the new one, never a
//! truncated one. [`load_state`] returns an empty state for a missing file and
//! an empty state plus the error for an unreadable or corrupt one.
//! Test: `a_save_killed_between_steps_leaves_a_state_that_parses`,
//! `state_survives_sigkill_mid_save`, `unknown_state_keys_round_trip`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
    /// Every other key, kept as read (wake counters, rc, usage, ...).
    #[serde(flatten)]
    pub other: Map<String, Value>,
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
    /// The bytes are in `<path>.tmp`, not yet synced.
    Written,
    /// `<path>.tmp` is synced, not yet renamed.
    Synced,
    /// `<path>.tmp` is renamed over `path`; the directory is not yet synced.
    Renamed,
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> StateError + '_ {
    move |source| StateError::Io {
        path: path.to_owned(),
        source,
    }
}

/// `<path>.tmp`, the prototype's temporary name.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Writes `state` to `path` atomically (tmp + fsync + rename + dir fsync).
///
/// # Errors
/// [`StateError::Io`] when any step fails; `path` then still holds the old
/// state.
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
    let tmp = tmp_path(path);
    let mut f = File::create(&tmp).map_err(io_err(&tmp))?;
    f.write_all(text.as_bytes()).map_err(io_err(&tmp))?;
    at(SaveStep::Written).map_err(io_err(&tmp))?;
    f.sync_all().map_err(io_err(&tmp))?;
    drop(f);
    at(SaveStep::Synced).map_err(io_err(&tmp))?;
    fs::rename(&tmp, path).map_err(io_err(path))?;
    at(SaveStep::Renamed).map_err(io_err(path))?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(io_err(dir))?;
    }
    Ok(())
}

/// Reads `path`. A missing file is an empty state and no error; an unreadable
/// or corrupt file is an empty state and the error, for the caller to log.
pub fn load_state(path: &Path) -> (PollState, Option<StateError>) {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (PollState::default(), None),
        Err(source) => {
            let err = StateError::Io {
                path: path.to_owned(),
                source,
            };
            return (PollState::default(), Some(err));
        }
    };
    match serde_json::from_str(&text) {
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
