//! Tests for the last-good record.

use super::*;
use std::collections::BTreeSet;
use tempfile::TempDir;

fn chain(deny: &[&str], sha: &str) -> ResolvedChain {
    ResolvedChain {
        chain: vec!["acme".to_string()],
        policy: OwnerPolicy {
            allow: Some(BTreeSet::new()),
            deny: deny.iter().map(|s| (*s).to_string()).collect(),
            claude_ai_connectors: Some(false),
            ..OwnerPolicy::default()
        },
        source_sha256: sha.to_string(),
    }
}

#[test]
fn an_absent_record_is_none() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(read_last_good(tmp.path(), "acme").unwrap(), None);
}

#[test]
fn a_recorded_policy_reads_back_with_its_timestamp() {
    let tmp = TempDir::new().unwrap();
    record_last_good(tmp.path(), "Acme", &chain(&["heygen"], "s1")).unwrap();
    assert!(last_good_path(tmp.path(), "Acme").ends_with("owner-policy/acme.last-good.json"));
    let record = read_last_good(tmp.path(), "acme").unwrap().unwrap();
    assert_eq!(record.policy, chain(&["heygen"], "s1").policy);
    assert_eq!(record.source_sha256, "s1");
    assert!(record.recorded_at.ends_with('Z'), "{}", record.recorded_at);
}

#[test]
fn recording_the_same_source_twice_keeps_the_first_timestamp() {
    let tmp = TempDir::new().unwrap();
    record_last_good(tmp.path(), "acme", &chain(&[], "s1")).unwrap();
    let path = last_good_path(tmp.path(), "acme");
    let before = std::fs::read(&path).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    record_last_good(tmp.path(), "acme", &chain(&[], "s1")).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_corrupt_record_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let path = last_good_path(tmp.path(), "acme");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{not json").unwrap();
    assert!(read_last_good(tmp.path(), "acme").is_err());
}

#[test]
fn a_record_for_another_owner_is_an_error() {
    let tmp = TempDir::new().unwrap();
    record_last_good(tmp.path(), "acme", &chain(&[], "s1")).unwrap();
    std::fs::copy(
        last_good_path(tmp.path(), "acme"),
        last_good_path(tmp.path(), "other"),
    )
    .unwrap();
    assert!(read_last_good(tmp.path(), "other").is_err());
}

/// Q2: a stored record is read with the floor re-applied.
#[test]
fn a_last_good_policy_has_the_floor_reapplied() {
    let tmp = TempDir::new().unwrap();
    record_last_good(tmp.path(), "acme", &chain(&["heygen", "trusty-mpm"], "s1")).unwrap();
    let record = read_last_good(tmp.path(), "acme").unwrap().unwrap();
    assert_eq!(
        record.policy.deny,
        ["heygen".to_string()].into_iter().collect::<BTreeSet<_>>()
    );
}

#[test]
fn a_non_owner_name_is_never_written() {
    let tmp = TempDir::new().unwrap();
    assert!(record_last_good(tmp.path(), "../x", &chain(&[], "s1")).is_err());
}

#[cfg(unix)]
#[test]
fn the_record_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    record_last_good(tmp.path(), "acme", &chain(&[], "s1")).unwrap();
    let path = last_good_path(tmp.path(), "acme");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let dir_mode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);
}
