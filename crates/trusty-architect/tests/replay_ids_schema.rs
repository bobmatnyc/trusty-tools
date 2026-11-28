//! #12029 P2a: the `ids36.py` and `dashboard_schema.py` recordings replay clean against the port.
//!
//! Recorded from the frozen Python through each directory's `driver.py`:
//! `ids36/` runs the script's own command line (`cli ...`) and its library
//! functions (`table`, `enc`, `dec`, `next`); `dashboard_schema/` runs case
//! lists against the library with synthetic schema directories seeded in the
//! sandbox. `examples/replay_port.rs` mirrors both drivers. Neither module
//! reads the clock or writes a file, so every `writes` is empty.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

/// Replays every recording in `tests/fixtures/replay/<dir>` against
/// `replay_port <port>`; returns the recording names, sorted.
fn replay_dir(dir: &str, port: &str) -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay")
        .join(dir);
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec![port.to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let (mut names, mut failed) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(rec.writes.is_empty(), "{}: writes", path.display());
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
fn ids36_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_dir("ids36", "ids36"),
        [
            "cli-next-00.json",
            "cli-next-0f.json",
            "cli-next-0k-skips-l.json",
            "cli-next-0n-skips-o.json",
            "cli-next-0z.json",
            "cli-next-empty.json",
            "cli-next-hex-ff.json",
            "cli-next-l.json",
            "cli-next-long.json",
            "cli-next-non-ascii.json",
            "cli-next-o.json",
            "cli-next-quote.json",
            "cli-next-short.json",
            "cli-next-trailing-newline.json",
            "cli-next-upper.json",
            "cli-next-zy.json",
            "cli-next-zz-wraps.json",
            "cli-usage-extra.json",
            "cli-usage-next-alone.json",
            "cli-usage-none.json",
            "cli-usage-verb.json",
            "dec-values.json",
            "driver-usage-enc.json",
            "driver-usage.json",
            "enc-bounds.json",
            "next-values.json",
            "table.json",
        ]
    );
}

#[test]
fn dashboard_schema_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_dir("dashboard_schema", "dashboard_schema"),
        [
            "driver-usage-extra.json",
            "driver-usage.json",
            "helpers.json",
            "keywords.json",
            "load-errors.json",
            "store.json",
        ]
    );
}
