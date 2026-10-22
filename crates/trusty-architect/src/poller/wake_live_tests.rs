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
//! Test: `live_wake_types_then_enters_and_refuses_a_draft`; #9818 adds
//! `live_wake_keys_reach_only_the_target_pane` and
//! `live_wake_reads_a_wide_dim_suggestion_row`.

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

    fn screen(&self, pane: &PaneId) -> io::Result<String> {
        let out = self.tmux(&["capture-pane", "-p", "-t", pane.as_target()])?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Polls the pane until `want` holds, for at most five seconds.
    fn until(&self, pane: &PaneId, want: impl Fn(&str) -> bool) -> io::Result<String> {
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
            // Exact session target for SESSION (this leaf crate cannot use trusty_common).
            .tmux(&[
                "display-message",
                "-p",
                "-t",
                "=scratch-a3b1:",
                "#{pane_id}",
            ])?
            .stdout,
    )
    .trim()
    .to_owned();
    let pane = PaneId::parse(&id)?;
    let ready = scratch.until(&pane, |s| s.contains('\u{276f}'))?;
    println!("--- capture before the wake ---\n{ready}");

    let tmux = SystemTmux::with_socket(&scratch.socket).with_program(&wrapper);
    let clock = SystemClock;
    let waker = Waker {
        tmux: &tmux,
        clock: &clock,
        alerts: Path::new("/tmp/a3b1-alerts.jsonl"),
        dry_run: false,
        messages: &crate::poller::NoMessages,
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
    let after = scratch.until(&pane, |s| s.contains('\u{276f}') && !s.contains("[poll"))?;
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
    scratch.tmux(&[
        "send-keys",
        "-t",
        pane.as_target(),
        "-l",
        "half-typed draft",
    ])?;
    let drafted = scratch.until(&pane, |s| s.contains("half-typed draft"))?;
    let refused = waker.wake_as(SESSION, &panes, 1, None)?;
    println!("wake 2 -> {refused}");
    let still = capture(&tmux, &pane)?;
    println!("--- capture after wake 2 ---\n{}", scratch.screen(&pane)?);
    assert_eq!(refused, "skipped: input typed");
    assert_eq!(drafted, scratch.screen(&pane)?, "the pane did not change");
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

// See #9818: the two cases below were deferred from PR #9817. The third case,
// a typed draft blocks the wake, is already covered by wake 2 of
// `live_wake_types_then_enters_and_refuses_a_draft`. Each test runs its own
// scratch server on its own `/tmp` socket.

/// The input-box script for one pane: draws a `width`-wide rule box with
/// `❯ ` (plus an optional dim suggestion), then appends each line read to `log`.
fn box_script(log: &Path, width: usize, suggestion: Option<&str>) -> String {
    let rule = "─".repeat(width);
    let prompt = match suggestion {
        None => "printf '❯ \\n'".to_owned(),
        Some(text) => format!("printf '❯ \\033[2m%s\\033[0m\\n' '{text}'"),
    };
    format!(
        "#!/bin/sh\nwhile :; do\n  printf '\\033[H\\033[2J'\n  \
         printf 'scratch live check\\n%s\\n' '{rule}'\n  {prompt}\n  \
         printf '%s\\n' '{rule}'\n  printf '\\033[3;3H'\n  IFS= read -r line || exit 0\n  \
         printf '%s\\n' \"$line\" >> '{}'\ndone\n",
        log.display()
    )
}

/// The wrapper through which tmux reads the pane as UTF-8.
fn utf8_wrapper(dir: &Path) -> io::Result<PathBuf> {
    let wrapper = dir.join("tmux-utf8");
    write_exec(
        &wrapper,
        &format!("#!/bin/sh\nunset TMUX\nLC_ALL={UTF8} exec tmux -u \"$@\"\n"),
    )?;
    Ok(wrapper)
}

/// A listed pane of `session` with the given active flags.
fn listed(session: &str, pane: &PaneId, active: &str) -> Pane {
    Pane {
        session: session.to_owned(),
        pane: pane.clone(),
        cmd: "sh".to_owned(),
        active: active.to_owned(),
        path: String::new(),
        dead: "0".to_owned(),
    }
}

/// Why: the wake must type into the owner's chosen pane and no other pane.
/// What: one scratch session with two box panes, both listed inactive so the
/// first listed is the target. One wake must submit the pointer in the first
/// pane and leave the second pane's log empty and its screen unchanged.
/// Test: this test; the target choice is `owner_pane` in `wake.rs`.
#[test]
#[ignore = "live: starts a scratch tmux server on a /tmp socket; run with --include-ignored"]
fn live_wake_keys_reach_only_the_target_pane() -> Result<(), Box<dyn std::error::Error>> {
    const NAME: &str = "scratch-a3b1-two";
    let dir = tempfile::tempdir()?;
    let (log_a, log_b) = (dir.path().join("a.log"), dir.path().join("b.log"));
    let (script_a, script_b) = (dir.path().join("a.sh"), dir.path().join("b.sh"));
    write_exec(&script_a, &box_script(&log_a, 80, None))?;
    write_exec(&script_b, &box_script(&log_b, 80, None))?;
    let wrapper = utf8_wrapper(dir.path())?;
    let scratch = Scratch {
        socket: PathBuf::from(format!("/tmp/a3b1-{}-two.sock", std::process::id())),
    };
    let started = scratch.tmux(&[
        "new-session",
        "-d",
        "-s",
        NAME,
        "-x",
        "200",
        "-y",
        "24",
        &format!("sh {}", script_a.display()),
    ])?;
    assert!(started.status.success(), "{started:?}");
    // Exact session target for NAME (this leaf crate cannot use trusty_common).
    let split = scratch.tmux(&[
        "split-window",
        "-d",
        "-t",
        "=scratch-a3b1-two:",
        &format!("sh {}", script_b.display()),
    ])?;
    assert!(split.status.success(), "{split:?}");
    let listing = scratch.tmux(&["list-panes", "-t", "=scratch-a3b1-two:", "-F", "#{pane_id}"])?;
    let ids: Vec<String> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let (first, second) = (PaneId::parse(&ids[0])?, PaneId::parse(&ids[1])?);
    scratch.until(&first, |s| s.contains('\u{276f}'))?;
    let ready_b = scratch.until(&second, |s| s.contains('\u{276f}'))?;

    let tmux = SystemTmux::with_socket(&scratch.socket).with_program(&wrapper);
    let clock = SystemClock;
    let waker = Waker {
        tmux: &tmux,
        clock: &clock,
        alerts: Path::new("/tmp/a3b1-two-alerts.jsonl"),
        dry_run: false,
        messages: &crate::poller::NoMessages,
    };
    let panes = [listed(NAME, &first, "01"), listed(NAME, &second, "01")];
    let sent = waker.wake_as(NAME, &panes, 1, None)?;
    println!("wake -> {sent}");
    scratch.until(&first, |s| !s.contains("[poll"))?;
    let submitted = fs::read_to_string(&log_a).unwrap_or_default();
    println!("--- a.log ---\n{submitted}");
    println!("--- second pane ---\n{}", scratch.screen(&second)?);
    assert_eq!(sent, format!("sent to {first}"));
    assert_eq!(submitted.lines().count(), 1, "{submitted}");
    assert!(!log_b.exists() || fs::read_to_string(&log_b)?.is_empty());
    assert_eq!(
        ready_b,
        scratch.screen(&second)?,
        "the second pane did not change"
    );
    Ok(())
}

/// Why: Claude Code draws its next-prompt suggestion dim; a wide pane with a
/// long dim row must still read as a suggestion, so the wake is allowed.
/// What: a 240-column pane whose `❯` row is a long all-dim suggestion. The
/// capture (with `-e`) must carry `ESC[2m` and a full-width rule, and a
/// dry-run wake must report it would send, with nothing typed.
/// Test: this test; the reading is `input_state_for_send` in `classify/guard.rs`.
#[test]
#[ignore = "live: starts a scratch tmux server on a /tmp socket; run with --include-ignored"]
fn live_wake_reads_a_wide_dim_suggestion_row() -> Result<(), Box<dyn std::error::Error>> {
    const NAME: &str = "scratch-a3b1-wide";
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("submitted.log");
    let script = dir.path().join("box.sh");
    let suggestion = "run the full test suite and summarise any failures ".repeat(3);
    write_exec(&script, &box_script(&log, 240, Some(suggestion.trim_end())))?;
    let wrapper = utf8_wrapper(dir.path())?;
    let scratch = Scratch {
        socket: PathBuf::from(format!("/tmp/a3b1-{}-wide.sock", std::process::id())),
    };
    let started = scratch.tmux(&[
        "new-session",
        "-d",
        "-s",
        NAME,
        "-x",
        "240",
        "-y",
        "12",
        &format!("sh {}", script.display()),
    ])?;
    assert!(started.status.success(), "{started:?}");
    // Exact session target for NAME (this leaf crate cannot use trusty_common).
    let id = String::from_utf8_lossy(
        &scratch
            .tmux(&[
                "display-message",
                "-p",
                "-t",
                "=scratch-a3b1-wide:",
                "#{pane_id}",
            ])?
            .stdout,
    )
    .trim()
    .to_owned();
    let pane = PaneId::parse(&id)?;
    let ready = scratch.until(&pane, |s| s.contains("summarise"))?;

    let tmux = SystemTmux::with_socket(&scratch.socket).with_program(&wrapper);
    let raw = capture(&tmux, &pane)?;
    let widest = raw
        .lines()
        .map(|l| l.chars().filter(|c| *c == '─').count())
        .max()
        .unwrap_or(0);
    println!("--- raw capture ---\n{raw:?}");
    assert!(raw.contains("\u{1b}[2m"), "tmux emitted no dim escape");
    assert!(widest >= 240, "rule is {widest} wide, not full width");

    let clock = SystemClock;
    let waker = Waker {
        tmux: &tmux,
        clock: &clock,
        alerts: Path::new("/tmp/a3b1-wide-alerts.jsonl"),
        dry_run: true,
        messages: &crate::poller::NoMessages,
    };
    let verdict = waker.wake_as(NAME, &[listed(NAME, &pane, "11")], 1, None)?;
    println!("wake -> {verdict}");
    assert!(
        verdict.starts_with(&format!("dry-run: would send to {id}: [poll")),
        "{verdict}"
    );
    assert_eq!(ready, scratch.screen(&pane)?, "the pane did not change");
    assert!(!log.exists(), "nothing was submitted");
    Ok(())
}
