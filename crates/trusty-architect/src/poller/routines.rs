//! D5r: did the Architect's routines run, and is a failure overdue a push?
//!
//! Why: Bob's D5r extends the watchdog: a routine that silently stopped
//! (heartbeat cron, the poller itself, the morning brief, the 06:30
//! dreaming/owner-model run, ID retirement) is invisible until someone
//! notices. A failure the Architect has not cleared in 30 min is pushed to
//! Bob, behind a config key that ships unset (#9779, D5r).
//! What: [`routine_alerts`] reads the routine list from local config
//! (`<root>/local/routines.json`, a JSON array of `{name, stamp | goals_dir,
//! interval_s, stale_after_s?}`; names and paths never live in code) and
//! raises `routine_stale` when a stamp file is missing or older than
//! `stale_after_s` (default twice the interval). A `goals_dir` routine
//! (goal_reconcile, owner ruling 2026-11-20) instead reads each goal doc in
//! that folder and flags every `lifecycle: "active"` goal whose
//! `reconciled_at` is missing, unreadable or stale; an unreadable folder or
//! doc is flagged too. A missing list turns the check off; a bad one is an
//! error the cycle raises as `routine_stale` / `config`.
//! [`push_due`] tracks how long each failure key has been active in
//! `state["push"]` and reports each one that passes [`PUSH_AFTER`] once.
//! Nothing here sends: with `ARCHITECT_PUSH_CHANNEL` unset the line says so,
//! and this build has no sender for a set channel either.
//! Test: `routines_flag_a_stale_and_a_missing_stamp`,
//! `routines_file_absent_turns_the_check_off`,
//! `routines_file_malformed_is_an_error`,
//! `routines_relative_stamp_is_an_error`,
//! `goal_reconcile_fresh_goals_raise_nothing`,
//! `goal_reconcile_flags_a_stale_active_goal`,
//! `goal_reconcile_skips_an_inactive_stale_goal`,
//! `goal_reconcile_flags_a_missing_reconciled_at`,
//! `goal_reconcile_unreadable_doc_fails_closed`,
//! `routines_stale_after_override`,
//! `push_due_fires_once_after_thirty_minutes_and_clears`.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::classify::Alert;
use crate::classify::alerts::{alert, days_from_civil};

/// Seconds a failure stays active before it is due a push to the owner.
pub const PUSH_AFTER: f64 = 1800.0;

/// One routine the watchdog expects to run; exactly one of `stamp` and
/// `goals_dir` is set.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Routine {
    /// Display name, also the alert fingerprint.
    pub name: String,
    /// Absolute path of the file the routine touches when it runs.
    #[serde(default)]
    pub stamp: Option<PathBuf>,
    /// Absolute folder of goal docs (`<id>.json`) whose `reconciled_at` the
    /// routine writes; every active goal is checked.
    #[serde(default)]
    pub goals_dir: Option<PathBuf>,
    /// Expected seconds between runs.
    pub interval_s: f64,
    /// Seconds after which a run is stale; default twice `interval_s`.
    #[serde(default)]
    pub stale_after_s: Option<f64>,
}

impl Routine {
    /// Seconds after which the routine counts as stale.
    pub fn stale_after(&self) -> f64 {
        self.stale_after_s.unwrap_or(2.0 * self.interval_s)
    }
}

/// Why the routine list could not be used.
#[derive(Debug, thiserror::Error)]
pub enum RoutinesError {
    /// The file exists but could not be read.
    #[error("read {path}: {source}")]
    Read {
        /// The routines file.
        path: PathBuf,
        /// The read error.
        #[source]
        source: io::Error,
    },
    /// The file is not a JSON array of routines.
    #[error("parse {path}: {source}")]
    Parse {
        /// The routines file.
        path: PathBuf,
        /// The parse error.
        #[source]
        source: serde_json::Error,
    },
    /// A routine's paths or durations are not usable.
    #[error("routine {name:?}: {why}")]
    Invalid {
        /// The routine's name.
        name: String,
        /// What is wrong.
        why: &'static str,
    },
}

/// Reads the routine list; `Ok(None)` when the file does not exist.
///
/// # Errors
/// [`RoutinesError`] for an unreadable, malformed or invalid list.
pub fn load_routines(path: &Path) -> Result<Option<Vec<Routine>>, RoutinesError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(RoutinesError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let list: Vec<Routine> =
        serde_json::from_str(&text).map_err(|source| RoutinesError::Parse {
            path: path.to_owned(),
            source,
        })?;
    let positive = |v: f64| v.is_finite() && v > 0.0;
    for r in &list {
        let why = match (&r.stamp, &r.goals_dir) {
            (Some(_), Some(_)) | (None, None) => "set exactly one of stamp and goals_dir",
            (Some(p), None) if !p.is_absolute() => "stamp must be an absolute path",
            (None, Some(p)) if !p.is_absolute() => "goals_dir must be an absolute path",
            _ if !positive(r.interval_s) => "interval_s must be positive",
            _ if r.stale_after_s.is_some_and(|v| !positive(v)) => "stale_after_s must be positive",
            _ => continue,
        };
        return Err(RoutinesError::Invalid {
            name: r.name.clone(),
            why,
        });
    }
    Ok(Some(list))
}

