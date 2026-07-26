//! Append-only JSONL journal mechanics, shared by every OKG journal.
//!
//! Why: the OKG now keeps TWO append-only journals per source — the item
//! [`ledger`](crate::okg::ledger) (what was pulled into the tree) and the
//! [`index journal`](crate::okg::index_journal) (what was pushed into the bound
//! search index). Both need the same two hard-won behaviours, and duplicating
//! them would let the copies drift: a crash-torn final line must cost exactly
//! one record rather than bricking the file, and the first append after such a
//! tear must emit a healing newline or it silently concatenates into the torn
//! fragment and loses the NEW record too (the bug fixed by
//! `append_after_torn_line_does_not_corrupt_the_new_record`).
//!
//! What: [`read_journal`] parses a whole file BYTE-wise — decoding per line, so
//! a tear mid-UTF-8-codepoint damages only its own line — and reports how many
//! lines were unusable plus whether the file ends without a newline.
//! [`Appender`] owns the lazily-opened handle and performs the healing write.
//!
//! Test: the behaviours are pinned by the callers' own suites
//! (`torn_line_is_skipped_not_fatal`,
//! `torn_mid_utf8_codepoint_is_skipped_not_fatal`,
//! `append_after_torn_line_does_not_corrupt_the_new_record`, and
//! `index_journal::tests::torn_index_journal_line_is_skipped_not_fatal`).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The outcome of reading one journal file.
pub(crate) struct JournalLoad<T> {
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
/// mid-codepoint whenever the record carries non-ASCII text — which for this
/// crate is the common case (a Gmail subject with an em-dash, a Drive filename
/// in kana), not the exotic one.
/// What: splits on `b'\n'`, tolerates CRLF and blank lines, and parses each
/// line independently.
/// Test: see the module doc.
pub(crate) fn read_journal<T: DeserializeOwned>(path: &Path) -> anyhow::Result<JournalLoad<T>> {
    if !path.exists() {
        return Ok(JournalLoad::default());
    }
    let bytes = std::fs::read(path)?;
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
pub(crate) struct Appender {
    path: PathBuf,
    handle: Option<File>,
    needs_newline: bool,
}

impl Appender {
    /// Build an appender for `path`, healing a torn final line on first write.
    pub(crate) fn new(path: PathBuf, needs_newline: bool) -> Self {
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
    /// line, syncs best-effort.
    /// Test: see the module doc.
    pub(crate) fn append<T: Serialize>(&mut self, record: &T) -> anyhow::Result<()> {
        if self.handle.is_none() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.handle = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        let line = serde_json::to_string(record)?;
        let file = self
            .handle
            .as_mut()
            .expect("handle just opened above; None is unreachable");
        if self.needs_newline {
            writeln!(file)?;
            self.needs_newline = false;
        }
        writeln!(file, "{line}")?;
        file.flush()?;
        // Best-effort durability: a filesystem that cannot fsync must not fail
        // the run — the line is already flushed to the OS at this point.
        let _ = file.sync_data();
        Ok(())
    }
}
