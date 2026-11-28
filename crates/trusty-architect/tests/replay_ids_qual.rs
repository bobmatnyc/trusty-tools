//! #12029 P2d: the `dashboard_ids.py` and `dashboard_qual.py` recordings replay clean against the port.
//!
//! Recorded from the frozen Python through `dashboard_ids/driver.py` and
//! `dashboard_qual/driver.py` on synthetic sandbox stores: the id space per
//! prefix across collections, odd ids, the counter file (hex-era and base-34
//! values, bad digits, unreadable files), the brief space, every retire arm
//! (seven days, the 289 floor, the usage pass, `keep`, the 24 h guard, an
//! archive that cannot be written), the command line on the default root,
//! qualified refs, store S ids and id aliases.
//! `examples/replay_port.rs` dispatches them to the library's driver mirrors.
//! Every write lands under the sandbox root.

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::time::Duration;

use trusty_architect::replay::{Recording, Target, load, replay};

mod replay_support;

fn fixtures(dir: &str) -> Vec<(String, Recording)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay")
        .join(dir);
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

fn replay_all(script: &str) -> Vec<String> {
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec![script.to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let recs = fixtures(script);
    let mut failed = Vec::new();
    for (name, rec) in &recs {
        let found = replay(rec, &target, &parent, Duration::from_secs(120)).unwrap();
        if !found.is_empty() {
            failed.push(format!("{name}: {found:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    recs.into_iter().map(|(n, _)| n).collect()
}

#[test]
fn dashboard_ids_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("dashboard_ids"),
        [
            "driver-usage.json",
            "floor.json",
            "functions.json",
            "retire.json"
        ]
    );
}

#[test]
fn dashboard_qual_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("dashboard_qual"),
        ["cases.json", "driver-usage.json"]
    );
}

/// A recorded write path: relative, with no `..` or root component.
fn inside_sandbox(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

/// The rulings' sandbox rule: every write is relative to the sandbox root,
/// and the usage runs write nothing.
#[test]
fn ids_and_qual_write_only_under_the_root() {
    for dir in ["dashboard_ids", "dashboard_qual"] {
        for (name, rec) in fixtures(dir) {
            if name == "driver-usage.json" {
                assert!(
                    rec.writes.is_empty(),
                    "{dir}/{name} writes {:?}",
                    rec.writes.keys()
                );
                continue;
            }
            assert!(!rec.writes.is_empty(), "{dir}/{name}: the cases write");
            for path in rec.writes.keys() {
                assert!(inside_sandbox(path), "{dir}/{name}: {path}");
            }
        }
    }
}
