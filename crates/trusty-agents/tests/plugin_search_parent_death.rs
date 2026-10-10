//! A `trusty-search` child started on a test's behalf carries that test's
//! parent-death stamp.
//!
//! Why: #9617 — `tagent plugins status` spawns `trusty-search serve`, which
//! starts a setsid-detached `trusty-search start --foreground` daemon. Run
//! from a test, that daemon outlived the test run (ppid 1, one per run, ports
//! from 7878 up). `detached_command` keeps a parent-death stamp only when the
//! spawner carries one, so the test harness must stamp the `tagent` it spawns.
//! What: puts a stub `trusty-search` first on `PATH`; the stub records the
//! `TRUSTY_EXIT_WITH_PARENT` it was started with and exits. No daemon, no port,
//! no sleep. The recorded value must name this test process.
//! Test: `plugins_status_stamps_the_trusty_search_child_for_parent_death`.

mod support;

use std::os::unix::fs::PermissionsExt;

use support::project::Project;

/// Stub body: record the stamp it inherited, then exit.
const STUB: &str = "#!/bin/sh\nprintf '%s' \"${TRUSTY_EXIT_WITH_PARENT:-UNSET}\" > \"${0%/*}/stamp.out\"\nexit 0\n";

#[tokio::test]
async fn plugins_status_stamps_the_trusty_search_child_for_parent_death() {
    let stub_dir = tempfile::tempdir().expect("stub dir");
    let stub = stub_dir.path().join("trusty-search");
    std::fs::write(&stub, STUB).expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");

    let proj = Project::new();
    let out = proj
        .run_args_with_path_prefix(&["plugins", "status"], stub_dir.path())
        .await
        .expect("spawn tagent plugins status");
    assert!(
        out.status.success(),
        "`plugins status` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let recorded = std::fs::read_to_string(stub_dir.path().join("stamp.out"))
        .expect("the stub must have been started by tagent");
    // Stamp format is `<pid>` or `<pid>:<start>` (trusty_common::parent_death).
    let me = std::process::id().to_string();
    let named_pid = recorded.split(':').next().unwrap_or_default();
    assert_eq!(
        named_pid, me,
        "trusty-search must be stamped with this test's pid; got {recorded:?}"
    );
}
