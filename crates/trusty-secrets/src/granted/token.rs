//! The grant token: which fd holds it, and reading it without consuming it
//! (#9071).
//!
//! Why: `tm secrets exec` hands the token down on an inherited fd and puts
//! only its number in `TRUSTY_SECRETS_TOKEN_FD` (Architect ruling Q1). The
//! fd is a read-only, unlinked regular file holding exactly the token (S10
//! redesign, Option B). A read at offset 0 consumes nothing, so the command,
//! its siblings and its descendants each read the same token, and none of
//! them can rewrite it for the others.
//! What: [`parse_token_fd`] (a decimal fd above stderr), [`read_inherited`]
//! (read an fd this process does not own) and [`read_owned`] (an fd the
//! caller owns). A read refuses an fd that is not open read-only, not a
//! regular file, still named by a path (`st_nlink > 0`) or not exactly 64
//! bytes, then reads with `pread` at offset 0: the shared file offset
//! neither matters nor moves. Only 64 lowercase hex digits are a token.
//! Nothing here changes an fd's flags.
//!
//! Test: `parse_token_fd_refuses_unset_garbage_and_stdio`,
//! `granted_client_checks_the_token_before_dialing`,
//! `granted_client_refuses_a_token_fd_of_the_wrong_shape`,
//! `granted_client_reads_the_token_at_any_offset`,
//! `granted_client_siblings_both_resolve`,
//! `granted_client_read_leaves_the_fd_for_a_child`,
//! `granted_token_read_refuses_a_short_or_long_file`.

use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};

use super::GrantedError;

/// A grant token is 32 random bytes as lowercase hex (`server::grant`).
const TOKEN_LEN: usize = 64;

/// The grant token; `Debug` redacts it.
pub(crate) struct Token(String);

impl Token {
    /// The token text, for the request frame only.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The fd number `TRUSTY_SECRETS_TOKEN_FD` names.
///
/// What: ASCII digits only (no sign, no space), parsed as an fd above 2;
/// anything else, or an unset variable, is [`GrantedError::NoGrant`].
/// Test: `parse_token_fd_refuses_unset_garbage_and_stdio`.
pub(crate) fn parse_token_fd(raw: Option<&OsStr>) -> Result<RawFd, GrantedError> {
    let text = raw.and_then(OsStr::to_str).ok_or(GrantedError::NoGrant)?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(GrantedError::NoGrant);
    }
    match text.parse::<RawFd>() {
        Ok(fd) if fd > 2 => Ok(fd),
        _ => Err(GrantedError::NoGrant),
    }
}

/// Read the token from the inherited `fd`, which stays open and unchanged.
///
/// What: reads through a close-on-exec duplicate, so `fd` itself is never
/// owned or closed here; a duplicate shares `fd`'s open file, and `pread`
/// moves no offset, so nothing changes for a later reader of `fd`.
/// Test: `granted_client_siblings_both_resolve`,
/// `granted_client_reads_the_token_at_any_offset`.
pub(crate) fn read_inherited(fd: RawFd) -> Result<Token, GrantedError> {
    let file = File::from(duplicate(fd).map_err(GrantedError::TokenUnreadable)?);
    read_token(&file)
}

/// Read the token from `fd`, which closes when this returns.
///
/// Test: `granted_client_checks_the_token_before_dialing`.
pub(crate) fn read_owned(fd: OwnedFd) -> Result<Token, GrantedError> {
    if fd.as_raw_fd() <= 2 {
        return Err(GrantedError::NoGrant);
    }
    read_token(&File::from(fd))
}

/// A close-on-exec duplicate of `fd`; fails when `fd` is not open.
///
/// Why: IO safety — an inherited fd number may name nothing, and an
/// `OwnedFd` must never be built on a closed fd. The duplicate is only read.
fn duplicate(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `F_DUPFD_CLOEXEC` reads no caller memory; a descriptor that is
    // not open fails with EBADF and creates nothing.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `copy` is a descriptor this call just created; nothing else
    // owns it, so it is closed exactly once.
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}

/// The token in `file`, after checking `file` is the shape tm hands down.
fn read_token(file: &File) -> Result<Token, GrantedError> {
    check_shape(file)?;
    read_at_start(file)
}

/// Refuse anything but a read-only fd on an unlinked, 64-byte regular file.
///
/// What: the fd's access mode, then `fstat`: a pipe, socket, directory,
/// device, named file or writable fd is [`GrantedError::TokenUnreadable`];
/// a size other than 64 is [`GrantedError::InvalidToken`].
pub(super) fn check_shape(file: &File) -> Result<(), GrantedError> {
    // SAFETY: `F_GETFL` reads the status flags of an fd this process holds.
    let status = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if status < 0 {
        return Err(GrantedError::TokenUnreadable(io::Error::last_os_error()));
    }
    if status & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(refuse("the token fd is not read-only"));
    }
    let meta = file.metadata().map_err(GrantedError::TokenUnreadable)?;
    if !meta.file_type().is_file() {
        return Err(refuse("the token fd is not a regular file"));
    }
    // #9071 S10: a named file is reachable by path, so it is not tm's.
    if meta.nlink() != 0 {
        return Err(refuse("the token file still has a name"));
    }
    if meta.len() != TOKEN_LEN as u64 {
        return Err(GrantedError::InvalidToken);
    }
    Ok(())
}

/// [`GrantedError::TokenUnreadable`] with fixed text.
fn refuse(reason: &'static str) -> GrantedError {
    GrantedError::TokenUnreadable(io::Error::new(ErrorKind::InvalidInput, reason))
}

/// The token at offset 0 of `file`, by `pread`; exactly 64 bytes, then EOF.
///
/// What: reads up to 65 bytes, looping on short reads; fewer or more than
/// 64 is [`GrantedError::InvalidToken`], never a truncated or padded token.
/// Test: `granted_token_read_refuses_a_short_or_long_file`.
pub(super) fn read_at_start(file: &File) -> Result<Token, GrantedError> {
    let mut buf = [0u8; TOKEN_LEN + 1];
    let mut filled = 0;
    while filled < buf.len() {
        match file.read_at(&mut buf[filled..], filled as u64) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(GrantedError::TokenUnreadable(e)),
        }
    }
    if filled != TOKEN_LEN {
        return Err(GrantedError::InvalidToken);
    }
    match std::str::from_utf8(&buf[..TOKEN_LEN]) {
        Ok(text) if is_token(text) => Ok(Token(text.to_owned())),
        _ => Err(GrantedError::InvalidToken),
    }
}

/// Whether `text` is 64 lowercase hex digits (tm's `exec_wire::is_token`).
fn is_token(text: &str) -> bool {
    text.len() == TOKEN_LEN
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
