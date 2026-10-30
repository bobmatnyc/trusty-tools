//! #10063 PR 1: the Rust dashboard store leaves the prototype's bytes and
//! outcomes (DOC-82 LDG-1.5, LDG-2.8.1, LDG-3.2).
//!
//! `tests/fixtures/dashboard/corpus.json` and the `<case>/<format>/` trees
//! are written by `regen_dashboard.py`, which runs each case's op batches
//! through the prototype's `dashboard_store.Store` in both formats. Each test
//! here runs the same batches through [`Store::batch`] on an empty temporary
//! root and compares every outcome and, after the last batch, every file.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;
use trusty_architect::dashboard::ids::{KEEP_FREE, free_count, next_id, retire_candidates};
use trusty_architect::dashboard::{
    DashError, DeleteArchive, FooterLinks, Format, ListQuery, Op, Removed, SchemaDir, SchemaSet,
    Store, footer, tally_line,
};

type R = Result<(), Box<dyn std::error::Error>>;

/// A test scanner: nothing is secret-shaped (#10063 PR 2: a store with no
/// scan refuses writes).
fn no_secret(_: &str) -> Option<String> {
    None
}

/// 2026-10-21T10:00:00Z, the regen script's `NOW`.
const NOW: f64 = 1_792_576_800.0;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dashboard")
}

fn corpus() -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(fixtures().join("corpus.json"))?;
    let v: Value = serde_json::from_str(&text)?;
    Ok(v["cases"].as_array().ok_or("cases")?.clone())
}

/// Accepts every delete; `PruneLog` is tested in `tests/dashboard_log.rs`.
#[derive(Default)]
struct Accept(Mutex<Vec<String>>);

impl DeleteArchive for Accept {
    fn record(&self, removed: &[Removed<'_>]) -> Result<(), DashError> {
        let mut seen = self
            .0
            .lock()
            .map_err(|_| DashError::Invalid("poisoned".into()))?;
        seen.extend(removed.iter().map(|r| format!("{}/{}", r.collection, r.id)));
        Ok(())
    }
}

/// The prototype's outcome class for a Rust result.
fn class(result: &Result<Vec<trusty_architect::dashboard::OpResult>, DashError>) -> String {
    match result {
        Ok(_) => "ok".into(),
        Err(DashError::Conflict(_)) => "Conflict".into(),
        Err(DashError::NotFound(_)) => "NotFound".into(),
        Err(DashError::SchemaInvalid(_)) => "SchemaInvalid".into(),
        Err(_) => "StoreError".into(),
    }
}

/// Every file under `root` (relative path to bytes), minus `.lock` and the
/// change logs (the regen script leaves them out; `tests/dashboard_log.rs`
/// covers them).
fn tree(root: &Path) -> std::io::Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .is_some_and(|n| n != ".lock" && !n.to_string_lossy().starts_with("changes"))
            {
                let rel = path.strip_prefix(root).map_err(std::io::Error::other)?;
                out.insert(rel.to_string_lossy().into_owned(), fs::read(&path)?);
            }
        }
    }
    Ok(out)
}

#[test]
fn dashboard_writer_matches_prototype_bytes() -> R {
    let mut batches = 0;
    for case in corpus()? {
        let name = case["name"].as_str().ok_or("name")?;
        for (fmt, format) in [("dir", Format::Dir), ("jsonl", Format::Jsonl)] {
            let tmp = tempfile::tempdir()?;
            let root = tmp.path().join("store");
            let store = Store::new(&root, format)
                .with_secret_scan(no_secret)
                .with_archive(Box::new(Accept::default()));
            for (n, batch) in case["batches"]
                .as_array()
                .ok_or("batches")?
                .iter()
                .enumerate()
            {
                let at = format!("{name}/{fmt} batch {n}");
                let ops: Result<Vec<Op>, DashError> = batch["ops"]
                    .as_array()
                    .ok_or("ops")?
                    .iter()
                    .map(Op::from_json)
                    .collect();
                let got = ops.and_then(|ops| store.batch(&ops));
                let want = &batch[fmt];
                let want_class = want["error"].as_str().unwrap_or("ok");
                assert_eq!(class(&got), want_class, "{at}: {got:?}");
                if let Ok(results) = &got {
                    assert_eq!(
                        &serde_json::to_value(results)?,
                        &want["ok"],
                        "{at}: results"
                    );
                }
                batches += 1;
            }
            let want = tree(&fixtures().join(name).join(fmt))?;
            let have = tree(&root)?;
            assert_eq!(
                have.keys().collect::<Vec<_>>(),
                want.keys().collect::<Vec<_>>(),
                "{name}/{fmt}: file set"
            );
            for (rel, bytes) in &want {
                assert!(
                    have.get(rel) == Some(bytes),
                    "{name}/{fmt}: {rel} differs from the prototype's bytes"
                );
            }
        }
    }
    assert!(batches >= 30, "corpus too small: {batches} batches");
    Ok(())
}

