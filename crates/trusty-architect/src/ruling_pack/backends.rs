//! The three external calls of `ruling_pack.py`: memory, search and grep (#12028 P1i).
//!
//! Why: a ruling pack is only as good as what memory, search and grep
//! return, and a backend that is down must be reported in the pack, never
//! hidden; each failure text is the script's.
//! What: [`recall_all`] runs one `trusty-memory serve` MCP stdio session
//! (`initialize`, `notifications/initialized`, then one `tools/call
//! memory_recall` per palace); [`search_index`] runs `trusty-search search`
//! without `TRUSTY_INDEX`; [`grep_root`] runs `rg -nF` per term over the
//! project root when it is a directory and `rg` is on `PATH`. Requests are
//! `json.dumps` text, so a replay compares them byte for byte.
//! Test: the `tests/replay_ruling_pack.rs` recordings,
//! `shipped_ruling_pack_runs_all_three_backends`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use super::pyerr::{Key, PyErr, dict_get, errno_only, subscript, timed_out, universal};
use crate::process::{Finished, RunFailure, run_bounded};
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pystr::splitlines;
use crate::pyval::{dumps, py_str, type_name};
use crate::writers::errno_text;

/// The script's `RECALL_TIMEOUT`: seconds to wait for one reply line.
const RECALL_TIMEOUT: u64 = 25;

fn s(v: &str) -> PyValue {
    PyValue::Str(v.to_owned())
}

