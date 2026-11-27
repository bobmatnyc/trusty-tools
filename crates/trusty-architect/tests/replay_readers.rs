//! #12028 P1c: the `technical_level.py` and `architect_config.py` recordings replay clean against the port.
//!
//! Both scripts are libraries. Each fixture directory holds the `driver.py`
//! that called the frozen module at record time; `examples/replay_port.rs`
//! mirrors each driver over `trusty_architect::technical_level` and
//! `trusty_architect::architect_config`.

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
fn technical_level_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("technical_level"),
        [
            "bad-usage.json",
            "read-bad-format-env.json",
            "read-corrupt.json",
            "read-jsonl.json",
            "read-no-key.json",
            "read-no-store.json",
            "read-non-string.json",
            "read-set.json",
            "read-unknown-value.json",
            "read-wrong-format.json",
            "table.json",
            "values.json",
        ]
    );
}

#[test]
fn architect_config_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_all("architect_config"),
        [
            "bad-json-bom.json",
            "bad-json-trailing-comma.json",
            "bad-json-unterminated.json",
            "bad-usage.json",
            "blank-file.json",
            "builder-slots-float.json",
            "cap-infinite.json",
            "cap-nan.json",
            "crlf-repos-not-object.json",
            "defaults-no-file.json",
            "env-empty-is-unset.json",
            "env-load-factor-nan.json",
            "env-load-factor-zero.json",
            "env-overrides.json",
            "env-port-not-integer.json",
            "env-port-out-of-range.json",
            "env-tailnet-bad.json",
            "full-file.json",
            "interval-zero.json",
            "override-file.json",
            "override-missing.json",
            "path-is-directory.json",
            "port-wrong-type.json",
            "repo-empty-slug.json",
            "repo-field-type.json",
            "tailnet-not-http.json",
            "top-not-object.json",
        ]
    );
}
