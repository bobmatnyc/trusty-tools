//! Batch ops: staged in memory, then committed all-or-nothing.
//!
//! Why: a batch must be all-or-nothing, a stale `if_version` must write
//! nothing, and `version` belongs to the store (DOC-82 LDG-2.8.1, #10063
//! AC 3). Staging every op before the first byte is written gives the first
//! two; restoring earlier files when a later write fails gives the rest.
//! What: [`Op`] (`set`, `update`, `delete`, with `if_version`, 0 meaning
//! "must not exist") and [`OpResult`]. [`Store::stage`] reads each target
//! once under the lock, checks names, format, `if_version` and the body
//! (an object without `version`; minute timestamps under time keys gain
//! `:00`), and chains later ops on one document. [`Store::commit`] writes
//! every dirty target (`dir`: one file each; `jsonl`: each touched
//! collection re-rendered whole, removed when empty) and, if any write
//! fails, restores the bytes of every earlier one.
//! Test: `stale_if_version_conflicts_and_leaves_bytes`,
//! `batch_rolls_back_on_midway_failure`, `body_with_version_refused`,
//! `dashboard_writer_matches_prototype_bytes`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use super::super::error::DashError;
use super::super::ids::check_name;
use super::jsonl::{self, Docs};
use super::{Format, Store, Versioned, dir};
use crate::ledger::store::{sync_dir, write_atomic};
use crate::pystr::py_repr;

/// One write in a batch.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Replace (or create) a document's body.
    Set {
        /// Collection name.
        collection: String,
        /// Doc id.
        id: String,
        /// The new body: a JSON object without `version`.
        data: Value,
        /// Expected current version; 0 means "must not exist".
        if_version: Option<u64>,
    },
    /// Merge keys into an existing document's body.
    Update {
        /// Collection name.
        collection: String,
        /// Doc id.
        id: String,
        /// The keys to set: a JSON object without `version`.
        data: Value,
        /// Expected current version.
        if_version: Option<u64>,
    },
    /// Remove an existing document.
    Delete {
        /// Collection name.
        collection: String,
        /// Doc id.
        id: String,
        /// Expected current version.
        if_version: Option<u64>,
    },
}

impl Op {
    /// An op from the prototype's JSON shape `{op, collection, id, data, if_version}`.
    ///
    /// # Errors
    /// [`DashError::Invalid`] for an unknown op, a missing name or a
    /// non-integer `if_version`, in the prototype's words.
    pub fn from_json(v: &Value) -> Result<Self, DashError> {
        let obj = v
            .as_object()
            .ok_or_else(|| DashError::Invalid("each batch op must be an object".into()))?;
        let name = |key: &str| obj.get(key).and_then(Value::as_str).map(str::to_owned);
        let if_version = match obj.get("if_version") {
            None | Some(Value::Null) => None,
            Some(n) => Some(
                n.as_u64()
                    .ok_or_else(|| DashError::Invalid("if_version must be an integer".into()))?,
            ),
        };
        let kind = obj.get("op").and_then(Value::as_str);
        let (Some(collection), Some(id)) = (name("collection"), name("id")) else {
            return Err(DashError::Invalid(match kind {
                Some("set" | "update" | "delete") => {
                    "each op needs a string collection and id".into()
                }
                _ => unknown_op(obj.get("op")),
            }));
        };
        let data = obj.get("data").cloned().unwrap_or(Value::Null);
        match kind {
            Some("set") => Ok(Self::Set {
                collection,
                id,
                data,
                if_version,
            }),
            Some("update") => Ok(Self::Update {
                collection,
                id,
                data,
                if_version,
            }),
            Some("delete") => Ok(Self::Delete {
                collection,
                id,
                if_version,
            }),
            _ => Err(DashError::Invalid(unknown_op(obj.get("op")))),
        }
    }

    fn parts(&self) -> (&str, &str, Option<u64>) {
        match self {
            Self::Set {
                collection,
                id,
                if_version,
                ..
            }
            | Self::Update {
                collection,
                id,
                if_version,
                ..
            }
            | Self::Delete {
                collection,
                id,
                if_version,
            } => (collection, id, *if_version),
        }
    }
}

