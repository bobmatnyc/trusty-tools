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
/// `state.json` (directory fsynced) and only then removes the journal.
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
    faults
        .at(CommitStep::Replaced, paths)
        .map_err(LedgerError::io(&paths.journal))?;
    remove(&paths.journal)
}

/// `path`'s bytes, decoded as the prototype does (`decode('utf-8',
/// 'replace')`); `None` only when it does not exist (#9758: any other read
/// error refuses the batch instead of reading as empty).
/// Test: `ledger_writer_replays_a_request_id_past_a_non_utf8_log_line`,
/// `ledger_writer_refuses_when_the_archive_is_unreadable`.
pub(super) fn read_lossy(path: &Path) -> Result<Option<String>, LedgerError> {
    match fs::read(path) {
        Ok(b) => Ok(Some(String::from_utf8_lossy(&b).into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LedgerError::io(path)(e)),
    }
}

/// Finishes a commit a killed writer left (LDG-2.8.2 replay): a whole
/// journal one `v` above `state.json` is completed; any other is dropped.
/// An unreadable or non-UTF-8 journal is left for repair and refuses the
/// batch (#9758).
/// Test: `ledger_writer_leaves_a_non_utf8_journal_and_refuses`.
pub(super) fn recover(paths: &StorePaths) -> Result<(), LedgerError> {
    let bytes = match fs::read(&paths.journal) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(LedgerError::io(&paths.journal)(e)),
    };
    let text = String::from_utf8(bytes).map_err(|_| {
        LedgerError::Unavailable(format!(
            "{} is not UTF-8; left for repair",
            paths.journal.display()
        ))
    })?;
    let journal: Option<Journal> = serde_json::from_str(&text).ok();
    let Some(state) = read_lossy(&paths.state)? else {
        return Err(LedgerError::Invalid(
            "state.json missing beside a commit journal; run init".into(),
        ));
    };
    match journal {
        Some(j) if state_v(&state).is_some_and(|v| v + 1 == j.v) => complete(paths, &j, &NoFaults),
        _ => remove(&paths.journal),
    }
}

/// Appends `text` at offset `at` of `path` unless a previous attempt did.
///
/// #9758: the prototype appends `log.jsonl` without the lock, so foreign
/// lines can sit after `at`. `text` already present at a line start after
/// `at` is done; a torn prefix of it at the end is finished; otherwise the
/// whole of `text` is appended on a fresh line, so it lands exactly once.
/// Test: `ledger_writer_commits_past_a_foreign_log_append_at_staged`,
/// `ledger_writer_recovers_a_killed_commit_past_a_foreign_log_append`.
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
    let want = text.as_bytes();
    let landed = (0..tail.len())
        .filter(|&i| i == 0 || tail[i - 1] == b'\n')
        .any(|i| tail[i..].starts_with(want));
    if landed {
        return Ok(());
    }
    let mut rest = want.strip_prefix(tail).map(<[u8]>::to_vec);
    if rest.is_none() {
        let fresh = have.is_empty() || have.ends_with(b"\n");
        rest = Some([if fresh { &b""[..] } else { b"\n" }, want].concat());
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(LedgerError::io(path))?;
    f.write_all(&rest.unwrap_or_default())
        .and_then(|()| f.sync_all())
        .map_err(LedgerError::io(path))
}

/// Drops `archive.jsonl` lines whose `cv` is above `v`: orphans of a commit
/// that never replaced `state.json` (the prototype's `reconcile_archive`,
/// #9758). Other lines keep their bytes.
/// Test: `ledger_writer_drops_an_orphan_archive_line_before_reusing_its_cv`.
pub(super) fn reconcile_archive(path: &Path, v: u64) -> Result<(), LedgerError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(LedgerError::io(path)(e)),
    };
    let orphan = |line: &[u8]| {
        serde_json::from_slice::<Value>(line.trim_ascii_end())
            .ok()
            .and_then(|x| x.get("cv")?.as_u64())
            .is_some_and(|cv| cv > v)
    };
    let lines: Vec<&[u8]> = bytes.split_inclusive(|b| *b == b'\n').collect();
    if !lines.iter().any(|l| orphan(l)) {
        return Ok(());
    }
    let keep: Vec<u8> = lines
        .into_iter()
        .filter(|l| !orphan(l))
        .flatten()
        .copied()
        .collect();
    write_atomic(path, keep)
}

/// The result of an earlier batch with `id`, from its `log.jsonl` line.
pub(super) fn replay(paths: &StorePaths, id: &str) -> Result<Option<BatchResponse>, LedgerError> {
    if id.is_empty() {
        return Ok(None);
    }
    let log = read_lossy(&paths.log)?.unwrap_or_default();
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
