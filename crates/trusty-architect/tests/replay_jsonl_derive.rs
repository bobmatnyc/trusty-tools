//! #12029 P2b: the `dashboard_jsonl.py` and `dashboard_derive.py` recordings replay clean against the port.
//!
//! Recorded from the frozen Python through each directory's `driver.py`
//! on synthetic sandbox stores: `dashboard_jsonl/` runs the module's command
//! line (`cli ...`) and case lists against its library, `dashboard_derive/`
//! runs case lists against its library. `examples/replay_port.rs` dispatches
//! both to the library's own driver mirror. Only the change-log appends write,
//! and every write lands under the sandbox root.

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

/// Replays every recording in `tests/fixtures/replay/<dir>` against
/// `replay_port <port>`; returns the recording names, sorted.
fn replay_dir(dir: &str, port: &str) -> Vec<String> {
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec![port.to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let mut failed = Vec::new();
    let recs = fixtures(dir);
    for (name, rec) in &recs {
        let found = replay(rec, &target, &parent, Duration::from_secs(30)).unwrap();
        if !found.is_empty() {
            failed.push(format!("{name}: {found:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    recs.into_iter().map(|(n, _)| n).collect()
}

/// A recorded write path: relative, with no `..` or root component.
fn inside_sandbox(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

#[test]
fn dashboard_jsonl_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_dir("dashboard_jsonl", "dashboard_jsonl"),
        [
            "append.json",
            "cli-bad.json",
            "cli-clean.json",
            "cli-default-root.json",
            "cli-empty-arg.json",
            "cli-extra-args.json",
            "cli-missing-root.json",
            "driver-usage.json",
            "fsck.json",
            "parse.json",
            "pure.json",
            "read.json",
        ]
    );
}

#[test]
fn dashboard_derive_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_dir("dashboard_derive", "dashboard_derive"),
        [
            "driver-usage.json",
            "ids.json",
            "pending.json",
            "predicates.json",
            "times.json",
        ]
    );
}

/// The rulings' sandbox rule: only the change-log appends write, under their
/// case root inside the sandbox, and nothing else writes at all.
#[test]
fn append_writes_only_under_the_root() {
    for (dir, writer) in [("dashboard_jsonl", "append.json"), ("dashboard_derive", "")] {
        for (name, rec) in fixtures(dir) {
            if name != writer {
                assert!(
                    rec.writes.is_empty(),
                    "{dir}/{name} writes {:?}",
                    rec.writes.keys()
                );
                continue;
            }
            assert!(
                !rec.writes.is_empty(),
                "{dir}/{name}: the append cases write"
            );
            for path in rec.writes.keys() {
                let p = Path::new(path);
                assert!(inside_sandbox(path) && p.starts_with("append"), "{path}");
                let file = p.file_name().unwrap().to_string_lossy();
                assert!(
                    file.starts_with("changes") && file.ends_with(".jsonl"),
                    "{path}"
                );
            }
        }
    }
}
