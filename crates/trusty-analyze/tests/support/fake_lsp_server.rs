//! A scripted stdio language server for the LSP child-manager tests (#9078, L2).
//!
//! Why: CI has no `rust-analyzer`, and a real server's indexing makes timing
//! nondeterministic. This fake speaks just enough LSP, deterministically, and can
//! be told to stall, crash, fail its start-up, cut a frame short, ignore `exit`,
//! stop reading its stdin (`no_read`),
//! or leave a grandchild behind. For the `analyze.lsp_*` methods (L3) it can
//! also publish diagnostics with no version (`unversioned`) and answer
//! `textDocument/references` with LSP error -32801 "content modified", either
//! twice and then normally (`content_modified`) or every time
//! (`content_modified_forever`). Every mode reports rust-analyzer's
//! `experimental/serverStatus` after `initialized`: quiescent at once, or, in
//! `loading`, not quiescent for [`LOADING_MS`] (answering `[]` and publishing
//! empty diagnostics meanwhile, as rust-analyzer does) and then quiescent with a
//! fresh publication; `never_ready` never becomes quiescent.
//! `unversioned_open_only` publishes an unversioned set on `didOpen` and
//! nothing on `didChange`.
//! What: it runs inside the integration-test binary itself. `tests/lsp_child.rs`
//! re-executes its own executable through `/bin/sh` as
//! `<exe> --exact fake_lsp_server_entry --nocapture 3>&1 1>&2`, with the mode in
//! `TRUSTY_ANALYZE_FAKE_LSP`. libtest's own banner goes to stderr, so the LSP
//! frames this module writes to fd 3 are the only bytes on the child's stdout.
//! With the variable unset, [`run_if_requested`] returns at once and the entry
//! test is a no-op.
//! Test: exercised by every subprocess test in `tests/lsp_child.rs` and
//! `tests/lsp_rpc.rs`.

#![allow(dead_code)] // each test binary compiles this module and uses a subset

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

/// How long `loading` mode stays not quiescent after `initialized`.
pub const LOADING_MS: u64 = 400;

/// The fake's stdout, shared with the `loading` timer thread; each frame is
/// written whole under the lock.
#[derive(Clone)]
struct Out(Arc<Mutex<std::fs::File>>);

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("out").write(buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.0.lock().expect("out").write_all(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().expect("out").flush()
    }
}

/// The last document opened or changed: `(uri, version)`.
type OpenDoc = Arc<Mutex<Option<(Value, Value)>>>;

/// The mode variable; its presence turns the entry test into the server.
pub const MODE_ENV: &str = "TRUSTY_ANALYZE_FAKE_LSP";
/// Where `grandchild` mode writes the pid of the process it leaves behind.
pub const PIDFILE_ENV: &str = "TRUSTY_ANALYZE_FAKE_LSP_PIDFILE";
/// What `fail_init` prints, matching the mise-shim crash DOC-72 §2 records.
pub const FAIL_INIT_STDERR: &str = "error: infinite recursion detected";

/// LSP error code a server sends while its workspace is still loading.
pub const CONTENT_MODIFIED: i64 = -32801;
/// How many `textDocument/references` requests `content_modified` refuses.
pub const CONTENT_MODIFIED_REFUSALS: u32 = 2;

