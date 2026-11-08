//! Shared fixtures for the adapter conformance suite (#10746 S13): a fake
//! Google HTTP server, a seeded token store, and import runs through the
//! real adapter binaries.
//!
//! [`FakeGoogle`] is a loopback HTTP/1.1 server the adapters reach through
//! `--api-base` (`BaseClient::with_loopback_root`). It serves a mailbox
//! (`messages.list` honouring `in:sent`, `after:`/`before:`, `maxResults`
//! and its own page tokens; `messages.get`) and a Drive (`files.list` by
//! parent with `trashed = false`, `pageSize` and page tokens; `files.get`,
//! of a listed file or a root folder;
//! `files.export` by MIME type). It answers 401 to any request without the
//! seeded bearer token, records every request, can hold a request whose
//! URL contains a marker until released, and can fail one with a status.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};
use trusty_okf::OkfError;
use trusty_okf::build::registry::{ConnectorParams, Locator, SourceSpec};
use trusty_okf::bundle::Bundle;
use trusty_okf::extract::NoWorker;
use trusty_okf::import::adapter::{AdapterCommand, AdapterLimits, AdapterSource};
use trusty_okf::import::{ImportReport, ImportSpec, ItemReport, import, plan_import};

/// The access token the seeded store holds and the fake accepts.
pub const TOKEN: &str = "stub-access-token";
/// The MIME type of a native Google Doc.
pub const DOC: &str = "application/vnd.google-apps.document";
pub const GMAIL: &str = env!("CARGO_BIN_EXE_okf-adapter-gmail");
pub const GDOCS: &str = env!("CARGO_BIN_EXE_okf-adapter-gdocs");

/// One request the fake received.
#[derive(Debug, Clone)]
pub struct Seen {
    pub path: String,
    pub query: BTreeMap<String, String>,
    pub auth: Option<String>,
}

/// The fake's data and controls.
#[derive(Default)]
pub struct State {
    /// `format=full` messages, in listing order.
    pub messages: Vec<Value>,
    /// Drive file metadata, each with `parents`.
    pub files: Vec<Value>,
    /// #10746: folders `files.get` answers but no listing returns: the
    /// roots a selection names. `start` seeds folder `F`.
    pub roots: Vec<Value>,
    /// `(file id, MIME)` to export bytes.
    pub exports: HashMap<(String, String), Vec<u8>>,
    /// A request whose path and query contain this waits until it is
    /// cleared.
    pub hold: Option<String>,
    /// A request whose path and query contain the key gets that status.
    pub fail: Vec<(String, u16)>,
    /// A fault: `messages.list` ignores `in:sent`.
    pub ignore_sent: bool,
    /// The most entries one list page returns, below the asked size.
    pub page_cap: Option<usize>,
    pub seen: Vec<Seen>,
}

pub struct FakeGoogle {
    pub url: String,
    pub state: Arc<Mutex<State>>,
}

