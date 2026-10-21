//! The commit journal, request-id replay and `log.jsonl` notices (#9758).
//!
//! Why: a killed writer must leave a store the next writer can finish, and a
//! retried request must get its first answer back, not a second commit
//! (LDG-2.8.2, #9758 amendment).
//! What: [`complete`] appends a journal's lines where missing and replaces
//! `state.json`; [`recover`] finishes or drops a journal left behind;
//! [`replay`] reads a seen `request_id`'s result from `log.jsonl`;
//! [`append_notice`] logs one notice under the caller's lock.
//! Test: `tests/ledger_writer.rs`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use serde_json::Value;

use super::{CommitStep, Faults, Journal, NoFaults, RequestLine};
use crate::error::LedgerError;
use crate::ledger::model::{Extra, LogLine};
use crate::ledger::ops::Now;
use crate::ledger::rpc::BatchResponse;
use crate::ledger::store::{StorePaths, write_atomic};

/// Appends the journal's lines where they are missing, replaces
/// `state.json` and removes the journal.
pub(super) fn complete(
    paths: &StorePaths,
    j: &Journal,
    faults: &dyn Faults,
) -> Result<(), LedgerError> {
    redo(&paths.archive, j.archive_at, &j.archive)?;
    redo(&paths.log, j.log_at, &j.log)?;
    faults
        .at(CommitStep::Logged, paths)
        .map_err(LedgerError::io(&paths.state))?;
    write_atomic(&paths.state, &j.state)?;
    remove(&paths.journal)
}

/// Finishes a commit a killed writer left (LDG-2.8.2 replay): a whole
/// journal one `v` above `state.json` is completed; any other is dropped.
pub(super) fn recover(paths: &StorePaths) -> Result<(), LedgerError> {
    let Ok(text) = fs::read_to_string(&paths.journal) else {
        return Ok(());
    };
    let journal: Option<Journal> = serde_json::from_str(&text).ok();
    let current = fs::read_to_string(&paths.state)
        .ok()
        .and_then(|t| state_v(&t));
    match journal {
        Some(j) if current.is_some_and(|v| v + 1 == j.v) => complete(paths, &j, &NoFaults),
        _ => remove(&paths.journal),
    }
}

/// Appends `text` at offset `at` of `path` unless a previous attempt did.
pub(super) fn redo(path: &Path, at: u64, text: &str) -> Result<(), LedgerError> {
    if text.is_empty() {
        return Ok(());
    }
    let have = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(LedgerError::io(path)(e)),
    };
    let start = usize::try_from(at).unwrap_or(usize::MAX).min(have.len());
    let tail = &have[start..];
    if tail.starts_with(text.as_bytes()) {
        return Ok(());
    }
    let rest = text.as_bytes().strip_prefix(tail).ok_or_else(|| {
        LedgerError::Unavailable(format!(
            "{} does not match the commit journal; left for repair",
            path.display()
        ))
    })?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(LedgerError::io(path))?;
    f.write_all(rest)
        .and_then(|()| f.sync_all())
        .map_err(LedgerError::io(path))
}

/// The result of an earlier batch with `id`, from its `log.jsonl` line.
pub(super) fn replay(paths: &StorePaths, id: &str) -> Result<Option<BatchResponse>, LedgerError> {
    if id.is_empty() {
        return Ok(None);
    }
    let log = fs::read_to_string(&paths.log).unwrap_or_default();
    let line = log
        .lines()
        .rev()
        .filter(|l| l.contains("\"request\""))
        .find_map(|l| {
            serde_json::from_str::<RequestLine>(l)
                .ok()
                .filter(|r| r.kind == "request" && r.request_id == id)
        });
    Ok(line.map(|r| r.result))
}
pub(super) fn notice(
    now: &Now,
    session: &str,
    kind: &str,
    text: &str,
) -> Result<String, LedgerError> {
    let (ts, session, kind) = (now.minute.clone(), session.to_owned(), kind.to_owned());
    LogLine::Notice {
        ts,
        session,
        kind,
        n: None,
        text: Some(text.to_owned()),
        extra: Extra::new(),
    }
    .to_line()
}

/// Appends one notice line to `log.jsonl` (the caller holds the lock).
pub(super) fn append_notice(
    paths: &StorePaths,
    now: &Now,
    session: &str,
    kind: &str,
    text: &str,
) -> Result<(), LedgerError> {
    let line = notice(now, session, kind, text)? + "\n";
    redo(&paths.log, file_len(&paths.log), &line)
}

pub(super) fn state_v(text: &str) -> Option<u64> {
    serde_json::from_str::<Value>(text).ok()?.get("v")?.as_u64()
}

pub(super) fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

pub(super) fn remove(path: &Path) -> Result<(), LedgerError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(LedgerError::io(path)(e)),
        _ => Ok(()),
    }
}
