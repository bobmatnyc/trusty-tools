//! #10063 PR 2: the change log, the prune log, `fsck`, store-wide
//! `validate` and the secret refusal, in both formats (DOC-82 LDG-2.3.5,
//! LDG-2.8.3, LDG-2.8.4).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use trusty_architect::dashboard::{DashError, Format, Op, OpResult, PruneLog, SchemaSet, Store};
use trusty_architect::{Clock, FakeClock};

type R = Result<(), Box<dyn std::error::Error>>;

const FORMATS: [Format; 2] = [Format::Dir, Format::Jsonl];

/// 2026-10-21T10:00:00Z.
const NOW: f64 = 1_792_576_800.0;
const NOW_ISO: &str = "2026-10-21T10:00:00Z";

fn no_secret(_: &str) -> Option<String> {
    None
}

/// A test scanner: a `ghp_` token with 16+ trailing characters.
fn ghp(text: &str) -> Option<String> {
    let at = text.find("ghp_")?;
    let run = text[at + 4..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .count();
    (run >= 16).then(|| "ghp_…".to_owned())
}

/// A store under `<tmp>/store` with its prune log at `<tmp>/archive/pruned.jsonl`.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    prune: PruneLog,
    clock: Arc<FakeClock>,
    format: Format,
}

impl Fixture {
    fn new(format: Format) -> Result<Self, Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("store");
        let prune = PruneLog::outside(&root, tmp.path().join("archive/pruned.jsonl"))?;
        Ok(Self {
            _tmp: tmp,
            root,
            prune,
            clock: Arc::new(FakeClock::new(NOW)),
            format,
        })
    }

    fn store(&self) -> Store {
        Store::new(&self.root, self.format)
            .with_secret_scan(no_secret)
            .with_archive(Box::new(self.prune.clone()))
            .with_clock(self.clock.clone())
            .with_actor("architect")
    }

    fn log(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }

    fn lines(&self, name: &str) -> Result<Vec<Value>, serde_json::Error> {
        self.log(name).lines().map(serde_json::from_str).collect()
    }

    fn doc_path(&self, coll: &str, id: &str) -> PathBuf {
        match self.format {
            Format::Dir => self.root.join(coll).join(format!("{id}.json")),
            Format::Jsonl => self.root.join(format!("{coll}.jsonl")),
        }
    }
}

fn seqs(lines: &[Value]) -> Vec<u64> {
    lines.iter().filter_map(|l| l["seq"].as_u64()).collect()
}

#[test]
fn change_line_hashes_the_written_bytes() -> R {
    for (format, sha) in [
        (
            Format::Dir,
            "f06291bf64f5c11f74f3fd42c0e7070bcbffeb0df06c43e72b630c9a63141beb",
        ),
        (
            Format::Jsonl,
            "692e7b5f99338dbca5580fa7ee86a874b9874ca76fd8f0d4a9b6cae58d632069",
        ),
    ] {
        let f = Fixture::new(format)?;
        let s = f.store();
        s.set("items", "A01", json!({"title": "t"}), Some(0))?;
        let want = format!(
            "{{\"by\":\"architect\",\"collection\":\"items\",\"id\":\"A01\",\"keys\":[\"title\"],\
             \"op\":\"set\",\"seq\":1,\"sha256\":\"{sha}\",\"ts\":\"{NOW_ISO}\",\"version\":1}}\n"
        );
        assert_eq!(f.log("changes.jsonl"), want, "{format}");
        s.update(
            "items",
            "A01",
            json!({"status": "open", "title": "t"}),
            Some(1),
        )?;
        s.delete("items", "A01", Some(2))?;
        let lines = f.lines("changes.jsonl")?;
        assert_eq!(seqs(&lines), [1, 2, 3], "{format}");
        assert_eq!(lines[1]["keys"], json!(["status"]), "{format}");
        assert_eq!(lines[1]["op"], "update");
        assert_eq!(
            (&lines[2]["op"], &lines[2]["sha256"], &lines[2]["version"]),
            (&json!("delete"), &Value::Null, &json!(2)),
            "{format}"
        );
        assert!(s.fsck()?.is_empty(), "{format}: {:?}", s.fsck());
    }
    Ok(())
}