fn unknown_op(op: Option<&Value>) -> String {
    let shown = match op {
        Some(Value::String(s)) => py_repr(s),
        _ => "None".into(),
    };
    format!("unknown op {shown}: use set, update or delete")
}

/// What one op did, in the prototype's JSON shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum OpResult {
    /// A set or update; `version` is the new version.
    Written {
        /// Collection name.
        collection: String,
        /// Doc id.
        id: String,
        /// The version written.
        version: u64,
    },
    /// A delete; `deleted` is always true.
    Deleted {
        /// Collection name.
        collection: String,
        /// Doc id.
        id: String,
        /// Always true.
        deleted: bool,
    },
}

/// One document's state as the batch goes.
pub(super) struct Slot {
    pub(super) coll: String,
    pub(super) id: String,
    pub(super) exists: bool,
    pub(super) version: u64,
    pub(super) data: Map<String, Value>,
    pub(super) dirty: bool,
    pub(super) old: Option<Versioned>,
}

/// Every slot a batch touched, in first-touch order, plus the `jsonl`
/// collections it read under the lock.
#[derive(Default)]
pub(super) struct Staged {
    pub(super) slots: Vec<Slot>,
    index: HashMap<(String, String), usize>,
    fresh: BTreeMap<String, Docs>,
}

const TS_KEYS: [&str; 8] = [
    "created", "updated", "due", "at", "until", "eta", "eta_low", "eta_high",
];

/// `value` with each minute-precision timestamp (`2026-10-12T03:46Z`) under a
/// time key (or a key ending `_at`) given seconds; free text is never touched.
pub fn normalize_timestamps(value: Value, key: Option<&str>) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let v = normalize_timestamps(v, Some(&k));
                    (k, v)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| normalize_timestamps(v, key))
                .collect(),
        ),
        Value::String(s)
            if key.is_some_and(|k| TS_KEYS.contains(&k) || k.ends_with("_at"))
                && is_minute_ts(&s) =>
        {
            Value::String(format!("{}:00Z", &s[..s.len() - 1]))
        }
        other => other,
    }
}

