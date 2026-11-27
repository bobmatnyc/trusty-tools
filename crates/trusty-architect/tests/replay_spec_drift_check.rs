//! #12028 P1b: every `spec_drift_check.py` recording replays clean against the port.
//!
//! The recordings in `tests/fixtures/replay/spec_drift_check/` were made
//! from the frozen Python script with `record --clock` (see the fixture
//! README) and replay clean against it; this test replays them against
//! `examples/replay_port.rs`, which runs `trusty_architect::spec_drift::run`
//! on the recorded instant.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

#[test]
fn spec_drift_recordings_replay_clean_against_the_port() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/spec_drift_check");
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec!["spec_drift_check".to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(
            rec.clock.is_some(),
            "{}: recorded without --clock",
            path.display()
        );
        let found = replay(&rec, &target, &parent, Duration::from_secs(30)).unwrap();
        assert!(found.is_empty(), "{}: {found:?}", path.display());
        names.push(path.file_name().unwrap().to_string_lossy().into_owned());
    }
    names.sort();
    assert_eq!(
        names,
        [
            "bad-arg-explicit.json",
            "bad-arg-root-value.json",
            "bad-arg-unrecognized.json",
            "daily-no-log.json",
            "daily-pass.json",
            "drift-many-fail.json",
            "drift-orphan-fail.json",
            "empty-manifest.json",
            "help.json",
            "manifest-key-errors.json",
            "missing-manifest.json",
            "pre-phase-pending.json",
        ]
    );
    // The daily-job case is the PASS path; the FAIL case files a finding.
    let daily = load(&dir.join("daily-pass.json")).unwrap();
    assert_eq!(daily.exit, 0);
    assert!(daily.writes.contains_key("local/soak/spec-drift.jsonl"));
    let fail = load(&dir.join("drift-orphan-fail.json")).unwrap();
    assert_eq!(fail.exit, 1);
    assert!(fail.writes.contains_key("local/improvements.md"));
}
