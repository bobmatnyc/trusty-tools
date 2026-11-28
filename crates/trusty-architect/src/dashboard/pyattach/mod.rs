//! The port of `dashboard_attach.py`: owner file uploads stored as Intent Ledger attachments (#12029 P2h).
//!
//! Why: the owner answers a dashboard ask with a file; it must land beside
//! the owning project's Intent Ledger and be recorded on the item. During
//! the overlap the frozen server and the port share the store and the
//! attachment tree, so the port must choose the same directory and name,
//! refuse the same uploads and record the same entry.
//! What: [`handle_upload`] (the item id, name, status, kind and count
//! checks, [`dirs::attach_dir`], [`save::save`], then [`record`], removing
//! the file when the record fails), [`record`] (the append pinned with
//! `if_version`, five tries on a conflict, at most [`MAX_PER_ITEM`]) and
//! [`serve_file`] (a recorded attachment read back by its stored name, never
//! a path from the URL). Library only: the dashboard server that calls it is
//! not ported yet.
//! Known gaps: a `DASH_ATTACH_MAX_MB` past `u64` bytes saturates where the
//! script's `int` is unbounded (and `inf` raises `OverflowError` there).
//! Test: `dashboard_attach_recordings_replay_clean_against_the_port`, the
//! error-arm tests in `tests.rs`.

pub mod cases;
pub mod dirs;
pub mod names;
mod nfc;
mod nfc_table;
pub mod save;
#[cfg(test)]
mod tests;

use std::io::Read;
use std::path::Path;

use crate::dashboard::pyupdates::ItemStore;
use crate::dashboard::store::py::PyStoreError;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pyval::{get, truthy};
pub use names::{MAX_NAME, clean_name, content_type, item_id_ok, max_bytes};

/// `MAX_PER_ITEM`.
pub const MAX_PER_ITEM: usize = 20;

/// An exception the module raises.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttachError {
    /// A store exception (`StoreError` and its subclasses, an `OSError`).
    #[error("{0}")]
    Store(PyStoreError),
    /// `TooLarge` (413 `too_large`).
    #[error("{0}")]
    TooLarge(String),
    /// Any other exception: `<Class>: <message>`, e.g. a `TypeError`.
    #[error("{0}")]
    Raise(String),
}

impl From<PyStoreError> for AttachError {
    fn from(e: PyStoreError) -> Self {
        Self::Store(e)
    }
}

impl AttachError {
    /// `(status, code)` of a `StoreError` subclass.
    pub fn http(&self) -> Option<(u16, &'static str)> {
        match self {
            Self::Store(e) => e.http(),
            Self::TooLarge(_) => Some((413, "too_large")),
            Self::Raise(_) => None,
        }
    }

    /// `type(e).__name__`.
    pub fn class(&self) -> &str {
        match self {
            Self::Store(e) => e.class(),
            Self::TooLarge(_) => "TooLarge",
            Self::Raise(l) => l.split(':').next().unwrap_or(""),
        }
    }
}

fn refuse<T>(msg: String) -> Result<T, AttachError> {
    Err(AttachError::Store(PyStoreError::Store(msg)))
}

/// `len(v or [])` for the `attachments` field; a value with no length raises.
fn count(v: Option<&PyValue>) -> Result<usize, AttachError> {
    match v.filter(|v| truthy(v)) {
        None => Ok(0),
        Some(PyValue::List(l)) => Ok(l.len()),
        Some(PyValue::Dict(p)) => Ok(p.len()),
        Some(PyValue::Str(s)) => Ok(s.chars().count()),
        Some(other) => Err(AttachError::Raise(format!(
            "TypeError: object of type '{}' has no len()",
            crate::pyval::type_name(other)
        ))),
    }
}

