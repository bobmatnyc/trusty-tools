//! #10153: `dbatg` MCP writes are bound to the bridge's own PM session.
//!
//! Why: DOC-82 LDG-3.6 — a PM writes only its own Ledger store. The `dbatg`
//! tool runs in the shared daemon, which cannot know the caller, so the
//! `tm serve --stdio` bridge checks. Before the fix the bridge forwarded every
//! write, and any PM could write any session's store through MCP.
//! What: a real `tm daemon` with a cleared environment (no `DBATG_SESSION`,
//! no `TMUX_PANE`), and a real `tm serve --stdio` bridge launched in a project
//! directory, whose `dbatg` calls travel as `mpm.mcp.dispatch` over the
//! daemon socket. Stores are created on disk first, so no test writes the
//! daemon's code registry.
//! Test: this file IS the test.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_architect::ledger::init_store;

use crate::common;

/// A child process killed when this value drops, pass or panic.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One JSON-RPC frame to `socket`; the one-line answer, if any.
fn socket_call(socket: &Path, method: &str) -> Option<Value> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}});
    stream.write_all(format!("{frame}\n").as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// A scratch home holding a daemon, two projects and three stores.
struct Fixture {
    home: tempfile::TempDir,
    socket: PathBuf,
    own: PathBuf,
    other: PathBuf,
    _daemon: Killed,
}

impl Fixture {
    /// Stores `pm-a` and `pm-b` in `own`, `pm-a` in `other`; a daemon serving.
    fn new() -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(home.path()).expect("canonical");
        let project = |name: &str| {
            let dir = root.join(name);
            std::fs::create_dir_all(dir.join(".git")).expect("project");
            dir
        };
        let (own, other) = (project("own"), project("other"));
        let registry = root.join("codes.json");
        for (dir, session, code) in [
            (&own, "pm-a", "ta"),
            (&own, "pm-b", "tb"),
            (&other, "pm-a", "tc"),
        ] {
            init_store(&registry, dir, session, code, "2026-11-25T09:00Z", &|_| {
                true
            })
            .expect("init store");
        }
        let data = root.join("data");
        // #10153: `apply_daemon_env` clears the env — the daemon names no PM.
        let child = common::daemon_command(&root, &root)
            .env("TRUSTY_DATA_DIR_OVERRIDE", &data)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a daemon");
        let daemon = Killed(child);
        let socket = data.join("trusty-mpm").join("trusty-mpm.sock");
        let deadline = Instant::now() + Duration::from_secs(30);
        while socket_call(&socket, "mpm.health").is_none_or(|r| r.get("result").is_none()) {
            assert!(
                Instant::now() < deadline,
                "no daemon on {}",
                socket.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        Self {
            home,
            socket,
            own,
            other,
            _daemon: daemon,
        }
    }

    /// A `tm serve --stdio` bridge launched in `own`, as `session` if `Some`.
    fn bridge(&self, session: Option<&str>) -> Bridge {
        let mut cmd = common::tm_command_in(self.home.path());
        cmd.current_dir(&self.own)
            .env("TRUSTY_MPM_SOCKET", &self.socket)
            .env_remove("DBATG_SESSION")
            .args(["serve", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(session) = session {
            cmd.env("DBATG_SESSION", session);
        }
        let mut child = cmd.spawn().expect("spawn the bridge");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Bridge {
            stdin,
            stdout,
            next_id: 1,
            _child: Killed(child),
        }
    }

    fn state(&self, project: &Path, session: &str) -> String {
        let path = project
            .join(".trusty-mpm/dbatg")
            .join(session)
            .join("state.json");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }
}

struct Bridge {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    _child: Killed,
}

impl Bridge {
    /// One `dbatg` tool call; the MCP tool result (`content`, `isError`).
    fn dbatg(&mut self, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                         "params": {"name": "dbatg", "arguments": arguments}});
        self.stdin
            .write_all(format!("{req}\n").as_bytes())
            .expect("request");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("reply");
        let reply: Value = serde_json::from_str(&line).expect("one JSON-RPC reply");
        assert_eq!(reply["id"], id, "{reply}");
        reply["result"].clone()
    }
}

fn add(project: &Path, session: &str, request_id: &str) -> Value {
    json!({"op": "batch", "request_id": request_id,
           "store": {"project": project, "session": session},
           "ops": [{"op": "add", "kind": "G", "title": "lorem"}]})
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap_or_default()
}

fn assert_refused(result: &Value, what: &str) {
    assert_eq!(result["isError"], true, "{what}: {result}");
    let text = text(result);
    assert!(
        text.contains("LDG-3.6") && text.ends_with("(exit 2)"),
        "{what}: {text}"
    );
}

/// LDG-3.6 and LDG-3.3: the bridge's own session writes and reads; another
/// session's store, or another project's, is refused and left unchanged.
#[test]
fn a_bridge_writes_its_own_store_and_refuses_a_foreign_one() {
    let fx = Fixture::new();
    let mut bridge = fx.bridge(Some("pm-a"));

    let own = bridge.dbatg(add(&fx.own, "pm-a", "r1"));
    assert_eq!(own["isError"], false, "{own}");
    let written: Value = serde_json::from_str(text(&own)).expect("batch reply");
    assert_eq!(written["events"][0]["id"], "ta-G01", "{written}");

    let before = (fx.state(&fx.own, "pm-b"), fx.state(&fx.other, "pm-a"));
    assert_refused(&bridge.dbatg(add(&fx.own, "pm-b", "r2")), "foreign session");
    assert_refused(
        &bridge.dbatg(add(&fx.other, "pm-a", "r3")),
        "foreign project",
    );
    let init = json!({"op": "init", "project": fx.own, "session": "pm-c", "code": "td"});
    assert_refused(&bridge.dbatg(init), "foreign init");
    let after = (fx.state(&fx.own, "pm-b"), fx.state(&fx.other, "pm-a"));
    assert_eq!(before, after, "a refused write reached a store");
    assert!(
        !fx.own.join(".trusty-mpm/dbatg/pm-c").exists(),
        "pm-c was created"
    );

    let show =
        json!({"op": "show", "brief": true, "store": {"project": fx.own, "session": "pm-a"}});
    let shown = bridge.dbatg(show);
    assert_eq!(shown["isError"], false, "{shown}");
    assert!(text(&shown).contains("ta-G01 lorem"), "{shown}");
    let foreign_read = json!({"op": "show", "store": {"project": fx.other, "session": "pm-a"}});
    let read = bridge.dbatg(foreign_read);
    assert_eq!(
        read["isError"], false,
        "a read of any store is allowed: {read}"
    );
}

/// Fail-open check: a bridge with no `DBATG_SESSION` and no tmux pane cannot
/// name its own store, so it refuses every write; reads still pass.
#[test]
fn a_bridge_with_no_known_session_refuses_every_dbatg_write() {
    let fx = Fixture::new();
    let mut bridge = fx.bridge(None);
    let before = fx.state(&fx.own, "pm-a");
    let refused = bridge.dbatg(add(&fx.own, "pm-a", "r1"));
    assert_refused(&refused, "unknown own session");
    assert!(
        text(&refused).contains("this session is unknown"),
        "{refused}"
    );
    assert_eq!(
        fx.state(&fx.own, "pm-a"),
        before,
        "the refused write landed"
    );
    let show = json!({"op": "show", "store": {"project": fx.own, "session": "pm-a"}});
    let read = bridge.dbatg(show);
    assert_eq!(read["isError"], false, "{read}");
}
