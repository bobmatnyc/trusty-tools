//! Crash and failure injection for `import` (#10746 S8; SPEC-OKF-01 §2.14
//! rule 10, §4.11, §4.4 "A refused write changes no file").
//!
//! Why: an import writes many files: concepts, verification-record lines,
//! the revision journal, the ledger, `log.md` and the run file. A crash at
//! any step boundary must leave a bundle that opens, and a re-run must
//! converge to the bundle of one uninterrupted run. A failed ledger or run
//! file write must fail the run, never pass as a warning while the ledger
//! advances.
//! What: a crash hook that, once it fires, fires at every later seam (a dead
//! process does nothing more); an I/O hook for the ledger and run file.
//! Test: self-contained.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use chrono::{DateTime, FixedOffset};

use crate::OkfError;
use crate::build::ledger::Ledger;
use crate::build::policy::DocStorePolicy;
use crate::build::registry::{Locator, SourceSpec};
use crate::bundle::Bundle;
use crate::bundle::ops::fail;
use crate::extract::NoWorker;
use crate::format::id::ConceptId;
use crate::format::{Actor, Verification, content_hash};
use crate::import::{
    DocTreeSource, ImportReport, ImportSpec, exit_code, import, import_status, plan_import,
};

const T0: &str = "2026-11-04T10:00:00Z";
const T1: &str = "2026-11-04T11:00:00Z";

fn at(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn spec(src: &Path) -> ImportSpec {
    let mut store = SourceSpec::new(
        "tree",
        None,
        Locator::DocStore {
            path: src.to_string_lossy().into_owned(),
            extensions: Vec::new(),
            recursive: true,
        },
        T0,
    );
    store.tombstone_deleted = true;
    ImportSpec::new(store)
}

fn run(base: &Path, when: &str) -> Result<ImportReport, OkfError> {
    let (src, root) = (base.join("src"), base.join("kb"));
    let policy = DocStorePolicy::new(vec![src.clone()]);
    let mut source = DocTreeSource::open(&src, &policy)?;
    let plan = plan_import(&Bundle::open(&root)?.0, &spec(&src), &mut source)?;
    import(
        &mut Bundle::open(&root)?.0,
        &plan,
        &mut source,
        &NoWorker,
        at(when),
    )
}

/// Files outside `_sources/`, with their bytes.
fn concepts(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in walkdir::WalkDir::new(root).sort_by_file_name() {
        let e = e.unwrap();
        let rel = e
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if e.file_type().is_file() && !rel.starts_with("_sources") {
            out.insert(rel, std::fs::read(e.path()).unwrap());
        }
    }
    out
}

/// Each ledger item's effective fingerprint, concept and status.
fn ledger(root: &Path) -> BTreeMap<String, String> {
    Ledger::load(root, "tree")
        .unwrap()
        .records()
        .map(|r| {
            (
                r.item_id.clone(),
                format!("{}|{}|{:?}", r.fingerprint, r.entity, r.status),
            )
        })
        .collect()
}

/// The verification events per record file, as a sorted multiset; the
/// operation bookkeeping (`op`, `step`) is dropped, since an interrupted
/// and an uninterrupted run name different operations (criterion 107).
fn events(root: &Path) -> BTreeMap<String, Vec<String>> {
    let dir = root.join("_sources/verification");
    let mut out = BTreeMap::new();
    if !dir.exists() {
        return out;
    }
    for e in walkdir::WalkDir::new(&dir).sort_by_file_name() {
        let e = e.unwrap();
        if e.file_type().is_file() {
            let text = std::fs::read_to_string(e.path()).unwrap();
            let mut lines: Vec<String> = text
                .lines()
                .map(|l| {
                    let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
                    let m = v.as_object_mut().unwrap();
                    m.remove("op");
                    m.remove("step");
                    v.to_string()
                })
                .collect();
            lines.sort();
            out.insert(
                e.path().strip_prefix(&dir).unwrap().display().to_string(),
                lines,
            );
        }
    }
    out
}

fn copy(from: &Path, to: &Path) {
    for e in walkdir::WalkDir::new(from) {
        let e = e.unwrap();
        let rel = e.path().strip_prefix(from).unwrap();
        let dst = to.join(rel);
        if e.file_type().is_dir() {
            std::fs::create_dir_all(&dst).unwrap();
        } else {
            // Keep the mtime: it is the imported `source_timestamp`.
            std::fs::copy(e.path(), &dst).unwrap();
            let modified = e.metadata().unwrap().modified().unwrap();
            let f = std::fs::File::options().write(true).open(&dst).unwrap();
            f.set_modified(modified).unwrap();
        }
    }
}

/// A source and bundle after a first import and a verification, with the
/// source then changed: `a.md` edited (its concept is verified, so the
/// rewrite is a multi-file operation), `b.txt` edited, `d.md` added and
/// `sub/c.md` removed (a tombstone).
fn base() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let (src, root) = (base.join("src"), base.join("kb"));
    std::fs::create_dir_all(&root).unwrap();
    write(&src, "a.md", "---\ntitle: Alpha\n---\nAlpha one.\n");
    write(&src, "b.txt", "Beta one.\n");
    write(&src, "sub/c.md", "Gamma.\n");
    run(&base, T0).unwrap();
    let mut b = Bundle::open(&root).unwrap().0;
    let a = ConceptId::parse("a").unwrap();
    let seen = content_hash(&b.get(&a).unwrap());
    let v = Verification {
        by: Actor::Process("reviewer".into()),
        at: at(T0),
        stale_after: None,
    };
    b.verify(&a, v, &seen).unwrap();
    write(&src, "a.md", "---\ntitle: Alpha\n---\nAlpha two.\n");
    write(&src, "b.txt", "Beta two.\n");
    write(&src, "d.md", "Delta.\n");
    // Moved out of the source tree: the file vanishes from the listing.
    std::fs::rename(src.join("sub/c.md"), base.join("c.md.gone")).unwrap();
    (tmp, base)
}

