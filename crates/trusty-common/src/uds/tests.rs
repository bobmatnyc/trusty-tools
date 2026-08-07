//! Coverage for the #5099 socket-permission contract.
//!
//! The mode assertions here are the regression proof: against the pre-fix
//! commit — where every bind site called `UnixListener::bind` bare — the
//! socket comes back at the umask-derived mode (`0755` under the common
//! `022` umask) and the directory at whatever `create_dir_all` produced.

use super::*;

use std::os::unix::fs::PermissionsExt;
use tokio::net::UnixStream;

/// Mode bits of `path`, masked to the permission nibbles.
fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("stat path under test")
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn scratch_socket_dir_from_uses_tmpdir_when_set() {
    // Why: on macOS the per-user `/var/folders/…/T/` must be honored, not
    // replaced by `/tmp`.
    let dir = scratch_socket_dir_from(Some("/var/folders/xy/T"), 501);
    assert_eq!(dir, Path::new("/var/folders/xy/T/trusty-501"));
}

#[test]
fn scratch_socket_dir_from_falls_back_to_tmp() {
    // Why: #5099's core exposure — an unset `TMPDIR` on Linux used to land the
    // socket directly in world-writable `/tmp`. The fallback must still
    // interpose a uid-keyed directory that this process can hold at 0700.
    for absent in [None, Some(""), Some("   ")] {
        let dir = scratch_socket_dir_from(absent, 1000);
        assert_eq!(
            dir,
            Path::new("/tmp/trusty-1000"),
            "TMPDIR={absent:?} must still get a uid-keyed subdirectory"
        );
    }
}

#[test]
fn scratch_socket_dir_is_uid_keyed() {
    // Why: two users on one host must never share a socket directory.
    let dir = scratch_socket_dir();
    let leaf = dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("scratch dir has a leaf name");
    assert_eq!(leaf, format!("trusty-{}", self_uid()));
}

#[test]
fn prepare_socket_dir_creates_at_0700() {
    // Why: the directory mode is what closes the bind-then-chmod window, so it
    // must be 0700 the moment the directory exists.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sockets");

    prepare_socket_dir(&dir).expect("prepare fresh socket dir");

    assert_eq!(
        mode_of(&dir),
        SOCKET_DIR_MODE,
        "fresh socket dir must be 0700"
    );
}

#[test]
fn prepare_socket_dir_creates_missing_ancestors() {
    // Why: callers pass a nested path (`~/.trusty-agents/sockets/`) whose
    // ancestors may not exist. Only the leaf is security-relevant.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("a").join("b").join("sockets");

    prepare_socket_dir(&dir).expect("prepare nested socket dir");

    assert_eq!(mode_of(&dir), SOCKET_DIR_MODE);
}

