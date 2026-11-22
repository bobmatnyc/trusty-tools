//! The `dep_degraded` alert: `fleet_poll_deps.py`, ported like-for-like (PR-B2).
//!
//! Why: the owner ruled that the poller alerts, urgently, when tm health is
//! degraded because trusty-memory or trusty-search is down, and that the
//! Rust scheduler gets the same kind in the same PR so parity stays honest.
//! What: [`read_tm_health`] asks tm's `mpm.health` over its Unix socket
//! (newline-framed JSON-RPC). [`classify`] maps the answer to up / pending /
//! degraded per service. [`step`] is the rule: two consecutive degraded reads
//! raise ONE `dep_degraded` alert (fingerprint = service, no session) per
//! outage; pending neither counts nor resets; up clears the service.
//! [`dep_health_step`] runs it once per cycle with its book in
//! `state["dep_health"]`, the Python module's key and shape. A bad answer is
//! a `dep_health_error` alert keyed by error class, and so is an unreachable
//! tm (class `unreachable`: connect refused, socket missing). Neither reads
//! healthy: the book is kept as it was. A corrupt book (not an object, an
//! entry missing `streak`/`alerted`) is coerced to defaults, never an error.
//! Test: `tests/dep_health.rs`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::classify::{Alert, alert};

/// The alert kind.
pub const KIND: &str = "dep_degraded";
/// The error-arm kind.
pub const ERROR_KIND: &str = "dep_health_error";
/// The services tm's health depends on, in tm's `degraded[]` order.
pub const SERVICES: [&str; 2] = ["trusty-memory", "trusty-search"];
/// Degraded reads in a row before one alert.
pub const CONSECUTIVE: u64 = 2;
/// The `PollState::other` key holding the book.
pub const STATE_KEY: &str = "dep_health";
const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FRAME: usize = 1 << 20;

/// One `mpm.health` call's result.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// The RPC `result` object.
    Ok(Value),
    /// No socket, or nothing listening: tm itself is down.
    Unreachable(String),
    /// An answer that is not a health result: `(class, detail)`.
    Error(&'static str, String),
}

/// One service's state in a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepState {
    /// Not listed.
    Up,
    /// Listed in `pending[]`: no current answer, not an outage.
    Pending,
    /// Listed in `degraded[]` as down or timed out.
    Degraded,
}

/// Reads `mpm.health` from the socket at `path`.
///
/// Why: the ruling reads tm health over its UDS method, not HTTP `/health`.
/// What: connect, write one frame, half-close, read one line. A missing
/// socket or a refused connect is [`Reading::Unreachable`]; a timeout, a
/// non-JSON or over-long reply, or an RPC error is [`Reading::Error`].
/// Test: `wire_read_parses_a_real_socket_and_garbage`,
/// `unreachable_socket_raises_dep_health_error`.
pub fn read_tm_health(path: &Path) -> Reading {
    if !path.exists() {
        return Reading::Unreachable(format!("{}: no socket", path.display()));
    }
    let mut stream = match UnixStream::connect(path) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Reading::Unreachable(format!("{}: {e}", path.display()));
        }
        Err(e) => return Reading::Error("io", e.to_string()),
    };
    let io = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => Reading::Error(
            "timeout",
            format!("no answer within {}s", TIMEOUT.as_secs()),
        ),
        _ => Reading::Error("io", e.to_string()),
    };
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "mpm.health", "params": {}});
    let sent = (|| {
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        stream.write_all(format!("{req}\n").as_bytes())?;
        stream.shutdown(std::net::Shutdown::Write)?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 65536];
        while !buf.contains(&b'\n') && buf.len() <= MAX_FRAME {
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Ok(buf)
    })();
    let buf = match sent {
        Ok(b) => b,
        Err(e) => return io(e),
    };
    let line = buf.split(|b| *b == b'\n').next().unwrap_or_default();
    let reply: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => {
            let why = format!("reply is not JSON ({} bytes)", buf.len());
            return Reading::Error("unparseable", why);
        }
    };
    match reply.get("result") {
        Some(r) => Reading::Ok(r.clone()),
        None => {
            let err = reply.get("error").map(Value::to_string);
            let why = err.unwrap_or_else(|| "reply has no result".to_owned());
            Reading::Error("rpc_error", why.chars().take(200).collect())
        }
    }
}

fn entries<'a>(result: &'a Map<String, Value>, key: &str) -> Result<Vec<&'a str>, String> {
    match result.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(a)) => Ok(a.iter().filter_map(Value::as_str).collect()),
        Some(_) => Err(format!("{key} is not a list")),
    }
}

