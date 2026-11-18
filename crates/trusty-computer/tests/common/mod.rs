//! Fake cua-driver daemon and fixture state for tests (port of the spike's
//! `tests/fake_driver.py` and `fixture/fixture_app.py`).
//!
//! Why: no test may reach the real cua-driver or Bob's desktop (issue #11460);
//! every broker, supervisor and redaction test talks to this fake instead.
//! What: [`FixtureState`] holds a field's text and a button's press count and
//! writes them atomically to a JSON readback file. [`FakeDaemon`] serves the
//! daemon's NDJSON wire format (`metadata`, `call`) on a Unix socket under a
//! temp dir, records every call, and drives the fixture on `click` and
//! `type_text` / `set_value`. [`window_state`] is the snapshot it answers.
//! Test: `fake_daemon_speaks_ndjson_and_drives_fixture` in `tests/spike_fixture.rs`.
#![allow(dead_code)] // Each test target uses a different part of the harness.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use serde_json::{Value, json};

/// The fixture app's observable state.
#[derive(Debug, Clone)]
pub struct FixtureState {
    pub readback: PathBuf,
    pub title: String,
    pub text: String,
    pub clicks: u64,
    pub keystrokes: u64,
}

impl FixtureState {
    pub fn new(readback: &Path) -> FixtureState {
        FixtureState {
            readback: readback.to_owned(),
            title: "trusty-computer fixture".to_owned(),
            text: String::new(),
            clicks: 0,
            keystrokes: 0,
        }
    }

    pub fn set_text(&mut self, text: &str) {
        let grown = text
            .chars()
            .count()
            .saturating_sub(self.text.chars().count());
        self.keystrokes += grown as u64;
        self.text = text.to_owned();
        self.flush();
    }

    pub fn press(&mut self) {
        self.clicks += 1;
        self.flush();
    }

    /// Write the readback atomically (temp file, then rename).
    pub fn flush(&self) {
        let data = json!({
            "title": self.title, "text": self.text, "clicks": self.clicks,
            "keystrokes": self.keystrokes, "pid": std::process::id(),
        });
        let tmp = self.readback.with_extension("tmp");
        fs::write(&tmp, data.to_string()).expect("write fixture readback");
        fs::rename(&tmp, &self.readback).expect("rename fixture readback");
    }
}

/// The readback file as JSON.
pub fn read_state(readback: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(readback).expect("read readback")).expect("json")
}

/// A window with a field, a counting button, an inert button and a menu bar.
pub fn window_state(clicks: u64) -> Value {
    let tree = [
        "- [0] AXWindow \"fixture\" [actions=[raise]]".to_owned(),
        "  - [1] AXTextField = \"\" [actions=[confirm]]".to_owned(),
        format!("  - AXStaticText = \"clicks={clicks}\""),
        "  - [2] AXButton \"Press Me\" [actions=[press]]".to_owned(),
        "  - [3] AXButton \"Inert\" [actions=[press]]".to_owned(),
        "- [4] AXMenuBar [actions=[cancel]]".to_owned(),
        "  - [5] AXMenuBarItem \"Apple\" [actions=[press]]".to_owned(),
        "    - [6] AXMenuItem \"Recent Items\" [actions=[press]]".to_owned(),
        "    - [7] AXMenuItem \"Shut Down\" [actions=[press]]".to_owned(),
    ]
    .join("\n");
    // (element_index, role, label, parent_index, element_token)
    type Row = (
        i64,
        &'static str,
        Option<&'static str>,
        Option<i64>,
        &'static str,
    );
    let rows: [Row; 8] = [
        (0, "AXWindow", Some("fixture"), None, "tok-window"),
        (1, "AXTextField", Some(""), Some(0), "tok-field"),
        (2, "AXButton", Some("Press Me"), Some(0), "tok-button"),
        (3, "AXButton", Some("Inert"), Some(0), "tok-inert"),
        (4, "AXMenuBar", None, None, "tok-menubar"),
        (5, "AXMenuBarItem", Some("Apple"), Some(4), "tok-apple"),
        (6, "AXMenuItem", Some("Recent Items"), Some(5), "tok-recent"),
        (7, "AXMenuItem", Some("Shut Down"), Some(5), "tok-shut"),
    ];
    let elements: Vec<Value> = rows
        .iter()
        .map(|(i, r, l, p, t)| {
            json!({"element_index": i, "role": r, "label": l, "parent_index": p, "element_token": t})
        })
        .collect();
    json!({
        "content": [{"type": "text", "text": format!("Window state\n{tree}")}],
        "structuredContent": {
            "elements": elements, "tree_markdown": tree,
            "element_count": elements.len(), "returned_element_count": elements.len(),
        },
    })
}

