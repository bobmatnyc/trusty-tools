//! `dashboard_store.Store(root).list_docs(coll)` as the frozen Python reads it (#12028 P1d).
//!
//! Why: `stale-tasks.py` prints the store's own error text (`cannot read
//! store: <e>`), and a document may hold values `serde_json` refuses (`NaN`,
//! a 70-digit integer); the crate's `dashboard::store::Store` reads into
//! `serde_json` and words its errors differently by design (#10063), so the
//! port reads the two layouts here, read-only and without the lock file.
//! What: [`list_docs`] guards the other layout, then reads `<root>/<coll>/*.json`
//! (sorted, hidden names skipped, `version` popped) or `<root>/<coll>.jsonl`
//! (`dashboard_jsonl.parse_file`: UTF-8, one `{data, id, sv, version}` line
//! each, `sv` 1, no duplicate id, sorted by id). Every failure is `Err(str(e))`.
//! [`abspath`] is `os.path.abspath`.
//! Test: the `stale-tasks` recordings (`corrupt-doc`, `doc-not-object`,
//! `wrong-format`, `items-is-file`, `jsonl-*`).

use std::path::Path;

use crate::dashboard::store::Format;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::py_repr;
use crate::spec_drift::py::os_error;

use super::{JsonFail, read_json};

/// One document: its id and its data dict.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PyDoc {
    pub(crate) id: String,
    pub(crate) data: PyValue,
    /// The stored `version` (#12087: `if_version` of a write); see [`doc_version`].
    pub(crate) version: u64,
}

/// `_load`'s version: a positive `int` (not a `bool`) that fits `u64`, else 1.
/// A larger `int` reads as 1, as the crate's `Store` reads it.
fn doc_version(v: Option<&PyValue>) -> u64 {
    match v {
        Some(PyValue::Int(PyInt::Small(n))) if *n >= 1 => n.unsigned_abs(),
        _ => 1,
    }
}

/// `os.path.abspath(p)`: joined to the working directory, then `normpath`.
pub(crate) fn abspath(p: &str) -> String {
    let joined = if p.starts_with('/') {
        p.to_owned()
    } else {
        let cwd = std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{cwd}/{p}")
    };
    let lead = if joined.starts_with("//") && !joined.starts_with("///") {
        "//"
    } else {
        "/"
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("{lead}{}", parts.join("/"))
}

/// `Store(root).list_docs(coll)` for an absolute `root`.
pub(crate) fn list_docs(root: &str, format: Format, coll: &str) -> Result<Vec<PyDoc>, String> {
    let jsonl = format!("{root}/{coll}.jsonl");
    let cdir = format!("{root}/{coll}");
    let has_jsonl = Path::new(&jsonl).exists();
    match format {
        Format::Dir if has_jsonl => Err(format!(
            "{root} holds {coll}.jsonl: set DASHBOARD_FORMAT=jsonl"
        )),
        Format::Dir => dir_docs(&cdir),
        Format::Jsonl => {
            if !has_jsonl && Path::new(&cdir).is_dir() {
                let names = names(&cdir)?.unwrap_or_default();
                if names.iter().any(|n| n.ends_with(".json")) {
                    return Err(format!(
                        "{cdir} holds per-document files and no {coll}.jsonl: run scripts/dashboard-migrate.py"
                    ));
                }
            }
            read_coll(&jsonl)
        }
    }
}

/// `Store(root).get(coll, id)` for an absolute `root`; `Ok(None)` where it
/// raises `NotFound` (#12087: `gh-session-track` reads `meta/gh_track`).
pub(crate) fn get_doc(
    root: &str,
    format: Format,
    coll: &str,
    id: &str,
) -> Result<Option<PyDoc>, String> {
    if format == Format::Jsonl {
        return Ok(list_docs(root, format, coll)?
            .into_iter()
            .find(|d| d.id == id));
    }
    if Path::new(&format!("{root}/{coll}.jsonl")).exists() {
        return Err(format!(
            "{root} holds {coll}.jsonl: set DASHBOARD_FORMAT=jsonl"
        ));
    }
    let path = format!("{root}/{coll}/{id}.json");
    let raw = match read_json(Path::new(&path)) {
        Ok(v) => v,
        Err(JsonFail::NotFound) => return Ok(None),
        Err(JsonFail::Error(e)) => return Err(format!("unreadable document {path}: {e}")),
    };
    let PyValue::Dict(mut pairs) = raw else {
        return Err(format!("document {path} is not a JSON object"));
    };
    let version = doc_version(pairs.iter().find(|(k, _)| k == "version").map(|p| &p.1));
    pairs.retain(|(k, _)| k != "version");
    Ok(Some(PyDoc {
        id: id.to_owned(),
        data: PyValue::Dict(pairs),
        version,
    }))
}

/// `os.listdir(dir)`; `Ok(None)` when it does not exist.
fn names(dir: &str) -> Result<Option<Vec<String>>, String> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(os_error(Path::new(dir), &e).msg),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| os_error(Path::new(dir), &e).msg)?;
        out.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(Some(out))
}

fn dir_docs(cdir: &str) -> Result<Vec<PyDoc>, String> {
    let mut names: Vec<String> = names(cdir)?
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.ends_with(".json") && !n.starts_with('.'))
        .collect();
    names.sort();
    let mut out = Vec::with_capacity(names.len());
    for n in names {
        let path = format!("{cdir}/{n}");
        let raw = match read_json(Path::new(&path)) {
            Ok(v) => v,
            Err(JsonFail::NotFound) => continue,
            Err(JsonFail::Error(e)) => return Err(format!("unreadable document {path}: {e}")),
        };
        let PyValue::Dict(mut pairs) = raw else {
            return Err(format!("document {path} is not a JSON object"));
        };
        let version = doc_version(
            pairs
                .iter()
                .rev()
                .find(|(k, _)| k == "version")
                .map(|p| &p.1),
        );
        pairs.retain(|(k, _)| k != "version");
        let id = n[..n.len() - 5].to_owned();
        out.push(PyDoc {
            id,
            data: PyValue::Dict(pairs),
            version,
        });
    }
    Ok(out)
}

/// `dashboard_jsonl.read_coll`, sorted by id as `list_docs` returns it.
// #12029 P2b: the line parser is the `dashboard_jsonl` port's, defined once.
fn read_coll(path: &str) -> Result<Vec<PyDoc>, String> {
    let blob = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(os_error(Path::new(path), &e).msg),
    };
    let lines = crate::dashboard::pyjsonl::parse_file(&blob, path).map_err(|e| e.to_string())?;
    let mut out: Vec<PyDoc> = lines
        .into_iter()
        .map(|(id, version, data)| PyDoc {
            id,
            version: doc_version(Some(&PyValue::Int(version))),
            data,
        })
        .collect();
    out.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    Ok(out)
}

/// `repr` of a `DASHBOARD_FORMAT` value for the format error.
pub(crate) fn format_error(value: &str) -> String {
    format!("DASHBOARD_FORMAT={}: use dir or jsonl", py_repr(value))
}
