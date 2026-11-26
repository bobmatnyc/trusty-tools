//! The edits `tm il config` makes, as pure functions on a [`Schedule`] (#11001).
//!
//! Why: the edits take every input as an argument, so the tests never touch
//! a real Architect directory; [`super::update`] runs them under the lock.
//! What: [`Schedule::enroll`], [`Schedule::remove`], [`Schedule::set`] and
//! [`resolve_project`]. Each validates before it changes anything, so a
//! refusal leaves the schedule as it was.
//! Test: `core::il_schedule::tests`.

use std::path::Path;

use serde_json::{Map, Value};

use super::fields::{SESSION_KEYS, THRESHOLDS, is_threshold};
use super::{IlScheduleError, Schedule, Session};

/// The signal ids a session may list.
const SIGNALS: &[&str] = &["S1", "S2", "S3", "S4", "S5"];

/// What an edit did: whether the schedule changed, and one line to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Whether the schedule must be written.
    pub changed: bool,
    /// The command's report line.
    pub message: String,
}

impl Outcome {
    /// A change to publish.
    pub fn changed(message: impl Into<String>) -> Self {
        Self {
            changed: true,
            message: message.into(),
        }
    }

    /// Nothing to write.
    pub fn unchanged(message: impl Into<String>) -> Self {
        Self {
            changed: false,
            message: message.into(),
        }
    }
}

/// `raw` as the absolute, canonical project directory the readers join on.
///
/// Why: `ctx-watch.py` joins `project` with `.trusty-mpm/datg`; a relative or
/// missing directory would make every evaluation of the session fail.
/// What: canonicalizes `raw` (relative to the process cwd); not a directory
/// or not UTF-8 is [`IlScheduleError::BadValue`].
/// Test: `enroll_refuses_a_missing_project`.
pub fn resolve_project(raw: &Path) -> Result<String, IlScheduleError> {
    let bad = |reason: String| IlScheduleError::BadValue {
        key: "project".into(),
        reason,
    };
    let dir = std::fs::canonicalize(raw).map_err(|e| bad(format!("{}: {e}", raw.display())))?;
    if !dir.is_dir() {
        return Err(bad(format!("{} is not a directory", dir.display())));
    }
    dir.into_os_string()
        .into_string()
        .map_err(|p| bad(format!("{} is not UTF-8", p.to_string_lossy())))
}

impl Schedule {
    /// Add session `name` for `project`, with an optional `code`.
    ///
    /// What: `project` goes through [`resolve_project`]. An equal entry is
    /// unchanged; the same name with other values is
    /// [`IlScheduleError::AlreadyEnrolled`].
    /// Test: `enroll_twice_is_unchanged_and_a_conflict_is_refused`.
    pub fn enroll(
        &mut self,
        name: &str,
        project: &Path,
        code: Option<&str>,
    ) -> Result<Outcome, IlScheduleError> {
        check_name(name)?;
        let code = code.map(check_code).transpose()?;
        let project = resolve_project(project)?;
        if let Some(old) = self.sessions.iter().find(|s| s.name == name) {
            if old.project == project && (code.is_none() || old.code == code) {
                return Ok(Outcome::unchanged(format!("unchanged: {name} is enrolled")));
            }
            return Err(IlScheduleError::AlreadyEnrolled {
                name: name.to_owned(),
                project: old.project.clone(),
            });
        }
        let message = format!("enrolled: {name} ({project})");
        self.sessions.push(Session {
            name: name.to_owned(),
            project,
            code,
            signals: None,
            thresholds: Map::new(),
            state_path: None,
            extra: Map::new(),
        });
        Ok(Outcome::changed(message))
    }