#[derive(Debug, Default)]
struct Shared {
    calls: Vec<(String, Value)>,
    fixture: Option<FixtureState>,
}

/// A fake daemon serving NDJSON on a Unix socket.
pub struct FakeDaemon {
    pub socket_path: PathBuf,
    pub version: String,
    shared: Arc<Mutex<Shared>>,
}

impl FakeDaemon {
    /// Bind `socket_path` and serve each connection on its own thread.
    pub fn start(socket_path: &Path, version: &str, fixture: Option<FixtureState>) -> FakeDaemon {
        let listener = UnixListener::bind(socket_path).expect("bind fake daemon socket");
        let shared = Arc::new(Mutex::new(Shared {
            calls: Vec::new(),
            fixture,
        }));
        let (state, version_owned) = (Arc::clone(&shared), version.to_owned());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (state, version) = (Arc::clone(&state), version_owned.clone());
                thread::spawn(move || serve(stream, &state, &version));
            }
        });
        FakeDaemon {
            socket_path: socket_path.to_owned(),
            version: version.to_owned(),
            shared,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().expect("fake daemon lock")
    }

    /// Every `call` received, in order.
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.lock().calls.clone()
    }

    /// The fixture's current state.
    pub fn fixture(&self) -> Option<FixtureState> {
        self.lock().fixture.clone()
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn serve(stream: UnixStream, shared: &Mutex<Shared>, version: &str) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) => answer(&req, shared, version),
            Err(e) => json!({"ok": false, "error": format!("bad json: {e}"), "exit_code": 64}),
        };
        if writeln!(writer, "{reply}").is_err() {
            return;
        }
    }
}

fn answer(req: &Value, shared: &Mutex<Shared>, version: &str) -> Value {
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method == "metadata" {
        return json!({"ok": true, "result": {"driver_version": version, "pid": 4242, "embedded": false}});
    }
    if method != "call" {
        return json!({"ok": false, "error": format!("unknown method {method}"), "exit_code": 64});
    }
    let name = req.get("name").and_then(Value::as_str).unwrap_or_default();
    let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
    let mut state = shared.lock().expect("fake daemon lock");
    state.calls.push((name.to_owned(), args.clone()));
    if name == "get_window_state" {
        let clicks = state.fixture.as_ref().map_or(0, |f| f.clicks);
        return json!({"ok": true, "result": window_state(clicks)});
    }
    let token = args.get("element_token").and_then(Value::as_str);
    if let Some(fixture) = state.fixture.as_mut() {
        if name == "click" && token == Some("tok-button") {
            fixture.press();
        } else if matches!(name, "type_text" | "set_value") && token == Some("tok-field") {
            let add = args
                .get("text")
                .or_else(|| args.get("value"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let text = format!("{}{add}", fixture.text);
            fixture.set_text(&text);
        }
    }
    json!({"ok": true, "result": {"tool": name, "ok": true}})
}

/// Send one NDJSON request to `socket` and read one reply line.
pub fn ndjson_call(socket: &Path, request: &Value) -> Value {
    let mut stream = UnixStream::connect(socket).expect("connect");
    writeln!(stream, "{request}").expect("send");
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).expect("read");
    serde_json::from_str(&line).expect("reply json")
}

/// A short temp dir under `/tmp`: macOS caps `sun_path` at 104 bytes.
pub fn short_tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("tc-")
        .tempdir_in("/tmp")
        .expect("temp dir")
}
