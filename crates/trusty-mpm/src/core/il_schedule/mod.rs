//! The Architect IL schedule, `local/il-schedule.json`, and its one writer (#11001).
//!
//! Why: ADR-0077 D2/D3. The schedule ships a preset embedded in the binary,
//! and `tm il config` is the only writer of the file. The per-session entries
//! had no example and no verb, so users edited the file by hand.
//! What: [`PRESET`] (the same bytes `tm fleet init` seeds), the typed
//! [`Schedule`] and [`Session`] shapes (unknown keys are kept, so a later
//! `schema_version` (#11002) or a hand-added key survives an edit), [`load`]
//! for readers, [`update`] for the locked read-modify-write, and [`reset`].
//! The edits live in `edit`, the field table in `fields`, the generated docs
//! in `docs`.
//! Test: `core::il_schedule::tests`, `tests/generated_docs.rs`.

mod docs;
mod edit;
mod fields;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use docs::{example_commands, render_docs};
pub use edit::{Outcome, resolve_project};
pub use fields::{SESSION_KEYS, THRESHOLDS};

/// The preset: the bytes `tm il config reset` writes and `tm fleet init` seeds.
pub const PRESET: &str = include_str!("../../assets/architect/templates/local/il-schedule.json");

/// The schedule's path below the Architect directory.
pub const REL_PATH: &str = "local/il-schedule.json";

/// `<dir>/local/il-schedule.json`.
pub fn path(dir: &Path) -> PathBuf {
    dir.join(REL_PATH)
}

/// A failure to read, validate or write the schedule.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IlScheduleError {
    /// The file exists but could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The schedule file.
        path: PathBuf,
        /// The OS error.
        source: std::io::Error,
    },
    /// The file is not a schedule this binary can edit.
    #[error(
        "{} is not a valid IL schedule ({source}); fix it by hand, or run \
         `tm il config reset --force` to restore the preset",
        path.display()
    )]
    Parse {
        /// The schedule file.
        path: PathBuf,
        /// The parse error.
        source: serde_json::Error,
    },
    /// The lock, the encode or the atomic write failed; the file is unchanged.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// The schedule file.
        path: PathBuf,
        /// The OS error.
        source: std::io::Error,
    },
    /// `reset` over a file that differs from the preset, without `--force`.
    #[error(
        "{} differs from the preset ({sessions} enrolled session(s)); \
         `tm il config reset --force` replaces it",
        path.display()
    )]
    Edited {
        /// The schedule file.
        path: PathBuf,
        /// How many sessions the reset would drop.
        sessions: usize,
    },
    /// A session name that is not enrolled.
    #[error("session {0} is not enrolled; `tm il config enroll {0} --project <dir>` adds it")]
    UnknownSession(String),
    /// `enroll` of a name already in `sessions`.
    #[error(
        "session {name} is already enrolled for {project}; change it with \
         `tm il config set --session {name} <key> <value>`"
    )]
    AlreadyEnrolled {
        /// The session name.
        name: String,
        /// Its recorded project directory.
        project: String,
    },
    /// A `set` key this binary does not edit.
    #[error("unknown key {key}; {hint}")]
    UnknownKey {
        /// The key as given.
        key: String,
        /// The keys accepted in this position.
        hint: String,
    },
    /// A value that fails the key's validation.
    #[error("invalid value for {key}: {reason}")]
    BadValue {
        /// The key.
        key: String,
        /// Why the value is refused.
        reason: String,
    },
}

/// The whole schedule file.
///
/// Why: a typed shape validates what the CLI writes; `extra` keeps every key
/// this binary does not know, so an edit never drops one.
/// What: the six keys of the preset plus `extra`, serialized in that order.
/// Test: `an_edit_keeps_unknown_keys_and_the_jobs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    /// Cadence jobs `il-cron.py` runs; opaque to the CLI.
    #[serde(default)]
    pub jobs: Vec<Value>,
    /// Enrolled sessions.
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// Global clear thresholds; a session's own `thresholds` override them.
    #[serde(default)]
    pub thresholds: Map<String, Value>,
    /// Projects whose sessions are never cleared.
    #[serde(default)]
    pub do_not_clear: Vec<Value>,
    /// Sessions on the pause list.
    #[serde(default)]
    pub paused_sessions: Vec<Value>,
    /// The documented session example; no reader acts on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_example: Option<Session>,
    /// Keys this binary does not know, kept verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One enrolled session, the entry `ctx-watch.py` and `il-cron.py` read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// The tmux session name.
    pub name: String,
    /// The project directory holding `.trusty-mpm/datg/`.
    pub project: String,
    /// The IL id code (2-8 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The clear signals evaluated; absent means S1-S5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<Vec<String>>,
    /// Per-session threshold overrides.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub thresholds: Map<String, Value>,
    /// An explicit IL `state.json` path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_path: Option<String>,
    /// Keys this binary does not know, kept verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Schedule {
    /// The parsed preset.
    pub fn preset() -> Self {
        // #11001: PRESET is compiled in; `the_preset_parses_and_has_one_example` proves it.
        serde_json::from_str(PRESET).expect("the embedded il-schedule preset parses")
    }

    /// The enrolled session named `name`.
    pub fn session_mut(&mut self, name: &str) -> Result<&mut Session, IlScheduleError> {
        self.sessions
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| IlScheduleError::UnknownSession(name.to_owned()))
    }

    /// The file text this schedule is written as: pretty JSON and a newline.
    pub fn to_text(&self) -> Result<String, serde_json::Error> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }
}

