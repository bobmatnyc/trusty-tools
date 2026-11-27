//! #12026 end-to-end: the `level_lint.py` recordings replay clean against a Rust binary.
//!
//! The recordings in `tests/fixtures/replay/level_lint/` were made from the
//! Architect's `level_lint.py` and replay clean against it (see the fixture
//! README); this test replays the same files against the Rust stand-in
//! `examples/replay_standin_level_lint.rs`, the target P1 (#12028) replaces.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

const NAME: &str = "replay_standin_level_lint";

fn standin() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| replay_support::build_example(NAME))
}

#[test]
fn level_lint_recordings_replay_clean_against_the_rust_stand_in() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/level_lint");
    let target = Target {
        program: standin().to_path_buf(),
        prefix: Vec::new(),
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
            "engineer-quiet.json",
            "pm-strict-file.json",
            "unknown-level-stdin.json"
        ]
    );
    let strict = load(&dir.join("pm-strict-file.json")).unwrap();
    assert_eq!(
        strict.reads,
        Some(vec!["note.md".to_owned()]),
        "the probe saw the file read"
    );
    assert_eq!(strict.exit, 1);
}
