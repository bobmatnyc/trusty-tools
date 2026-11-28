//! Error arms of the `dashboard_attach.py` port (#12029 P2h).

use std::cell::Cell;
use std::os::unix::fs::symlink;
use std::path::Path;

use super::dirs::{attach_dir, safe_dir};
use super::nfc::nfc;
use super::save::save;
use super::{AttachError, MAX_PER_ITEM, Upload, clean_name, handle_upload, max_bytes, record};
use crate::dashboard::pyupdates::ItemStore;
use crate::dashboard::store::py::{PyDocOut, PyStore, PyStoreError};
use crate::pyjson_load::{PyInt, PyValue, loads};

type R = Result<(), Box<dyn std::error::Error>>;

fn ls(p: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(p)
        .map(|r| {
            r.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Expected values printed by Python 3.14.8 `unicodedata.normalize("NFC", s)`.
#[test]
fn nfc_matches_python_on_composed_decomposed_and_singleton_text() {
    let cases = [
        ("e\u{301}", "\u{e9}"),
        ("A\u{328}\u{301}", "\u{104}\u{301}"),
        ("x\u{301}\u{328}y", "x\u{328}\u{301}y"),
        ("\u{212a}", "K"),
        ("\u{2126}", "\u{3a9}"),
        ("\u{37e}", ";"),
        ("\u{1100}\u{1161}\u{11a8}", "\u{ac01}"),
        ("\u{1100}\u{1161}", "\u{ac00}"),
        ("\u{ac00}\u{11a8}", "\u{ac01}"),
        ("\u{958}", "\u{915}\u{93c}"),
        ("a\u{344}b", "\u{e4}\u{301}b"),
        ("\u{1e9b}\u{323}", "\u{1e9b}\u{323}"),
        ("q\u{307}\u{323}", "q\u{323}\u{307}"),
        ("\u{c5}\u{301}", "\u{1fa}"),
        ("A\u{30a}", "\u{c5}"),
        ("\u{fb01}", "\u{fb01}"),
        ("\u{f73}", "\u{f71}\u{f72}"),
        ("\u{345}\u{301}a", "\u{301}\u{345}a"),
        ("\u{1fef}", "`"),
        ("\u{300}a", "\u{300}a"),
        ("e\u{301}\u{301}", "\u{e9}\u{301}"),
        ("\u{5d0}\u{5b8}\u{5b7}", "\u{5d0}\u{5b7}\u{5b8}"),
    ];
    for (raw, want) in cases {
        assert_eq!(nfc(raw), want, "{raw:?}");
    }
}

/// Expected values printed by the frozen `clean_name`.
#[test]
fn clean_name_follows_python() {
    let ok = [
        ("e\u{301}.txt", "_.txt"),
        ("\u{212a}.txt", "K.txt"),
        (" .hidden. ", "hidden"),
        ("  \u{a0}pad\u{3000} ", "pad"),
        ("q[1]{2}<3>.log", "q_1__2__3_.log"),
    ];
    for (raw, want) in ok {
        assert_eq!(clean_name(Some(raw)).as_deref(), Ok(want), "{raw:?}");
    }
    let long = format!("{}.verylongextension12345", "b".repeat(120));
    let got = clean_name(Some(&long)).unwrap_or_default();
    assert_eq!(got.len(), 100);
    assert!(got.ends_with(".verylongextensi"), "{got}");
    for raw in [
        None,
        Some("  "),
        Some("a/b"),
        Some("a..b"),
        Some("a\u{7f}"),
        Some("C:x"),
        Some("..."),
    ] {
        assert!(clean_name(raw).is_err(), "{raw:?}");
    }
}

/// Fail-open parity (#12029 P2h Q6): an unusable `DASH_ATTACH_MAX_MB` is 25 MiB.
#[test]
fn max_bytes_falls_back_to_25_mib() {
    for v in ["abc", "", "0", "-3", "nan"] {
        let env = |_: &str| Some(v.to_owned());
        assert_eq!(max_bytes(&env), 25 << 20, "{v:?}");
    }
    let env = |_: &str| Some("2".to_owned());
    assert_eq!(max_bytes(&env), 2 << 20);
    assert_eq!(max_bytes(&|_: &str| None), 25 << 20);
}

/// Fail-open parity (#12029 P2h Q6): a missing or unreadable schedule is the Architect fallback.
#[test]
fn attach_dir_follows_the_schedule_and_falls_back() -> R {
    let tmp = tempfile::tempdir()?;
    let cwd = tmp.path();
    std::fs::create_dir_all(cwd.join("proj/.trusty-mpm/il"))?;
    let sched = cwd.join("s.json");
    std::fs::write(
        &sched,
        r#"{"sessions": [{"name": "pm-a", "project": "proj"}]}"#,
    )?;
    let root = cwd.to_string_lossy();
    let (dir, il) = attach_dir(Some("pm-a"), &sched, "arch", None, cwd);
    assert_eq!(
        (dir, il),
        (format!("{root}/proj/.trusty-mpm/il/attachments"), "project")
    );
    let fallback = (format!("{root}/arch/attachments"), "architect-fallback");
    std::fs::write(&sched, "{broken")?;
    assert_eq!(
        attach_dir(Some("pm-a"), &sched, "arch", None, cwd),
        fallback
    );
    assert_eq!(
        attach_dir(Some("pm-a"), &cwd.join("absent.json"), "arch", None, cwd),
        fallback
    );
    Ok(())
}

#[test]
fn a_symlinked_item_dir_is_refused_and_nothing_is_written() -> R {
    let tmp = tempfile::tempdir()?;
    let real = tmp.path().join("real");
    std::fs::create_dir(&real)?;
    let base = tmp.path().join("att");
    std::fs::create_dir(&base)?;
    symlink(&real, base.join("D01"))?;
    let got = safe_dir(&base.to_string_lossy(), "D01");
    assert!(
        matches!(&got, Err(PyStoreError::Store(m)) if m.contains("not a plain directory")),
        "{got:?}"
    );
    let linked = tmp.path().join("linked");
    symlink(&real, &linked)?;
    let mut body: &[u8] = b"hello";
    let got = save(
        &mut body,
        5,
        "a.txt",
        &linked.join("x").to_string_lossy(),
        "D01",
        100,
    );
    assert!(got.is_err(), "{got:?}");
    assert!(ls(&real).is_empty(), "{:?}", ls(&real));
    Ok(())
}

#[test]
fn an_oversize_body_is_refused_and_nothing_is_written() -> R {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().join("att");
    let mut body: &[u8] = b"hello";
    let got = save(&mut body, 5, "a.txt", &dir.to_string_lossy(), "D01", 4);
    assert_eq!(
        got,
        Err(AttachError::TooLarge(
            "file is 5 bytes; the limit is 4".into()
        ))
    );
    assert!(!dir.exists());
    let mut short: &[u8] = b"hi";
    let got = save(&mut short, 5, "a.txt", &dir.to_string_lossy(), "D01", 100);
    assert!(got.is_err());
    assert!(
        ls(&dir.join("D01")).is_empty(),
        "{:?}",
        ls(&dir.join("D01"))
    );
    Ok(())
}

#[test]
fn save_links_without_overwriting_and_numbers_collisions() -> R {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().to_string_lossy().into_owned();
    let item = tmp.path().join("D01");
    std::fs::create_dir(&item)?;
    std::fs::write(item.join("a.txt"), "old")?;
    for n in 2..=999 {
        std::fs::write(item.join(format!("a-{n}.txt")), "old")?;
    }
    let mut body: &[u8] = b"new";
    let got = save(&mut body, 3, "a.txt", &dir, "D01", 100);
    assert_eq!(
        got,
        Err(AttachError::Store(PyStoreError::Store(
            "too many files with this name".into()
        )))
    );
    std::fs::remove_file(item.join("a-500.txt"))?;
    let mut body: &[u8] = b"new";
    let saved = save(&mut body, 3, "a.txt", &dir, "D01", 100)?;
    assert_eq!(saved.stored, "a-500.txt");
    assert_eq!(std::fs::read_to_string(item.join("a.txt"))?, "old");
    assert!(!ls(&item).iter().any(|n| n.starts_with(".upload-")));
    Ok(())
}

/// A store fake: `get` answers one item; `update` fails as told.
struct Fake {
    data: PyValue,
    conflicts: Cell<usize>,
    fail: Option<PyStoreError>,
    gets: Cell<usize>,
}

impl ItemStore for Fake {
    fn list_items(&self) -> Result<Vec<PyDocOut>, PyStoreError> {
        Ok(Vec::new())
    }
    fn get_item(&self, id: &str) -> Result<PyDocOut, PyStoreError> {
        self.gets.set(self.gets.get() + 1);
        Ok(PyDocOut {
            id: id.into(),
            version: PyInt::Small(1),
            data: self.data.clone(),
        })
    }
    fn update_item(
        &self,
        _: &str,
        _: &PyValue,
        _: &PyInt,
        _: Option<&str>,
    ) -> Result<PyInt, PyStoreError> {
        if self.conflicts.get() > 0 {
            self.conflicts.set(self.conflicts.get() - 1);
            return Err(PyStoreError::Conflict("stale".into()));
        }
        match &self.fail {
            Some(e) => Err(e.clone()),
            None => Ok(PyInt::Small(2)),
        }
    }
    fn now_iso(&self) -> String {
        "2026-11-27T13:00:00Z".into()
    }
}

fn fake(data: &str, conflicts: usize, fail: Option<PyStoreError>) -> Fake {
    Fake {
        data: loads(data).unwrap_or(PyValue::Null),
        conflicts: Cell::new(conflicts),
        fail,
        gets: Cell::new(0),
    }
}

#[test]
fn record_retries_five_conflicts_then_gives_up() {
    let entry = PyValue::Dict(Vec::new());
    let four = fake("{}", 4, None);
    assert_eq!(record(&four, "D01", &entry, None), Ok(PyInt::Small(2)));
    assert_eq!(four.gets.get(), 5);
    let five = fake("{}", 5, None);
    let got = record(&five, "D01", &entry, None);
    assert_eq!(
        got,
        Err(AttachError::Store(PyStoreError::Conflict(
            "item changed repeatedly; retry the upload".into()
        )))
    );
}

fn upload<'a>(
    body: &'a mut &'a [u8],
    cwd: &'a Path,
    env: &'a dyn Fn(&str) -> Option<String>,
) -> Upload<'a> {
    let length = i64::try_from(body.len()).unwrap_or(0);
    Upload {
        item_id: "D01",
        raw_name: Some("a.txt"),
        body,
        length,
        schedule: Path::new("/nonexistent/il-schedule.json"),
        architect_il: "arch",
        store_root: None,
        actor: None,
        env,
        cwd,
    }
}

#[test]
fn a_failed_record_removes_the_saved_file() -> R {
    let tmp = tempfile::tempdir()?;
    let store = fake(
        r#"{"kind": "question", "status": "open"}"#,
        0,
        Some(PyStoreError::Store("refused".into())),
    );
    let env = |_: &str| None;
    let mut body: &[u8] = b"hello";
    let got = handle_upload(&store, upload(&mut body, tmp.path(), &env));
    assert_eq!(
        got,
        Err(AttachError::Store(PyStoreError::Store("refused".into())))
    );
    let dir = tmp.path().join("arch/attachments/D01");
    assert!(dir.is_dir(), "the item dir is made before the save");
    assert!(ls(&dir).is_empty(), "{:?}", ls(&dir));
    Ok(())
}

#[test]
fn a_21st_attachment_is_refused_and_nothing_is_written() -> R {
    let tmp = tempfile::tempdir()?;
    let list: Vec<String> = (0..MAX_PER_ITEM)
        .map(|i| format!("{{\"name\": \"f{i}\"}}"))
        .collect();
    let data = format!(
        r#"{{"kind": "question", "status": "open", "attachments": [{}]}}"#,
        list.join(",")
    );
    let store = fake(&data, 0, None);
    let env = |_: &str| None;
    let mut body: &[u8] = b"hello";
    let got = handle_upload(&store, upload(&mut body, tmp.path(), &env));
    let refused = AttachError::Store(PyStoreError::Store(
        "at most 20 attachments per item".into(),
    ));
    assert_eq!(got, Err(refused.clone()));
    assert!(!tmp.path().join("arch").exists());
    assert_eq!(record(&store, "D01", &PyValue::Null, None), Err(refused));
    // The real store: the item keeps its twenty.
    let st = PyStore::for_test(&tmp.path().join("q"), "dir");
    st.set_json("items", "D01", &data)?;
    let got = record(&st, "D01", &PyValue::Null, None);
    assert!(got.is_err(), "{got:?}");
    assert_eq!(st.get_item("D01")?.version, PyInt::Small(1));
    Ok(())
}