fn decode(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(h, 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn day_ms(d: &str) -> i64 {
    let date = chrono::NaiveDate::parse_from_str(d, "%Y/%m/%d").unwrap();
    date.and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

fn ms_of(m: &Value) -> i64 {
    m["internalDate"].as_str().unwrap().parse().unwrap()
}

fn page<T: Clone>(
    all: &[T],
    q: &BTreeMap<String, String>,
    size_key: &str,
    cap: Option<usize>,
) -> (Vec<T>, Option<String>) {
    let from: usize = q
        .get("pageToken")
        .map(|t| t.trim_start_matches('o').parse().unwrap())
        .unwrap_or(0);
    let size: usize = q.get(size_key).map(|s| s.parse().unwrap()).unwrap_or(100);
    let size = cap.map_or(size, |c| size.min(c));
    let to = all.len().min(from + size);
    let next = (to < all.len()).then(|| format!("o{to}"));
    (all[from.min(to)..to].to_vec(), next)
}

fn respond(st: &State, path: &str, q: &BTreeMap<String, String>) -> (u16, &'static str, Vec<u8>) {
    let js = |v: Value| (200, "application/json", v.to_string().into_bytes());
    let missing = || {
        (
            404,
            "application/json",
            json!({"error": {"message": "not found"}})
                .to_string()
                .into_bytes(),
        )
    };
    let gmail = "/gmail.googleapis.com/gmail/v1/users/me/messages";
    let drive = "/www.googleapis.com/drive/v3/files";
    if path == gmail {
        let query = q.get("q").cloned().unwrap_or_default();
        let sent_only = query.contains("in:sent") && !st.ignore_sent;
        let word = |p: &str| {
            query
                .split([' ', '(', ')'])
                .find_map(|w| w.strip_prefix(p).map(day_ms))
        };
        let (after, before) = (word("after:"), word("before:"));
        let hits: Vec<Value> = st
            .messages
            .iter()
            .filter(|m| !sent_only || m["labelIds"].as_array().unwrap().contains(&json!("SENT")))
            .filter(|m| after.is_none_or(|a| ms_of(m) >= a))
            .filter(|m| before.is_none_or(|b| ms_of(m) < b))
            .map(|m| json!({"id": m["id"], "threadId": m["threadId"]}))
            .collect();
        let (items, next) = page(&hits, q, "maxResults", st.page_cap);
        let mut v = json!({"messages": items, "resultSizeEstimate": hits.len()});
        if let Some(n) = next {
            v["nextPageToken"] = json!(n);
        }
        return js(v);
    }
    if let Some(id) = path.strip_prefix(&format!("{gmail}/")) {
        return st
            .messages
            .iter()
            .find(|m| m["id"] == id)
            .map_or_else(missing, |m| js(m.clone()));
    }
    if path == drive {
        let query = q.get("q").cloned().unwrap_or_default();
        let parent = query.split('\'').nth(1).unwrap_or_default().to_string();
        let hits: Vec<Value> = st
            .files
            .iter()
            .filter(|f| f["parents"].as_array().unwrap().contains(&json!(parent)))
            .filter(|f| f["trashed"] != true)
            .cloned()
            .collect();
        let (items, next) = page(&hits, q, "pageSize", st.page_cap);
        let mut v = json!({"files": items});
        if let Some(n) = next {
            v["nextPageToken"] = json!(n);
        }
        return js(v);
    }
    if let Some(rest) = path.strip_prefix(&format!("{drive}/")) {
        if let Some(id) = rest.strip_suffix("/export") {
            let mime = q.get("mimeType").cloned().unwrap_or_default();
            return match st.exports.get(&(id.to_string(), mime.clone())) {
                Some(b) => (
                    200,
                    if mime == "text/html" {
                        "text/html"
                    } else {
                        "text/markdown"
                    },
                    b.clone(),
                ),
                None => (
                    400,
                    "application/json",
                    b"{\"error\":{\"message\":\"no export\"}}".to_vec(),
                ),
            };
        }
        return st
            .files
            .iter()
            .chain(&st.roots)
            .find(|f| f["id"] == rest)
            .map_or_else(missing, |f| js(f.clone()));
    }
    missing()
}

fn serve(stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut r = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let mut auth = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("authorization")
        {
            auth = Some(v.trim().to_string());
        }
    }
    let (path, raw_q) = target.split_once('?').unwrap_or((&target, ""));
    let path = decode(path);
    let query: BTreeMap<String, String> = raw_q
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect();
    let full = decode(&target);
    state.lock().unwrap().seen.push(Seen {
        path: path.clone(),
        query: query.clone(),
        auth: auth.clone(),
    });
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let held = {
            let st = state.lock().unwrap();
            st.hold.as_ref().is_some_and(|h| full.contains(h.as_str()))
        };
        if !held || Instant::now() > until {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (status, ct, body) = {
        let st = state.lock().unwrap();
        if auth.as_deref() != Some(&format!("Bearer {TOKEN}")) {
            (
                401,
                "application/json",
                b"{\"error\":\"unauthenticated\"}".to_vec(),
            )
        } else if let Some((_, code)) = st.fail.iter().find(|(k, _)| full.contains(k.as_str())) {
            (
                *code,
                "application/json",
                b"{\"error\":{\"message\":\"injected\"}}".to_vec(),
            )
        } else {
            respond(&st, &path, &query)
        }
    };
    let mut w = stream;
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = w.write_all(head.as_bytes());
    let _ = w.write_all(&body);
    let _ = w.flush();
}

impl FakeGoogle {
    pub fn start() -> FakeGoogle {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let state = Arc::new(Mutex::new(State {
            roots: vec![folder("F", "root")],
            ..State::default()
        }));
        let st = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let st = Arc::clone(&st);
                std::thread::spawn(move || serve(stream, st));
            }
        });
        FakeGoogle { url, state }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.with(|s| s.seen.clone())
    }

    pub fn clear_seen(&self) {
        self.with(|s| s.seen.clear());
    }

    /// The message IDs fetched with `messages.get`, in order.
    pub fn fetched(&self) -> Vec<String> {
        let prefix = "/gmail.googleapis.com/gmail/v1/users/me/messages/";
        self.seen()
            .iter()
            .filter_map(|s| s.path.strip_prefix(prefix).map(str::to_string))
            .collect()
    }

    /// The `q` of every `messages.list` call.
    pub fn list_queries(&self) -> Vec<String> {
        self.seen()
            .iter()
            .filter(|s| s.path == "/gmail.googleapis.com/gmail/v1/users/me/messages")
            .map(|s| s.query.get("q").cloned().unwrap_or_default())
            .collect()
    }

    /// Wait until a request whose path contains `needle` arrived.
    pub fn wait_for(&self, needle: &str) {
        let until = Instant::now() + Duration::from_secs(20);
        while !self.seen_or_held(needle) {
            assert!(Instant::now() < until, "no request for {needle}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn seen_or_held(&self, needle: &str) -> bool {
        self.seen().iter().any(|s| s.path.contains(needle))
    }
}

pub fn enc(s: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
}

/// A `format=full` message with a plain text body.
pub fn message(id: &str, thread: &str, ms: i64, subject: &str, text: &str, sent: bool) -> Value {
    json!({
        "id": id, "threadId": thread, "internalDate": ms.to_string(), "snippet": "snip",
        "labelIds": if sent { json!(["SENT"]) } else { json!(["INBOX"]) },
        "payload": {
            "mimeType": "multipart/alternative",
            "headers": [{"name": "Subject", "value": subject},
                        {"name": "Date", "value": "Thu, 1 Jan 2026 00:00:00 +0000"}],
            "parts": [{"mimeType": "text/plain", "body": {"data": enc(text)}}]
        }
    })
}

/// A native Doc's metadata under `parent`.
pub fn doc(id: &str, parent: &str, version: u64) -> Value {
    json!({"id": id, "name": format!("Doc {id}"), "mimeType": DOC, "parents": [parent],
           "version": version.to_string(), "modifiedTime": "2026-02-01T00:00:00Z",
           "createdTime": "2026-01-01T00:00:00Z", "trashed": false})
}

/// A folder's metadata under `parent`.
pub fn folder(id: &str, parent: &str) -> Value {
    json!({"id": id, "name": id, "mimeType": "application/vnd.google-apps.folder",
           "parents": [parent], "trashed": false})
}

/// A temporary home whose trusty-gworkspace store holds a token for
/// `account`, and a bundle root.
pub struct Home {
    pub tmp: tempfile::TempDir,
    pub home: PathBuf,
    pub root: PathBuf,
}

impl Home {
    pub fn new(account: Option<&str>) -> Home {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let (home, root) = (base.join("home"), base.join("bundle"));
        std::fs::create_dir_all(home.join(".gworkspace-mcp")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        if let Some(a) = account {
            let store = json!({ a: {
                "metadata": {"service_name": a, "created_at": "2026-01-01T00:00:00Z"},
                "token": {"access_token": TOKEN, "expires_at": "2099-01-01T00:00:00Z"}
            }});
            std::fs::write(home.join(".gworkspace-mcp/tokens.json"), store.to_string()).unwrap();
        }
        Home { tmp, home, root }
    }

    /// The command for adapter `bin` against `fake`, with exactly `HOME` in
    /// its environment and `extra` arguments.
    pub fn cmd(&self, bin: &str, fake: &FakeGoogle, extra: &[&str]) -> AdapterCommand {
        let mut c = AdapterCommand::new(bin);
        c.args = ["--api-base", fake.url.as_str()]
            .iter()
            .chain(extra)
            .map(Into::into)
            .collect();
        c.environment = vec![("HOME".into(), self.home.clone().into())];
        c.current_dir = Some(self.home.clone());
        c
    }

    /// Write the adapter's default grant file.
    pub fn grant(&self, store: &str, account: &str) {
        let p = self.home.join(".config/trusty-connectors/config.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            format!("[[inbound_grant]]\nstore = \"{store}\"\naccount = \"{account}\"\nreview = \"DOC-63 S-4.4 test review\"\n"),
        )
        .unwrap();
    }
}

pub fn now() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::parse_from_rfc3339("2026-11-04T12:00:00Z").unwrap()
}

/// Plan and import `spec` through a fresh adapter process.
pub fn run(cmd: &AdapterCommand, root: &Path, spec: &ImportSpec) -> Result<ImportReport, OkfError> {
    let mut src = AdapterSource::spawn(cmd, spec, AdapterLimits::default())?;
    let plan = plan_import(&Bundle::open(root)?.0, spec, &mut src)?;
    import(
        &mut Bundle::open(root)?.0,
        &plan,
        &mut src,
        &NoWorker,
        now(),
    )
}

pub fn item<'a>(r: &'a ImportReport, id: &str) -> &'a ItemReport {
    r.items
        .iter()
        .find(|i| i.item_id == id)
        .unwrap_or_else(|| panic!("{id} not in {:?}", r.items))
}

