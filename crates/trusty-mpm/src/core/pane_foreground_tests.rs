//! Tests for #10136's foreground probe: the `ps` parse and a live child.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use super::pane_foreground::{harness_holds_pane, parse_foreground};
use super::process::{PaneClaude, harness_pids_under_pane, harness_under_pane};

/// Kills, on drop, every process a live #10136 test started — on every path,
/// a panic or a `wait_until` timeout included.
///
/// Why: a `set -m` job runs in its own process group, so tmux's
/// `kill-session` hangs up the pane and leaves the job running under pid 1.
/// What: SIGKILLs and reaps each of `children`; SIGKILLs each process named
/// `harness` under `roots`, read at drop time so a pid never collected still
/// dies. Its parent, or pid 1 once orphaned, reaps it.
pub(crate) struct KillOnDrop {
    pub(crate) children: Vec<Child>,
    pub(crate) roots: Vec<u32>,
    pub(crate) harness: &'static str,
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        for &root in &self.roots {
            for pid in harness_pids_under_pane(root, self.harness).0 {
                if let Ok(raw) = i32::try_from(pid) {
                    let _ = kill(Pid::from_raw(raw), Signal::SIGKILL);
                }
            }
        }
    }
}

/// Only a live state whose group is a positive tty foreground group holds the
/// pane; every malformed answer is `None`, which the probe reads as `Unknown`.
#[test]
fn a_foreground_reading_needs_a_live_state_and_the_tty_group() {
    for (answer, want) in [
        ("S+   4242   4242\n", Some(true)),
        ("Ss 4242 4242", Some(true)),
        ("R+ 7 7", Some(true)),
        // Stopped or dead while still the foreground group.
        ("T 4242 4242", Some(false)),
        ("t+ 4242 4242", Some(false)),
        ("Z 4242 4242", Some(false)),
        // A background group, and no tty: macOS prints 0, Linux -1.
        ("S 4242 4300", Some(false)),
        ("S 4242 0", Some(false)),
        ("S 4242 -1", Some(false)),
        // Unparseable: an empty answer (a dead pid), a missing, extra or
        // non-numeric field, a numeric state.
        ("", None),
        ("\n", None),
        ("S 4242", None),
        ("S 4242 4242 4242", None),
        ("S x 4242", None),
        ("S 4242 y", None),
        ("4242 4242 4242", None),
    ] {
        assert_eq!(parse_foreground(answer), want, "{answer:?}");
    }
}

/// Poll `ready` every 10 ms, failing after 10 s.
fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A child in its own process group exists under the "pane" but never holds
/// a tty foreground, running or stopped: `Unknown`, never `Present` or
/// `Absent`. A dead pane process is `Unknown`; a pane with no match `Absent`.
#[test]
fn a_stopped_or_background_harness_does_not_hold_the_pane() {
    let child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sleep");
    let cpid = child.id();
    let reaper = KillOnDrop {
        children: vec![child],
        roots: Vec::new(),
        harness: "sleep",
    };
    // #10144: the name is the spawner's until the exec completes.
    wait_until("the exec", || {
        harness_under_pane(cpid, "sleep") == PaneClaude::Present
    });
    assert_eq!(harness_holds_pane(cpid, "sleep"), PaneClaude::Unknown);
    assert_eq!(
        harness_holds_pane(cpid, "no-such-harness"),
        PaneClaude::Absent
    );

    let pid = Pid::from_raw(i32::try_from(cpid).expect("a pid"));
    kill(pid, Signal::SIGSTOP).expect("stop the child");
    let stopped = harness_holds_pane(cpid, "sleep");
    drop(reaper);
    assert_eq!(stopped, PaneClaude::Unknown);
    assert_eq!(harness_holds_pane(u32::MAX, "claude"), PaneClaude::Unknown);
}
