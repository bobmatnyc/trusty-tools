//! The acquisition adapter spawn API and its `acquire` client (SPEC-OKF-01
//! §2.15, §2.7.1 B10, §3 (m) A, Appendix B "Acquire role").
//!
//! Why: authenticated acquisition runs in a separate adapter program that
//! holds its own grant (ADR-0026). trusty-okf starts it, speaks the worker
//! transport to it, and must add nothing to its environment: the adapter
//! finds its credential store and network settings through the list its
//! caller supplies (B10, r6). trusty-okf itself receives no credential (B6).
//! What: [`AdapterCommand`] names the program, its arguments, the exact
//! environment list and an optional working directory. Its
//! [`AdapterCommand::spawn`] clears the inherited environment and sets only
//! that list, so the child sees exactly it. [`AdapterSource`] spawns the
//! command, runs the handshake in the `acquire` role under a deadline, and
//! implements [`ImportSource`]: each `list` and `fetch` is one `acquire`
//! request carrying the store kind, the store selection and the run's
//! `operator_initiated` value. Frames are a 4-byte big-endian length and a
//! JSON header, capped at [`MAX_FRAME_BYTES`]. A request past its deadline,
//! a bad frame or a wrong `request_id` kills the adapter;
//! [`AdapterSource::cancel`] and `Drop` kill and reap it, so no adapter
//! outlives its source.
//! Slice S9 of #10746.
//! The adapter leads its own process group, and every stop kills the group,
//! so no process it started outlives it. A list reply whose `cursor` is not
//! a string or null is a protocol error.
//! Test: `c97_cli_imports_through_an_adapter_process_and_reaps_on_cancel`,
//! `c139_adapter_gets_exactly_the_caller_environment`,
//! `c98_acquire_requests_carry_operator_initiated`,
//! `numeric_cursor_is_a_protocol_error_and_tombstones_nothing`,
//! `cancel_kills_the_adapter_process_group`
//! (`crates/trusty-okf/tests/adapter_criteria.rs`).
//!
//! # Spec References
//! - SPEC-OKF-01 §2.7.1 B10, §2.15, Appendix B "Acquire role".

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::plan::ImportSpec;
use super::record::{SourceRecord, check_selection};
use super::source::{ImportSource, ListEntry, Listing};
use crate::OkfError;
use crate::build::registry::Locator;
use crate::extract::ExtractError;

/// The one protocol version this host speaks (Appendix B).
pub const ADAPTER_PROTOCOL: u64 = 1;

/// The `host_version` the host's `hello` carries.
pub const HOST_VERSION: &str = "trusty-okf-import/1";

/// The largest frame the host reads (the Appendix B output ceiling).
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// One adapter program and the exact environment it runs with (B10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdapterCommand {
    /// The adapter binary.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// The child's whole environment, in order. trusty-okf adds nothing.
    pub environment: Vec<(OsString, OsString)>,
    /// The working directory, when the caller names one.
    pub current_dir: Option<PathBuf>,
}

impl AdapterCommand {
    /// A command for `program` with no arguments and an empty environment.
    pub fn new(program: impl Into<PathBuf>) -> AdapterCommand {
        AdapterCommand {
            program: program.into(),
            ..AdapterCommand::default()
        }
    }

    /// Start the program with exactly [`Self::environment`]: the inherited
    /// environment is cleared first (B10). stdin and stdout are pipes;
    /// stderr is inherited, diagnostic only. On Unix the child leads a new
    /// process group.
    pub fn spawn(&self) -> std::io::Result<Child> {
        let mut c = Command::new(&self.program);
        // #10746: B10 — clear, then the caller's list and nothing else.
        c.args(&self.args)
            .env_clear()
            .envs(self.environment.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(dir) = &self.current_dir {
            c.current_dir(dir);
        }
        // #10746 S9 fix round: its own process group, so a stop kills every
        // process the adapter started, not the adapter alone.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        c.spawn()
    }
}

/// Deadlines for one adapter (Appendix B "Handshake", per request).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterLimits {
    /// From spawn to a valid `hello` reply.
    pub handshake: Duration,
    /// From an `acquire` request to its reply.
    pub request: Duration,
}

impl Default for AdapterLimits {
    fn default() -> Self {
        AdapterLimits {
            handshake: Duration::from_secs(10),
            request: Duration::from_secs(120),
        }
    }
}

/// One listed entry on the wire.
#[derive(Deserialize)]
struct WireEntry {
    item_id: String,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    skip: Option<String>,
    #[serde(default)]
    deleted: bool,
}

/// A running adapter, as an [`ImportSource`].
pub struct AdapterSource {
    program: PathBuf,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    frames: Receiver<Result<Vec<u8>, String>>,
    kind: String,
    store: String,
    selection: Value,
    operator_initiated: bool,
    limits: AdapterLimits,
    next_id: u64,
    dead: Option<String>,
}

