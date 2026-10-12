//! #9171 A3b-1 live check: the wake send path against a scratch tmux server.
//!
//! Why: the scripted tests prove the argv; this one proves the keys land in a
//! real pane and that a typed draft blocks them. It must never reach the
//! fleet, so it runs only under `--include-ignored`, on its own short
//! `/tmp` socket (`tmux -S`), in a throwaway session `scratch-a3b1` that is
//! not a `tm-*` name, with `TMUX` removed, and kills that server on drop.
//! What: the pane runs a shell loop that draws an input box (rule, `❯ `,
//! rule), parks the cursor in it, and appends each submitted line to a log.
//! One wake into the empty box must type the pointer and press Enter (the log
//! gains that line); a second wake, after a draft is typed into the box, must
//! send nothing. Both captures are printed for the run's evidence.
//! Test: `live_wake_types_then_enters_and_refuses_a_draft`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use super::Waker;
use crate::poller::{Pane, capture};
use crate::{PaneId, SystemClock, SystemTmux};

/// The throwaway session: deliberately not a `tm-*` fleet name.
const SESSION: &str = "scratch-a3b1";
const UTF8: &str = if cfg!(target_os = "macos") {
    "en_US.UTF-8"
} else {
    "C.UTF-8"
};

/// The scratch server; dropping it kills the server and removes its socket.
struct Scratch {
    socket: PathBuf,
}

impl Scratch {
    fn tmux(&self, args: &[&str]) -> io::Result<Output> {
        Command::new("tmux")
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .env_remove("TMUX")
            .env("LC_ALL", UTF8)
            .output()
    }

    fn screen(&self, pane: &str) -> io::Result<String> {
        let out = self.tmux(&["capture-pane", "-p", "-t", pane])?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Polls the pane until `want` holds, for at most five seconds.
    fn until(&self, pane: &str, want: impl Fn(&str) -> bool) -> io::Result<String> {
        let start = Instant::now();
        loop {
            let screen = self.screen(pane)?;
            if want(&screen) || start.elapsed() > Duration::from_secs(5) {
                return Ok(screen);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
        let _ = fs::remove_file(&self.socket);
    }
}

fn write_exec(path: &Path, text: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, text)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[test]
#[ignore = "live: starts a scratch tmux server on a /tmp socket; run with --include-ignored"]
fn live_wake_types_then_enters_and_refuses_a_draft() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("submitted.log");
    let rule = "─".repeat(80);
    let script = dir.path().join("box.sh");
    write_exec(
        &script,
        &format!(
            "#!/bin/sh\nwhile :; do\n  printf '\\033[H\\033[2J'\n  \
             printf 'scratch-a3b1 live check\\n%s\\n\u{276f} \\n%s\\n' '{rule}' '{rule}'\n  \
             printf '\\033[3;3H'\n  IFS= read -r line || exit 0\n  \
             printf '%s\\n' \"$line\" >> '{}'\ndone\n",
            log.display()
        ),
    )?;
    // tmux runs through this wrapper so its client reads the pane as UTF-8.
    let wrapper = dir.path().join("tmux-utf8");
    write_exec(
        &wrapper,
        &format!("#!/bin/sh\nunset TMUX\nLC_ALL={UTF8} exec tmux -u \"$@\"\n"),
    )?;
    let scratch = Scratch {
        socket: PathBuf::from(format!("/tmp/a3b1-{}.sock", std::process::id())),
    };
    let started = scratch.tmux(&[
        "new-session",
        "-d",
        "-s",
        SESSION,
        "-x",
        "200",
        "-y",
        "12",
        &format!("sh {}", script.display()),
    ])?;
    assert!(started.status.success(), "{started:?}");
    let id = String::from_utf8_lossy(
        &scratch
            .tmux(&["display-message", "-p", "-t", SESSION, "#{pane_id}"])?
            .stdout,
    )
    .trim()
    .to_owned();
    let pane = PaneId::parse(&id)?;
    let ready = scratch.until(&id, |s| s.contains('\u{276f}'))?;
    println!("--- capture before the wake ---\n{ready}");

    let tmux = SystemTmux::with_socket(&scratch.socket).with_program(&wrapper);
    let clock = SystemClock;
    let waker = Waker {
        tmux: &tmux,
        clock: &clock,
        alerts: Path::new("/tmp/a3b1-alerts.jsonl"),
        dry_run: false,
    };
    let panes = [Pane {
        session: SESSION.to_owned(),
        pane: pane.clone(),
        cmd: "sh".to_owned(),
        active: "11".to_owned(),
        path: String::new(),
        dead: "0".to_owned(),
    }];

    // 1. Empty box: the pointer is typed, confirmed and submitted.
    let sent = waker.wake_as(SESSION, &panes, 1, None)?;
    println!("wake 1 -> {sent}");
    let after = scratch.until(&id, |s| s.contains('\u{276f}') && !s.contains("[poll"))?;
    println!("--- capture after wake 1 ---\n{after}");
    let submitted = fs::read_to_string(&log)?;
    println!("--- submitted.log ---\n{submitted}");
    assert_eq!(sent, format!("sent to {id}"));
    assert_eq!(submitted.lines().count(), 1);
    assert!(
        submitted.contains("1 new alerts -> /tmp/a3b1-alerts.jsonl"),
        "{submitted}"
    );

    // 2. A draft in the box: the wake refuses and sends nothing.
    scratch.tmux(&["send-keys", "-t", &id, "-l", "half-typed draft"])?;
    let drafted = scratch.until(&id, |s| s.contains("half-typed draft"))?;
    let refused = waker.wake_as(SESSION, &panes, 1, None)?;
    println!("wake 2 -> {refused}");
    let still = capture(&tmux, &pane)?;
    println!("--- capture after wake 2 ---\n{}", scratch.screen(&id)?);
    assert_eq!(refused, "skipped: input typed");
    assert_eq!(drafted, scratch.screen(&id)?, "the pane did not change");
    assert!(still.contains("half-typed draft"));
    assert_eq!(
        fs::read_to_string(&log)?.lines().count(),
        1,
        "nothing more submitted"
    );

    let socket = scratch.socket.clone();
    drop(scratch);
    assert!(!socket.exists(), "the scratch server's socket is gone");
    Ok(())
}
