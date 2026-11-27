//! #12028 P1f: the `il_agent.py` recordings replay clean against the port.
//!
//! Recorded through the `driver.py` in the fixture directory, which points
//! the script's own directory at the sandbox root (the `IL:` line of
//! `dispatch` names it) and which `examples/replay_port.rs` mirrors. Every
//! run is read-only, so a clean replay also proves the run wrote nothing.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

#[test]
fn il_agent_recordings_replay_clean_against_the_port() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/il_agent");
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec!["il_agent".to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let (mut names, mut failed) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let found = replay(&rec, &target, &parent, Duration::from_secs(30)).unwrap();
        if !found.is_empty() {
            failed.push(format!("{}: {found:?}", path.display()));
        }
        names.push(path.file_name().unwrap().to_string_lossy().into_owned());
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    names.sort();
    assert_eq!(
        names,
        [
            "bad-arg-ask-value.json",
            "bad-arg-no-task.json",
            "bad-arg-no-verb.json",
            "bad-arg-unrecognized.json",
            "bad-arg-verb.json",
            "brief-badid.json",
            "brief-bare-id.json",
            "brief-closed.json",
            "brief-datg-session.json",
            "brief-empty-ledger.json",
            "brief-extra-field.json",
            "brief-extra-unknown.json",
            "brief-field.json",
            "brief-full.json",
            "brief-goal-missing.json",
            "brief-legacy-dir.json",
            "brief-no-code.json",
            "brief-no-created.json",
            "brief-no-library.json",
            "brief-other-goal.json",
            "brief-rref-slots.json",
            "brief-rref-unknown.json",
            "brief-stale-stamp.json",
            "brief-state-dir.json",
            "brief-sub-project.json",
            "brief-top-project.json",
            "brief-unknown.json",
            "dispatch-full.json",
            "dispatch-plain.json",
            "dispatch-reject.json",
            "handback-bad-expect.json",
            "handback-check-reject.json",
            "handback-closed.json",
            "handback-empty-stdin.json",
            "handback-file-crlf.json",
            "handback-last-block.json",
            "handback-mismatch.json",
            "handback-no-block.json",
            "handback-no-created.json",
            "handback-no-stamp.json",
            "handback-ok.json",
            "handback-pulse-fails.json",
            "handback-write-field.json",
            "help-brief.json",
            "help-dispatch.json",
            "help-handback.json",
            "help.json",
            "state-bad-json.json",
            "state-bad-shape.json",
            "state-empty.json",
            "state-missing.json",
        ]
    );
}
