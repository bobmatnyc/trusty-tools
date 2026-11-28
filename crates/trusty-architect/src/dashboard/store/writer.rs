//! One writer class per collection (#12029 AC2).
//!
//! Why: the prototype store (`dashboard_store.py`, ported byte for byte as
//! [`super::py`]) and the #10063 [`super::Store`] take the same `flock` on
//! `<root>/.lock` but write different bytes and keep different change logs.
//! Two classes writing one collection in a parallel window corrupt it, so the
//! first class to write a collection owns it and the other is refused, in
//! code.
//! What: [`WriterClass`] names the two classes. `admit` runs under the
//! exclusive store lock: it reads the claims from the lock file, refuses the
//! batch with [`DashError::WriterClass`] when any touched collection is
//! claimed by the other class, and appends one `writer <class> <collection>`
//! line per new claim to the lock file, synced. The lock file holds the
//! claims because it is the one file every writer already opens, it is never
//! replaced (a rename would break the `flock` other waiters hold), and the
//! prototype opens it in append mode and never reads it. A claim line that
//! does not parse, a torn last line included, refuses every batch: a claim is
//! never guessed.
//! Test: `a_second_writer_class_on_one_collection_is_refused`,
//! `each_class_keeps_its_own_collections`,
//! `an_unreadable_claim_refuses_the_batch`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use super::super::error::DashError;
use super::lock::LOCK_FILE;

/// The two implementations that write a dashboard store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterClass {
    /// `dashboard_store.py` and its byte-for-byte port, [`super::py`].
    Python,
    /// The #10063 [`super::Store`].
    Rust,
}

impl WriterClass {
    /// The name a claim line carries.
    pub fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Rust => "rust",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "python" => Some(Self::Python),
            "rust" => Some(Self::Rust),
            _ => None,
        }
    }
}

/// The claims recorded in `<root>/.lock`: collection to class.
///
/// # Errors
/// [`DashError::WriterClass`] for a line that is not a claim;
/// [`DashError::Io`] when the file cannot be read.
pub(crate) fn claims(root: &Path) -> Result<BTreeMap<String, WriterClass>, DashError> {
    let path = root.join(LOCK_FILE);
    let blob = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(DashError::io(&path)(e)),
    };
    let bad = |n: usize| {
        DashError::WriterClass(format!(
            "{}: line {n} is not a writer claim; repair it before any write",
            path.display()
        ))
    };
    let text = std::str::from_utf8(&blob).map_err(|_| bad(1))?;
    let mut out = BTreeMap::new();
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    for (n, line) in lines.iter().enumerate() {
        let claim = line
            .strip_suffix('\n')
            .and_then(|l| l.strip_prefix("writer "))
            .and_then(|l| l.split_once(' '))
            .and_then(|(class, coll)| Some((WriterClass::parse(class)?, coll)))
            .filter(|(_, coll)| !coll.is_empty());
        let Some((class, coll)) = claim else {
            return Err(bad(n + 1));
        };
        // A second line for a collection never changes its owner.
        out.entry(coll.to_owned()).or_insert(class);
    }
    Ok(out)
}

/// Why: AC2 says a collection has exactly one writer class; the guard must
/// hold in code, not by convention.
/// What: refuses the batch when any of `colls` is claimed by a class other
/// than `class`; otherwise claims the unclaimed ones for `class`. The caller
/// holds the exclusive store lock, so two writers cannot both claim one
/// collection.
/// Test: `a_second_writer_class_on_one_collection_is_refused`,
/// `each_class_keeps_its_own_collections`, `an_unreadable_claim_refuses_the_batch`.
///
/// # Errors
/// [`DashError::WriterClass`] naming the collection and its owner, or an
/// unreadable claim; [`DashError::Io`] when the claim cannot be appended.
pub(crate) fn admit<'a>(
    root: &Path,
    colls: impl IntoIterator<Item = &'a str>,
    class: WriterClass,
) -> Result<(), DashError> {
    let held = claims(root)?;
    let mut new = BTreeSet::new();
    for coll in colls {
        match held.get(coll) {
            Some(owner) if *owner != class => {
                return Err(DashError::WriterClass(format!(
                    "{coll} is written by the {} writer class; the {} writer is refused \
                     (one writer class per collection)",
                    owner.name(),
                    class.name()
                )));
            }
            Some(_) => {}
            None => {
                new.insert(coll);
            }
        }
    }
    if new.is_empty() {
        return Ok(());
    }
    let text: String = new
        .iter()
        .map(|c| format!("writer {} {c}\n", class.name()))
        .collect();
    let path = root.join(LOCK_FILE);
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(DashError::io(&path))?;
    f.write_all(text.as_bytes()).map_err(DashError::io(&path))?;
    f.sync_data().map_err(DashError::io(&path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::store::py::PyStore;
    use crate::dashboard::store::{Format, Store};
    use serde_json::json;

    fn rust_store(root: &Path) -> Store {
        Store::new(root, Format::Dir).with_secret_scan(|_| None)
    }

    #[test]
    fn a_second_writer_class_on_one_collection_is_refused() -> Result<(), Box<dyn std::error::Error>>
    {
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
        let lock = std::fs::read_to_string(tmp.path().join(LOCK_FILE))?;
        assert_eq!(lock, "writer python items\nwriter rust notes\n");
        Ok(())
    }

    #[test]
    fn an_unreadable_claim_refuses_the_batch() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        for text in ["writer python items", "writer go items\n", "junk\n"] {
            std::fs::write(tmp.path().join(LOCK_FILE), text)?;
            let got = rust_store(tmp.path()).set("other", "X1", json!({}), None);
            assert!(
                matches!(got, Err(DashError::WriterClass(_))),
                "{text:?}: {got:?}"
            );
            let got = PyStore::for_test(tmp.path(), "dir").set_json("other", "X1", "{}");
            assert!(got.is_err(), "{text:?}");
        }
        assert!(!tmp.path().join("other").exists());
        Ok(())
    }
}
