//! Coverage for the #5099 socket-permission contract.
//!
//! The mode assertions are the regression proof: against the pre-fix commit —
//! where every bind site called `UnixListener::bind` bare — the socket comes
//! back at the umask-derived mode (`0755` under the common `022` umask) and the
//! directory at whatever `create_dir_all` produced.
//!
//! Every refusal path is covered by a pure decision function
//! (`classify_existing_dir`, `peer_uid_verdict`), so foreign-uid and
//! foreign-owner policy is asserted without root and without a second account.
//! What remains genuinely unreachable unprivileged is only the platform syscall
//! plumbing that feeds those functions.

use super::dir::{DirVerdict, classify_existing_dir};
use super::peer::peer_uid_verdict;
use super::*;

use std::os::unix::fs::PermissionsExt;

/// Mode bits of `path`, masked to the permission nibbles. Uses `lstat` so a
/// symlink's own mode is reported, never its target's.
fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("stat path under test")
        .permissions()
        .mode()
        & 0o777
}

// ── path resolution ─────────────────────────────────────────────────────────

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

// ── sun_path budget (review finding 4) ──────────────────────────────────────

#[test]
fn sun_path_capacity_is_platform_plausible() {
    // Why: derived from struct layout, so a wrong answer would silently skew
    // every budget check. macOS is 104, Linux 108.
    let cap = sun_path_capacity();
    assert!(
        (104..=108).contains(&cap),
        "sun_path capacity {cap} is outside the known 104..=108 range"
    );
}

#[test]
fn check_sun_path_budget_accepts_a_path_that_fits() {
    let path = PathBuf::from("/tmp/trusty-501/short.sock");
    check_sun_path_budget(&path).expect("a short path must fit");
}

#[test]
fn check_sun_path_budget_rejects_an_over_long_path() {
    // Why: the pre-check exists to replace a bare `invalid argument` with a
    // message naming the budget and the overflow.
    let long = format!("/tmp/{}.sock", "x".repeat(sun_path_capacity()));
    let err = check_sun_path_budget(Path::new(&long)).expect_err("must reject");
    match err {
        UdsSecurityError::PathTooLong { len, capacity, .. } => {
            assert!(len >= capacity, "len {len} must exceed capacity {capacity}");
            let rendered = err.to_string();
            assert!(
                rendered.contains(&capacity.to_string()) && rendered.contains(&len.to_string()),
                "diagnostic must name both the budget and the actual length: {rendered}"
            );
        }
        other => panic!("expected PathTooLong, got {other:?}"),
    }
}

#[tokio::test]
async fn bind_hardened_rejects_an_over_long_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let long = tmp.path().join(format!("{}.sock", "x".repeat(120)));
    let err = bind_hardened(&long).expect_err("must reject before binding");
    assert!(
        matches!(err, UdsSecurityError::PathTooLong { .. }),
        "expected PathTooLong, got {err:?}"
    );
}

// ── directory decisions, as pure functions (review finding 5) ───────────────

#[test]
fn classify_existing_dir_rejects_a_symlink() {
    // Why: THE review-finding-1 policy. `metadata()` on a symlinked directory
    // reports the TARGET's owner and mode, so a link pointing at any directory
    // this uid happens to own would otherwise pass the owner check — and
    // `set_permissions` would then chmod the target.
    let err = classify_existing_dir(Path::new("/tmp/x"), true, 501, 0o700, 501)
        .expect_err("a symlink must be refused even when owner and mode look right");
    assert!(
        matches!(err, UdsSecurityError::SymlinkDir { .. }),
        "expected SymlinkDir, got {err:?}"
    );
}

#[test]
fn classify_existing_dir_rejects_a_foreign_owner() {
    let err = classify_existing_dir(Path::new("/tmp/x"), false, 0, 0o700, 501)
        .expect_err("a root-owned directory must be refused");
    match err {
        UdsSecurityError::ForeignDirOwner {
            owner, expected, ..
        } => {
            assert_eq!((owner, expected), (0, 501));
        }
        other => panic!("expected ForeignDirOwner, got {other:?}"),
    }
}

#[test]
fn classify_existing_dir_narrows_a_wide_dir() {
    let v = classify_existing_dir(Path::new("/tmp/x"), false, 501, 0o755, 501).expect("ours");
    assert_eq!(v, DirVerdict::Narrow);
}

