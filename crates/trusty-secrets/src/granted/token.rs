//! The grant token: which fd holds it, reading it without blocking, and
//! handing it on to child processes (#9071).
//!
//! Why: `tm secrets exec` puts the token on a pipe and its fd number in
//! `TRUSTY_SECRETS_TOKEN_FD` (Architect ruling Q1). Reading drains the pipe,
//! and DOC-74 §10.5 says the command's descendants share its grant, so the
//! reader puts the token back on the same fd number, as tm's exec stub does
//! for the command (Architect ruling D3).
//! What: [`parse_token_fd`] (a decimal fd above stderr), [`take_inherited`]
//! (read an inherited fd, then put the token back on it) and [`read_owned`]
//! (read an fd the caller owns). A read checks the fd is a pipe and polls
//! before every read, so a pipe whose writer has sent nothing never blocks.
//! It reads to EOF, or stops once a whole token is in and nothing more
//! waits: a writer copy a concurrent fork holds until its exec keeps EOF
//! away. It reads at most 256 bytes; only 64 lowercase hex digits are a
//! token.
//!
//! Limit (#9071): the grant reaches the reading process and its own
//! descendants, a linear chain. A re-arm is a `dup2` in the reading process
//! only; siblings share the parent's original pipe, so the first reader
//! drains it for all of them. A sibling of an earlier reader, or a second
//! child of the same parent, finds the pipe drained and fails closed with
//! [`GrantedError::InvalidToken`]; no value is exposed. Example: in
//! `sh -c 'migrate && serve'`, `serve` cannot resolve once `migrate` has.
//!
//! Test: `parse_token_fd_refuses_unset_garbage_and_stdio`,
//! `granted_client_checks_the_token_before_dialing`,
//! `granted_client_token_fd_errors_never_block`,
//! `granted_client_rearms_the_token_for_a_child`,
//! `granted_client_sibling_finds_the_pipe_drained`.

use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::FileTypeExt;

use super::GrantedError;

/// A grant token is 32 random bytes as lowercase hex (`server::grant`).
const TOKEN_LEN: usize = 64;

/// The most bytes read from the token fd before it is refused.
const MAX_TOKEN_READ: usize = 256;

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

/// Read the token from the inherited `fd`, then put it back on `fd`.
///
/// What: reads through a close-on-exec duplicate, so `fd` itself is never
/// owned here; only a well-formed token is put back. The new pipe replaces
/// `fd` atomically (`dup2`) and is not close-on-exec, so a child spawned
/// afterwards inherits it.
/// Test: `granted_client_rearms_the_token_for_a_child`,
/// `granted_client_token_fd_errors_never_block`.
pub(crate) fn take_inherited(fd: RawFd) -> Result<Token, GrantedError> {
    let pipe = duplicate(fd).map_err(GrantedError::TokenUnreadable)?;
    let token = read_token(File::from(pipe))?;
    rearm(fd, &token).map_err(GrantedError::Rearm)?;
    Ok(token)
}

/// Read the token from `fd`, which closes when this returns.
///
/// Test: `granted_client_checks_the_token_before_dialing`.
pub(crate) fn read_owned(fd: OwnedFd) -> Result<Token, GrantedError> {
    if fd.as_raw_fd() <= 2 {
        return Err(GrantedError::NoGrant);
    }
    read_token(File::from(fd))
}

/// A close-on-exec duplicate of `fd`; fails when `fd` is not open.
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

/// The token on `pipe`, read to EOF without blocking.
fn read_token(mut pipe: File) -> Result<Token, GrantedError> {
    let kind = pipe
        .metadata()
        .map_err(GrantedError::TokenUnreadable)?
        .file_type();
    if !kind.is_fifo() {
        return Err(GrantedError::TokenUnreadable(io::Error::new(
            ErrorKind::InvalidInput,
            "the token fd is not a pipe",
        )));
    }
    let mut text = Vec::with_capacity(TOKEN_LEN);
    let mut buf = [0u8; MAX_TOKEN_READ + 1];
    loop {
        if !readable(&pipe).map_err(GrantedError::TokenUnreadable)? {
            // #9071: a whole token and an open writer is the token.
            if text.len() == TOKEN_LEN {
                break;
            }
            return Err(GrantedError::TokenUnreadable(io::Error::new(
                ErrorKind::WouldBlock,
                "no grant token is waiting on the fd",
            )));
        }
        let n = match pipe.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(GrantedError::TokenUnreadable(e)),
        };
        if n == 0 {
            break;
        }
        text.extend_from_slice(&buf[..n]);
        if text.len() > MAX_TOKEN_READ {
            return Err(GrantedError::InvalidToken);
        }
    }
    let text = String::from_utf8(text).map_err(|_| GrantedError::InvalidToken)?;
    if is_token(&text) {
        Ok(Token(text))
    } else {
        Err(GrantedError::InvalidToken)
    }
}

/// Whether `text` is 64 lowercase hex digits (tm's `exec_wire::is_token`).
fn is_token(text: &str) -> bool {
    text.len() == TOKEN_LEN
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether a read of `pipe` returns at once: data waits, or the writer is
/// gone (EOF). `false` when the writer is open and nothing waits.
fn readable(pipe: &File) -> io::Result<bool> {
    let mut poll = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: one valid `pollfd`, count 1, timeout 0; `poll` writes only
        // its `revents`.
        let ready = unsafe { libc::poll(&mut poll, 1, 0) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            return Ok(false);
        }
        break;
    }
    if poll.revents & libc::POLLNVAL != 0 {
        return Err(io::Error::from_raw_os_error(libc::EBADF));
    }
    if poll.revents & (libc::POLLIN | libc::POLLHUP) == 0 {
        return Err(io::Error::other("the token fd reported an error"));
    }
    Ok(true)
}

/// Put `token` on a fresh pipe and move its read end onto `fd`.
///
/// What: the token is 64 bytes, under the pipe buffer, so the write never
/// blocks; the write end closes before `dup2`, so a reader sees EOF after
/// the token. `dup2` clears close-on-exec on `fd`.
fn rearm(fd: RawFd, token: &Token) -> io::Result<()> {
    let (reader, mut writer) = io::pipe()?;
    writer.write_all(token.expose().as_bytes())?;
    drop(writer);
    // SAFETY: both descriptors are open: `reader` is owned here and `fd` was
    // read just before. `dup2` closes the old `fd` and reuses its number in
    // one step; `reader`'s own number closes when it drops.
    if unsafe { libc::dup2(reader.as_raw_fd(), fd) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
