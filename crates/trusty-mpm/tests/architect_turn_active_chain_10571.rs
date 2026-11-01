//! The Architect's clear chain on a fresh install, end to end (#10571 Phase 2).
//!
//! Why: the chain broke where no unit test looked: the bundled poller's
//! `parse_pane` had no `turn_active`, so `turn-active.py` exited 3 on a real
//! Architect pane. Only the seeded scripts, run against a live tmux pane, prove
//! the parts agree.
//! What: starts an isolated daemon with `scripts/sandbox_daemon.sh`, runs
//! `tm fleet init --no-launch` against it under a scratch home, then starts a
//! private tmux server (its own socket file, never the default socket or the
//! `tm-architect` name) holding a fake Architect pane and a fake PM pane that
//! print a running or an idle Claude Code screen. The seeded `fleet-poll.py`
//! runs one real cycle, then a dry run whose `turn_active` must equal
//! `turn-active.py`'s exit (0 running, 10 idle) on the live capture.
//! `datg.py ready` refuses the seeded placeholders and passes once the ledger
//! is filled in. `self-clear.sh --live` then runs against a fake `tmux` that
//! records every key: `/clear` is sent only when the pane is idle and the
//! ledger is current. Negatives: a running pane and a stale ledger send no key;
//! a parser without `turn_active` exits 3 (and, once #10599 PR-1 lands,
//! sends no key).
//! Test: `cargo test -p trusty-mpm --test integration architect_turn_active_chain_10571::`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::common;

/// The Architect and poller session names: distinct from `tm-architect`.
const ARCH: &str = "tm-e2e10571";
/// The watched PM session the poller classifies.
const PM: &str = "tm-e2e10571-pm";
/// How long the sandbox daemon may take to bind its socket.
const DAEMON_BOUND: Duration = Duration::from_secs(60);

/// A finished turn at an empty box, as Claude Code draws it.
const IDLE: &str = "⏺ The release check is done.\n\n✻ Cooked for 41s · done 12:00 AM\n\n\
    ────────────────────────────────────────\n❯ {BOX}\n────────────────────────────────────────\n  \
    ⏵⏵ bypass permissions on (shift+tab to cycle)\n";
/// A running turn: the spinner line has "esc to interrupt".
const RUNNING: &str = "⏺ Checking the release.\n\n✻ Brewing… (12s · esc to interrupt)\n\n\
    ────────────────────────────────────────\n❯ {BOX}\n────────────────────────────────────────\n  \
    ⏵⏵ bypass permissions on (shift+tab to cycle)\n";

/// The fake Claude pane: print the screen file, then wait.
const PANE_PY: &str = "import sys, time\nsys.stdout.write(open(sys.argv[1], encoding='utf-8').read()\
    .replace('{BOX}', ''))\nsys.stdout.flush()\ntime.sleep(3600)\n";

/// A fake `tmux` for self-clear.sh: serves `pane.txt` with the typed box text
/// and records each typed text and Enter in `keys.log`.
const FAKE_TMUX: &str = r#"#!/usr/bin/env python3
import os, sys
d = os.environ["FAKE_TMUX_DIR"]
a = sys.argv[1:]
while a and a[0] in ("-S", "-L", "-u"):
    a = a[1:] if a[0] == "-u" else a[2:]
box_p = os.path.join(d, "box.txt")
box = open(box_p, encoding="utf-8").read() if os.path.exists(box_p) else ""
def log(line):
    with open(os.path.join(d, "keys.log"), "a", encoding="utf-8") as f:
        f.write(line + "\n")
if a and a[0] == "capture-pane":
    sys.stdout.write(open(os.path.join(d, "pane.txt"), encoding="utf-8").read().replace("{BOX}", box))
elif a and a[0] == "send-keys":
    rest = a[1:]
    if "-t" in rest:
        i = rest.index("-t")
        rest = rest[:i] + rest[i + 2:]
    if "-l" in rest:
        text = rest[rest.index("-l") + 1]
        open(box_p, "w", encoding="utf-8").write(box + text)
        log("TYPE " + text)
    elif "Enter" in rest:
        log("ENTER " + box)
        open(box_p, "w", encoding="utf-8").write("")