#[test]
fn classify_existing_dir_accepts_an_already_correct_dir() {
    let v = classify_existing_dir(Path::new("/tmp/x"), false, 501, 0o700, 501).expect("ours");
    assert_eq!(v, DirVerdict::Accept);
}

#[test]
fn classify_existing_dir_checks_symlink_before_owner() {
    // Why: ordering matters. An attacker-owned symlink must report SymlinkDir,
    // not ForeignDirOwner — the latter would imply the path itself was checked.
    let err = classify_existing_dir(Path::new("/tmp/x"), true, 999, 0o777, 501).expect_err("no");
    assert!(
        matches!(err, UdsSecurityError::SymlinkDir { .. }),
        "symlink must be rejected before ownership is considered, got {err:?}"
    );
}

// ── directory behaviour on a real filesystem ────────────────────────────────

#[test]
fn prepare_socket_dir_creates_at_0700() {
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("a").join("b").join("sockets");

    prepare_socket_dir(&dir).expect("prepare nested socket dir");

    assert_eq!(mode_of(&dir), SOCKET_DIR_MODE);
}

#[test]
fn prepare_socket_dir_narrows_a_wide_existing_dir() {
    // Why: the directory this replaces was created by `create_dir_all`, so on
    // every existing install it is already 0755. Upgrading must repair it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sockets");
    std::fs::create_dir(&dir).expect("create wide dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("widen");
    assert_eq!(mode_of(&dir), 0o755, "precondition: dir starts wide");

    prepare_socket_dir(&dir).expect("prepare pre-existing socket dir");

    assert_eq!(
        mode_of(&dir),
        SOCKET_DIR_MODE,
        "existing dir must be narrowed"
    );
}

#[test]
fn prepare_socket_dir_rejects_a_symlink() {
    // Why: the end-to-end version of review finding 1, on a real filesystem.
    // The link points at a directory THIS uid owns, which is exactly the case
    // the old `metadata()`-based check waved through. Asserts the target is
    // left untouched — the old code chmod'd it to 0700.
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("attacker_controlled");
    std::fs::create_dir(&target).expect("create target");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o777)).expect("widen");
    let link = tmp.path().join("sockets");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");

    let err = prepare_socket_dir(&link).expect_err("a symlinked socket dir must be refused");

    assert!(
        matches!(err, UdsSecurityError::SymlinkDir { .. }),
        "expected SymlinkDir, got {err:?}"
    );
    assert_eq!(
        mode_of(&target),
        0o777,
        "the symlink target must NOT have been chmod'd"
    );
}

#[test]
fn prepare_socket_dir_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sockets");

    prepare_socket_dir(&dir).expect("first prepare");
    prepare_socket_dir(&dir).expect("second prepare must succeed");

    assert_eq!(mode_of(&dir), SOCKET_DIR_MODE);
}

// ── bind ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn bind_hardened_sets_socket_0600_and_dir_0700() {
    // Why: THE regression test for #5099. Against the pre-fix commit the socket
    // comes back at the umask default (0755 under umask 022) and this assertion
    // fails.
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
    // mode-only assertion. Also proves the accept-side peer check admits a
    // legitimate peer.
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
    // Failure path: a bare relative filename has no directory to harden, so the
    // bind must be refused rather than silently binding unprotected.
    let err = bind_hardened(Path::new("bare.sock")).expect_err("must refuse");
    assert!(
        matches!(err, UdsSecurityError::NoParent { .. }),
        "expected NoParent, got {err:?}"
    );
}

#[tokio::test]
async fn bind_hardened_propagates_an_address_in_use_failure() {
    // Failure path: `bind_hardened` deliberately does NOT unlink a stale socket
    // (that would break `CtrlSocket::bind_singleton`'s probe-first guarantee),
    // so a second bind must surface EADDRINUSE, not swallow it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("t.sock");
    let _first = bind_hardened(&sock).expect("first bind");

    let err = bind_hardened(&sock).expect_err("second bind must fail");
    assert!(
        matches!(err, UdsSecurityError::Bind { .. }),
        "expected Bind, got {err:?}"
    );
}

// ── connect (review finding 3) ──────────────────────────────────────────────

