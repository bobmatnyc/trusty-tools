//! A PM launch whose prompt file cannot be written registers nothing (#8286).
//!
//! Why: `tm connect`, `tm session start` and `tm launch` each refuse when the
//! PM system-prompt file cannot be written. The refusal only helps if it lands
//! BEFORE the daemon is told about the session; a refusal after
//! `POST /sessions` leaves a registered session with no Claude behind it. The
//! unit tests of each prompt helper cannot see that order, so these tests drive
//! the built `tm` binary end to end.
//!
//! What: each case points the child's `TMPDIR` at a regular file, so every
//! prompt write fails with ENOTDIR, and both daemon transports at a recording
//! stub: `--url` at an HTTP listener (`tm launch`, `tm connect`) and
//! `TRUSTY_MPM_SOCKET` at a unix socket (`tm session start`, which reaches the
//! daemon over the socket only, #6288). It asserts the child exits non-zero
//! with the prompt refusal and that the stub saw no registration. Lives in
//! `env_serial` with the other modules that spawn `tm` against a rewritten
//! environment.
//!
//! Test: this file IS the test.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use crate::common::tm_command_in;

/// A daemon stand-in on both transports that records every call.
///
/// HTTP calls are logged as their request line (`POST /sessions HTTP/1.1`),
/// socket calls as their JSON-RPC method (`mpm.sessions.register`).
struct RecordingDaemon {
    url: String,
    socket: PathBuf,
    calls: Arc<Mutex<Vec<String>>>,
}

impl RecordingDaemon {
    /// Bind an ephemeral TCP port and a socket under `dir`; serve both on
    /// detached threads.
    fn start(dir: &Path) -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
        let url = format!("http://{}", listener.local_addr().expect("local addr"));
        let seen = Arc::clone(&calls);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve_http(&stream, &seen);
            }
        });
        // The client refuses a socket whose directory is not 0700 or which is
        // not itself 0600 (`trusty_common::uds`).
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod socket dir");
        let socket = dir.join("trusty-mpm.sock");
        let unix = UnixListener::bind(&socket).expect("bind stub socket");
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
            .expect("chmod socket");
        let seen = Arc::clone(&calls);
        std::thread::spawn(move || {
            for stream in unix.incoming().flatten() {
                serve_rpc(&stream, &seen);
            }
        });
        Self { url, socket, calls }
    }

    /// Every call that would register a session.
    fn registrations(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .filter(|c| c.starts_with("POST") || c.as_str() == "mpm.sessions.register")
            .cloned()
            .collect()
    }
}

/// Answer one HTTP request: `[]` to a `GET`, a session body otherwise.
fn serve_http(stream: &std::net::TcpStream, calls: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);
    calls
        .lock()
        .expect("call log")
        .push(request_line.trim().to_owned());
    let payload = if request_line.starts_with("GET") {
        "[]"
    } else {
        r#"{"name":"tm-8286-probe","id":null}"#
    };
    let _ = (&*stream).write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        )
        .as_bytes(),
    );
}

/// Answer one newline-framed JSON-RPC call with a session-shaped result.
fn serve_rpc(stream: &std::os::unix::net::UnixStream, calls: &Mutex<Vec<String>>) {
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).is_err() {
        return;
    }
    let method = serde_json::from_str::<serde_json::Value>(&line)
        .ok()
        .and_then(|v| v["method"].as_str().map(str::to_owned))
        .unwrap_or_default();
    calls.lock().expect("call log").push(method.clone());
    let result = if method == "mpm.sessions.list" {
        serde_json::json!([])
    } else {
        serde_json::json!({ "name": "tm-8286-probe", "id": null })
    };
    let reply = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result });
    let _ = (&*stream).write_all(format!("{reply}\n").as_bytes());
}

