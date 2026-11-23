//! `ta ask` against the built binary and its on-demand daemon (#11650).
//!
//! Why: the on-demand start, the reuse of a running daemon, racing clients
//! and the idle exit are process-lifecycle behaviour; only real processes
//! prove them. Every run is offline (`TCODE_MOCK_LLM=echo`), under a
//! temporary `$HOME`, with the data directory (and so every socket) in a
//! short `/tmp` directory. Every test waits for its daemons to exit and for
//! their sockets to be removed.
//! Test: this file IS the test.

mod common;

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// What the echo model answers: its third scripted reply is the first text.
const ECHO_ANSWER: &str = "engineer: echoed hello-from-mock-engineer";

struct Env {
    home: TempDir,
    data: TempDir,
}

impl Env {
    /// A temp home with the instance `e2e`, and a short data directory.
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        let instance = home.path().join("trusty-agents").join("e2e");
        std::fs::create_dir_all(&instance).expect("instance home");
        std::fs::write(instance.join("instructions.md"), "You are e2e.\n").expect("instructions");
        let data = tempfile::Builder::new()
            .prefix("ta-")
            .tempdir_in("/tmp")
            .expect("data");
        Self { home, data }
    }

    fn ta(&self) -> Command {
        let mut cmd = common::ta(&self.home);
        cmd.env("TRUSTY_DATA_DIR_OVERRIDE", self.data.path())
            .env("TCODE_MOCK_LLM", "echo");
        cmd
    }

    fn dir(&self) -> PathBuf {
        self.data.path().join("trusty-agents")
    }

    fn socket(&self) -> PathBuf {
        self.dir().join("trusty-agents-e2e.sock")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir().join("trusty-agents-e2e.log")).unwrap_or_default()
    }

    fn turns(&self) -> usize {
        let log = self
            .home
            .path()
            .join("trusty-agents/e2e/stores/ask-turns.jsonl");
        std::fs::read_to_string(log).map_or(0, |b| b.lines().count())
    }

    fn ask(&self, question: &str) -> Output {
        self.ta()
            .args(["ask", "e2e", question])
            .output()
            .expect("run ta ask")
    }

    /// Wait (bounded) until no daemon serves the instance and its log says
    /// every daemon that served has exited.
    fn wait_idle(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let log = self.log();
            let served = log.matches(": serving ").count();
            let exited = log.matches(" exited (").count();
            if !self.socket().exists() && served == exited {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not exit; log:\n{log}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !self.dir().join("trusty-agents.sock").exists(),
            "the health socket is removed with the daemon"
        );
    }
}

fn assert_answered(out: &Output) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "ta ask failed: {stderr}");
    assert_eq!(stdout.trim(), ECHO_ANSWER, "stderr: {stderr}");
}

#[test]
fn ask_starts_the_daemon_on_demand_and_it_exits_idle() {
    let env = Env::new();
    assert!(!env.socket().exists());
    assert_answered(&env.ask("hello there"));
    env.wait_idle();
    let log = env.log();
    assert_eq!(log.matches(": serving ").count(), 1, "{log}");
    assert_eq!(env.turns(), 1);
}

/// Why (#11742): a cold ask waited about 10 s with no output. It now prints
/// one progress line on stderr; stdout carries only the answer, and a warm
/// ask prints no progress line.
#[test]
fn a_cold_ask_reports_the_start_on_stderr_only() {
    const NOTICE: &str = "ta: starting e2e assistant";
    let env = Env::new();
    let cold = env.ask("hello there");
    assert_answered(&cold);
    let stderr = String::from_utf8_lossy(&cold.stderr);
    assert_eq!(stderr.matches(NOTICE).count(), 1, "stderr: {stderr}");
    assert!(!String::from_utf8_lossy(&cold.stdout).contains(NOTICE));
    env.wait_idle();

    // Warm: a daemon already serves the instance.
    let mut daemon = env
        .ta()
        .args(["ask-daemon", "e2e"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let mut ready = String::new();
    BufReader::new(daemon.stdout.take().expect("stdout"))
        .read_line(&mut ready)
        .expect("ready line");
    assert_eq!(ready.trim(), "ready");
    let hold = UnixStream::connect(env.socket()).expect("hold");
    drop(daemon.stdin.take());
    let warm = env.ask("again");
    assert_answered(&warm);
    let stderr = String::from_utf8_lossy(&warm.stderr);
    assert!(!stderr.contains(NOTICE), "warm stderr: {stderr}");
    drop(hold);
    assert!(wait_exit(&mut daemon).success());
}

/// The questions in `env`'s turn log, in order.
fn asked(env: &Env) -> Vec<String> {
    let log = env
        .home
        .path()
        .join("trusty-agents/e2e/stores/ask-turns.jsonl");
    std::fs::read_to_string(log)
        .expect("turn log")
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("turn record");
            v["question"].as_str().expect("question").to_string()
        })
        .collect()
}

