//! #12087: the `gh-session-track.py` recordings replay clean against the port.
//!
//! Recorded from the frozen script itself with `--repo example-org/example-repo`
//! and `--root {ROOT}/q`; `examples/replay_port.rs` runs the port's command
//! line. gh is a fake declared in each recording, so a clean replay also
//! proves the port ran the same four queries with the same argv. The write
//! recordings compare every written store file; the store lock and the Rust
//! store's change log are left out on both sides (`skip_written_file`), and
//! each seeded session carries `updated` at the recorded instant, the value
//! the Rust store restamps (#10678).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

#[test]
fn gh_session_track_recordings_replay_clean_against_the_port() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/gh-session-track");
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec!["gh-session-track".to_owned()],
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
            "bad-arg-ambiguous.json",
            "bad-arg-missing-value.json",
            "bad-arg-unrecognized.json",
            "dry-run-abbrev.json",
            "dry-run-empty-json.json",
            "dry-run-empty.json",
            "dry-run-json.json",
            "dry-run-jsonl.json",
            "dry-run-no-store.json",
            "dry-run-text.json",
            "gh-fails-api.json",
            "gh-fails-pr-list.json",
            "gh-invalid-json-text.json",
            "gh-invalid-json.json",
            "help.json",
            "store-bad-format.json",
            "store-corrupt-meta.json",
            "store-corrupt-session.json",
            "store-session-not-object.json",
            "store-wrong-format.json",
            "write-batch-fails.json",
            "write-empty-replies.json",
            "write-json.json",
            "write-multi-session.json",
            "write-new-store.json",
            "write-one-session.json",
        ]
    );
}