/// Why: two uploads on one item must both be recorded, never one lost.
/// What: `record(store, item_id, entry, actor)`: read the item, refuse a
/// 21st attachment, append `entry` to a list copy of `attachments` (any
/// other value counts as empty) and update with the version just read; on
/// a conflict read again, five tries in all. The answer is the new version.
/// Test: `record_retries_five_conflicts_then_gives_up`,
/// `a_21st_attachment_is_refused_and_nothing_is_written`.
///
/// # Errors
/// The cap refusal, `Conflict("item changed repeatedly; retry the upload")`
/// after five conflicts, or the store's exception.
pub(crate) fn record<S: ItemStore + ?Sized>(
    store: &S,
    item_id: &str,
    entry: &PyValue,
    actor: Option<&str>,
) -> Result<PyInt, AttachError> {
    for _ in 0..5 {
        let doc = store.get_item(item_id)?;
        let mut cur = match get(&doc.data, "attachments") {
            Some(PyValue::List(l)) => l.clone(),
            _ => Vec::new(),
        };
        if cur.len() >= MAX_PER_ITEM {
            return refuse(format!("at most {MAX_PER_ITEM} attachments per item"));
        }
        cur.push(entry.clone());
        let body = PyValue::Dict(vec![("attachments".into(), PyValue::List(cur))]);
        match store.update_item(item_id, &body, &doc.version, actor) {
            Err(PyStoreError::Conflict(_)) => {}
            r => return Ok(r?),
        }
    }
    Err(AttachError::Store(PyStoreError::Conflict(
        "item changed repeatedly; retry the upload".into(),
    )))
}

/// One upload request: `POST /api/attach/<item id>?name=` and its context.
pub struct Upload<'a> {
    /// The item id from the URL.
    pub item_id: &'a str,
    /// The `name` query value; `None` when absent or not a string.
    pub raw_name: Option<&'a str>,
    /// The request body.
    pub body: &'a mut dyn Read,
    /// `Content-Length`.
    pub length: i64,
    /// `il-schedule.json`.
    pub schedule: &'a Path,
    /// The Architect's IL directory.
    pub architect_il: &'a str,
    /// A session-mode server's store root.
    pub store_root: Option<&'a str>,
    /// The writing actor.
    pub actor: Option<&'a str>,
    /// The environment (`DASH_ATTACH_MAX_MB`).
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// The working directory relative paths resolve against.
    pub cwd: &'a Path,
}

fn s(v: &str) -> PyValue {
    PyValue::Str(v.to_owned())
}

/// Why: the whole upload, so the server has one call to make.
/// What: `handle_upload(...)`: the checks in the script's order, then save,
/// then record; a failed record removes the saved file and re-raises. The
/// answer is `{attachment: entry, version}`; `entry` is `{name,
/// stored_name, size, sha256, uploaded_at, path, il, type}`.
/// Test: `dashboard_attach_recordings_replay_clean_against_the_port`,
/// `a_failed_record_removes_the_saved_file`.
///
/// # Errors
/// Every refusal of the script, in its words, or the store's exception.
pub(crate) fn handle_upload<S: ItemStore + ?Sized>(
    store: &S,
    u: Upload<'_>,
) -> Result<PyValue, AttachError> {
    if !item_id_ok(u.item_id) {
        return refuse("bad item id".into());
    }
    let name = clean_name(u.raw_name)?;
    let item = store.get_item(u.item_id)?.data;
    let is = |k: &str, any: &[&str]| matches!(get(&item, k), Some(PyValue::Str(v)) if any.contains(&v.as_str()));
    if !is("status", &["open", "answered"]) {
        return refuse("uploads are accepted on open or answered items only".into());
    }
    if !is("kind", &["question", "action", "blocker"]) {
        return refuse("uploads are accepted on decisions and blockers only".into());
    }
    if count(get(&item, "attachments"))? >= MAX_PER_ITEM {
        return refuse(format!("at most {MAX_PER_ITEM} attachments per item"));
    }
    // `item.get("session") or ""`: a value that is not a string names no entry.
    let session = match get(&item, "session").filter(|v| truthy(v)) {
        Some(PyValue::Str(v)) => Some(v.as_str()),
        Some(_) => None,
        None => Some(""),
    };
    let (directory, il) =
        dirs::attach_dir(session, u.schedule, u.architect_il, u.store_root, u.cwd);
    let saved = save::save(
        u.body,
        u.length,
        &name,
        &directory,
        u.item_id,
        max_bytes(u.env),
    )?;
    let size = PyValue::Int(PyInt::Small(i64::try_from(saved.size).unwrap_or(i64::MAX)));
    let entry = PyValue::Dict(vec![
        ("name".into(), s(&name)),
        ("stored_name".into(), s(&saved.stored)),
        ("size".into(), size),
        ("sha256".into(), s(&saved.sha256)),
        ("uploaded_at".into(), s(&store.now_iso())),
        ("path".into(), s(&saved.path)),
        ("il".into(), s(il)),
        ("type".into(), s(content_type(&saved.stored))),
    ]);
    match record(store, u.item_id, &entry, u.actor) {
        Ok(version) => Ok(PyValue::Dict(vec![
            ("attachment".into(), entry),
            ("version".into(), PyValue::Int(version)),
        ])),
        Err(e) => {
            let _ = std::fs::remove_file(&saved.path);
            Err(e)
        }
    }
}

