//! Tests for the #8942 ruling 2 sidecar prune. Hermetic: fake probes over a
//! scratch root; nothing reaches tmux or the process table.

use std::path::Path;

use super::{PruneReport, SidecarProbes, prune_stale_sidecars};

/// Write a sidecar for `session` launched as `pid` at start time 100.
fn sidecar(root: &Path, pid: u32, session: &str) -> std::path::PathBuf {
    let dir = root.join("architect-launch");
    std::fs::create_dir_all(&dir).expect("sidecar dir");
    let path = dir.join(format!("{pid}.architect-session"));
    let body = serde_json::json!({"pid": pid, "start_time": 100, "session": session});
    std::fs::write(&path, body.to_string()).expect("sidecar");
    path
}

/// Run the prune with fixed answers for every probe.
fn prune(
    root: &Path,
    alive: Option<bool>,
    start: Result<Option<u64>, String>,
    exists: Option<bool>,
) -> PruneReport {
    let pid_alive = move |_| alive;
    let start_time = move |_| start.clone();
    let session_exists = move |_: &str| exists;
    let probes = SidecarProbes {
        pid_alive: &pid_alive,
        start_time: &start_time,
        session_exists: &session_exists,
    };
    prune_stale_sidecars(root, probes).expect("the directory lists")
}

/// A dead pid, no process with the recorded start time, and no session:
/// the sidecar is removed, also when the pid was reused by another process.
#[test]
fn a_sidecar_is_pruned_only_when_all_three_checks_prove_the_launch_gone() {
    for start in [Ok(None), Ok(Some(7))] {
        let root = crate::test_support::hermetic_temp_dir();
        let path = sidecar(root.path(), 42, "tm-arch");
        let report = prune(root.path(), Some(false), start, Some(false));
        assert_eq!(report.pruned, vec!["tm-arch".to_owned()]);
        assert!(!path.exists());
    }
}

/// Fail-Open Check (ruling 2): any check that says live, or cannot answer,
/// keeps the sidecar; an unreadable sidecar is kept too.
#[test]
fn a_sidecar_is_kept_when_any_check_is_live_or_undeterminable() {
    let cases: [(Option<bool>, Result<Option<u64>, String>, Option<bool>); 6] = [
        (Some(true), Ok(None), Some(false)),
        (None, Ok(None), Some(false)),
        (Some(false), Ok(Some(100)), Some(false)),
        (Some(false), Err("no table".into()), Some(false)),
        (Some(false), Ok(None), Some(true)),
        (Some(false), Ok(None), None),
    ];
    for (alive, start, exists) in cases {
        let root = crate::test_support::hermetic_temp_dir();
        let path = sidecar(root.path(), 42, "tm-arch");
        let report = prune(root.path(), alive, start.clone(), exists);
        assert!(report.pruned.is_empty(), "{alive:?} {start:?} {exists:?}");
        assert!(path.exists(), "{alive:?} {start:?} {exists:?}");
    }
    let root = crate::test_support::hermetic_temp_dir();
    let dir = root.path().join("architect-launch");
    std::fs::create_dir_all(&dir).expect("dir");
    let corrupt = dir.join("9.architect-session");
    std::fs::write(&corrupt, "not json").expect("corrupt");
    let report = prune(root.path(), Some(false), Ok(None), Some(false));
    assert!(corrupt.exists());
    assert!(report.kept[0].contains("unreadable"), "{report:?}");
}
