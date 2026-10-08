//! The gchat state directory, `<project>/.trusty-channels/state/`.
//!
//! Why: learned DM spaces, the question ledger and the audit log are runtime
//! state, never config, so they live apart from `routes.toml` (#9448
//! ruling 4).
//! What: [`spaces`] (`gchat-spaces.json`), [`ledger`] (`questions.jsonl`),
//! [`audit`] (`audit.jsonl`), the single-writer lock ([`lock_dir`],
//! `gchat.lock`), plus the shared path and append helpers.
//! Test: `src/gchat/tests/ledger.rs`, `src/gchat/tests/inbound.rs`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::gchat::error::StateError;
use crate::gchat::routes::CONFIG_DIR;

pub mod audit;
pub mod ledger;
pub mod spaces;

/// The state directory name inside [`CONFIG_DIR`].
pub const STATE_DIR: &str = "state";
/// Route → DM space bindings.
pub const SPACES_FILE: &str = "gchat-spaces.json";
/// The question ledger.
pub const QUESTIONS_FILE: &str = "questions.jsonl";
/// Refused sends and dropped inbound messages.
pub const AUDIT_FILE: &str = "audit.jsonl";
/// The single-writer lock file.
pub const LOCK_FILE: &str = "gchat.lock";

/// `<project_dir>/.trusty-channels/state`.
pub fn state_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(CONFIG_DIR).join(STATE_DIR)
}

/// Create the state directory if needed, with a `.gitignore` of `*` so
/// answers and audit lines are never committed with the project.
pub(crate) fn ensure_dir(dir: &Path) -> Result<(), StateError> {
    std::fs::create_dir_all(dir).map_err(|e| StateError::io(dir, &e))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, "*\n").map_err(|e| StateError::io(&ignore, &e))?;
    }
    Ok(())
}

/// An exclusive advisory lock on one state directory; dropping it unlocks.
#[derive(Debug)]
pub(crate) struct StateLock {
    _file: std::fs::File,
}

/// Take the state directory's single-writer lock without waiting.
///
/// Why: the ledger allocates ids from memory, inbound acks a reply it cannot
/// bind, and `learn` rewrites the spaces file from its own copy, so two
/// writers on one directory reuse ids, lose answers and erase bindings
/// (#9448 review).
/// What: opens (creating) `<dir>/gchat.lock` and takes an exclusive
/// `flock`-style lock with `File::try_lock`. A held lock returns
/// [`StateError::Locked`]; the lock lives as long as the returned guard.
/// Test: `second_open_on_one_state_dir_is_refused_until_the_first_drops`.
pub(crate) fn lock_dir(dir: &Path) -> Result<StateLock, StateError> {
    let path = dir.join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| StateError::io(&path, &e))?;
    match file.try_lock() {
        Ok(()) => Ok(StateLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(StateError::Locked { path }),
        Err(std::fs::TryLockError::Error(e)) => Err(StateError::io(&path, &e)),
    }
}

/// Append one JSON line to `path`, creating the file.
///
/// Why: the ledger and audit log are append-only JSONL; one `write_all` of a
/// whole line keeps a concurrent reader from seeing half a record.
/// What: serializes `value`, appends `\n`, writes once, then fsyncs.
/// Test: `ledger_survives_restart`.
pub(crate) fn append_line<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), StateError> {
    let mut line = serde_json::to_vec(value).map_err(|e| StateError::Corrupt {
        path: path.to_path_buf(),
        line: 0,
        reason: format!("cannot encode record: {e}"),
    })?;
    line.push(b'\n');
    append_bytes(path, &line)
}

/// Append `bytes` to `path` with one `write_all`, creating the file, then
/// fsync.
pub(crate) fn append_bytes(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| StateError::io(path, &e))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_data())
        .map_err(|e| StateError::io(path, &e))
}

/// Write `bytes` to `path` through a temp file, an fsync and a rename, so a
/// crash leaves either the old file or the whole new one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| StateError::io(&tmp, &e))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| StateError::io(&tmp, &e))?;
    std::fs::rename(&tmp, path).map_err(|e| StateError::io(path, &e))
}

/// A content-free description of a JSON decode failure: a serde message can
/// quote its input, and state lines may hold answer text.
pub(crate) fn json_error_class(e: &serde_json::Error) -> &'static str {
    match e.classify() {
        serde_json::error::Category::Io => "io error",
        serde_json::error::Category::Syntax => "invalid JSON",
        serde_json::error::Category::Data => "unexpected JSON shape",
        serde_json::error::Category::Eof => "truncated JSON",
    }
}

/// The current UTC time, RFC 3339 to the second.
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
