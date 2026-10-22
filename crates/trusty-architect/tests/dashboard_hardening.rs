//! #10063 PR 3: `fsck` dangling references (DOC-82 LDG-2.2.5), the change
//! log's first batch and legacy baseline, symlinked parents on every write
//! path, and the method names DOC-82 gives the daemon (owner ruling D34).

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use trusty_architect::dashboard::store::{dir, jsonl};
use trusty_architect::dashboard::{DashError, DeleteArchive, Format, Op, PruneLog, Removed, Store};

type R = Result<(), Box<dyn std::error::Error>>;

const FORMATS: [Format; 2] = [Format::Dir, Format::Jsonl];

fn no_secret(_: &str) -> Option<String> {
    None
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// A store at `<tmp>/store` with its prune log at `<tmp>/archive/pruned.jsonl`.
fn store(tmp: &Path, format: Format) -> Result<(Store, PruneLog), DashError> {
    let root = tmp.join("store");
    let prune = PruneLog::outside(&root, tmp.join("archive/pruned.jsonl"))?;
    let s = Store::new(&root, format)
        .with_secret_scan(no_secret)
        .with_archive(Box::new(prune.clone()))
        .with_actor("architect");
    Ok((s, prune))
}

/// Writes `docs` of `coll` by hand, as a store the Rust writer never saw.
fn write_legacy(root: &Path, format: Format, coll: &str, docs: &[(&str, Value)]) -> R {
    match format {
        Format::Dir => {
            fs::create_dir_all(root.join(coll))?;
            for (id, data) in docs {
                let path = dir::doc_path(root, coll, id);
                fs::write(path, dir::render(1, &obj(data.clone())))?;
            }
        }
        Format::Jsonl => {
            fs::create_dir_all(root)?;
            let all: jsonl::Docs = docs
                .iter()
                .map(|(id, data)| ((*id).to_owned(), (1, obj(data.clone()))))
                .collect();
            fs::write(jsonl::coll_path(root, coll), jsonl::render_file(&all))?;
        }
    }
    Ok(())
}

/// LDG-2.2.5: a live record naming an id no document holds, in `goal`,
/// `depends_on` or `links`, is reported; terminal records and non-id text
/// are not.
#[test]
fn fsck_reports_dangling_links_depends_on_and_goal() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let (s, _) = store(tmp.path(), format)?;
        s.set("goals", "G01", json!({"title": "g"}), None)?;
        s.set(
            "items",
            "D01",
            json!({"status": "open", "goal": "G01"}),
            None,
        )?;
        s.set(
            "items",
            "D02",
            json!({"status": "open", "goal": "G09"}),
            None,
        )?;
        s.set(
            "items",
            "D03",
            json!({"status": "open", "depends_on": ["D01", "T07"]}),
            None,
        )?;
        let links = json!(["D01", "A0f", {"label": "x", "item": "G0a"},
            {"label": "web", "url": "https://example.com"}, "free text"]);
        s.set(
            "items",
            "D04",
            json!({"status": "open", "links": links}),
            None,
        )?;
        s.set(
            "items",
            "D05",
            json!({"status": "done", "goal": "G0e", "depends_on": ["T0e"]}),
            None,
        )?;
        let want = [
            "items/D02: /goal: G09 is not a goal id in the store",
            "items/D03: /depends_on/1: T07 is not an id in the store",
            "items/D04: /links/1: A0f is not an id in the store",
            "items/D04: /links/2/item: G0a is not an id in the store",
        ];
        assert_eq!(s.fsck()?, want, "{format}");
    }
    Ok(())
}

/// Legacy documents, written before the store had a change log, are a
/// baseline the first Rust write records; a later hand edit is still found.
#[test]
fn fsck_accepts_a_legacy_baseline_after_the_first_write() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let (s, _) = store(tmp.path(), format)?;
        let root = s.root().to_path_buf();
        let legacy = [("L01", json!({"t": "one"})), ("L02", json!({"t": "two"}))];
        write_legacy(&root, format, "items", &legacy)?;
        assert_eq!(s.fsck()?, Vec::<String>::new(), "{format}: before");
        s.set("items", "N01", json!({"t": "new"}), None)?;
        assert_eq!(s.fsck()?, Vec::<String>::new(), "{format}: after");
        let edited = [
            ("L01", json!({"t": "hand edit"})),
            ("L02", json!({"t": "two"})),
        ];
        let mut all = edited.to_vec();
        all.push(("N01", json!({"t": "new"})));
        write_legacy(&root, format, "items", &all)?;
        let problems = s.fsck()?;
        assert!(
            problems.iter().any(|p| p.starts_with("items/L01: ")),
            "{format}: a hand edit after the baseline is found: {problems:?}"
        );
    }
    Ok(())
}

/// Records the batch's deletes, keeps what the change log holds now, then
/// puts a directory where `changes.jsonl` was, so the commit runs and the
/// append after it fails.
struct BreakLog {
    prune: PruneLog,
    log: PathBuf,
    kept: Kept,
}

/// What the change log held when [`BreakLog`] broke it.
type Kept = std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>;

