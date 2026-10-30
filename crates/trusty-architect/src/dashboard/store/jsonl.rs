//! The `jsonl` format: one canonical `<coll>.jsonl` per collection.
//!
//! Why: the owner wants every dashboard structure deterministic and encoded
//! in JSONL, so the same documents give the same bytes in Python and Rust
//! (`dashboard_jsonl.py`, DOC-82 LDG-3.2).
//! What: one line per document, `{"data":{...},"id":"A3e","sv":1,"version":7}`
//! in [`super::super::canonical::compact`] form, LF-terminated, sorted by id
//! bytes, no blank lines; an empty collection has no file. [`parse_file`]
//! accepts any formatting but is strict about shape: a line is an object with
//! a string `id`, an object `data`, `sv` 1 and an integer `version` >= 1, and
//! an id appears once. A partial last line is a corrupt file, never a
//! shorter collection. [`collections`] lists `*.jsonl` minus the change logs
//! (`changes.jsonl`, `changes-YYYY-MM.jsonl`), whose names are reserved.
//! Test: `jsonl_lines_sort_by_bytes_and_round_trip`,
//! `partial_jsonl_line_is_corrupt_not_shorter`, `dashboard_writer_matches_prototype_bytes`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::super::canonical::compact;
use super::super::error::DashError;

/// The line schema version this reader writes and accepts.
pub const SV: u64 = 1;

/// `{id: (version, data)}` of one collection, in id byte order.
pub type Docs = BTreeMap<String, super::Versioned>;

/// `<root>/<coll>.jsonl`.
pub fn coll_path(root: &Path, coll: &str) -> PathBuf {
    root.join(format!("{coll}.jsonl"))
}

/// Whether `coll` names a change log (`changes` or `changes-YYYY-MM`), in
/// any letter case (#10063 df-G01: `Changes.jsonl` is `changes.jsonl` on a
/// case-insensitive filesystem).
pub fn is_reserved(coll: &str) -> bool {
    let lower = coll.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("changes") else {
        return false;
    };
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    rest.is_empty()
        || rest
            .strip_prefix('-')
            .and_then(|r| r.split_once('-'))
            .is_some_and(|(y, m)| digits(y, 4) && digits(m, 2))
}

/// One document as its line, with the trailing LF.
pub fn render_line(id: &str, version: u64, data: &Map<String, Value>) -> String {
    compact(&json!({"data": data, "id": id, "sv": SV, "version": version})) + "\n"
}

/// A whole collection file; empty for no documents.
pub fn render_file(docs: &Docs) -> String {
    docs.iter()
        .map(|(id, (version, data))| render_line(id, *version, data))
        .collect()
}

/// Parses a collection file's bytes.
///
/// # Errors
/// [`DashError::Corrupt`] naming `path` and the line: not UTF-8, not JSON,
/// the wrong shape, a newer `sv`, a bad `version`, a duplicate id.
pub fn parse_file(blob: &[u8], path: &Path) -> Result<Docs, DashError> {
    let text = std::str::from_utf8(blob)
        .map_err(|e| DashError::corrupt(path, format!("not UTF-8: {e}")))?;
    let mut out = Docs::new();
    for (n, raw) in text.split('\n').enumerate() {
        if raw.is_empty() {
            continue;
        }
        let at = |why: String| DashError::corrupt(path, format!("line {}: {why}", n + 1));
        let line: Value =
            serde_json::from_str(raw).map_err(|e| at(format!("invalid JSON: {e}")))?;
        let (Some(id), Some(data)) = (
            line.get("id").and_then(Value::as_str),
            line.get("data").and_then(Value::as_object),
        ) else {
            return Err(at("a line is {data, id, sv, version}".into()));
        };
        match line.get("sv").and_then(Value::as_u64) {
            Some(SV) => {}
            Some(sv) if sv > SV => {
                return Err(at(format!("sv {sv} is newer than this reader ({SV})")));
            }
            _ => {
                return Err(at(format!(
                    "bad sv {}",
                    line.get("sv").unwrap_or(&Value::Null)
                )));
            }
        }
        let version = line
            .get("version")
            .and_then(Value::as_u64)
            .filter(|v| *v >= 1);
        let Some(version) = version else {
            return Err(at(format!(
                "bad version {}",
                line.get("version").unwrap_or(&Value::Null)
            )));
        };
        if out.insert(id.to_owned(), (version, data.clone())).is_some() {
            return Err(at(format!("duplicate id {id}")));
        }
    }
    Ok(out)
}

