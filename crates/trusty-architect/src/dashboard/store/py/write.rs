//! `dashboard_store.py`'s write path: lock, stage, prune log, commit with rollback (#12029 P2c).
//!
//! Why: a batch is all-or-nothing in the prototype: every op is staged in
//! memory under one exclusive `flock` on `<root>/.lock`, a delete is recorded
//! in the prune log before anything is removed (fail closed), and a write
//! that fails midway restores the earlier files. Every failure of the lock,
//! the temp-file write or the replace is returned, never logged and passed.
//! What: [`PyStore::batch`] and the one-op forms; `apply` writes the plan
//! through `atomic_write` (temp file, fsync, `rename`) and on any failure
//! restores the earlier paths in reverse, returning the restore's own error
//! when a restore fails (as Python's handler re-raises its own exception).
//! The jsonl commit validates touched documents, renders each collection
//! file, then appends the change log; a change-log failure is a stderr line
//! after the commit, as in the prototype.
//! Test: `dashboard_store_recordings_replay_clean_against_the_port`
//! (`errors`, `lock`), `a_failed_replace_is_an_error_and_restores_the_batch`,
//! `a_lock_that_cannot_be_taken_is_an_error_and_writes_nothing`.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde_json::Value;

use super::value::{
    check_body, check_id, next_version, normalize_timestamps, refuse_duplicate_request, render,
    restore_on_brief, sorted,
};
use super::{Fmt, Found, PyStore, PyStoreError, dirname, from_jsonl};
use crate::dashboard::pyjsonl::{self, join};
use crate::dashboard::store::{lock, writer};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pyval::{dumps, get, repr};

/// One staged document (`staged[(coll, id)]`).
struct Slot {
    coll: String,
    id: String,
    path: String,
    exists: bool,
    version: PyInt,
    data: Vec<(String, PyValue)>,
    dirty: bool,
    old: Option<Found>,
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
struct Staged {
    slots: Vec<Slot>,
    index: HashMap<(String, String), usize>,
    fresh: HashMap<String, Vec<pyjsonl::Line>>,
    op_of: HashMap<(String, String), String>,
}

/// `os.makedirs(path, exist_ok=True)`, with the `OSError` of the deepest failing `mkdir`.
pub(crate) fn makedirs(path: &str) -> Result<(), PyStoreError> {
    let trimmed = path.trim_end_matches('/');
    let head = dirname(trimmed);
    if !head.is_empty() && head != trimmed && !Path::new(head).exists() {
        match makedirs(head) {
            Err(PyStoreError::Os { errno, .. }) if errno == libc::EEXIST => {}
            other => other?,
        }
    }
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(_) if Path::new(path).is_dir() => Ok(()),
        Err(e) => Err(PyStoreError::os(path, &e)),
    }
}

/// `_atomic_write(path, payload)`: a temp file in the same directory, fsynced, then `os.replace`.
fn atomic_write(path: &str, payload: &[u8]) -> Result<(), PyStoreError> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let tmp = join(
        dirname(path),
        &format!(".{base}.{}.{}.tmp", std::process::id(), random_hex(8)),
    );
    let result = (|| {
        let mut f = File::create(&tmp).map_err(|e| PyStoreError::os(&tmp, &e))?;
        f.write_all(payload)
            .map_err(|e| PyStoreError::os(&tmp, &e))?;
        f.sync_all().map_err(|e| PyStoreError::os(&tmp, &e))?;
        // #12029: the replace's failure is the batch's failure, never a warning.
        fs::rename(&tmp, path).map_err(|e| PyStoreError::os(&tmp, &e))
    })();
    if Path::new(&tmp).exists() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `n` lowercase hex digits from the process's random hasher keys and the clock.
pub(crate) fn random_hex(n: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    while out.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(n);
    out
}

/// `dj.fsync_dir(path)`.
fn fsync_dir(path: &str) -> Result<(), PyStoreError> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| PyStoreError::os(path, &e))
}

