//! The AC2 writer-class guard: claims, refusals, torn and failed appends (#12029).

use super::*;
use crate::dashboard::store::changes::CHANGES;
use crate::dashboard::store::py::PyStore;
use crate::dashboard::store::{Format, Store};
use serde_json::json;

/// A Rust-class store that claims what it writes.
fn rust_store(root: &Path) -> Store {
    Store::new(root, Format::Dir)
        .with_secret_scan(|_| None)
        .with_writer_claim()
}

fn lock_text(root: &Path) -> String {
    std::fs::read_to_string(root.join(LOCK_FILE)).unwrap_or_else(|_| "absent".into())
}

#[test]
fn a_second_writer_class_on_one_collection_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let py = PyStore::for_test(tmp.path(), "dir");
    py.set_json("items", "A01", r#"{"status": "open"}"#)?;
    let got = rust_store(tmp.path()).set("items", "A02", json!({"status": "open"}), None);
    assert!(matches!(got, Err(DashError::WriterClass(_))), "{got:?}");
    assert!(!tmp.path().join("items/A02.json").exists());
    // The other way round: a Rust-owned collection refuses the Python class.
    rust_store(tmp.path()).set("notes", "N01", json!({"t": 1}), None)?;
    let got = py.set_json("notes", "N02", r#"{"t": 2}"#);
    assert!(got.is_err_and(|e| e.to_string().contains("one writer class")));
    assert!(!tmp.path().join("notes/N02.json").exists());
    Ok(())
}

#[test]
fn each_class_keeps_its_own_collections() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let py = PyStore::for_test(tmp.path(), "dir");
    py.set_json("items", "A01", r#"{"status": "open"}"#)?;
    py.set_json("items", "A02", r#"{"status": "open"}"#)?;
    rust_store(tmp.path()).set("notes", "N01", json!({"t": 1}), None)?;
    rust_store(tmp.path()).set("notes", "N02", json!({"t": 2}), None)?;
    let owners = claims(tmp.path())?;
    assert_eq!(owners.get("items"), Some(&WriterClass::Python));
    assert_eq!(owners.get("notes"), Some(&WriterClass::Rust));
    assert_eq!(
        lock_text(tmp.path()),
        "writer python items\nwriter rust notes\n"
    );
    Ok(())
}

#[test]
fn an_unreadable_claim_refuses_the_batch() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let bad = [
        "writer go items\n",
        "junk\n",
        "\n",
        "writer python it ems\n",
        "writer python items\r\n",
        "writer rust items\nwriter python\n",
    ];
    for text in bad {
        std::fs::write(tmp.path().join(LOCK_FILE), text)?;
        let got = rust_store(tmp.path()).set("other", "X1", json!({}), None);
        assert!(
            matches!(got, Err(DashError::WriterClass(_))),
            "{text:?}: {got:?}"
        );
        let got = PyStore::for_test(tmp.path(), "dir").set_json("other", "X1", "{}");
        assert!(got.is_err(), "{text:?}");
        assert_eq!(lock_text(tmp.path()), text, "a refusal repairs nothing");
    }
    assert!(!tmp.path().join("other").exists());
    Ok(())
}

/// #12029 critic 1: Python's `$` accepts `items\n`; its claim would be a line
/// no later batch could read.
#[test]
fn a_collection_name_with_a_newline_is_refused_before_its_claim()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let py = PyStore::for_test(tmp.path(), "dir");
    let got = py.set_json("items\n", "A01", r#"{"t": 1}"#);
    assert!(got.is_err(), "{got:?}");
    assert!(!lock_text(tmp.path()).contains("items"));
    assert!(!tmp.path().join("items\n").exists());
    py.set_json("other", "X1", "{}")?;
    assert_eq!(lock_text(tmp.path()), "writer python other\n");
    Ok(())
}

/// #12029 critic 2: on APFS `Items` and `items` are one directory.
#[test]
fn a_case_variant_of_a_claimed_collection_is_refused_across_classes()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    rust_store(tmp.path()).set("items", "A01", json!({"t": 1}), None)?;
    let got = PyStore::for_test(tmp.path(), "dir").set_json("Items", "A02", r#"{"t": 2}"#);
    assert!(got.is_err_and(|e| e.to_string().contains("one writer class")));
    assert_eq!(lock_text(tmp.path()), "writer rust items\n");
    Ok(())
}

/// #12029 critic 3: a torn last line is not a claim, and the next claim cuts it.
#[test]
fn a_torn_last_claim_line_is_ignored_and_dropped() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::write(
        tmp.path().join(LOCK_FILE),
        "writer python items\nwriter python no",
    )?;
    PyStore::for_test(tmp.path(), "dir").set_json("notes", "N01", "{}")?;
    assert_eq!(
        lock_text(tmp.path()),
        "writer python items\nwriter python notes\n"
    );
    Ok(())
}

/// #12029 critic 3: a failed append (here half a line, then ENOSPC) leaves
/// the lock file as it was, and the next batch is admitted.
#[test]
fn a_failed_claim_append_leaves_the_lock_file_as_it_was() -> Result<(), Box<dyn std::error::Error>>
{
    let tmp = tempfile::tempdir()?;
    std::fs::write(tmp.path().join(LOCK_FILE), "writer python items\n")?;
    let mut full = |f: &mut File, b: &[u8]| {
        f.write_all(&b[..b.len() / 2])?;
        Err(std::io::Error::from_raw_os_error(libc::ENOSPC))
    };
    let got = admit_with(tmp.path(), ["notes"], WriterClass::Rust, &mut full);
    assert!(matches!(got, Err(DashError::Io { .. })), "{got:?}");
    assert_eq!(lock_text(tmp.path()), "writer python items\n");
    admit(tmp.path(), ["notes"], WriterClass::Rust)?;
    assert_eq!(
        lock_text(tmp.path()),
        "writer python items\nwriter rust notes\n"
    );
    Ok(())
}

/// #12029 critic 4: the refusal runs before `changes::prepare` creates the log.
#[test]
fn a_refused_batch_leaves_the_change_log_untouched() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::write(tmp.path().join(LOCK_FILE), "writer python items\n")?;
    let got = rust_store(tmp.path()).set("items", "A01", json!({"t": 1}), None);
    assert!(matches!(got, Err(DashError::WriterClass(_))), "{got:?}");
    assert!(!tmp.path().join(CHANGES).exists());
    Ok(())
}

/// #12029 ruling (b): the live `Store` writes no claim until P2g.
#[test]
fn the_default_store_writes_no_claim() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let live = Store::new(tmp.path(), Format::Dir).with_secret_scan(|_| None);
    live.set("items", "A01", json!({"t": 1}), None)?;
    assert_eq!(lock_text(tmp.path()), "", "the lock file stays empty");
    // A Python claim does not refuse it either: the default path never reads one.
    std::fs::write(tmp.path().join(LOCK_FILE), "writer python notes\n")?;
    live.set("notes", "N01", json!({"t": 1}), None)?;
    assert_eq!(lock_text(tmp.path()), "writer python notes\n");
    Ok(())
}
