//! `dashboard_store.py`'s write path: lock, stage, prune log, commit with rollback (#12029 P2c).
//!
//! Why: a batch is all-or-nothing in the prototype: every op is staged in
//! memory under one exclusive `flock` on `<root>/.lock`, a delete is recorded
//! in the prune log before anything is removed (fail closed), and a write
//! that fails midway restores the earlier files. Every failure of the lock,
//! the temp-file write or the replace is returned, never logged and passed.
//! What: [`PyStore::batch`] (lock, stage, the AC2 claim, prune log, commit)
//! and the one-op forms, `add` and `seed_meta`. The dir commit goes through
//! `fsio::apply`; the jsonl commit lives in `jsonl`.
//! Test: `dashboard_store_recordings_replay_clean_against_the_port`
//! (`errors`, `lock`), `a_failed_replace_is_an_error_and_restores_the_batch`,
//! `a_lock_that_cannot_be_taken_is_an_error_and_writes_nothing`.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use super::fsio::{apply, makedirs, random_hex};
use super::value::{
    check_body, check_id, next_version, normalize_timestamps, refuse_duplicate_request, render,
    restore_on_brief, sorted,
};
use super::{Fmt, Found, PyStore, PyStoreError, dirname};
use crate::dashboard::pyjsonl;
use crate::dashboard::store::{lock, writer};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pyval::{dumps, get, repr};

/// One staged document (`staged[(coll, id)]`).
pub(super) struct Slot {
    pub(super) coll: String,
    pub(super) id: String,
    pub(super) path: String,
    pub(super) exists: bool,
    pub(super) version: PyInt,
    pub(super) data: Vec<(String, PyValue)>,
    pub(super) dirty: bool,
    pub(super) old: Option<Found>,
}

/// What one op did: `{collection, id, version}` or `{collection, id, deleted}`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpOut {
    pub(crate) collection: String,
    pub(crate) id: String,
    /// `None` for a delete.
    pub(crate) version: Option<PyInt>,
}

/// The staged documents in insertion order, and the jsonl lines read once per collection.
#[derive(Default)]
pub(super) struct Staged {
    pub(super) slots: Vec<Slot>,
    pub(super) index: HashMap<(String, String), usize>,
    pub(super) fresh: HashMap<String, Vec<pyjsonl::Line>>,
    pub(super) op_of: HashMap<(String, String), String>,
}

impl PyStore {
    /// `_current(coll, doc_id, fresh)`.
    fn current(
        &self,
        st: &mut Staged,
        coll: &PyValue,
        id: &PyValue,
    ) -> Result<Option<Found>, PyStoreError> {
        let name = self.guard(coll)?.to_owned();
        if self.fmt == Fmt::Dir {
            return self.load(&self.path(coll, id)?);
        }
        let id = check_id("doc id", id)?;
        if !st.fresh.contains_key(&name) {
            let lines = self.read_coll(&name)?;
            st.fresh.insert(name.clone(), lines);
        }
        Ok(st.fresh[&name]
            .iter()
            .find(|(i, _, _)| i == id)
            .map(|(_, v, d)| {
                let pairs = match d {
                    PyValue::Dict(p) => p.clone(),
                    _ => Vec::new(),
                };
                (v.clone(), pairs)
            }))
    }