/// `Store._apply(plan, fsync_dirs)`: write in order; on failure restore every earlier path.
fn apply(plan: &[(String, Option<Vec<u8>>)], fsync_dirs: bool) -> Result<(), PyStoreError> {
    let mut backups: Vec<(&str, Option<Vec<u8>>)> = Vec::new();
    fn step<'a>(
        path: &'a str,
        payload: &Option<Vec<u8>>,
        backups: &mut Vec<(&'a str, Option<Vec<u8>>)>,
        fsync_dirs: bool,
    ) -> Result<(), PyStoreError> {
        let old = if Path::new(path).exists() {
            Some(fs::read(path).map_err(|e| PyStoreError::os(path, &e))?)
        } else {
            None
        };
        let had = old.is_some();
        backups.push((path, old));
        match payload {
            None if had => fs::remove_file(path).map_err(|e| PyStoreError::os(path, &e))?,
            None => {}
            Some(bytes) => {
                makedirs(dirname(path))?;
                atomic_write(path, bytes)?;
            }
        }
        if fsync_dirs {
            fsync_dir(dirname(path))?;
        }
        Ok(())
    }
    for (path, payload) in plan {
        if let Err(first) = step(path, payload, &mut backups, fsync_dirs) {
            for (path, old) in backups.iter().rev() {
                match old {
                    None if Path::new(path).exists() => {
                        fs::remove_file(path).map_err(|e| PyStoreError::os(path, &e))?;
                    }
                    None => {}
                    Some(bytes) => atomic_write(path, bytes)?,
                }
            }
            return Err(first);
        }
    }
    Ok(())
}

