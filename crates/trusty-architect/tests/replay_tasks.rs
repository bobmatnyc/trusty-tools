//! #12028 P1d: the task checker recordings replay clean against the ports.
//!
//! `stale-tasks.py` is recorded directly; `stalled_tasks.py`,
//! `task_wait_check.py` and `il_timeline.py` through the `driver.py` in their
//! fixture directory, which `examples/replay_port.rs` mirrors.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

/// Replays every recording in `fixtures/replay/<script>` against
/// `replay_port <script>`; returns the sorted file names.
fn replay_all(script: &str) -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay")
        .join(script);
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec![script.to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let found = replay(&rec, &target, &parent, Duration::from_secs(30)).unwrap();
        assert!(found.is_empty(), "{}: {found:?}", path.display());
        names.push(path.file_name().unwrap().to_string_lossy().into_owned());
    }
    names.sort();
    names
}

#[test]
fn stale_tasks_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("stale-tasks"),
        [
            "bad-arg-hours-empty.json",
            "bad-arg-hours.json",
            "bad-arg-root-value.json",
            "bad-arg-unrecognized.json",
            "bad-format-env.json",
            "corrupt-doc.json",
            "corrupt-relative-root.json",
            "doc-not-object.json",
            "empty-store-json.json",
            "empty-store.json",
            "env-root.json",
            "happy-json.json",
            "happy-text.json",
            "help.json",
            "hours-flag.json",
            "items-is-file.json",
            "jsonl-bad-line.json",
            "jsonl-bad-sv.json",
            "jsonl-per-doc.json",
            "jsonl-store.json",
            "missing-store.json",
            "wrong-format.json",
        ]
    );
}

#[test]
fn stalled_tasks_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("stalled_tasks"),
        [
            "bad-json-state.json",
            "bad-shape-state.json",
            "bad-usage.json",
            "cli-stalled.json",
            "hook-form.json",
            "missing-state.json",
            "no-owner-queue.json",
            "non-dict-task.json",
            "none-clean.json",
            "state-dir-env.json",
        ]
    );
}

#[test]
fn task_wait_check_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("task_wait_check"),
        [
            "bad-json-state.json",
            "bad-shape-state.json",
            "clean-none.json",
            "items-missing.json",
            "missing-state.json",
            "stale-waits.json",
            "task-no-id.json",
            "task-not-dict.json",
        ]
    );
}

#[test]
fn il_timeline_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("il_timeline"),
        [
            "bad-log-line-crash.json",
            "bad-state-crash.json",
            "bad-usage.json",
            "empty.json",
            "tie-higher-id.json",
            "two-projects.json",
        ]
    );
}
