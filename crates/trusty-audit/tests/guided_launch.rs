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

/// The named verb is what a script, a CI job and the E2E harness run, and
/// #5502's own reachability table mandates that spelling — so it must print the
/// card and exit even with a terminal attached (#5896 review).
///
/// It is checked here rather than only as a unit test because the unit-level
/// mistake was precisely a rule stated one layer too late: `Cli::to_command`
/// collapses `taudit` and `taudit guided` onto the same value, so an assertion
/// over `Command` could not have caught this.
#[test]
fn the_named_guided_verb_prints_the_card_and_exits() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("trusty-audit-work");

    // No `setsid`: this child KEEPS whatever terminal the test runner has, so
    // the verb is the only thing that can be deciding.
    let out = Command::new(TAUDIT)
        .args(["guided", "--work-dir"])
        .arg(&work)
        .arg("--config")
        .arg(tmp.path().join("engagement.toml"))
        .current_dir(tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("`taudit guided` exits");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Working directory:"), "{stdout}");
    assert!(
        !stdout.contains("press Enter when done"),
        "`taudit guided` reached the registration prompt:\n{stdout}"
    );
}

/// A launch backgrounded with `&` must print the card, not stop silently
/// (#5896 review).
///
/// `trusty-audit &` still opens `/dev/tty` for WRITE, so the terminal probe
/// succeeded and the first READ raised SIGTTIN — whose default action stops the
/// job, with nothing printed and no diagnostic to say why. `DevTty::open` now
/// refuses a terminal this process is in a background group of.
///
/// The child is put in its own process group with `setpgid(0, 0)` while keeping
/// the runner's controlling terminal, which is exactly the shape `&` produces.
/// It SKIPS when the runner has no controlling terminal — in CI there is
/// nothing to be in the background of, and both the defect and the fix behave
/// identically. The wait is bounded rather than blocking, because the failure
/// mode under test is a process that stops rather than one that exits.
#[test]
fn a_backgrounded_launch_prints_the_card_rather_than_stopping() {
    if std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .is_err()
    {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("trusty-audit-work");

    let mut command = Command::new(TAUDIT);
    command
        .arg("--work-dir")
        .arg(&work)
        .arg("--config")
        .arg(tmp.path().join("engagement.toml"))
        .current_dir(tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // SAFETY: `setpgid` is async-signal-safe and this hook runs in the forked
    // child before `exec`, moving only the child into its own process group.
    unsafe {
        command.pre_exec(|| match libc::setpgid(0, 0) {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }

    let mut child = command.spawn().expect("taudit spawns");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let exited = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break Some(status),
            None if std::time::Instant::now() >= deadline => break None,
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };

    let Some(status) = exited else {
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "a backgrounded launch never exited — it was stopped by SIGTTIN on a \
             read it should never have attempted"
        );
    };

    let out = child.wait_with_output().expect("output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Working directory:"), "{stdout}");
    assert!(
        !stdout.contains("press Enter when done"),
        "a backgrounded launch reached the registration prompt:\n{stdout}"
    );
}