#[test]
fn changes_seq_strictly_increases_across_rotation() -> R {
    for format in FORMATS {
        let f = Fixture::new(format)?;
        let s = f.store();
        let batch = [
            Op::Set {
                collection: "items".into(),
                id: "A02".into(),
                data: json!({"n": 1}),
                if_version: None,
            },
            Op::Set {
                collection: "goals".into(),
                id: "G01".into(),
                data: json!({"n": 1}),
                if_version: None,
            },
        ];
        s.batch(&batch)?;
        let order: Vec<Value> = f
            .lines("changes.jsonl")?
            .iter()
            .map(|l| l["collection"].clone())
            .collect();
        assert_eq!(
            order,
            [json!("goals"), json!("items")],
            "{format}: byte order"
        );
        f.clock.sleep(Duration::from_secs(20 * 86_400));
        s.update("items", "A02", json!({"n": 2}), None)?;
        let october = f.lines("changes-2026-10.jsonl")?;
        let november = f.lines("changes.jsonl")?;
        assert_eq!(seqs(&october), [1, 2], "{format}");
        assert_eq!(seqs(&november), [3], "{format}");
        assert_eq!(november[0]["ts"], "2026-11-10T10:00:00Z");
        s.update("items", "A02", json!({"n": 3}), None)?;
        assert_eq!(seqs(&f.lines("changes.jsonl")?), [3, 4], "{format}");
        assert!(s.fsck()?.is_empty(), "{format}: {:?}", s.fsck());
    }
    Ok(())
}

#[test]
fn rotation_never_overwrites_a_rotated_file() -> R {
    for format in FORMATS {
        let f = Fixture::new(format)?;
        let s = f.store();
        s.set("items", "A01", json!({"n": 1}), None)?;
        let kept = f.log("changes.jsonl");
        fs::write(f.root.join("changes-2026-10.jsonl"), &kept)?;
        f.clock.sleep(Duration::from_secs(20 * 86_400));
        let got = s.update("items", "A01", json!({"n": 2}), Some(1));
        let Err(DashError::Unlogged { results, cause }) = got else {
            return Err(format!("{format}: {got:?}").into());
        };
        assert!(cause.to_string().contains("never overwritten"), "{cause}");
        assert_eq!(
            results,
            [OpResult::Written {
                collection: "items".into(),
                id: "A01".into(),
                version: 2
            }]
        );
        assert_eq!(s.get("items", "A01")?.version, 2, "{format}: committed");
        assert_eq!(f.log("changes-2026-10.jsonl"), kept, "{format}");
        assert_eq!(f.log("changes.jsonl"), kept, "{format}");
    }
    Ok(())
}

#[test]
fn change_log_failure_is_unlogged_after_commit() -> R {
    for format in FORMATS {
        let f = Fixture::new(format)?;
        let s = f.store();
        fs::create_dir_all(f.root.join("changes.jsonl"))?;
        let got = s.set("items", "A01", json!({"n": 1}), None);
        assert!(
            matches!(&got, Err(DashError::Unlogged { results, .. }) if results.len() == 1),
            "{format}: {got:?}"
        );
        let msg = got.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(msg.contains("committed") && msg.contains("fsck"), "{msg}");
        assert_eq!(s.get("items", "A01")?.data["n"], 1, "{format}");
    }
    Ok(())
}

#[test]
fn fsck_reports_gap() -> R {
    for format in FORMATS {
        let f = Fixture::new(format)?;
        let s = f.store();
        for n in 1..=3 {
            s.set("items", &format!("A0{n}"), json!({"n": n}), None)?;
        }
        let log = f.root.join("changes.jsonl");
        let kept: Vec<String> = f.log("changes.jsonl").lines().map(str::to_owned).collect();
        fs::write(&log, format!("{}\n{}\n", kept[0], kept[2]))?;
        let problems = s.fsck()?;
        let want = format!("{}:2: seq 3 follows 1: 1 missing", log.display());
        assert!(problems.contains(&want), "{format}: {problems:?}");
        fs::write(&log, format!("{}\n{}\n{}\n", kept[1], kept[0], kept[2]))?;
        let problems = s.fsck()?;
        let want = format!("{}:2: seq 1 after 2: not increasing", log.display());
        assert!(problems.contains(&want), "{format}: {problems:?}");
    }
    Ok(())
}

