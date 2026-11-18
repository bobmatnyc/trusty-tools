//! End-to-end smoke: the real binaries only, every path in a temp directory.
//!
//! `trusty-browser --state <tmp> serve` (with the `test-seams` env switch:
//! in-memory key store, open enrollment check), `native-host` driven by a
//! fake Chrome on its stdio for pairing, connect and the worker, then
//! `trusty-browser-mcp` in connect mode for `tools/list`, `browser_connect`,
//! `browser_select` and one `page_read`, and a clean shutdown that leaves no
//! child process and no socket behind.
//!
//! macOS only: pairing runs through popup enrollment, whose native-host side
//! (`peer_darwin::verify_enrollment_service`, Darwin `LOCAL_PEERPID`/libproc)
//! exists only on macOS; elsewhere it refuses every peer by design.
#![cfg(target_os = "macos")]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_browser::peer_darwin::ORIGIN;
use trusty_browser::proof::{Hello, Role, pairing_proof};

const BROWSER: &str = env!("CARGO_BIN_EXE_trusty-browser");
const MCP: &str = env!("CARGO_BIN_EXE_trusty-browser-mcp");
const PROFILE: &str = "0dcdaf05-6de3-4872-9ad0-186105244c23";
const EMAIL: &str = "alpha@example.test";
const KEY: &str = "bc1755f18c664ce836300569acb31692fca2cbfa21aeb25edf99aa8127951418";
const NONCE: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const WAIT: Duration = Duration::from_secs(20);

/// Every child the test starts; killed on drop so a failed assert leaks nothing.
#[derive(Default)]
struct Children(Vec<Child>);

impl Drop for Children {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn wait_exit(child: &mut Child, what: &str) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(
            start.elapsed() < WAIT,
            "{what} did not exit within {WAIT:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn env_for(cmd: &mut Command, home: &Path) {
    // HOME points into the temp tree so no default path reaches ~/.trusty.
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .stderr(Stdio::inherit());
}

fn write_frame(w: &mut impl Write, body: &Value) {
    let bytes = serde_json::to_vec(body).expect("json");
    w.write_all(&(bytes.len() as u32).to_ne_bytes())
        .expect("head");
    w.write_all(&bytes).expect("body");
    w.flush().expect("flush");
}

/// A reader thread turning native-message frames into parsed values.
fn frames(mut r: impl Read + Send + 'static) -> Receiver<Value> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        loop {
            let mut head = [0u8; 4];
            if r.read_exact(&mut head).is_err() {
                return;
            }
            let mut body = vec![0u8; u32::from_ne_bytes(head) as usize];
            if r.read_exact(&mut body).is_err() {
                return;
            }
            let v = serde_json::from_slice(&body).expect("frame json");
            if tx.send(v).is_err() {
                return;
            }
        }
    });
    rx
}

/// A `native-host` child, as Chrome launches it, for `socket`.
fn host(state: &Path, home: &Path, socket: &str) -> (Child, ChildStdin, Receiver<Value>) {
    let mut cmd = Command::new(BROWSER);
    cmd.arg("native-host")
        .arg("--socket")
        .arg(state.join(socket))
        .arg(ORIGIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    env_for(&mut cmd, home);
    let mut child = cmd.spawn().expect("spawn native-host");
    let stdin = child.stdin.take().expect("stdin");
    let rx = frames(child.stdout.take().expect("stdout"));
    (child, stdin, rx)
}

/// One popup op through a fresh native host on `enrollment.sock`.
fn popup(state: &Path, home: &Path, op: &str) -> Value {
    let (mut child, mut stdin, rx) = host(state, home, "enrollment.sock");
    write_frame(
        &mut stdin,
        &json!({"op": op, "profile": PROFILE, "email": EMAIL, "key": KEY}),
    );
    let reply = rx.recv_timeout(WAIT).expect("popup reply");
    drop(stdin);
    let status = wait_exit(&mut child, "enrollment native-host");
    assert!(status.success(), "enrollment native-host exited {status}");
    reply
}

/// The fake extension worker: answers every broker command until the host closes.
fn fake_worker(mut stdin: ChildStdin, rx: Receiver<Value>) -> JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut ops = Vec::new();
        while let Ok(cmd) = rx.recv() {
            let op = cmd["op"].as_str().unwrap_or_default().to_owned();
            let result = match op.as_str() {
                "page_tab" => json!({"access": true, "document": "doc-1",
                    "origin": "https://example.test", "title": "Example"}),
                "page_read" => json!({"document": "doc-1", "origin": "https://example.test",
                    "url": "https://example.test/a", "title": "Example", "text": "",
                    "elements": [{"ref": "e1", "role": "button", "name": "Go"}],
                    "truncated": false}),
                _ => json!({"document": "doc-1"}),
            };
            let reply = json!({"id": cmd["id"], "ok": true, "email": cmd["email"],
                "result": result, "tab": cmd["tab"]});
            write_frame(&mut stdin, &reply);
            ops.push(op);
        }
        ops
    })
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next: u64,
}

impl Mcp {
    fn send(&mut self, msg: &Value) {
        let mut line = serde_json::to_vec(msg).expect("json");
        line.push(b'\n');
        self.stdin.write_all(&line).expect("mcp write");
        self.stdin.flush().expect("mcp flush");
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let line = self.lines.recv_timeout(WAIT).expect("mcp reply");
        let v: Value = serde_json::from_str(&line).expect("mcp json");
        assert_eq!(v["id"], json!(id), "{v}");
        v
    }