fn dict(pairs: Vec<(&str, PyValue)>) -> PyValue {
    PyValue::Dict(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// `msg.get("id") == n` with Python's numeric equality (`1.0 == 1 == True`).
fn id_is(id: Option<&PyValue>, n: i64) -> bool {
    match id {
        Some(PyValue::Int(PyInt::Small(i))) => *i == n,
        Some(PyValue::Float(f)) => *f == n as f64,
        Some(PyValue::Bool(b)) => i64::from(*b) == n,
        _ => false,
    }
}

/// The script's `Mcp`: a newline-delimited JSON-RPC client over a child's stdio.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    n: i64,
}

impl Mcp {
    /// Starts `trusty-memory serve` and runs the handshake.
    fn start() -> Result<Self, String> {
        let mut child = Command::new("trusty-memory")
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| errno_text(&e, "trusty-memory"))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("trusty-memory pipes are missing".into());
        };
        let (tx, lines) = channel();
        // The reader ends at EOF; a dropped sender reads as a closed pipe.
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut buf = Vec::new();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut c = Self {
            child,
            stdin,
            lines,
            n: 0,
        };
        let client = dict(vec![("name", s("ruling_pack")), ("version", s("1"))]);
        let params = dict(vec![
            ("protocolVersion", s("2024-11-05")),
            ("capabilities", PyValue::Dict(Vec::new())),
            ("clientInfo", client),
        ]);
        let started = c.call("initialize", params).and_then(|_| {
            c.send(&dict(vec![
                ("jsonrpc", s("2.0")),
                ("method", s("notifications/initialized")),
            ]))
        });
        match started {
            Ok(()) => Ok(c),
            Err(e) => {
                c.close();
                Err(e)
            }
        }
    }

    fn send(&mut self, m: &PyValue) -> Result<(), String> {
        let line = format!("{}\n", dumps(m, None, true));
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| errno_only(&e))
    }

    /// `_call`: the `result` of the reply whose id is this request's.
    fn call(&mut self, method: &str, params: PyValue) -> Result<PyValue, String> {
        self.n += 1;
        let id = PyValue::Int(PyInt::Small(self.n));
        self.send(&dict(vec![
            ("jsonrpc", s("2.0")),
            ("id", id),
            ("method", s(method)),
            ("params", params),
        ]))?;
        loop {
            let line = match self.lines.recv_timeout(Duration::from_secs(RECALL_TIMEOUT)) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("{method} timed out after {RECALL_TIMEOUT}s"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("trusty-memory closed the pipe".into());
                }
            };
            let msg = loads(&line).map_err(|e| e.py_str())?;
            if !id_is(dict_get(&msg, "id").map_err(|e| e.msg)?, self.n) {
                continue;
            }
            if let Ok(Some(err)) = dict_get(&msg, "error") {
                return Err(py_str(err).chars().take(200).collect());
            }
            return subscript(&msg, Key::Str("result")).map_err(|e| e.msg);
        }
    }

    /// `recall`: the `results` of one `memory_recall` (top 3, min score 0.4).
    fn recall(&mut self, palace: &str, query: &str) -> Result<PyValue, String> {
        let args = dict(vec![
            ("palace", s(palace)),
            ("query", s(query)),
            ("top_k", PyValue::Int(PyInt::Small(3))),
            ("min_score", PyValue::Float(0.4)),
        ]);
        let params = dict(vec![("name", s("memory_recall")), ("arguments", args)]);
        let r = self.call("tools/call", params)?;
        let text = (|| -> Result<PyValue, PyErr> {
            let first = subscript(&subscript(&r, Key::Str("content"))?, Key::Idx(0))?;
            subscript(&first, Key::Str("text"))
        })()
        .map_err(|e| e.msg)?;
        let PyValue::Str(text) = text else {
            return Err(format!(
                "the JSON object must be str, bytes or bytearray, not {}",
                type_name(&text)
            ));
        };
        let doc = loads(&text).map_err(|e| e.py_str())?;
        Ok(dict_get(&doc, "results")
            .map_err(|e| e.msg)?
            .cloned()
            .unwrap_or(PyValue::List(Vec::new())))
    }

    fn close(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One palace's recall: its hits (the `results` value) or its error text.
pub(crate) struct Recall {
    pub(crate) hits: PyValue,
    pub(crate) error: Option<String>,
}

/// `recall_all(palaces, query)`: one client for every palace; a startup
/// failure marks each palace `memory unavailable: <e>`, a failed recall
/// marks its palace `recall failed: <e>`.
pub(crate) fn recall_all(palaces: &[String], query: &str) -> Vec<Recall> {
    let none = || PyValue::List(Vec::new());
    let mut c = match Mcp::start() {
        Ok(c) => c,
        Err(e) => {
            return palaces
                .iter()
                .map(|_| Recall {
                    hits: none(),
                    error: Some(format!("memory unavailable: {e}")),
                })
                .collect();
        }
    };
    let out = palaces
        .iter()
        .map(|p| match c.recall(p, query) {
            Ok(hits) => Recall { hits, error: None },
            Err(e) => Recall {
                hits: none(),
                error: Some(format!("recall failed: {e}")),
            },
        })
        .collect();
    c.close();
    out
}

/// `subprocess.run(argv, timeout=secs, text=True)`: exit code (a signal as
/// its negative), stdout and stderr with newlines translated, or the
/// exception's text.
fn run(argv: &[String], secs: u64, unset_index: bool) -> Result<(i32, String, String), String> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if unset_index {
        cmd.env_remove("TRUSTY_INDEX");
    }
    match run_bounded(cmd, Duration::from_secs(secs)) {
        Ok(Finished {
            status,
            stdout,
            stderr,
        }) => {
            use std::os::unix::process::ExitStatusExt;
            let rc = status.code().or(status.signal().map(|n| -n)).unwrap_or(-1);
            let text = |b: Vec<u8>| universal(&String::from_utf8_lossy(&b));
            Ok((rc, text(stdout), text(stderr)))
        }
        Err(RunFailure::Spawn(e)) => Err(errno_text(&e, &argv[0])),
        Err(RunFailure::Timeout) => Err(timed_out(argv, secs)),
        Err(RunFailure::Wait(e)) => Err(errno_only(&e)),
    }
}