#[test]
fn fsck_reports_version_mismatch() -> R {
    for format in FORMATS {
        let f = Fixture::new(format)?;
        let s = f.store();
        s.set("items", "A01", json!({"n": 1}), None)?;
        s.update("items", "A01", json!({"n": 2}), None)?;
        s.set("items", "A02", json!({"n": 1}), None)?;
        s.set("goals", "G01", json!({"n": 1}), None)?;
        s.delete("goals", "G01", None)?;
        let path = f.doc_path("items", "A01");
        let text = fs::read_to_string(&path)?;
        let (from, to) = match format {
            Format::Dir => ("\"version\": 2", "\"version\": 3"),
            Format::Jsonl => ("\"version\":2", "\"version\":3"),
        };
        fs::write(&path, text.replacen(from, to, 1))?;
        let a02 = f.doc_path("items", "A02");
        let text = fs::read_to_string(&a02)?;
        let edited = match format {
            Format::Dir => text.replacen("\"n\": 1", "\"n\": 9", 1),
            Format::Jsonl => text.replacen("\"n\":1},\"id\":\"A02\"", "\"n\":9},\"id\":\"A02\"", 1),
        };
        fs::write(&a02, edited)?;
        let g01 = match format {
            Format::Dir => (
                f.root.join("goals/G01.json"),
                "{\n  \"n\": 1,\n  \"version\": 1\n}\n",
            ),
            Format::Jsonl => (
                f.root.join("goals.jsonl"),
                "{\"data\":{\"n\":1},\"id\":\"G01\",\"sv\":1,\"version\":1}\n",
            ),
        };
        if let Some(parent) = g01.0.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&g01.0, g01.1)?;
        let problems = s.fsck()?;
        let want = [
            "goals/G01: logged deleted (seq 5) but present in the file",
            "items/A01: version 3 differs from last logged version 2 (seq 2)",
            "items/A02: file line hash differs from last logged hash (seq 3)",
        ];
        assert_eq!(problems, want, "{format}");
    }
    Ok(())
}

#[test]
fn fsck_clean_after_writes() -> R {
    for format in FORMATS {
        let f = Fixture::new(format)?;
        let s = f.store();
        s.set("items", "A01", json!({"n": 1, "é": "ü"}), None)?;
        s.set("items", "A02", json!({"n": 1}), None)?;
        s.delete("items", "A02", None)?;
        assert_eq!(s.fsck()?, Vec::<String>::new(), "{format}");
        if format == Format::Jsonl {
            let path = f.root.join("items.jsonl");
            let text = fs::read_to_string(&path)?;
            fs::write(&path, text.replace(":1", ": 1"))?;
            let problems = s.fsck()?;
            let want = format!(
                "{}: bytes are not canonical (re-render differs)",
                path.display()
            );
            assert_eq!(problems.first(), Some(&want), "{problems:?}");
        }
    }
    Ok(())
}

#[test]
fn fsck_reports_torn_change_line() -> R {
    for format in FORMATS {
        let f = Fixture::new(format)?;
        let s = f.store();
        s.set("items", "A01", json!({"n": 1}), None)?;
        let log = f.root.join("changes.jsonl");
        let torn = f.log("changes.jsonl") + "{\"seq\":2,\"collec";
        fs::write(&log, &torn)?;
        let problems = s.fsck()?;
        let want = format!("{}:2: torn last line (no newline)", log.display());
        assert!(problems.contains(&want), "{format}: {problems:?}");
        let got = s.update("items", "A01", json!({"n": 2}), None);
        assert!(
            matches!(got, Err(DashError::Unlogged { .. })),
            "{format}: {got:?}"
        );
        assert_eq!(
            f.log("changes.jsonl"),
            torn,
            "{format}: appended past a torn line"
        );
    }
    Ok(())
}

#[test]
fn fsck_missing_root_is_an_error() {
    for format in FORMATS {
        let store = Store::new("/nonexistent/10063/store", format);
        assert!(
            matches!(store.fsck(), Err(DashError::NotFound(_))),
            "{format}"
        );
    }
}