    /// A tool's text payload; panics when the tool reports an error.
    fn tool(&mut self, name: &str, args: Value) -> String {
        let v = self.call("tools/call", json!({"name": name, "arguments": args}));
        let result = &v["result"];
        assert_ne!(result["isError"], json!(true), "{name}: {v}");
        result["content"][0]["text"]
            .as_str()
            .expect("text")
            .to_owned()
    }
}

fn private_tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let canon = fs::canonicalize(root.path()).expect("canon");
    let state = canon.join("state");
    let home = canon.join("home");
    for d in [&state, &home] {
        fs::create_dir(d).expect("mkdir");
        fs::set_permissions(d, fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    (root, state, home)
}

fn sockets(state: &Path) -> Vec<String> {
    use std::os::unix::fs::FileTypeExt;
    fs::read_dir(state)
        .expect("read state")
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_socket()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn smoke_serve_pair_connect_page_op_shutdown() {
    let (_root, state, home) = private_tree();
    let mut kids = Children::default();

    // 1. serve, argv exactly as launchd runs it so the host's broker pin passes.
    let mut cmd = Command::new(BROWSER);
    cmd.arg("--state")
        .arg(&state)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    env_for(&mut cmd, &home);
    cmd.env("TRUSTY_BROWSER_TEST_SEAMS", "1");
    kids.0.push(cmd.spawn().expect("spawn serve"));
    let start = Instant::now();
    while sockets(&state).len() < 4 {
        assert!(
            start.elapsed() < WAIT,
            "broker sockets: {:?}",
            sockets(&state)
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // 2. pairing through the native host, as the popup does it.
    let paired = popup(&state, &home, "pair");
    assert_eq!(paired, json!({"ok": true, "profile": PROFILE}));

    // 3. the worker's native port: hello, challenge, proof, ack.
    let (worker_child, mut worker_in, worker_rx) = host(&state, &home, "service.sock");
    kids.0.push(worker_child);
    write_frame(
        &mut worker_in,
        &json!({"type": "hello", "profile": PROFILE, "email": EMAIL,
            "incarnation": "inc-1", "nonce": NONCE}),
    );
    let challenge = worker_rx.recv_timeout(WAIT).expect("challenge");
    let server = challenge["nonce"].as_str().expect("nonce").to_owned();
    let fields = Hello {
        profile: PROFILE,
        email: EMAIL,
        incarnation: "inc-1",
        nonce: NONCE,
    };
    let proof = pairing_proof(KEY, Role::Extension, &fields, &server).expect("proof");
    write_frame(&mut worker_in, &json!({"type": "proof", "proof": proof}));
    let ack = worker_rx.recv_timeout(WAIT).expect("ack");
    assert_eq!(ack["ok"], json!(true), "{ack}");
    let worker = fake_worker(worker_in, worker_rx);

    // 4. the popup's connect grants this profile to agents.
    let connected = popup(&state, &home, "connect");
    assert_eq!(connected["ok"], json!(true), "{connected}");

    // 5. trusty-browser-mcp in connect mode.
    let mut cmd = Command::new(MCP);
    cmd.arg("--state")
        .arg(&state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    env_for(&mut cmd, &home);
    let mut child = cmd.spawn().expect("spawn mcp");
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
    let mut mcp = Mcp {
        child,
        stdin,
        lines,
        next: 0,
    };
    let init = mcp.call(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "smoke", "version": "0"}}),
    );
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        json!("trusty-browser")
    );
    mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let tools = mcp.call("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.contains(&"browser_connect") && names.contains(&"page_read"),
        "{names:?}"
    );

    let attached = mcp.tool("browser_connect", json!({}));
    assert!(attached.contains(PROFILE), "{attached}");
    let selected = mcp.tool("browser_select", json!({"tab": 7}));
    assert!(selected.contains("doc-1"), "{selected}");
    let lease: Value = serde_json::from_str(&selected).expect("select json");
    let read = mcp.tool("page_read", json!({"lease": lease["lease"]}));
    assert!(read.contains("https://example.test/a"), "{read}");

    // 6. clean shutdown: MCP on EOF, broker on SIGTERM, worker host on broker loss.
    let Mcp {
        mut child, stdin, ..
    } = mcp;
    drop(stdin);
    let status = wait_exit(&mut child, "trusty-browser-mcp");
    assert!(status.success(), "trusty-browser-mcp exited {status}");

    let serve_pid = kids.0[0].id() as libc::pid_t;
    // SAFETY: kill on our own live child's pid.
    assert_eq!(unsafe { libc::kill(serve_pid, libc::SIGTERM) }, 0);
    let status = wait_exit(&mut kids.0[0], "serve");
    assert!(status.success(), "serve exited {status}");
    let status = wait_exit(&mut kids.0[1], "worker native-host");
    assert!(status.success(), "worker native-host exited {status}");
    let ops = worker.join().expect("worker thread");
    assert_eq!(ops, ["page_tab", "page_read"]);

    assert_eq!(sockets(&state), Vec::<String>::new(), "leaked sockets");
    for c in &mut kids.0 {
        assert!(c.try_wait().expect("try_wait").is_some(), "leaked child");
    }
}