/// Why (#11742): a help word inside a question is question text, not a
/// request for usage.
#[test]
fn help_words_inside_a_question_are_asked() {
    let env = Env::new();
    assert_answered(
        &env.ta()
            .args(["ask", "e2e", "what", "is", "-h"])
            .output()
            .expect("run"),
    );
    env.wait_idle();
    assert_eq!(asked(&env), ["what is -h"]);
}

/// Why (#11742): a leading `--` ends options, so `-- --help` asks the
/// literal question `--help`, without the `--`.
#[test]
fn a_leading_double_dash_is_dropped_from_the_question() {
    let env = Env::new();
    assert_answered(
        &env.ta()
            .args(["ask", "e2e", "--", "--help"])
            .output()
            .expect("run"),
    );
    env.wait_idle();
    assert_eq!(asked(&env), ["--help"]);
}

#[test]
fn a_second_ask_reuses_the_running_daemon() {
    let env = Env::new();
    let log = std::fs::File::create(env.data.path().join("daemon.log")).expect("log");
    let mut daemon = env
        .ta()
        .args(["ask-daemon", "e2e"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .expect("spawn daemon");
    let mut ready = String::new();
    BufReader::new(daemon.stdout.take().expect("stdout"))
        .read_line(&mut ready)
        .expect("ready line");
    assert_eq!(ready.trim(), "ready");
    let inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(env.socket()).unwrap());

    // An open connection keeps the daemon up between the two asks. #11650:
    // `connect` returning is the synchronisation point: the daemon checks its
    // listen backlog before an idle exit, so a connection made before the
    // release is never missed, whether or not it was accepted yet.
    let hold = UnixStream::connect(env.socket()).expect("hold");
    drop(daemon.stdin.take());
    assert_answered(&env.ask("first"));
    assert_answered(&env.ask("second"));
    assert!(
        daemon.try_wait().expect("try_wait").is_none(),
        "the daemon was reused"
    );
    let socket_inode =
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(env.socket()).unwrap());
    assert_eq!(socket_inode, inode, "no second daemon bound the socket");
    assert!(
        env.log().is_empty(),
        "no daemon was spawned by the asks: {}",
        env.log()
    );
    assert_eq!(env.turns(), 2);

    drop(hold);
    let status = wait_exit(&mut daemon);
    assert!(status.success(), "{status}");
    assert!(!env.socket().exists(), "the daemon removed its socket");
    let daemon_log = std::fs::read_to_string(env.data.path().join("daemon.log")).unwrap();
    assert_eq!(daemon_log.matches(": serving ").count(), 1, "{daemon_log}");
    assert!(daemon_log.contains("answered: 2"), "{daemon_log}");
}

#[test]
fn racing_asks_all_answer_through_one_daemon_at_a_time() {
    let env = Env::new();
    let clients: Vec<_> = (0..4)
        .map(|i| {
            env.ta()
                .args(["ask", "e2e", &format!("question {i}")])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn ta ask")
        })
        .collect();
    for client in clients {
        assert_answered(&client.wait_with_output().expect("wait"));
    }
    env.wait_idle();
    assert_eq!(env.turns(), 4, "every ask logged once, none lost");
    // Serving and exit lines alternate: no two daemons served at once.
    let log = env.log();
    let lifecycle: Vec<&str> = log
        .lines()
        .filter(|l| l.contains(": serving ") || l.contains(" exited ("))
        .collect();
    for (i, line) in lifecycle.iter().enumerate() {
        let serving = line.contains(": serving ");
        assert_eq!(serving, i % 2 == 0, "overlapping daemons in:\n{log}");
    }
}

#[test]
fn ask_names_an_unknown_instance() {
    let env = Env::new();
    let out = env
        .ta()
        .args(["ask", "nosuch", "hi"])
        .output()
        .expect("run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown assistant instance `nosuch`"),
        "{stderr}"
    );
    assert!(!env.dir().join("trusty-agents-nosuch.sock").exists());
}

fn wait_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("daemon did not exit after its last connection closed");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
