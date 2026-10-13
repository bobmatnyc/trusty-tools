//! Unit tests for the token fd the exec stub hands the command and the sweep
//! of stale token directories (`exec_token.rs`, #9071 S10).
//!
//! Every file is under a `TempDir` or the stub's own private directory; no
//! server, home or Keychain is touched.

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use nix::sys::stat::{UtimensatFlags, utimensat};
use nix::sys::time::TimeSpec;
use tempfile::TempDir;
use trusty_secrets::granted::GrantedClient;

use super::exec_stub::token_for_command;
use super::exec_token::{STEPS, sweep, token_file};

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

/// The names left in `dir`.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Why: #9071 S10 AC5 — after `token_file` returns, its private directory
/// is gone (the base holds nothing), the file is exactly 0600, and the fd is
/// read-only: a write through it fails, so a descendant cannot rewrite the
/// token for its siblings.
/// Test: itself.
#[test]
fn token_file_leaves_no_path_and_a_read_only_fd() {
    let base = TempDir::new().expect("base");
    let fd = token_file(base.path(), TOKEN, None).expect("token file");
    assert_eq!(names(base.path()), Vec::<String>::new(), "a path was left");
    let mut file = File::from(fd);
    let meta = file.metadata().expect("fstat");
    assert_eq!(meta.nlink(), 0);
    assert_eq!(meta.mode() & 0o7777, 0o600, "mode {:o}", meta.mode());
    assert!(file.write_all(b"x").is_err(), "the fd is writable");
    let mut text = [0u8; 64];
    file.read_exact_at(&mut text, 0).expect("pread");
    assert_eq!(&text[..], TOKEN.as_bytes());
}

/// Why: #9071 S10 AC6 and the Fail-Open table — each step of `token_file`
/// that fails (mkdir, either chmod, create, reopen, unlink, write, the
/// directory's removal, clearing close-on-exec) is an error, never an fd,
/// and leaves no directory or token file behind, the unlink case included.
/// Red when a step's error is dropped, or a failure skips the cleanup.
/// Test: itself.
#[test]
fn token_file_refuses_on_every_failed_step() {
    for step in STEPS {
        let base = TempDir::new().expect("base");
        let error = token_file(base.path(), TOKEN, Some(step)).expect_err("a failed step");
        assert!(!error.to_string().contains(TOKEN), "{step:?}: {error}");
        assert_eq!(
            names(base.path()),
            Vec::<String>::new(),
            "{step:?} left a path"
        );
    }
    // A base that does not exist fails at mkdir, for real.
    let base = TempDir::new().expect("base");
    let error = token_file(&base.path().join("absent"), TOKEN, None);
    assert!(error.is_err(), "no base, yet an fd");
}

/// Set `path`'s modification time `ago` in the past, never following a link.
fn backdate(path: &Path, ago: Duration) {
    let at = SystemTime::now() - ago;
    let secs = at.duration_since(SystemTime::UNIX_EPOCH).expect("epoch");
    let secs = i64::try_from(secs.as_secs()).expect("secs");
    let spec = TimeSpec::new(secs, 0);
    utimensat(None, path, &spec, &spec, UtimensatFlags::NoFollowSymlink).expect("utimensat");
}

/// A `0700` directory `name` in `base`, holding a `token` file.
fn grant_dir(base: &Path, name: &str, mode: u32) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir(&dir).expect("mkdir");
    std::fs::write(dir.join("token"), b"dead").expect("token");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
    dir
}

/// Why: #9071 S10 AC11, decision 11 — the sweep removes a `tm-grant-*`
/// directory of this user, mode 0700, older than the TTL, and leaves a
/// young one, another name, another mode, a plain file, another owner's,
/// and a symlink to an old directory (neither the link nor its target is
/// touched). Red when the sweep follows a link or removes a young dir.
/// Test: itself.
#[test]
fn sweep_removes_only_stale_grant_dirs_of_this_user() {
    let uid = nix::unistd::geteuid().as_raw();
    let ttl = Duration::from_secs(3600);
    let old = ttl + Duration::from_secs(60);
    let base = TempDir::new().expect("base");
    let elsewhere = TempDir::new().expect("elsewhere");
    let target = grant_dir(elsewhere.path(), "tm-grant-target", 0o700);
    backdate(&target, old);
    let link = base.path().join("tm-grant-link");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    backdate(&link, old);
    for (name, mode, age) in [
        ("tm-grant-stale", 0o700, old),
        ("tm-grant-young", 0o700, Duration::from_secs(60)),
        ("other-stale", 0o700, old),
        ("tm-grant-wide", 0o755, old),
    ] {
        let dir = grant_dir(base.path(), name, mode);
        backdate(&dir, age);
    }
    let file = base.path().join("tm-grant-file");
    std::fs::write(&file, b"dead").expect("file");
    backdate(&file, old);

    assert_eq!(
        sweep(base.path(), uid + 1, ttl),
        0,
        "another owner's dir went"
    );
    assert_eq!(sweep(base.path(), uid, ttl), 1);
    let left = [
        "other-stale",
        "tm-grant-file",
        "tm-grant-link",
        "tm-grant-wide",
        "tm-grant-young",
    ];
    assert_eq!(names(base.path()), left);
    assert!(target.join("token").exists(), "the sweep followed the link");
    assert_eq!(sweep(&base.path().join("absent"), uid, ttl), 0);
}
