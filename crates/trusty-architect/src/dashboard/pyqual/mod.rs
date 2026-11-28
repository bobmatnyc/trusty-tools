//! The port of `dashboard_qual.py`: fleet-wide qualified ids and id aliases (#12029 P2d).
//!
//! Why: two stores (the Architect's and a session's) both hold bare
//! `D01`-style ids, so a reference shown beside another session's entries
//! must name the store that owns it, `<S id>:<id>`, and a retired id prefix
//! must still resolve. Both are derived on read, never stored.
//! What: [`split`] (`^(S[0-9a-km-np-z]{2}):([A-Za-z0-9_.-]+)$`), [`qualify`],
//! [`store_sid`] (`meta/state.session_id`, else `S14` for the default root,
//! else none), [`alias_rules`] (`meta/id-aliases.json` prefix rules),
//! [`alias_target`] (`A3e` to `B3e` when a rule matches and `A3e` has no
//! document), [`resolve`] (a qualified ref with another store's S id is
//! `NotFound` naming the owner) and [`with_qualifier`] (a `qualified` key on
//! `items` results). Reads only.
//! Test: `dashboard_qual_recordings_replay_clean_against_the_port`.

mod cases;

use std::path::{Path, PathBuf};

pub use cases::replay_cases;

use super::store::py::{PyStore, PyStoreError, with_key};
use crate::ids36::DIGITS;
use crate::pyjson_load::PyValue;
use crate::pyval::{get, py_str};

/// The Architect store's session (`ARCHITECT_SID`).
pub(crate) const ARCHITECT_SID: &str = "S14";

/// Python's `$`: the text before one final `\n`.
fn body(s: &str) -> &str {
    s.strip_suffix('\n').unwrap_or(s)
}

fn is_sid(s: &str) -> bool {
    let mut c = s.chars();
    c.next() == Some('S')
        && c.next().is_some_and(|d| DIGITS.contains(d))
        && c.next().is_some_and(|d| DIGITS.contains(d))
        && c.next().is_none()
}

/// `QUAL_RE.match(ref)`: the S id and the local id.
fn qual_match(r: &str) -> Option<(&str, &str)> {
    let (sid, local) = body(r).split_once(':')?;
    let local_ok = !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
    (is_sid(sid) && local_ok).then_some((sid, local))
}

/// `split(ref)`: `(S id, local id)` for a qualified ref, else `(None, ref)`.
pub(crate) fn split(r: &str) -> (Option<String>, String) {
    match qual_match(r) {
        Some((sid, local)) => (Some(sid.into()), local.into()),
        None => (None, r.into()),
    }
}

/// `qualify(sid, local_id)`: `"<S id>:<id>"`, or the bare id when the S id is unknown.
pub(crate) fn qualify(sid: Option<&str>, local: &str) -> String {
    match sid.filter(|s| !s.is_empty()) {
        Some(s) => format!("{s}:{local}"),
        None => local.to_owned(),
    }
}

/// `os.path.realpath`: symlinks resolved as far as the path exists.
fn realpath(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => realpath(parent).join(name),
        _ => p.to_path_buf(),
    }
}

/// `store.get(coll, id)["data"]`, with a `StoreError` (not an `OSError`) as `None`.
fn data_of(store: &PyStore, coll: &str, id: &str) -> Result<Option<PyValue>, PyStoreError> {
    match store.get(&PyValue::Str(coll.into()), &PyValue::Str(id.into())) {
        Ok(d) => Ok(Some(d.data)),
        Err(e) if e.http().is_some() => Ok(None),
        Err(e) => Err(e),
    }
}

/// Why: the qualifier is the owning store's session, not the item's `session` field.
/// What: `store_sid(store)`: `meta/state.session_id` when it is an S id; else
/// `S14` when the store's root is `default_root` (after `realpath`); else `None`.
/// Test: `dashboard_qual_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// An `OSError` reading `meta/state`.
pub(crate) fn store_sid(
    store: &PyStore,
    default_root: &Path,
) -> Result<Option<String>, PyStoreError> {
    if let Some(data) = data_of(store, "meta", "state")?
        && let Some(PyValue::Str(sid)) = get(&data, "session_id")
        && qual_match(&format!("{sid}:x")).is_some()
    {
        return Ok(Some(sid.clone()));
    }
    let own = realpath(Path::new(&store.root)) == realpath(default_root);
    Ok(own.then(|| ARCHITECT_SID.to_owned()))
}

