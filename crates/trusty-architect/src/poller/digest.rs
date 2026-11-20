//! The wake digest: what the woken alerts and the held PM messages say, on
//! the one line the wake types (df-T7i P1b).
//!
//! Why: a bare "N new alerts" makes the Architect open `alerts.jsonl` for
//! every wake; the prototype appends a capped one-line digest (D22, Db8) and
//! carries the woken alerts in `poll-state.json` until a wake is sent, so a
//! failing wake cannot lose them or grow the list for days.
//! What: [`WakeItem`] is one carried alert (`state["wake_alerts"]`);
//! [`bound_wake_alerts`] dedupes and caps that list; [`held_since`] reads the
//! held (non-wake) PM messages newer than a cutoff, classified by
//! [`pm_message_wakes`] (`pm_intake.classify`); [`wake_digest`] joins both
//! (`pm-digest.py wake_digest`); [`cap_wake`] puts the digest on the wake
//! line within [`WAKE_MAX_CHARS`]. The reads never fail: a missing or bad
//! file is no messages.
//! Test: `dispatch_wake_matches_python_goldens` (tests/parity_wake.rs),
//! `digest_matches_python_goldens` (tests/parity_p1b.rs),
//! `bound_wake_alerts_keeps_the_newest_fifty_and_counts_drops`.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::classify::Alert;
use crate::classify::alerts::iso;
use crate::pystr::{is_digit, is_space, is_word};

/// A typed wake longer than this cannot be confirmed in the input box.
pub const WAKE_MAX_CHARS: usize = 400;
/// The carried `wake_alerts` backlog; the oldest are dropped and counted.
pub const WAKE_ALERTS_MAX: usize = 50;
const DIGEST_CAP: usize = 8;
const DIGEST_WIDTH: usize = 140;

/// One woken alert carried in `poll-state.json` until a wake is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeItem {
    /// The alert's session (`""` for a fleet-wide one).
    pub session: Option<String>,
    /// The alert kind.
    pub kind: Option<String>,
    /// The alert detail.
    pub detail: Option<String>,
}

impl From<&Alert> for WakeItem {
    fn from(a: &Alert) -> Self {
        Self {
            session: Some(a.session.clone()),
            kind: Some(a.kind.clone()),
            detail: Some(a.detail.clone()),
        }
    }
}

/// Reads `state["wake_alerts"]`; entries that are not objects are skipped.
pub(crate) fn read_items(other: &Map<String, Value>) -> Vec<WakeItem> {
    let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    other
        .get("wake_alerts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|v| v.is_object())
        .map(|v| WakeItem {
            session: field(v, "session"),
            kind: field(v, "kind"),
            detail: field(v, "detail"),
        })
        .collect()
}

/// `bound_wake_alerts`: keeps the newest copy of each (session, kind,
/// detail), then the newest [`WAKE_ALERTS_MAX`]; adds every dropped entry to
/// `wake_alerts_dropped`. Returns the kept list.
pub fn bound_wake_alerts(other: &mut Map<String, Value>, pend: Vec<WakeItem>) -> Vec<WakeItem> {
    let total = pend.len();
    let mut kept: Vec<WakeItem> = Vec::new();
    for a in pend.into_iter().rev() {
        if !kept.contains(&a) {
            kept.push(a);
        }
    }
    kept.reverse();
    let cut = kept.len().saturating_sub(WAKE_ALERTS_MAX);
    kept.drain(..cut);
    let dropped = (total - kept.len()) as u64;
    if dropped > 0 {
        let before = other
            .get("wake_alerts_dropped")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        other.insert("wake_alerts_dropped".to_owned(), (before + dropped).into());
    }
    let list = kept.iter().map(item_json).collect();
    other.insert("wake_alerts".to_owned(), Value::Array(list));
    kept
}

fn item_json(a: &WakeItem) -> Value {
    let mut m = Map::new();
    for (k, v) in [
        ("session", &a.session),
        ("kind", &a.kind),
        ("detail", &a.detail),
    ] {
        m.insert(k.to_owned(), v.clone().map_or(Value::Null, Value::String));
    }
    Value::Object(m)
}

/// `pm_intake.classify(msg)[0] == "wake"`: urgent, `head`, a wake kind, an
/// unknown kind, or a held kind whose text asks for a ruling or names a
/// session snapshot.
pub fn pm_message_wakes(msg: &Map<String, Value>) -> bool {
    let truthy = |v: Option<&Value>| match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    };
    let kind = match msg.get("kind") {
        Some(Value::String(s)) => s.clone(),
        v if truthy(v) => v.map(Value::to_string).unwrap_or_default(),
        _ => String::new(),
    };
    if truthy(msg.get("urgent")) || !matches!(kind.as_str(), "ack" | "progress" | "fyi") {
        return true;
    }
    let text = match msg.get("text") {
        Some(Value::String(s)) => s.clone(),
        v if truthy(v) => v.map(Value::to_string).unwrap_or_default(),
        _ => String::new(),
    };
    asks(&strip_tags(&text)) || names_snapshot(&text)
}

