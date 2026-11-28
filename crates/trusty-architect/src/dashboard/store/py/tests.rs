//! The port's error arms: a failed lock or replace is an error, never a warning (#12029 P2c).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::{Options, PyStore, PyStoreError};
use crate::clock::FakeClock;
use crate::pyjson_load::{PyValue, loads};

impl PyStore {
    /// A store at `root` in `fmt`, schemas off, on a frozen clock.
    pub(crate) fn for_test(root: &Path, fmt: &str) -> Self {
        let opts = Options {
            fmt: Some(fmt.into()),
            schema_mode: Some("off".into()),
            ..Options::default()
        };
        let none = |_: &str| None;
        let root = root.to_string_lossy();
        match PyStore::new(
            &root,
            opts,
            &none,
            Path::new("/"),
            "/nonexistent".into(),
            Arc::new(FakeClock::new(1_795_784_400.0)),
        ) {
            Ok(s) => s,
            Err(e) => panic!("for_test: {e}"),
        }
    }

    /// `set(coll, id, json)` with the body given as JSON text.
    pub(crate) fn set_json(&self, coll: &str, id: &str, body: &str) -> Result<(), PyStoreError> {
        let data = loads(body).map_err(|e| PyStoreError::Store(e.py_str()))?;
        let (c, i) = (PyValue::Str(coll.into()), PyValue::Str(id.into()));
        self.set(&c, &i, Some(&data), None, None).map(|_| ())
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|_| "absent".into())
}

#[test]
fn a_failed_replace_is_an_error_and_restores_the_batch() -> Result<(), Box<dyn std::error::Error>> {
    for fmt in ["dir", "jsonl"] {
        let tmp = tempfile::tempdir()?;
        let store = PyStore::for_test(tmp.path(), fmt);
        store.set_json("items", "A01", r#"{"status": "open"}"#)?;
        store.set_json("fresh", "F01", r#"{"a": 1}"#)?;
        let (items, fresh) = match fmt {
            "dir" => (tmp.path().join("items"), tmp.path().join("fresh/F01.json")),
            _ => (tmp.path().to_path_buf(), tmp.path().join("fresh.jsonl")),
        };
        let fresh_before = read(&fresh);
        let item_file = if fmt == "dir" {
            items.join("A01.json")
        } else {
            items.join("items.jsonl")
        };
        let item_before = read(&item_file);
        // The second write's temp file cannot be created: the replace never happens.
        std::fs::set_permissions(&items, std::fs::Permissions::from_mode(0o555))?;
        let ops = loads(
            r#"[{"op": "update", "collection": "fresh", "id": "F01", "data": {"a": 2}},
                {"op": "set", "collection": "items", "id": "A02", "data": {"status": "done"}}]"#,
        )
        .map_err(|e| e.py_str())?;
        let got = store.batch(&ops, None);
        std::fs::set_permissions(&items, std::fs::Permissions::from_mode(0o755))?;
        assert!(
            matches!(&got, Err(PyStoreError::Os { errno, .. }) if *errno == libc::EACCES),
            "{fmt}: {got:?}"
        );
        assert_eq!(read(&item_file), item_before, "{fmt}");
        if fmt == "dir" {
            assert!(!items.join("A02.json").exists());
            // The earlier write in another directory was restored.
            assert_eq!(read(&fresh), fresh_before, "{fmt}");
        }
        assert!(store.take_stderr().is_empty(), "{fmt}: never a warning");
    }
    Ok(())
}

#[test]
fn a_lock_that_cannot_be_taken_is_an_error_and_writes_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    // The lock file is a directory: opening it fails.
    let tmp = tempfile::tempdir()?;
    std::fs::create_dir(tmp.path().join(".lock"))?;
    let got = PyStore::for_test(tmp.path(), "dir").set_json("items", "A01", "{}");
    assert!(
        matches!(&got, Err(PyStoreError::Os { class, .. }) if class == "IsADirectoryError"),
        "{got:?}"
    );
    assert!(!tmp.path().join("items").exists());
    // A holder that never lets go: the bounded wait ends in an error.
    let tmp = tempfile::tempdir()?;
    let held = crate::ledger::store::lock(&tmp.path().join(".lock"), None)?;
    let store = PyStore::for_test(tmp.path(), "jsonl").with_lock_wait(Duration::from_millis(50));
    let got = store.set_json("items", "A01", "{}");
    drop(held);
    assert!(matches!(&got, Err(PyStoreError::Unavailable(_))), "{got:?}");
    assert!(!tmp.path().join("items.jsonl").exists());
    // The root is a file: makedirs fails before the lock.
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("afile");
    std::fs::write(&root, "x")?;
    let got = PyStore::for_test(&root, "dir").set_json("items", "A01", "{}");
    assert!(
        matches!(&got, Err(PyStoreError::Os { class, .. }) if class == "FileExistsError"),
        "{got:?}"
    );
    Ok(())
}
