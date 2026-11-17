//! Tests for presence grants (`presence.rs`) and their key (`presence_key.rs`), #8878 PR2a.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use super::*;
use crate::core::presence_key::{GrantKey, KeySource};

const NOW: u64 = 1_800_000_000;
const ARCHITECT: ClaudeProcess = ClaudeProcess {
    pid: 4242,
    start_time: 1_799_990_000,
};

fn key() -> GrantKey {
    GrantKey::from_bytes([7; 32])
}

fn root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(".trusty-mpm");
    std::fs::create_dir_all(root.join(ARCHITECT_DIR)).expect("mkdir anchor");
    (dir, root)
}

fn check_now(root: &Path, claude: &ClaudeProcess, now: u64) -> Result<Grant, PresenceRefusal> {
    check(root, claude, || Some(key()), now)
}

/// Every grant file under `root`, with its bytes: a refusal changes none.
fn grants(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(root.join(ARCHITECT_DIR))
        .expect("read anchor")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == PRESENCE_EXT))
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap_or_default();
            (p, bytes)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn a_minted_grant_passes_the_check() {
    let (_d, root) = root();
    let path = mint(&root, &ARCHITECT, METHOD_LOCAL, &key(), NOW).expect("mint");
    assert_eq!(path, grant_path(&root, ARCHITECT.pid));
    let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let grant = check_now(&root, &ARCHITECT, NOW + 60).expect("valid grant");
    assert_eq!((grant.pid, grant.method.as_str()), (ARCHITECT.pid, "local"));
    assert_eq!(
        check_now(
            &root,
            &ClaudeProcess {
                pid: 1,
                ..ARCHITECT
            },
            NOW
        ),
        Err(PresenceRefusal::NoGrant)
    );
}

/// Spoof: a grant copied to another PID's file name is not that PID's grant.
#[test]
fn a_grant_copied_to_another_pid_is_refused() {
    let (_d, root) = root();
    let other = ClaudeProcess {
        pid: 777,
        ..ARCHITECT
    };
    mint(&root, &ARCHITECT, METHOD_LOCAL, &key(), NOW).expect("mint");
    std::fs::copy(
        grant_path(&root, ARCHITECT.pid),
        grant_path(&root, other.pid),
    )
    .expect("copy");
    let before = grants(&root);
    assert_eq!(
        check_now(&root, &other, NOW),
        Err(PresenceRefusal::OtherProcess)
    );
    assert_eq!(grants(&root), before, "a refusal writes nothing");
}

/// Spoof: the same PID with another start time is a reused PID.
#[test]
fn a_grant_with_a_stale_start_time_is_refused() {
    let (_d, root) = root();
    mint(&root, &ARCHITECT, METHOD_LOCAL, &key(), NOW).expect("mint");
    let before = grants(&root);
    let reused = ClaudeProcess {
        start_time: ARCHITECT.start_time + 5,
        ..ARCHITECT
    };
    assert_eq!(
        check_now(&root, &reused, NOW),
        Err(PresenceRefusal::StaleStartTime)
    );
    assert_eq!(grants(&root), before);
}

/// Spoof (Bob D5b Q3): a same-uid write without the key cannot forge a grant.
#[test]
fn a_forged_grant_without_the_key_is_refused() {
    let (_d, root) = root();
    // Sealed with a key the attacker chose.
    mint(
        &root,
        &ARCHITECT,
        METHOD_LOCAL,
        &GrantKey::from_bytes([9; 32]),
        NOW,
    )
    .expect("mint");
    assert_eq!(
        check_now(&root, &ARCHITECT, NOW),
        Err(PresenceRefusal::Forged)
    );
    // A real grant with a field edited after sealing.
    mint(&root, &ARCHITECT, METHOD_LOCAL, &key(), NOW - 10_000).expect("mint");
    let path = grant_path(&root, ARCHITECT.pid);
    let text = std::fs::read_to_string(&path).expect("read");
    let edited = text.replace(
        &format!("\"last_used\":{}", NOW - 10_000),
        &format!("\"last_used\":{NOW}"),
    );
    assert_ne!(text, edited);
    std::fs::write(&path, edited).expect("edit");
    let before = grants(&root);
    assert_eq!(
        check_now(&root, &ARCHITECT, NOW),
        Err(PresenceRefusal::Forged)
    );
    // No key at all is a refusal too.
    assert_eq!(
        check(&root, &ARCHITECT, || None, NOW),
        Err(PresenceRefusal::NoKey)
    );
    assert_eq!(grants(&root), before);
}