/// Each service's [`DepState`] in an `mpm.health` result.
///
/// What: degraded when `degraded[]` has a `"<service>: ..."` entry that is
/// not a pending one; pending when `pending[]` (or a pre-PR-B2 pending entry
/// in `degraded[]`) names it; else up.
///
/// # Errors
/// The result is not an object, or a list field is not a list.
///
/// Test: `pending_read_does_not_count`, `garbage_reply_is_the_error_arm`.
pub fn classify(result: &Value) -> Result<Vec<(&'static str, DepState)>, String> {
    let obj = result.as_object().ok_or("result is not an object")?;
    let degraded = entries(obj, "degraded")?;
    let pending = entries(obj, "pending")?;
    Ok(SERVICES
        .iter()
        .map(|svc| {
            let prefix = format!("{svc}:");
            let mine: Vec<&str> = degraded
                .iter()
                .filter_map(|e| e.strip_prefix(&prefix))
                .collect();
            let state = if mine.iter().any(|r| !r.trim().starts_with("pending")) {
                DepState::Degraded
            } else if !mine.is_empty() || pending.iter().any(|e| e.starts_with(&prefix)) {
                DepState::Pending
            } else {
                DepState::Up
            };
            (*svc, state)
        })
        .collect())
}

/// The rule for one classified read; updates `book` and returns new alerts.
///
/// `book` is `{service: {"streak": n, "alerted": bool}}`; a missing or
/// mistyped field reads as its default.
/// Test: `corrupt_state_book_recovers_and_still_alerts`, `one_degraded_read_raises_nothing`, `two_consecutive_raise_exactly_one`,
/// `third_read_does_not_duplicate`, `recovery_then_two_degraded_alerts_again`.
pub fn step(
    book: &mut Map<String, Value>,
    reading: &[(&'static str, DepState)],
    result: &Value,
    now: f64,
) -> Vec<Alert> {
    let mut found = Vec::new();
    for (svc, state) in reading {
        match state {
            DepState::Up => {
                book.remove(*svc);
            }
            DepState::Pending => {}
            DepState::Degraded => {
                let entry = book.entry(*svc).or_insert_with(|| json!({}));
                let streak = entry.get("streak").and_then(Value::as_u64).unwrap_or(0) + 1;
                let alerted = entry.get("alerted").and_then(Value::as_bool) == Some(true);
                let fire = streak >= CONSECUTIVE && !alerted;
                *entry = json!({"streak": streak, "alerted": alerted || fire});
                if fire {
                    let prefix = format!("{svc}:");
                    let why: Vec<&str> = result
                        .get("degraded")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter(|e| e.starts_with(&prefix))
                        .collect();
                    let why = if why.is_empty() {
                        (*svc).to_owned()
                    } else {
                        why.join("; ")
                    };
                    let detail = format!("tm health degraded: {why}");
                    found.push(alert(KIND, svc, &detail, "", "", now));
                }
            }
        }
    }
    found
}

/// The cycle step: `(alerts, log lines)`, with the book in `other["dep_health"]`.
///
/// Why: see the module doc. `socket` is `Config::tm_health_socket`; `None`
/// (`TM_HEALTH=off`) turns the step off. `read` is the socket seam. An
/// unreachable tm is a `dep_health_error` with fingerprint `unreachable`.
/// Test: `tests/dep_health.rs`.
pub fn dep_health_step(
    socket: Option<&Path>,
    read: &dyn Fn(&Path) -> Reading,
    other: &mut Map<String, Value>,
    now: f64,
) -> (Vec<Alert>, Vec<String>) {
    let Some(path) = socket else {
        return (Vec::new(), Vec::new());
    };
    let error = |cls: &str, detail: &str| {
        let d = format!("cannot read tm health ({cls}: {detail}); streaks held");
        vec![alert(ERROR_KIND, cls, &d, "", "", now)]
    };
    let result = match read(path) {
        Reading::Unreachable(why) => return (error("unreachable", &why), Vec::new()),
        Reading::Error(cls, detail) => return (error(cls, &detail), Vec::new()),
        Reading::Ok(r) => r,
    };
    let reading = match classify(&result) {
        Ok(r) => r,
        Err(e) => return (error("ValueError", &e), Vec::new()),
    };
    let mut book = match other.remove(STATE_KEY) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let found = step(&mut book, &reading, &result, now);
    other.insert(STATE_KEY.to_owned(), Value::Object(book));
    (found, Vec::new())
}

/// tm's socket for `home`, as `trusty_common::daemon_socket_path` derives it.
///
/// What: `TRUSTY_DATA_DIR_OVERRIDE` (absolute, not `/`) else the platform data
/// dir under `home`; then `trusty-mpm/` unless it already ends `.trusty-mpm`.
/// Test: `socket_path_follows_tm_derivation`.
pub fn tm_socket_path(home: &Path, get: &dyn Fn(&str) -> Option<String>) -> std::path::PathBuf {
    let over = get("TRUSTY_DATA_DIR_OVERRIDE").map(|v| v.trim().to_owned());
    let base = match over {
        Some(v) if v.starts_with('/') && v != "/" => std::path::PathBuf::from(v),
        _ if cfg!(target_os = "macos") => home.join("Library/Application Support"),
        _ => get("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map_or_else(|| home.join(".local/share"), std::path::PathBuf::from),
    };
    let dir = if base.to_string_lossy().ends_with(".trusty-mpm") {
        base
    } else {
        base.join("trusty-mpm")
    };
    dir.join("trusty-mpm.sock")
}
