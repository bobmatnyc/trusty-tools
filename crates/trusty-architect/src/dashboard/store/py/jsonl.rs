//! The prototype's jsonl commit: schema check, collection files, change log (#12029 P2c).
//!
//! Why: a jsonl batch renders each touched collection file whole and logs
//! one change entry per document, with that document's schema warnings.
//! What: `check_schema` (off, warn to stderr, enforce refuses),
//! `commit_jsonl` (render, apply with rollback, then append the change log;
//! a change-log failure is a stderr line after the commit, as in the
//! prototype) and `entry`.
//! Test: `dashboard_store_recordings_replay_clean_against_the_port` (`jsonl`).

use serde_json::Value;

use super::write::{Slot, Staged};
use super::{PyStore, PyStoreError, from_jsonl};
use crate::dashboard::pyjsonl;
use crate::pyjson_load::{PyInt, PyValue};

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
    pub(super) fn commit_jsonl(&self, st: &Staged, actor: &str) -> Result<(), PyStoreError> {
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
        super::fsio::apply(&plan, true)?;
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
}
