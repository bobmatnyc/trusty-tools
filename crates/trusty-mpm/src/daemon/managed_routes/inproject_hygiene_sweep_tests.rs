//! Tests for the hygiene sweep's switch and spawn (#1709, #7965).

use super::{parse_enabled, spawn_if_enabled};

/// The sweep is ON unless explicitly switched off.
///
/// Why this is the assertion rather than the inverse: #7965's operator mitigation
/// was `TRUSTY_MPM_INPROJECT_HYGIENE=0`, and that lever only means anything if
/// unset still means ON. A default flip would leave the sweep dead and every test
/// here green.
#[test]
fn parse_enabled_defaults_on_and_honours_the_off_switch() {
    assert!(parse_enabled(None), "unset must enable the sweep");
    assert!(parse_enabled(Some("1")));
    assert!(parse_enabled(Some("true")));
    assert!(
        parse_enabled(Some("")),
        "an empty value is not an off switch"
    );
    for off in ["0", "false", "off", "no", " OFF ", "False"] {
        assert!(!parse_enabled(Some(off)), "`{off}` must disable the sweep");
    }
}

/// A disabled sweep spawns nothing and returns at once.
///
/// Why it is `#[serial]`: it writes the process-global env var the gate reads.
#[tokio::test]
#[serial_test::serial]
async fn spawn_if_enabled_is_a_noop_when_disabled() {
    let dir = tempfile::tempdir().expect("scratch framework root");
    let state = std::sync::Arc::new(crate::daemon::state::DaemonState::with_root(
        dir.path().to_path_buf(),
    ));
    // SAFETY: the test is serialised against every other env-writing test in this
    // binary by `serial_test`.
    unsafe { std::env::set_var(super::ENV_ENABLED, "0") };
    spawn_if_enabled(state);
    unsafe { std::env::remove_var(super::ENV_ENABLED) };
    assert!(
        !crate::daemon::services::sweep_status::HYGIENE.is_running(),
        "a disabled sweep must not start a pass"
    );
}