/// What a search returned: its hits and `meta.last_indexed`.
pub(crate) struct Search {
    pub(crate) hits: PyValue,
    pub(crate) last_indexed: Option<PyValue>,
}

/// `search_index(index, query)`: `Ok(Err(text))` is a reported failure; the
/// outer `Err` is an exception the script does not catch.
pub(crate) fn search_index(index: &str, query: &str) -> Result<Result<Search, String>, PyErr> {
    let argv: Vec<String> = [
        "trusty-search",
        "search",
        "-i",
        index,
        "--json",
        "-k",
        "5",
        query,
    ]
    .map(str::to_owned)
    .to_vec();
    let (rc, out, err) = match run(&argv, 60, true) {
        Ok(r) => r,
        Err(e) => return Ok(Err(format!("search unavailable: {e}"))),
    };
    let Some(i) = out.find('{').filter(|_| rc == 0) else {
        let why = if err.is_empty() { &out } else { &err };
        let why: String = crate::pystr::strip(why).chars().take(160).collect();
        return Ok(Err(format!("search failed rc={rc}: {why}")));
    };
    let d = match loads(&out[i..]) {
        Ok(d) => d,
        Err(e) => return Ok(Err(format!("search output not JSON: {}", e.py_str()))),
    };
    let hits = dict_get(&d, "results")?
        .cloned()
        .unwrap_or(PyValue::List(Vec::new()));
    let meta = dict_get(&d, "meta")?.filter(|m| crate::pyval::truthy(m));
    let last_indexed = match meta {
        Some(m) => dict_get(m, "last_indexed")?.cloned(),
        None => None,
    };
    Ok(Ok(Search { hits, last_indexed }))
}

/// `shutil.which(name)` is not `None`: an executable, non-directory file in a
/// `PATH` entry (an empty entry is the working directory).
fn on_path(name: &str) -> bool {
    // An unset PATH is `os.confstr("CS_PATH")`.
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    path.split(':').any(|dir| {
        let p = if dir.is_empty() {
            std::path::PathBuf::from(name)
        } else {
            std::path::Path::new(dir).join(name)
        };
        let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
            return false;
        };
        // SAFETY: `c` is a valid NUL-terminated path for the call's duration.
        let exec = unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0;
        exec && p.exists() && !p.is_dir()
    })
}

/// `grep_root(root, terms)`: per term, up to three `path:line:text` lines
/// with the root prefix cut and each line cut at 140 characters; a repeated
/// term keeps its first place and its last lines. The note is the skip or
/// failure text.
pub(crate) fn grep_root(
    root: Option<&PyValue>,
    terms: &[String],
) -> (Vec<(String, Vec<String>)>, Option<String>) {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let root = match root {
        Some(PyValue::Str(r)) if !r.is_empty() && std::path::Path::new(r).is_dir() => r,
        _ => return (out, Some(skipped())),
    };
    if !on_path("rg") {
        return (out, Some(skipped()));
    }
    for t in terms {
        let argv: Vec<String> = [
            "rg",
            "-nF",
            "--no-heading",
            "-m",
            "2",
            "-g",
            "!*.lock",
            "-g",
            "!node_modules",
            "-g",
            "!CHANGELOG*",
            t,
            root,
        ]
        .map(str::to_owned)
        .to_vec();
        let o = match run(&argv, 30, false) {
            Ok((_, o, _)) => o,
            Err(e) => return (out, Some(format!("grep failed: {e}"))),
        };
        let prefix = format!("{root}/");
        let lines: Vec<String> = splitlines(&o)
            .into_iter()
            .take(3)
            .map(|l| l.replacen(&prefix, "", 1).chars().take(140).collect())
            .collect();
        match out.iter_mut().find(|(k, _)| k == t) {
            Some(slot) => slot.1 = lines,
            None => out.push((t.clone(), lines)),
        }
    }
    (out, None)
}

fn skipped() -> String {
    "grep skipped: root missing or rg absent".into()
}
