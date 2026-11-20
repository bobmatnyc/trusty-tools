//! The Intent Ledger fresh/ready predicate, ported from `datg_read.op_fresh`
//! and `datg_ready.op_ready` (#11366).
//!
//! Why: `tm architect restart` (#11365) must refuse unless the Architect's
//! ledger holds a fresh checkpoint. Only `datg.py` computed that verdict, so
//! the gate needs it in Rust. `datg.py` itself stays Python (ruling Dpe).
//! What: [`fresh`] and [`ready`] read a `datg` state directory (`state.json`
//! plus the optional `stamp.json`) and return the same exit code `datg.py
//! fresh` / `datg.py ready` print: 0 fresh/ready, 1 stale/not ready,
//! 2 missing or unreadable. Read-only; never writes the store.
//! Test: `tests/ledger_fresh.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Session values `datg.py` treats as no value at all (`datg_store.VAGUE`).
const VAGUE: [&str; 11] = [
    "latest", "main", "master", "head", "current", "tbd", "unknown", "?", "n/a", "none", "-",
];
/// The marker `tm fleet init` seeds when no task or next is known (`datg_ready.NEEDS_INPUT`).
const NEEDS_INPUT: &str = "needs owner input:";

/// A `datg.py fresh` / `ready` verdict: the exit code and its text lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// 0 fresh/ready, 1 stale/not ready, 2 missing or unreadable.
    pub code: i32,
    /// One line per reason (`fresh` prints exactly one).
    pub reasons: Vec<String>,
}

impl Verdict {
    fn new(code: i32, reason: impl Into<String>) -> Self {
        Self {
            code,
            reasons: vec![reason.into()],
        }
    }

    /// True only for code 0; every other code, unreadable included, is not fresh.
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// The `datg` state directory of `project`, as `datg_store.datg_dir` resolves
/// it without tmux: `<project>/.trusty-mpm/datg/<session>` when `session`
/// names a directory holding a `state.json`, else the legacy single directory.
pub fn state_dir(project: &Path, session: Option<&str>) -> PathBuf {
    let base = project.join(".trusty-mpm").join("datg");
    match session {
        Some(s) if !s.is_empty() && base.join(s).join("state.json").is_file() => base.join(s),
        _ => base,
    }
}

/// `load_state`: the parsed `state.json`, or the reason it is unusable.
fn load_state(dir: &Path) -> Result<Map<String, Value>, String> {
    let text = std::fs::read_to_string(dir.join("state.json")).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "state.json missing; run init".to_owned()
        } else {
            format!("state.json malformed ({e}); refusing to write")
        }
    })?;
    let st: Value = serde_json::from_str(&text)
        .map_err(|e| format!("state.json malformed ({e}); refusing to write"))?;
    let shaped = st.as_object().filter(|o| {
        let code_ok = o.get("code").and_then(Value::as_str).is_some_and(code_ok);
        code_ok
            && o.get("v").is_some_and(Value::is_i64)
            && o.get("next").is_some_and(Value::is_object)
            && ["goals", "items", "tasks"]
                .iter()
                .all(|k| o.get(*k).is_some_and(Value::is_array))
            && o.get("session").is_none_or(Value::is_object)
    });
    shaped
        .cloned()
        .ok_or_else(|| "state.json malformed (bad shape); refusing to write".to_owned())
}

/// `CODE_RE`: `^[a-z][a-z0-9]{1,7}$`.
fn code_ok(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=8).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// `parse_ts`: `YYYY-MM-DDTHH:MM[:SS]Z` as UTC epoch seconds; `None` otherwise.
fn parse_ts(v: Option<&Value>) -> Option<i64> {
    let norm = super::limits::normalize_ts(v?.as_str()?).ok()?;
    let n = |r: std::ops::Range<usize>| norm.get(r)?.parse::<i64>().ok();
    let days = crate::dashboard::derive::days_from_civil(n(0..4)?, n(5..7)?, n(8..10)?);
    Some(days * 86_400 + n(11..13)? * 3_600 + n(14..16)? * 60 + n(17..19)?)
}

