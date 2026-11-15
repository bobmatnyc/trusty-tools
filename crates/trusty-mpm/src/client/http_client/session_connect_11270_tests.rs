//! #11270: the TUI launch step kills a fresh Architect it could not bind.
//!
//! Hermetic: a pinned scratch home and a fake `tmux` that logs every argv
//! and prints nothing, so no `claude` is ever found under the pane.

use crate::client::DaemonClient;
use crate::core::session_profile::{SessionProfile, launch_env};
use crate::runtime::launch_spec::LaunchSpec;

/// #11270 (H2, Fail-Open Check): the launch is typed, no `claude` is bound,
/// the session is killed and the step is an error naming the remedy.
#[serial_test::serial]
#[tokio::test]
async fn a_tui_architect_left_unbound_is_killed_and_errors_11270() {
    let home = tempfile::tempdir().expect("home");
    let log = home.path().join("argv.log");
    let bin = home.path().join("fake-tmux");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
        log.display()
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let client = DaemonClient::new("http://127.0.0.1:0").with_home(home.path());
    let spec = LaunchSpec {
        session_id: String::new(),
        launch_id: String::new(),
        cwd: home.path().to_path_buf(),
        program: "claude".to_owned(),
        args: Vec::new(),
        env_unset: Vec::new(),
        env_set: vec![launch_env(SessionProfile::Architect)],
    };
    let err = crate::core::tmux::with_tmux_binary(bin, async {
        client.send_client_spec(None, "tm-a", &spec)
    })
    .await
    .expect_err("an unbound Architect is an error");
    let text = format!("{err:#}");
    for part in ["NOT bound", "was killed", "tm fleet relaunch"] {
        assert!(text.contains(part), "{part} missing: {text}");
    }
    let argv = std::fs::read_to_string(&log).expect("tmux ran");
    let typed = argv.find("send-keys").expect("the launch was typed");
    let killed = argv.find("kill-session").expect("the session was killed");
    assert!(typed < killed, "{argv}");
}