#[test]
fn other_format_refused_not_empty() -> R {
    let dir_store =
        Store::new(fixtures().join("synthetic/dir"), Format::Jsonl).with_secret_scan(no_secret);
    let jsonl_store =
        Store::new(fixtures().join("synthetic/jsonl"), Format::Dir).with_secret_scan(no_secret);
    for store in [&dir_store, &jsonl_store] {
        let at = format!("{:?}", store.format());
        assert!(
            matches!(
                store.list("items", &ListQuery::default()),
                Err(DashError::WrongFormat(_))
            ),
            "{at}: list"
        );
        assert!(
            matches!(store.get("items", "D01"), Err(DashError::WrongFormat(_))),
            "{at}: get"
        );
        assert!(
            matches!(store.collections(), Err(DashError::WrongFormat(_))),
            "{at}: collections"
        );
        let refused = store.set("items", "D09", serde_json::json!({}), Some(0));
        assert!(
            matches!(refused, Err(DashError::WrongFormat(_))),
            "{at}: set"
        );
    }
    Ok(())
}

#[test]
fn golden_reads_agree_across_formats() -> R {
    for case in ["synthetic", "live", "edge"] {
        let dir = Store::new(fixtures().join(case).join("dir"), Format::Dir);
        let jsonl = Store::new(fixtures().join(case).join("jsonl"), Format::Jsonl);
        let colls = dir.collections()?;
        assert_eq!(colls, jsonl.collections()?, "{case}: collections");
        let names: Vec<&str> = colls.iter().map(String::as_str).collect();
        assert_eq!(
            dir.snapshot(&names)?,
            jsonl.snapshot(&names)?,
            "{case}: documents"
        );
    }
    Ok(())
}

#[test]
fn derived_values_match_prototype() -> R {
    let links = FooterLinks {
        local: "http://127.0.0.1:7890/".into(),
        tailnet: "https://dash.example.ts.net:7890/".into(),
    };
    for case in corpus()? {
        let name = case["name"].as_str().ok_or("name")?;
        let want = &case["derived"];
        for format in [Format::Dir, Format::Jsonl] {
            let store = Store::new(fixtures().join(name).join(format.to_string()), format);
            let snap = store.snapshot(&trusty_architect::dashboard::ids::ID_COLLECTIONS)?;
            for p in "GPADTSQR".chars() {
                let key = p.to_string();
                assert_eq!(
                    next_id(&snap, p).map(Value::from).unwrap_or(Value::Null),
                    want["next"][&key]
                );
                assert_eq!(
                    Value::from(free_count(&snap, p)),
                    want["free"][&key],
                    "{name} free {p}"
                );
                assert_eq!(
                    Value::from(retire_candidates(&snap, p, KEEP_FREE)),
                    want["retire"][&key]
                );
            }
            let f = footer(&store, NOW, &links)?;
            assert_eq!(
                serde_json::to_value(&f.tally)?,
                want["tally"],
                "{name}: tally"
            );
            assert_eq!(
                tally_line(&f.tally),
                want["tally_line"].as_str().ok_or("line")?
            );
            assert_eq!(f.footer, want["footer"].as_str().ok_or("footer")?);
        }
    }
    Ok(())
}