impl DeleteArchive for BreakLog {
    fn record(&self, removed: &[Removed<'_>]) -> Result<(), DashError> {
        self.prune.record(removed)?;
        let bytes = fs::read(&self.log).ok();
        if let Ok(mut kept) = self.kept.lock() {
            *kept = bytes;
        }
        let _ = fs::remove_file(&self.log);
        fs::create_dir(&self.log).map_err(|e| DashError::Invalid(format!("break log: {e}")))
    }
}

/// The first batch on a store with no change log commits but its append
/// fails: `fsck` must still name the document written unlogged.
#[test]
fn fsck_reports_a_first_batch_committed_with_no_change_log() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let (_, prune) = store(tmp.path(), format)?;
        let root = tmp.path().join("store");
        write_legacy(&root, format, "items", &[("L01", json!({"t": "old"}))])?;
        let log = root.join("changes.jsonl");
        let kept = Kept::default();
        let breaker = BreakLog {
            prune,
            log: log.clone(),
            kept: kept.clone(),
        };
        let s = Store::new(&root, format)
            .with_secret_scan(no_secret)
            .with_archive(Box::new(breaker));
        let got = s.batch(&[
            Op::Delete {
                collection: "items".into(),
                id: "L01".into(),
                if_version: None,
            },
            Op::Set {
                collection: "items".into(),
                id: "B01".into(),
                data: json!({"t": "new"}),
                if_version: None,
            },
        ]);
        assert!(
            matches!(got, Err(DashError::Unlogged { .. })),
            "{format}: {got:?}"
        );
        // The operator puts back what the log held, as `Unlogged` says.
        fs::remove_dir(&log)?;
        let held = kept.lock().map_err(|_| "poisoned")?.clone();
        if let Some(bytes) = held {
            fs::write(&log, bytes)?;
        }
        let problems = Store::new(&root, format).fsck()?;
        assert!(
            problems.contains(&"items/B01: present but no change line".to_owned()),
            "{format}: the unlogged first batch is invisible: {problems:?}"
        );
    }
    Ok(())
}

/// A prune log whose parent directory became a symlink into the served root
/// after it was checked is never written through it.
#[test]
fn delete_refuses_a_prune_log_whose_parent_became_a_symlink() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("store");
        let prune = PruneLog::outside(&root, tmp.path().join("archive/sub/pruned.jsonl"))?;
        let s = Store::new(&root, format)
            .with_secret_scan(no_secret)
            .with_archive(Box::new(prune));
        s.set("items", "A01", json!({"n": 1}), None)?;
        symlink(&root, tmp.path().join("archive"))?;
        let got = s.delete("items", "A01", None);
        assert!(
            got.is_err(),
            "{format}: delete wrote through a link: {got:?}"
        );
        assert!(
            !root.join("sub").exists(),
            "{format}: the prune log leaked into the served root"
        );
        assert_eq!(s.get("items", "A01")?.data["n"], 1, "{format}: deleted");
    }
    Ok(())
}

/// A collection directory that is a symlink is never written through.
#[test]
fn write_refuses_a_collection_directory_that_is_a_symlink() -> R {
    let tmp = tempfile::tempdir()?;
    let (s, _) = store(tmp.path(), Format::Dir)?;
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside)?;
    fs::create_dir_all(s.root())?;
    symlink(&outside, s.root().join("items"))?;
    let got = s.set("items", "A01", json!({"n": 1}), None);
    assert!(got.is_err(), "write followed a symlinked parent: {got:?}");
    assert_eq!(
        fs::read_dir(&outside)?.count(),
        0,
        "a file was written outside"
    );
    Ok(())
}

/// A `changes.jsonl` that is a symlink is never appended through, and the
/// batch is refused before any document is written.
#[test]
fn write_refuses_a_change_log_that_is_a_symlink() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let (s, _) = store(tmp.path(), format)?;
        s.set("items", "A01", json!({"n": 1}), None)?;
        let outside = tmp.path().join("outside.jsonl");
        let log = s.root().join("changes.jsonl");
        fs::rename(&log, &outside)?;
        symlink(&outside, &log)?;
        let before = fs::read(&outside)?;
        let got = s.set("items", "A02", json!({"n": 2}), None);
        assert!(got.is_err(), "{format}: appended through a link: {got:?}");
        assert_eq!(
            fs::read(&outside)?,
            before,
            "{format}: outside file changed"
        );
        assert!(
            matches!(s.get("items", "A02"), Err(DashError::NotFound(_))),
            "{format}: the document was written"
        );
    }
    Ok(())
}

/// DOC-82 names the daemon methods with the router's `mpm.` prefix (owner
/// ruling D34), as the code serves them.
#[test]
fn doc82_names_the_dashboard_methods_with_the_mpm_prefix() -> R {
    let spec = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md");
    let text = fs::read_to_string(spec)?;
    let row = "`mpm.architect.dashboard.get/list/set/update/delete/batch/pruned/validate/fsck/tally/footer/ids`";
    assert!(text.contains(row), "DOC-82 section 3.3 does not name {row}");
    assert!(
        !text.contains("| `architect.dashboard."),
        "DOC-82 still names the unprefixed methods"
    );
    Ok(())
}
