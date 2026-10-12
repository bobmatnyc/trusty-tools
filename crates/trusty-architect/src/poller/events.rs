//! Hook events (`events.jsonl`) that become alerts.
//!
//! Why: Claude Code's notification hook appends `permission_prompt` and
//! `agent_needs_input` events that a pane capture can miss (#9171 A3a).
//! What: [`read_events`] is the prototype's `read_events`: it reads the bytes
//! after the stored offset, turns each JSON line of an [`EVENT_TYPES`] type
//! from a watched session into an alert, and stores the new offset. The first
//! run (no offset) skips the backlog; a file smaller than the offset was
//! rotated and is read from 0; a line that is not JSON is skipped. The offset
//! moves past a truncated last line, as in the prototype.
//! Test: `test_events_become_alerts_except_idle_prompt`,
//! `read_events_matches_python_goldens`.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use super::SKIP_SESSIONS;
use crate::classify::{Alert, alert};
use crate::classify::alerts::fingerprint;
use crate::pystr::splitlines;

/// Event types that raise an alert; `idle_prompt` is not one.
pub const EVENT_TYPES: [&str; 2] = ["permission_prompt", "agent_needs_input"];

/// A non-empty string field, else `""` (Python's `ev.get(k) or ""`).
// #9171: a truthy non-string value reads as "" here; the prototype would
// carry it into the alert as a JSON number or list.
fn text_field<'a>(ev: &'a serde_json::Map<String, Value>, key: &str) -> &'a str {
    ev.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// New alert-worthy events since `offset`; updates `offset`.
///
/// # Errors
/// The file exists but cannot be read (the prototype fails the cycle).
pub fn read_events(path: &Path, offset: &mut Option<u64>, now: f64) -> io::Result<Vec<Alert>> {
    let size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    let mut off = match *offset {
        None => size,
        Some(o) if size < o => 0,
        Some(o) => o,
    };
    let mut out = Vec::new();
    if size > off {
        let mut f = File::open(path)?;
        f.seek(SeekFrom::Start(off))?;
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        off += bytes.len() as u64;
        let chunk = String::from_utf8_lossy(&bytes);
        for line in splitlines(&chunk) {
            let Ok(Value::Object(ev)) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let kind = text_field(&ev, "type");
            let session = text_field(&ev, "tmux_session");
            if !EVENT_TYPES.contains(&kind) || SKIP_SESSIONS.contains(&session) {
                continue;
            }
            let message = match text_field(&ev, "message") {
                "" => kind,
                m => m,
            };
            let pane = text_field(&ev, "tmux_pane");
            out.push(alert(kind, &fingerprint(line), message, session, pane, now));
        }
    }
    *offset = Some(off);
    Ok(out)
}
