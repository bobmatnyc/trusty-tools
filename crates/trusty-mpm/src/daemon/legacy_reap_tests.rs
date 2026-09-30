//! Tests for the shutdown legacy-registry reap (#8942). Hermetic: the floor is
//! rooted in a scratch directory and tmux is a scripted fake that logs argv.

use super::reap_legacy_names;
use crate::daemon::tmux::TmuxDriver;
use crate::session_manager::SupervisorFloor;

/// #8942: the legacy registry holds the Architect's pane (boot discovery adds
/// any claude pane), and the reap still never kills it or its poller, while
/// an ordinary name in the same sweep is killed. Serial: the tmux spawn guard
/// reads `$HOME`, which other serial tests reassign.
#[serial_test::serial]
#[test]
fn reap_all_live_sessions_never_kills_a_sidecar_named_session() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::test_support::hermetic_temp_dir();
    let root = dir.path().join(".trusty-mpm");
    let sidecars = root.join("architect-launch");
    std::fs::create_dir_all(&sidecars).expect("sidecar dir");
    let sidecar = serde_json::json!({"pid": 111, "start_time": 1, "session": "tm-arch"});
    std::fs::write(sidecars.join("111.architect-session"), sidecar.to_string()).expect("sidecar");
    let log = dir.path().join("calls.log");
    let bin = dir.path().join("fake-tmux");
    let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display());
    std::fs::write(&bin, script).expect("write fake tmux");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy())
        .with_floor(SupervisorFloor::at_root(&root));

    let names = ["tm-arch", "tm-arch-poll", "tm-work"].map(String::from);
    let reaped = reap_legacy_names(&driver, names);

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(reaped, 1, "only the ordinary session is reaped: {calls}");
    assert!(calls.contains("kill-session -t =tm-work"), "{calls}");
    assert!(
        !calls.contains("=tm-arch"),
        "the Architect's names were killed: {calls}"
    );
}