/// Spoof: a symlink at the grant path is never followed.
#[test]
fn a_symlinked_grant_is_refused() {
    let (dir, root) = root();
    let real = dir.path().join("elsewhere.presence");
    mint(&root, &ARCHITECT, METHOD_LOCAL, &key(), NOW).expect("mint");
    std::fs::rename(grant_path(&root, ARCHITECT.pid), &real).expect("move");
    std::os::unix::fs::symlink(&real, grant_path(&root, ARCHITECT.pid)).expect("symlink");
    assert_eq!(
        check_now(&root, &ARCHITECT, NOW),
        Err(PresenceRefusal::NotRegularFile)
    );
    // A symlinked anchor directory is unreadable, never followed.
    let (_d2, other) = root_with_linked_dir(&root);
    assert_eq!(
        check_now(&other, &ARCHITECT, NOW),
        Err(PresenceRefusal::Unreadable)
    );
}

fn root_with_linked_dir(target: &Path) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(".trusty-mpm");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::os::unix::fs::symlink(target.join(ARCHITECT_DIR), root.join(ARCHITECT_DIR)).expect("link");
    (dir, root)
}

/// D5b Q2: 120 idle minutes pass, 121 do not.
#[test]
fn a_grant_idle_for_121_minutes_is_refused() {
    let (_d, root) = root();
    mint(&root, &ARCHITECT, METHOD_LOCAL, &key(), NOW).expect("mint");
    assert!(check_now(&root, &ARCHITECT, NOW + 120 * 60).is_ok());
    let before = grants(&root);
    assert_eq!(
        check_now(&root, &ARCHITECT, NOW + 121 * 60),
        Err(PresenceRefusal::Idle)
    );
    assert_eq!(
        check_now(&root, &ARCHITECT, NOW - 3600),
        Err(PresenceRefusal::FutureDated)
    );
    assert_eq!(grants(&root), before);
}

#[test]
fn a_malformed_or_open_grant_is_refused() {
    let (_d, root) = root();
    let path = grant_path(&root, ARCHITECT.pid);
    std::fs::write(&path, b"{not json").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!(
        check_now(&root, &ARCHITECT, NOW),
        Err(PresenceRefusal::Malformed)
    );
    mint(&root, &ARCHITECT, METHOD_LOCAL, &key(), NOW).expect("mint");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert_eq!(
        check_now(&root, &ARCHITECT, NOW),
        Err(PresenceRefusal::UnsafeMode)
    );
    // A method tm never mints, sealed with the real key.
    mint(&root, &ARCHITECT, "sms", &key(), NOW).expect("mint");
    assert_eq!(
        check_now(&root, &ARCHITECT, NOW),
        Err(PresenceRefusal::UnknownMethod)
    );
}

#[test]
fn a_used_grant_is_refreshed() {
    let (_d, root) = root();
    mint(&root, &ARCHITECT, METHOD_LOCAL, &key(), NOW).expect("mint");
    let grant = check_now(&root, &ARCHITECT, NOW + 100 * 60).expect("valid");
    refresh(&root, &grant, &key(), NOW + 100 * 60).expect("refresh");
    // 200 minutes after the mint, 100 after the last use: still valid.
    let grant = check_now(&root, &ARCHITECT, NOW + 200 * 60).expect("refreshed grant");
    assert_eq!(grant.granted_at, NOW);
    assert_eq!(grant.last_used, NOW + 100 * 60);
}

#[test]
fn a_sandbox_key_file_round_trips_and_refuses_a_symlink() {
    let (dir, root) = root();
    let source = KeySource::for_root(&root, true);
    assert_eq!(source.load(), Ok(None));
    let made = source.load_or_create().expect("create");
    assert_eq!(source.load(), Ok(Some(made.clone())));
    assert_eq!(source.load_or_create(), Ok(made));
    assert_eq!(KeySource::for_root(&root, false), KeySource::Keychain);
    let KeySource::File(path) = source.clone() else {
        unreachable!("sandbox source is a file")
    };
    let moved = dir.path().join("key");
    std::fs::rename(&path, &moved).expect("move");
    std::os::unix::fs::symlink(&moved, &path).expect("symlink");
    assert!(source.load().is_err(), "a symlinked key file is refused");
}
