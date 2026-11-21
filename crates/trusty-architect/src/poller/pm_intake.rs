//! PM-message intake: `pm_intake.py`'s `poll_step`, the step that turns
//! `inbox/pm-messages.jsonl` into `pm_message` wake alerts.
//!
//! Why: the deployed `fleet-poll.py` raises `pm_message` from this queue every
//! cycle; the Rust poller had no step, so the shadow never raised one (MB1595).
//! What: [`pm_intake_alerts`] reads the lines added since `state["pm_offset"]`
//! (a smaller file restarts at 0; a partial last line waits), marks a repeat of
//! `(session, kind, pr, head, text)` inside [`DEDUP_SECONDS`] as held, and
//! returns one `pm_message` alert, fingerprint `sha1(line)[:12]`, per message
//! [`pm_message_wakes`] wakes. Held messages are only logged: the prototype
//! also writes them to `triage.jsonl`, which this crate's triage does not own
//! yet. A missing file is no alerts.
//! Test: `shadow_reads_selftest_and_pm_messages_from_the_live_inbox`
//! (tests/shadow_parity_gaps.rs), `pm_intake_holds_repeats_and_waits_for_a_whole_line`.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use super::digest::pm_message_wakes;
use crate::classify::{Alert, alert};
use crate::sha1::sha1_hex;

/// A repeat of the same message inside this many seconds is held.
pub const DEDUP_SECONDS: f64 = 1800.0;

/// Python `str(v)` for a JSON value the message carries.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Null => "None".to_owned(),
        other => other.to_string(),
    }
}

/// Python `str(msg.get(k) or "")`: a falsy value reads as empty.
fn field(msg: &Map<String, Value>, k: &str) -> String {
    match msg.get(k) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => String::new(),
        Some(Value::Array(a)) if a.is_empty() => String::new(),
        Some(Value::Object(o)) if o.is_empty() => String::new(),
        Some(v) => py_str(v),
    }
}

/// `pm_intake_alerts(state, now, dry_run)`; in a dry run `log` gets one line
/// per held message, as the prototype prints them.
pub fn pm_intake_alerts(
    state: &mut Map<String, Value>,
    path: &Path,
    now: f64,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Vec<Alert> {
    let bytes = fs::read(path).unwrap_or_default();
    let size = bytes.len() as u64;
    let mut off = state.get("pm_offset").and_then(Value::as_u64).unwrap_or(0);
    if size < off {
        off = 0;
    }
    let mut out = Vec::new();
    if size > off {
        let chunk = &bytes[off as usize..];
        let end = chunk.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        off += end as u64;
        let mut seen = state
            .get("pm_seen")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&chunk[..end]).into_owned();
        for line in crate::pystr::splitlines(&text) {
            let Ok(Value::Object(msg)) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let key = ["session", "kind", "pr", "head", "text"]
                .map(|k| field(&msg, k))
                .join("|");
            let last = seen.get(&key).and_then(Value::as_f64).unwrap_or(0.0);
            let dup = now - last < DEDUP_SECONDS;
            if !dup {
                seen.insert(key, Value::from(now));
            }
            let session = field(&msg, "session");
            if dup || !pm_message_wakes(&msg) {
                if dry_run {
                    let why = if dup {
                        "duplicate within 30 min"
                    } else {
                        "held"
                    };
                    log.push(format!("  pm-message held ({session}): {why}"));
                }
                continue;
            }
            let mid = {
                let mut h = sha1_hex(line.as_bytes());
                h.truncate(12);
                h
            };
            let kind = field(&msg, "kind");
            let mut bits = vec![if kind.is_empty() {
                "?".to_owned()
            } else {
                kind
            }];
            let pr = field(&msg, "pr");
            if !pr.is_empty() {
                bits.push(format!("PR#{pr}"));
            }
            let head = field(&msg, "head");
            if !head.is_empty() {
                bits.push(head.chars().take(10).collect());
            }
            let origin = if msg.get("source").and_then(Value::as_str) == Some("mcp") {
                let sender = field(&msg, "sender");
                let sender = if sender.is_empty() {
                    "?".to_owned()
                } else {
                    sender
                };
                let sender: String = sender.chars().take(64).collect();
                format!("[OUTSIDE MCP sender={sender}, not the owner; id {mid}] ")
            } else {
                String::new()
            };
            let detail = format!("{origin}{}: {}", bits.join(" "), field(&msg, "text"));
            out.push(alert("pm_message", &mid, &detail, &session, "", now));
        }
        seen.retain(|_, t| t.as_f64().is_some_and(|t| now - t < DEDUP_SECONDS));
        state.insert("pm_seen".to_owned(), Value::Object(seen));
    }
    state.insert("pm_offset".to_owned(), Value::from(off));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pm_intake_holds_repeats_and_waits_for_a_whole_line() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("pm-messages.jsonl");
        let done = r#"{"session": "tm-a", "kind": "done", "text": "x"}"#;
        fs::write(&path, format!("{done}\n{done}\n{{\"kind\": \"blocker\""))?;
        let (mut st, mut log) = (Map::new(), Vec::new());
        let got = pm_intake_alerts(&mut st, &path, 1.0e9, true, &mut log);
        assert_eq!(got.len(), 1, "the repeat is held");
        assert_eq!(got[0].detail, "done: x");
        assert_eq!(log, ["  pm-message held (tm-a): duplicate within 30 min"]);
        assert_eq!(st["pm_offset"], Value::from(2 * (done.len() as u64 + 1)));
        assert!(pm_intake_alerts(&mut st, &path, 1.0e9 + 100.0, true, &mut log).is_empty());
        Ok(())
    }
}
