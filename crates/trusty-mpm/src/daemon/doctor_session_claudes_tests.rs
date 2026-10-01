//! Coverage for the `session_claudes` doctor row (#8980).

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use super::*;
use crate::core::paths::FrameworkPaths;
use crate::core::session::SessionId;
use crate::core::twin_identity::ClaudeProcess;
use crate::daemon::state::DaemonState;

/// A framework root holding a trusted registry with one bound and one
/// unproven id, at `mode`: (the root, the registry file).
fn registry(dir: &Path, mode: u32) -> (PathBuf, PathBuf) {
    let state = DaemonState::with_paths(&FrameworkPaths::under(dir));
    let claude = ClaudeProcess {
        pid: 300,
        start_time: 50,
    };
    state
        .bind_session_claude(SessionId::new(), claude)
        .expect("vacant");
    state
        .settle_unproven_session(SessionId::new())
        .expect("saved");
    let root = state.framework_root().to_path_buf();
    let file = root.join(SESSION_CLAUDES_FILE);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).expect("chmod");
    (root, file)
}

/// Assert `check` warns, names `why`, and says the daemon stays sealed
/// until restart.
fn assert_warns(check: &DoctorCheck, why: &str) {
    assert_eq!(check.name, "session_claudes");
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(check.message.contains(why), "{}", check.message);
    assert!(
        check.message.contains("stays sealed until restart"),
        "{}",
        check.message
    );
}

/// #8980: no registry yet is healthy — nothing has announced itself.
#[test]
fn a_missing_registry_is_ok_8980() {
    let dir = tempfile::tempdir().expect("tempdir");
    let check = check_session_claudes(dir.path());
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(check.message.contains("no session-claude registry yet"));
}

/// #8980: a registry the daemon trusts is `Ok`, with its counts.
#[test]
fn a_trusted_registry_is_ok_8980() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, _) = registry(dir.path(), 0o600);
    let check = check_session_claudes(&root);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("1 session(s) bound") && check.message.contains("1 unproven"),
        "{}",
        check.message
    );
}

/// #8980 Fail-Open Check: a corrupt registry warns, never `Ok`.
#[test]
fn a_corrupt_registry_warns_and_names_the_restart_8980() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, file) = registry(dir.path(), 0o600);
    std::fs::write(&file, b"{\"version\":1,\"sessions\":").expect("corrupt it");
    assert_warns(&check_session_claudes(&root), "does not parse");
}

/// #8980 Fail-Open Check: a registry owned by another uid warns.
#[test]
fn a_foreign_owned_registry_warns_8980() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, _) = registry(dir.path(), 0o600);
    let other = crate::daemon::state::session_claudes::current_uid().wrapping_add(1);
    assert_warns(&check_session_claudes_as(&root, other), "owned by uid");
}

/// #8980 Fail-Open Check: a registry open to other users warns.
#[test]
fn an_open_registry_warns_8980() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, _) = registry(dir.path(), 0o644);
    assert_warns(&check_session_claudes(&root), "only its owner may");
}

/// #8980 Fail-Open Check: a registry this process cannot read warns.
#[test]
fn an_unreadable_registry_warns_8980() {
    if crate::daemon::state::session_claudes::current_uid() == 0 {
        eprintln!("root reads any file; skipping");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, _) = registry(dir.path(), 0o000);
    assert_warns(&check_session_claudes(&root), "could not be opened");
}
