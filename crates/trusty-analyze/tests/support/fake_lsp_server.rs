//! A scripted stdio language server for the LSP child-manager tests (#9078, L2).
//!
//! Why: CI has no `rust-analyzer`, and a real server's indexing makes timing
//! nondeterministic. This fake speaks just enough LSP, deterministically, and can
//! be told to stall, crash, fail its start-up, cut a frame short, ignore `exit`,
//! stop reading its stdin (`no_read`),
//! or leave a grandchild behind.
//! What: it runs inside the integration-test binary itself. `tests/lsp_child.rs`
//! re-executes its own executable through `/bin/sh` as
//! `<exe> --exact fake_lsp_server_entry --nocapture 3>&1 1>&2`, with the mode in
//! `TRUSTY_ANALYZE_FAKE_LSP`. libtest's own banner goes to stderr, so the LSP
//! frames this module writes to fd 3 are the only bytes on the child's stdout.
//! With the variable unset, [`run_if_requested`] returns at once and the entry
//! test is a no-op.
//! Test: exercised by every subprocess test in `tests/lsp_child.rs`.

use std::io::{BufRead, BufReader, Write};

use serde_json::{json, Value};

/// The mode variable; its presence turns the entry test into the server.
pub const MODE_ENV: &str = "TRUSTY_ANALYZE_FAKE_LSP";
/// Where `grandchild` mode writes the pid of the process it leaves behind.
pub const PIDFILE_ENV: &str = "TRUSTY_ANALYZE_FAKE_LSP_PIDFILE";
/// What `fail_init` prints, matching the mise-shim crash DOC-72 §2 records.
pub const FAIL_INIT_STDERR: &str = "error: infinite recursion detected";

/// Run as the fake server when [`MODE_ENV`] is set; otherwise return.
pub fn run_if_requested() {
    let Ok(mode) = std::env::var(MODE_ENV) else {
        return;
    };
    let code = serve(&mode);
    std::process::exit(code);
}

fn serve(mode: &str) -> i32 {
    if mode == "fail_init" {
        eprintln!("{FAIL_INIT_STDERR}");
        return 1;
    }
    let mut out = lsp_stdout();
    let mut input = BufReader::new(std::io::stdin().lock());
    while let Some(msg) = read_frame(&mut input) {
        let method = msg["method"].as_str().unwrap_or_default().to_string();
        let id = msg.get("id").cloned();
        match (method.as_str(), id) {
            ("initialize", Some(id)) => {
                if mode == "partial_frame" {
                    let _ = out.write_all(b"Content-Length: 200\r\n\r\n{\"jsonrpc\":");
                    let _ = out.flush();
                    return 0;
                }
                if mode == "grandchild" {
                    spawn_grandchild();
                }
                reply(
                    &mut out,
                    id,
                    json!({
                        "capabilities": {"referencesProvider": true, "textDocumentSync": 1},
                        "serverInfo": {"name": "fake-lsp"}
                    }),
                );
                if mode == "no_read" {
                    // #9078: answer the handshake, then never read stdin again,
                    // so the client's next large write fills the pipe.
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                    return 0;
                }
            }
            ("shutdown", Some(id)) => reply(&mut out, id, Value::Null),
            ("exit", None) if mode == "ignore_exit" => {}
            ("exit", None) => return 0,
            ("textDocument/didOpen" | "textDocument/didChange", None) => {
                let doc = &msg["params"]["textDocument"];
                send(
                    &mut out,
                    &json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": {"uri": doc["uri"], "version": doc["version"], "diagnostics": [{
                            "range": {"start": {"line": 0, "character": 0},
                                      "end": {"line": 0, "character": 1}},
                            "severity": 1,
                            "message": "canned diagnostic"
                        }]}
                    }),
                );
            }
            (_, None) => {}
            (_, Some(_)) if mode == "crash" => return 3,
            (_, Some(_)) if mode == "stall" => {}
            ("textDocument/references", Some(id)) => {
                let uri = msg["params"]["textDocument"]["uri"].clone();
                reply(
                    &mut out,
                    id,
                    json!([{"uri": uri, "range": {
                    "start": {"line": 1, "character": 2},
                    "end": {"line": 1, "character": 5}}}]),
                );
            }
            (_, Some(id)) => send(
                &mut out,
                &json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "method not found"}}),
            ),
        }
    }
    0
}

/// fd 3 is the child's real stdout (see the module doc). It is marked
/// close-on-exec so a grandchild cannot hold the client's pipe open.
fn lsp_stdout() -> std::fs::File {
    use std::os::fd::FromRawFd;
    // SAFETY: the `/bin/sh` wrapper in `tests/lsp_child.rs` always opens fd 3
    // (`3>&1`) before exec'ing this binary, and nothing else in this process
    // owns it.
    unsafe {
        libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC);
        std::fs::File::from_raw_fd(3)
    }
}

fn spawn_grandchild() {
    let child = std::process::Command::new("sleep")
        .arg("300")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn grandchild");
    if let Ok(path) = std::env::var(PIDFILE_ENV) {
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, child.id().to_string()).expect("write pidfile");
        std::fs::rename(&tmp, &path).expect("publish pidfile");
    }
    // Deliberately leaked: the manager must kill it through the process group.
    std::mem::forget(child);
}

fn read_frame(input: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                len = value.trim().parse::<usize>().ok();
            }
        }
    }
    let mut body = vec![0u8; len?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn reply(out: &mut impl Write, id: Value, result: Value) {
    send(out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn send(out: &mut impl Write, msg: &Value) {
    let body = serde_json::to_vec(msg).expect("encode");
    let _ = write!(out, "Content-Length: {}\r\n\r\n", body.len());
    let _ = out.write_all(&body);
    let _ = out.flush();
}
