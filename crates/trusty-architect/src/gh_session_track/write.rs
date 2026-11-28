//! `apply_plan()` of `gh-session-track.py`: one all-or-nothing store batch (#12087).
//!
//! Why: the Sessions section reads `details.prs`, `details.issues`,
//! `details.pr_stats`, `details.gh_at` and `meta/gh_track`; a pass must
//! write all of them or none, through the crate's one store writer.
//! What: [`load_sessions`] and [`meta_version`] read the store as the frozen
//! Python reads it (`tasks::store_read`, the script's error texts). [`apply`]
//! plans against fresh store state and writes the plan with one
//! [`Store::batch`]; a [`DashError::Conflict`] (another writer moved a
//! version, the script's `ds.Conflict`) re-reads and re-plans, at most
//! [`TRIES`] times, and the last conflict is returned. Any other error ends
//! the pass at once; the batch has written nothing.
//! Test: `gh_session_track_retries_a_conflict_then_writes`,
//! `gh_session_track_gives_up_after_three_conflicts_and_writes_nothing`,
//! the write recordings in `tests/replay_gh_session_track.rs`.

use super::plan::{Fetched, Plan, Session, SessionMap, build_plan};
use super::render::build_ops;
use crate::dashboard::store::Format;
use crate::dashboard::{DashError, Store};
use crate::pyval::get;
use crate::tasks::store_read::{get_doc, list_docs};

/// `apply_plan(..., tries=3)`.
pub const TRIES: u32 = 3;

/// Why a pass wrote nothing.
#[derive(Debug)]
pub enum Fail {
    /// The store read or the batch refused: `gh-session-track: <text>`, exit 1.
    Store(String),
    /// The script raises past its `except`: its traceback's last line, exit 1.
    Raised(String),
}

impl From<DashError> for Fail {
    fn from(e: DashError) -> Self {
        Self::Store(e.to_string())
    }
}

/// `load_sessions(store)`: one session per non-empty `name`, the last document
/// of a repeated name winning, as the script's dict does.
///
/// # Errors
/// The store read's text.
pub(crate) fn load_sessions(root: &str, format: Format) -> Result<Vec<Session>, String> {
    let mut out: Vec<Session> = Vec::new();
    for d in list_docs(root, format, "sessions")? {
        // #12087: a non-string name is skipped (the script keys by any value).
        let Some(crate::pyjson_load::PyValue::Str(name)) = get(&d.data, "name") else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let s = Session {
            name: name.clone(),
            id: d.id.clone(),
            version: d.version,
            data: d.data.clone(),
        };
        match out.iter_mut().find(|x| x.name == s.name) {
            Some(slot) => *slot = s,
            None => out.push(s),
        }
    }
    Ok(out)
}

/// `meta_version(store)`: the version of `meta/gh_track`, 0 when absent.
fn meta_version(root: &str, format: Format) -> Result<u64, String> {
    Ok(get_doc(root, format, "meta", "gh_track")?.map_or(0, |d| d.version))
}

/// What one pass writes.
pub struct Pass<'a> {
    /// The repo slug (`meta/gh_track.repo`).
    pub repo: &'a str,
    /// The run's instant, `%Y-%m-%dT%H:%M:%SZ`.
    pub now: &'a str,
    /// The session mapping.
    pub map: &'a SessionMap,
}

/// `apply_plan(store, ...)`: plan against fresh state and write one batch.
///
/// Why: see the module doc.
/// What: each attempt reads the sessions, builds the plan, reads the
/// `meta/gh_track` version, builds the ops, calls `before_batch(attempt)`
/// and runs [`Store::batch`] on `store` (whose root is `root`, absolute).
/// `before_batch` is a no-op in the shipped binary; a test moves a version
/// there to stand for a concurrent writer.
/// Test: `gh_session_track_retries_a_conflict_then_writes`,
/// `gh_session_track_gives_up_after_three_conflicts_and_writes_nothing`,
/// `gh_session_track_secret_refusal_leaves_the_store_unchanged`.
pub(crate) fn apply(
    store: &Store,
    root: &str,
    fetched: &Fetched,
    pass: &Pass<'_>,
    before_batch: &mut dyn FnMut(u32),
) -> Result<Plan, Fail> {
    let format = store.format();
    let mut attempt = 0;
    loop {
        let sessions = load_sessions(root, format).map_err(Fail::Store)?;
        let plan = build_plan(fetched, &sessions, pass.now, pass.map).map_err(Fail::Raised)?;
        let version = meta_version(root, format).map_err(Fail::Store)?;
        let ops = build_ops(&plan, &sessions, pass.repo, pass.now, version).map_err(Fail::Store)?;
        before_batch(attempt);
        match store.batch(&ops) {
            Ok(_) => return Ok(plan),
            // #12087: `ds.Conflict` retries; the last one is raised.
            Err(DashError::Conflict(_)) if attempt + 1 < TRIES => attempt += 1,
            Err(e) => return Err(e.into()),
        }
    }
}
