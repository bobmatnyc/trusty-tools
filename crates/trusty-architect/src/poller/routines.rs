//! D5r: did the Architect's routines run, and is a failure overdue a push?
//!
//! Why: Bob's D5r extends the watchdog: a routine that silently stopped
//! (heartbeat cron, the poller itself, the morning brief, the 06:30
//! dreaming/owner-model run, ID retirement) is invisible until someone
//! notices. A failure the Architect has not cleared in 30 min is pushed to
//! Bob, behind a config key that ships unset (#9779, D5r).
//! What: [`routine_alerts`] reads the routine list from local config
//! (`<root>/local/routines.json`, a JSON array of `{name, stamp, interval_s}`;
//! names and paths never live in code) and raises `routine_stale` when a
//! stamp file is missing or older than twice its interval. A missing list
//! turns the check off; a bad one is an error the cycle logs.
//! [`push_due`] tracks how long each failure key has been active in
//! `state["push"]` and reports each one that passes [`PUSH_AFTER`] once.
//! Nothing here sends: with `ARCHITECT_PUSH_CHANNEL` unset the line says so,
//! and this build has no sender for a set channel either.
//! Test: `routines_flag_a_stale_and_a_missing_stamp`,
//! `routines_file_absent_turns_the_check_off`,
//! `routines_file_malformed_is_an_error`,
//! `routines_relative_stamp_is_an_error`,
//! `push_due_fires_once_after_thirty_minutes_and_clears`.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::classify::Alert;
use crate::classify::alerts::alert;

/// Seconds a failure stays active before it is due a push to the owner.
pub const PUSH_AFTER: f64 = 1800.0;

/// One routine the watchdog expects to run.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Routine {
    /// Display name, also the alert fingerprint.
    pub name: String,
    /// Absolute path of the file the routine touches when it runs.
    pub stamp: PathBuf,
    /// Expected seconds between runs; stale after twice this.
    pub interval_s: f64,
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
    /// A routine's stamp path is relative or its interval is not positive.
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
    for r in &list {
        let why = if !r.stamp.is_absolute() {
            "stamp must be an absolute path"
        } else if r.interval_s.is_nan() || r.interval_s <= 0.0 {
            "interval_s must be positive"
        } else {
            continue;
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
        let detail = match mtime(&r.stamp) {
            None => format!(
                "{}: no run recorded (expected every {} min)",
                r.name,
                every(r)
            ),
            Some(t) if now - t > 2.0 * r.interval_s => format!(
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
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    let Value::Object(book) = entry else {
        return Vec::new();
    };
    book.retain(|k, _| active.contains(k));
    let mut lines = Vec::new();
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
