//! The `dir` format: one JSON file per document.
//!
//! Why: the live Architect store and the page's ArtifactData mirror use this
//! layout today (`dashboard_store.py`, DOC-82 LDG-3.2).
//! What: `<root>/<coll>/<id>.json` holds the body plus an integer `version`,
//! written as [`super::super::canonical::pretty`] plus LF. [`load`] reads one
//! (an old mirror file with no `version`, or a bad one, counts as version 1);
//! [`list`] reads a collection in id order; [`collections`] lists
//! directories holding at least one visible `*.json`. A truncated or
//! non-object file is corrupt, never absent.
//! Test: `dir_old_mirror_file_reads_as_version_one`, `partial_dir_doc_is_corrupt`,
//! `dashboard_writer_matches_prototype_bytes`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::super::canonical::pretty;
use super::super::error::DashError;
use super::{Doc, Versioned};

/// `<root>/<coll>/<id>.json`; the caller has checked both names.
pub fn doc_path(root: &Path, coll: &str, id: &str) -> PathBuf {
    root.join(coll).join(format!("{id}.json"))
}

/// A document file's bytes: the body with `version`, indented, LF-terminated.
pub fn render(version: u64, data: &Map<String, Value>) -> String {
    let mut body = data.clone();
    body.insert("version".into(), Value::from(version));
    pretty(&Value::Object(body)) + "\n"
}

/// `(version, body)` of the file at `path`; `None` when it is absent.
///
/// # Errors
/// [`DashError::Corrupt`] for a file that is not a JSON object;
/// [`DashError::Io`] when it cannot be read.
pub fn load(path: &Path) -> Result<Option<Versioned>, DashError> {
    let blob = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DashError::io(path)(e)),
    };
    let raw: Value = serde_json::from_slice(&blob)
        .map_err(|e| DashError::corrupt(path, format!("unreadable document: {e}")))?;
    let Value::Object(mut body) = raw else {
        return Err(DashError::corrupt(path, "document is not a JSON object"));
    };
    let version = body
        .remove("version")
        .and_then(|v| v.as_u64())
        .filter(|v| *v >= 1)
        .unwrap_or(1);
    Ok(Some((version, body)))
}

/// The documents of `<root>/<coll>/`, in id order; empty when it is absent.
///
/// # Errors
/// [`load`]'s errors for any document.
pub fn list(root: &Path, coll: &str) -> Result<Vec<Doc>, DashError> {
    let dir = root.join(coll);
    let mut names: Vec<String> = super::list_names(&dir)?
        .into_iter()
        .filter(|n| n.ends_with(".json") && !n.starts_with('.'))
        .collect();
    names.sort();
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if let Some((version, data)) = load(&dir.join(&name))? {
            let id = name.strip_suffix(".json").unwrap_or(&name).to_owned();
            out.push(Doc { id, version, data });
        }
    }
    Ok(out)
}

/// Collections of a `dir` store: visible directories (not `*.json.d`) holding
/// a visible `*.json`, sorted; empty when `root` is absent.
///
/// # Errors
/// [`DashError::Io`] when a directory exists but cannot be listed.
pub fn collections(root: &Path) -> Result<Vec<String>, DashError> {
    let mut out = Vec::new();
    for name in super::list_names(root)? {
        let dir = root.join(&name);
        if name.starts_with('.') || name.ends_with(".json.d") || !dir.is_dir() {
            continue;
        }
        let has_doc = super::list_names(&dir)?
            .iter()
            .any(|n| n.ends_with(".json") && !n.starts_with('.'));
        if has_doc {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

/// Whether `<root>/<coll>/` holds any `*.json` (hidden included, as the
/// prototype's format guard counts them).
pub fn has_docs(root: &Path, coll: &str) -> Result<bool, DashError> {
    let dir = root.join(coll);
    if !dir.is_dir() {
        return Ok(false);
    }
    Ok(super::list_names(&dir)?
        .iter()
        .any(|n| n.ends_with(".json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_old_mirror_file_reads_as_version_one() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        for (name, text, want) in [
            ("a.json", "{\"x\": 1}", 1),
            ("b.json", "{\"x\": 1, \"version\": 7}", 7),
            ("c.json", "{\"version\": true}", 1),
            ("d.json", "{\"version\": 0}", 1),
            ("e.json", "{\"version\": 2.0}", 1),
        ] {
            fs::write(tmp.path().join(name), text)?;
            let (version, body) = load(&tmp.path().join(name))?.ok_or("absent")?;
            assert_eq!(version, want, "{name}");
            assert!(!body.contains_key("version"));
        }
        assert!(load(&tmp.path().join("missing.json"))?.is_none());
        Ok(())
    }

    #[test]
    fn partial_dir_doc_is_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        for (name, text) in [
            ("t.json", "{\"x\": 1, \"y\""),
            ("l.json", "[1, 2]"),
            ("e.json", ""),
        ] {
            fs::write(tmp.path().join(name), text)?;
            let got = load(&tmp.path().join(name));
            assert!(
                matches!(got, Err(DashError::Corrupt { .. })),
                "{name}: {got:?}"
            );
        }
        Ok(())
    }
}