#[test]
fn prepare_socket_dir_narrows_a_wide_existing_dir() {
    // Why: the directory this replaces was created by `create_dir_all`, so on
    // every existing install it is already 0755. Upgrading must repair it
    // rather than trusting whatever is on disk.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sockets");
    std::fs::create_dir(&dir).expect("create wide dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
        .expect("widen dir to 0755");
    assert_eq!(mode_of(&dir), 0o755, "precondition: dir starts wide");

    prepare_socket_dir(&dir).expect("prepare pre-existing socket dir");

    assert_eq!(
        mode_of(&dir),
        SOCKET_DIR_MODE,
        "existing dir must be narrowed"
    );
}

#[test]
fn prepare_socket_dir_is_idempotent() {
    // Why: every daemon restart re-runs this; a second call must not fail on
    // the directory it created itself.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sockets");

    prepare_socket_dir(&dir).expect("first prepare");
    prepare_socket_dir(&dir).expect("second prepare must succeed");

    assert_eq!(mode_of(&dir), SOCKET_DIR_MODE);
}

#[tokio::test]
async fn bind_hardened_sets_socket_0600_and_dir_0700() {
    // Why: THE regression test for #5099. Against the pre-fix commit the
    // socket comes back at the umask default (0755 under umask 022) and this
    // assertion fails. ADR-0031/0032 cite the 0600 property; this is what
    // makes the citation true.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sockets");
    let sock = dir.join("t.sock");

    let _listener = bind_hardened(&sock).expect("bind hardened listener");

    assert_eq!(mode_of(&sock), SOCKET_MODE, "socket must be 0600");
    assert_eq!(mode_of(&dir), SOCKET_DIR_MODE, "socket dir must be 0700");
}

#[tokio::test]
async fn bind_hardened_socket_is_connectable_after_hardening() {
    // Why: narrowing to 0600 must not break the same-uid client the socket
    // exists for — a fix that made the socket unusable would also pass a
    // mode-only assertion.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("t.sock");
    let listener = bind_hardened(&sock).expect("bind hardened listener");

    let client = UnixStream::connect(&sock).await.expect("same-uid connect");
    let (accepted, _) = listener.accept().await.expect("accept");

    ensure_peer_is_self(&accepted).expect("same-uid peer must be accepted");
    drop(client);
}

#[tokio::test]
async fn bind_hardened_rejects_a_path_with_no_parent() {
    // Failure path: a bare relative filename has no directory to harden, so
    // the bind must be refused rather than silently binding unprotected.
    let err = bind_hardened(Path::new("bare.sock")).expect_err("must refuse");
    assert!(
        matches!(err, UdsSecurityError::NoParent { .. }),
        "expected NoParent, got {err:?}"
    );
}

#[tokio::test]
async fn bind_hardened_propagates_an_address_in_use_failure() {
    // Failure path: `bind_hardened` deliberately does NOT unlink a stale
    // socket (that would break `CtrlSocket::bind_singleton`'s probe-first
    // guarantee), so a second bind must surface EADDRINUSE, not swallow it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("t.sock");
    let _first = bind_hardened(&sock).expect("first bind");

    let err = bind_hardened(&sock).expect_err("second bind must fail");
    assert!(
        matches!(err, UdsSecurityError::Bind { .. }),
        "expected Bind, got {err:?}"
    );
}

#[tokio::test]
async fn peer_uid_of_self_connection_is_self() {
    // Why: proves the platform syscall is wired up correctly on this target —
    // a stub returning 0 would pass `ensure_peer_is_self` only for root.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("t.sock");
    let listener = bind_hardened(&sock).expect("bind");

    let _client = UnixStream::connect(&sock).await.expect("connect");
    let (accepted, _) = listener.accept().await.expect("accept");

    assert_eq!(
        peer_uid(&accepted).expect("read peer uid"),
        self_uid(),
        "a connection from this process must report this process's uid"
    );
}

/// Cross-uid refusal — the half of the peer check that cannot run unprivileged.
///
/// `#[ignore]`d because connecting as a second uid requires either root (to
/// `setuid`) or a pre-provisioned second account, and CI runs as one
/// unprivileged user. Run under a second uid with
/// `cargo test -p trusty-common -- --include-ignored ensure_peer_is_self_refuses_foreign_uid`.
/// This does NOT count as passing coverage for the refusal path; the refusal
/// branch is otherwise proven only by `ForeignPeer`'s construction being
/// unreachable for a same-uid peer in `peer_uid_of_self_connection_is_self`.
#[tokio::test]
#[ignore = "needs a second uid; unprivileged CI cannot connect as another user"]
async fn ensure_peer_is_self_refuses_foreign_uid() {
    let Ok(sock) = std::env::var("TRUSTY_UDS_FOREIGN_PEER_SOCK") else {
        eprintln!(
            "SKIPPED: set TRUSTY_UDS_FOREIGN_PEER_SOCK to a socket bound by another uid. \
             This run proves NOTHING about the foreign-uid refusal path."
        );
        return;
    };
    let stream = UnixStream::connect(&sock)
        .await
        .expect("connect as other uid");
    let err = ensure_peer_is_self(&stream).expect_err("foreign uid must be refused");
    assert!(
        matches!(err, UdsSecurityError::ForeignPeer { .. }),
        "expected ForeignPeer, got {err:?}"
    );
}

/// Foreign-owner refusal on the directory — also unprivileged-impossible.
///
/// `#[ignore]`d for the same reason: creating a directory owned by another uid
/// requires root. Not counted as passing coverage.
#[test]
#[ignore = "needs root to create a directory owned by another uid"]
fn prepare_socket_dir_rejects_foreign_owner() {
    let Ok(dir) = std::env::var("TRUSTY_UDS_FOREIGN_DIR") else {
        eprintln!(
            "SKIPPED: set TRUSTY_UDS_FOREIGN_DIR to a directory owned by another uid. \
             This run proves NOTHING about the foreign-owner refusal path."
        );
        return;
    };
    let err = prepare_socket_dir(Path::new(&dir)).expect_err("foreign owner must be refused");
    assert!(
        matches!(err, UdsSecurityError::ForeignDirOwner { .. }),
        "expected ForeignDirOwner, got {err:?}"
    );
}
