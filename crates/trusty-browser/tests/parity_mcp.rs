//! Phase 5 parity: the `trusty-browser-mcp` binary against Python 0.7.0.
//!
//! The fixtures under `tests/fixtures/mcp/` were captured from the Python
//! `trusty-browser-mcp` by `capture/capture.py` (`initialize`, `tools/list`)
//! and `capture/capture_mcp.py` (tool-call exchanges against a scripted
//! `agent.sock`, and the argument-validation table). Each replay runs the Rust
//! binary against the same scripted socket and compares every MCP line and
//! every frame it sends. No broker, vault or keychain is involved.

mod common;

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{items, json, read};
use trusty_browser::mcp_args::validate;
use trusty_browser::mcp_tools::{CONNECT, WORKFLOW};
use trusty_browser::pyjson::Json;

const EXE: &str = env!("CARGO_BIN_EXE_trusty-browser-mcp");

/// Scripted replies (`None` closes the connection) and the frames received.
#[derive(Default)]
struct Script {
    replies: VecDeque<Option<String>>,
    frames: Vec<(String, Option<String>)>,
}

/// A scripted `agent.sock`, like `capture_mcp.py`'s `FakeBroker`.
struct FakeBroker {
    path: PathBuf,
    script: Arc<Mutex<Script>>,
}

impl FakeBroker {
    fn start(path: &Path) -> FakeBroker {
        let listener = UnixListener::bind(path).expect("bind agent.sock");
        let script = Arc::new(Mutex::new(Script::default()));
        let shared = Arc::clone(&script);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || serve(conn, &shared));
            }
        });
        FakeBroker {
            path: path.to_path_buf(),
            script,
        }
    }

    fn queue(&self, replies: Vec<Option<String>>) {
        self.script.lock().expect("lock").replies = replies.into();
    }

    fn take(&self) -> (Vec<(String, Option<String>)>, usize) {
        let mut s = self.script.lock().expect("lock");
        (std::mem::take(&mut s.frames), s.replies.len())
    }

    /// New connections fail from now on, as after Python's `stop()`.
    fn stop(&self) {
        fs::remove_file(&self.path).expect("unlink agent.sock");
    }
}

fn serve(mut conn: UnixStream, script: &Mutex<Script>) {
    loop {
        let mut head = [0u8; 4];
        if conn.read_exact(&mut head).is_err() {
            return;
        }
        let mut body = vec![0u8; u32::from_ne_bytes(head) as usize];
        if conn.read_exact(&mut body).is_err() {
            return;
        }
        let reply = {
            let mut s = script.lock().expect("lock");
            let reply = s.replies.pop_front().expect("an unscripted frame was sent");
            let request = String::from_utf8(body).expect("utf-8 frame");
            s.frames.push((request, reply.clone()));
            reply
        };
        let Some(reply) = reply else { return };
        let mut out = (reply.len() as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(reply.as_bytes());
        if conn.write_all(&out).is_err() {
            return;
        }
    }
}

/// A running `trusty-browser-mcp` with a line reader that cannot hang the test.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Mcp {
    fn spawn(args: &[&str]) -> Mcp {
        let mut child = Command::new(EXE)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn trusty-browser-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Mcp {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).expect("write");
        self.stdin.write_all(b"\n").expect("write");
        self.stdin.flush().expect("flush");
    }

    fn recv(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(20))
            .expect("an MCP reply within 20 s")
    }

    fn finish(mut self) {
        drop(self.stdin);
        let status = self.child.wait().expect("wait");
        assert!(status.success(), "trusty-browser-mcp exited {status}");
    }
}

fn uuid4_like(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && [8, 13, 18, 23].iter().all(|&i| b[i] == b'-')
        && b[14] == b'4'
        && b"89ab".contains(&b[19])
        && s.bytes()
            .enumerate()
            .all(|(i, c)| [8, 13, 18, 23].contains(&i) || c.is_ascii_hexdigit())
}

/// Replace each `"request_id": "<36-char uuid4>"` with `<uuid4>`, collecting the ids.
fn mask_uuids(text: &str, seen: &mut Vec<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('"') {
        out.push_str(&rest[..=i]);
        rest = &rest[i + 1..];
        if rest.len() >= 37 && uuid4_like(&rest[..36]) && rest.as_bytes()[36] == b'"' {
            seen.push(rest[..36].to_owned());
            out.push_str("<uuid4>");
            rest = &rest[36..];
        }
    }
    out.push_str(rest);
    out
}

struct Roots {
    shown: String,
    real: String,
}

impl Roots {
    fn norm(&self, text: &str, seen: &mut Vec<String>) -> String {
        // Linux has no `/private` alias, so the real root can equal the shown one;
        // then every hit is the path the binary was given, which is `<ROOT>`.
        let text = if self.real == self.shown {
            text.to_owned()
        } else {
            text.replace(&self.real, "<REALROOT>")
        };
        mask_uuids(&text.replace(&self.shown, "<ROOT>"), seen)
    }
}

