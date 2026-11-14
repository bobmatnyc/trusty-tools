//! #11270: the GUI spawn of an Architect never types into a session it did
//! not create, and never leaves an unbound Architect running.
//!
//! Hermetic: a fake `tmux` logs every argv; `display-message` answers a pane
//! id only when the session "exists". No real tmux, `claude` or home is used.

use std::path::{Path, PathBuf};

use super::TmuxService;
use crate::core::session_profile::SessionProfile;
use crate::daemon::error::DaemonError;
use crate::daemon::tmux::TmuxDriver;

/// A fake `tmux` under `dir` logging to `<dir>/argv.log`; `display-message`
/// prints `%1` when `running`, else exits 1.
fn fake_tmux(dir: &Path, running: bool) -> (PathBuf, PathBuf) {
    let log = dir.join("argv.log");
    let bin = dir.join("fake-tmux");
    let pane = if running { "echo %1; exit 0" } else { "exit 1" };
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in *display-message*) {pane};; esac\nexit 0\n",
        log.display()
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    (bin, log)
}

/// Run [`TmuxService::start_gui_claude`] for `profile` on the fake.
async fn start(bin: PathBuf, root: &Path, profile: SessionProfile) -> Result<(), DaemonError> {
    crate::core::tmux::with_tmux_binary(bin, async {
        let driver = TmuxDriver::discover().expect("the fake tmux resolves");
        TmuxService::start_gui_claude(
            &driver,
            "tm-gui",
            Path::new("/w"),
            "claude",
            profile,
            Some(root),
        )
    })
    .await
}

/// #11270 (M1): an Architect spawn into a running session is refused with
/// the remedy before any create or keystroke.
#[serial_test::serial]
#[tokio::test]
async fn a_gui_architect_spawn_into_a_running_session_types_nothing_11270() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (bin, log) = fake_tmux(dir.path(), true);
    let err = start(
        bin,
        &dir.path().join(".trusty-mpm"),
        SessionProfile::Architect,
    )
    .await
    .expect_err("an Architect into a running session is refused");
    assert!(
        matches!(&err, DaemonError::Unprocessable(why) if why.contains("tm fleet relaunch")),
        "{err:?}"
    );
    let argv = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(!argv.contains("new-session"), "{argv}");
    assert!(!argv.contains("send-keys"), "{argv}");
}

/// #11270 (H2, M3): a fresh GUI Architect is created without `-A`, and one
/// whose `claude` cannot be bound is killed and is an error naming the
/// remedy; a PM spawn is unchanged and kills nothing.
#[serial_test::serial]
#[tokio::test]
async fn a_gui_architect_spawn_creates_exclusively_and_an_unbound_one_is_killed_11270() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(".trusty-mpm");
    let (bin, log) = fake_tmux(dir.path(), false);
    let err = start(bin.clone(), &root, SessionProfile::Architect)
        .await
        .expect_err("an unbound Architect is an error");
    assert!(
        matches!(&err, DaemonError::Internal(why)
            if why.contains("NOT bound") && why.contains("tm fleet relaunch")),
        "{err:?}"
    );
    let argv = std::fs::read_to_string(&log).expect("tmux ran");
    let create = argv
        .lines()
        .find(|l| l.contains("new-session"))
        .expect("a create");
    assert!(!create.contains(" -A"), "an Architect attaches: {create}");
    assert!(argv.contains("kill-session"), "{argv}");
    assert!(!root.join("architect-launch").exists(), "no record");

    std::fs::remove_file(&log).expect("reset log");
    start(bin, &root, SessionProfile::Pm)
        .await
        .expect("a PM spawn is unchanged");
    let argv = std::fs::read_to_string(&log).expect("tmux ran");
    assert!(!argv.contains("kill-session"), "{argv}");
}