/// `git <args>` in `dir`, which must succeed.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// A git repository at `dir` with one commit, optionally with an origin.
fn git_repo(dir: &Path, origin: Option<&str>) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "probe\n").expect("write README");
    git(dir, &["add", "README.md"]);
    git(
        dir,
        &[
            "-c",
            "user.name=probe",
            "-c",
            "user.email=probe@example.invalid",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    if let Some(url) = origin {
        git(dir, &["remote", "add", "origin", url]);
    }
}

/// Run `tm --url <stub> <args>` with every prompt write failing, and return
/// the stub and the child's stderr after asserting a non-zero exit.
fn run_with_unwritable_prompt_dir(
    home: &Path,
    scratch: &Path,
    args: &[&str],
    extra_env: &[(&str, &Path)],
) -> (RecordingDaemon, String) {
    let not_a_dir = scratch.join("tmpdir-is-a-file");
    std::fs::write(&not_a_dir, "").expect("plant a file where TMPDIR points");
    let sock_dir = scratch.join("sock");
    std::fs::create_dir_all(&sock_dir).expect("mkdir socket dir");
    let daemon = RecordingDaemon::start(&sock_dir);
    let mut cmd = tm_command_in(home);
    cmd.arg("--url")
        .arg(&daemon.url)
        .args(args)
        .env("TRUSTY_MPM_SOCKET", &daemon.socket)
        .env("TMPDIR", &not_a_dir)
        .env("TM_DISABLE_SPAWN_DISCLAIM", "1");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn tm");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "a launch with no writable prompt file must fail; stderr: {stderr}"
    );
    (daemon, stderr)
}

/// Assert the child refused over the prompt file and registered nothing.
fn assert_refused_before_post(daemon: &RecordingDaemon, stderr: &str, action: &str) {
    assert!(
        stderr.contains("could not write the PM system-prompt file")
            && stderr.contains(&format!("refusing to {action}")),
        "the refusal must name the prompt file and the {action}: {stderr}"
    );
    assert_eq!(
        daemon.registrations(),
        Vec::<String>::new(),
        "the prompt refusal must come BEFORE any call registers a session; stderr: {stderr}"
    );
}

/// A scratch directory under `/private/tmp`, canonical so the paths `tm`
/// derives match the ones the test planted.
fn scratch() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("tm-test-8286-")
        .tempdir_in("/tmp")
        .expect("scratch dir");
    let canonical = dir.path().canonicalize().expect("canonical scratch");
    (dir, canonical)
}

/// #8286: `tm connect` refuses before `POST /api/v1/sessions/connect`.
#[test]
fn connect_refuses_an_unwritable_prompt_before_registering() {
    let (_guard, root) = scratch();
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let project = root.join("project");
    git_repo(&project, None);
    let (daemon, stderr) =
        run_with_unwritable_prompt_dir(&home, &root, &["connect", &project.to_string_lossy()], &[]);
    assert_refused_before_post(&daemon, &stderr, "connect");
}

/// #8286: the in-place `tm session start` refuses before `POST /sessions`,
/// which it sends over the daemon socket as `mpm.sessions.register`.
#[test]
fn session_start_refuses_an_unwritable_prompt_before_registering() {
    let (_guard, root) = scratch();
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    // No remote: the in-place path, not the managed route.
    let project = root.join("project");
    git_repo(&project, None);
    let (daemon, stderr) = run_with_unwritable_prompt_dir(
        &home,
        &root,
        &["session", "start", "--dir", &project.to_string_lossy()],
        &[],
    );
    assert_refused_before_post(&daemon, &stderr, "launch");
}

/// #8286: `tm launch` refuses before `POST /sessions`. FAILS ON c6c4a28860:
/// the prompt was written after the session was registered.
#[test]
fn launch_refuses_an_unwritable_prompt_before_registering() {
    let (_guard, root) = scratch();
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    // The checkout IS the managed checkout, so `tm launch` clones nothing.
    let repos = root.join("repos");
    let project = repos.join("acme").join("widget");
    git_repo(&project, Some("https://github.com/acme/widget.git"));
    let (daemon, stderr) = run_with_unwritable_prompt_dir(
        &home,
        &root,
        &["launch", &project.to_string_lossy()],
        &[("TRUSTY_MPM_REPOS_ROOT", &repos)],
    );
    assert_refused_before_post(&daemon, &stderr, "launch");
}