/// Replay one captured session; returns every fresh uuid4 the binary generated.
fn replay(name: &str, args: &[&str], broker: &FakeBroker, roots: &Roots) -> Vec<String> {
    let steps = json(&format!("mcp/{name}.exchange.json"));
    let steps = items(&steps);
    let replies = |step: &Json| -> Vec<Option<String>> {
        items(step.get("frames").expect("frames"))
            .iter()
            .map(|f| f.get("reply").and_then(Json::as_str).map(str::to_owned))
            .collect()
    };
    broker.queue(replies(&steps[0]));
    let mut mcp = Mcp::spawn(args);
    let mut seen = Vec::new();
    for (n, step) in steps.iter().enumerate() {
        if step.get("action").and_then(Json::as_str) == Some("stop") {
            broker.stop();
        }
        if n > 0 {
            broker.queue(replies(step));
        }
        let send = common::s(step, "send").replace("<ROOT>", &roots.shown);
        mcp.send(&send);
        let want = step.get("recv").and_then(Json::as_str);
        if let Some(want) = want {
            let got = roots.norm(&mcp.recv(), &mut seen);
            assert_eq!(got, want, "{name} step {n}: {send}");
        }
        let (frames, left) = broker.take();
        assert_eq!(left, 0, "{name} step {n}: scripted replies not consumed");
        let want: Vec<(String, Option<String>)> = items(step.get("frames").expect("frames"))
            .iter()
            .map(|f| {
                let request = common::s(f, "request").to_owned();
                (
                    request,
                    f.get("reply").and_then(Json::as_str).map(str::to_owned),
                )
            })
            .collect();
        let got: Vec<(String, Option<String>)> = frames
            .into_iter()
            .map(|(r, rep)| (roots.norm(&r, &mut seen), rep))
            .collect();
        assert_eq!(got, want, "{name} step {n}: agent.sock frames for {send}");
    }
    mcp.finish();
    seen
}

fn private_dir() -> tempfile::TempDir {
    // Under /tmp, so the upload root has the /tmp vs /private/tmp alias Python saw.
    let dir = tempfile::Builder::new()
        .prefix("tb-mcp5-")
        .tempdir_in("/tmp")
        .expect("tempdir");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

/// The upload tree of `capture_mcp.py` `make_root`.
fn make_root(base: &Path) -> PathBuf {
    let root = base.join("up");
    fs::create_dir_all(root.join("d")).expect("mkdir");
    let files: [(&str, Vec<u8>); 8] = [
        ("a.txt", b"hello\n".to_vec()),
        ("d/b.png", (0..=255u8).collect()),
        ("X.JSON", b"{}".to_vec()),
        ("noext", b"n".to_vec()),
        ("t.tar.gz", b"t".to_vec()),
        ("big.bin", vec![1u8; 600 * 1024]),
        ("h1.bin", vec![2u8; 300 * 1024]),
        ("h2.bin", vec![3u8; 300 * 1024]),
    ];
    for (name, data) in files {
        fs::write(root.join(name), data).expect("write");
    }
    std::os::unix::fs::symlink("/etc/hosts", root.join("link")).expect("symlink");
    std::os::unix::fs::symlink(root.join("d"), root.join("dlink")).expect("symlink");
    let fifo = std::ffi::CString::new(root.join("fifo").to_str().expect("utf-8")).expect("cstr");
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0, "mkfifo");
    fs::write(root.join("locked"), b"x").expect("write");
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).expect("chmod");
    root
}

#[test]
fn tools_list_and_initialize_are_byte_identical() {
    let init = r#"{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fixture-capture", "version": "0"}}}"#;
    let list = r#"{"jsonrpc": "2.0", "id": 2, "method": "tools/list"}"#;
    let dir = private_dir();
    let state = dir.path().to_str().expect("utf-8").to_owned();
    let cred = dir.path().join("cred.json");
    fs::write(
        &cred,
        br#"{"workflow": "fixture", "ticket": "plain-ticket"}"#,
    )
    .expect("write");
    fs::set_permissions(&cred, fs::Permissions::from_mode(0o600)).expect("chmod");
    let broker = FakeBroker::start(&dir.path().join("agent.sock"));
    for mode in ["connect", "workflow"] {
        let mut args = vec!["--state", state.as_str()];
        if mode == "workflow" {
            broker.queue(vec![Some(r#"{"ok":true}"#.to_owned())]);
            args.extend(["--credential", cred.to_str().expect("utf-8")]);
        }
        let mut mcp = Mcp::spawn(&args);
        for (msg, fixture) in [(init, "initialize"), (list, "tools_list")] {
            mcp.send(msg);
            mcp.send(r#"{"jsonrpc": "2.0", "method": "notifications/initialized"}"#);
            let want = read(&format!("mcp/{mode}.{fixture}.json"));
            let mut got = mcp.recv().into_bytes();
            got.push(b'\n');
            assert_eq!(
                got, want,
                "{mode} {fixture} differs from the Python fixture"
            );
        }
        mcp.finish();
    }
}

#[test]
fn connect_exchange_replays_byte_identically() {
    let dir = private_dir();
    let root = make_root(dir.path());
    let roots = Roots {
        shown: root.to_str().expect("utf-8").to_owned(),
        real: fs::canonicalize(&root)
            .expect("realpath")
            .to_str()
            .expect("utf-8")
            .to_owned(),
    };
    let state = dir.path().join("state");
    fs::create_dir(&state).expect("mkdir");
    let broker = FakeBroker::start(&state.join("agent.sock"));
    let args = [
        "--state",
        state.to_str().expect("utf-8"),
        "--upload-root",
        &roots.shown,
    ];
    let fresh = replay("connect", &args, &broker, &roots);
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o600)).expect("chmod");
    // Each omitted request_id was a new uuid4 (plan section 3, item 14).
    assert!(fresh.len() >= 6, "fresh ids: {fresh:?}");
    let mut unique = fresh.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), fresh.len(), "a uuid4 repeated: {fresh:?}");
}

