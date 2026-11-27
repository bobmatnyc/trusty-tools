//! The Architect's task checkers: ports of `stale-tasks.py`, `stalled_tasks.py`,
//! `task_wait_check.py` and `il_timeline.py` (#12028 P1d).
//!
//! Why: the Architect names idle dashboard tasks, stalled ledger tasks and
//! waits on answered items every pass; epic #12025 moves each script to Rust
//! once its recordings replay with zero diffs.
//! What: [`stale`] (dashboard tasks idle past `--hours`), [`stalled`] (the
//! STALLED and OWNER WAIT lines), [`wait_check`] (STALE-WAIT and UNCHECKABLE
//! lines), [`timeline`] (which task a project was on at a time). This module
//! holds what they share: [`Ledger::load`] (`datg.load_state`) and the
//! Python-faithful JSON file read. A read that fails is an error or, where
//! the script skips the file, a skip; it never reads as an empty input.
//! Test: `tests/replay_tasks.rs`, `tests/tasks_smoke.rs`.

pub mod stale;
pub mod stalled;
pub(crate) mod store_read; // #12087: gh-session-track reads the store the same way
pub mod timeline;
pub mod wait_check;

use std::path::Path;

use crate::il::store::{Where, resolve};
use crate::pyjson_load::{PyValue, loads};
use crate::pyval::{get, is_int, py_str};
use crate::spec_drift::py::read;

/// Why a ledger could not be loaded: `datg.DatgError`'s text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskError {
    /// `state.json` does not exist.
    #[error("state.json missing; run init")]
    Missing,
    /// `state.json` is unreadable, not JSON, or the wrong shape.
    #[error("state.json malformed ({0}); refusing to write")]
    Malformed(String),
    /// The state directory could not be resolved.
    #[error("{0}")]
    Resolve(String),
}

/// A failed JSON file read, as the script's `except` clauses see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JsonFail {
    /// `FileNotFoundError`.
    NotFound,
    /// Any other `OSError` or `ValueError`: `str(e)`.
    Error(String),
}

/// `json.load(open(path, encoding="utf-8"))`.
pub(crate) fn read_json(path: &Path) -> Result<PyValue, JsonFail> {
    let text = read(path).map_err(|e| match e.class.as_str() {
        "FileNotFoundError" => JsonFail::NotFound,
        _ => JsonFail::Error(e.msg),
    })?;
    loads(&text).map_err(|e| JsonFail::Error(e.py_str()))
}

/// A loaded Intent Ledger `state.json`: a dict of the shape `datg` checks.
#[derive(Debug, Clone, PartialEq)]
pub struct Ledger(pub(crate) PyValue);

/// `^[a-z][a-z0-9]{1,7}$` (Python's `$` also matches before a final `\n`).
fn code_ok(s: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let b = s.as_bytes();
    (2..=8).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

impl Ledger {
    /// Why: both ledger checkers start from `datg.load_state(project)`.
    /// What: resolves the state directory as `datg_dir` does
    /// (`$DATG_STATE_DIR`, `$DATG_SESSION`, the tmux session when per-PM
    /// directories exist) and reads `state.json`; the shape check is the
    /// script's (`code`, an int `v`, a dict `next`, lists `goals`, `items`,
    /// `tasks`, a dict `session` when present).
    /// Test: the `missing-state`, `bad-json-state`, `bad-shape-state` and
    /// `state-dir-env` recordings.
    ///
    /// # Errors
    /// [`TaskError`] with the script's text.
    pub fn load(project: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<Self, TaskError> {
        let w = Where {
            project: project.to_path_buf(),
            session: None,
            state_dir: env("DATG_STATE_DIR").map(Into::into),
            env_session: env("DATG_SESSION"),
            tmux_pane: env("TMUX_PANE"),
        };
        let paths = resolve(&w).map_err(|e| TaskError::Resolve(e.to_string()))?;
        let st = match read_json(&paths.state) {
            Ok(v) => v,
            Err(JsonFail::NotFound) => return Err(TaskError::Missing),
            Err(JsonFail::Error(e)) => return Err(TaskError::Malformed(e)),
        };
        let is_list = |k: &str| matches!(get(&st, k), Some(PyValue::List(_)));
        let ok = matches!(st, PyValue::Dict(_))
            && code_ok(&get(&st, "code").map_or(String::new(), py_str))
            && get(&st, "v").is_some_and(is_int)
            && matches!(get(&st, "next"), Some(PyValue::Dict(_)))
            && ["goals", "items", "tasks"].iter().all(|k| is_list(k))
            && matches!(get(&st, "session"), None | Some(PyValue::Dict(_)));
        if !ok {
            return Err(TaskError::Malformed("bad shape".into()));
        }
        Ok(Self(st))
    }

    /// `state["tasks"]`.
    pub(crate) fn tasks(&self) -> &[PyValue] {
        match get(&self.0, "tasks") {
            Some(PyValue::List(t)) => t,
            _ => &[],
        }
    }
}
