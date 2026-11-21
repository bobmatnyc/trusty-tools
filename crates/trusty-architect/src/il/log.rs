//! Reading `log.jsonl` and its rotated files.
//!
//! Why: the digest, `acts`, prune and rotate all read the log, which has two
//! line shapes (`t`/`ev` events and `ts`/`kind` notices) and grows to a megabyte.
//! Reads must tolerate a torn or foreign line and must not scan more than they
//! have to.
//! What: [`log_rec`] normalises both shapes, [`read_tail`] reads the last bytes
//! of a file on a line boundary, [`recent`] keeps the lines of chosen kinds
//! (pre-filtered by substring before parsing), [`answered_times`] folds the
//! `answered` events, and [`log_files_newest_first`] lists the rotated files.
//! Test: `log_rec_reads_both_shapes`, `read_tail_drops_the_partial_first_line`,
//! `recent_filters_kinds_without_parsing_the_rest`, `rotated_files_are_listed_newest_first`.

use super::json::Json;
use super::store::Paths;
use super::time::parse_ts;
use crate::pystr::splitlines;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// One parsed log line.
#[derive(Debug, Clone)]
pub struct Rec {
    /// The `t` or `ts` timestamp, when readable.
    pub t: Option<i64>,
    /// `ev` or `kind`, `""` when neither.
    pub kind: String,
    /// The whole record.
    pub rec: Json,
}

/// `log_rec`: both line shapes, `None` for anything that is not a JSON object.
pub fn log_rec(line: &str) -> Option<Rec> {
    let rec = Json::parse(line).ok()?;
    if !matches!(rec, Json::Obj(_)) {
        return None;
    }
    let pick = |a: &str, b: &str| -> Option<String> {
        [a, b]
            .iter()
            .find_map(|k| rec.get(k).filter(|v| v.truthy()).map(Json::py_str))
    };
    let t = parse_ts(&pick("t", "ts").unwrap_or_default());
    let kind = pick("ev", "kind").unwrap_or_default();
    Some(Rec { t, kind, rec })
}

/// The text of `path`, or only its last `tail` bytes starting at a line boundary.
/// A missing file reads as empty. Invalid UTF-8 becomes U+FFFD.
pub fn read_tail(path: &Path, tail: Option<u64>) -> String {
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    let size = f.metadata().map_or(0, |m| m.len());
    let mut buf = Vec::new();
    match tail {
        Some(n) if size > n => {
            if f.seek(SeekFrom::Start(size - n)).is_err() || f.read_to_end(&mut buf).is_err() {
                return String::new();
            }
            // `readline()` after the seek: the first, partial line is dropped.
            let cut = buf
                .iter()
                .position(|&b| b == b'\n')
                .map_or(buf.len(), |i| i + 1);
            buf.drain(..cut);
        }
        _ => {
            if f.read_to_end(&mut buf).is_err() {
                return String::new();
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Lines of the log tail whose kind is one of `kinds`, oldest first. Lines that cannot contain a
/// kind name are skipped before the JSON parse.
pub fn recent(path: &Path, tail: Option<u64>, kinds: &[&str]) -> Vec<Rec> {
    let text = read_tail(path, tail);
    splitlines(&text)
        .into_iter()
        .filter(|l| kinds.iter().any(|k| l.contains(k)))
        .filter_map(log_rec)
        .filter(|r| kinds.contains(&r.kind.as_str()))
        .collect()
}

/// `answered_times`: `{id: epoch of the last answered event}`; ids absent count as old.
pub fn answered_times(path: &Path) -> HashMap<String, i64> {
    let text = read_tail(path, None);
    let mut out = HashMap::new();
    for l in splitlines(&text)
        .into_iter()
        .filter(|l| l.contains("answered"))
    {
        if let Some(r) = log_rec(l)
            && r.kind == "answered"
            && let Some(t) = r.t
        {
            let id = r.rec.get("id").filter(|v| v.truthy()).map(Json::py_str);
            if let Some(id) = id {
                out.insert(id, t);
            }
        }
    }
    out
}

/// `log.jsonl`, then `log-<YYYYMMDD>.jsonl` newest rotation day first, as `(path, rotation day)`.
pub fn log_files_newest_first(paths: &Paths) -> Vec<(PathBuf, Option<String>)> {
    let mut rot: Vec<String> = fs::read_dir(&paths.dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            n.len() == 18
                && n.starts_with("log-")
                && n.ends_with(".jsonl")
                && n[4..12].bytes().all(|b| b.is_ascii_digit())
        })
        .collect();
    rot.sort_by(|a, b| b.cmp(a));
    let mut out = vec![(paths.log.clone(), None)];
    out.extend(
        rot.into_iter()
            .map(|n| (paths.dir.join(&n), Some(n[4..12].to_owned()))),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_rec_reads_both_shapes() {
        let a =
            log_rec(r#"{"t":"2026-11-20T10:00Z","id":"pm-T01","ev":"done","note":"n"}"#).unwrap();
        assert_eq!((a.kind.as_str(), a.t.is_some()), ("done", true));
        let b = log_rec(r#"{"ts":"2026-11-20T10:00Z","session":"","kind":"digest"}"#).unwrap();
        assert_eq!(b.kind, "digest");
        assert!(log_rec("not json").is_none());
        assert!(log_rec("[1]").is_none());
        assert_eq!(log_rec("{}").unwrap().kind, "");
    }

    #[test]
    fn read_tail_drops_the_partial_first_line() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("log.jsonl");
        fs::write(&f, "aaaa\nbbbb\ncccc\n").unwrap();
        assert_eq!(read_tail(&f, Some(7)), "cccc\n");
        assert_eq!(read_tail(&f, Some(100)), "aaaa\nbbbb\ncccc\n");
        assert_eq!(read_tail(&t.path().join("none"), None), "");
    }

    #[test]
    fn recent_filters_kinds_without_parsing_the_rest() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("log.jsonl");
        let body = [
            r#"{"t":"2026-11-20T10:00Z","id":"a","ev":"done"}"#,
            r#"{"t":"2026-11-20T10:01Z","id":"b","ev":"update","note":"done thing"}"#,
            "garbage done",
            r#"{"ts":"2026-11-20T10:02Z","kind":"drift","text":"x"}"#,
        ]
        .join("\n");
        fs::write(&f, body).unwrap();
        let got = recent(&f, None, &["done", "drift"]);
        assert_eq!(
            got.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            ["done", "drift"]
        );
    }

    #[test]
    fn rotated_files_are_listed_newest_first() {
        let t = tempfile::tempdir().unwrap();
        for n in [
            "log-20261101.jsonl",
            "log-20261103.jsonl",
            "log-x.jsonl",
            "state.json",
        ] {
            fs::write(t.path().join(n), "").unwrap();
        }
        let p = Paths::at(t.path().to_path_buf());
        let got: Vec<_> = log_files_newest_first(&p)
            .into_iter()
            .map(|(_, d)| d)
            .collect();
        assert_eq!(
            got,
            [None, Some("20261103".into()), Some("20261101".into())]
        );
    }
}
