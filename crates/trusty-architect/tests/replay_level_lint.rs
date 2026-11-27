//! #12026 end-to-end, #12028 P1c port: every `level_lint.py` recording replays clean against the port.
//!
//! The recordings in `tests/fixtures/replay/level_lint/` were made from the
//! Architect's `level_lint.py` (the first three by #12026, the rest from the
//! frozen Python reference on 2026-11-27) and replay clean against it (see the
//! fixture README); this test replays them against `examples/replay_port.rs`,
//! which runs `trusty_architect::level_lint::run`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

#[test]
fn level_lint_recordings_replay_clean_against_the_port() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/level_lint");
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec!["level_lint".to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let found = replay(&rec, &target, &parent, Duration::from_secs(30)).unwrap();
        assert!(found.is_empty(), "{}: {found:?}", path.display());
        names.push(path.file_name().unwrap().to_string_lossy().into_owned());
    }
    names.sort();
    assert_eq!(
        names,
        [
            "abbrev-and-dashes.json",
            "bad-arg-ambiguous.json",
            "bad-arg-explicit.json",
            "bad-arg-missing-value.json",
            "bad-arg-option-in-text.json",
            "bad-arg-unrecognized.json",
            "crlf-file.json",
            "cto-equals-quiet.json",
            "empty-file.json",
            "empty-stdin.json",
            "engineer-quiet.json",
            "help.json",
            "non-technical-args.json",
            "pm-strict-file.json",
            "rules-corpus.json",
            "unknown-level-stdin.json",
        ]
    );
    let strict = load(&dir.join("pm-strict-file.json")).unwrap();
    assert_eq!(
        strict.reads,
        Some(vec!["note.md".to_owned()]),
        "the probe saw the file read"
    );
    assert_eq!(strict.exit, 1);
    // Every rule fires at least once in the corpus recording.
    let corpus = load(&dir.join("rules-corpus.json")).unwrap();
    for rule in ["path", "sha", "flag", "envvar", "job", "tool", "identifier"] {
        assert!(
            corpus.stdout.contains(&format!("]: {rule} `")),
            "{rule} missing"
        );
    }
}
