//! #12028 P1e: the state writer recordings replay clean against the ports.
//!
//! `state-archive.py` and `aliases.py` are recorded directly; `brief-id.py`
//! through the `driver.py` in its fixture directory, which points the
//! library's paths at the sandbox root and which `examples/replay_port.rs`
//! mirrors. Each recording's `writes` holds the files the run wrote, so a
//! clean replay proves the written bytes too.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

/// Replays every recording in `fixtures/replay/<script>` against
/// `replay_port <script>`; returns the sorted file names, and fails with
/// every differing recording at once.
fn replay_all(script: &str) -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay")
        .join(script);
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec![script.to_owned()],
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
    names
}

#[test]
fn state_archive_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("state-archive"),
        [
            "archive-without-head.json",
            "bad-arg-keep-missing.json",
            "bad-arg-keep.json",
            "bad-arg-unrecognized.json",
            "crlf-move.json",
            "dry-run-nothing.json",
            "dry-run.json",
            "empty-state.json",
            "help.json",
            "keep-1-crlf.json",
            "keep-big.json",
            "keep-negative.json",
            "keep-zero-next-ids-safe.json",
            "move-existing-archive.json",
            "move-new-archive.json",
            "no-sections.json",
            "nothing-to-move.json",
            "refuse-next-ids-dry.json",
            "refuse-next-ids.json",
        ]
    );
}

#[test]
fn aliases_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("aliases"),
        [
            "add-bad-json.json",
            "add-email-refused.json",
            "add-host-github.json",
            "add-key-error.json",
            "add-merge.json",
            "add-new-file.json",
            "add-no-alias-key.json",
            "add-project.json",
            "add-refused.json",
            "add-write-fails.json",
            "bad-arg-add-kind.json",
            "bad-arg-add-name.json",
            "bad-arg-ambiguous.json",
            "bad-arg-command.json",
            "bad-arg-kind.json",
            "bad-arg-unrecognized.json",
            "check-clean.json",
            "check-errors.json",
            "check-missing-table.json",
            "check-not-object.json",
            "help-add.json",
            "help.json",
            "list-bad-json.json",
            "list-empty-table.json",
            "list-kind.json",
            "list-missing-table.json",
            "list-not-object.json",
            "list.json",
            "no-command.json",
            "resolve-all.json",
            "resolve-ambiguous.json",
            "resolve-bad-args.json",
            "resolve-bad-table.json",
            "resolve-blank.json",
            "resolve-hit.json",
            "resolve-max.json",
            "resolve-missing-table.json",
            "resolve-no-match.json",
            "resolve-quiet-dict.json",
            "resolve-quiet.json",
            "resolve-stdin.json",
        ]
    );
}

#[test]
fn brief_id_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("brief-id"),
        [
            "assign-fence.json",
            "assign-marker-registered.json",
            "assign-marker-unregistered.json",
            "assign-new.json",
            "assign-no-h1.json",
            "assign-path-known.json",
            "bad-arg-assign-path.json",
            "bad-arg-command.json",
            "bad-arg-no-command.json",
            "bad-arg-unrecognized.json",
            "check-clean.json",
            "check-empty-dir.json",
            "check-problems.json",
            "dashes-before-command.json",
            "find-absolute.json",
            "find-caseless.json",
            "find-legacy-number.json",
            "find-mb.json",
            "find-no-registry.json",
            "find-not-found.json",
            "help-assign.json",
            "help.json",
            "list-empty.json",
            "list.json",
            "next-empty.json",
            "next-mixed.json",
        ]
    );
}
