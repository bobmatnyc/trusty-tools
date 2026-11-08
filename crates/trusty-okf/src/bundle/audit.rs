//! The migration audit (SPEC-OKF-01 §2.9 "How a migration detects loss",
//! step 3).
//!
//! Why: a migration must lose nothing. The plan is checked before the
//! apply, but only a comparison of the tree before and after the apply
//! proves that what landed on disk kept every ID, body, key, source,
//! verification event and log line.
//! What: [`Inventory::take`] records, before the first write, the concept ID
//! set, each changed concept's parsed values and body, and the bytes of
//! every journal under `_sources/` and every log (`log.md`, `_state/`).
//! [`Inventory::check`] compares the tree after the apply with it and
//! returns one [`LossItem`] per failure; `migrate` turns a non-empty list
//! into `OkfError::MigrationLoss` after restoring every changed file.
//! Slice S6 of #10746.
//! Test: `c24_audit_restores_every_file_when_a_sources_entry_is_dropped`,
//! `c120_body_byte_change_restores_and_a_corrupt_blob_is_restore_failed`,
//! `c23_migration_keeps_ids_and_bodies_on_the_trusty_kb_tree`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_yaml::{Mapping, Value};

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::lock::LOCK_REL;
use crate::bundle::ops::OPS_DIR;
use crate::bundle::paths::{io_err, read_rel};
use crate::bundle::record::frontmatter_events;
use crate::format::concept::{Concept, parse_concept};
use crate::format::id::ConceptId;

/// The directory of migration manifests and backups (§2.9 step 2).
pub(crate) const MIGRATIONS_DIR: &str = "_sources/migrations";

/// One failed audit check (SPEC-OKF-01 §4.4 `MigrationLoss`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossItem {
    /// Bundle-relative path the check is about; empty for the ID set.
    pub path: String,
    /// `id_set`, `body`, `key`, `source`, `verification`, `journal`, `log`,
    /// `unreadable` or `restore_failed`.
    pub kind: String,
    /// What differed: the key, the source value or a short reason.
    pub detail: String,
}

impl LossItem {
    pub(crate) fn new(path: &str, kind: &str, detail: &str) -> Self {
        Self {
            path: path.to_string(),
            kind: kind.to_string(),
            detail: detail.to_string(),
        }
    }
}

/// The tree before a migration's apply.
#[derive(Debug)]
pub(crate) struct Inventory {
    ids: Vec<ConceptId>,
    concepts: Vec<(ConceptId, Concept)>,
    journals: BTreeMap<String, Vec<u8>>,
    logs: BTreeMap<String, Vec<u8>>,
}

/// Every regular file under `dir` (bundle-relative `rel`), recursively.
fn files_under(root: &Path, rel: &str, out: &mut Vec<String>) -> Result<(), OkfError> {
    let dir = root.join(rel);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(&dir, e)),
    };
    for e in entries {
        let e = e.map_err(|e| io_err(&dir, e))?;
        let name = e.file_name().to_string_lossy().into_owned();
        let child = format!("{rel}/{name}");
        let ft = e.file_type().map_err(|err| io_err(&e.path(), err))?;
        if ft.is_dir() {
            files_under(root, &child, out)?;
        } else if ft.is_file() {
            out.push(child);
        }
    }
    Ok(())
}

/// Journal files: every file under `_sources/` except the operation and
/// migration machinery and the lock (§2.9 item 8).
fn journal_paths(root: &Path) -> Result<Vec<String>, OkfError> {
    let mut all = Vec::new();
    files_under(root, "_sources", &mut all)?;
    all.retain(|p| {
        !p.starts_with(&format!("{OPS_DIR}/"))
            && !p.starts_with(&format!("{MIGRATIONS_DIR}/"))
            && p != LOCK_REL
    });
    Ok(all)
}

/// Log files: the root `log.md` and every file under `_state/`.
fn log_paths(root: &Path) -> Result<Vec<String>, OkfError> {
    let mut all = vec!["log.md".to_string()];
    files_under(root, "_state", &mut all)?;
    Ok(all)
}

