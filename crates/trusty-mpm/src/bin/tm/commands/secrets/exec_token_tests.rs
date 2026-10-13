//! Unit tests for the token fd the exec stub hands the command (#9071 S10).
//!
//! Every file is under a `TempDir` or the stub's own private directory; no
//! server, home or Keychain is touched.

use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, MetadataExt};

use trusty_secrets::granted::GrantedClient;

use super::exec_stub::token_for_command;

/// A well-formed token whose text appears nowhere else.
const TOKEN: &str = "9072feedface9072feedface9072feedface9072feedface9072feedfacebeef";

/// The fd `token_for_command` returns, owned by the test.
fn stub_token_fd() -> OwnedFd {
    let fd = token_for_command(TOKEN).expect("the stub made a token fd");
    // SAFETY: `token_for_command` created this fd and gave up ownership of it.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// Why: #9071 S10 AC1 — two siblings hold descriptors on the one open file
/// the stub hands down (a fork leaves them exactly so; `dup` does the same)
/// and both read the token. Red on the S9 pipe: the first read drains it
/// and the second sibling gets `InvalidToken`.
/// Test: itself.
#[test]
fn stub_token_fd_serves_two_sibling_readers() {
    let fd = stub_token_fd();
    for sibling in 0..2 {
        let copy = fd.try_clone().expect("dup the token fd");
        let client = GrantedClient::from_token_fd(copy);
        assert!(client.is_ok(), "sibling {sibling}: {client:?}");
    }
}

/// Why: #9071 S10 AC5 — the command gets a read-only fd on an unlinked
/// regular file of exactly the token, owner-only, kept across exec. Red on
/// the S9 pipe, and on a writable fd a descendant could rewrite.
/// Test: itself.
#[test]
fn stub_token_fd_is_an_unlinked_read_only_file() {
    let file = File::from(stub_token_fd());
    let raw = file.as_raw_fd();
    // SAFETY: `fcntl` reads the flags of an fd this test owns.
    let status = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    assert!(
        status >= 0 && status & libc::O_ACCMODE == libc::O_RDONLY,
        "{status}"
    );
    // SAFETY: as above.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC == 0, "flags {flags}");
    let meta = file.metadata().expect("fstat");
    assert!(meta.file_type().is_file(), "not a regular file");
    assert_eq!(meta.nlink(), 0, "the token file still has a name");
    assert_eq!(meta.len(), 64);
    assert_eq!(meta.mode() & 0o077, 0, "mode {:o}", meta.mode());
    assert_eq!(meta.uid(), nix::unistd::geteuid().as_raw());
    let mut text = [0u8; 64];
    file.read_exact_at(&mut text, 0).expect("pread");
    assert_eq!(&text[..], TOKEN.as_bytes());
}