impl std::fmt::Debug for AdapterSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterSource")
            .field("program", &self.program)
            .field("kind", &self.kind)
            .field("dead", &self.dead)
            .finish_non_exhaustive()
    }
}

/// Read frames from `out` until EOF, a read error or an oversized frame.
fn pump(mut out: impl Read, tx: mpsc::Sender<Result<Vec<u8>, String>>) {
    loop {
        let mut len = [0u8; 4];
        if let Err(e) = out.read_exact(&mut len) {
            let _ = tx.send(Err(format!("crashed: stdout closed ({e})")));
            return;
        }
        let n = u32::from_be_bytes(len) as usize;
        if n > MAX_FRAME_BYTES {
            let _ = tx.send(Err(format!("protocol_error: frame of {n} bytes")));
            return;
        }
        let mut buf = vec![0u8; n];
        if let Err(e) = out.read_exact(&mut buf) {
            let _ = tx.send(Err(format!("crashed: torn frame ({e})")));
            return;
        }
        if tx.send(Ok(buf)).is_err() {
            return;
        }
    }
}

impl AdapterSource {
    /// Spawn `cmd` for the connector store of `spec` and run the handshake.
    ///
    /// Errors, each before any write: `PolicyRefused` when the spec allows
    /// no child process or names no connector store; `SelectionRequired`
    /// (no adapter is started); an I/O error naming the program when the
    /// spawn or the handshake fails (`handshake_timeout`, `protocol_error`,
    /// `adapter_incompatible`), after the adapter is killed and reaped.
    pub fn spawn(
        cmd: &AdapterCommand,
        spec: &ImportSpec,
        limits: AdapterLimits,
    ) -> Result<AdapterSource, OkfError> {
        let refuse = |reason: String| OkfError::PolicyRefused {
            path: cmd.program.clone(),
            reason,
        };
        let Locator::Connector { kind, params } = &spec.store.locator else {
            return Err(refuse("policy_refused: not a connector store".into()));
        };
        if !spec.child_processes {
            return Err(refuse(format!(
                "policy_refused: a {kind} store runs through an adapter process, and this run \
                 allows no child process"
            )));
        }
        check_selection(spec)?;
        let mut child = cmd.spawn().map_err(|e| io(&cmd.program, e.to_string()))?;
        let (tx, frames) = mpsc::channel();
        let stdout = child.stdout.take();
        let stdin = child.stdin.take();
        if let Some(out) = stdout {
            std::thread::spawn(move || pump(out, tx));
        }
        let mut me = AdapterSource {
            program: cmd.program.clone(),
            child: Some(child),
            stdin,
            frames,
            kind: kind.clone(),
            store: spec.store.id.clone(),
            selection: serde_json::to_value(params).unwrap_or(Value::Null),
            operator_initiated: spec.operator_initiated,
            limits,
            next_id: 0,
            dead: None,
        };
        me.handshake()
            .map_err(|why| io(&me.program, format!("adapter {why}")))?;
        Ok(me)
    }

    /// The adapter's process ID while it runs.
    pub fn id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Kill and reap the adapter now; later calls fail with `cancelled`.
    /// Returns the reaped exit status, `None` when it was already reaped.
    pub fn cancel(&mut self) -> Option<ExitStatus> {
        self.stop("cancelled", Duration::ZERO)
    }

    fn handshake(&mut self) -> Result<(), String> {
        self.send(&json!({
            "type": "hello",
            "role": "acquire",
            "protocol_min": ADAPTER_PROTOCOL,
            "protocol_max": ADAPTER_PROTOCOL,
            "host_version": HOST_VERSION,
        }))?;
        let reply = self.recv(self.limits.handshake, "handshake_timeout")?;
        let ok = reply["type"] == "hello"
            && reply["protocol"].as_u64() == Some(ADAPTER_PROTOCOL)
            && reply["kind"].as_str().is_none_or(|k| k == self.kind);
        if ok {
            Ok(())
        } else {
            self.stop("adapter_incompatible", Duration::ZERO);
            Err(format!("adapter_incompatible: {reply}"))
        }
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        let body = msg.to_string();
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(body.as_bytes());
        let wrote = match self.stdin.as_mut() {
            Some(w) => w.write_all(&frame).and_then(|()| w.flush()),
            None => Err(std::io::Error::other("stdin closed")),
        };
        wrote.map_err(|e| {
            self.stop("crashed", Duration::ZERO);
            format!("crashed: {e}")
        })
    }

    fn recv(&mut self, within: Duration, late: &str) -> Result<Value, String> {
        let frame = match self.frames.recv_timeout(within) {
            Ok(Ok(frame)) => frame,
            Ok(Err(why)) => {
                self.stop("crashed", Duration::ZERO);
                return Err(why);
            }
            Err(_) => {
                self.stop(late, Duration::ZERO);
                return Err(late.to_string());
            }
        };
        serde_json::from_slice(&frame).map_err(|e| {
            self.stop("protocol_error", Duration::ZERO);
            format!("protocol_error: {e}")
        })
    }

