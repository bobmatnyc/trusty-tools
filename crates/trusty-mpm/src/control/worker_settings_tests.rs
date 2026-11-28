//! Tests for the control-plane worker's carrier body (#12077 part 4c, Q3).

use super::*;
use crate::test_support::{STABLE_HOOK_EXE, hermetic_temp_dir};

fn config(state_root: &Path) -> ControlPlaneConfig {
    ControlPlaneConfig {
        hook_exe: Some(PathBuf::from(STABLE_HOOK_EXE)),
        state_root: Some(state_root.to_path_buf()),
        ..Default::default()
    }
}

#[test]
fn a_worker_body_carries_the_egress_guard_12077() {
    let workdir = hermetic_temp_dir();
    let state = hermetic_temp_dir();
    let file = worker_settings(workdir.path(), &config(state.path())).expect("a body");
    assert!(file.starts_with(state.path()), "{}", file.display());
    require_egress_guard(&file).expect("the body carries the guard");
    assert!(
        !workdir
            .path()
            .join(".claude")
            .join("settings.json")
            .exists()
    );
}

/// Fail-Open Check: a body that cannot be written refuses the spawn.
#[test]
fn an_unwritable_worker_body_refuses_the_spawn_12077() {
    let workdir = hermetic_temp_dir();
    let state = hermetic_temp_dir();
    let root = state.path().join("state");
    std::fs::write(&root, b"not a directory").expect("seed");
    let err = worker_settings(workdir.path(), &config(&root)).expect_err("refused");
    assert!(format!("{err:#}").contains("hooks"), "{err:#}");
}

/// Fail-Open Check: the spawn check reads the built body, so one without the
/// egress guard is refused.
#[test]
fn a_body_without_the_egress_guard_is_refused_12077() {
    let state = hermetic_temp_dir();
    let file = state.path().join("body.json");
    std::fs::write(&file, br#"{"hooks":{"Stop":[]}}"#).expect("seed");
    let err = require_egress_guard(&file).expect_err("no guard");
    assert!(err.to_string().contains("egress guard Missing"), "{err}");
}

/// Fail-Open Check (Q2): a worker with no stable hook binary is refused.
#[test]
fn an_unresolvable_hook_binary_refuses_the_worker_12077() {
    let workdir = hermetic_temp_dir();
    let state = hermetic_temp_dir();
    let err = worker_settings_resolved(
        workdir.path(),
        &config(state.path()),
        Err(crate::core::standalone::hooks::StableHookExeError::Unresolved.into()),
    )
    .expect_err("refused");
    assert!(
        format!("{err:#}").contains("refusing the launch"),
        "{err:#}"
    );
    assert!(!state.path().join("session-settings").exists());
}