    /// `_stage(staged, op, fresh)`.
    fn stage(&self, st: &mut Staged, op: &PyValue) -> Result<OpOut, PyStoreError> {
        let null = PyValue::Null;
        let kind = get(op, "op").unwrap_or(&null);
        let kind = match kind {
            PyValue::Str(k) if matches!(k.as_str(), "set" | "update" | "delete") => k.clone(),
            other => {
                return Err(PyStoreError::Store(format!(
                    "unknown op {}: use set, update or delete",
                    repr(other)
                )));
            }
        };
        let (coll_v, id_v) = (
            get(op, "collection").unwrap_or(&null),
            get(op, "id").unwrap_or(&null),
        );
        let path = self.path(coll_v, id_v)?;
        let (coll, id) = (
            check_id("collection", coll_v)?.to_owned(),
            check_id("doc id", id_v)?.to_owned(),
        );
        let key = (coll.clone(), id.clone());
        let at = match st.index.get(&key) {
            Some(i) => *i,
            None => {
                let found = self.current(st, coll_v, id_v)?;
                st.slots.push(Slot {
                    coll: coll.clone(),
                    id: id.clone(),
                    path,
                    exists: found.is_some(),
                    version: found.as_ref().map_or(PyInt::Small(0), |f| f.0.clone()),
                    data: found.as_ref().map_or_else(Vec::new, |f| f.1.clone()),
                    dirty: false,
                    old: found,
                });
                st.index.insert(key.clone(), st.slots.len() - 1);
                st.slots.len() - 1
            }
        };
        st.op_of.insert(key, kind.clone());
        let cur = &mut st.slots[at];
        match get(op, "if_version") {
            None | Some(PyValue::Null) => {}
            Some(PyValue::Int(want)) => {
                let have = if cur.exists {
                    cur.version.clone()
                } else {
                    PyInt::Small(0)
                };
                if crate::pyval::cmp_int(want, &have).is_ne() {
                    return Err(PyStoreError::Conflict(format!(
                        "{coll}/{id}: if_version {} but current version is {}",
                        crate::pyval::int_text(want),
                        crate::pyval::int_text(&have)
                    )));
                }
            }
            Some(_) => return Err(PyStoreError::Store("if_version must be an integer".into())),
        }
        if kind == "delete" {
            if !cur.exists {
                return Err(PyStoreError::NotFound(format!("{coll}/{id} not found")));
            }
            (cur.exists, cur.data, cur.dirty) = (false, Vec::new(), true);
            return Ok(OpOut {
                collection: coll,
                id,
                version: None,
            });
        }
        let body = check_body(get(op, "data"))?;
        let PyValue::Dict(mut data) = normalize_timestamps(&PyValue::Dict(body.to_vec()), None)
        else {
            return Err(PyStoreError::Store(
                "document body must be a JSON object".into(),
            ));
        };
        if kind == "update" {
            if !cur.exists {
                return Err(PyStoreError::NotFound(format!("{coll}/{id} not found")));
            }
            let mut merged = cur.data.clone();
            for (k, v) in data {
                merged = super::value::with_key(&merged, &k, v);
            }
            data = merged;
        }
        if cur.exists {
            let (old, new) = (PyValue::Dict(cur.data.clone()), PyValue::Dict(data.clone()));
            refuse_duplicate_request(&coll, &id, &old, &new)?;
        }
        let was_pending = matches!(get(&PyValue::Dict(cur.data.clone()), "status"),
            Some(PyValue::Str(s)) if s == "brief_pending");
        if coll == "items" && cur.exists && was_pending {
            data = restore_on_brief(data);
        }
        cur.version = next_version(&cur.version);
        (cur.exists, cur.data, cur.dirty) = (true, data, true);
        Ok(OpOut {
            collection: coll,
            id,
            version: Some(cur.version.clone()),
        })
    }

    /// Why: the one write entry; every other write is a one-op batch.
    /// What: `batch(ops, actor)`; see the module doc. The AC2 claim runs
    /// after staging, before the prune log, so a refusal writes nothing.
    /// Test: `dashboard_store_recordings_replay_clean_against_the_port`,
    /// `a_second_writer_class_on_one_collection_is_refused`.
    ///
    /// # Errors
    /// Every prototype refusal, an `OSError`, [`PyStoreError::WriterClass`],
    /// or [`PyStoreError::Unavailable`] when the lock wait passes.
    pub(crate) fn batch(
        &self,
        ops: &PyValue,
        actor: Option<&str>,
    ) -> Result<Vec<OpOut>, PyStoreError> {
        let ops = match ops {
            PyValue::List(l) if !l.is_empty() => l,
            _ => {
                return Err(PyStoreError::Store(
                    "batch needs a non-empty list of ops".into(),
                ));
            }
        };
        let actor = actor.filter(|a| !a.is_empty()).unwrap_or(&self.actor);
        makedirs(&self.root)?;
        let _lock =
            lock::acquire(Path::new(&self.root), self.lock_wait).map_err(|e| self.lock_error(e))?;
        let mut st = Staged::default();
        let mut results = Vec::with_capacity(ops.len());
        for op in ops {
            if !matches!(op, PyValue::Dict(_)) {
                return Err(PyStoreError::Store(
                    "each batch op must be an object".into(),
                ));
            }
            results.push(self.stage(&mut st, op)?);
        }
        // #12029 AC2: one writer class per collection.
        writer::admit(
            Path::new(&self.root),
            st.slots.iter().map(|s| s.coll.as_str()),
            Self::CLASS,
        )
        .map_err(|e| self.lock_error(e))?;
        self.log_pruned(&st, actor)?;
        match self.fmt {
            Fmt::Jsonl => self.commit_jsonl(&st, actor)?,
            Fmt::Dir => {
                let plan: Vec<_> = st
                    .slots
                    .iter()
                    .filter(|s| s.dirty)
                    .map(|s| {
                        (
                            s.path.clone(),
                            s.exists.then(|| render(&s.version, &s.data).into_bytes()),
                        )
                    })
                    .collect();
                apply(&plan, false)?;
            }
        }
        Ok(results)
    }

    /// A lock or claim failure as the prototype would raise it.
    pub(super) fn lock_error(&self, e: crate::dashboard::DashError) -> PyStoreError {
        use crate::dashboard::DashError;
        match e {
            DashError::Io { path, source } => PyStoreError::os(&path.to_string_lossy(), &source),
            DashError::Unavailable(m) => PyStoreError::Unavailable(m),
            DashError::WriterClass(m) => PyStoreError::WriterClass(m),
            other => PyStoreError::Store(other.to_string()),
        }
    }