    /// One `acquire` request and its reply of type `want`.
    fn acquire(&mut self, op: &str, extra: Value) -> Result<Value, (String, String)> {
        if let Some(why) = &self.dead {
            return Err((why.clone(), "the adapter is not running".into()));
        }
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({
            "type": "acquire",
            "request_id": id,
            "op": op,
            "kind": self.kind,
            "store": self.store,
            "selection": self.selection,
            "operator_initiated": self.operator_initiated,
        });
        if let (Some(m), Value::Object(more)) = (msg.as_object_mut(), extra) {
            m.extend(more);
        }
        let split = |s: String| match s.split_once(": ") {
            Some((code, rest)) => (code.to_string(), rest.to_string()),
            None => (s, String::new()),
        };
        self.send(&msg).map_err(split)?;
        let reply = self.recv(self.limits.request, "timeout").map_err(split)?;
        if reply["request_id"].as_u64() != Some(id) {
            self.stop("protocol_error", Duration::ZERO);
            return Err(("protocol_error".into(), format!("reply for {reply}")));
        }
        if reply["type"] == "error" {
            let code = reply["code"].as_str().unwrap_or("fetch_error").to_string();
            let message: String = reply["message"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(512)
                .collect();
            return Err((code, message));
        }
        Ok(reply)
    }

    /// Close stdin, wait up to `grace` for a clean exit, then kill; reap.
    fn stop(&mut self, why: &str, grace: Duration) -> Option<ExitStatus> {
        if self.dead.is_none() {
            self.dead = Some(why.to_string());
        }
        self.stdin = None;
        let mut child = self.child.take()?;
        let until = Instant::now() + grace;
        while Instant::now() < until {
            if let Ok(Some(status)) = child.try_wait() {
                // A clean exit can leave group members behind.
                kill_group(&child);
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        kill_group(&child);
        let _ = child.kill();
        child.wait().ok()
    }
}

/// #10746 S9 fix round: SIGKILL the adapter's whole process group, so a
/// grandchild holding stdout open cannot outlive it or keep the frame
/// reader blocked. The group outlives a reaped leader while a member lives,
/// so its ID cannot be reused while there is anything left to kill.
fn kill_group(child: &Child) {
    #[cfg(unix)]
    if let Some(pid) = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    #[cfg(not(unix))]
    let _ = child;
}

fn io(program: &std::path::Path, why: String) -> OkfError {
    OkfError::Io {
        path: program.to_path_buf(),
        source: std::io::Error::other(why),
    }
}

impl Drop for AdapterSource {
    fn drop(&mut self) {
        if self.child.is_some() && self.dead.is_none() {
            let _ = self.send(&json!({"type": "shutdown"}));
        }
        self.stop("closed", Duration::from_secs(2));
    }
}

impl ImportSource for AdapterSource {
    fn list(&mut self, cursor: Option<&str>) -> Result<Listing, OkfError> {
        let reply = self
            .acquire("list", json!({ "cursor": cursor }))
            .map_err(|(code, msg)| io(&self.program, format!("adapter list {code}: {msg}")))?;
        // #10746 S9 fix round: a cursor that is neither a string nor null is
        // a protocol error, never "no more pages".
        let cursor = match &reply["cursor"] {
            Value::Null => None,
            Value::String(c) => Some(c.clone()),
            other => {
                self.stop("protocol_error", Duration::ZERO);
                return Err(io(
                    &self.program,
                    format!("adapter list protocol_error: cursor {other}"),
                ));
            }
        };
        let entries: Vec<WireEntry> = serde_json::from_value(reply["entries"].clone())
            .map_err(|e| io(&self.program, format!("adapter list protocol_error: {e}")))?;
        Ok(Listing {
            entries: entries
                .into_iter()
                .map(|w| ListEntry {
                    item_id: w.item_id,
                    timestamp: w.timestamp,
                    skip: w.skip,
                    group: w.group,
                    revision: w.revision,
                    deleted: w.deleted,
                })
                .collect(),
            cursor,
            complete: reply["complete"].as_bool().unwrap_or(false),
        })
    }

    /// The item's record, re-serialised in the host's field order so equal
    /// records give equal bytes.
    fn fetch(&mut self, item_id: &str) -> Result<Vec<u8>, ExtractError> {
        let fail = |code: String, message: String| ExtractError { code, message };
        let reply = self
            .acquire("fetch", json!({ "item_id": item_id }))
            .map_err(|(c, m)| fail(c, m))?;
        let rec: SourceRecord = serde_json::from_value(reply["record"].clone())
            .map_err(|e| fail("protocol_error".into(), e.to_string()))?;
        if rec.item_id != item_id {
            return Err(fail(
                "protocol_error".into(),
                format!("asked for {item_id}, got {}", rec.item_id),
            ));
        }
        serde_json::to_vec(&rec).map_err(|e| fail("protocol_error".into(), e.to_string()))
    }
}