#[test]
fn noroot_exchange_replays_byte_identically() {
    let dir = private_dir();
    let broker = FakeBroker::start(&dir.path().join("agent.sock"));
    let roots = Roots {
        shown: "<unused>".into(),
        real: "<unused>".into(),
    };
    replay(
        "noroot",
        &["--state", dir.path().to_str().expect("utf-8")],
        &broker,
        &roots,
    );
}

#[test]
fn workflow_exchange_replays_byte_identically() {
    let dir = private_dir();
    let cred = dir.path().join("cred.json");
    fs::write(
        &cred,
        br#"{"workflow": "fixture", "ticket": "plain-ticket"}"#,
    )
    .expect("write");
    fs::set_permissions(&cred, fs::Permissions::from_mode(0o600)).expect("chmod");
    let broker = FakeBroker::start(&dir.path().join("agent.sock"));
    let roots = Roots {
        shown: "<unused>".into(),
        real: "<unused>".into(),
    };
    let state = dir.path().to_str().expect("utf-8");
    let fresh = replay(
        "workflow",
        &[
            "--state",
            state,
            "--credential",
            cred.to_str().expect("utf-8"),
        ],
        &broker,
        &roots,
    );
    assert_eq!(fresh.len(), 2, "fresh ids: {fresh:?}");
    assert_ne!(fresh[0], fresh[1]);
}

#[test]
fn validation_matches_pydantic_for_every_captured_case() {
    let rows = json("mcp/validation.json");
    let mut checked = 0;
    for row in items(&rows) {
        let tool = common::s(row, "tool");
        let args = row.get("arguments").expect("arguments");
        // Python ints are unbounded; this port stops at u64 (module doc of mcp_args).
        if args
            .dumps(false)
            .ok()
            .is_some_and(|a| a.contains(&"1".repeat(30)))
        {
            continue;
        }
        let spec = CONNECT
            .iter()
            .chain(WORKFLOW)
            .find(|t| t.name == tool)
            .expect("tool in the table");
        let got = validate(tool, spec.fields, args);
        match (row.get("ok"), row.get("error").and_then(Json::as_str)) {
            (Some(ok), None) => {
                let got = got.unwrap_or_else(|e| panic!("{tool} {args:?}: {e}"));
                let got = Json::object(got);
                assert_eq!(
                    got.dumps(false).ok(),
                    ok.dumps(false).ok(),
                    "{tool} {args:?}"
                );
            }
            (None, Some(err)) => assert_eq!(got.err().as_deref(), Some(err), "{tool} {args:?}"),
            _ => panic!("bad row {row:?}"),
        }
        checked += 1;
    }
    assert!(checked > 120, "only {checked} rows");
}

fn run_status(args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(EXE)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn bad_flags_and_credentials_are_refused() {
    let dir = private_dir();
    let state = dir.path().to_str().expect("utf-8");
    let missing = dir.path().join("nope");
    let (code, err) = run_status(&[
        "--state",
        state,
        "--upload-root",
        missing.to_str().expect("utf-8"),
    ]);
    assert_eq!(code, Some(2));
    assert!(err.contains("is not a directory"), "{err}");
    let (code, err) = run_status(&["--state", state, "--workflow", "Bad"]);
    assert_eq!(
        (code, err.contains("invalid workflow name")),
        (Some(2), true),
        "{err}"
    );
    let (code, _) = run_status(&["--state", state, "--workflow", "a", "--credential", "x"]);
    assert_eq!(code, Some(2));
    let cred = dir.path().join("cred.json");
    fs::write(&cred, br#"{"workflow": "w", "ticket": "t"}"#).expect("write");
    fs::set_permissions(&cred, fs::Permissions::from_mode(0o644)).expect("chmod");
    let (code, err) = run_status(&[
        "--state",
        state,
        "--credential",
        cred.to_str().expect("utf-8"),
    ]);
    assert_eq!(code, Some(1));
    assert!(
        err.contains("Use an owner-only workflow credential file (0600)"),
        "{err}"
    );
}
