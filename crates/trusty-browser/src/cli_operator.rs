//! Operator verbs on `operator.sock` and the legacy `revoke` (`cli.py`, `service.py`).
//!
//! Why: `grants`, `revoke-grant(s)`, `pairings` and `unpair` are thin clients
//! of the running broker, and scripts parse their output, so the frames,
//! validation messages, printed reply and exit codes must equal Python's.
//! What: [`operator_frame`] validates and builds the request
//! (`json.dumps` default separators), [`operator_call`] sends one frame and
//! reads one reply (15 s timeout); the CLI prints the reply compactly and
//! exits 1 unless `"ok"` is `true`. [`revoke`] asks the broker, or, with no
//! broker listening, retires the route offline under `service.lock`.
//! Test: `operator_verbs_validate_like_python`, `operator_call_prints_the_reply_and_exit_code`,
//! `revoke_offline_without_a_route_is_a_no_op` in `tests/cli.rs`.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::frame::HEADER;
use crate::pyjson::Json;
use crate::vault::KeyStore;

const UNREACHABLE: &str = "The Trusty Browser service is not reachable on operator.sock";

fn lower_hex(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `GRANT_ID`: `g-` and 12 lower-case hex characters.
pub fn grant_id_ok(s: &str) -> bool {
    s.strip_prefix("g-")
        .is_some_and(|h| h.len() == 12 && lower_hex(h))
}

/// `PROFILE_ID`: a lower-case UUID v4.
pub fn profile_id_ok(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let lens = [8, 4, 4, 4, 12];
    parts.len() == 5
        && parts
            .iter()
            .zip(lens)
            .all(|(p, n)| p.len() == n && lower_hex(p))
        && parts[2].starts_with('4')
        && parts[3].starts_with(['8', '9', 'a', 'b'])
}

/// An operator verb's request; `Err` holds Python's validation message.
pub fn operator_frame(verb: &str, arg: Option<&str>) -> Result<Json, String> {
    let op = |o: &str| Json::object([("op", Json::from(o))]);
    match (verb, arg) {
        ("grants" | "revoke-grants" | "pairings", _) => Ok(op(&verb.replace('-', "_"))),
        ("revoke-grant", Some(g)) if grant_id_ok(g) => {
            let mut f = op("revoke_grant");
            f.insert("grant", Json::from(g));
            Ok(f)
        }
        ("revoke-grant", _) => {
            Err("A grant id is g- followed by 12 lower-case hex characters".to_owned())
        }
        ("unpair", Some(p)) if profile_id_ok(p) => {
            let mut f = op("unpair");
            f.insert("profile", Json::from(p));
            Ok(f)
        }
        ("unpair", _) => Err("A profile id is a lower-case UUID v4".to_owned()),
        _ => Err(format!("not an operator verb: {verb}")),
    }
}

fn exchange(path: &Path, frame: &Json) -> io::Result<Json> {
    exchange_on(UnixStream::connect(path)?, frame)
}

fn exchange_on(mut wire: UnixStream, frame: &Json) -> io::Result<Json> {
    let body = frame
        .dumps_spaced()
        .map_err(|e| io::Error::other(e.to_string()))?;
    wire.set_read_timeout(Some(Duration::from_secs(15)))?;
    wire.set_write_timeout(Some(Duration::from_secs(15)))?;
    let len = u32::try_from(body.len()).map_err(io::Error::other)?;
    wire.write_all(&len.to_ne_bytes())?;
    wire.write_all(body.as_bytes())?;
    let mut head = [0u8; HEADER];
    wire.read_exact(&mut head)?;
    let size = u32::from_ne_bytes(head) as usize;
    if size > crate::frame::MAX_FRAME {
        return Err(io::Error::other("reply too large"));
    }
    let mut reply = vec![0u8; size];
    wire.read_exact(&mut reply)?;
    Json::parse(&reply).map_err(io::Error::other)
}

/// `operator_call`: one frame on `state/operator.sock`, one reply object.
pub fn operator_call(state: &Path, frame: &Json) -> Result<Json, String> {
    let reply =
        exchange(&state.join("operator.sock"), frame).map_err(|_| UNREACHABLE.to_owned())?;
    match reply {
        Json::Object(_) => Ok(reply),
        _ => Err("Invalid reply from the Trusty Browser service".to_owned()),
    }
}

/// True when the reply's `ok` is exactly `true`.
pub fn reply_ok(reply: &Json) -> bool {
    matches!(reply.get("ok"), Some(Json::Bool(true)))
}

/// `service.operator_revoke`: retire a legacy workflow route.
///
/// Why: a running broker owns the registry; an offline one must not start
/// while the route is removed, so the offline path takes `service.lock` too.
/// What: with `operator.sock` absent or refusing, open the registry, take
/// `service.lock` (`LOCK_EX|LOCK_NB`), remove the route and finish pending
/// removals; otherwise send `{"op": "revoke", ...}` and require `ok`.
pub fn revoke(state: &Path, workflow: &str, store: Box<dyn KeyStore>) -> Result<(), String> {
    let sock = state.join("operator.sock");
    let frame = Json::object([
        ("op", Json::from("revoke")),
        ("workflow", Json::from(workflow)),
    ]);
    let wire = match sock.exists().then(|| UnixStream::connect(&sock)) {
        None => None,
        Some(Ok(w)) => Some(w),
        Some(Err(e)) if e.kind() == io::ErrorKind::ConnectionRefused => None,
        Some(Err(e)) => return Err(e.to_string()),
    };
    if let Some(wire) = wire {
        let reply = exchange_on(wire, &frame).map_err(|e| e.to_string())?;
        return if reply_ok(&reply) {
            Ok(())
        } else {
            Err("RevokeFailed".to_owned())
        };
    }
    let registry = crate::registry::Registry::open(state).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(state.join("service.lock"))
        .map_err(|e| e.to_string())?;
    // SAFETY: flock on an fd we own; released when `lock` drops.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let vault = crate::vault::Vault::new(state, store).map_err(|e| e.to_string())?;
    crate::legacy::remove(registry.connection(), &vault, workflow).map_err(|e| e.to_string())?;
    crate::legacy::finish_removals(registry.connection(), &vault, state)
        .map_err(|e| e.to_string())?;
    Ok(())
}
