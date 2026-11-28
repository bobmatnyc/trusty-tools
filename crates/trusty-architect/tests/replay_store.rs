//! #12029 P2c: the `dashboard_store.py` recordings replay clean against the port.
//!
//! Recorded from the frozen Python through `dashboard_store/driver.py` on
//! synthetic sandbox stores in both formats: versions and conflicts, names
//! and bodies, the request refusals, lists, collections, the prune log, the
//! change log, schema warn and enforce, the other-format guard, corrupt
//! files, a missing root, a root that is a file, a lock that cannot be
//! opened, a replace that fails midway (rollback) and lock contention.
//! `examples/replay_port.rs` dispatches them to the library's driver mirror.
//! Every write lands under the sandbox root.

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::time::Duration;

use trusty_architect::replay::{Recording, Target, load, replay};

mod replay_support;

fn fixtures() -> Vec<(String, Recording)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/dashboard_store");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        out.push((
            path.file_name().unwrap().to_string_lossy().into_owned(),
            rec,
        ));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn dashboard_store_recordings_replay_clean_against_the_port() {
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec!["dashboard_store".to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let recs = fixtures();
    let mut failed = Vec::new();
    for (name, rec) in &recs {
        let found = replay(rec, &target, &parent, Duration::from_secs(60)).unwrap();
        if !found.is_empty() {
            failed.push(format!("{name}: {found:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    let names: Vec<&str> = recs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "dir.json",
            "driver-usage.json",
            "errors.json",
            "jsonl.json",
            "lock.json"
        ]
    );
}

/// A recorded write path: relative, with no `..` or root component.
fn inside_sandbox(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

/// The rulings' sandbox rule: every write is relative to the sandbox root,
/// and the usage run writes nothing.
#[test]
fn store_writes_only_under_the_root() {
    for (name, rec) in fixtures() {
        if name == "driver-usage.json" {
            assert!(
                rec.writes.is_empty(),
                "{name} writes {:?}",
                rec.writes.keys()
            );
            continue;
        }
        assert!(!rec.writes.is_empty(), "{name}: the store cases write");
        for path in rec.writes.keys() {
            assert!(inside_sandbox(path), "{name}: {path}");
        }
    }
}