pub fn s(v: &str) -> toml::Value {
    toml::Value::String(v.into())
}

/// A connector spec of `kind` with `params`.
pub fn spec(store: &str, kind: &str, params: &[(&str, toml::Value)]) -> ImportSpec {
    let mut p = ConnectorParams::new();
    for (k, v) in params {
        p.insert(k, v.clone());
    }
    let locator = Locator::Connector {
        kind: kind.into(),
        params: p,
    };
    ImportSpec::new(SourceSpec::new(
        store,
        None,
        locator,
        "2026-11-04T00:00:00Z",
    ))
}

/// A `gmail` spec for account `work` with `query` and `extra` params.
pub fn mail(query: &str, extra: &[(&str, toml::Value)]) -> ImportSpec {
    let mut p = vec![("account", s("work")), ("query", s(query))];
    p.extend(extra.iter().cloned());
    spec("mail-src", "gmail", &p)
}

/// A `gdocs` spec over folder `F`, tombstoning on.
pub fn docs(recursive: bool, extra: &[(&str, toml::Value)]) -> ImportSpec {
    let mut p = vec![
        ("account", s("work")),
        ("folder_id", s("F")),
        ("recursive", toml::Value::Boolean(recursive)),
    ];
    p.extend(extra.iter().cloned());
    let mut sp = spec("docs-src", "gdocs", &p);
    sp.store.tombstone_deleted = true;
    sp
}

pub fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

pub fn frontmatter(root: &Path, rel: &str) -> serde_yaml::Mapping {
    let text = read(root, rel);
    serde_yaml::from_str(text.split("---\n").nth(1).unwrap()).unwrap()
}

pub fn fm_str(m: &serde_yaml::Mapping, k: &str) -> Option<String> {
    m.get(k).and_then(|v| v.as_str()).map(str::to_string)
}

/// Every file under `root` with its bytes.
pub fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .map(Result::unwrap)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (rel, std::fs::read(e.path()).unwrap())
        })
        .collect()
}

/// The PIDs of processes, zombies included, whose command line holds
/// `needle` (the fake's unique URL).
pub fn processes_with(needle: &str) -> Vec<u32> {
    let out = std::process::Command::new("ps")
        .args(["-eo", "pid=,args="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(needle))
        // A zombie counts: it is a child nobody reaped.
        .filter_map(|l| l.split_whitespace().next()?.parse().ok())
        .collect()
}

/// Wait up to 5 s for no process to hold `needle`.
pub fn gone(needle: &str) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while !processes_with(needle).is_empty() {
        if Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}