/// With DASHBOARD_PROTO_DIR naming the prototype repository (Architect ruling
/// Q1: schemas come from there until PR 3 vendors them), every schema loads
/// and each golden document gets the prototype's exact error lines.
#[test]
fn prototype_schemas_load_and_fixtures_validate() -> R {
    let Some(proto) = std::env::var_os("DASHBOARD_PROTO_DIR") else {
        eprintln!("DASHBOARD_PROTO_DIR unset: prototype schema check skipped");
        return Ok(());
    };
    let set = SchemaDir::load(&Path::new(&proto).join("schemas/dashboard"))?;
    assert!(set.names().len() >= 15, "schemas: {:?}", set.names());
    let mut checked = 0;
    for case in corpus()? {
        let name = case["name"].as_str().ok_or("name")?;
        let store = Store::new(fixtures().join(name).join("dir"), Format::Dir);
        for coll in store.collections()? {
            for doc in store.list(&coll, &ListQuery::default())? {
                let key = format!("{coll}/{}", doc.id);
                let got = set.validate(&coll, &doc.id, &doc.data)?;
                assert_eq!(
                    Value::from(got),
                    case["derived"]["schema_errors"][&key],
                    "{name}: {key}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 50, "only {checked} documents checked");
    Ok(())
}

/// Every string in the `live` case is filler, a timestamp, a synthetic id, a
/// `pm-<n>` session, or an enum token (`[a-z_]{1,24}`) under a kept enum key;
/// an unscrubbed value fails.
#[test]
fn dashboard_fixtures_are_scrubbed() -> R {
    const FILLER: [&str; 10] = [
        "lorem", "ipsum", "dolor", "sit", "amet", "elit", "sed", "tempor", "über", "magna",
    ];
    // regen_dashboard.py's KEEP plus scrub.py's.
    const KEEP: [&str; 11] = [
        "status",
        "kind",
        "lifecycle",
        "state",
        "live_state",
        "choice",
        "user_state",
        "ev",
        "mode",
        "who",
        "esc",
    ];
    let filler = |s: &str| {
        s.split([' ', '\n', '-'])
            .all(|t| FILLER.iter().any(|w| w.starts_with(t)))
    };
    let ok = |s: &str| {
        s.is_empty()
            || filler(s)
            || trusty_architect::ledger::is_bare_id(s)
            || trusty_architect::ledger::normalize_ts(s).is_ok()
            || s.strip_prefix("pm-")
                .is_some_and(|n| n.parse::<u32>().is_ok())
    };
    fn strings(key: &str, v: &Value, out: &mut Vec<(String, String)>) {
        match v {
            Value::String(s) => out.push((key.to_owned(), s.clone())),
            Value::Array(a) => a.iter().for_each(|x| strings(key, x, out)),
            Value::Object(o) => o.iter().for_each(|(k, x)| strings(k, x, out)),
            _ => {}
        }
    }
    let store = Store::new(fixtures().join("live/dir"), Format::Dir);
    for coll in store.collections()? {
        for doc in store.list(&coll, &ListQuery::default())? {
            let meta = coll == "meta";
            let id_ok = ok(&doc.id)
                || meta
                || doc
                    .id
                    .strip_prefix("doc-")
                    .is_some_and(|n| n.parse::<u32>().is_ok());
            assert!(id_ok, "{coll}: unscrubbed doc id of {} chars", doc.id.len());
            let mut found = Vec::new();
            strings("", &Value::Object(doc.data.clone()), &mut found);
            for (key, s) in found {
                // Never print the value: an unscrubbed one is private.
                let token = s.len() <= 24 && s.chars().all(|c| c.is_ascii_lowercase() || c == '_');
                let kept = KEEP.contains(&key.as_str()) && token;
                assert!(
                    ok(&s) || kept,
                    "{coll}/{}: {key}: unscrubbed {}-char string",
                    doc.id,
                    s.chars().count()
                );
            }
        }
    }
    Ok(())
}
