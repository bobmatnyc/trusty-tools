//! The trusty-console token file: one loader for the console and its clients
//! (#11074, DOC-90 §4).
//!
//! Why: on a cloud sled any caller that sent a matching `Host` reached every
//! console route. Owner ruling D1b puts a token on every TCP route. The
//! console (which enforces it) and each in-repo TCP client (which sends it)
//! must agree on where the token lives and what counts as a valid file; a
//! second spelling of either is the defect the common-entry-point rule
//! forbids, so both read it through this module.
//! What: [`load_from_env`] reads [`TOKEN_FILE_ENV`]. Unset is `Ok(None)`, the
//! token is off. Set, the file must exist, be a regular file, carry no group
//! or other permission bit, and hold [`MIN_TOKEN_BYTES`] to
//! [`MAX_TOKEN_BYTES`] visible ASCII bytes after trailing whitespace is
//! trimmed; anything else is a [`TokenFileError`] that names the path and the
//! problem, never a byte of the file. [`ConsoleToken`] prints redacted and
//! compares in constant time ([`crate::daemon_token::credentials_match`]).
//! The token never comes from an environment value or a CLI argument: the
//! variable names a path, never the token.
//! Test: `console_token_tests::*`.
//!
//! # Spec References
//! - [`SPEC-SLEDNET-04~draft`](../../../docs/specs/DOC-90-cloud-sled-network-boundary.md#SPEC-SLEDNET-04~draft)

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The variable that names the console token file and turns the token on.
pub const TOKEN_FILE_ENV: &str = "TRUSTY_CONSOLE_TOKEN_FILE";

/// The shortest token the console accepts, in bytes (DOC-90 TOK-3).
///
/// The same floor as [`crate::daemon_token::MIN_TOKEN_LEN`], which
/// [`ConsoleToken::matches`] relies on: that comparison refuses anything
/// shorter outright.
pub const MIN_TOKEN_BYTES: usize = crate::daemon_token::MIN_TOKEN_LEN;

/// The longest token file read, in bytes; a larger file is refused unread.
///
/// Why: the file is read whole into memory before any check, so a bound keeps
/// a misconfigured path (a log, a device) from being slurped. 4 KiB is far
/// past any generated token.
pub const MAX_TOKEN_BYTES: usize = 4096;

/// Why a token file was refused.
///
/// Every variant names the path and the problem only. None carries file
/// content, so printing one (an `anyhow` chain, a log line) cannot leak it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenFileError {
    /// The variable is set to an empty value.
    #[error("TRUSTY_CONSOLE_TOKEN_FILE is set but empty; name the token file or unset it")]
    EmptyVariable,
    /// Nothing exists at the path.
    #[error("console token file {}: does not exist", path.display())]
    Missing {
        /// The configured path.
        path: PathBuf,
    },
    /// The path exists but is not a regular file.
    #[error("console token file {}: not a regular file", path.display())]
    NotAFile {
        /// The configured path.
        path: PathBuf,
    },
    /// The file carries a group or other permission bit.
    #[error(
        "console token file {}: mode {:04o} grants group or other access; use 0400 or 0600",
        path.display(),
        mode
    )]
    GroupOrOtherAccess {
        /// The configured path.
        path: PathBuf,
        /// The file's permission bits.
        mode: u32,
    },
    /// The file could not be opened or read.
    #[error("console token file {}: cannot be read ({kind})", path.display())]
    Unreadable {
        /// The configured path.
        path: PathBuf,
        /// The I/O error kind; never the file content.
        kind: std::io::ErrorKind,
    },
    /// The trimmed token is shorter than [`MIN_TOKEN_BYTES`].
    #[error("console token file {}: token is shorter than 32 bytes", path.display())]
    TooShort {
        /// The configured path.
        path: PathBuf,
    },
    /// The file is longer than [`MAX_TOKEN_BYTES`].
    #[error("console token file {}: file is longer than 4096 bytes", path.display())]
    TooLong {
        /// The configured path.
        path: PathBuf,
    },
    /// The token holds a byte outside visible ASCII (space, control, non-ASCII).
    #[error("console token file {}: token holds a byte outside visible ASCII", path.display())]
    NotVisibleAscii {
        /// The configured path.
        path: PathBuf,
    },
    /// This platform cannot read Unix permission bits, so the file cannot be
    /// checked.
    #[error("console token file {}: permission bits cannot be checked on this platform", path.display())]
    UnsupportedPlatform {
        /// The configured path.
        path: PathBuf,
    },
}

