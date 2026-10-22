//! #10063 PR 1: store failure paths in both formats. A refused write leaves
//! every byte as it was (DOC-82 LDG-2.8.1, LDG-2.8.4, LDG-3.2).

use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_architect::dashboard::{
    DashError, DeleteArchive, Format, ListQuery, Op, Removed, SchemaDir, SchemaMode, Store,
};

type R = Result<(), Box<dyn std::error::Error>>;

const FORMATS: [Format; 2] = [Format::Dir, Format::Jsonl];

struct Accept;

impl DeleteArchive for Accept {
    fn record(&self, _: &[Removed<'_>]) -> Result<(), DashError> {
        Ok(())
    }
}

struct Refuse;

impl DeleteArchive for Refuse {
    fn record(&self, removed: &[Removed<'_>]) -> Result<(), DashError> {
        Err(DashError::Invalid(format!(
            "archive write failed for {} documents",
            removed.len()
        )))
    }
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

fn seeded(root: &Path, format: Format) -> Result<Store, DashError> {
    let store = Store::new(root, format).with_archive(Box::new(Accept));
    store.set(
        "items",
        "D01",
        json!({"status": "open", "kind": "question"}),
        Some(0),
    )?;
    store.update("items", "D01", json!({"answer": "x"}), Some(1))?;
    store.set("goals", "G01", json!({"title": "g"}), None)?;
    Ok(store)
}

#[test]
fn stale_if_version_conflicts_and_leaves_bytes() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let store = seeded(tmp.path(), format)?;
        let before = snapshot(tmp.path());
        let stale = [
            Op::Update {
                collection: "items".into(),
                id: "D01".into(),
                data: json!({"answer": "y"}),
                if_version: Some(1),
            },
            Op::Set {
                collection: "items".into(),
                id: "D01".into(),
                data: json!({}),
                if_version: Some(0),
            },
            Op::Delete {
                collection: "goals".into(),
                id: "G01".into(),
                if_version: Some(3),
            },
        ];
        for op in stale {
            let got = store.batch(std::slice::from_ref(&op));
            assert!(
                matches!(got, Err(DashError::Conflict(_))),
                "{format}: {op:?}: {got:?}"
            );
            assert_eq!(snapshot(tmp.path()), before, "{format}: {op:?} wrote bytes");
        }
        let doc = store.get("items", "D01")?;
        assert_eq!(
            (doc.version, doc.data.get("answer")),
            (2, Some(&json!("x")))
        );
    }
    Ok(())
}

#[test]
fn body_with_version_refused() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let store = seeded(tmp.path(), format)?;
        let before = snapshot(tmp.path());
        for data in [
            json!({"version": 9}),
            json!({"a": 1, "version": null}),
            json!(["not", "an", "object"]),
        ] {
            let ops = [
                Op::Set {
                    collection: "items".into(),
                    id: "D02".into(),
                    data: json!({"ok": true}),
                    if_version: None,
                },
                Op::Update {
                    collection: "items".into(),
                    id: "D01".into(),
                    data,
                    if_version: None,
                },
            ];
            assert!(
                matches!(store.batch(&ops), Err(DashError::Invalid(_))),
                "{format}"
            );
            assert_eq!(
                snapshot(tmp.path()),
                before,
                "{format}: a refused body wrote bytes"
            );
        }
    }
    Ok(())
}

/// A write that fails after an earlier write in the same batch succeeded
/// restores the earlier file. The fault: a directory where the second
/// write's temp file must go (the shared atomic writer names it
/// `<file stem>.tmp.<pid>` beside the target).
#[test]
fn batch_rolls_back_on_midway_failure() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let store = seeded(tmp.path(), format)?;
        let before = snapshot(tmp.path());
        // dir commits in op order; jsonl commits by collection name. Either
        // way `goals` is written first and `items` is blocked.
        let (blocked, ops) = match format {
            Format::Dir => (
                tmp.path()
                    .join("items")
                    .join(format!("D01.tmp.{}", std::process::id())),
                [("goals", "G01"), ("items", "D01")],
            ),
            Format::Jsonl => (
                tmp.path().join(format!("items.tmp.{}", std::process::id())),
                [("goals", "G01"), ("items", "D01")],
            ),
        };
        fs::create_dir_all(&blocked)?;
        let batch: Vec<Op> = ops
            .iter()
            .map(|(c, i)| Op::Update {
                collection: (*c).into(),
                id: (*i).into(),
                data: json!({"n": 1}),
                if_version: None,
            })
            .collect();
        let got = store.batch(&batch);
        assert!(
            matches!(got, Err(DashError::Io { .. })),
            "{format}: {got:?}"
        );
        let after: Vec<_> = snapshot(tmp.path());
        assert_eq!(
            after, before,
            "{format}: the first write was not rolled back"
        );
    }
    Ok(())
}

#[test]
fn delete_is_refused_without_an_archive_or_when_it_fails() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        seeded(tmp.path(), format)?;
        let before = snapshot(tmp.path());
        let bare = Store::new(tmp.path(), format);
        assert!(
            matches!(
                bare.delete("items", "D01", None),
                Err(DashError::Invalid(_))
            ),
            "{format}"
        );
        let failing = Store::new(tmp.path(), format).with_archive(Box::new(Refuse));
        let got = failing.delete("goals", "G01", Some(1));
        assert_eq!(
            got.map_err(|e| e.to_string()).err().as_deref(),
            Some("archive write failed for 1 documents")
        );
        assert_eq!(
            snapshot(tmp.path()),
            before,
            "{format}: a refused delete removed a document"
        );
        // A document created and deleted in one batch has no record to keep.
        let ops = [
            Op::Set {
                collection: "items".into(),
                id: "D09".into(),
                data: json!({}),
                if_version: Some(0),
            },
            Op::Delete {
                collection: "items".into(),
                id: "D09".into(),
                if_version: None,
            },
        ];
        bare.batch(&ops)?;
        assert!(matches!(
            bare.get("items", "D09"),
            Err(DashError::NotFound(_))
        ));
    }
    Ok(())
}