/// Every seam name one uninterrupted second import passes, in order.
fn seams(base: &Path) -> Vec<String> {
    let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
    let s = seen.clone();
    fail::set(move |n| {
        s.borrow_mut().push(n.to_string());
        false
    });
    let got = run(base, T1);
    fail::clear();
    got.unwrap();
    seen.borrow().clone()
}

/// Why: §4.11 and §2.14 rule 10 — a crash at any step boundary of an
/// import leaves a bundle that opens, keeps every verification event, and a
/// re-run converges to the uninterrupted bundle (criterion 83's engine half,
/// point 4 of the S8 brief).
/// What: for each seam of the second import, a fresh copy crashes there
/// (every later seam fails too), then `Bundle::open` recovers and the
/// import re-runs at the same `now`; the files outside `_sources/`, the
/// ledger and the verification record equal the uninterrupted run's.
/// Test: self-contained.
#[test]
fn crash_at_every_import_boundary_recovers_to_the_uninterrupted_bundle() {
    let (_t, b0) = base();
    let ref_dir = tempfile::tempdir().unwrap();
    let reference = ref_dir.path().canonicalize().unwrap();
    copy(&b0, &reference);
    let names = seams(&reference);
    eprintln!("{} crash seams: {names:?}", names.len());
    assert!(names.iter().any(|n| n == "import-item"), "{names:?}");
    assert!(
        names.iter().any(|n| n == "intent"),
        "a multi-file write ran: {names:?}"
    );
    let want = (
        concepts(&reference.join("kb")),
        ledger(&reference.join("kb")),
        events(&reference.join("kb")),
    );
    for (k, name) in names.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        copy(&b0, &base);
        let hits = Rc::new(Cell::new(0usize));
        let h = hits.clone();
        fail::set(move |_| {
            h.set(h.get() + 1);
            h.get() > k
        });
        let crashed = run(&base, T1);
        fail::clear();
        assert!(crashed.is_err(), "seam {k} {name}: the run must stop");
        let root = base.join("kb");
        Bundle::open(&root).unwrap_or_else(|e| panic!("seam {k} {name}: open {e}"));
        let again = run(&base, T1).unwrap_or_else(|e| panic!("seam {k} {name}: re-run {e}"));
        assert_eq!(again.exit_code, 0, "seam {k} {name}: {again:?}");
        let got = (concepts(&root), ledger(&root), events(&root));
        let differ: Vec<&String> = want
            .0
            .keys()
            .chain(got.0.keys())
            .filter(|f| got.0.get(*f) != want.0.get(*f))
            .collect();
        assert!(
            differ.is_empty(),
            "seam {k} {name}: files differ: {differ:?}"
        );
        assert_eq!(got.1, want.1, "seam {k} {name}: ledger differs");
        assert_eq!(got.2, want.2, "seam {k} {name}: verification events differ");
    }
}