#[test]
fn delete_refused_when_pruned_write_fails() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("store");
        let blocker = tmp.path().join("archive");
        fs::write(&blocker, "a file, so the log's directory cannot be made")?;
        let prune = PruneLog::outside(&root, blocker.join("pruned.jsonl"))?;
        let s = Store::new(&root, format)
            .with_secret_scan(no_secret)
            .with_archive(Box::new(prune));
        s.set("items", "A01", json!({"n": 1}), None)?;
        let log = fs::read_to_string(root.join("changes.jsonl"))?;
        let got = s.delete("items", "A01", None);
        assert!(
            matches!(got, Err(DashError::Io { .. })),
            "{format}: {got:?}"
        );
        assert_eq!(s.get("items", "A01")?.data["n"], 1, "{format}: deleted");
        assert_eq!(
            fs::read_to_string(root.join("changes.jsonl"))?,
            log,
            "{format}"
        );
    }
    Ok(())
}

#[test]
fn pruned_by_id_returns_record() -> R {
    for format in FORMATS {
        let f = Fixture::new(format)?;
        let s = f.store();
        s.set("items", "A01", json!({"title": "é", "n": 1}), None)?;
        s.update("items", "A01", json!({"n": 2}), None)?;
        s.set("goals", "A01", json!({"n": 1}), None)?;
        s.batch(&[
            Op::Delete {
                collection: "items".into(),
                id: "A01".into(),
                if_version: Some(2),
            },
            Op::Delete {
                collection: "goals".into(),
                id: "A01".into(),
                if_version: None,
            },
        ])?;
        let want = format!(
            "{{\"by\":\"architect\",\"collection\":\"items\",\"data\":{{\"n\":2,\"title\":\"é\"}},\
             \"deleted_at\":\"{NOW_ISO}\",\"id\":\"A01\",\"version\":2}}"
        );
        assert_eq!(
            f.prune.lines(Some("A01"), Some("items"))?,
            [want],
            "{format}"
        );
        assert_eq!(f.prune.lines(Some("A01"), None)?.len(), 2, "{format}");
        assert!(f.prune.lines(Some("A09"), None)?.is_empty());
        assert!(!f.prune.path().starts_with(&f.root));
    }
    Ok(())
}

#[test]
fn prune_log_inside_root_refused() -> R {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("store");
    fs::create_dir_all(&root)?;
    for inside in [
        root.join("pruned.jsonl"),
        root.join(".archive/pruned.jsonl"),
        root.clone(),
        tmp.path().join("x/../store/p.jsonl"),
    ] {
        let got = PruneLog::outside(&root, &inside);
        assert!(
            matches!(got, Err(DashError::Invalid(_))),
            "{}: {got:?}",
            inside.display()
        );
    }
    let sibling = PruneLog::default_for(&root)?;
    assert!(sibling.path().ends_with("owner-queue-archive/pruned.jsonl"));
    assert!(!sibling.path().starts_with(fs::canonicalize(&root)?));
    assert!(PruneLog::outside(&root, tmp.path().join("store-archive/p.jsonl")).is_ok());
    Ok(())
}

#[test]
fn pruned_reads_prototype_lines() -> R {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join("archive/pruned.jsonl");
    fs::create_dir_all(tmp.path().join("archive"))?;
    let proto = "{\"actor\": \"architect\", \"collection\": \"items\", \"data\": {\"n\": 1}, \
                 \"id\": \"D01\", \"pruned_at\": \"2026-10-12T00:00:00Z\", \"version\": 3}";
    let ours = "{\"by\":\"architect\",\"collection\":\"items\",\"data\":{},\"deleted_at\":\
                \"2026-10-21T10:00:00Z\",\"id\":\"D01\",\"version\":4}";
    fs::write(&path, format!("{proto}\n{ours}\n"))?;
    let log = PruneLog::outside(&tmp.path().join("store"), &path)?;
    assert_eq!(log.lines(Some("D01"), Some("items"))?, [proto, ours]);
    fs::write(&path, format!("{proto}\nnot json\n"))?;
    assert!(matches!(
        log.lines(None, None),
        Err(DashError::Corrupt { .. })
    ));
    Ok(())
}

/// A schema set that flags one marked document and accepts the rest.
struct Marked;

