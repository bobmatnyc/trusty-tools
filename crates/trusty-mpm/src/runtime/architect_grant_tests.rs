//! Tests for the #11270 Architect launch grant (`architect_grant.rs`).

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use super::*;
use crate::core::session_profile::{SessionProfile, launch_env};

/// A spec in `cwd` that sets each of `profiles`' stamps in order.
fn spec(cwd: &Path, profiles: &[SessionProfile]) -> LaunchSpec {
    LaunchSpec {
        session_id: String::new(),
        launch_id: String::new(),
        cwd: cwd.to_path_buf(),
        program: "claude".to_owned(),
        args: vec!["--name".to_owned(), "tm-architect".to_owned()],
        env_unset: Vec::new(),
        env_set: profiles.iter().map(|p| launch_env(*p)).collect(),
    }
}

/// #11270: the stamp the child sees decides: the spec's own, else the
/// inherited one unless the spec unsets it.
#[test]
fn the_child_stamp_decides_whether_a_launch_is_an_architect_11270() {
    let cwd = Path::new("/w");
    let architect = launch_env(SessionProfile::Architect).1;
    let inherited = Some(OsStr::new(architect.as_str()));
    let own = spec(cwd, &[SessionProfile::Architect]);
    assert!(launch_is_architect(&own, None));
    assert!(!launch_is_architect(
        &spec(cwd, &[SessionProfile::Pm]),
        inherited
    ));
    assert!(launch_is_architect(&spec(cwd, &[]), inherited));
    assert!(!launch_is_architect(&spec(cwd, &[]), None));
    let mut unset = spec(cwd, &[]);
    unset.env_unset.push(SESSION_PROFILE_ENV.to_owned());
    assert!(!launch_is_architect(&unset, inherited));
}

/// #11270: an issued grant is redeemed once, for its own spec only.
#[test]
fn a_grant_is_redeemed_once_for_its_own_spec_11270() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(".trusty-mpm");
    let granted = spec(dir.path(), &[SessionProfile::Architect]);
    let path = issue(&root, &granted).expect("issue");
    assert!(path.starts_with(grant_dir(&root)), "{}", path.display());
    assert_eq!(redeem(&root, &granted), Ok(()));
    assert!(!path.exists(), "the grant is consumed");
    assert!(
        redeem(&root, &granted).is_err(),
        "a grant starts one claude"
    );
}

/// #11270 (Fail-Open Check): no grant, another spec's grant, a revoked grant
/// and an expired grant are each refused.
#[test]
fn an_expired_or_foreign_grant_is_refused_11270() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(".trusty-mpm");
    let granted = spec(dir.path(), &[SessionProfile::Architect]);
    let err = redeem(&root, &granted).expect_err("no grant");
    assert!(err.contains("binds the Architect"), "{err}");

    issue(&root, &granted).expect("issue");
    let mut forged = granted.clone();
    forged.args.push("--dangerous".to_owned());
    assert!(redeem(&root, &forged).is_err(), "a grant covers one spec");

    revoke(&root, &granted);
    assert!(redeem(&root, &granted).is_err(), "a revoked grant");

    issue(&root, &granted).expect("issue");
    std::thread::sleep(Duration::from_millis(50));
    let err = redeem_within(&root, &granted, Duration::from_millis(1)).expect_err("expired");
    assert!(err.contains("expired"), "{err}");
}
