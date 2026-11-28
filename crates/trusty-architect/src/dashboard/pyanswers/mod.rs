//! The port of `owner_answers.py` and `pending-answers.py`: the owner events the Architect must act on (#12029 P2e).
//!
//! Why: an owner answer nobody relays leaves the owner waiting; the poller
//! and the `pending-answers` command must list the same events, with the
//! same ids, as the frozen Python. The rules are `dashboard_derive.pending`,
//! ported in [`super::pyderive`]; this module is the store read around them.
//! What: [`pending_of`] runs the derive rule on a snapshot and applies the
//! ILIO split (the live Architect drops items whose `refs` hold
//! [`ILIO_REF`]; the trial, `DASHBOARD_ACTOR=`[`ILIO_ACTOR`], keeps only
//! them). [`pending`] reads [`COLLECTIONS`] from a [`PyStore`] first.
//! [`resolve_id`] and [`qualified`] are the `dashboard_qual` wrappers
//! (`qid` is `<S id>:<id>`, bare for `owner_command`). `cli` is
//! `pending-answers.py`. Reads only.
//! Test: `owner_answers_recordings_replay_clean_against_the_port`,
//! `pending_answers_recordings_replay_clean_against_the_port`.

mod cases;
pub mod cli;

use std::collections::BTreeSet;
use std::path::Path;

pub use cases::replay_cases;

use super::pyderive::{self, PySnap};
use super::pyids::snapshot_of;
use super::pyqual;
use super::store::py::{PyStore, PyStoreError, with_key};
use crate::pyjson_load::PyValue;
use crate::pyval::{get, is_str, py_str};

/// `COLLECTIONS`: what `pending` reads.
pub(crate) const COLLECTIONS: [&str; 6] = [
    "items",
    "sessions",
    "effort_tunes",
    "il_steers",
    "goals",
    "instructions",
];
/// The `refs` entry of an item the ILIO trial Architect owns.
pub const ILIO_REF: &str = "arch:ilio";
/// The `DASHBOARD_ACTOR` of the ILIO trial session.
pub const ILIO_ACTOR: &str = "architect-ilio";

/// `_ilio_ids(items)`: ids of the items whose `refs` list holds [`ILIO_REF`].
fn ilio_ids(snap: &PySnap) -> BTreeSet<&str> {
    let tagged = |d: &&crate::tasks::store_read::PyDoc| match get(&d.data, "refs") {
        Some(PyValue::List(refs)) => refs.iter().any(|r| is_str(Some(r), ILIO_REF)),
        _ => false,
    };
    pyderive::coll(snap, "items")
        .iter()
        .filter(tagged)
        .map(|d| d.id.as_str())
        .collect()
}

/// `e.get("id")` as text (`None` for an event with no id).
pub(crate) fn field(e: &PyValue, k: &str) -> Option<String> {
    get(e, k).map(py_str)
}

/// Why: the poller and the command share one rule, the derive one, and one ILIO split.
/// What: `derive.pending(snap, now)` without the ILIO items, or with only
/// them when `ilio_trial`. `now` is Unix seconds.
/// Test: `owner_answers_recordings_replay_clean_against_the_port`,
/// `poller_owner_answers_follow_the_python_pending_rule`.
pub(crate) fn pending_of(snap: &PySnap, now: f64, ilio_trial: bool) -> Vec<PyValue> {
    let ilio = ilio_ids(snap);
    pyderive::pending::pending(snap, now)
        .into_iter()
        .filter(|e| field(e, "id").is_some_and(|id| ilio.contains(id.as_str())) == ilio_trial)
        .collect()
}

/// Why: `owner_answers.pending(root, now)`, the reader `pending-answers.py` uses.
/// What: snapshots [`COLLECTIONS`] and runs [`pending_of`].
/// Test: `owner_answers_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// The store's: a corrupt document, the other format, an `OSError`.
pub(crate) fn pending(
    store: &PyStore,
    now: f64,
    ilio_trial: bool,
) -> Result<Vec<PyValue>, PyStoreError> {
    Ok(pending_of(
        &snapshot_of(store, &COLLECTIONS)?,
        now,
        ilio_trial,
    ))
}

/// `resolve_id(root, ref)`: the local `items` id of a bare or qualified ref.
///
/// # Errors
/// [`PyStoreError::NotFound`] for another store's S id; the store's.
pub(crate) fn resolve_id(
    store: &PyStore,
    r: &str,
    default_root: &Path,
) -> Result<String, PyStoreError> {
    pyqual::resolve(store, "items", r, default_root)
}

/// `qualified(root, entries)`: each entry with a `qid` key last (`id` bare
/// for `owner_command`, else qualified with the store's S id).
///
/// # Errors
/// An `OSError` reading `meta/state`.
pub(crate) fn qualified(
    store: &PyStore,
    entries: Vec<PyValue>,
    default_root: &Path,
) -> Result<Vec<PyValue>, PyStoreError> {
    let sid = pyqual::store_sid(store, default_root)?;
    Ok(entries
        .into_iter()
        .map(|e| {
            let id = field(&e, "id").unwrap_or_default();
            let qid = if is_str(get(&e, "kind"), "owner_command") {
                id
            } else {
                pyqual::qualify(sid.as_deref(), &id)
            };
            match &e {
                PyValue::Dict(p) => PyValue::Dict(with_key(p, "qid", PyValue::Str(qid))),
                _ => e,
            }
        })
        .collect())
}
