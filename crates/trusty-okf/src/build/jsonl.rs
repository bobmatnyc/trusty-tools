//! Append-only JSONL journal mechanics, shared by every build journal.
//!
//! Why: a source keeps TWO append-only journals — the item
//! [`ledger`](crate::build::ledger) (what was pulled into the tree) and the
//! index journal (what was pushed into the bound search index; since slice
//! S5 of #10746, `state::index`, outside the bundle). Both need the same two hard-won
//! behaviours, and duplicating them would let the copies drift: a crash-torn
//! final line must cost exactly one record rather than bricking the file, and
//! the first append after such a tear must emit a healing newline or it
//! silently concatenates into the torn fragment and loses the NEW record too
//! (the bug fixed by `append_after_torn_line_does_not_corrupt_the_new_record`).
//!
//! What: [`read_journal`] parses a whole file BYTE-wise — decoding per line, so
//! a tear mid-UTF-8-codepoint damages only its own line — and reports how many
//! lines were unusable plus whether the file ends without a newline.
//! [`Appender`] owns the lazily-opened handle and performs the healing write.
//! #10746: public, because trusty-agents' in-tree index journal
//! (`kb::okg::index_journal`, moved from trusty-kb in S11) shares it; errors
//! are [`OkfError`]. The on-disk format is unchanged.
//!
//! Test: the behaviours are pinned by the callers' own suites
//! (`torn_line_is_skipped_not_fatal`,
//! `torn_mid_utf8_codepoint_is_skipped_not_fatal` and
//! `append_after_torn_line_does_not_corrupt_the_new_record`; trusty-agents'
//! index-journal suite covers its own use).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::OkfError;
use crate::bundle::paths::io_err;

/// The outcome of reading one journal file.
pub struct JournalLoad<T> {
    /// Every record that parsed, in file order.
    pub records: Vec<T>,
    /// Lines that could not be used (bad JSON or invalid UTF-8).
    pub malformed: usize,
    /// The file's last line lacks a terminating newline — the signature of a
    /// write interrupted mid-line. The next append must heal it.
    pub needs_newline: bool,
}

impl<T> Default for JournalLoad<T> {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            malformed: 0,
            needs_newline: false,
        }
    }
}

/// Read and parse an append-only JSONL journal, tolerating damage.
///
/// Why: an absent journal is a first run, not an error, and a damaged line must
/// be SKIPPED rather than abort the load — one torn write from a crash would
/// otherwise permanently brick the source.
///
/// The parse is deliberately BYTE-oriented. Reading the file as a `String`
/// first fails the WHOLE load on invalid UTF-8, and a crash mid-write lands
/// mid-codepoint whenever the record carries non-ASCII text — a mail subject
/// with an em-dash, a filename in kana — the common case, not the exotic one.
/// What: splits on `b'\n'`, tolerates CRLF and blank lines, and parses each
/// line independently.
/// Test: see the module doc.
pub fn read_journal<T: DeserializeOwned>(path: &Path) -> Result<JournalLoad<T>, OkfError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(JournalLoad::default()),
        Err(e) => return Err(io_err(path, e)),
    };
    let mut out = JournalLoad {
        needs_newline: !bytes.is_empty() && bytes.last() != Some(&b'\n'),
        ..JournalLoad::default()
    };
    for line in bytes.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        // `from_slice` handles the UTF-8 check itself, so an invalid-UTF-8 line
        // lands in the same `malformed` bucket as bad JSON.
        match serde_json::from_slice::<T>(line) {
            Ok(record) => out.records.push(record),
            Err(_) => out.malformed += 1,
        }
    }
    Ok(out)
}

/// A lazily-opened, per-item-durable appender for one journal file.
pub struct Appender {
    path: PathBuf,
    handle: Option<File>,
    needs_newline: bool,
}

impl Appender {
    /// Build an appender for `path`, healing a torn final line on first write.
    pub fn new(path: PathBuf, needs_newline: bool) -> Self {
        Self {
            path,
            handle: None,
            needs_newline,
        }
    }

    /// Durably append one record as a JSON line.
    ///
    /// Why: per-item durability is what makes a crashed run converge — the line
    /// is flushed AND `sync_data`'d before returning, so a process killed
    /// immediately after has lost nothing it already reported.
    /// What: opens (once) in append mode, heals a torn final line, writes one
    /// line, syncs best-effort. A record that does not serialise is
    /// [`OkfError::Parse`]; a file error is [`OkfError::Io`].
    /// Test: see the module doc.
    pub fn append<T: Serialize>(&mut self, record: &T) -> Result<(), OkfError> {
        let line = serde_json::to_string(record).map_err(|e| OkfError::Parse {
            path: self.path.clone(),
            line: None,
            message: format!("journal record does not serialise: {e}"),
        })?;
        let path = self.path.clone();
        let io = |e: std::io::Error| io_err(&path, e);
        if self.handle.is_none() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
            }
            self.handle = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .map_err(io)?,
            );
        }
        let Some(file) = self.handle.as_mut() else {
            return Err(io(std::io::Error::other("journal handle not open")));
        };
        if self.needs_newline {
            writeln!(file).map_err(io)?;
            self.needs_newline = false;
        }
        writeln!(file, "{line}").map_err(io)?;
        file.flush().map_err(io)?;
        // Best-effort durability: a filesystem that cannot fsync must not fail
        // the run — the line is already flushed to the OS at this point.
        let _ = file.sync_data();
        Ok(())
    }
}
