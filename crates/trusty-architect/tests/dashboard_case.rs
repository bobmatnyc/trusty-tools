//! #10063 PR 2 hard gate: ids and collection names that differ only in ASCII
//! case. On APFS `A01.json` and `a01.json` are one file, so a write of `a01`
//! overwrote `A01` (code-critic repro). Both formats refuse such a write, and
//! a read never answers one name with another name's body.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use trusty_architect::dashboard::{DashError, DeleteArchive, Format, Op, Removed, Store};

type R = Result<(), Box<dyn std::error::Error>>;

const FORMATS: [Format; 2] = [Format::Dir, Format::Jsonl];

struct Accept;

impl DeleteArchive for Accept {
    fn record(&self, _: &[Removed<'_>]) -> Result<(), DashError> {
        Ok(())
    }
}

/// A test scanner: nothing is secret-shaped.
fn no_secret(_: &str) -> Option<String> {
    None
}

fn store(root: &Path, format: Format) -> Store {
    Store::new(root, format)
        .with_archive(Box::new(Accept))
        .with_secret_scan(no_secret)
}

/// Every file under `root` with its bytes, `.lock` excluded.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n != ".lock") {
                out.push((path.clone(), fs::read(&path).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

fn is_case_refusal(got: &Result<impl std::fmt::Debug, DashError>, other: &str) -> bool {
    matches!(got, Err(DashError::Invalid(m)) if m.contains("only in letter case") && m.contains(other))
}

#[test]
fn case_variant_id_write_refused_and_writes_nothing() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let s = store(tmp.path(), format);
        s.set("items", "A01", json!({"title": "first"}), None)?;
        let before = snapshot(tmp.path());
        for got in [
            s.set("items", "a01", json!({"title": "second"}), None),
            s.set("items", "a01", json!({"title": "second"}), Some(0)),
            s.update("items", "a01", json!({"title": "second"}), None),
        ] {
            assert!(is_case_refusal(&got, "items/A01"), "{format}: {got:?}");
        }
        assert_eq!(snapshot(tmp.path()), before, "{format}: bytes changed");
        let a = s.get("items", "A01")?;
        assert_eq!(a.data["title"], "first", "{format}");
        assert_eq!(a.version, 1, "{format}");
    }
    Ok(())
}

#[test]
fn case_variant_in_one_batch_refused() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let s = store(tmp.path(), format);
        let set = |id: &str| Op::Set {
            collection: "items".into(),
            id: id.into(),
            data: json!({"title": id}),
            if_version: None,
        };
        let got = s.batch(&[set("B07"), set("b07")]);
        assert!(is_case_refusal(&got, "items/B07"), "{format}: {got:?}");
        assert!(snapshot(tmp.path()).is_empty(), "{format}: wrote a file");
        assert!(matches!(s.get("items", "B07"), Err(DashError::NotFound(_))));
    }
    Ok(())
}

#[test]
fn get_never_answers_under_a_case_variant_name() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let s = store(tmp.path(), format);
        s.set("items", "A01", json!({"title": "first"}), None)?;
        let got = s.get("items", "a01");
        assert!(
            matches!(got, Err(DashError::NotFound(_))),
            "{format}: {got:?}"
        );
        // A delete of the variant must not reach the other id's file.
        let gone = s.delete("items", "a01", None);
        assert!(
            matches!(gone, Err(DashError::NotFound(_))),
            "{format}: {gone:?}"
        );
        assert_eq!(s.get("items", "A01")?.data["title"], "first", "{format}");
    }
    Ok(())
}

#[test]
fn case_variant_collection_refused() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let s = store(tmp.path(), format);
        s.set("items", "A01", json!({"title": "first"}), None)?;
        let before = snapshot(tmp.path());
        let got = s.set("Items", "A01", json!({"title": "second"}), None);
        assert!(is_case_refusal(&got, "items"), "{format}: {got:?}");
        let read = s.get("Items", "A01");
        assert!(is_case_refusal(&read, "items"), "{format}: {read:?}");
        assert_eq!(snapshot(tmp.path()), before, "{format}: bytes changed");
        // The change log's name is reserved whatever its case.
        let log = s.set("Changes", "A01", json!({}), None);
        assert!(
            matches!(log, Err(DashError::Invalid(_))),
            "{format}: {log:?}"
        );
    }
    Ok(())
}
