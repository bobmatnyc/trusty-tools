//! End-to-end tests of the warm server through the built binary.
//!
//! Every test uses its own runtime, cache and config directories
//! (`TRUSTY_SUMMARIZER_RUNTIME_DIR`), so none touches a developer's real
//! server or cache, and each stops (or kills) the servers it started.
//! No network beyond 127.0.0.1, no Ollama, no model download.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;

const BOUND: Duration = Duration::from_secs(20);

/// One isolated environment: config, cache, runtime directory, documents.
struct Env {
    tmp: tempfile::TempDir,
    pids: std::cell::RefCell<Vec<u32>>,
}

impl Env {
    fn new(config: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("config.toml"), config).unwrap();
        let docs = tmp.path().join("docs");
        std::fs::create_dir_all(docs.join("nested")).unwrap();
        let mut md = String::from("# Guide\n\nThe guide explains the summarizer cache.\n\n");
        for i in 1..=4 {
            md.push_str(&format!(
                "## Part {i}\n\nPart {i} covers topic number {i}. It has a second sentence.\n\n"
            ));
        }
        std::fs::write(docs.join("guide.md"), md).unwrap();
        std::fs::write(
            docs.join("nested/notes.txt"),
            "Notes start here. The notes are short.\n\nA last line.\n",
        )
        .unwrap();
        Env {
            tmp,
            pids: Default::default(),
        }
    }

    fn run_dir(&self) -> PathBuf {
        self.tmp.path().join("run")
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_trusty-summarizer"));
        c.current_dir(self.tmp.path())
            .env_remove("TRUSTY_SUMMARIZER_CONFIG")
            .env_remove("TRUSTY_SUMMARIZER_CACHE_DIR")
            .env("TRUSTY_SUMMARIZER_RUNTIME_DIR", self.run_dir())
            .env(
                "TRUSTY_SUMMARIZER_USAGE_LOG",
                self.tmp.path().join("usage.jsonl"),
            )
            .arg("--config")
            .arg(self.tmp.path().join("config.toml"));
        c
    }

    fn summarize(&self, extra: &[&str]) -> Output {
        let mut c = self.cmd();
        c.args(["summarize", "--no-cache", "docs"]).args(extra);
        c.output().unwrap()
    }

    fn status(&self) -> Value {
        let out = self.cmd().args(["server", "status"]).output().unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        if let Some(pid) = v["pid"].as_u64() {
            self.pids.borrow_mut().push(pid as u32);
        }
        v
    }

    fn pid(&self) -> u32 {
        let s = self.status();
        assert_eq!(s["running"], true, "{s}");
        s["pid"].as_u64().unwrap() as u32
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.cmd().args(["server", "stop"]).output();
        for pid in self.pids.borrow().iter() {
            if alive(*pid) {
                kill(*pid);
            }
        }
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn kill(pid: u32) {
    // SAFETY: sends SIGKILL to a server process this test started.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Waits, bounded, for `cond`.
fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let started = Instant::now();
    while !cond() {
        assert!(started.elapsed() < BOUND, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Records with the per-invocation stage timings removed.
fn records(out: &Output) -> Vec<Value> {
    String::from_utf8(out.stdout.clone())
        .unwrap()
        .lines()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).expect("each line is JSON");
            v.as_object_mut().unwrap().remove("stages");
            v
        })
        .collect()
}

#[test]
fn warm_output_matches_in_process_and_one_server_serves_both_calls() {
    let env = Env::new("[outline]\nmax_section_lines = 4\n");
    let first = env.summarize(&[]);
    assert!(first.status.success(), "{}", stderr(&first));
    let pid = env.pid();
    let second = env.summarize(&[]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(env.pid(), pid, "the second call reused the server");

    let cold = env.summarize(&["--no-warm"]);
    assert!(cold.status.success(), "{}", stderr(&cold));
    let warm = records(&first);
    assert_eq!(warm.len(), 2);
    assert_eq!(warm, records(&cold));
    assert_eq!(records(&second), warm);
    // Paths are spelled as given, relative to the caller's directory.
    assert_eq!(warm[0]["path"], "docs/guide.md");
    assert_eq!(warm[1]["path"], "docs/nested/notes.txt");
    assert_eq!(env.status()["configs_loaded"], 1);

    // --no-warm never creates a runtime directory or socket.
    let other = Env::new("");
    let out = other.summarize(&["--no-warm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!other.run_dir().exists());
    // Nor does warm.enabled = false.
    let off = Env::new("[warm]\nenabled = false\n");
    assert!(off.summarize(&[]).status.success());
    assert!(!off.run_dir().exists());
}

#[test]
fn server_exits_after_idle_timeout() {
    let env = Env::new("[warm]\nidle_timeout_secs = 1\n");
    let out = env.summarize(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let pid = env.pid();
    let socket = env.run_dir().join("server.sock");
    assert!(socket.exists());
    wait_for("idle exit", || !alive(pid));
    assert!(!socket.exists(), "the socket is removed on idle exit");
    assert_eq!(env.status()["running"], false);
}

#[test]
fn racing_calls_start_one_server() {
    let env = Env::new("");
    let children: Vec<_> = (0..4)
        .map(|_| {
            let mut c = env.cmd();
            c.args(["summarize", "--no-cache", "docs"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            c.spawn().unwrap()
        })
        .collect();
    let outs: Vec<Output> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    for out in &outs {
        assert!(out.status.success(), "{}", stderr(out));
        assert_eq!(records(out), records(&outs[0]));
    }
    env.pid();
    let log = std::fs::read_to_string(env.run_dir().join("server.log")).unwrap();
    assert_eq!(log.matches("listening on").count(), 1, "{log}");
}

#[test]
fn stale_socket_is_replaced() {
    let env = Env::new("");
    assert!(env.summarize(&[]).status.success());
    let old = env.pid();
    kill(old);
    wait_for("killed server", || !alive(old));
    assert!(
        env.run_dir().join("server.sock").exists(),
        "stale socket left"
    );
    let out = env.summarize(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_ne!(env.pid(), old);
}

#[test]
fn server_with_other_identity_is_replaced() {
    // An environment value the library reads differs: a new server, not
    // the old one's results.
    let env = Env::new("");
    let mut c = env.cmd();
    c.env("FASTEMBED_CACHE_DIR", env.tmp.path().join("models-a"));
    c.args(["summarize", "--no-cache", "docs"]);
    assert!(c.output().unwrap().status.success());
    let first = env.pid();
    let mut c = env.cmd();
    c.env("FASTEMBED_CACHE_DIR", env.tmp.path().join("models-b"));
    c.args(["summarize", "--no-cache", "docs"]);
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    wait_for("replaced server to exit", || !alive(first));
    let mut c = env.cmd();
    c.env("FASTEMBED_CACHE_DIR", env.tmp.path().join("models-b"));
    let s: Value =
        serde_json::from_slice(&c.args(["server", "status"]).output().unwrap().stdout).unwrap();
    let second = s["pid"].as_u64().unwrap() as u32;
    env.pids.borrow_mut().push(second);
    assert_ne!(second, first);
}

#[test]
fn each_request_runs_under_its_own_configuration() {
    let one = "[backends.extractive-tf]\nmax_sentences = 1\n";
    let env = Env::new(one);
    let a = env.summarize(&[]);
    assert!(a.status.success(), "{}", stderr(&a));
    let pid = env.pid();
    std::fs::write(
        env.tmp.path().join("config.toml"),
        "[backends.extractive-tf]\nmax_sentences = 3\n",
    )
    .unwrap();
    let b = env.summarize(&[]);
    assert!(b.status.success(), "{}", stderr(&b));
    assert_eq!(env.pid(), pid);
    assert_eq!(records(&b), records(&env.summarize(&["--no-warm"])));
    assert_ne!(
        records(&a),
        records(&b),
        "the second config's results differ"
    );
    std::fs::write(env.tmp.path().join("config.toml"), one).unwrap();
    assert_eq!(records(&a), records(&env.summarize(&["--no-warm"])));
    assert_eq!(env.status()["configs_loaded"], 2);
}

/// An OpenAI-compatible endpoint in the test process. Answers the first
/// request (checking the bearer key when `key` is set); holds every later
/// one open and reports it on the channel.
fn endpoint(key: Option<&'static str>, answer_all: bool) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let count = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                head.push_str(&line);
            }
            let mut body = vec![0; len];
            let _ = reader.read_exact(&mut body);
            let n = count.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(head.clone());
            if n > 0 && !answer_all {
                held.push(stream);
                continue;
            }
            let authorized = key.is_none_or(|k| head.contains(&format!("Bearer {k}")));
            let (status, reply) = if authorized {
                (
                    200,
                    r#"{"choices":[{"message":{"content":"A summary."},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#,
                )
            } else {
                (401, r#"{"error":"no key"}"#)
            };
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    (url, rx)
}

fn llm_config(url: &str, extra: &str) -> String {
    format!(
        "concurrency = 1\n[summary]\nbackend = \"ep\"\n\
         [backends.ep]\ntype = \"openai\"\nbase_url = \"{url}\"\nmodel = \"m\"\n{extra}"
    )
}

#[test]
fn server_killed_mid_batch_fails_the_call() {
    let (url, seen) = endpoint(None, false);
    let env = Env::new(&llm_config(&url, ""));
    let child = env
        .cmd()
        .args(["summarize", "--no-cache", "docs"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The first file is answered; the second request is held open.
    seen.recv_timeout(BOUND).unwrap();
    seen.recv_timeout(BOUND).unwrap();
    let pid = env.pid();
    kill(pid);
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "a lost batch must not succeed");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        records(&out).len(),
        1,
        "the record before the loss streamed"
    );
    assert!(
        stderr(&out).contains("connection lost after 1 records"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn keys_travel_per_request_and_are_not_kept() {
    let (url, _seen) = endpoint(Some("sk-test-warm-777"), true);
    let env = Env::new(&llm_config(
        &url,
        "api_key_env = \"TS_WARM_TEST_API_KEY\"\n",
    ));
    let mut c = env.cmd();
    c.env("TS_WARM_TEST_API_KEY", "sk-test-warm-777").args([
        "summarize",
        "--no-cache",
        "docs/guide.md",
    ]);
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let r = records(&out);
    assert_eq!(r[0]["summary"], "A summary.");
    let pid = env.pid();

    // A later call without the key: the server did not keep it.
    let mut c = env.cmd();
    c.env_remove("TS_WARM_TEST_API_KEY")
        .args(["summarize", "--no-cache", "docs/guide.md"]);
    let out = c.output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let r = records(&out);
    assert_eq!(r[0]["error"]["kind"], "backend_failed");
    assert!(
        r[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("TS_WARM_TEST_API_KEY is not set"),
        "{}",
        r[0]
    );
    assert_eq!(env.pid(), pid);
    let log = std::fs::read_to_string(env.run_dir().join("server.log")).unwrap();
    assert!(!log.contains("sk-test-warm-777"));
    assert_no_key_in(&env.run_dir(), "sk-test-warm-777");
}

fn assert_no_key_in(dir: &Path, key: &str) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file()
            && let Ok(bytes) = std::fs::read(&path)
        {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(key),
                "{}",
                path.display()
            );
        }
    }
}
