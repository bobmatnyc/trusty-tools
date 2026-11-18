//! Fail-closed JSONL audit log, one line per broker call.
//!
//! Why: every call that reaches, or is refused before, the driver must leave
//! a record, and a call whose record cannot be written must not run (spike
//! `audit.py`, plan section 4 steps 5 and 9).
//! What: [`Audit::write`] appends one key-sorted JSON object per line with a
//! leading `ts`, to an owner-only 0600 file, and fsyncs it; the file rotates
//! at 5 MiB keeping `.1` and `.2` (shape from trusty-browser `audit.rs`).
//! [`summarize_args`] logs argument keys and text lengths, never a value.
//! Test: `tests/spike_audit.rs`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::private;

/// Argument keys whose string length is logged.
pub const TEXT_KEYS: [&str; 2] = ["text", "value"];
/// Rotate once the file reaches this size.
pub const ROTATE_BYTES: u64 = 5 * 1024 * 1024;
/// Rotated generations kept (`audit.jsonl.1` .. `.KEEP`).
pub const KEEP: usize = 2;

/// The audit line was not written; the caller must refuse the call.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    /// The log exists but is not an owner-only regular file.
    #[error("audit log must be an owner-only regular file")]
    NotPrivate,
    /// The write failed.
    #[error("audit write failed: {0}")]
    Io(#[from] io::Error),
}

/// Keys plus text lengths; never a value.
///
/// Why: typed text and screen content must stay out of the log.
/// What: `{"keys": sorted keys}` plus `<key>_len` (code points) for each
/// string `text` / `value` argument.
/// Test: `audit_line_per_call`.
pub fn summarize_args(args: &Map<String, Value>) -> Value {
    let mut keys: Vec<&String> = args.keys().collect();
    keys.sort();
    let mut out = Map::new();
    out.insert(
        "keys".to_owned(),
        Value::from(keys.into_iter().cloned().collect::<Vec<_>>()),
    );
    for key in TEXT_KEYS {
        if let Some(Value::String(s)) = args.get(key) {
            out.insert(format!("{key}_len"), Value::from(s.chars().count()));
        }
    }
    sorted(Value::Object(out))
}

/// `value` with every object's keys in sorted order (`sort_keys=True`).
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// Seconds since the epoch, rounded to 3 places as the spike's `ts`.
fn now() -> f64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    (secs * 1000.0).round() / 1000.0
}

/// The audit log at one path.
#[derive(Debug, Clone)]
pub struct Audit {
    path: PathBuf,
}

impl Audit {
    /// A log at `path`; nothing is created until the first write.
    pub fn new(path: impl Into<PathBuf>) -> Audit {
        Audit { path: path.into() }
    }

    /// The log path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The line [`Audit::write`] appends for `record` at `ts`, without newline.
    ///
    /// Why: callers and tests need the exact bytes without a write.
    /// What: `{"ts": ts, ..record}` with keys sorted at every depth; a `ts`
    /// in `record` wins, as in the spike's `{"ts": ..., **record}`.
    /// Test: `audit_line_is_sorted_and_carries_ts`.
    pub fn line(record: &Map<String, Value>, ts: f64) -> String {
        let mut full = Map::new();
        full.insert("ts".to_owned(), Value::from(ts));
        for (k, v) in record {
            full.insert(k.clone(), v.clone());
        }
        sorted(Value::Object(full)).to_string()
    }

    /// Append one record; an error means it was not recorded (fail closed).
    ///
    /// Why: a call whose audit line is lost must not reach the driver.
    /// What: rotates first, opens `O_APPEND|O_NOFOLLOW` 0600, refuses a
    /// non-private file, writes the line in one call and fsyncs.
    /// Test: `audit_write_failure_refuses_call`, `audit_refuses_a_shared_log`.
    pub fn write(&self, record: &Map<String, Value>) -> Result<(), AuditError> {
        let mut line = Audit::line(record, now());
        line.push('\n');
        self.rotate()?;
        let mut file = private::open_nofollow(&self.path, true)?;
        if !private::private_file(&file.metadata()?) {
            return Err(AuditError::NotPrivate);
        }
        file.write_all(line.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    /// `audit.jsonl` -> `.1` -> `.2` once the live file reaches [`ROTATE_BYTES`].
    ///
    /// Why: the log is append-only and must not grow without bound.
    /// What: a missing file is not an error; the oldest generation is replaced.
    /// Test: `audit_rotates_at_the_size_limit`.
    pub fn rotate(&self) -> io::Result<()> {
        self.rotate_at(ROTATE_BYTES)
    }

    /// [`Audit::rotate`] with an explicit size threshold.
    pub fn rotate_at(&self, limit: u64) -> io::Result<()> {
        let size = match fs::symlink_metadata(&self.path) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if size < limit {
            return Ok(());
        }
        for index in (2..=KEEP).rev() {
            let older = self.generation(index - 1);
            if older.exists() {
                fs::rename(&older, self.generation(index))?;
            }
        }
        fs::rename(&self.path, self.generation(1))
    }

    /// The path of rotated generation `index`.
    pub fn generation(&self, index: usize) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }
}