#[test]
fn lock_contention_is_unavailable_and_writes_nothing() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        seeded(tmp.path(), format)?;
        let before = snapshot(tmp.path());
        let held = fs::OpenOptions::new()
            .write(true)
            .open(tmp.path().join(".lock"))?;
        // SAFETY: the fd stays open for `held`'s life; flock touches nothing else.
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
        let store = Store::new(tmp.path(), format).with_lock_wait(Duration::from_millis(150));
        let start = Instant::now();
        let got = store.set("items", "D02", json!({}), None);
        assert!(
            matches!(got, Err(DashError::Unavailable(_))),
            "{format}: {got:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{format}: the wait was not bounded"
        );
        assert_eq!(snapshot(tmp.path()), before);
        drop(held);
        store.set("items", "D02", json!({}), None)?;
    }
    Ok(())
}

#[test]
fn corrupt_or_partial_files_are_errors_and_never_overwritten() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let store = seeded(tmp.path(), format)?;
        let path = match format {
            Format::Dir => tmp.path().join("items/D01.json"),
            Format::Jsonl => tmp.path().join("items.jsonl"),
        };
        let mut bytes = fs::read(&path)?;
        bytes.truncate(bytes.len() - 7);
        fs::write(&path, &bytes)?;
        let before = snapshot(tmp.path());
        assert!(
            matches!(
                store.list("items", &ListQuery::default()),
                Err(DashError::Corrupt { .. })
            ),
            "{format}"
        );
        assert!(
            matches!(store.get("items", "D01"), Err(DashError::Corrupt { .. })),
            "{format}"
        );
        let write = store.update("items", "D01", json!({"x": 1}), None);
        assert!(
            matches!(write, Err(DashError::Corrupt { .. })),
            "{format}: {write:?}"
        );
        assert_eq!(
            snapshot(tmp.path()),
            before,
            "{format}: a corrupt file was overwritten"
        );
        assert!(
            store.get("goals", "G01").is_ok(),
            "{format}: another collection still reads"
        );
    }
    Ok(())
}

#[test]
fn schema_enforce_refuses_and_warn_writes() -> R {
    let schemas = tempfile::tempdir()?;
    let items = json!({"type": "object", "required": ["status"], "properties": {"status": {"enum": ["open", "done"]}}});
    fs::write(schemas.path().join("items.schema.json"), items.to_string())?;
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let enforce = Store::new(tmp.path(), format).with_schemas(
            Box::new(SchemaDir::load(schemas.path())?),
            SchemaMode::Enforce,
        );
        let got = enforce.set("items", "D01", json!({"status": "nope"}), None);
        let Err(DashError::SchemaInvalid(errors)) = got else {
            return Err(format!("{format}: want SchemaInvalid, got {got:?}").into());
        };
        assert_eq!(
            errors,
            ["items/D01: /status: \"nope\" is not one of [\"open\", \"done\"]"]
        );
        assert!(
            snapshot(tmp.path()).is_empty(),
            "{format}: enforce wrote bytes"
        );
        enforce.set("items", "D01", json!({"status": "open"}), None)?;
        let warn = Store::new(tmp.path(), format)
            .with_schemas(Box::new(SchemaDir::load(schemas.path())?), SchemaMode::Warn);
        warn.update("items", "D01", json!({"status": "nope"}), None)?;
        assert_eq!(warn.get("items", "D01")?.data["status"], json!("nope"));
    }
    Ok(())
}

#[test]
fn list_filters_orders_and_limits_like_the_prototype() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let store = Store::new(tmp.path(), format);
        let docs = [
            ("a", json!({"rank": 3, "status": "open"})),
            ("b", json!({"rank": "x", "status": "open"})),
            ("c", json!({"rank": 1.5, "status": "done"})),
            ("d", json!({"status": "open"})),
            ("e", json!({"rank": [1], "status": "open"})),
            ("f", json!({"rank": null, "status": "open"})),
            ("g", json!({"rank": 3, "status": "open"})),
        ];
        let ops: Vec<Op> = docs
            .iter()
            .map(|(id, d)| Op::Set {
                collection: "items".into(),
                id: (*id).into(),
                data: d.clone(),
                if_version: None,
            })
            .collect();
        store.batch(&ops)?;
        let ids = |q: ListQuery| -> Result<Vec<String>, DashError> {
            Ok(store.list("items", &q)?.into_iter().map(|d| d.id).collect())
        };
        let order = |desc| ListQuery {
            order_by: Some("rank".into()),
            desc,
            ..ListQuery::default()
        };
        assert_eq!(
            ids(order(false))?,
            ["c", "a", "g", "b", "e", "f", "d"],
            "{format}"
        );
        assert_eq!(
            ids(order(true))?,
            ["f", "e", "b", "a", "g", "c", "d"],
            "{format}"
        );
        let open = ListQuery {
            status: Some("open".into()),
            limit: Some(2),
            ..ListQuery::default()
        };
        assert_eq!(ids(open)?, ["a", "b"], "{format}");
        assert_eq!(store.collections()?, ["items"]);
        let _: Value = serde_json::to_value(store.get("items", "a")?)?;
    }
    Ok(())
}
