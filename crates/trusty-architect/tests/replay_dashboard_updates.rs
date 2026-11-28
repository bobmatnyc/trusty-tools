//! #12029 P2h: the `dashboard_updates.py` and `dashboard_attach.py` recordings replay clean against the port.
//!
//! Recorded from the frozen Python (architect `0fbbc81eed`) through
//! `dashboard_updates/driver.py` (the script's command line, `cli`, and its
//! functions, `cases`) and `dashboard_attach/driver.py` (a CASES.json list:
//! the module has no command line) on synthetic stores in both formats.
//! `examples/replay_port.rs` dispatches them to the library's mirrors.
//! One recorded difference, a ruled divergence (#12029 P2h Q5): the owner
//! answer's update text starts [`ANSWERED`] where the frozen script names the
//! owner; the recordings hold the port's text ([`answer_text_is_neutral`]).
//! The jsonl recordings seeded the frozen `schemas/dashboard`; that seed is
//! the one copy P2g keeps ([`SCHEMAS`]) and is laid back into each recording
//! in [`WITH_SCHEMAS`]. Every write lands under the sandbox root.

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::time::Duration;

use trusty_architect::replay::{Recording, Target, load, replay};

mod replay_support;

/// The owner-answer text the port writes (#12029 P2h Q5).
const ANSWERED: &str = "Owner answered";

/// The frozen `schemas/dashboard`, one copy (#12029 P2g).
const SCHEMAS: &str = "tests/fixtures/dashboard-db/frozen-schemas.json";

/// The recordings that seeded the frozen schemas (the jsonl store validates in `warn`).
const WITH_SCHEMAS: [&str; 6] = [
    "dashboard_attach/cases-jsonl.json",
    "dashboard_updates/cases-jsonl.json",
    "dashboard_updates/cli-ingest-jsonl.json",
    "dashboard_updates/cli-list-docs-failed-jsonl.json",
    "dashboard_updates/cli-note-jsonl.json",
    "dashboard_updates/cli-note-noinbox-jsonl.json",
];

fn schema_files() -> BTreeMap<String, String> {
    #[derive(serde::Deserialize)]
    struct Shared {
        files: BTreeMap<String, String>,
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(SCHEMAS);
    let s: Shared = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    s.files
}

/// Every recording of `dir` as committed.
fn raw_fixtures(dir: &str) -> Vec<(String, Recording)> {
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
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        out.push((name, rec));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Every recording, with the shared schemas laid into those that seeded them.
fn fixtures(dir: &str) -> Vec<(String, Recording)> {
    let files = schema_files();
    let mut out = raw_fixtures(dir);
    for (name, rec) in &mut out {
        if WITH_SCHEMAS.contains(&format!("{dir}/{name}").as_str()) {
            for (k, v) in &files {
                assert!(
                    rec.files.insert(k.clone(), v.clone()).is_none(),
                    "{name}: {k}"
                );
            }
        }
    }
    out
}

fn replay_all(dir: &str) -> Vec<(String, Recording)> {
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec![dir.to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let recs = fixtures(dir);
    let mut failed = Vec::new();
    for (name, rec) in &recs {
        let found = replay(rec, &target, &parent, Duration::from_secs(120)).unwrap();
        if !found.is_empty() {
            failed.push(format!("{name}: {found:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    recs
}

#[test]
fn dashboard_updates_recordings_replay_clean_against_the_port() {
    let recs = replay_all("dashboard_updates");
    assert!(recs.len() >= 20, "{} recordings", recs.len());
    for fmt in ["dir", "jsonl"] {
        for stem in ["cases", "cli-ingest", "cli-note", "cli-note-noinbox"] {
            let name = format!("{stem}-{fmt}.json");
            assert!(recs.iter().any(|(n, _)| *n == name), "no {name}");
        }
    }
    for exit in [0, 2] {
        assert!(
            recs.iter().any(|(_, r)| r.exit == exit),
            "no recording exits {exit}"
        );
    }
}

#[test]
fn dashboard_attach_recordings_replay_clean_against_the_port() {
    let recs = replay_all("dashboard_attach");
    let names: Vec<&str> = recs.iter().map(|(n, _)| n.as_str()).collect();
    for want in ["cases-dir.json", "cases-jsonl.json", "driver-usage.json"] {
        assert!(names.contains(&want), "no {want}");
    }
}

/// A recorded write path: relative, with no `..` or root component.
fn inside_sandbox(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

#[test]
fn p2h_recordings_write_only_under_the_root_and_seed_no_schema_copy() {
    for dir in ["dashboard_updates", "dashboard_attach"] {
        for (name, rec) in raw_fixtures(dir) {
            for path in rec.writes.keys() {
                assert!(inside_sandbox(path), "{dir}/{name}: {path}");
            }
            assert!(
                !rec.files.keys().any(|k| k.starts_with("schemas/")),
                "{dir}/{name} seeds its own schema copy"
            );
        }
    }
}

/// Ruled divergence, #12029 P2h Q5: the owner-answer entry reads
/// `Owner answered <id>: <answer>`; no recording holds the frozen wording.
#[test]
fn answer_text_is_neutral() {
    let recs = raw_fixtures("dashboard_updates");
    let with = recs
        .iter()
        .filter(|(_, r)| {
            r.stdout.contains(ANSWERED) || r.writes.values().flatten().any(|w| w.contains(ANSWERED))
        })
        .count();
    assert!(with >= 10, "{with} recordings carry the answer entry");
    for (name, rec) in &recs {
        let all = [rec.stdout.as_str(), rec.stderr.as_str()]
            .into_iter()
            .chain(rec.writes.values().flatten().map(String::as_str));
        for text in all {
            let any = text.matches(" answered D").count();
            let ours = text.matches("Owner answered D").count();
            assert_eq!(any, ours, "{name}: an answer entry with another wording");
        }
    }
}
