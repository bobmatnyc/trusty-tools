//! #12029 P2g: the `dashboard-db.py` recordings replay clean against the port.
//!
//! Recorded from the frozen Python (architect `0fbbc81eed`) through
//! `dashboard-db/ddb_driver.py`, which points the script's checkout at the
//! sandbox root, on synthetic stores in both formats: every sub-command,
//! its not-found (exit 1), validation (exit 2) and conflict (exit 3) paths,
//! the footer with the questions link and the reminders line present,
//! absent and failed, the argparse errors and help.
//! `examples/replay_port.rs dashboard-db` runs the port's command line.
//! Two recorded differences (intended divergences, #12029 P2g): the port
//! reads the reminders with one `datg` call where the script imports
//! `datg`, so the recordings that have a ledger expect [`DATG_CALL`] on the
//! `datg` fake; and `help-top` holds the port's neutral `status-brief`
//! example ([`HELP_EXAMPLE`]). The frozen `schemas/dashboard` the
//! recordings seeded lives once in [`SCHEMAS`] and is laid back into each
//! recording it lists. Every write lands under the sandbox root.

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::time::Duration;

use trusty_architect::replay::{Recording, Target, load, replay};

mod replay_support;

const DIR: &str = "dashboard-db";

/// The `datg` call the port makes where the script reads the ledger in process.
const DATG_CALL: [&str; 6] = [
    "datg",
    "--project",
    "{ROOT}/local/il",
    "reminders",
    "--due",
    "--json",
];

/// The frozen `schemas/dashboard`, one copy: `{"recordings": [...], "files": {...}}`.
const SCHEMAS: &str = "tests/fixtures/dashboard-db/frozen-schemas.json";

/// The port's `status-brief` example in the top help; the frozen help names
/// a path under the Architect's private brief directory there instead.
const HELP_EXAMPLE: &str = "MB0be9 sample/notes/x.md";

/// The recordings whose sandbox holds a ledger, so the port calls `datg` once.
const WITH_LEDGER: [&str; 4] = [
    "footer-reminders-failed.json",
    "footer-reminders-json.json",
    "footer-reminders-none.json",
    "footer-reminders.json",
];

/// The shared schema files and the recordings that seed them.
fn schemas() -> (Vec<String>, BTreeMap<String, String>) {
    #[derive(serde::Deserialize)]
    struct Shared {
        recordings: Vec<String>,
        files: BTreeMap<String, String>,
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(SCHEMAS);
    let text = std::fs::read_to_string(&path).unwrap();
    let s: Shared = serde_json::from_str(&text).unwrap();
    (s.recordings, s.files)
}

/// Every recording, with the shared schema files laid into those that seed them.
fn fixtures() -> Vec<(String, Recording)> {
    let (users, files) = schemas();
    let mut out = raw_fixtures();
    for (name, rec) in &mut out {
        if users.contains(name) {
            for (k, v) in &files {
                assert!(
                    rec.files.insert(k.clone(), v.clone()).is_none(),
                    "{name}: {k}"
                );
            }
        }
    }
    out
}

/// Every recording as committed.
fn raw_fixtures() -> Vec<(String, Recording)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay")
        .join(DIR);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        out.push((
            path.file_name().unwrap().to_string_lossy().into_owned(),
            rec,
        ));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// `rec` with the port's `datg` call added to its fake calls.
fn with_datg_call(mut rec: Recording) -> Recording {
    let fakes = rec
        .fakes
        .as_mut()
        .expect("a ledger recording declares the datg fake");
    assert!(fakes.calls.is_empty(), "the frozen script calls no datg");
    assert!(fakes.stubs.contains_key("datg"));
    fakes
        .calls
        .push(DATG_CALL.iter().map(|s| (*s).to_owned()).collect());
    rec
}

#[test]
fn dashboard_db_recordings_replay_clean_against_the_port() {
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec![DIR.to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let recs = fixtures();
    let mut failed = Vec::new();
    for (name, rec) in &recs {
        let rec = if WITH_LEDGER.contains(&name.as_str()) {
            with_datg_call(rec.clone())
        } else {
            rec.clone()
        };
        let found = replay(&rec, &target, &parent, Duration::from_secs(120)).unwrap();
        if !found.is_empty() {
            failed.push(format!("{name}: {found:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    assert!(recs.len() >= 100, "{} recordings", recs.len());
}

/// Each of the 18 sub-commands has a recording, and each exit class
/// (0, 1 not found, 2 refused, 3 conflict) is recorded.
#[test]
fn dashboard_db_recordings_cover_every_sub_command_and_exit() {
    let recs = fixtures();
    let commands = [
        "get",
        "list",
        "set",
        "update",
        "delete",
        "pruned",
        "next",
        "create",
        "log",
        "status-brief",
        "summary",
        "requests",
        "validate",
        "fsck",
        "tally",
        "footer",
        "batch",
    ];
    for c in commands {
        assert!(
            recs.iter().any(|(_, r)| r.argv.iter().any(|a| a == c)),
            "no recording runs {c}"
        );
    }
    for exit in [0, 1, 2, 3] {
        assert!(
            recs.iter().any(|(_, r)| r.exit == exit),
            "no recording exits {exit}"
        );
    }
    assert!(
        recs.iter()
            .any(|(_, r)| r.argv.iter().any(|a| a == "-h" || a == "--help"))
    );
}

/// A recorded write path: relative, with no `..` or root component.
fn inside_sandbox(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

/// Every write is under the sandbox root, and only the recordings with a
/// ledger declare the `datg` fake's answers that matter.
#[test]
fn dashboard_db_recordings_write_only_under_the_root() {
    for (name, rec) in fixtures() {
        for path in rec.writes.keys() {
            assert!(inside_sandbox(path), "{name}: {path}");
        }
        if WITH_LEDGER.contains(&name.as_str()) {
            assert!(
                rec.files
                    .keys()
                    .any(|f| f.ends_with(".trusty-mpm/datg/state.json")),
                "{name}"
            );
        }
    }
}

/// The shared schema file lists only recordings that exist, and a committed
/// recording carries no schema file of its own.
#[test]
fn dashboard_db_recordings_share_one_frozen_schema_copy() {
    let (users, files) = schemas();
    assert_eq!(files.len(), 18);
    assert!(files.keys().all(|k| k.starts_with("schemas/dashboard/")));
    let recs = raw_fixtures();
    for u in &users {
        assert!(recs.iter().any(|(n, _)| n == u), "{u} is not a recording");
    }
    for (name, rec) in &recs {
        assert!(
            !rec.files.keys().any(|k| k.starts_with("schemas/")),
            "{name} seeds its own schema copy"
        );
    }
}

/// Intended divergence (e), #12029 P2g: the top help's `status-brief`
/// example is a neutral path; `help-top` records it in place of the frozen
/// one, once, and the replay holds the port to it.
#[test]
fn help_names_a_neutral_status_brief_example() {
    let recs = raw_fixtures();
    let (_, top) = recs.iter().find(|(n, _)| n == "help-top.json").unwrap();
    assert_eq!(top.stdout.matches(HELP_EXAMPLE).count(), 1);
    assert!(top.stdout.contains("prepend a status brief to a task"));
    for (name, rec) in recs.iter().filter(|(n, _)| n.starts_with("help-")) {
        assert!(!rec.stdout.contains("cto/"), "{name}");
    }
}
