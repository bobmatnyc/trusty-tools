//! Shared file and seal helpers for the presence anchor (#8878 PR2b-1a).
//!
//! Why: grants, device records and pending requests all live in
//! `~/.trusty-mpm/architect-launch/` and all need the same fail-closed read
//! (no symlink, a 0600 regular file of this user's, a size bound) and the
//! same HMAC message layout. One copy keeps the three readers identical.
//! What: [`read_owner_only`] reads one file under checked directories;
//! [`sealed_message`] builds a length-prefixed, tag-separated message for
//! [`crate::core::presence_key::GrantKey::seal`]; [`random_hex`] reads fresh
//! random bytes; [`valid_token`] bounds an id used as a file name.
//! Test: `presence_store_tests.rs`.

use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

/// Why a file in the anchor could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FileFault {
    /// The file (or one of its directories) does not exist.
    #[error("the file does not exist")]
    Missing,
    /// A directory or the file could not be read, or a directory is a symlink.
    #[error("the file could not be read")]
    Unreadable,
    /// The file is a symlink or not a regular file.
    #[error("the file is a symlink or not a regular file")]
    NotRegularFile,
    /// The file is readable by others or owned by another user.
    #[error("the file is not a 0600 file of this user")]
    UnsafeMode,
    /// The file is larger than the bound.
    #[error("the file is larger than allowed")]
    Oversized,
}

/// The bytes of `file`, after every directory in `dirs` was checked to be a
/// real directory (not a symlink).
///
/// Why/What: see the module doc. The file is opened with `O_NOFOLLOW`, must
/// be a regular file of this user's with no group or other bits, and at most
/// `max` bytes long. A missing directory or file is [`FileFault::Missing`].
/// Test: `an_owner_only_read_refuses_links_modes_and_size`.
pub fn read_owner_only(dirs: &[&Path], file: &Path, max: u64) -> Result<Vec<u8>, FileFault> {
    for dir in dirs {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if meta.is_dir() => {}
            // A symlinked or non-directory directory is never followed.
            Ok(_) => return Err(FileFault::Unreadable),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(FileFault::Missing),
            Err(_) => return Err(FileFault::Unreadable),
        }
    }
    let handle = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(file)
    {
        Ok(handle) => handle,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(FileFault::Missing),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(FileFault::NotRegularFile),
        Err(_) => return Err(FileFault::Unreadable),
    };
    let meta = handle.metadata().map_err(|_| FileFault::Unreadable)?;
    if !meta.is_file() {
        return Err(FileFault::NotRegularFile);
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if meta.mode() & 0o077 != 0 || meta.uid() != euid {
        return Err(FileFault::UnsafeMode);
    }
    let mut raw = Vec::new();
    handle
        .take(max + 1)
        .read_to_end(&mut raw)
        .map_err(|_| FileFault::Unreadable)?;
    if raw.len() as u64 > max {
        return Err(FileFault::Oversized);
    }
    Ok(raw)
}

/// `tag`, a NUL, then each field as `<byte length>:<bytes>`.
///
/// Why: a length prefix keeps two field lists from encoding to the same
/// bytes, whatever the fields contain; the tag separates record kinds.
/// Test: `sealed_messages_are_unambiguous`.
pub fn sealed_message(tag: &str, fields: &[&str]) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(tag.len() + 1 + fields.iter().map(|f| f.len() + 8).sum::<usize>());
    out.extend_from_slice(tag.as_bytes());
    out.push(0);
    for field in fields {
        out.extend_from_slice(field.len().to_string().as_bytes());
        out.push(b':');
        out.extend_from_slice(field.as_bytes());
    }
    out
}

/// `N` fresh random bytes from `/dev/urandom`.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("no random bytes: {e}"))?;
    Ok(bytes)
}

/// `N` fresh random bytes, hex.
pub fn random_hex<const N: usize>() -> Result<String, String> {
    random_bytes::<N>().map(hex::encode)
}

/// Whether `token` is safe as a file-name stem: 1 to `max` ASCII letters,
/// digits, `-` or `_` (base64url and hex both qualify).
///
/// Test: `tokens_with_path_characters_are_refused`.
pub fn valid_token(token: &str, max: usize) -> bool {
    !token.is_empty()
        && token.len() <= max
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
#[path = "presence_store_tests.rs"]
mod tests;