/// `ASKS`: `\?|\b(approve|choose|which)\b`, case-insensitive.
fn asks(text: &str) -> bool {
    if text.contains('?') {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().flat_map(|c| c.to_lowercase()).collect();
    if lower.len() != chars.len() {
        // A case fold that changes length: fall back to a per-word check.
        return text
            .split(|c: char| !is_word(c))
            .any(|w| ["approve", "choose", "which"].contains(&w.to_lowercase().as_str()));
    }
    ["approve", "choose", "which"].iter().any(|w| {
        let w: Vec<char> = w.chars().collect();
        (0..=lower.len().saturating_sub(w.len())).any(|i| {
            lower.len() >= w.len()
                && lower[i..i + w.len()] == w[..]
                && (i == 0 || !is_word(chars[i - 1]))
                && chars.get(i + w.len()).is_none_or(|c| !is_word(*c))
        })
    })
}

/// `SNAPSHOT`: `\.trusty-mpm/sessions/\S*session-\d{8}-\d{6}\.md`.
fn names_snapshot(text: &str) -> bool {
    let key = ".trusty-mpm/sessions/";
    text.match_indices(key).any(|(i, _)| {
        let rest = &text[i + key.len()..];
        let run: String = rest.chars().take_while(|c| !is_space(*c)).collect();
        run.match_indices("session-").any(|(j, _)| {
            let t: Vec<char> = run[j + 8..].chars().collect();
            let digits = |r: std::ops::Range<usize>| {
                t.get(r).is_some_and(|s| s.iter().all(|c| is_digit(*c)))
            };
            digits(0..8)
                && t.get(8) == Some(&'-')
                && digits(9..15)
                && t[15..].starts_with(&['.', 'm', 'd'])
        })
    })
}

/// `task_tag.strip_tags`: drops the lines that are only a `Task: <id>` tag
/// with `| key: value` fields.
fn strip_tags(text: &str) -> String {
    crate::pystr::splitlines(text)
        .into_iter()
        .filter(|l| !is_tag_line(crate::pystr::strip(l)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `TAGLINE_RE` over one stripped line.
fn is_tag_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("Task:") else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    let Some(mut rest) = task_id(rest) else {
        return false;
    };
    loop {
        let r = rest.trim_start_matches([' ', '\t']);
        if r.is_empty() {
            return true;
        }
        let Some(r) = r.strip_prefix('|') else {
            return false;
        };
        let r = r.trim_start_matches([' ', '\t']);
        let key = r.chars().take_while(char::is_ascii_lowercase).count();
        if key == 0 || !r[key..].starts_with(':') {
            return false;
        }
        rest = r[key + 1..].trim_start_matches(|c| c != '|');
    }
}

/// `IL_ID(@\d{4})?` or `DASH_ID` at the start of `s`; the rest after it.
fn task_id(s: &str) -> Option<&str> {
    let id_char =
        |c: &u8| c.is_ascii_digit() || (c.is_ascii_lowercase() && !matches!(*c, b'l' | b'o'));
    let b = s.as_bytes();
    // DASH_ID: T + 2-4 id chars, then a non-id char (the regex backtracks to
    // the longest run that lets the rest match; the rest needs space, | or end).
    if b.first() == Some(&b'T') {
        let n = b[1..].iter().take_while(|c| id_char(c)).count();
        if (2..=4).contains(&n) {
            return Some(&s[1 + n..]);
        }
    }
    // IL_ID: [a-z][a-z0-9]{1,7}-T + two id chars.
    b.first().filter(|c| c.is_ascii_lowercase())?;
    let n = b[1..]
        .iter()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .count();
    if !(1..=7).contains(&n) || b.get(1 + n..3 + n) != Some(b"-T") {
        return None;
    }
    let id = &b[3 + n..];
    if id.len() < 2 || !id[..2].iter().all(id_char) {
        return None;
    }
    let mut end = 5 + n;
    if b.get(end) == Some(&b'@')
        && b.get(end + 1..end + 5)
            .is_some_and(|d| d.iter().all(u8::is_ascii_digit))
    {
        end += 5;
    }
    Some(&s[end..])
}

/// `held_since(cutoff)`: held PM messages in `path` with `ts` after `cutoff`,
/// oldest first. A missing file or a bad line is skipped.
pub fn held_since(path: &Path, cutoff: f64) -> Vec<Map<String, Value>> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.split('\n')
        .filter_map(|line| serde_json::from_str::<Map<String, Value>>(line).ok())
        .filter(|m| {
            let ts = m.get("ts").and_then(Value::as_str).and_then(parse_ts);
            ts.is_some_and(|t| t > cutoff) && !pm_message_wakes(m)
        })
        .collect()
}

/// `%Y-%m-%dT%H:%M:%SZ` to epoch seconds (UTC).
fn parse_ts(text: &str) -> Option<f64> {
    let b = text.as_bytes();
    if b.len() != 20 || b[19] != b'Z' {
        return None;
    }
    // Round-trip through the formatter: any other shape fails.
    let num = |r: std::ops::Range<usize>| text.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let days = days_from_civil(y, mo, d);
    let t = (days * 86_400 + h * 3600 + mi * 60 + s) as f64;
    (iso(t) == text).then_some(t)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `wake_digest(alerts, msgs, path)`: one `<session> <kind>: <text>` item per
/// carried alert, then per held message, at most eight, then
/// `+N more -> path`, joined with ` | `; `""` when there is nothing.
pub fn wake_digest(alerts: &[WakeItem], msgs: &[Map<String, Value>], path: &Path) -> String {
    let py_str = |v: Option<&Value>, none: &str| match v {
        None | Some(Value::Null) => none.to_owned(),
        Some(Value::String(s)) if s.is_empty() => none.to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(false)) => none.to_owned(),
        Some(other) => other.to_string(),
    };
    let mut items: Vec<(String, String, String)> = alerts
        .iter()
        .map(|a| {
            let s = a
                .session
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "owner".to_owned());
            let k = a
                .kind
                .clone()
                .filter(|k| !k.is_empty())
                .unwrap_or_else(|| "?".to_owned());
            (s, k, a.detail.clone().unwrap_or_default())
        })
        .collect();
    items.extend(msgs.iter().map(|m| {
        let text = match m.get("text") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        };
        (
            py_str(m.get("session"), "?"),
            py_str(m.get("kind"), "?"),
            text,
        )
    }));
    let mut out: Vec<String> = items
        .iter()
        .take(DIGEST_CAP)
        .map(|(s, k, t)| format!("{s} {k}: {}", flat(t)))
        .collect();
    if items.len() > DIGEST_CAP {
        out.push(format!(
            "+{} more -> {}",
            items.len() - DIGEST_CAP,
            path.display()
        ));
    }
    out.join(" | ")
}

