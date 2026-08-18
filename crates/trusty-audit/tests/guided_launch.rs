//! What a bare launch does when nobody is there to answer it (#5885).
//!
//! Why: the launch became interactive — it asks for audit targets and then for
//! permission to sweep. That is the right behaviour at a terminal and the wrong
//! one everywhere else: a CI job, a cron entry, or a script that runs `taudit`
//! and reads its output must get the status card and an exit, never a read that
//! never returns. The decision is `DevTty::open()`, the same probe the
//! credential prompt uses (#5868), and this file is what proves it holds from
//! outside the process.
//!
//! What: the real binary, launched into its own session with `setsid(2)` so it
//! has no controlling terminal at all — inheriting the developer's terminal
//! would let the interactive path open `/dev/tty` and block the test. Its stdin
//! carries a line that would register a target if anything ever read it, which
//! is the `curl … | sh` hazard stated as an assertion.
//! Test: this is the test.

#![cfg(unix)]

use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const TAUDIT: &str = env!("CARGO_BIN_EXE_taudit");

/// A repository name that is only ever reachable by READING stdin.
///
/// The install path is `curl … | sh`, where stdin is the pipe carrying the
/// script text. A launch that reads it would take shell lines for audit targets.
const NEVER_READ: &str = "acme/never-read-from-stdin";

struct Launch {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    work: PathBuf,
}

impl Launch {
    /// The registry as it stands after the launch — empty when nothing was read.
    fn registry(&self) -> String {
        std::fs::read_to_string(self.work.join("state/audit-targets.toml")).unwrap_or_default()
    }
}

/// Run a bare `taudit` with no controlling terminal, feeding it `NEVER_READ`.
fn launch_without_a_terminal() -> Launch {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("trusty-audit-work");

    let mut command = Command::new(TAUDIT);
    command
        .arg("--work-dir")
        .arg(&work)
        // No engagement config: the first state a recipient is ever in, and the
        // one the owner's launch was in.
        .arg("--config")
        .arg(tmp.path().join("engagement.toml"))
        .current_dir(tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // SAFETY: `setsid` is async-signal-safe and this hook runs in the forked
    // child before `exec`, touching nothing but the child's own session.
    unsafe {
        command.pre_exec(|| match libc::setsid() {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }

    let mut child = command.spawn().expect("taudit spawns");
    let written = child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(format!("{NEVER_READ}\n\n").as_bytes());
    // A broken pipe here is the property under test, not a flake: the child
    // printed its card and exited without ever reading stdin, so the write lost
    // its reader. Any OTHER failure is a real one.
    if let Err(e) = written {
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe,
            "writing the launch's stdin failed for a reason other than it having exited: {e}"
        );
    }

    let out = child.wait_with_output().expect("taudit exits");
    Launch {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code(),
        work,
    }
}

/// The whole non-interactive contract in one launch: it exits, it prints the
/// status card, and it read nothing.
///
/// The exit is the load-bearing part and it is asserted by this test COMPLETING
/// — a launch that reached the prompt would block here forever rather than fail.
#[test]
fn a_launch_with_no_terminal_prints_the_card_and_reads_nothing() {
    let launch = launch_without_a_terminal();

    assert_eq!(
        launch.code,
        Some(0),
        "stdout: {}\nstderr: {}",
        launch.stdout,
        launch.stderr
    );

    // Today's card, unchanged.
    assert!(
        launch.stdout.contains("Working directory:"),
        "{}",
        launch.stdout
    );
    assert!(
        launch
            .stdout
            .contains("Next: register the repositories and boards to audit (`trusty-audit add`)"),
        "{}",
        launch.stdout
    );
    assert!(launch.stdout.contains("Coverage: "), "{}", launch.stdout);

    // The prompt is a front-end path that must not have been entered at all.
    assert!(
        !launch.stdout.contains("press Enter when done"),
        "a launch with no terminal reached the registration prompt:\n{}",
        launch.stdout
    );

    // The `curl … | sh` hazard: stdin was never the thing to ask on.
    assert!(
        !launch.stdout.contains(NEVER_READ) && !launch.registry().contains(NEVER_READ),
        "the launch read a target from stdin:\nstdout: {}\nregistry: {}",
        launch.stdout,
        launch.registry()
    );
}

/// The stray character the owner saw after the coverage paragraph.
///
/// The card ends with a sentence, so a trailing colon reads as "…and here it
/// comes" with nothing after it. Asserted on the rendered card rather than on
/// the constant, because the colon could be introduced by either.
#[test]
fn the_card_ends_a_sentence_rather_than_trailing_a_colon() {
    let launch = launch_without_a_terminal();
    let last = launch
        .stdout
        .trim_end()
        .lines()
        .last()
        .expect("the card has a last line")
        .to_owned();

    assert!(
        !last.ends_with(':'),
        "the card ends with a stray colon: {last:?}"
    );
    assert!(last.ends_with('.'), "the card ends mid-sentence: {last:?}");
}
