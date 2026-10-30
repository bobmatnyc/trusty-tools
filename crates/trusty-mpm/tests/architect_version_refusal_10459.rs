//! An Architect launch on an old Claude Code refuses before any write (#10459).
//!
//! Why (DOC-86 §5, PR2 fix round): the Architect's PM guard carries
//! `onFailure: "block"`, which Claude Code honours from v2.1.295. Every seam
//! that writes that guard and starts `claude` must refuse an older one first,
//! or the guard is written for a `claude` that ignores it.
//! What: each case allow-lists a scratch project for the Architect profile,
//! puts a fake `claude` printing `2.1.294` first on `PATH`, points both daemon
//! transports at a stub, and runs one seam of the built `tm`. It asserts a
//! non-zero exit naming the floor, no `.claude/settings.json`, no
//! `.worktrees/` and no session registration.
//! Test: this file IS the test.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use crate::common::tm_command_in;

/// A daemon stand-in on HTTP and the unix socket that logs every call.
struct StubDaemon {
    url: String,
    socket: PathBuf,
    calls: Arc<Mutex<Vec<String>>>,
}

impl StubDaemon {
    fn start(dir: &Path) -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::clone(&calls);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve_http(&stream, &seen);
            }
        });
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let socket = dir.join("trusty-mpm.sock");
        let unix = UnixListener::bind(&socket).expect("bind stub socket");
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).expect("chmod");
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
        let calls = self.calls.lock().expect("calls");
        calls
            .iter()
            .filter(|c| c.starts_with("POST") || c.as_str() == "mpm.sessions.register")
            .cloned()
            .collect()
    }
}

fn serve_http(stream: &std::net::TcpStream, calls: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    calls
        .lock()
        .expect("calls")
        .push(request_line.trim().to_owned());
    let payload = if request_line.starts_with("GET") {
        "[]"
    } else {
        r#"{"name":"tm-10459-probe","id":null}"#
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

fn serve_rpc(stream: &std::os::unix::net::UnixStream, calls: &Mutex<Vec<String>>) {
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).is_err() {
        return;
    }
    let method = serde_json::from_str::<serde_json::Value>(&line)
        .ok()
        .and_then(|v| v["method"].as_str().map(str::to_owned))
        .unwrap_or_default();
    calls.lock().expect("calls").push(method.clone());
    let result = if method == "mpm.sessions.list" {
        serde_json::json!([])
    } else {
        serde_json::json!({ "name": "tm-10459-probe", "id": null })
    };
    let reply = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result });
    let _ = (&*stream).write_all(format!("{reply}\n").as_bytes());
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// A one-commit repository at `dir` asking for the Architect profile.
fn architect_repo(dir: &Path, origin: Option<&str>) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join(".trusty-mpm.toml"), "profile = \"supervisor\"\n").expect("profile");
    git(dir, &["add", ".trusty-mpm.toml"]);
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

/// A scratch root under `/tmp`, canonical, with `home/` allow-listing
/// `project` and `bin/claude` printing an old version.
fn scratch(project_rel: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let guard = tempfile::Builder::new()
        .prefix("tm-test-10459-")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = guard.path().canonicalize().expect("canonical");
    let project = root.join(project_rel);
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".trusty-mpm")).expect("home");
    std::fs::write(
        home.join(".trusty-mpm/config.toml"),
        format!(
            "[architect]\nprojects = [{:?}]\n",
            project.display().to_string()
        ),
    )
    .expect("grant");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    // See #10248
    trusty_common::exec_stub::write_executable(
        &bin.join("claude"),
        "#!/bin/sh\n[ \"$1\" = --version ] && { echo '2.1.294 (Claude Code)'; exit 0; }\nexit 9\n",
    )
    .expect("fake claude");
    (guard, root, project)
}

/// Run `tm --url <stub> <args>` and assert the refusal left nothing behind.
fn assert_refused(root: &Path, project: &Path, args: &[&str], extra_env: &[(&str, &Path)]) {
    let sock = root.join("sock");
    std::fs::create_dir_all(&sock).expect("sock dir");
    let daemon = StubDaemon::start(&sock);
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = tm_command_in(&root.join("home"));
    cmd.arg("--url")
        .arg(&daemon.url)
        .args(args)
        .env("TRUSTY_MPM_SOCKET", &daemon.socket)
        .env("PATH", path)
        .env("TM_DISABLE_SPAWN_DISCLAIM", "1");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn tm");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "{args:?} must fail; stderr: {stderr}"
    );
    assert!(stderr.contains("older than v2.1.295"), "{args:?}: {stderr}");
    assert!(
        !project.join(".claude/settings.json").exists(),
        "{args:?} wrote the hooks before refusing; stderr: {stderr}"
    );
    assert!(
        !project.join(".worktrees").exists(),
        "{args:?} created a worktree before refusing"
    );
    assert_eq!(daemon.registrations(), Vec::<String>::new(), "{stderr}");
}

#[test]
fn connect_refuses_an_old_claude_code_before_any_write() {
    let (_guard, root, project) = scratch("project");
    architect_repo(&project, None);
    assert_refused(
        &root,
        &project,
        &["connect", &project.to_string_lossy()],
        &[],
    );
}

#[test]
fn launch_refuses_an_old_claude_code_before_any_write() {
    // The checkout IS the managed checkout, so `tm launch` clones nothing.
    let (_guard, root, project) = scratch("repos/acme/widget");
    architect_repo(&project, Some("https://github.com/acme/widget.git"));
    let repos = root.join("repos");
    assert_refused(
        &root,
        &project,
        &["launch", &project.to_string_lossy()],
        &[("TRUSTY_MPM_REPOS_ROOT", &repos)],
    );
}

#[test]
fn session_start_refuses_an_old_claude_code_before_any_write() {
    let (_guard, root, project) = scratch("project");
    architect_repo(&project, None);
    assert_refused(
        &root,
        &project,
        &["session", "start", "--dir", &project.to_string_lossy()],
        &[],
    );
}
