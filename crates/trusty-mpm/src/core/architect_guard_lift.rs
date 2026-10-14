//! The Architect's PM-guard lift: read it, set it, remove it (#8436 PR C).
//!
//! Why: owner ruling Dca A — the lift loosens the pm-guard, so it is the
//! owner's choice at `tm fleet init` and defaults OFF. The vehicle is the one
//! the live Architect uses today: `env.TRUSTY_MPM_PM_UNRESTRICTED = "1"` in
//! the project's `.claude/settings.json`, which `pm_guard` reads as exactly
//! `"1"`. Every doubt resolves to not lifted, and a failure never leaves a
//! half-written settings file (plan §4).
//! What: [`read_state`] classifies the file read-only; [`set`] and [`remove`]
//! change only that one key under the same `.lock` sidecar the launch's
//! settings merge holds, and publish atomically. A symlinked `.claude/` or
//! settings file, a file that is not a JSON object and an `env` that is not an
//! object are refused with the file untouched. Known gap (#3981): any
//! guard-exempt writer can still set the key; accepted for an Architect-only
//! project.
//! Test: `architect_guard_lift_tests.rs`, `tests/tm_fleet.rs`
//! (`init_with_guard_lift_sets_the_key_and_keeps_other_keys`).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// The env key `pm_guard` reads; `"1"` lifts the guard.
pub const LIFT_KEY: &str = "TRUSTY_MPM_PM_UNRESTRICTED";

/// The guard lift's state in one project, read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiftState {
    /// No settings file, no `env`, or no key: the guard is on.
    Off,
    /// The key is the string `"1"`: the guard is lifted.
    Lifted,
    /// The key holds another value, which the guard treats as off.
    Other(String),
    /// The file cannot be read or parsed; the state is unknown.
    Unknown(String),
}

impl LiftState {
    /// One line for a report: `off`, `lifted`, `set to …` or `unknown (…)`.
    pub fn describe(&self) -> String {
        match self {
            Self::Off => "off".to_owned(),
            Self::Lifted => "lifted".to_owned(),
            Self::Other(v) => format!("{LIFT_KEY} set to {v}, treated as off by the guard"),
            Self::Unknown(why) => format!("lift state unknown ({why})"),
        }
    }
}

/// What a [`set`] or [`remove`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiftChange {
    /// The file already said what was asked; nothing was written.
    Unchanged,
    /// The key was written or removed; `was` is a replaced non-`"1"` value.
    Written {
        /// The value [`set`] replaced, when it was not absent.
        was: Option<String>,
    },
}

/// `<project_dir>/.claude/settings.json`.
pub fn settings_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("settings.json")
}

/// `<project_dir>/.claude/settings.local.json`.
pub fn local_settings_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("settings.local.json")
}

/// The settings files (by file name) in which the key is `"1"`. Red stub.
pub fn lifted_in(_project_dir: &Path) -> Vec<&'static str> {
    Vec::new()
}

/// The guard-lift state of `project_dir`; writes nothing.
///
/// Test: `the_state_reader_covers_every_shape`.
pub fn read_state(project_dir: &Path) -> LiftState {
    match load(project_dir) {
        Ok(None) => LiftState::Off,
        Ok(Some(doc)) => state_of(&doc),
        Err(why) => LiftState::Unknown(why),
    }
}

/// The state a parsed settings object carries.
fn state_of(doc: &Map<String, Value>) -> LiftState {
    match doc.get("env") {
        None => LiftState::Off,
        Some(Value::Object(env)) => match env.get(LIFT_KEY) {
            None => LiftState::Off,
            Some(Value::String(v)) if v == "1" => LiftState::Lifted,
            Some(other) => LiftState::Other(other.to_string()),
        },
        Some(_) => LiftState::Unknown("`env` is not an object".to_owned()),
    }
}

/// Refuse a symlinked `.claude/` or settings file; `Ok` when neither is one.
fn refuse_symlinks(project_dir: &Path) -> Result<(), String> {
    for path in [project_dir.join(".claude"), settings_path(project_dir)] {
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!(
                "{} is a symlink; tm does not write through it",
                path.display()
            ));
        }
    }
    Ok(())
}

/// The settings object, `None` when the file is absent.
fn load(project_dir: &Path) -> Result<Option<Map<String, Value>>, String> {
    refuse_symlinks(project_dir)?;
    let path = settings_path(project_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };
    if raw.trim().is_empty() {
        return Ok(Some(Map::new()));
    }
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(doc)) => Ok(Some(doc)),
        Ok(_) => Err(format!("{} is not a JSON object", path.display())),
        Err(err) => Err(format!("{} is not valid JSON ({err})", path.display())),
    }
}

/// Set the key to `"1"`, keeping every other key.
///
/// Why/What: see the module doc. Creates `.claude/` and the file when absent.
/// Test: `set_creates_merges_and_reports_the_old_value`,
/// `an_unusable_file_is_refused_and_left_byte_identical`.
pub fn set(project_dir: &Path) -> Result<LiftChange, String> {
    edit(project_dir, |env| match env.get(LIFT_KEY) {
        Some(Value::String(v)) if v == "1" => None,
        old => {
            let was = old.map(Value::to_string);
            env.insert(LIFT_KEY.to_owned(), Value::String("1".to_owned()));
            Some(was)
        }
    })
}

/// Remove the key, keeping every other key; absent is [`LiftChange::Unchanged`].
///
/// Test: `remove_drops_only_the_key`.
pub fn remove(project_dir: &Path) -> Result<LiftChange, String> {
    if !settings_path(project_dir).exists() && refuse_symlinks(project_dir).is_ok() {
        return Ok(LiftChange::Unchanged);
    }
    edit(project_dir, |env| env.remove(LIFT_KEY).map(|_| None))
}

/// Apply `change` to the `env` object under the settings lock; `change`
/// returns `None` for "nothing to write", else the replaced value.
fn edit(
    project_dir: &Path,
    change: impl FnOnce(&mut Map<String, Value>) -> Option<Option<String>>,
) -> Result<LiftChange, String> {
    refuse_symlinks(project_dir)?;
    let claude = project_dir.join(".claude");
    std::fs::create_dir_all(&claude)
        .map_err(|err| format!("cannot create {}: {err}", claude.display()))?;
    let path = settings_path(project_dir);
    let outcome = crate::core::settings_lock::with_settings_lock(&path, || {
        // Re-read inside the lock: nothing can publish between it and the write.
        let mut doc = load(project_dir)?.unwrap_or_default();
        let env = match doc
            .entry("env")
            .or_insert_with(|| Value::Object(Map::new()))
        {
            Value::Object(env) => env,
            _ => return Err("`env` is not an object".to_owned()),
        };
        let Some(was) = change(env) else {
            return Ok(LiftChange::Unchanged);
        };
        crate::core::settings_lock::publish(&path, &Value::Object(doc))
            .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
        Ok(LiftChange::Written { was })
    });
    outcome.map_err(|err| format!("cannot lock {}: {err}", path.display()))?
}

#[cfg(test)]
#[path = "architect_guard_lift_tests.rs"]
mod tests;