/// `datg.py fresh [--since EPOCH] [--max-age MIN]` against the store in `dir`.
///
/// Fresh when the session was written after the last recorded change
/// (`stamp.json` `changed_epoch`, or `since`) and, with `max_age_min`, no
/// older than that. A store that cannot be read is code 2, never fresh.
pub fn fresh(dir: &Path, since: Option<f64>, max_age_min: Option<f64>, now: f64) -> Verdict {
    let st = match load_state(dir) {
        Ok(st) => st,
        Err(e) => return Verdict::new(2, format!("unreadable: {e}")),
    };
    let empty = Map::new();
    let ses = st
        .get("session")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    // `_read_json`: an absent or malformed stamp reads as {}.
    let stamp: Map<String, Value> = std::fs::read_to_string(dir.join("stamp.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let mut floor = since.or_else(|| stamp.get("changed_epoch").and_then(Value::as_f64));
    let state_v = st.get("v").and_then(Value::as_i64).unwrap_or(0);
    if since.is_none()
        && stamp
            .get("v")
            .and_then(Value::as_i64)
            .is_some_and(|sv| state_v > sv)
    {
        floor = None; // a write landed after the Stop that recorded the change
    }
    let Some(w_at) = parse_ts(ses.get("written_at")) else {
        return Verdict::new(1, "stale: no session write recorded");
    };
    let exact = ses.get("written_epoch").and_then(Value::as_i64);
    let w = exact.unwrap_or(w_at) as f64;
    if exact.is_none() {
        floor = floor.map(|f| f - f.rem_euclid(60.0)); // written_at has minute precision
    }
    let info = format!(
        "written_at={}",
        ses.get("written_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
    );
    if floor.is_some_and(|f| if exact.is_some() { w <= f } else { w < f }) {
        return Verdict::new(
            1,
            format!("stale: state changed after the last write; {info}"),
        );
    }
    if let Some(max) = max_age_min.filter(|m| now - w > m * 60.0) {
        return Verdict::new(1, format!("stale: write older than {max} min; {info}"));
    }
    Verdict::new(0, format!("fresh: {info}"))
}

/// `_placeholder`: no value, under 3 characters, a vague word or the seeded marker.
fn placeholder(v: Option<&Value>) -> bool {
    let text = match v {
        None | Some(Value::Null | Value::Bool(false)) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    let t = text.trim().to_lowercase();
    t.chars().count() < 3 || VAGUE.contains(&t.as_str()) || t.starts_with(NEEDS_INPUT)
}

/// Items of `list` in `st` whose `status` is one of `want`.
fn count_status(st: &Map<String, Value>, list: &str, want: &[&str]) -> usize {
    st.get(list).and_then(Value::as_array).map_or(0, |a| {
        a.iter()
            .filter(|i| {
                i.get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| want.contains(&s))
            })
            .count()
    })
}

/// `datg.py ready`: the readiness check `self-clear.sh` `il_ok` runs.
///
/// Ready (0) needs a SESSION task and next that are not placeholders, an
/// active goal or open/running task, `owed` and `crons` recorded, and
/// [`fresh`]. 1 lists each failed rule; 2 means missing or unreadable.
pub fn ready(dir: &Path, since: Option<f64>, max_age_min: Option<f64>, now: f64) -> Verdict {
    if !dir.join("state.json").exists() {
        return Verdict::new(2, "the Intent Ledger is missing; run datg.py init");
    }
    let st = match load_state(dir) {
        Ok(st) => st,
        Err(e) => return Verdict::new(2, format!("the Intent Ledger cannot be read ({e})")),
    };
    let empty = Map::new();
    let ses = st
        .get("session")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut reasons = Vec::new();
    for (key, what) in [
        ("task", "what is being worked on"),
        ("next", "the next step"),
    ] {
        if placeholder(ses.get(key)) {
            reasons.push(format!("no {key} recorded ({what})"));
        }
    }
    if count_status(&st, "goals", &["active"]) + count_status(&st, "tasks", &["open", "running"])
        == 0
    {
        reasons.push("no open goal or task item: there is no saved work to resume".to_owned());
    }
    let owed_none = ses
        .get("owed_none")
        .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)));
    if !ses.contains_key("owed") && !owed_none {
        reasons.push("what is owed is not recorded".to_owned());
    }
    if !ses.contains_key("crons") {
        reasons.push("the scheduled jobs are not recorded".to_owned());
    }
    let f = fresh(dir, since, max_age_min, now);
    match f.code {
        0 => {}
        2 => {
            return Verdict::new(
                2,
                format!(
                    "the Intent Ledger cannot be read ({})",
                    f.reasons.join("; ")
                ),
            );
        }
        _ => reasons.push(format!(
            "the ledger was not written after the latest change ({})",
            f.reasons.join("; ")
        )),
    }
    let code = if reasons.is_empty() { 0 } else { 1 };
    if reasons.is_empty() {
        reasons.push("ready: task, next, saved work, owed and crons recorded; fresh".to_owned());
    }
    Verdict { code, reasons }
}