impl SchemaSet for Marked {
    fn validate(
        &self,
        coll: &str,
        id: &str,
        data: &Map<String, Value>,
    ) -> Result<Vec<String>, DashError> {
        Ok(data
            .contains_key("bad")
            .then(|| format!("{coll}/{id}: /bad: marked"))
            .into_iter()
            .collect())
    }
}

#[test]
fn validate_reports_dangling_cross_references() -> R {
    for format in FORMATS {
        let f = Fixture::new(format)?;
        let s = f.store();
        let set = |coll: &str, id: &str, data: Value| Op::Set {
            collection: coll.into(),
            id: id.into(),
            data,
            if_version: None,
        };
        s.batch(&[
            set("goals", "G01", json!({})),
            set("items", "A01", json!({"goal": "G01"})),
            set("items", "A02", json!({"goal": "G09", "bad": true})),
            set("items", "A03", json!({"goal": 7})),
            set("goal_removals", "doc-001", json!({"goal": "G05"})),
            set("goal_removals", "doc-002", json!({})),
            set("step_progress", "A01", json!({"item": "A01"})),
            set("step_progress", "T02", json!({"item": "T09"})),
        ])?;
        let v = s.validate(&Marked)?;
        assert_eq!(v.documents, 8, "{format}");
        let want = [
            "items/A02: /bad: marked",
            "items/A02: /goal: G09 is not a goal id in the store",
            "goal_removals/doc-001: /goal: G05 is not a goal id in the store",
            "goal_removals/doc-002: /goal: None is not a goal id in the store",
            "step_progress/T02: /item: T09 is not an item id in the store",
        ];
        assert_eq!(v.errors, want, "{format}");
        let text = serde_json::to_value(&v)?;
        assert_eq!(text["documents"], 8);
    }
    Ok(())
}

#[test]
fn validate_missing_root_is_an_error() {
    let store = Store::new(Path::new("/nonexistent/10063/store"), Format::Dir);
    assert!(matches!(
        store.validate(&Marked),
        Err(DashError::NotFound(_))
    ));
}

#[test]
fn secret_value_refused_and_not_logged() -> R {
    let secret = "ghp_abcdefghijklmnop0123456789";
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("store");
        let s = Store::new(&root, format).with_secret_scan(ghp);
        s.set("items", "A01", json!({"n": 1}), None)?;
        let before = fs::read(root.join("changes.jsonl"))?;
        let cases = [
            vec![Op::Set {
                collection: "items".into(),
                id: "A02".into(),
                data: json!({"note": format!("token {secret}")}),
                if_version: None,
            }],
            vec![
                Op::Update {
                    collection: "items".into(),
                    id: "A01".into(),
                    data: json!({"n": 2}),
                    if_version: None,
                },
                Op::Update {
                    collection: "items".into(),
                    id: "A01".into(),
                    data: json!({"nested": [{ secret: "key" }]}),
                    if_version: None,
                },
            ],
            vec![Op::Set {
                collection: "items".into(),
                id: secret.into(),
                data: json!({}),
                if_version: None,
            }],
        ];
        for (n, ops) in cases.iter().enumerate() {
            let got = s.batch(ops);
            let Err(err @ DashError::SecretRefused(_)) = &got else {
                return Err(format!("{format} case {n}: {got:?}").into());
            };
            for shown in [err.to_string(), format!("{err:?}")] {
                assert!(!shown.contains("abcdefghijklmnop"), "{format}: {shown}");
                assert!(shown.contains("ghp_…"), "{format}: {shown}");
            }
            let op = if n == 1 { "op 2" } else { "op 1" };
            assert!(err.to_string().starts_with(op), "{format}: {err}");
        }
        assert_eq!(fs::read(root.join("changes.jsonl"))?, before, "{format}");
        assert_eq!(s.get("items", "A01")?.version, 1, "{format}");
        assert!(matches!(s.get("items", "A02"), Err(DashError::NotFound(_))));
    }
    Ok(())
}

#[test]
fn write_without_secret_scan_refused() -> R {
    for format in FORMATS {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("store");
        let s = Store::new(&root, format);
        let got = s.set("items", "A01", json!({}), None);
        assert!(
            matches!(&got, Err(DashError::Invalid(m)) if m.contains("no secret scan")),
            "{format}: {got:?}"
        );
        assert!(!root.exists(), "{format}: the store was touched");
    }
    Ok(())
}