/// A command that re-executes the current test binary as the fake in `mode`.
///
/// The binary must define a `fake_lsp_server_entry` test that calls
/// [`run_if_requested`]; `pidfile` is where `grandchild` mode writes its pid.
pub fn command(mode: &str, pidfile: &Path) -> trusty_analyze::lsp::LspCommand {
    let exe = std::env::current_exe().expect("current_exe");
    let script = format!(
        "{MODE_ENV}=\"$1\" {PIDFILE_ENV}=\"$2\" exec \"$0\" --exact fake_lsp_server_entry \
         --nocapture --test-threads=1 3>&1 1>&2"
    );
    trusty_analyze::lsp::LspCommand {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            script,
            exe.display().to_string(),
            mode.into(),
            pidfile.display().to_string(),
        ],
    }
}

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
    let mut out = Out(Arc::new(Mutex::new(lsp_stdout())));
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut references_seen = 0u32;
    let ready = Arc::new(AtomicBool::new(false));
    let open_doc: OpenDoc = Arc::new(Mutex::new(None));
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
            ("initialized", None) => on_initialized(mode, &out, &ready, &open_doc),
            ("textDocument/didOpen" | "textDocument/didChange", None) => {
                let doc = &msg["params"]["textDocument"];
                // Held across the publish so the `loading` timer cannot flip
                // readiness between the check and the frame.
                let mut last = open_doc.lock().expect("open_doc");
                *last = Some((doc["uri"].clone(), doc["version"].clone()));
                if !(mode == "unversioned_open_only" && method == "textDocument/didChange") {
                    let loaded = ready.load(Ordering::SeqCst);
                    publish(&mut out, mode, &doc["uri"], &doc["version"], loaded);
                }
            }
            (_, None) => {}
            (_, Some(_)) if mode == "crash" => return 3,
            (_, Some(_)) if mode == "stall" => {}
            // Still loading: rust-analyzer answers an empty list.
            ("textDocument/references", Some(id)) if !ready.load(Ordering::SeqCst) => {
                reply(&mut out, id, json!([]));
            }
            ("textDocument/references", Some(id))
                if mode == "content_modified_forever"
                    || (mode == "content_modified"
                        && references_seen < CONTENT_MODIFIED_REFUSALS) =>
            {
                references_seen += 1;
                send(
                    &mut out,
                    &json!({"jsonrpc": "2.0", "id": id,
                            "error": {"code": CONTENT_MODIFIED, "message": "content modified"}}),
                );
            }
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

/// Report readiness after `initialized`; `loading` flips to quiescent on a timer
/// and re-publishes the open document, as rust-analyzer does after a load.
fn on_initialized(mode: &str, out: &Out, ready: &Arc<AtomicBool>, open_doc: &OpenDoc) {
    let mut out = out.clone();
    if mode != "loading" && mode != "never_ready" {
        ready.store(true, Ordering::SeqCst);
        server_status(&mut out, true);
        return;
    }
    server_status(&mut out, false);
    if mode == "never_ready" {
        return;
    }
    let (ready, open_doc, mode) = (Arc::clone(ready), Arc::clone(open_doc), mode.to_string());
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(LOADING_MS));
        let last = open_doc.lock().expect("open_doc");
        ready.store(true, Ordering::SeqCst);
        server_status(&mut out, true);
        if let Some((uri, version)) = last.as_ref() {
            publish(&mut out, &mode, uri, version, true);
        }
    });
}

fn server_status(out: &mut Out, quiescent: bool) {
    send(
        out,
        &json!({"jsonrpc": "2.0", "method": "experimental/serverStatus",
                "params": {"health": "ok", "quiescent": quiescent}}),
    );
}

/// Publish diagnostics for one document: the canned one once loaded, an
/// empty set while loading; no version in the `unversioned*` modes.
fn publish(out: &mut impl Write, mode: &str, uri: &Value, version: &Value, loaded: bool) {
    let diagnostics = if loaded {
        json!([{
            "range": {"start": {"line": 0, "character": 0},
                      "end": {"line": 0, "character": 1}},
            "severity": 1,
            "code": "E0001",
            "source": "fake-lsp",
            "message": "canned diagnostic"
        }])
    } else {
        json!([])
    };
    let mut params = json!({"uri": uri, "version": version, "diagnostics": diagnostics});
    if mode.starts_with("unversioned") {
        // rust-analyzer's cargo-check pass publishes with no version.
        if let Some(p) = params.as_object_mut() {
            p.remove("version");
        }
    }
    send(
        out,
        &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                "params": params}),
    );
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

/// Write one whole frame in a single `write_all`, so a frame from the
/// `loading` timer thread never interleaves with one from the main loop.
fn send(out: &mut impl Write, msg: &Value) {
    let body = serde_json::to_vec(msg).expect("encode");
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    let _ = out.write_all(&frame);
    let _ = out.flush();
}