/// `alias_rules(store)`: `[(from, to)]` single capital letters from `meta/id-aliases`.
pub(crate) fn alias_rules(store: &PyStore) -> Result<Vec<(String, String)>, PyStoreError> {
    let Some(data) = data_of(store, "meta", "id-aliases")? else {
        return Ok(Vec::new());
    };
    let capital = |v: Option<&PyValue>| {
        let s = v.map_or_else(|| "None".to_owned(), py_str);
        (s.len() == 1 && s.chars().all(|c| c.is_ascii_uppercase())).then_some(s)
    };
    let mut out = Vec::new();
    if let Some(PyValue::List(rules)) = get(&data, "rules") {
        for r in rules.iter().filter(|r| matches!(r, PyValue::Dict(_))) {
            if let (Some(f), Some(t)) = (capital(get(r, "from")), capital(get(r, "to"))) {
                out.push((f, t));
            }
        }
    }
    Ok(out)
}

/// `ALIAS_ID_RE.match(local)`: the prefix letter and the two digits.
fn alias_id(local: &str) -> Option<(char, &str)> {
    let b = body(local);
    let mut c = b.chars();
    let p = c.next().filter(char::is_ascii_uppercase)?;
    let digits = c.as_str();
    (digits.chars().count() == 2 && digits.chars().all(|d| DIGITS.contains(d)))
        .then_some((p, digits))
}

/// Why: a retired prefix's ids must still resolve until the migration removes them.
/// What: `alias_target(store, coll, local)`: for the first rule whose `from`
/// is `local`'s prefix, `local` when it has a document, else `to` + digits;
/// `local` when no rule matches.
/// Test: `dashboard_qual_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// A `StoreError` other than `NotFound` (a bad collection name), an `OSError`.
pub(crate) fn alias_target(
    store: &PyStore,
    coll: &str,
    local: &str,
) -> Result<String, PyStoreError> {
    let Some((prefix, digits)) = alias_id(local) else {
        return Ok(local.to_owned());
    };
    for (from, to) in alias_rules(store)? {
        if from.starts_with(prefix) {
            return match store.get(&PyValue::Str(coll.into()), &PyValue::Str(local.into())) {
                Ok(_) => Ok(local.to_owned()),
                Err(PyStoreError::NotFound(_)) => Ok(format!("{to}{digits}")),
                Err(e) => Err(e),
            };
        }
    }
    Ok(local.to_owned())
}

/// Why: a qualified ref naming another store must fail with the owner's name.
/// What: `resolve(store, coll, ref)`: the local id, through [`alias_target`].
/// Test: `dashboard_qual_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// [`PyStoreError::NotFound`] for another store's S id; [`alias_target`]'s.
pub(crate) fn resolve(
    store: &PyStore,
    coll: &str,
    r: &str,
    default_root: &Path,
) -> Result<String, PyStoreError> {
    let (sid, local) = split(r);
    if let Some(sid) = sid
        && let Some(own) = store_sid(store, default_root)?.filter(|own| *own != sid)
    {
        return Err(PyStoreError::NotFound(format!(
            "{coll}/{r} not found: this store belongs to {own}; use {own}:{local}"
        )));
    }
    alias_target(store, coll, &local)
}

/// `with_qualifier(store, coll, docs)`: `docs` (one dict or a list) with a
/// derived `qualified` key on each, for `items` only.
pub(crate) fn with_qualifier(
    store: &PyStore,
    coll: &str,
    docs: PyValue,
    default_root: &Path,
) -> Result<PyValue, PyStoreError> {
    if coll != "items" {
        return Ok(docs);
    }
    let sid = store_sid(store, default_root)?;
    let one = |d: &PyValue| match d {
        PyValue::Dict(p) => {
            let id = get(d, "id").map_or_else(String::new, py_str);
            PyValue::Dict(with_key(
                p,
                "qualified",
                PyValue::Str(qualify(sid.as_deref(), &id)),
            ))
        }
        other => other.clone(),
    };
    Ok(match &docs {
        PyValue::List(l) => PyValue::List(l.iter().map(one).collect()),
        d => one(d),
    })
}