/// `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}Z$`, ASCII digits.
fn is_minute_ts(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 17
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 => *c == b':',
            16 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

fn check_body(data: &Value) -> Result<&Map<String, Value>, DashError> {
    let body = data
        .as_object()
        .ok_or_else(|| DashError::Invalid("document body must be a JSON object".into()))?;
    if body.contains_key("version") {
        return Err(DashError::Invalid(
            "'version' is reserved for the store".into(),
        ));
    }
    Ok(body)
}

impl Store {
    /// Stages `op` on top of `staged`; reads the target under the caller's lock.
    pub(super) fn stage(&self, staged: &mut Staged, op: &Op) -> Result<OpResult, DashError> {
        let (coll, id, expected) = op.parts();
        check_name("collection", coll)?;
        check_name("doc id", id)?;
        self.guard(coll)?;
        let key = (coll.to_owned(), id.to_owned());
        let slot_at = match staged.index.get(&key) {
            Some(i) => *i,
            None => {
                let found = self.current(&mut staged.fresh, coll, id)?;
                staged.slots.push(Slot {
                    coll: coll.to_owned(),
                    id: id.to_owned(),
                    exists: found.is_some(),
                    version: found.as_ref().map_or(0, |f| f.0),
                    data: found.as_ref().map(|f| f.1.clone()).unwrap_or_default(),
                    dirty: false,
                    old: found,
                });
                staged.index.insert(key.clone(), staged.slots.len() - 1);
                staged.slots.len() - 1
            }
        };
        let cur = &mut staged.slots[slot_at];
        let have = if cur.exists { cur.version } else { 0 };
        if let Some(want) = expected.filter(|w| *w != have) {
            return Err(DashError::Conflict(format!(
                "{coll}/{id}: if_version {want} but current version is {have}"
            )));
        }
        let (Op::Set { data, .. } | Op::Update { data, .. }) = op else {
            if !cur.exists {
                return Err(DashError::NotFound(format!("{coll}/{id} not found")));
            }
            cur.exists = false;
            cur.data = Map::new();
            cur.dirty = true;
            return Ok(OpResult::Deleted {
                collection: key.0,
                id: key.1,
                deleted: true,
            });
        };
        let body = check_body(data)?.clone();
        let Value::Object(body) = normalize_timestamps(Value::Object(body), None) else {
            return Err(DashError::Invalid(
                "document body must be a JSON object".into(),
            ));
        };
        if matches!(op, Op::Update { .. }) {
            if !cur.exists {
                return Err(DashError::NotFound(format!("{coll}/{id} not found")));
            }
            cur.data.extend(body);
        } else {
            cur.data = body;
        }
        cur.exists = true;
        cur.dirty = true;
        cur.version += 1;
        Ok(OpResult::Written {
            collection: key.0,
            id: key.1,
            version: cur.version,
        })
    }

    /// `(version, body)` of `coll/id` as of now; `jsonl` reads each collection
    /// once per batch.
    fn current(
        &self,
        fresh: &mut BTreeMap<String, Docs>,
        coll: &str,
        id: &str,
    ) -> Result<Option<Versioned>, DashError> {
        match self.format {
            Format::Dir => dir::load(&dir::doc_path(&self.root, coll, id)),
            Format::Jsonl => {
                if !fresh.contains_key(coll) {
                    fresh.insert(coll.to_owned(), jsonl::read_coll(&self.root, coll)?);
                }
                Ok(fresh.get(coll).and_then(|docs| docs.get(id)).cloned())
            }
        }
    }

    /// Writes every dirty slot; on a failure restores every earlier file.
    pub(super) fn commit(&self, staged: &Staged) -> Result<(), DashError> {
        let dirty = staged.slots.iter().filter(|s| s.dirty);
        let plan: Vec<(PathBuf, Option<String>)> = match self.format {
            Format::Dir => dirty
                .map(|s| {
                    let path = dir::doc_path(&self.root, &s.coll, &s.id);
                    (path, s.exists.then(|| dir::render(s.version, &s.data)))
                })
                .collect(),
            Format::Jsonl => {
                let mut by_coll: BTreeMap<&str, Docs> = BTreeMap::new();
                for s in dirty {
                    let docs = by_coll
                        .entry(s.coll.as_str())
                        .or_insert_with(|| staged.fresh.get(&s.coll).cloned().unwrap_or_default());
                    if s.exists {
                        docs.insert(s.id.clone(), (s.version, s.data.clone()));
                    } else {
                        docs.remove(&s.id);
                    }
                }
                by_coll
                    .into_iter()
                    .map(|(coll, docs)| {
                        let text = jsonl::render_file(&docs);
                        (
                            jsonl::coll_path(&self.root, coll),
                            (!text.is_empty()).then_some(text),
                        )
                    })
                    .collect()
            }
        };
        apply(&plan)
    }
}

/// Writes `(path, bytes | None = remove)` in order; on any failure restores
/// every earlier path's previous bytes (or absence) and returns the failure.
fn apply(plan: &[(PathBuf, Option<String>)]) -> Result<(), DashError> {
    let mut backups: Vec<(&PathBuf, Option<Vec<u8>>)> = Vec::new();
    for (path, payload) in plan {
        let old = match fs::read(path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(rollback(&backups, DashError::io(path)(e))),
        };
        let existed = old.is_some();
        backups.push((path, old));
        let step = match payload {
            None if existed => fs::remove_file(path)
                .map_err(DashError::io(path))
                .and_then(|()| sync_dir(path).map_err(DashError::from)),
            None => Ok(()),
            Some(text) => write(path, text),
        };
        if let Err(e) = step {
            return Err(rollback(&backups, e));
        }
    }
    Ok(())
}

fn write(path: &Path, text: &str) -> Result<(), DashError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(DashError::io(parent))?;
    }
    write_atomic(path, text).map_err(DashError::from)
}

/// Restores `backups` newest first; a restore that fails is logged, and the
/// original error is returned.
fn rollback(backups: &[(&PathBuf, Option<Vec<u8>>)], error: DashError) -> DashError {
    for (path, old) in backups.iter().rev() {
        let restored = match old {
            None if path.exists() => fs::remove_file(path).map_err(DashError::io(path)),
            None => Ok(()),
            Some(bytes) => write_atomic(path, bytes).map_err(DashError::from),
        };
        if let Err(e) = restored {
            tracing::error!(path = %path.display(), error = %e, "dashboard store: rollback could not restore a file");
        }
    }
    error
}
