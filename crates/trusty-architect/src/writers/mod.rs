//! Ports of the Architect's state-writing scripts (#12028 P1e).
//!
//! Why: `state-archive.py`, `brief-id.py` (with `brief_ids.py`) and
//! `aliases.py` rewrite or append to files the Architect reads back, so a
//! port has to write the same bytes the same way, and fail where the script
//! fails, before the cutover can switch a caller.
//! What: [`state_archive`], [`brief_ids`] and [`aliases`], each a library
//! function returning an [`Outcome`], plus the file helpers they share:
//! [`read_text`] (Python's `open(encoding="utf-8").read()`, universal
//! newlines included) and [`os_error`] (the last line of the traceback an
//! `OSError` ends the script with).
//! Test: `tests/replay_writers.rs`, `tests/writers_smoke.rs`.

pub mod aliases;
pub mod brief_ids;
pub mod state_archive;

use std::io;
use std::path::Path;

pub use crate::spec_drift::Outcome;

/// A finished run with these streams and this exit code.
pub(crate) fn outcome(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// Python's `OSError` text for `e` on `path`: `[Errno N] <message>: '<path>'`.
pub(crate) fn errno_text(e: &io::Error, path: &str) -> String {
    let msg = e.to_string();
    let msg = msg.split(" (os error").next().unwrap_or(&msg).to_owned();
    match e.raw_os_error() {
        Some(n) => format!("[Errno {n}] {msg}: {}", crate::pystr::py_repr(path)),
        None => msg,
    }
}

/// The traceback's last line for an `OSError` `e` raised on `path`.
pub(crate) fn os_error(e: &io::Error, path: &str) -> String {
    let class = match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EACCES | libc::EPERM) => "PermissionError",
        Some(libc::EEXIST) => "FileExistsError",
        _ => "OSError",
    };
    format!("{class}: {}", errno_text(e, path))
}

/// Why a text file could not be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadError {
    /// The open or read failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The bytes are not UTF-8 (`UnicodeDecodeError` in the script).
    #[error("UnicodeDecodeError: 'utf-8' codec can't decode the file")]
    NotUtf8,
}

impl ReadError {
    /// The traceback's last line the script prints for this error.
    pub(crate) fn last_line(&self, path: &str) -> String {
        match self {
            ReadError::Io(e) => os_error(e, path),
            ReadError::NotUtf8 => self.to_string(),
        }
    }
}

/// `open(path, encoding="utf-8").read()`: UTF-8, `\r\n` and a lone `\r`
/// read as `\n` (universal newlines).
pub(crate) fn read_text(path: &Path) -> Result<String, ReadError> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8(bytes).map_err(|_| ReadError::NotUtf8)?;
    Ok(universal_newlines(text))
}

/// `\r\n` and a lone `\r` become `\n`, as a text-mode read translates them.
pub(crate) fn universal_newlines(text: String) -> String {
    if !text.contains('\r') {
        return text;
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}