/// What [`load`] found.
#[derive(Debug)]
pub struct Loaded {
    /// The parsed schedule.
    pub schedule: Schedule,
    /// The exact text: the file's, or [`PRESET`] when the file is absent.
    pub text: String,
    /// Whether the text came from the file.
    pub from_file: bool,
}

/// Read and parse the schedule; an absent file is the preset.
///
/// Why: `show` prints the schedule the readers act on; a malformed file must
/// be an error, never shown as the preset.
/// What: the file's text when it exists, else [`PRESET`]; parsed either way.
/// Test: `show_of_an_absent_file_is_the_preset`, `a_malformed_file_is_refused_and_left_unchanged`.
pub fn load(path: &Path) -> Result<Loaded, IlScheduleError> {
    let (text, from_file) = match std::fs::read_to_string(path) {
        Ok(text) => (text, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (PRESET.to_owned(), false),
        Err(source) => {
            return Err(IlScheduleError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let schedule = serde_json::from_str(&text).map_err(|source| IlScheduleError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Loaded {
        schedule,
        text,
        from_file,
    })
}

/// Lock the schedule, re-read it, apply `f`, and publish only a change.
///
/// Why: ADR-0077 D3 — the CLI is the only writer, and two CLI runs may race.
/// A refused edit or a failed write must leave the file byte-identical and
/// be an error (Fail-Open).
/// What: under `trusty_common::file_lock` on the file, [`load`] (absent file
/// = preset), then `f`. An `Err` from `f` writes nothing. `Ok` with
/// `changed` publishes the new text by `atomic_file::write_atomic`; any
/// write failure is returned as [`IlScheduleError::Write`].
/// Test: `enroll_then_set_then_remove_round_trips`,
/// `a_refused_edit_leaves_the_file_byte_identical`,
/// `a_write_failure_is_an_error_and_the_file_is_unchanged`.
pub fn update(
    path: &Path,
    f: impl FnOnce(&mut Schedule) -> Result<Outcome, IlScheduleError>,
) -> Result<Outcome, IlScheduleError> {
    let write_err = |source| IlScheduleError::Write {
        path: path.to_path_buf(),
        source,
    };
    locked(path, || {
        let mut schedule = load(path)?.schedule;
        let outcome = f(&mut schedule)?;
        if outcome.changed {
            let text = schedule
                .to_text()
                .map_err(|e| write_err(std::io::Error::other(e)))?;
            trusty_common::atomic_file::write_atomic(path, text.as_bytes()).map_err(write_err)?;
        }
        Ok(outcome)
    })
}

/// Restore [`PRESET`]; a file that differs needs `force`.
///
/// Why: issue #11001 AC3. `reset` drops every enrolled session, so an edited
/// file is replaced only on request. It writes the preset bytes verbatim
/// and does not parse the old file, so it also repairs a malformed one.
/// What: under the lock: absent file or `force` writes the preset; a file
/// equal to the preset is unchanged; any other file is
/// [`IlScheduleError::Edited`].
/// Test: `reset_refuses_an_edited_file_without_force`, `reset_force_restores_the_preset_bytes`.
pub fn reset(path: &Path, force: bool) -> Result<Outcome, IlScheduleError> {
    locked(path, || {
        let current = match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(IlScheduleError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if current.as_deref() == Some(PRESET) {
            return Ok(Outcome::unchanged("unchanged: the schedule is the preset"));
        }
        if let (Some(text), false) = (&current, force) {
            let sessions = serde_json::from_str::<Schedule>(text).map_or(0, |s| s.sessions.len());
            return Err(IlScheduleError::Edited {
                path: path.to_path_buf(),
                sessions,
            });
        }
        trusty_common::atomic_file::write_atomic(path, PRESET.as_bytes()).map_err(|source| {
            IlScheduleError::Write {
                path: path.to_path_buf(),
                source,
            }
        })?;
        Ok(Outcome::changed("reset: the schedule is the preset"))
    })
}

/// Run `f` under the schedule's exclusive lock, creating `local/` first.
fn locked<R>(
    path: &Path,
    f: impl FnOnce() -> Result<R, IlScheduleError>,
) -> Result<R, IlScheduleError> {
    let write_err = |source| IlScheduleError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(write_err)?;
    }
    trusty_common::file_lock::with_exclusive_lock(path, f).map_err(write_err)?
}

#[cfg(test)]
mod tests;