    /// Drop session `name`; an unknown name is an error.
    /// Test: `enroll_then_set_then_remove_round_trips`.
    pub fn remove(&mut self, name: &str) -> Result<Outcome, IlScheduleError> {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.name != name);
        if self.sessions.len() == before {
            return Err(IlScheduleError::UnknownSession(name.to_owned()));
        }
        Ok(Outcome::changed(format!("removed: {name}")))
    }

    /// Set `key` to `value`, globally or on `session`.
    ///
    /// Why: issue #11001 AC2 — thresholds and session fields change through
    /// the CLI only.
    /// What: global keys are `thresholds.<key>`. Session keys are `code`,
    /// `signals` (comma list of S1-S5), `state_path` (absolute),
    /// `thresholds.<key>`, and `project` ([`resolve_project`]). Threshold
    /// values are non-negative integers; `stale_pct` is at most 100. An equal value is unchanged.
    /// Test: `set_validates_keys_and_values`, `enroll_then_set_then_remove_round_trips`.
    pub fn set(
        &mut self,
        session: Option<&str>,
        key: &str,
        value: &str,
    ) -> Result<Outcome, IlScheduleError> {
        let label = match session {
            Some(name) => format!("{name}.{key}"),
            None => key.to_owned(),
        };
        let Some(name) = session else {
            let Some(threshold) = key.strip_prefix("thresholds.") else {
                return Err(unknown_key(key, false));
            };
            let n = threshold_value(threshold, value)?;
            return Ok(put(&mut self.thresholds, threshold, n, &label));
        };
        let entry = self.session_mut(name)?;
        if let Some(threshold) = key.strip_prefix("thresholds.") {
            let n = threshold_value(threshold, value)?;
            return Ok(put(&mut entry.thresholds, threshold, n, &label));
        }
        let changed = match key {
            "project" => replace(&mut entry.project, resolve_project(Path::new(value))?),
            "code" => replace(&mut entry.code, Some(check_code(value)?)),
            "signals" => replace(&mut entry.signals, Some(parse_signals(value)?)),
            "state_path" => {
                if !Path::new(value).is_absolute() {
                    return Err(bad("state_path", "must be an absolute path"));
                }
                replace(&mut entry.state_path, Some(value.to_owned()))
            }
            _ => return Err(unknown_key(key, true)),
        };
        Ok(report(changed, &label, value))
    }
}

/// Store `new` in `slot`; whether it changed.
fn replace<T: PartialEq>(slot: &mut T, new: T) -> bool {
    let changed = *slot != new;
    *slot = new;
    changed
}

/// Put threshold `key = n` into `map`.
fn put(map: &mut Map<String, Value>, key: &str, n: u64, label: &str) -> Outcome {
    let new = Value::from(n);
    let changed = map.get(key) != Some(&new);
    map.insert(key.to_owned(), new);
    report(changed, label, &n.to_string())
}

/// The report line of a `set`.
fn report(changed: bool, label: &str, value: &str) -> Outcome {
    if changed {
        Outcome::changed(format!("set: {label} = {value}"))
    } else {
        Outcome::unchanged(format!("unchanged: {label} = {value}"))
    }
}

/// A threshold value, after the key check.
fn threshold_value(key: &str, value: &str) -> Result<u64, IlScheduleError> {
    if !is_threshold(key) {
        let known: Vec<&str> = THRESHOLDS.iter().map(|(k, _)| *k).collect();
        return Err(IlScheduleError::UnknownKey {
            key: format!("thresholds.{key}"),
            hint: format!("known thresholds: {}", known.join(", ")),
        });
    }
    let label = format!("thresholds.{key}");
    let n: u64 = value
        .parse()
        .map_err(|_| bad(&label, "must be a non-negative integer"))?;
    if key == "stale_pct" && n > 100 {
        return Err(bad(&label, "is a percent, 0-100"));
    }
    Ok(n)
}

/// A comma list of signal ids, deduplicated and sorted.
fn parse_signals(value: &str) -> Result<Vec<String>, IlScheduleError> {
    let mut out: Vec<String> = Vec::new();
    for raw in value.split(',').map(str::trim) {
        let id = raw.to_ascii_uppercase();
        if !SIGNALS.contains(&id.as_str()) {
            return Err(bad("signals", &format!("{raw:?} is not one of S1-S5")));
        }
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out.sort();
    Ok(out)
}

/// A tmux session name: 1-64 of `[A-Za-z0-9_-]`.
fn check_name(name: &str) -> Result<(), IlScheduleError> {
    let ok = (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(bad("name", "must be 1-64 letters, digits, '-' or '_'"))
    }
}

/// An IL id code: 2-8 ASCII letters or digits.
fn check_code(code: &str) -> Result<String, IlScheduleError> {
    if (2..=8).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric()) {
        Ok(code.to_owned())
    } else {
        Err(bad("code", "must be 2-8 letters or digits"))
    }
}

/// [`IlScheduleError::BadValue`].
fn bad(key: &str, reason: &str) -> IlScheduleError {
    IlScheduleError::BadValue {
        key: key.to_owned(),
        reason: reason.to_owned(),
    }
}

/// [`IlScheduleError::UnknownKey`] with the keys valid in this position.
fn unknown_key(key: &str, on_session: bool) -> IlScheduleError {
    let hint = if on_session {
        let keys: Vec<&str> = SESSION_KEYS
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| *k != "name")
            .collect();
        format!(
            "session keys: {} (thresholds as thresholds.<key>)",
            keys.join(", ")
        )
    } else {
        "without --session only thresholds.<key> is set; pass --session <name> for a session field"
            .to_owned()
    };
    IlScheduleError::UnknownKey {
        key: key.to_owned(),
        hint,
    }
}