/// The documents of `<root>/<coll>.jsonl`; empty when the file is absent.
///
/// # Errors
/// [`DashError::Io`] when it cannot be read; [`parse_file`]'s errors.
pub fn read_coll(root: &Path, coll: &str) -> Result<Docs, DashError> {
    let path = coll_path(root, coll);
    match fs::read(&path) {
        Ok(blob) => parse_file(&blob, &path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Docs::new()),
        Err(e) => Err(DashError::io(&path)(e)),
    }
}

/// Collection names with a `.jsonl` file under `root`, change logs excluded,
/// sorted; empty when `root` is absent.
///
/// # Errors
/// [`DashError::Io`] when `root` exists but cannot be listed.
pub fn collections(root: &Path) -> Result<Vec<String>, DashError> {
    let mut out: Vec<String> = super::list_names(root)?
        .into_iter()
        .filter(|n| !n.starts_with('.'))
        .filter_map(|n| n.strip_suffix(".jsonl").map(str::to_owned))
        .filter(|c| !is_reserved(c))
        .collect();
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_lines_sort_by_bytes_and_round_trip() -> Result<(), DashError> {
        let mut docs = Docs::new();
        for (i, id) in ["b", "B", "a.b", "a-b", "0x"].iter().enumerate() {
            let data = json!({"z": i, "a": "é"})
                .as_object()
                .cloned()
                .unwrap_or_default();
            docs.insert((*id).to_owned(), (1, data));
        }
        let text = render_file(&docs);
        let order: Vec<&str> = docs.keys().map(String::as_str).collect();
        assert_eq!(order, ["0x", "B", "a-b", "a.b", "b"]);
        assert!(text.starts_with(
            "{\"data\":{\"a\":\"é\",\"z\":4},\"id\":\"0x\",\"sv\":1,\"version\":1}\n"
        ));
        assert_eq!(parse_file(text.as_bytes(), Path::new("x.jsonl"))?, docs);
        assert!(is_reserved("changes") && is_reserved("changes-2026-10"));
        assert!(is_reserved("Changes") && is_reserved("CHANGES-2026-10"));
        assert!(!is_reserved("changes-2026") && !is_reserved("changesx") && !is_reserved("items"));
        Ok(())
    }

    #[test]
    fn partial_jsonl_line_is_corrupt_not_shorter() {
        let good = "{\"data\":{},\"id\":\"A01\",\"sv\":1,\"version\":1}\n";
        let cases = [
            format!("{good}{{\"data\":{{}},\"id\":\"A02\",\"sv\""),
            format!("{good}{good}"),
            "{\"data\":{},\"id\":\"A01\",\"sv\":2,\"version\":1}\n".to_owned(),
            "{\"data\":{},\"id\":\"A01\",\"sv\":1,\"version\":0}\n".to_owned(),
            "{\"data\":[],\"id\":\"A01\",\"sv\":1,\"version\":1}\n".to_owned(),
        ];
        for blob in cases {
            assert!(
                matches!(
                    parse_file(blob.as_bytes(), Path::new("i.jsonl")),
                    Err(DashError::Corrupt { .. })
                ),
                "{blob}"
            );
        }
        assert!(parse_file(b"\xff", Path::new("i.jsonl")).is_err());
    }
}
