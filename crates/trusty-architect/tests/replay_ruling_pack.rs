//! #12028 P1i: the `ruling_pack.py` recordings replay clean against the port.
//!
//! Recorded from the frozen script through `tests/fixtures/replay/ruling_pack/driver.py`,
//! which sets the script's ROOT to the sandbox root; `examples/replay_port.rs`
//! runs the port's command line there. `trusty-memory`, `trusty-search` and
//! `rg` are fakes declared in each recording (the memory fake answers
//! JSON-RPC by request id), so a clean replay also proves the port sent the
//! same MCP requests and ran the same searches and greps. The key map is a
//! sample in the sandbox; the one `write` recording writes its pack under
//! the sandbox's `local/packs`, and the replay compares that file.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

#[test]
fn ruling_pack_recordings_replay_clean_against_the_port() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/ruling_pack");
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec!["ruling_pack".to_owned()],
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
            "bad-arg-abbrev.json",
            "bad-arg-int.json",
            "bad-arg-missing.json",
            "bad-arg-type.json",
            "cap.json",
            "help.json",
            "hit-all.json",
            "identifiers.json",
            "init-error.json",
            "keys-bad-json.json",
            "keys-missing.json",
            "keys-no-projects.json",
            "memory-absent.json",
            "memory-only.json",
            "no-fakes.json",
            "no-root.json",
            "recall-errors.json",
            "recall-miss.json",
            "recall-no-result.json",
            "recall-shapes.json",
            "unknown-project.json",
            "write.json",
        ]
    );
}

#[test]
fn the_write_recording_lands_its_pack_under_the_sandbox_root() {
    // See #12028 P1i: the one write arm writes under the sandbox's
    // `local/packs` and nowhere else; every other recording writes nothing.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/ruling_pack");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let rec = load(&path).unwrap();
        let writes: Vec<&String> = rec.writes.keys().collect();
        if path.file_stem().is_some_and(|s| s == "write") {
            assert_eq!(writes, ["local/packs/20261127T130000Z-sample-app.md"]);
            assert!(
                rec.stdout
                    .ends_with("\npack file: {ROOT}/local/packs/20261127T130000Z-sample-app.md\n")
            );
            assert!(!rec.argv.iter().any(|a| a == "--no-write" || a == "--keys"));
        } else {
            assert!(writes.is_empty(), "{}: {writes:?}", path.display());
        }
    }
}