/// A validated console token.
///
/// Why: the value must never reach a log, a panic message or an error body,
/// so the type has no `Display` and a redacted `Debug`; the only way to the
/// bytes is [`ConsoleToken::expose`], which a reader can grep for.
pub struct ConsoleToken(String);

impl std::fmt::Debug for ConsoleToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConsoleToken(<redacted>)")
    }
}

impl ConsoleToken {
    /// Whether `presented` is this token, compared in constant time.
    ///
    /// Test: `matches_only_the_exact_token`.
    pub fn matches(&self, presented: &str) -> bool {
        crate::daemon_token::credentials_match(&self.0, presented)
    }

    /// The token itself, for a client building its `Authorization` header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Load the console token named by [`TOKEN_FILE_ENV`].
///
/// Why: the one entry point the console (fail closed at start) and its
/// clients (send the header) both call.
/// What: [`load_from_var`] over the process environment.
/// Test: `load_from_var_unset_is_off` and the per-variant tests, which drive
/// [`load_from_var`] so no test mutates the process environment.
pub fn load_from_env() -> Result<Option<ConsoleToken>, TokenFileError> {
    load_from_var(std::env::var_os(TOKEN_FILE_ENV))
}

/// [`load_from_env`] with the variable's value injected.
///
/// What: `None` → `Ok(None)`; an empty value → [`TokenFileError::EmptyVariable`];
/// otherwise [`load_from_path`].
/// Test: `load_from_var_unset_is_off`, `load_from_var_empty_is_refused`.
pub fn load_from_var(value: Option<OsString>) -> Result<Option<ConsoleToken>, TokenFileError> {
    match value {
        None => Ok(None),
        Some(v) if v.is_empty() => Err(TokenFileError::EmptyVariable),
        Some(v) => load_from_path(Path::new(&v)).map(Some),
    }
}

/// Read and validate the token file at `path`.
///
/// Why: DOC-90 TOK-3 — the console must exit before it binds on a missing,
/// short or group/other-readable file, so every check runs here, before the
/// caller can serve.
/// What: `stat` (following a symlink, as a Kubernetes Secret volume uses),
/// then the permission bits, then a bounded read, then trailing whitespace is
/// trimmed and the length and byte set are checked.
/// Test: `missing_file_is_refused`, `directory_is_refused`,
/// `group_readable_file_is_refused`, `other_readable_file_is_refused`,
/// `short_token_is_refused`, `empty_file_is_refused`,
/// `long_file_is_refused`, `inner_space_is_refused`,
/// `valid_file_loads_with_its_newline_trimmed`.
pub fn load_from_path(path: &Path) -> Result<ConsoleToken, TokenFileError> {
    let owned = || path.to_path_buf();
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => TokenFileError::Missing { path: owned() },
        kind => TokenFileError::Unreadable {
            path: owned(),
            kind,
        },
    })?;
    if !meta.is_file() {
        return Err(TokenFileError::NotAFile { path: owned() });
    }
    check_mode(path, &meta)?;
    let unreadable = |e: std::io::Error| TokenFileError::Unreadable {
        path: owned(),
        kind: e.kind(),
    };
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let mut raw = Vec::with_capacity(128);
    // One byte past the cap tells "exactly at the cap" from "longer".
    file.take(MAX_TOKEN_BYTES as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(unreadable)?;
    if raw.len() > MAX_TOKEN_BYTES {
        return Err(TokenFileError::TooLong { path: owned() });
    }
    let token = raw.trim_ascii_end();
    if token.len() < MIN_TOKEN_BYTES {
        return Err(TokenFileError::TooShort { path: owned() });
    }
    if !token.iter().all(|b| b.is_ascii_graphic()) {
        return Err(TokenFileError::NotVisibleAscii { path: owned() });
    }
    // Visible ASCII is valid UTF-8, so this never takes the error arm.
    let token = String::from_utf8(token.to_vec())
        .map_err(|_| TokenFileError::NotVisibleAscii { path: owned() })?;
    Ok(ConsoleToken(token))
}

/// Refuse any group or other permission bit (DOC-90 TOK-2: `0400` or `0600`).
#[cfg(unix)]
fn check_mode(path: &Path, meta: &std::fs::Metadata) -> Result<(), TokenFileError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(TokenFileError::GroupOrOtherAccess {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

/// No permission bits to read here, so the file cannot be trusted: fail closed.
#[cfg(not(unix))]
fn check_mode(path: &Path, _meta: &std::fs::Metadata) -> Result<(), TokenFileError> {
    Err(TokenFileError::UnsupportedPlatform {
        path: path.to_path_buf(),
    })
}

#[cfg(all(test, unix))]
#[path = "console_token_tests.rs"]
mod console_token_tests;