    /// `_log_pruned(staged, actor)`: one line per deleted existing document, fsynced; fail closed.
    fn log_pruned(&self, st: &Staged, actor: &str) -> Result<(), PyStoreError> {
        let mut recs = String::new();
        for s in st.slots.iter().filter(|s| s.dirty && !s.exists) {
            let Some((version, data)) = &s.old else {
                continue;
            };
            let rec = PyValue::Dict(vec![
                ("pruned_at".into(), PyValue::Str(self.now_iso())),
                ("actor".into(), PyValue::Str(actor.to_owned())),
                ("collection".into(), PyValue::Str(s.coll.clone())),
                ("id".into(), PyValue::Str(s.id.clone())),
                ("version".into(), PyValue::Int(version.clone())),
                ("data".into(), PyValue::Dict(data.clone())),
            ]);
            recs.push_str(&(dumps(&sorted(&rec), None, false) + "\n"));
        }
        if recs.is_empty() {
            return Ok(());
        }
        let path = self.prune_log_path();
        let refused = |e: String| {
            PyStoreError::Store(format!(
                "prune log not written ({path}): {e}; delete refused"
            ))
        };
        let dir = dirname(&path);
        if dir.is_empty() {
            return Err(refused("[Errno 2] No such file or directory: ''".into()));
        }
        makedirs(dir).map_err(|e| refused(e.to_string()))?;
        let os = |e: std::io::Error| refused(crate::writers::errno_text(&e, &path));
        let mut f = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(os)?;
        f.write_all(recs.as_bytes()).map_err(os)?;
        f.sync_all().map_err(os)
    }

    /// One op as a one-op batch.
    fn one(
        &self,
        kind: &str,
        coll: &PyValue,
        id: &PyValue,
        data: Option<&PyValue>,
        if_version: Option<&PyValue>,
        actor: Option<&str>,
    ) -> Result<OpOut, PyStoreError> {
        let mut op = vec![
            ("op".to_owned(), PyValue::Str(kind.into())),
            ("collection".to_owned(), coll.clone()),
            ("id".to_owned(), id.clone()),
        ];
        if kind != "delete" {
            op.push(("data".into(), data.cloned().unwrap_or(PyValue::Null)));
        }
        op.push((
            "if_version".into(),
            if_version.cloned().unwrap_or(PyValue::Null),
        ));
        let mut out = self.batch(&PyValue::List(vec![PyValue::Dict(op)]), actor)?;
        out.pop()
            .ok_or_else(|| PyStoreError::Store("batch returned no result".into()))
    }

    /// `set(coll, doc_id, data, if_version, actor)`.
    pub(crate) fn set(
        &self,
        coll: &PyValue,
        id: &PyValue,
        data: Option<&PyValue>,
        if_version: Option<&PyValue>,
        actor: Option<&str>,
    ) -> Result<OpOut, PyStoreError> {
        self.one("set", coll, id, data, if_version, actor)
    }

    /// `update(coll, doc_id, data, if_version, actor)`.
    pub(crate) fn update(
        &self,
        coll: &PyValue,
        id: &PyValue,
        data: Option<&PyValue>,
        if_version: Option<&PyValue>,
        actor: Option<&str>,
    ) -> Result<OpOut, PyStoreError> {
        self.one("update", coll, id, data, if_version, actor)
    }

    /// `delete(coll, doc_id, if_version, actor)`.
    pub(crate) fn delete(
        &self,
        coll: &PyValue,
        id: &PyValue,
        if_version: Option<&PyValue>,
        actor: Option<&str>,
    ) -> Result<OpOut, PyStoreError> {
        self.one("delete", coll, id, None, if_version, actor)
    }

    /// `add(coll, data, actor)`: a new document under a uuid4-shaped 20-hex-digit id.
    pub(crate) fn add(
        &self,
        coll: &PyValue,
        data: Option<&PyValue>,
        actor: Option<&str>,
    ) -> Result<OpOut, PyStoreError> {
        let mut id: Vec<char> = random_hex(20).chars().collect();
        id[12] = '4';
        id[16] = ['8', '9', 'a', 'b'][id[16].to_digit(16).unwrap_or(0) as usize % 4];
        let id = PyValue::Str(id.into_iter().collect());
        self.set(coll, &id, data, Some(&PyValue::Int(PyInt::Small(0))), actor)
    }

    /// `seed_meta(owner_name)`: create `meta/state` when absent; true when created.
    ///
    /// # Errors
    /// [`PyStore::batch`]'s errors other than a conflict.
    pub(crate) fn seed_meta(&self, owner_name: &str) -> Result<bool, PyStoreError> {
        let body = PyValue::Dict(vec![
            ("owner_name".into(), PyValue::Str(owner_name.into())),
            ("updated".into(), PyValue::Str(self.now_iso())),
        ]);
        let (coll, id) = (PyValue::Str("meta".into()), PyValue::Str("state".into()));
        match self.set(
            &coll,
            &id,
            Some(&body),
            Some(&PyValue::Int(PyInt::Small(0))),
            None,
        ) {
            Ok(_) => Ok(true),
            Err(PyStoreError::Conflict(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
}
