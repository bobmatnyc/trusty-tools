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
use std::path::{Path, PathBuf};

pub use crate::spec_drift::Outcome;

/// A finished run with these streams and this exit code.
pub(crate) fn outcome(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// The file at the root of an Architect checkout.
pub const CHECKOUT_MARKER: &str = "ARCHITECT.md";

/// The Architect checkout at or above `start`: the nearest directory holding
/// [`CHECKOUT_MARKER`].
///
/// Why: the scripts take the checkout from their own location
/// (`dirname(dirname(abspath(__file__)))`); a binary has no such location,
/// and the working directory alone let a run outside the checkout reuse ids
/// and write stray files (#12073).
/// What: walks `start`'s ancestors, `start` first; `None` when none holds
/// the marker. Callers refuse on `None`, never fall back to `start`.
/// Test: `shipped_brief_id_finds_the_checkout_from_a_foreign_directory`.
pub fn find_checkout(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join(CHECKOUT_MARKER).is_file())
        .map(Path::to_path_buf)
}

/// The checkout above the working directory, or the refusal to print.
///
/// Why: one lookup and one message for the three binaries (#12073).
/// What: `Ok((cwd, checkout))`, else an exit-1 [`Outcome`] naming `prog`
/// and `hint` (how to run without a checkout, if there is a way).
/// Test: the `*_finds_the_checkout_from_a_foreign_directory` tests.
pub fn checkout_from_cwd(prog: &str, hint: &str) -> Result<(PathBuf, PathBuf), Outcome> {
    let cwd = std::env::current_dir().ok();
    match cwd.as_deref().and_then(find_checkout) {
        Some(root) => Ok((cwd.unwrap_or_default(), root)),
        None => Err(no_checkout(prog, hint)),
    }
}

/// The exit-1 refusal for a run that needs a checkout and found none (#12073).
pub fn no_checkout(prog: &str, hint: &str) -> Outcome {
    outcome(
        String::new(),
        format!(
            "{prog}: no Architect checkout ({CHECKOUT_MARKER}) at or above the working directory{hint}\n"
        ),
        1,
    )
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