fn snapshot(root: &Path, paths: Vec<String>) -> Result<BTreeMap<String, Vec<u8>>, OkfError> {
    let mut out = BTreeMap::new();
    for p in paths {
        if let Some(bytes) = read_rel(root, &p)? {
            out.insert(p, bytes);
        }
    }
    Ok(out)
}

/// Whether `new` holds `old` as one contiguous run of bytes.
fn contains(new: &[u8], old: &[u8]) -> bool {
    old.is_empty() || new.windows(old.len()).any(|w| w == old)
}

/// The string items of a `sources` value.
fn source_strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether the §2.8 rule for `key` maps `old` to the new values `new`, so the
/// inverse yields `old` byte for byte.
fn mapped(key: &str, old: &Value, new: &Mapping) -> bool {
    let get = |k: &str| new.get(Value::String(k.to_string()));
    match key {
        "updated" => get("generated").and_then(|g| g.get("at")) == Some(old),
        "sources" => match (old, get("sources")) {
            (Value::Sequence(o), Some(Value::Sequence(n))) if o.len() == n.len() => {
                o.iter().zip(n).all(|(o, n)| match o {
                    Value::String(s) => n.get("resource").and_then(Value::as_str) == Some(s),
                    other => other == n,
                })
            }
            _ => false,
        },
        "verified" => get("verified") == Some(&Value::Sequence(vec![old.clone()])),
        _ => false,
    }
}

impl Inventory {
    /// Record the tree before the apply; `changed` are the concepts the
    /// migration will write.
    pub(crate) fn take(b: &Bundle, changed: &[ConceptId]) -> Result<Self, OkfError> {
        let mut concepts = Vec::new();
        for id in changed {
            concepts.push((id.clone(), b.get(id)?));
        }
        Ok(Self {
            ids: b.concept_ids()?,
            concepts,
            journals: snapshot(b.root(), journal_paths(b.root())?)?,
            logs: snapshot(b.root(), log_paths(b.root())?)?,
        })
    }

    /// Compare the tree now with the inventory (§2.9 step 3).
    pub(crate) fn check(&self, b: &Bundle) -> Result<Vec<LossItem>, OkfError> {
        let mut loss = Vec::new();
        if b.concept_ids()? != self.ids {
            loss.push(LossItem::new("", "id_set", "concept ID set changed"));
        }
        for (id, old) in &self.concepts {
            let path = id.file_name();
            let new = match read_rel(b.root(), &path)?
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .map(|t| parse_concept(id.clone(), &t))
            {
                Some(Ok((c, _))) => c,
                _ => {
                    loss.push(LossItem::new(&path, "unreadable", "cannot parse"));
                    continue;
                }
            };
            if new.body != old.body {
                loss.push(LossItem::new(&path, "body", "body bytes differ"));
            }
            for (k, v) in old.values() {
                let key = k.as_str().unwrap_or_default();
                if new.values().get(k) != Some(v) && !mapped(key, v, new.values()) {
                    loss.push(LossItem::new(&path, "key", key));
                }
            }
            let resources: Vec<String> = crate::format::v02::sources(&new)
                .into_iter()
                .map(|s| s.resource)
                .collect();
            for s in source_strings(old.get("sources")) {
                if !resources.contains(&s) {
                    loss.push(LossItem::new(&path, "source", &s));
                }
            }
            let mut before = frontmatter_events(old.get("verified"));
            let mut after = frontmatter_events(new.get("verified"));
            before.sort();
            after.sort();
            if before != after {
                loss.push(LossItem::new(
                    &path,
                    "verification",
                    "event multiset differs",
                ));
            }
        }
        for (rel, old) in &self.journals {
            let now = read_rel(b.root(), rel)?.unwrap_or_default();
            if !now.starts_with(old) {
                loss.push(LossItem::new(rel, "journal", "not equal or only appended"));
            }
        }
        for (rel, old) in &self.logs {
            let now = read_rel(b.root(), rel)?.unwrap_or_default();
            if !contains(&now, old) {
                loss.push(LossItem::new(rel, "log", "old log is not a contiguous run"));
            }
        }
        Ok(loss)
    }
}