elif a and a[0] == "display-message":
    print(os.environ.get("ARCHITECT_SESSION", ""))
"#;

/// One scratch install: sandbox daemon, scratch home, private tmux server.
struct Chain {
    root: tempfile::TempDir,
    daemon: Option<Child>,
}

impl Chain {
    fn new() -> Self {
        // Short: the daemon and tmux sockets must fit `sun_path`.
        let root = tempfile::Builder::new()
            .prefix("e2e")
            .tempdir_in("/tmp")
            .expect("scratch root");
        for sub in ["sandbox", "bin", "fake", "inbox", "tmuxd"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("scratch dir");
        }
        let mut chain = Self { root, daemon: None };
        chain.start_daemon();
        chain.write_fakes();
        chain
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    /// The sandbox daemon's home, also this install's home.
    fn home(&self) -> PathBuf {
        self.path("sandbox/home")
    }

    fn arch(&self) -> PathBuf {
        self.home().join("arch")
    }

    fn daemon_socket(&self) -> PathBuf {
        self.path("sandbox/data/trusty-mpm/trusty-mpm.sock")
    }

    fn tmux_socket(&self) -> PathBuf {
        self.path("t.sock")
    }

    /// `scripts/sandbox_daemon.sh --bin <tm> --dir <sandbox>`, waited until it binds.
    fn start_daemon(&mut self) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sandbox_daemon.sh");
        let mut cmd = Command::new(script);
        common::isolate_spawned_tm(&mut cmd, &self.path("sandbox"));
        cmd.args(["--bin", common::tm_bin(), "--dir"])
            .arg(self.path("sandbox"))
            .env_remove("TMUX")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.daemon = Some(cmd.spawn().expect("spawn sandbox_daemon.sh"));
        let started = Instant::now();
        while !self.daemon_socket().exists() {
            assert!(
                started.elapsed() < DAEMON_BOUND,
                "the sandbox daemon did not bind {}",
                self.daemon_socket().display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn write_fakes(&self) {
        // `tm fleet init` checks the Claude Code version; this answers it.
        trusty_common::exec_stub::write_executable(
            &self.path("bin/claude"),
            "#!/bin/sh\n[ \"$1\" = --version ] && echo '2.1.312 (Claude Code)'\nexit 0\n",
        )
        .expect("fake claude");
        trusty_common::exec_stub::write_executable(&self.path("fake/tmux"), FAKE_TMUX)
            .expect("fake tmux");
        std::fs::write(self.path("pane.py"), PANE_PY).expect("pane script");
        // architect-idle.py: the newest transcript ends on a finished turn, written a while ago.
        std::fs::create_dir_all(self.transcript_dir()).expect("transcript dir");
        let transcript = self.transcript_dir().join("t.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\
             \"end_turn\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}\n",
        )
        .expect("transcript");
        std::fs::File::options()
            .write(true)
            .open(&transcript)
            .and_then(|f| f.set_modified(SystemTime::now() - Duration::from_secs(300)))
            .expect("age the transcript");
    }

    /// self-ctx.py's transcript dir: `<CLAUDE_CONFIG_DIR>/projects/<slug of the Architect dir>`.
    fn transcript_dir(&self) -> PathBuf {
        let slug: String = self
            .arch()
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        self.path("cc/projects").join(slug)
    }

    /// A command with only this install's environment.
    fn base(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("HOME", self.home())
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("PYTHONIOENCODING", "utf-8")
            .env("TMUX_TMPDIR", self.path("tmuxd"))
            .env("CLAUDE_CONFIG_DIR", self.path("cc"))
            .env("ARCHITECT_PROJECT_DIR", self.arch())
            .stdin(Stdio::null());
        cmd
    }

    /// `python3 <arch>/scripts/<script> <args>` as the Architect install runs it.
    fn script(&self, script: &str, args: &[&str]) -> Command {
        let mut cmd = self.base("python3");
        cmd.arg(self.arch().join("scripts").join(script))
            .args(args)
            .current_dir(self.arch())
            .env("ARCHITECT_SESSION", ARCH)
            .env("ARCHITECT_INBOX_DIR", self.path("inbox"))
            .env("TMUX_SOCKET", self.tmux_socket())
            .env("LOAD_FACTOR", "1000");
        cmd
    }

    /// `tmux -S <private socket> <args>`.
    fn tmux(&self, args: &[&str]) -> Output {
        let out = self
            .base("tmux")
            .arg("-u")
            .arg("-S")
            .arg(self.tmux_socket())
            .args(args)
            .output()
            .expect("tmux");
        assert!(out.status.success(), "tmux {args:?}: {}", text(&out));
        out
    }

    /// Show `screen` in session `name`, starting the session on first use.
    fn show(&self, name: &str, screen: &str) {
        let file = self.path(&format!("{name}.screen"));
        std::fs::write(&file, screen).expect("screen");
        let cmd = format!(
            "LANG=C.UTF-8 LC_ALL=C.UTF-8 PYTHONIOENCODING=utf-8 exec python3 {} {}",
            self.path("pane.py").display(),
            file.display()
        );
        let target = trusty_common::tmux::exact_window_target(name);
        let exists = self
            .base("tmux")
            .arg("-S")
            .arg(self.tmux_socket())
            .args([
                "has-session",
                "-t",
                &trusty_common::tmux::exact_session_target(name),
            ])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if exists {
            self.tmux(&["respawn-pane", "-k", "-t", &target, &cmd]);
        } else {
            self.tmux(&[
                "new-session",
                "-d",
                "-s",
                name,
                "-x",
                "120",
                "-y",
                "40",
                &cmd,
            ]);
        }
        // Wait for the box rule to reach the screen.
        let started = Instant::now();
        while !self.capture(name).contains("❯") {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "{name} never drew"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The live pane capture, with escape codes, as self-clear.sh takes it.
    fn capture(&self, name: &str) -> String {
        String::from_utf8_lossy(
            &self
                .tmux(&[
                    "capture-pane",
                    "-p",
                    "-e",
                    "-t",
                    &trusty_common::tmux::exact_window_target(name),
                ])
                .stdout,
        )
        .into_owned()
    }

    /// `turn-active.py`'s exit code on `capture`.
    fn turn_active(&self, capture: &str) -> i32 {
        let mut child = self
            .script("turn-active.py", &[])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("turn-active.py");
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(capture.as_bytes())
            .expect("pipe the capture");
        child
            .wait_with_output()
            .expect("turn-active.py")
            .status
            .code()
            .unwrap_or(-1)
    }

    /// The poller dry run's `turn_active` for the PM pane.
    fn poller_turn_active(&self) -> bool {
        let out = self
            .script("fleet-poll.py", &["--dry-run"])
            .output()
            .expect("fleet-poll");
        assert!(out.status.success(), "fleet-poll --dry-run: {}", text(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find(|l| l.contains(&format!(" {PM} ")))
            .unwrap_or_else(|| panic!("no dry-run line for {PM}:\n{stdout}"));
        if line.contains("turn_active=True") {
            true
        } else {
            assert!(line.contains("turn_active=False"), "{line}");
            false
        }
    }

    /// `datg.py --project local/il <args>` in the Architect directory.
    fn datg(&self, args: &[&str]) -> Output {
        let mut full = vec!["--project", "local/il"];
        full.extend_from_slice(args);
        self.script("datg.py", &full).output().expect("datg.py")
    }

    /// `self-clear.sh --live` against the fake tmux showing `screen`, bounded by `wait`.
    /// Returns (exit code or None when it was still waiting, recorded keys, its output).
    fn self_clear(&self, screen: &str, wait: Duration) -> (Option<i32>, String, String) {
        std::fs::write(self.path("fake/pane.txt"), screen).expect("fake pane");
        let _ = std::fs::remove_file(self.path("fake/box.txt"));
        let _ = std::fs::remove_file(self.path("fake/keys.log"));
        let path = format!(
            "{}:{}",
            self.path("fake").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = std::fs::File::create(self.path("fake/self-clear.out")).expect("output file");
        let mut child = self
            .base("bash")
            .arg(self.arch().join("scripts/self-clear.sh"))
            .arg("--live")
            .current_dir(self.arch())
            .env("PATH", path)
            .env("FAKE_TMUX_DIR", self.path("fake"))
            .env("ARCHITECT_ROOT", self.arch())
            .env("ARCHITECT_SESSION", ARCH)
            .env("SELF_CLEAR_POLL", "0")
            .env("SELF_CLEAR_ECHO", "0")
            .env("SELF_CLEAR_SETTLE", "0")
            .stdout(out.try_clone().expect("output file"))
            .stderr(out)
            .spawn()
            .expect("self-clear.sh");
        let started = Instant::now();
        let code = loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                break status.code();
            }
            if started.elapsed() > wait {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let keys = std::fs::read_to_string(self.path("fake/keys.log")).unwrap_or_default();
        let said = std::fs::read_to_string(self.path("fake/self-clear.out")).unwrap_or_default();
        (code, keys, said)
    }

    /// `tm fleet init --no-launch` against the sandbox daemon.
    fn fleet_init(&self) {
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = common::tm_command_in(&self.home());
        for key in trusty_mpm::core::claude_env_scrub::INHERITED_SESSION_MARKERS {
            cmd.env_remove(key);
        }
        let out = cmd
            .args(["fleet", "init", "--no-launch", "--session", ARCH, "--dir"])
            .arg(self.arch())
            .env("PATH", path)
            .env("TRUSTY_MPM_SOCKET", self.daemon_socket())
            .env("TMUX_TMPDIR", self.path("tmuxd"))
            .env_remove("TMUX")
            .env_remove("TMUX_SOCKET")
            .env("TRUSTY_MPM_ALLOW_HOST_STATE", "1")
            .stdin(Stdio::null())
            .output()
            .expect("tm fleet init");
        assert!(out.status.success(), "tm fleet init: {}", text(&out));
    }

    /// Fill in the seeded ledger so `datg.py ready` can pass.
    fn fill_ledger(&self) {
        for args in [
            &[
                "add",
                "G",
                "--title",
                "Ship the release the owner asked for",
            ][..],
            &[
                "session",
                "set",
                "--task",
                "Relay the release check to the PM",
                "--next",
                "Ask the PM for the CI result",
                "--owe",
                "none",
                "--cron",
                "none",
            ][..],
        ] {
            let out = self.datg(args);
            assert!(out.status.success(), "datg.py {args:?}: {}", text(&out));
        }
    }
}

impl Drop for Chain {
    fn drop(&mut self) {
        let _ = self
            .base("tmux")
            .arg("-S")
            .arg(self.tmux_socket())
            .arg("kill-server")
            .stderr(Stdio::null())
            .status();
        if let Some(mut child) = self.daemon.take() {
            // SIGTERM: sandbox_daemon.sh passes it on and tears its tmux down.
            let _ = Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status();
            let started = Instant::now();
            while child.try_wait().ok().flatten().is_none() && started.elapsed() < DAEMON_BOUND {
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The real chain on a fresh install: poller, turn-active, ready, self-clear.
#[test]
fn the_clear_chain_runs_on_a_fresh_install() {
    let chain = Chain::new();
    chain.fleet_init();
    for seeded in [
        "scripts/fleet-poll.py",
        "scripts/fleet_poll_io.py",
        "scripts/turn-active.py",
        "local/il-schedule.json",
    ] {
        assert!(chain.arch().join(seeded).is_file(), "{seeded} not seeded");
    }

    // A running turn: one real poller cycle, then the parse agrees.
    chain.show(ARCH, RUNNING);
    chain.show(PM, RUNNING);
    let once = chain
        .script("fleet-poll.py", &["--once"])
        .output()
        .expect("fleet-poll --once");
    assert!(once.status.success(), "fleet-poll --once: {}", text(&once));
    let log = std::fs::read_to_string(chain.path("inbox/poll.log")).expect("poll.log");
    assert!(log.contains("panes=1 "), "{log}");
    assert!(chain.path("inbox/poll.lock").is_file());
    chain.show(ARCH, RUNNING); // the cycle may have typed its wake into the fake pane
    assert_eq!(chain.turn_active(&chain.capture(ARCH)), 0, "running pane");
    assert!(
        chain.poller_turn_active(),
        "the poller must read the same running turn"
    );

    // A finished turn: idle, and the poller agrees.
    chain.show(ARCH, IDLE);
    chain.show(PM, IDLE);
    assert_eq!(chain.turn_active(&chain.capture(ARCH)), 10, "idle pane");
    assert!(
        !chain.poller_turn_active(),
        "the poller must read the same idle pane"
    );

    // The seeded ledger holds placeholders: not ready, and self-clear sends nothing.
    let out = chain.datg(&["ready"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    chain.fill_ledger();
    let out = chain.datg(&["ready"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));

    // self-clear.sh logs to <arch>/inbox, the default inbox a live poller creates;
    // this test's poller writes to ARCHITECT_INBOX_DIR instead.
    std::fs::create_dir_all(chain.arch().join("inbox")).expect("default inbox");
    // Running pane, ready ledger: self-clear waits and types nothing.
    let (code, keys, said) = chain.self_clear(RUNNING, Duration::from_secs(10));
    assert!(
        code != Some(0) && !keys.contains("/clear"),
        "{code:?} {keys}\n{said}"
    );

    // Idle pane, ready ledger: /clear, then the resume prompt.
    let (code, keys, said) = chain.self_clear(IDLE, Duration::from_secs(60));
    assert_eq!(code, Some(0), "{keys}\n{said}");
    assert!(keys.starts_with("TYPE /clear\nENTER /clear\n"), "{keys}");
    assert!(keys.contains("TYPE Resume from the Architect IL"), "{keys}");

    // A state change after the last ledger write: stale, ready refuses, no key.
    let stamp = chain.arch().join("local/il/.trusty-mpm/datg/stamp.json");
    let later = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
        + 120;
    std::fs::write(&stamp, format!("{{\"changed_epoch\": {later}}}")).expect("stamp");
    let out = chain.datg(&["ready"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let (code, keys, said) = chain.self_clear(IDLE, Duration::from_secs(30));
    assert_eq!(code, Some(4), "{keys}\n{said}");
    assert!(keys.is_empty(), "{keys}");

    // A parser with no turn_active field: turn-active.py cannot tell, exit 3, never 10.
    std::fs::write(
        chain.arch().join("scripts/fleet-classify.py"),
        "def parse_pane(capture):\n    return {}\n",
    )
    .expect("break the parser");
    assert_eq!(chain.turn_active(&chain.capture(ARCH)), 3);
}

/// A parser with no `turn_active` (the bundle before #10579) exits 3, and the
/// clear must not run. Ignored until #10599 PR-1: today's self-clear.sh reads
/// any nonzero exit as idle.
#[test]
#[ignore = "#10599 PR-1: self-clear.sh treats only turn-active exit 10 as idle"]
fn a_parser_without_turn_active_blocks_the_clear() {
    let chain = Chain::new();
    chain.fleet_init();
    chain.fill_ledger();
    std::fs::create_dir_all(chain.arch().join("inbox")).expect("default inbox");
    std::fs::write(
        chain.arch().join("scripts/fleet-classify.py"),
        "def parse_pane(capture):\n    return {}\n",
    )
    .expect("break the parser");
    chain.show(ARCH, IDLE);
    assert_eq!(chain.turn_active(&chain.capture(ARCH)), 3);
    let (_, keys, said) = chain.self_clear(IDLE, Duration::from_secs(30));
    assert!(!keys.contains("/clear"), "{keys}\n{said}");
}