/// `_flat`: whitespace runs collapsed to one space, cut to 140 characters.
fn flat(text: &str) -> String {
    let joined = text
        .split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    joined.chars().take(DIGEST_WIDTH).collect()
}

/// `cap_wake(head, digest)`: `head | digest` on one line, at most
/// [`WAKE_MAX_CHARS`] characters; the digest is cut with an ellipsis, and
/// dropped when fewer than 20 characters of it would fit.
pub fn cap_wake(head: &str, digest: &str) -> String {
    let room = WAKE_MAX_CHARS as i64 - head.chars().count() as i64 - 3;
    if digest.is_empty() || room < 20 {
        return head.to_owned();
    }
    let digest = digest
        .split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let room = room as usize;
    if digest.chars().count() <= room {
        format!("{head} | {digest}")
    } else {
        let cut: String = digest.chars().take(room - 1).collect();
        format!("{head} | {cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(s: &str, k: &str, d: &str) -> WakeItem {
        WakeItem {
            session: Some(s.to_owned()),
            kind: Some(k.to_owned()),
            detail: Some(d.to_owned()),
        }
    }

    #[test]
    fn bound_wake_alerts_keeps_the_newest_fifty_and_counts_drops() {
        let mut other = Map::new();
        let mut pend: Vec<WakeItem> = (0..60)
            .map(|i| item("tm-a", "idle", &i.to_string()))
            .collect();
        pend.push(item("tm-a", "idle", "59"));
        let kept = bound_wake_alerts(&mut other, pend);
        assert_eq!(kept.len(), WAKE_ALERTS_MAX);
        assert_eq!(kept[0].detail.as_deref(), Some("10"));
        assert_eq!(other["wake_alerts_dropped"], 11);
    }

    #[test]
    fn digest_overflow_names_the_alerts_path() {
        let alerts: Vec<WakeItem> = (0..9).map(|i| item("", "load", &i.to_string())).collect();
        let d = wake_digest(&alerts, &[], Path::new("/a.jsonl"));
        assert!(d.starts_with("owner load: 0 | "));
        assert!(d.ends_with(" | +1 more -> /a.jsonl"));
    }

    #[test]
    fn missing_messages_file_is_no_messages() {
        assert!(held_since(Path::new("/nonexistent/pm-messages.jsonl"), 0.0).is_empty());
    }
}