#[tokio::test]
async fn connect_hardened_accepts_a_properly_hardened_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("t.sock");
    let listener = bind_hardened(&sock).expect("bind");

    let client = connect_hardened(&sock).await.expect("dial our own socket");
    let (accepted, _) = listener.accept().await.expect("accept");
    ensure_peer_is_self(&accepted).expect("peer is us");
    drop(client);
}

#[tokio::test]
async fn connect_hardened_refuses_a_world_readable_socket() {
    // Why: a daemon that predates #5099 still answers at the canonical path.
    // The client must refuse it rather than trusting the transport.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("t.sock");
    let _listener = bind_hardened(&sock).expect("bind");
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o777)).expect("widen socket");

    let err = connect_hardened(&sock).await.expect_err("must refuse");
    match err {
        UdsSecurityError::UntrustedSocket { ref reason, .. } => {
            assert!(
                reason.contains("0777"),
                "reason must name the mode: {reason}"
            );
        }
        other => panic!("expected UntrustedSocket, got {other:?}"),
    }
}

#[tokio::test]
async fn connect_hardened_refuses_a_socket_in_a_wide_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sockets");
    let sock = dir.join("t.sock");
    let _listener = bind_hardened(&sock).expect("bind");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("widen dir");

    let err = connect_hardened(&sock).await.expect_err("must refuse");
    match err {
        UdsSecurityError::UntrustedSocket { ref reason, .. } => {
            assert!(
                reason.contains("directory") && reason.contains("0755"),
                "reason must name the directory and its mode: {reason}"
            );
        }
        other => panic!("expected UntrustedSocket, got {other:?}"),
    }
}

#[tokio::test]
async fn connect_hardened_refuses_a_regular_file() {
    // Why: an attacker who cannot bind can still plant a regular file. Dialling
    // it would fail with a confusing ENOTSOCK deep in the client.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sockets");
    prepare_socket_dir(&dir).expect("prepare");
    let planted = dir.join("t.sock");
    std::fs::write(&planted, b"not a socket").expect("plant file");
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    let err = connect_hardened(&planted).await.expect_err("must refuse");
    match err {
        UdsSecurityError::UntrustedSocket { ref reason, .. } => {
            assert!(reason.contains("not a socket"), "got: {reason}");
        }
        other => panic!("expected UntrustedSocket, got {other:?}"),
    }
}

#[test]
fn verify_socket_for_connect_refuses_a_symlinked_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("elsewhere");
    std::fs::create_dir(&target).expect("create target");
    let link = tmp.path().join("sockets");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");

    let err = verify_socket_for_connect(&link.join("t.sock")).expect_err("must refuse");
    assert!(
        matches!(err, UdsSecurityError::SymlinkDir { .. }),
        "expected SymlinkDir, got {err:?}"
    );
}

// ── peer decisions, as pure functions (review finding 5) ────────────────────

#[test]
fn peer_uid_verdict_accepts_the_same_uid() {
    peer_uid_verdict(501, 501).expect("a same-uid peer must be admitted");
}

#[test]
fn peer_uid_verdict_refuses_a_foreign_uid() {
    // Why: this is the refusal that previously had NO unprivileged coverage —
    // the `#[ignore]`d cross-uid test could not run in CI, so the policy was
    // asserted nowhere.
    let err = peer_uid_verdict(999, 501).expect_err("a foreign uid must be refused");
    match err {
        UdsSecurityError::ForeignPeer { peer, expected } => {
            assert_eq!((peer, expected), (999, 501));
        }
        other => panic!("expected ForeignPeer, got {other:?}"),
    }
}

#[test]
fn peer_uid_verdict_refuses_root_when_we_are_not_root() {
    // Why: root is deliberately not special-cased. Admitting it would widen the
    // accepted set for no gain, since root bypasses the filesystem check anyway.
    let err = peer_uid_verdict(0, 501).expect_err("root must be refused like any foreign uid");
    assert!(matches!(err, UdsSecurityError::ForeignPeer { peer: 0, .. }));
}

#[tokio::test]
async fn peer_uid_of_self_connection_is_self() {
    // Why: proves the platform syscall is wired up correctly on this target — a
    // stub returning 0 would pass `peer_uid_verdict` only for root.
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