fn mtime(path: &Path) -> Option<f64> {
    let modified = path.metadata().ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// `routine_stale` alerts, raised on `session`, for the list at `path`.
///
/// # Errors
/// As [`load_routines`]; the caller logs it and the check is off this cycle.
pub fn routine_alerts(path: &Path, session: &str, now: f64) -> Result<Vec<Alert>, RoutinesError> {
    let Some(list) = load_routines(path)? else {
        return Ok(Vec::new());
    };
    let every = |r: &Routine| (r.interval_s / 60.0).round() as i64;
    let mut out = Vec::new();
    for r in &list {
        let stamp = match (&r.stamp, &r.goals_dir) {
            (Some(stamp), _) => stamp,
            (None, Some(dir)) => {
                out.extend(goal_alerts(r, dir, session, now));
                continue;
            }
            (None, None) => continue, // refused by load_routines
        };
        let detail = match mtime(stamp) {
            None => format!(
                "{}: no run recorded (expected every {} min)",
                r.name,
                every(r)
            ),
            Some(t) if now - t > r.stale_after() => format!(
                "{}: last ran {} min ago (expected every {} min)",
                r.name,
                ((now - t) / 60.0).floor() as i64,
                every(r)
            ),
            Some(_) => continue,
        };
        out.push(alert("routine_stale", &r.name, &detail, session, "", now));
    }
    Ok(out)
}

/// `YYYY-MM-DDTHH:MM:SSZ` as UTC epoch seconds; `None` for any other form.
fn parse_utc(text: &str) -> Option<f64> {
    let b = text.as_bytes();
    let shape = b.len() == 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T';
    if !shape || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| text.get(r)?.parse::<i64>().ok();
    let (mo, d, h, mi, s) = (n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(n(0..4)?, mo, d);
    Some((days * 86_400 + h * 3_600 + mi * 60 + s) as f64)
}

/// Why one goal doc fails the goal_reconcile check; `None` when it passes
/// or the goal is not active.
fn goal_problem(path: &Path, stale_after: f64, now: f64) -> Option<String> {
    let doc: Value = match fs::read_to_string(path).map(|t| serde_json::from_str(&t)) {
        Ok(Ok(doc)) => doc,
        Ok(Err(e)) => return Some(format!("unreadable ({e})")),
        Err(e) => return Some(format!("unreadable ({e})")),
    };
    let Value::Object(doc) = doc else {
        return Some("unreadable (not a JSON object)".to_owned());
    };
    if doc.get("lifecycle").and_then(Value::as_str) != Some("active") {
        return None;
    }
    let Some(raw) = doc.get("reconciled_at").and_then(Value::as_str) else {
        return Some("never reconciled (no reconciled_at)".to_owned());
    };
    let Some(t) = parse_utc(raw) else {
        return Some(format!("reconciled_at {raw:?} is not YYYY-MM-DDTHH:MM:SSZ"));
    };
    let age = now - t;
    (age > stale_after).then(|| {
        let hours = |s: f64| (s / 3600.0).floor() as i64;
        format!(
            "last reconciled {} h ago (stale after {} h)",
            hours(age),
            hours(stale_after)
        )
    })
}

/// One `routine_stale` alert per active goal in `dir` that fails the check,
/// fingerprinted `<routine>:<goal id>`; an unreadable folder is one alert.
fn goal_alerts(r: &Routine, dir: &Path, session: &str, now: f64) -> Vec<Alert> {
    let raise = |fp: &str, why: String| {
        let detail = format!("{}: {why}", r.name);
        alert("routine_stale", fp, &detail, session, "", now)
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return vec![raise(&r.name, format!("goals dir {}: {e}", dir.display()))],
    };
    let mut docs = Vec::new();
    for entry in entries {
        match entry {
            Ok(e) => docs.push(e.path()),
            Err(e) => return vec![raise(&r.name, format!("goals dir {}: {e}", dir.display()))],
        }
    }
    docs.retain(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        name.ends_with(".json") && !name.starts_with('.')
    });
    docs.sort();
    let mut out = Vec::new();
    for path in docs {
        let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        if let Some(why) = goal_problem(&path, r.stale_after(), now) {
            out.push(raise(
                &format!("{}:{id}", r.name),
                format!("goal {id} {why}"),
            ));
        }
    }
    out
}

/// Log lines for each failure key active for [`PUSH_AFTER`]; `active` are
/// this cycle's failure keys (`kind:fingerprint`). Tracks first-seen times
/// in `other["push"]`, drops cleared keys, and reports each key once.
pub fn push_due(
    other: &mut Map<String, Value>,
    active: &[String],
    now: f64,
    channel: Option<&str>,
) -> Vec<String> {
    if active.is_empty() && !other.contains_key("push") {
        return Vec::new(); // nothing tracked: leave the state file untouched
    }
    let entry = other
        .entry("push".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let mut lines = Vec::new();
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
        lines.push("push: state record was not an object; reset".to_owned());
    }
    let Value::Object(book) = entry else {
        return lines;
    };
    book.retain(|k, _| active.contains(k));
    for key in active {
        let rec = book.entry(key.clone()).or_insert_with(|| {
            let mut m = Map::new();
            m.insert("since".to_owned(), Value::from(now));
            Value::Object(m)
        });
        let since = rec.get("since").and_then(Value::as_f64).unwrap_or(now);
        let sent = rec.get("reported").and_then(Value::as_bool) == Some(true);
        if sent || now - since < PUSH_AFTER {
            continue;
        }
        if let Value::Object(m) = rec {
            m.insert("reported".to_owned(), Value::Bool(true));
        }
        let why = match channel {
            None => "ARCHITECT_PUSH_CHANNEL unset; nothing sent".to_owned(),
            Some(c) => format!("channel {c} has no sender in this build; nothing sent"),
        };
        lines.push(format!("push due: {key} uncleared 30 min ({why})"));
    }
    lines
}