/// Why: criterion 83 — a run killed after some items, then re-run, yields
/// the uninterrupted bundle, and `import status` lists the killed run as
/// interrupted.
/// What: the crash fires at the second `import-item` seam (one item
/// written), then a re-run; status lists the killed attempt interrupted and
/// the re-run finished.
/// Test: self-contained.
#[test]
fn c83_killed_run_reruns_to_the_same_bundle_and_reads_interrupted() {
    let (_t, b0) = base();
    let ref_dir = tempfile::tempdir().unwrap();
    let reference = ref_dir.path().canonicalize().unwrap();
    copy(&b0, &reference);
    run(&reference, T1).unwrap();
    let base = b0.clone();
    let seen = Rc::new(Cell::new(0usize));
    let s = seen.clone();
    fail::set(move |n| {
        if n == "import-item" {
            s.set(s.get() + 1);
        }
        s.get() >= 2
    });
    let killed = run(&base, T1);
    fail::clear();
    assert!(killed.is_err());
    let root = base.join("kb");
    let (_, runs) = import_status(&Bundle::open(&root).unwrap().0, None, None).unwrap();
    let interrupted: Vec<_> = runs.iter().filter(|r| r.interrupted).collect();
    assert_eq!(interrupted.len(), 1, "{runs:?}");
    run(&base, T1).unwrap();
    let (_, runs) = import_status(&Bundle::open(&root).unwrap().0, None, None).unwrap();
    assert_eq!(runs.iter().filter(|r| r.interrupted).count(), 1, "{runs:?}");
    assert_eq!(
        runs.iter().filter(|r| !r.interrupted).count(),
        2,
        "{runs:?}"
    );
    assert_eq!(concepts(&root), concepts(&reference.join("kb")));
    assert_eq!(ledger(&root), ledger(&reference.join("kb")));
}

/// Why: Fail-Open Check — a ledger append that fails after a concept write
/// must end the run with an error (§4.10 exit 1), and the ledger must not
/// record the item; the next run converges.
/// What: an I/O hook fails the `import-ledger` seam once.
/// Test: self-contained.
#[test]
fn ledger_append_failure_ends_the_run_and_records_nothing() {
    let (_t, base) = base();
    let root = base.join("kb");
    let before = ledger(&root);
    let fired = Rc::new(Cell::new(false));
    let f = fired.clone();
    fail::set_io(move |n| {
        let hit = n == "import-ledger" && !f.get();
        if hit {
            f.set(true);
        }
        hit
    });
    let got = run(&base, T1);
    fail::clear();
    assert!(got.is_err(), "{got:?}");
    assert_eq!(exit_code(&got), 1);
    assert_eq!(
        ledger(&root),
        before,
        "the ledger advanced past a failed append"
    );
    let (_, runs) = import_status(&Bundle::open(&root).unwrap().0, None, None).unwrap();
    assert!(runs.iter().any(|r| r.interrupted), "{runs:?}");
    let again = run(&base, T1).unwrap();
    assert_eq!(again.exit_code, 0, "{again:?}");
}

/// Why: Fail-Open Check — a finish line that cannot be written is a run
/// error, not a warning; the attempt reads interrupted.
/// What: an I/O hook fails the `import-finish` seam.
/// Test: self-contained.
#[test]
fn run_file_failure_is_a_run_error() {
    let (_t, base) = base();
    fail::set_io(|n| n == "import-finish");
    let got = run(&base, T1);
    fail::clear();
    assert!(got.is_err(), "{got:?}");
    assert_eq!(exit_code(&got), 1);
    let root = base.join("kb");
    let (_, runs) = import_status(&Bundle::open(&root).unwrap().0, None, None).unwrap();
    assert!(runs.iter().any(|r| r.interrupted), "{runs:?}");
}