/// A [`PyValue`] as the schema validator's JSON (a big int as a float, a non-finite float as `null`).
fn to_json(v: &PyValue) -> Value {
    match v {
        PyValue::Null => Value::Null,
        PyValue::Bool(b) => Value::Bool(*b),
        PyValue::Int(PyInt::Small(n)) => Value::from(*n),
        PyValue::Int(i) => {
            serde_json::Number::from_f64(crate::pyval::int_text(i).parse().unwrap_or(f64::NAN))
                .map_or(Value::Null, Value::Number)
        }
        PyValue::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
        PyValue::Str(s) => Value::String(s.clone()),
        PyValue::List(items) => Value::Array(items.iter().map(to_json).collect()),
        PyValue::Dict(pairs) => {
            Value::Object(pairs.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
    }
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
    fn lock_error(&self, e: crate::dashboard::DashError) -> PyStoreError {
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

    /// `_check_schema(staged)`: the error list; enforce mode refuses.
    fn check_schema(&self, st: &Staged) -> Result<Vec<String>, PyStoreError> {
        if self.schema_mode == "off" {
            return Ok(Vec::new());
        }
        let mut touched: Vec<&Slot> = st.slots.iter().filter(|s| s.dirty && s.exists).collect();
        touched.sort_by(|a, b| (&a.coll, &a.id).cmp(&(&b.coll, &b.id)));
        let mut errs = Vec::new();
        for s in touched {
            let data = to_json(&PyValue::Dict(s.data.clone()));
            let got = crate::dashboard::schema_check::script::validate(
                &self.schema_dir,
                &s.coll,
                &s.id,
                &data,
            )
            .map_err(|e| PyStoreError::Schema(e.to_string()))?;
            errs.extend(got);
        }
        if !errs.is_empty() && self.schema_mode == "enforce" {
            return Err(PyStoreError::SchemaInvalid(errs));
        }
        for e in &errs {
            self.warn(&format!("dashboard-store: schema warning: {e}\n"));
        }
        Ok(errs)
    }

    /// `_commit_jsonl(staged, fresh, actor, op_of)`.
    fn commit_jsonl(&self, st: &Staged, actor: &str) -> Result<(), PyStoreError> {
        let warnings = self.check_schema(st)?;
        let mut colls: Vec<&str> = st
            .slots
            .iter()
            .filter(|s| s.dirty)
            .map(|s| s.coll.as_str())
            .collect();
        colls.sort_unstable();
        colls.dedup();
        let (mut plan, mut entries) = (Vec::new(), Vec::new());
        for coll in colls {
            let mine: Vec<&Slot> = st
                .slots
                .iter()
                .filter(|s| s.dirty && s.coll == coll)
                .collect();
            let mut docs: Vec<(String, PyValue, PyValue)> = st
                .fresh
                .get(coll)
                .into_iter()
                .flatten()
                .filter(|(id, _, _)| !mine.iter().any(|s| &s.id == id))
                .map(|(id, v, d)| (id.clone(), PyValue::Int(v.clone()), d.clone()))
                .collect();
            for s in mine.iter().filter(|s| s.exists) {
                docs.push((
                    s.id.clone(),
                    PyValue::Int(s.version.clone()),
                    PyValue::Dict(s.data.clone()),
                ));
            }
            let text = pyjsonl::render_file(&docs)
                .map_err(|e| PyStoreError::Store(format!("{coll}: {e}")))?;
            plan.push((
                pyjsonl::coll_path(&self.root, coll),
                (!text.is_empty()).then(|| text.into_bytes()),
            ));
            let mut order = mine;
            order.sort_by(|a, b| a.id.cmp(&b.id));
            for s in order {
                entries.push(self.entry(st, s, actor, &warnings)?);
            }
        }
        apply(&plan, true)?;
        if let Err(e) = pyjsonl::append_changes(&self.root, &entries, &self.now_iso()).map(|a| {
            if let Some(w) = a.warning {
                self.warn(&w);
            }
        }) {
            self.warn(&format!("dashboard-store: change log not written: {e}\n"));
        }
        Ok(())
    }

    /// One change-log entry for a staged document, with its schema warnings.
    fn entry(
        &self,
        st: &Staged,
        s: &Slot,
        actor: &str,
        warnings: &[String],
    ) -> Result<PyValue, PyStoreError> {
        let text = |v: &str| PyValue::Str(v.to_owned());
        let mut e = vec![
            ("actor".into(), text(actor)),
            ("coll".into(), text(&s.coll)),
            ("id".into(), text(&s.id)),
        ];
        if s.exists {
            let data = PyValue::Dict(s.data.clone());
            let old = s.old.as_ref().map(|(_, d)| PyValue::Dict(d.clone()));
            let keys = pyjsonl::changed_keys(old.as_ref(), Some(&data)).map_err(from_jsonl)?;
            let version = PyValue::Int(s.version.clone());
            let sv = PyValue::Int(PyInt::Small(pyjsonl::SV));
            let line = pyjsonl::render_line(&s.id, &version, &data, &sv).map_err(from_jsonl)?;
            let op = st
                .op_of
                .get(&(s.coll.clone(), s.id.clone()))
                .cloned()
                .unwrap_or_default();
            e.extend([
                (
                    "keys".into(),
                    PyValue::List(keys.into_iter().map(PyValue::Str).collect()),
                ),
                ("op".into(), PyValue::Str(op)),
                (
                    "sha256".into(),
                    PyValue::Str(pyjsonl::sha256(line.as_bytes())),
                ),
                ("version".into(), version),
            ]);
        } else {
            let version = s.old.as_ref().map_or(PyInt::Small(0), |o| o.0.clone());
            e.extend([
                ("keys".into(), PyValue::List(Vec::new())),
                ("op".into(), text("delete")),
                ("sha256".into(), PyValue::Null),
                ("version".into(), PyValue::Int(version)),
            ]);
        }
        let prefix = format!("{}/{}:", s.coll, s.id);
        let mine: Vec<PyValue> = warnings
            .iter()
            .filter(|w| w.starts_with(&prefix))
            .map(|w| text(w))
            .collect();
        if !mine.is_empty() {
            e.push(("warnings".into(), PyValue::List(mine)));
        }
        Ok(PyValue::Dict(e))
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