/// One served attachment: `(bytes, content type, inline?)`.
pub type Served = (Vec<u8>, &'static str, bool);

fn no_such() -> AttachError {
    AttachError::Store(PyStoreError::NotFound("no such attachment".into()))
}

/// Why: `GET /api/attach/<item id>/<stored_name>` must serve only a file the item recorded.
/// What: `serve_file(store, item_id, stored_name)`: the first attachment
/// whose `stored_name` matches and whose `path` is a string; its path must
/// end `attachments/<item id>/<stored_name>`, be a regular file and no
/// link, resolve inside its directory, and hold at most [`max_bytes`].
/// Raster images are inline.
/// Test: `dashboard_attach_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// `NotFound("no such attachment")` for every refusal, or the store's exception.
pub(crate) fn serve_file<S: ItemStore + ?Sized>(
    store: &S,
    item_id: &str,
    stored_name: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Served, AttachError> {
    use crate::dashboard::store::py::dirname;
    if !item_id_ok(item_id) {
        return Err(no_such());
    }
    let data = store.get_item(item_id)?.data;
    let list = match get(&data, "attachments").filter(|v| truthy(v)) {
        Some(PyValue::List(l)) => l.clone(),
        Some(PyValue::Str(_) | PyValue::Dict(_)) | None => Vec::new(),
        Some(other) => {
            let t = crate::pyval::type_name(other);
            return Err(AttachError::Raise(format!(
                "TypeError: '{t}' object is not iterable"
            )));
        }
    };
    let base = |p: &str| p.rsplit('/').next().unwrap_or("").to_owned();
    for a in &list {
        let (Some(PyValue::Str(sn)), Some(PyValue::Str(path))) =
            (get(a, "stored_name"), get(a, "path"))
        else {
            continue;
        };
        if sn != stored_name {
            continue;
        }
        let parent = dirname(path);
        let meta = std::fs::symlink_metadata(path);
        let plain = meta.as_ref().is_ok_and(|m| m.file_type().is_file());
        let inside = plain
            && dirs::realpath(path).ok()
                == dirs::realpath(parent)
                    .ok()
                    .map(|r| format!("{r}/{stored_name}"));
        if base(path) != stored_name
            || base(parent) != item_id
            || base(dirname(parent)) != "attachments"
            || !inside
        {
            break;
        }
        let limit = max_bytes(env);
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(limit.saturating_add(1)).read_to_end(&mut bytes))
            .map_err(|e| AttachError::Store(PyStoreError::os(path, &e)))?;
        if bytes.len() as u64 > limit {
            break;
        }
        let ext = names::splitext(stored_name).1.to_lowercase();
        return Ok((
            bytes,
            content_type(stored_name),
            names::inline_type(&ext).is_some(),
        ));
    }
    Err(no_such())
}
