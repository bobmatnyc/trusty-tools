//! Tests for the `palace locks` row and the maintenance-lease probe (#8751).
//!
//! Why: a running daemon's `maintenance.lock` was reported as a stale lock to
//! delete. These pin the three holder states and the fail-closed error arm.
//! What: builds a registry root in a tempdir and drives
//! [`palace_locks_verdict`] with the real probe or a stand-in one.
//! Test: this IS the test module.

use std::io;
use std::path::Path;

use trusty_common::memory_core::maintenance_lease::MAINTENANCE_LOCK_FILE;

use super::super::CheckStatus;
use super::{classify_lease, palace_locks_verdict, pid_liveness, LeaseHolder};

/// Write `content` as the lease file under `root`.
fn seed_lease(root: &Path, content: &str) {
    std::fs::write(root.join(MAINTENANCE_LOCK_FILE), content).expect("seed lease");
}

fn detail(result: &super::CheckResult) -> &str {
    result.detail.as_deref().unwrap_or_default()
}

/// Why (#8751): the daemon holds this lease for its lifetime; doctor told the
/// operator to delete it. Fails against the pre-fix scan, which listed every
/// `*.lock` as removable.
/// What: a lease recording THIS process's pid, probed for real; asserts a pass
/// that names the live holder and carries no removal hint.
/// Test: itself.
#[test]
fn a_live_holder_lease_is_not_stale() {
    let root = tempfile::tempdir().expect("tempdir");
    seed_lease(root.path(), &format!("{}\n", std::process::id()));

    let result = palace_locks_verdict("palace locks".into(), root.path(), pid_liveness);

    assert_eq!(result.status, CheckStatus::Pass, "{result:?}");
    assert!(
        detail(&result).contains("held by running pid")
            && !detail(&result).contains("can be removed"),
        "a live lease must never be offered for removal: {}",
        detail(&result)
    );
}

/// Why (#8751): a lease whose holder exited is a leftover file, and may still
/// be reported as removable.
/// What: a lease recording pid 4242 with a probe that reports it not running;
/// asserts a warning listing the lease with the removal hint.
/// Test: itself.
#[test]
fn a_dead_holder_lease_is_stale() {
    let root = tempfile::tempdir().expect("tempdir");
    seed_lease(root.path(), "4242\n");

    let result = palace_locks_verdict("palace locks".into(), root.path(), |_| Ok(false));

    assert_eq!(result.status, CheckStatus::Warn, "{result:?}");
    let text = detail(&result);
    assert!(
        text.contains(MAINTENANCE_LOCK_FILE)
            && text.contains("can be removed")
            && text.contains("pid 4242 is not running"),
        "a dead-holder lease is stale: {text}"
    );
}

/// Why (#8751, the error arm): a lease that cannot be read, holds no pid, or
/// whose probe errors has told doctor nothing. Reading any of those as stale
/// would advise deleting a lease a live daemon may hold.
/// What: one row per failure — a directory named like the lease (unreadable),
/// an empty file, a non-numeric file, pid 0, and a probe error; each must be
/// `Unknown`, say "cannot determine", and carry no removal hint.
/// Test: itself.
#[test]
fn an_unreadable_or_unparseable_lease_is_undetermined() {
    let probe_error = |_: u32| -> io::Result<bool> { Err(io::Error::other("probe exploded")) };
    let rows: [(&str, Option<&str>, bool); 5] = [
        ("unreadable", None, false),
        ("empty", Some(""), false),
        ("garbage", Some("not-a-pid"), false),
        ("pid zero", Some("0"), false),
        ("probe error", Some("4242"), true),
    ];
    for (name, content, erroring_probe) in rows {
        let root = tempfile::tempdir().expect("tempdir");
        match content {
            Some(c) => seed_lease(root.path(), c),
            None => std::fs::create_dir(root.path().join(MAINTENANCE_LOCK_FILE)).expect("dir"),
        }
        let result = if erroring_probe {
            palace_locks_verdict("palace locks".into(), root.path(), probe_error)
        } else {
            palace_locks_verdict("palace locks".into(), root.path(), |_| Ok(false))
        };
        assert_eq!(result.status, CheckStatus::Unknown, "{name}: {result:?}");
        let text = detail(&result);
        assert!(
            text.contains("cannot determine") && !text.contains("can be removed"),
            "{name}: an unprobed lease must fail closed: {text}"
        );
    }
}

/// Why (#8751): excluding the live lease must not hide a real crash leftover
/// in a palace directory.
/// What: a live lease beside `<palace>/kg.redb.lock`; asserts a warning that
/// lists the sidecar and not the lease.
/// Test: itself.
#[test]
fn a_stray_lock_beside_a_live_lease_still_warns() {
    let root = tempfile::tempdir().expect("tempdir");
    seed_lease(root.path(), "4242");
    let palace = root.path().join("palace_a");
    std::fs::create_dir(&palace).expect("palace dir");
    std::fs::write(palace.join("kg.redb.lock"), b"").expect("sidecar");

    let result = palace_locks_verdict("palace locks".into(), root.path(), |_| Ok(true));

    assert_eq!(result.status, CheckStatus::Warn, "{result:?}");
    let text = detail(&result);
    assert!(
        text.starts_with("1 lock file(s) found:") && text.contains("kg.redb.lock"),
        "{text}"
    );
    assert!(text.contains("held by running pid 4242"), "{text}");
    assert_eq!(
        classify_lease(&root.path().join(MAINTENANCE_LOCK_FILE), |_| Ok(true)),
        LeaseHolder::Live(4242)
    );
}
