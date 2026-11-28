//! #12029 P2e: the `owner_answers.py` and `pending-answers.py` recordings replay clean against the port.
//!
//! Recorded from the frozen Python through `owner_answers/driver.py` and the
//! script's own command line on synthetic sandbox stores: every pending
//! kind, the relay rules (`relayed`, `relayed_at`, a "Relayed" note),
//! self-test canaries, parked and ILIO items, qualified ids, both formats,
//! store errors and the command's exit codes.
//! `examples/replay_port.rs` dispatches them to `dashboard::pyanswers`.
//! Every write lands under the sandbox root. #12029 P2g: `cases.json`
//! replays with [`ANSWER_NOTE_DIVERGENCE`] applied, the one intended
//! difference from the frozen Python.

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

/// #12029 P2g: the intended answer-note divergence (Architect ruling 17:06Z).
/// The frozen derive drops a plain answer's `answer_note`; the port appends
/// `: <note>` as the bundled copy's `_with_note` does (#10765). Each entry is
/// a recorded stdout line, the port's line, and how often the line appears.
const ANSWER_NOTE_DIVERGENCE: [(&str, &str, usize); 4] = [
    (
        r#"  {"id": "D02", "kind": "answered", "at": "2026-11-20T09:16:00Z", "text": "No"}"#,
        r#"  {"id": "D02", "kind": "answered", "at": "2026-11-20T09:16:00Z", "text": "No: keep it small"}"#,
        4,
    ),
    (
        r#"  {"id": "D0d", "kind": "answered", "at": "2026-11-20T09:27:00Z", "text": "Yes"}"#,
        r#"  {"id": "D0d", "kind": "answered", "at": "2026-11-20T09:27:00Z", "text": "Yes: Acknowledged"}"#,
        3,
    ),
    (
        "S14:D02\tanswered\t2026-11-20T09:16:00Z\tNo",
        "S14:D02\tanswered\t2026-11-20T09:16:00Z\tNo: keep it small",
        3,
    ),
    (
        "S14:D0d\tanswered\t2026-11-20T09:27:00Z\tYes",
        "S14:D0d\tanswered\t2026-11-20T09:27:00Z\tYes: Acknowledged",
        3,
    ),
];

/// `rec` with each divergent stdout line replaced by the port's, after
/// checking the recording holds it exactly `count` times.
fn with_answer_note_divergence(mut rec: Recording) -> Recording {
    let mut lines: Vec<String> = rec.stdout.split('\n').map(str::to_owned).collect();
    for (old, new, count) in ANSWER_NOTE_DIVERGENCE {
        let hits = lines.iter().filter(|l| l.as_str() == old).count();
        assert_eq!(
            hits, count,
            "the recording no longer holds {old:?} {count} times"
        );
        for l in lines.iter_mut().filter(|l| l.as_str() == old) {
            *l = new.to_owned();
        }
    }
    rec.stdout = lines.join("\n");
    rec
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
        let rec = if script == "owner_answers" && name == "cases.json" {
            with_answer_note_divergence(rec.clone())
        } else {
            rec.clone()
        };
        let found = replay(&rec, &target, &parent, Duration::from_secs(120)).unwrap();
        if !found.is_empty() {
            failed.push(format!("{name}: {found:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    recs.into_iter().map(|(n, _)| n).collect()
}

#[test]
fn owner_answers_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("owner_answers"),
        ["cases.json", "driver-usage.json"]
    );
}

#[test]
fn pending_answers_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("pending-answers"),
        [
            "bad-arg.json",
            "empty.json",
            "env-root.json",
            "help.json",
            "listed.json",
            "not-a-dir.json"
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
/// and the command, a reader, writes nothing.
#[test]
fn owner_answers_write_only_under_the_root() {
    for (name, rec) in fixtures("owner_answers") {
        for path in rec.writes.keys() {
            assert!(inside_sandbox(path), "owner_answers/{name}: {path}");
        }
    }
    for (name, rec) in fixtures("pending-answers") {
        assert!(
            rec.writes.is_empty(),
            "pending-answers/{name} writes {:?}",
            rec.writes.keys()
        );
    }
}
